//! Configuration fragments (`ctx.fragments.<name>`): Starlark views of Bazel's
//! `CppConfiguration`, `JavaConfiguration`, `AppleConfiguration`, `ObjcConfiguration`,
//! `ProtoConfiguration`, `PythonConfiguration` and `PlatformConfiguration`, with the option
//! defaults of `CppOptions`, `JavaOptions`, `AppleCommandLineOptions`, `ObjcCommandLineOptions`
//! and `ProtoConfiguration` (Bazel 9). Options come from `BuildConfigurationKey::options`
//! (bare option name → value; list options are comma-joined).

use crate::starlark_rule::{mk_list, mk_struct};
use crate::BuildConfigurationKey;
use std::sync::Arc;
use zel_cmdline::Label;
use zel_starlark::{
    ClosureEnv, Depset, DepsetOrder, Expr, Location, ParamDef, Stmt, UserFunctionDef, Value,
};

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
        repr_override: None,
        closure: ClosureEnv::default(),
    }));
    (name.to_string(), f)
}

fn mk(fields: Vec<(String, Value)>) -> Value {
    Value::Struct(Arc::new(zel_starlark::StructValue::new(
        fields.into_iter().collect(),
    )))
}

fn f(name: &str, v: Value) -> (String, Value) {
    (name.to_string(), v)
}

struct Opts<'a> {
    cfg: &'a BuildConfigurationKey,
    bo: crate::build_options::BuildOptions,
}

impl<'a> Opts<'a> {
    fn new(cfg: &'a BuildConfigurationKey) -> Self {
        Self {
            cfg,
            bo: cfg.build_options(),
        }
    }

    fn raw(&self, k: &str) -> Option<String> {
        if let Some(v) = self.cfg.options.get(k) {
            return Some(v.clone());
        }
        if let Some(n) = self.cfg.native.get(k) {
            return match &n.value {
                crate::build_options::ConfigValue::Null => None,
                other => Some(other.display()),
            };
        }
        match self.bo.get(k) {
            Some(crate::build_options::ConfigValue::Scalar(s)) => Some(s.clone()),
            Some(crate::build_options::ConfigValue::List(vs)) if !vs.is_empty() => {
                Some(vs.join(","))
            }
            _ => None,
        }
    }

    fn has_explicit(&self, k: &str) -> bool {
        self.cfg.options.contains_key(k)
            || self.cfg.native.contains_key(k)
            || self
                .cfg
                .transition_changes
                .contains_key(&format!("//command_line_option:{k}"))
    }

    fn s(&self, k: &str, d: &str) -> String {
        self.raw(k).unwrap_or_else(|| d.to_string())
    }

    fn opt_s(&self, k: &str) -> Value {
        match self.raw(k).filter(|v| !v.is_empty()) {
            Some(v) => Value::String(v),
            None => Value::None,
        }
    }

    fn dotted(&self, k: &str) -> Value {
        match self.raw(k).filter(|v| !v.is_empty()) {
            Some(v) => Value::Host("dotted_version".into(), v),
            None => Value::None,
        }
    }

    fn b(&self, k: &str, d: bool) -> bool {
        if let Some(v) = self.cfg.options.get(k) {
            return match v.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "t" | "y" => true,
                "0" | "false" | "no" | "f" | "n" => false,
                _ => d,
            };
        }
        if let Some(n) = self.cfg.native.get(k) {
            if let crate::build_options::ConfigValue::Scalar(v) = &n.value {
                return match v.to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "t" | "y" => true,
                    "0" | "false" | "no" | "f" | "n" => false,
                    _ => d,
                };
            }
        }
        if let Some(crate::build_options::ConfigValue::Scalar(v)) = self.bo.get(k) {
            return match v.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "t" | "y" => true,
                "0" | "false" | "no" | "f" | "n" => false,
                _ => d,
            };
        }
        d
    }

    fn list(&self, k: &str) -> Vec<String> {
        if let Some(n) = self.cfg.native.get(k) {
            if self.cfg.options.get(k).is_none_or(|raw| raw == &n.raw) {
                if let crate::build_options::ConfigValue::List(l) = &n.value {
                    return l.clone();
                }
            }
        }
        if let Some(v) = self.cfg.options.get(k) {
            return v
                .split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
        }
        match self.bo.get(k) {
            Some(crate::build_options::ConfigValue::List(vs)) => vs
                .iter()
                .flat_map(|v| v.split(','))
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            Some(crate::build_options::ConfigValue::Scalar(v)) => v
                .split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Repeatable flag list without comma-splitting (`--protocopt`, `--javacopt`, `--jvmopt`, etc.).
    fn list_unsplit(&self, k: &str) -> Vec<String> {
        if let Some(crate::build_options::ConfigValue::List(vs)) = self.bo.get(k) {
            return vs.clone();
        }
        if let Some(v) = self.cfg.options.get(k) {
            if v.is_empty() {
                return Vec::new();
            }
            return v.split(',').map(str::to_string).collect();
        }
        Vec::new()
    }

    fn list_v(&self, k: &str) -> Value {
        mk_list(self.list(k).into_iter().map(Value::String).collect())
    }

    fn list_unsplit_v(&self, k: &str) -> Value {
        mk_list(
            self.list_unsplit(k)
                .into_iter()
                .map(Value::String)
                .collect(),
        )
    }

    fn label(&self, k: &str) -> Value {
        match self
            .raw(k)
            .filter(|v| !v.is_empty() && v != "default")
            .and_then(|v| Label::parse_canonical(&v).ok())
        {
            Some(l) => Value::Label(l),
            None => Value::None,
        }
    }
}

fn sv(s: &str) -> Value {
    Value::String(s.to_string())
}

fn apple_platform_struct(name: &str, plist: &str, platform_type: &str, is_device: bool) -> Value {
    mk_struct(vec![
        ("name", sv(name)),
        ("name_in_plist", sv(plist)),
        ("platform_type", sv(platform_type)),
        ("is_device", Value::Bool(is_device)),
    ])
}

/// Resolves a `{platform}_{arch}` (`darwin_{arch}` on macOS) string against Bazel's fixed
/// `ApplePlatform.forTargetCpu` sets.
pub fn apple_platform_for_target_cpu(target_cpu: &str) -> Result<Value, String> {
    Ok(match target_cpu {
        "ios_x86_64" | "ios_i386" | "ios_sim_arm64" | "ios_sim_arm64e" => {
            apple_platform_struct("ios_simulator", "iPhoneSimulator", "ios", false)
        }
        "ios_armv6" | "ios_arm64" | "ios_armv7" | "ios_armv7s" | "ios_arm64e" => {
            apple_platform_struct("ios_device", "iPhoneOS", "ios", true)
        }
        "visionos_sim_arm64" | "visionos_sim_arm64e" => {
            apple_platform_struct("visionos_simulator", "XRSimulator", "visionos", false)
        }
        "visionos_arm64" | "visionos_arm64e" => {
            apple_platform_struct("visionos_device", "XROS", "visionos", true)
        }
        "watchos_i386" | "watchos_x86_64" | "watchos_arm64" | "watchos_sim_arm64e" => {
            apple_platform_struct("watchos_simulator", "WatchSimulator", "watchos", false)
        }
        "watchos_armv7k"
        | "watchos_arm64_32"
        | "watchos_device_arm64"
        | "watchos_device_arm64e" => {
            apple_platform_struct("watchos_device", "WatchOS", "watchos", true)
        }
        "tvos_x86_64" | "tvos_sim_arm64" | "tvos_sim_arm64e" => {
            apple_platform_struct("tvos_simulator", "AppleTVSimulator", "tvos", false)
        }
        "tvos_arm64" | "tvos_arm64e" => {
            apple_platform_struct("tvos_device", "AppleTVOS", "tvos", true)
        }
        "catalyst_x86_64" => apple_platform_struct("catalyst", "MacOSX", "catalyst", true),
        "darwin_x86_64" | "darwin_arm64" | "darwin_arm64e" => {
            apple_platform_struct("macos", "MacOSX", "macos", true)
        }
        _ => {
            return Err(format!(
                "No supported apple platform registered for target cpu {target_cpu}"
            ))
        }
    })
}

fn default_apple_cpu_for_platform(ptype: &str, cfg_cpu: &str) -> Result<String, String> {
    let is_aarch64 = cfg!(target_arch = "aarch64");
    Ok(match ptype {
        "ios" => {
            if let Some(r) = cfg_cpu.strip_prefix("ios_") {
                r.to_string()
            } else if is_aarch64 {
                "sim_arm64".to_string()
            } else {
                "x86_64".to_string()
            }
        }
        "visionos" => {
            if let Some(r) = cfg_cpu.strip_prefix("visionos_") {
                r.to_string()
            } else {
                "sim_arm64".to_string()
            }
        }
        "watchos" => {
            if let Some(r) = cfg_cpu.strip_prefix("watchos_") {
                r.to_string()
            } else if is_aarch64 {
                "arm64".to_string()
            } else {
                "x86_64".to_string()
            }
        }
        "tvos" => {
            if let Some(r) = cfg_cpu.strip_prefix("tvos_") {
                r.to_string()
            } else if is_aarch64 {
                "sim_arm64".to_string()
            } else {
                "x86_64".to_string()
            }
        }
        "macos" => {
            if let Some(r) = cfg_cpu.strip_prefix("darwin_") {
                r.to_string()
            } else if is_aarch64 {
                "arm64".to_string()
            } else {
                "x86_64".to_string()
            }
        }
        "catalyst" => {
            if let Some(r) = cfg_cpu.strip_prefix("catalyst_") {
                r.to_string()
            } else {
                "x86_64".to_string()
            }
        }
        other => return Err(format!("Unsupported platform type \"{other}\"")),
    })
}

fn apple_cpus_for_platform(o: &Opts<'_>, ptype: &str) -> Result<Vec<String>, String> {
    let flag = match ptype {
        "ios" => "ios_multi_cpus",
        "visionos" => "visionos_cpus",
        "watchos" => "watchos_cpus",
        "tvos" => "tvos_cpus",
        "macos" => "macos_cpus",
        "catalyst" => "catalyst_cpus",
        other => return Err(format!("Unsupported platform type \"{other}\"")),
    };
    let explicit = o.list(flag);
    if !explicit.is_empty() {
        return Ok(explicit);
    }
    Ok(vec![default_apple_cpu_for_platform(ptype, &o.cfg.cpu)?])
}

/// Resolves `multi_arch_platform(platform_type)` in `cfg`.
pub fn apple_multi_arch_platform(
    cfg: &BuildConfigurationKey,
    platform_type: &str,
) -> Result<Value, String> {
    let o = Opts::new(cfg);
    let ptype = platform_type.to_ascii_lowercase();
    let cfg_ptype = o.s("apple_platform_type", "macos").to_ascii_lowercase();
    let split = o.s("apple_split_cpu", "");
    let raw_cpu = if ptype == cfg_ptype && !split.is_empty() {
        split
    } else {
        apple_cpus_for_platform(&o, &ptype)?[0].clone()
    };
    let target_cpu = if ptype == "macos" {
        format!("darwin_{}", raw_cpu.strip_prefix("darwin_").unwrap_or(&raw_cpu))
    } else {
        let prefix = format!("{ptype}_");
        format!("{ptype}_{}", raw_cpu.strip_prefix(&prefix).unwrap_or(&raw_cpu))
    };
    apple_platform_for_target_cpu(&target_cpu)
}

/// Validates and normalizes a `StrictDepsMode` option (`ERROR`, `OFF`, `WARN`, `STRICT`, `DEFAULT`).
pub fn strict_deps_mode_upper(cfg: &BuildConfigurationKey, option: &str, default: &str) -> Result<String, String> {
    let o = Opts::new(cfg);
    let raw = o.s(option, default);
    let upper = raw.to_ascii_uppercase();
    match upper.as_str() {
        "OFF" | "WARN" | "ERROR" | "STRICT" | "DEFAULT" => Ok(upper),
        _ => Err(format!(
            "Not a valid strict dependency checking level: '{raw}' (should be off, warn, error, strict or default)"
        )),
    }
}

/// Builds Bazel's `ProtoConstants` `--direct_dependencies_violation_msg=...` protoc flag.
pub fn direct_dependencies_violation_msg(label: &Label) -> String {
    format!(
        "--direct_dependencies_violation_msg=%s is imported, but {label} doesn't directly depend on a proto_library that 'srcs' it."
    )
}

/// The Starlark value of fragment `name` in `cfg`; `None` for unknown fragments.
pub fn fragment(name: &str, cfg: &BuildConfigurationKey) -> Option<Value> {
    fragment_checked(name, cfg).ok().flatten()
}

/// Checked variant of [`fragment`] that surfaces invalid fragment option errors.
pub fn fragment_checked(name: &str, cfg: &BuildConfigurationKey) -> Result<Option<Value>, String> {
    let o = Opts::new(cfg);
    let mode = cfg.compilation_mode.as_str();
    Ok(Some(match name {
        "cpp" => {
            let strip = o.s("strip", "sometimes");
            let strip_binaries = strip == "always" || (strip == "sometimes" && mode == "fastbuild");
            let fission = o.list("fission");
            let fission_active = fission.iter().any(|m| m == mode || m == "yes")
                && !fission.iter().any(|m| m == "no");
            let omitfp = o.b("experimental_omitfp", false);
            let mut copts: Vec<Value> = Vec::new();
            if omitfp {
                copts.extend(
                    [
                        "-fomit-frame-pointer",
                        "-fasynchronous-unwind-tables",
                        "-DNO_FRAME_POINTER",
                    ]
                    .map(sv),
                );
            }
            copts.extend(o.list("copt").into_iter().map(Value::String));
            let _fdo_path = Value::None;
            mk(vec![
                // Struct fields.
                f("copts", mk_list(copts)),
                f("cxxopts", o.list_v("cxxopt")),
                f("conlyopts", o.list_v("conlyopt")),
                f("objccopts", o.list_v("objccopt")),
                f("linkopts", o.list_v("linkopt")),
                f("lto_backend_options", o.list_v("ltobackendopt")),
                f(
                    "_dont_enable_host_nonhost",
                    Value::Bool(o.b(
                        "incompatible_dont_enable_host_nonhost_crosstool_features",
                        true,
                    )),
                ),
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
                f(
                    "apple_generate_dsym",
                    Value::Bool(o.b("apple_generate_dsym", false)),
                ),
                f(
                    "objc_generate_linkmap",
                    Value::Bool(o.b("objc_generate_linkmap", false)),
                ),
                f(
                    "objc_should_strip_binary",
                    Value::Bool(o.b("objc_enable_binary_stripping", false) && strip_binaries),
                ),
                // Methods.
                m("compilation_mode", sv(mode)),
                m("lto_index_options", o.list_v("ltoindexopt")),
                m("minimum_os_version", o.opt_s("minimum_os_version")),
                m(
                    "dynamic_mode",
                    sv(&o.s("dynamic_mode", "default").to_uppercase()),
                ),
                m("start_end_lib", Value::Bool(o.b("start_end_lib", true))),
                m(
                    "legacy_whole_archive",
                    Value::Bool(o.b("legacy_whole_archive", true)),
                ),
                m(
                    "incompatible_remove_legacy_whole_archive",
                    Value::Bool(o.b("incompatible_remove_legacy_whole_archive", true)),
                ),
                m(
                    "interface_shared_objects",
                    Value::Bool(o.b("interface_shared_objects", true)),
                ),
                m("fdo_path", Value::None),
                m("cs_fdo_path", Value::None),
                m("fdo_instrument", Value::None),
                m("cs_fdo_instrument", Value::None),
                m("propeller_optimize_absolute_cc_profile", Value::None),
                m("propeller_optimize_absolute_ld_profile", Value::None),
                m("_fdo_prefetch_hints_label", o.label("fdo_prefetch_hints")),
                m(
                    "use_llvm_coverage_map_format",
                    Value::Bool(o.b("experimental_use_llvm_covmap", false)),
                ),
                m(
                    "generate_llvm_lcov",
                    Value::Bool(o.b("experimental_generate_llvm_lcov", false)),
                ),
                m("incompatible_use_specific_tool_files", Value::Bool(true)),
                m("include_scanning", Value::Bool(false)),
                m(
                    "should_generate_dotd_files",
                    Value::Bool(o.b("cc_dotd_files", true)),
                ),
                m(
                    "objc_should_generate_dotd_files",
                    Value::Bool(o.b("objc_use_dotd_pruning", true)),
                ),
                m(
                    "objc_enable_binary_stripping",
                    Value::Bool(o.b("objc_enable_binary_stripping", false)),
                ),
                m("experimental_cc_implementation_deps", Value::Bool(true)),
                m(
                    "experimental_cpp_modules",
                    Value::Bool(o.b("experimental_cpp_modules", false)),
                ),
                m(
                    "experimental_link_static_libraries_once",
                    Value::Bool(o.b("experimental_link_static_libraries_once", true)),
                ),
                m("proto_profile", Value::Bool(o.b("proto_profile", true))),
                m(
                    "save_feature_state",
                    Value::Bool(o.b("experimental_save_feature_state", false)),
                ),
                m("save_temps", Value::Bool(o.b("save_temps", false))),
                m("force_pic", Value::Bool(o.b("force_pic", false))),
                m(
                    "share_native_deps",
                    Value::Bool(o.b("share_native_deps", true)),
                ),
                m(
                    "process_headers_in_dependencies",
                    Value::Bool(o.b("process_headers_in_dependencies", false)),
                ),
                m(
                    "fission_active_for_current_compilation_mode",
                    Value::Bool(fission_active),
                ),
                m("build_test_dwp", Value::Bool(o.b("build_test_dwp", false))),
                m("should_strip_binaries", Value::Bool(strip_binaries)),
                m("strip_opts", o.list_v("stripopt")),
                m("grte_top", o.label("grte_top")),
                m("disable_nocopts", Value::Bool(true)),
            ])
        }
        "platform" => {
            let platform_label =
                |s: Option<&String>| match s.and_then(|p| Label::parse_canonical(p).ok()) {
                    Some(l) => Value::Label(l),
                    None => Value::Label(Label::parse_canonical("@platforms//host:host").unwrap()),
                };
            let target_plat = platform_label(cfg.platforms.first());
            let host_plat = platform_label(cfg.options.get("host_platform"));
            mk_struct(vec![
                ("platform", target_plat.clone()),
                ("host_platform", host_plat),
                ("platforms", mk_list(vec![target_plat])),
            ])
        }
        "java" => {
            let flags = o.list("javacopt");
            mk(vec![
                // Struct fields.
                f(
                    "default_javac_flags",
                    mk_list(flags.iter().cloned().map(Value::String).collect()),
                ),
                f(
                    "default_javac_flags_depset",
                    Value::Depset(Arc::new(
                        Depset::new(
                            DepsetOrder::Default,
                            flags.into_iter().map(Value::String).collect(),
                            vec![],
                        )
                        .map_err(|e| e.0)?,
                    )),
                ),
                f(
                    "strict_java_deps",
                    sv(&o
                        .s("experimental_strict_java_deps", "default")
                        .to_lowercase()),
                ),
                f(
                    "one_version_enforcement_level",
                    sv(&o
                        .s("experimental_one_version_enforcement", "OFF")
                        .to_uppercase()),
                ),
                f(
                    "one_version_enforcement_on_java_tests",
                    Value::Bool(o.b("one_version_enforcement_on_java_tests", true)),
                ),
                f("multi_release_deploy_jars", Value::Bool(true)),
                f(
                    "run_android_lint",
                    Value::Bool(o.b("experimental_run_android_lint_on_java_rules", false)),
                ),
                f("default_jvm_opts", o.list_v("jvmopt")),
                f("plugins", o.list_v("plugin")),
                f("launcher", o.label("java_launcher")),
                // Methods.
                m("use_ijars", Value::Bool(o.b("use_ijars", true))),
                m(
                    "use_header_compilation",
                    Value::Bool(o.b("java_header_compilation", true)),
                ),
                m("use_header_compilation_direct_deps", Value::Bool(true)),
                m(
                    "reduce_java_classpath",
                    sv(&o.s("experimental_java_classpath", "bazel").to_uppercase()),
                ),
                m(
                    "enforce_explicit_java_test_deps",
                    Value::Bool(o.b("explicit_java_test_deps", false)),
                ),
                m("generate_java_deps", Value::Bool(o.b("java_deps", true))),
                m(
                    "disallow_java_import_exports",
                    Value::Bool(o.b("incompatible_disallow_java_import_exports", false)),
                ),
                m("auto_create_java_test_deploy_jars", Value::Bool(false)),
            ])
        }
        // `BazelPythonConfiguration`.
        "bazel_py" => mk_struct(vec![
            (
                "python_import_all_repositories",
                Value::Bool(o.b("experimental_python_import_all_repositories", true)),
            ),
            (
                "python_path",
                sv(&{
                    let p = o.s("python_path", "");
                    if p.is_empty() {
                        if cfg_is_windows() {
                            "python".to_string()
                        } else {
                            "python3".to_string()
                        }
                    } else {
                        p
                    }
                }),
            ),
        ]),
        "py" => mk_struct(vec![
            ("default_python_version", sv("PY3")),
            (
                "build_python_zip",
                Value::Bool(o.b("build_python_zip", cfg_is_windows())),
            ),
            (
                "default_to_explicit_init_py",
                Value::Bool(o.b("incompatible_default_to_explicit_init_py", false)),
            ),
        ]),
        "proto" => {
            let hdr_suffixes: Vec<Value> = if o.has_explicit("cc_proto_library_header_suffixes") {
                let mut v: Vec<String> = o
                    .raw("cc_proto_library_header_suffixes")
                    .map(|s| s.split(',').map(str::to_string).collect())
                    .unwrap_or_default();
                v.sort();
                v.dedup();
                v.into_iter().map(Value::String).collect()
            } else {
                vec![sv(".pb.h")]
            };
            let src_suffixes: Vec<Value> = if o.has_explicit("cc_proto_library_source_suffixes") {
                let mut v: Vec<String> = o
                    .raw("cc_proto_library_source_suffixes")
                    .map(|s| s.split(',').map(str::to_string).collect())
                    .unwrap_or_default();
                v.sort();
                v.dedup();
                v.into_iter().map(Value::String).collect()
            } else {
                vec![sv(".pb.cc")]
            };
            let _ = strict_deps_mode_upper(cfg, "strict_proto_deps", "error")?;
            let _ = strict_deps_mode_upper(cfg, "strict_public_imports", "off")?;
            mk_struct(vec![
                ("experimental_protoc_opts", o.list_unsplit_v("protocopt")),
                ("cc_proto_library_header_suffixes", mk_list(hdr_suffixes)),
                ("cc_proto_library_source_suffixes", mk_list(src_suffixes)),
                (
                    "strict_proto_deps",
                    Value::BuiltinFunction("host:fragment.proto.strict_proto_deps".into()),
                ),
                (
                    "strict_public_imports",
                    Value::BuiltinFunction("host:fragment.proto.strict_public_imports".into()),
                ),
                (
                    "experimental_proto_descriptorsets_include_source_info",
                    Value::BuiltinFunction(
                        "host:fragment.proto.experimental_proto_descriptorsets_include_source_info"
                            .into(),
                    ),
                ),
            ])
        }
        "objc" => {
            let avoid_hardcoded = o.b("incompatible_avoid_hardcoded_objc_compilation_flags", true);
            let mode_copts: Vec<String> = match mode {
                "fastbuild" => {
                    if o.has_explicit("experimental_objc_fastbuild_options") {
                        o.raw("experimental_objc_fastbuild_options")
                            .map(|s| {
                                if s.is_empty() {
                                    Vec::new()
                                } else {
                                    s.split(',').map(str::to_string).collect()
                                }
                            })
                            .unwrap_or_default()
                    } else {
                        vec!["-O0".to_string(), "-DDEBUG=1".to_string()]
                    }
                }
                "dbg" => {
                    let mut flags: Vec<String> = if !avoid_hardcoded {
                        vec![
                            "-O0",
                            "-DDEBUG=1",
                            "-fstack-protector",
                            "-fstack-protector-all",
                            "-g",
                        ]
                        .into_iter()
                        .map(String::from)
                        .collect()
                    } else {
                        Vec::new()
                    };
                    if o.b("objc_debug_with_GLIBCXX", false) {
                        flags.extend(
                            [
                                "-D_GLIBCXX_DEBUG",
                                "-D_GLIBCXX_DEBUG_PEDANTIC",
                                "-D_GLIBCPP_CONCEPT_CHECKS",
                            ]
                            .into_iter()
                            .map(String::from),
                        );
                    }
                    flags
                }
                "opt" => {
                    if !avoid_hardcoded {
                        vec![
                            "-Os",
                            "-DNDEBUG=1",
                            "-Wno-unused-variable",
                            "-Winit-self",
                            "-Wno-extra",
                        ]
                        .into_iter()
                        .map(String::from)
                        .collect()
                    } else {
                        Vec::new()
                    }
                }
                other => return Err(format!("Invalid compilation mode: '{other}'")),
            };
            mk(vec![
                f(
                    "copts_for_current_compilation_mode",
                    mk_list(mode_copts.into_iter().map(Value::String).collect()),
                ),
                f(
                    "strip_executable_safely",
                    Value::Bool(o.b("incompatible_strip_executable_safely", false)),
                ),
                f(
                    "builtin_objc_strip_action",
                    Value::Bool(o.b("incompatible_builtin_objc_strip_action", true)),
                ),
                f(
                    "alwayslink_by_default",
                    Value::Bool(o.b("incompatible_objc_alwayslink_by_default", false)),
                ),
                f(
                    "disallow_sdk_frameworks_attributes",
                    Value::Bool(o.b("incompatible_disallow_sdk_frameworks_attributes", false)),
                ),
                f("ios_simulator_device", o.opt_s("ios_simulator_device")),
                f("ios_simulator_version", o.dotted("ios_simulator_version")),
                f("run_memleaks", Value::Bool(o.b("ios_memleaks", false))),
                f(
                    "signing_certificate_name",
                    o.opt_s("ios_signing_cert_name"),
                ),
                f(
                    "uses_device_debug_entitlements",
                    Value::Bool(o.b("device_debug_entitlements", true) && mode != "opt"),
                ),
                f(
                    "target_should_alwayslink",
                    Value::BuiltinFunction("host:fragment.objc.target_should_alwayslink".into()),
                ),
            ])
        }
        "apple" => {
            let ptype = o.s("apple_platform_type", "macos").to_ascii_lowercase();
            let split_cpu = o.s("apple_split_cpu", "");
            let ptype_cpus = apple_cpus_for_platform(&o, &ptype)?;
            let raw_cpu = if !split_cpu.is_empty() {
                split_cpu.clone()
            } else {
                ptype_cpus[0].clone()
            };
            let stripped_prefix = if ptype == "macos" {
                raw_cpu.strip_prefix("darwin_").unwrap_or(&raw_cpu)
            } else {
                let prefix = format!("{ptype}_");
                raw_cpu.strip_prefix(&prefix).unwrap_or(&raw_cpu)
            };
            let target_cpu = if ptype == "macos" {
                format!("darwin_{stripped_prefix}")
            } else {
                format!("{ptype}_{stripped_prefix}")
            };
            let single_arch_platform = apple_platform_for_target_cpu(&target_cpu)?;
            let single_arch_cpu = stripped_prefix
                .strip_prefix("sim_")
                .or_else(|| stripped_prefix.strip_prefix("device_"))
                .unwrap_or(stripped_prefix);
            let ios_cpus = apple_cpus_for_platform(&o, "ios")?;
            let visionos_cpus = apple_cpus_for_platform(&o, "visionos")?;
            let watchos_cpus = apple_cpus_for_platform(&o, "watchos")?;
            let tvos_cpus = apple_cpus_for_platform(&o, "tvos")?;
            let macos_cpus = apple_cpus_for_platform(&o, "macos")?;
            let catalyst_cpus = apple_cpus_for_platform(&o, "catalyst")?;
            let cpus_struct = mk_struct(vec![
                (
                    "apple_split_cpu",
                    if split_cpu.is_empty() {
                        Value::None
                    } else {
                        sv(&split_cpu)
                    },
                ),
                (
                    "ios_multi_cpus",
                    mk_list(ios_cpus.into_iter().map(Value::String).collect()),
                ),
                (
                    "visionos_cpus",
                    mk_list(visionos_cpus.into_iter().map(Value::String).collect()),
                ),
                (
                    "watchos_cpus",
                    mk_list(watchos_cpus.into_iter().map(Value::String).collect()),
                ),
                (
                    "tvos_cpus",
                    mk_list(tvos_cpus.into_iter().map(Value::String).collect()),
                ),
                (
                    "macos_cpus",
                    mk_list(macos_cpus.into_iter().map(Value::String).collect()),
                ),
                (
                    "catalyst_cpus",
                    mk_list(catalyst_cpus.into_iter().map(Value::String).collect()),
                ),
            ]);
            mk_struct(vec![
                ("apple_platform_type", sv(&ptype)),
                ("apple_cpus", cpus_struct),
                ("single_arch_platform", single_arch_platform),
                ("single_arch_cpu", sv(single_arch_cpu)),
                (
                    "multi_arch_platform",
                    Value::BuiltinFunction("host:fragment.apple.multi_arch_platform".into()),
                ),
                ("xcode_version_flag", o.dotted("xcode_version")),
                ("ios_sdk_version_flag", o.dotted("ios_sdk_version")),
                ("macos_sdk_version_flag", o.dotted("macos_sdk_version")),
                ("tvos_sdk_version_flag", o.dotted("tvos_sdk_version")),
                ("watchos_sdk_version_flag", o.dotted("watchos_sdk_version")),
                ("ios_minimum_os_flag", o.dotted("ios_minimum_os")),
                ("watchos_minimum_os_flag", o.dotted("watchos_minimum_os")),
                ("tvos_minimum_os_flag", o.dotted("tvos_minimum_os")),
                ("macos_minimum_os_flag", o.dotted("macos_minimum_os")),
                (
                    "prefer_mutual_xcode",
                    Value::Bool(o.b("experimental_prefer_mutual_xcode", true)),
                ),
                (
                    "include_xcode_exec_requirements",
                    Value::Bool(o.b("experimental_include_xcode_execution_requirements", false)),
                ),
            ])
        }
        "coverage" => mk_struct(vec![(
            "collect_code_coverage",
            Value::Bool(o.b("collect_code_coverage", false)),
        )]),
        _ => return Ok(None),
    }))
}

fn cfg_is_windows() -> bool {
    cfg!(windows)
}

/// The label a `configuration_field(fragment, name)` default resolves to in `cfg`
/// (`@StarlarkConfigurationField` of the native fragments), `None` if the field is unset.
pub fn late_bound_label(fragment: &str, name: &str, cfg: &BuildConfigurationKey) -> Option<Label> {
    let o = Opts::new(cfg);
    let tools = |p: &str| Label::parse_canonical(&format!("@@bazel_tools//{p}")).ok();
    let opt = |k: &str| {
        o.raw(k)
            .filter(|v| !v.is_empty() && v != "null")
            .and_then(|v| Label::parse_canonical(&v).ok())
    };
    match (fragment, name) {
        ("java", "launcher") => opt("java_launcher").or_else(|| {
            cfg.build_options()
                .get("java_launcher")
                .map(|v| v.display())
                .filter(|v| !v.is_empty() && v != "null")
                .and_then(|v| Label::parse_canonical(&v).ok())
        }),
        ("apple", "xcode_config_label") => {
            opt("xcode_version_config").or_else(|| tools("tools/objc:host_xcodes"))
        }
        ("proto", "proto_compiler") => {
            opt("proto_compiler").or_else(|| tools("tools/proto:protoc"))
        }
        ("proto", "proto_toolchain_for_java") => {
            opt("proto_toolchain_for_java").or_else(|| tools("tools/proto:java_toolchain"))
        }
        ("proto", "proto_toolchain_for_java_lite") | ("proto", "proto_toolchain_for_javalite") => {
            opt("proto_toolchain_for_javalite").or_else(|| tools("tools/proto:javalite_toolchain"))
        }
        ("proto", "proto_toolchain_for_cc") => {
            opt("proto_toolchain_for_cc").or_else(|| tools("tools/proto:cc_toolchain"))
        }
        ("proto", "proto_toolchain_for_j2objc") => {
            opt("proto_toolchain_for_j2objc").or_else(|| tools("tools/j2objc:j2objc_proto_toolchain"))
        }
        ("cpp", "libc_top") => opt("grte_top"),
        ("cpp", "zipper") => {
            let fdo = [
                "fdo_optimize",
                "fdo_profile",
                "memprof_profile",
                "xbinary_fdo",
            ]
            .iter()
            .any(|k| o.raw(k).is_some_and(|v| !v.is_empty()));
            if fdo {
                tools("tools/zip:unzip_fdo")
            } else {
                None
            }
        }
        ("coverage", "output_generator") => {
            if o.b("collect_code_coverage", false) {
                tools("tools/test:lcov_merger")
            } else {
                None
            }
        }
        (_, n) => opt(n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn struct_field<'a>(v: &'a Value, k: &str) -> &'a Value {
        match v {
            Value::Struct(s) => s.fields.get(k).expect(k),
            _ => panic!("expected struct"),
        }
    }

    fn set_scalar(cfg: &mut BuildConfigurationKey, k: &str, v: &str) {
        cfg.native.insert(
            k.to_string(),
            crate::NativeSetting {
                raw: v.to_string(),
                value: crate::build_options::ConfigValue::Scalar(v.to_string()),
            },
        );
    }

    #[test]
    fn apple_fragment_cpus_platforms_and_dotted_versions() {
        let mut cfg = BuildConfigurationKey::default();
        set_scalar(&mut cfg, "apple_platform_type", "ios");
        cfg.native.insert(
            "ios_multi_cpus".into(),
            crate::NativeSetting {
                raw: "sim_arm64,arm64".into(),
                value: crate::build_options::ConfigValue::List(vec![
                    "sim_arm64".into(),
                    "arm64".into(),
                ]),
            },
        );
        set_scalar(&mut cfg, "ios_sdk_version", "17.4");
        set_scalar(&mut cfg, "ios_minimum_os", "16.0");
        set_scalar(&mut cfg, "xcode_version", "15.3.0");

        let apple = fragment_checked("apple", &cfg).unwrap().unwrap();
        assert_eq!(struct_field(&apple, "apple_platform_type"), &sv("ios"));
        assert_eq!(struct_field(&apple, "single_arch_cpu"), &sv("arm64"));
        let plat = struct_field(&apple, "single_arch_platform");
        assert_eq!(struct_field(plat, "name"), &sv("ios_simulator"));
        assert_eq!(struct_field(plat, "is_device"), &Value::Bool(false));
        assert_eq!(
            struct_field(&apple, "ios_sdk_version_flag"),
            &Value::Host("dotted_version".into(), "17.4".into())
        );
        assert_eq!(
            struct_field(&apple, "ios_minimum_os_flag"),
            &Value::Host("dotted_version".into(), "16.0".into())
        );
        assert_eq!(
            struct_field(&apple, "xcode_version_flag"),
            &Value::Host("dotted_version".into(), "15.3".into())
        );
        assert_eq!(
            late_bound_label("apple", "xcode_config_label", &cfg).unwrap(),
            Label::parse_canonical("@@bazel_tools//tools/cpp:host_xcodes").unwrap()
        );
        let mut core_cfg = BuildConfigurationKey::default();
        core_cfg.core_only = true;
        assert_eq!(
            late_bound_label("apple", "xcode_config_label", &core_cfg).unwrap(),
            Label::parse_canonical("@@bazel_tools//tools/objc:host_xcodes").unwrap()
        );
    }

    #[test]
    fn apple_watchos_simulator_vs_device_and_invalid_cpu() {
        let mut cfg = BuildConfigurationKey::default();
        set_scalar(&mut cfg, "apple_platform_type", "watchos");
        cfg.native.insert(
            "watchos_cpus".into(),
            crate::NativeSetting {
                raw: "arm64".into(),
                value: crate::build_options::ConfigValue::List(vec!["arm64".into()]),
            },
        );
        let w_sim = fragment_checked("apple", &cfg).unwrap().unwrap();
        assert_eq!(
            struct_field(struct_field(&w_sim, "single_arch_platform"), "name"),
            &sv("watchos_simulator")
        );

        let mut cfg_dev = BuildConfigurationKey::default();
        set_scalar(&mut cfg_dev, "apple_platform_type", "watchos");
        set_scalar(&mut cfg_dev, "apple_split_cpu", "device_arm64");
        let w_dev = fragment_checked("apple", &cfg_dev).unwrap().unwrap();
        assert_eq!(struct_field(&w_dev, "single_arch_cpu"), &sv("arm64"));
        assert_eq!(
            struct_field(struct_field(&w_dev, "single_arch_platform"), "name"),
            &sv("watchos_device")
        );

        let mut bad = BuildConfigurationKey::default();
        set_scalar(&mut bad, "apple_platform_type", "catalyst");
        bad.native.insert(
            "catalyst_cpus".into(),
            crate::NativeSetting {
                raw: "arm64".into(),
                value: crate::build_options::ConfigValue::List(vec!["arm64".into()]),
            },
        );
        assert!(fragment_checked("apple", &bad).is_err());
    }

    #[test]
    fn objc_fragment_copts_and_simulator_fields() {
        let mut cfg = BuildConfigurationKey::default();
        cfg.compilation_mode = "fastbuild".into();
        set_scalar(&mut cfg, "ios_simulator_device", "iPhone 15");
        set_scalar(&mut cfg, "ios_simulator_version", "17.2");
        set_scalar(&mut cfg, "ios_memleaks", "true");
        let objc = fragment_checked("objc", &cfg).unwrap().unwrap();
        assert_eq!(
            struct_field(&objc, "copts_for_current_compilation_mode"),
            &mk_list(vec![sv("-O0"), sv("-DDEBUG=1")])
        );
        assert_eq!(struct_field(&objc, "ios_simulator_device"), &sv("iPhone 15"));
        assert_eq!(
            struct_field(&objc, "ios_simulator_version"),
            &Value::Host("dotted_version".into(), "17.2".into())
        );
        assert_eq!(struct_field(&objc, "run_memleaks"), &Value::Bool(true));
        assert_eq!(
            struct_field(&objc, "uses_device_debug_entitlements"),
            &Value::Bool(true)
        );

        cfg.compilation_mode = "opt".into();
        let objc_opt = fragment_checked("objc", &cfg).unwrap().unwrap();
        assert_eq!(
            struct_field(&objc_opt, "uses_device_debug_entitlements"),
            &Value::Bool(false)
        );
    }

    #[test]
    fn proto_fragment_unsplit_protoc_opts_and_strict_deps() {
        let mut cfg = BuildConfigurationKey::default();
        cfg.native.insert(
            "protocopt".into(),
            crate::NativeSetting {
                raw: "--foo=a,b,".into(),
                value: crate::build_options::ConfigValue::List(vec![
                    "--foo=a,b".into(),
                    "".into(),
                ]),
            },
        );
        set_scalar(&mut cfg, "strict_proto_deps", "strict");
        set_scalar(&mut cfg, "strict_public_imports", "error");

        let proto = fragment_checked("proto", &cfg).unwrap().unwrap();
        assert_eq!(
            struct_field(&proto, "experimental_protoc_opts"),
            &mk_list(vec![sv("--foo=a,b"), sv("")])
        );
        assert_eq!(
            strict_deps_mode_upper(&cfg, "strict_proto_deps", "error").unwrap(),
            "STRICT"
        );
        assert_eq!(
            strict_deps_mode_upper(&cfg, "strict_public_imports", "off").unwrap(),
            "ERROR"
        );
        let lbl = Label::parse_canonical("//pkg:my_proto").unwrap();
        assert_eq!(
            direct_dependencies_violation_msg(&lbl),
            "--direct_dependencies_violation_msg=%s is imported, but //pkg:my_proto doesn't directly depend on a proto_library that 'srcs' it."
        );
    }
}
