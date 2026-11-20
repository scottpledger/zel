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

pub(crate) struct Opts<'a> {
    pub(crate) cfg: &'a BuildConfigurationKey,
    bo: crate::build_options::BuildOptions,
}

impl<'a> Opts<'a> {
    pub(crate) fn new(cfg: &'a BuildConfigurationKey) -> Self {
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

    pub(crate) fn b(&self, k: &str, d: bool) -> bool {
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

    /// Repeatable flag list without comma-splitting (`--copt`, `--protocopt`, `--javacopt`,
    /// `--jvmopt`, etc.).
    pub(crate) fn list_unsplit(&self, k: &str) -> Vec<String> {
        if !self.cfg.is_exec && self.cfg.has_options_override(k) {
            if let Some(v) = self.cfg.options.get(k) {
                return crate::build_options::decode_unsplit_list(v);
            }
        }
        if let Some(crate::build_options::ConfigValue::List(vs)) = self.bo.get(k) {
            return vs.clone();
        }
        if let Some(v) = self.cfg.options.get(k) {
            return crate::build_options::decode_unsplit_list(v);
        }
        Vec::new()
    }

    /// Raw or `BuildOptions`-converted `PerLabelOptions` entries (`--per_file_copt`,
    /// `--host_per_file_copt`, `--per_file_ltobackendopt`), preferring raw `filter@opts` strings
    /// when available so original regex filters and comma-space option values are preserved.
    pub(crate) fn per_label_list(&self, k: &str) -> Vec<String> {
        if self.cfg.is_exec {
            if k == "per_file_copt" {
                if self.cfg.has_options_override("host_per_file_copt") {
                    if let Some(v) = self.cfg.options.get("host_per_file_copt") {
                        return crate::build_options::decode_unsplit_list(v);
                    }
                }
                if let Some(n) = self.cfg.native.get("host_per_file_copt") {
                    if !n.raw.is_empty() {
                        return crate::build_options::decode_unsplit_list(&n.raw);
                    }
                }
            }
            return self.list_unsplit(k);
        }
        if self.cfg.has_options_override(k) {
            if let Some(v) = self.cfg.options.get(k) {
                return crate::build_options::decode_unsplit_list(v);
            }
        }
        if let Some(n) = self.cfg.native.get(k) {
            if !n.raw.is_empty() {
                return crate::build_options::decode_unsplit_list(&n.raw);
            }
        }
        self.list_unsplit(k)
    }

    #[allow(dead_code)]
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
        format!(
            "darwin_{}",
            raw_cpu.strip_prefix("darwin_").unwrap_or(&raw_cpu)
        )
    } else {
        let prefix = format!("{ptype}_");
        format!(
            "{ptype}_{}",
            raw_cpu.strip_prefix(&prefix).unwrap_or(&raw_cpu)
        )
    };
    apple_platform_for_target_cpu(&target_cpu)
}

/// Validates and normalizes a `StrictDepsMode` option (`ERROR`, `OFF`, `WARN`, `STRICT`, `DEFAULT`).
pub fn strict_deps_mode_upper(
    cfg: &BuildConfigurationKey,
    option: &str,
    default: &str,
) -> Result<String, String> {
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

/// The Starlark value of fragment `name` in `cfg`; `Ok(None)` for unknown fragments,
/// or `Err` when the configuration options for `name` are invalid.
pub fn fragment(name: &str, cfg: &BuildConfigurationKey) -> Result<Option<Value>, String> {
    fragment_checked(name, cfg)
}

/// Validates all configuration fragments declared by a rule or aspect (`fragments = [...]`).
pub fn validate_declared_fragments(
    fragments: &[String],
    cfg: &BuildConfigurationKey,
) -> Result<(), String> {
    for name in fragments {
        if name != "coverage" {
            fragment_checked(name, cfg)?;
        }
    }
    Ok(())
}

/// Parses Bazel's `--grte_top` (`LibcTopLabelConverter`): `"default"` or `""` -> `None`,
/// otherwise must start with `//` or `@` and rewrites target name to `:everything`.
pub fn parse_libc_top_label(raw: &str) -> Result<Option<Label>, String> {
    if raw.is_empty() || raw == "default" {
        return Ok(None);
    }
    if !raw.starts_with("//") && !raw.starts_with('@') {
        return Err(format!(
            "Invalid --grte_top value '{raw}': label must start with '//' or '@'"
        ));
    }
    let lbl = Label::parse_canonical(raw)
        .map_err(|e| format!("Invalid --grte_top label '{raw}': {e}"))?;
    let rewritten = Label::create(lbl.package_identifier().clone(), "everything")
        .map_err(|e| format!("Invalid --grte_top label '{raw}': {e}"))?;
    Ok(Some(rewritten))
}

/// Tokenizes Java options (`ShellUtils.tokenize` on UTF-16 code units): splits only on ASCII
/// space and tab, and inside `"..."` retains `\` unless followed by `\` or `"`.
pub fn tokenize_java_options(s: &str) -> Result<Vec<String>, String> {
    let units: Vec<u16> = s.encode_utf16().collect();
    let mut out = Vec::new();
    let mut cur: Vec<u16> = Vec::new();
    let mut has_tok = false;
    let mut quote: Option<u16> = None;
    let mut i = 0;
    while i < units.len() {
        let u = units[i];
        if let Some(q) = quote {
            if u == q {
                quote = None;
            } else if q == u16::from(b'"') && u == u16::from(b'\\') {
                if i + 1 >= units.len() {
                    return Err("unterminated quotation".to_string());
                }
                let next = units[i + 1];
                if next == u16::from(b'\\') || next == u16::from(b'"') {
                    cur.push(next);
                    i += 1;
                } else {
                    cur.push(u);
                }
            } else {
                cur.push(u);
            }
        } else if u == u16::from(b' ') || u == u16::from(b'\t') {
            if has_tok {
                out.push(String::from_utf16_lossy(&cur));
                cur.clear();
                has_tok = false;
            }
        } else if u == u16::from(b'\'') || u == u16::from(b'"') {
            quote = Some(u);
            has_tok = true;
        } else if u == u16::from(b'\\') {
            i += 1;
            if i >= units.len() {
                return Err("unterminated backslash escape".to_string());
            }
            cur.push(units[i]);
            has_tok = true;
        } else {
            cur.push(u);
            has_tok = true;
        }
        i += 1;
    }
    if quote.is_some() {
        return Err("unterminated quotation".to_string());
    }
    if has_tok {
        out.push(String::from_utf16_lossy(&cur));
    }
    Ok(out)
}

/// Escapes and joins tokenized Java options into a single shell-quoted string
/// (`JavaHelper.detokenizeJavaOptions`).
pub fn detokenize_java_options(opts: &[String]) -> String {
    opts.iter()
        .map(|t| {
            if !t.is_empty()
                && t.bytes().all(|b| {
                    b.is_ascii_alphanumeric()
                        || matches!(
                            b,
                            b'_' | b'-' | b'.' | b'/' | b':' | b'=' | b'+' | b',' | b'@'
                        )
                })
            {
                t.clone()
            } else {
                format!("'{}'", t.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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
            let raw_copts = o.list_unsplit("copt");
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
            copts.extend(raw_copts.iter().cloned().map(Value::String));
            let mut linkopts: Vec<Value> = o
                .list_unsplit("linkopt")
                .into_iter()
                .map(Value::String)
                .collect();
            if omitfp {
                linkopts.push(sv("-Wl,--eh-frame-hdr"));
            }

            let fdo_instrument = o.raw("fdo_instrument").filter(|s| !s.is_empty());
            let cs_fdo_instrument = o.raw("cs_fdo_instrument").filter(|s| !s.is_empty());
            let fdo_optimize_raw = o.raw("fdo_optimize").filter(|s| !s.is_empty());
            let fdo_profile = o.label("fdo_profile");
            let cs_fdo_profile = o.label("cs_fdo_profile");
            let cs_fdo_abs = o.raw("cs_fdo_absolute_path").filter(|s| !s.is_empty());
            let prop_cc = o
                .raw("propeller_optimize_absolute_cc_profile")
                .filter(|s| !s.is_empty());
            let prop_ld = o
                .raw("propeller_optimize_absolute_ld_profile")
                .filter(|s| !s.is_empty());

            if fdo_instrument.is_some() && cs_fdo_instrument.is_some() {
                return Err("Cannot instrument both regular and CS FDO".into());
            }
            if fdo_optimize_raw.is_some() && !matches!(fdo_profile, Value::None) {
                return Err("Cannot --fdo_optimize and --fdo_profile at the same time".into());
            }
            let any_fdo_opt = fdo_optimize_raw.is_some()
                || !matches!(fdo_profile, Value::None)
                || !matches!(cs_fdo_profile, Value::None)
                || cs_fdo_abs.is_some();
            if (fdo_instrument.is_some() || cs_fdo_instrument.is_some()) && any_fdo_opt {
                return Err("Cannot instrument and optimize for FDO at the same time".into());
            }
            if fdo_instrument.is_some() && !raw_copts.iter().any(|c| c == "-Wno-error") {
                return Err("Cannot instrument FDO without --copt=-Wno-error".into());
            }

            let (fdo_optimize_val, fdo_path_val) = match &fdo_optimize_raw {
                Some(s) if s.starts_with("//") || s.starts_with('@') => {
                    let lbl = Label::parse_canonical(s)
                        .map_err(|e| format!("Invalid --fdo_optimize label '{s}': {e}"))?;
                    (Value::Label(lbl), Value::None)
                }
                Some(s) => (Value::None, sv(s)),
                None => (Value::None, Value::None),
            };
            let propeller_optimize = if prop_cc.is_some()
                || prop_ld.is_some()
                || fdo_instrument.is_some()
                || cs_fdo_instrument.is_some()
            {
                Value::None
            } else {
                o.label("propeller_optimize")
            };
            let xbinary_fdo = if fdo_optimize_raw.is_some()
                || !matches!(fdo_profile, Value::None)
                || fdo_instrument.is_some()
                || cs_fdo_instrument.is_some()
            {
                Value::None
            } else {
                o.label("xbinary_fdo")
            };
            let libc_top_val = match o.raw("grte_top") {
                Some(raw) => parse_libc_top_label(&raw)?
                    .map(Value::Label)
                    .unwrap_or(Value::None),
                None => Value::None,
            };

            mk(vec![
                // Struct fields.
                f("copts", mk_list(copts)),
                f("cxxopts", o.list_unsplit_v("cxxopt")),
                f("conlyopts", o.list_unsplit_v("conlyopt")),
                f("objccopts", o.list_unsplit_v("objccopt")),
                f("linkopts", mk_list(linkopts)),
                f("lto_backend_options", o.list_unsplit_v("ltobackendopt")),
                f(
                    "_dont_enable_host_nonhost",
                    Value::Bool(o.b(
                        "incompatible_dont_enable_host_nonhost_crosstool_features",
                        true,
                    )),
                ),
                f("custom_malloc", o.label("custom_malloc")),
                f("fdo_optimize", fdo_optimize_val),
                f("fdo_prefetch_hints", o.label("fdo_prefetch_hints")),
                f("fdo_profile", fdo_profile),
                f("cs_fdo_profile", cs_fdo_profile),
                f("propeller_optimize", propeller_optimize),
                f("xbinary_fdo", xbinary_fdo),
                f("memprof_profile", o.label("memprof_profile")),
                f("libc_top", libc_top_val.clone()),
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
                m("lto_index_options", o.list_unsplit_v("ltoindexopt")),
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
                m("fdo_path", fdo_path_val),
                m(
                    "cs_fdo_path",
                    cs_fdo_abs.map(|s| sv(&s)).unwrap_or(Value::None),
                ),
                m(
                    "fdo_instrument",
                    fdo_instrument.map(|s| sv(&s)).unwrap_or(Value::None),
                ),
                m(
                    "cs_fdo_instrument",
                    cs_fdo_instrument.map(|s| sv(&s)).unwrap_or(Value::None),
                ),
                m(
                    "propeller_optimize_absolute_cc_profile",
                    prop_cc.map(|s| sv(&s)).unwrap_or(Value::None),
                ),
                m(
                    "propeller_optimize_absolute_ld_profile",
                    prop_ld.map(|s| sv(&s)).unwrap_or(Value::None),
                ),
                m("_fdo_prefetch_hints_label", o.label("fdo_prefetch_hints")),
                m(
                    "use_llvm_coverage_map_format",
                    Value::Bool(o.b("experimental_use_llvm_covmap", false)),
                ),
                m(
                    "generate_llvm_lcov",
                    Value::Bool(o.b("experimental_generate_llvm_lcov", false)),
                ),
                m(
                    "incompatible_use_specific_tool_files",
                    Value::Bool(o.b("incompatible_use_specific_tool_files", true)),
                ),
                m(
                    "include_scanning",
                    Value::Bool(o.b("include_scanning", false)),
                ),
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
                m(
                    "experimental_cc_implementation_deps",
                    Value::Bool(o.b("experimental_cc_implementation_deps", true)),
                ),
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
                m("strip_opts", o.list_unsplit_v("stripopt")),
                m("grte_top", libc_top_val),
                m(
                    "disable_nocopts",
                    Value::Bool(o.b("incompatible_disable_nocopts", true)),
                ),
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
            let mut flags: Vec<String> = Vec::new();
            for raw_opt in o.list_unsplit("javacopt") {
                match tokenize_java_options(&raw_opt) {
                    Ok(toks) => flags.extend(toks),
                    Err(_) => flags.push(raw_opt),
                }
            }
            let depset_direct = if flags.is_empty() {
                Vec::new()
            } else {
                vec![Value::String(detokenize_java_options(&flags))]
            };
            let reduce_cp = o
                .s("experimental_java_classpath", "bazel")
                .to_ascii_uppercase();
            let gen_java_deps = o.b("java_deps", true) || reduce_cp != "OFF";
            let root_pkg = zel_cmdline::PackageIdentifier::create_in_main_repo("")
                .map_err(|e| e.to_string())?;
            let empty_map = zel_cmdline::RepositoryMapping::always_fallback();
            let mut plugin_labels = Vec::new();
            for p in o.list("plugin") {
                let norm_p = if p.starts_with("//") || p.starts_with('@') {
                    p.clone()
                } else {
                    format!("//{p}")
                };
                let lbl = Label::parse_with_package_context(&norm_p, &root_pkg, &empty_map)
                    .or_else(|_| Label::parse_canonical(&norm_p))
                    .map_err(|e| format!("Invalid --plugin label '{p}': {e}"))?;
                plugin_labels.push(Value::Label(lbl));
            }
            mk(vec![
                // Struct fields.
                f(
                    "default_javac_flags",
                    mk_list(flags.into_iter().map(Value::String).collect()),
                ),
                f(
                    "default_javac_flags_depset",
                    Value::Depset(Arc::new(
                        Depset::new(DepsetOrder::Preorder, depset_direct, vec![])
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
                f(
                    "multi_release_deploy_jars",
                    Value::Bool(o.b("incompatible_multi_release_deploy_jars", true)),
                ),
                f(
                    "run_android_lint",
                    Value::Bool(o.b("experimental_run_android_lint_on_java_rules", false)),
                ),
                f("default_jvm_opts", o.list_unsplit_v("jvmopt")),
                f("plugins", mk_list(plugin_labels)),
                f("launcher", o.label("java_launcher")),
                // Methods.
                m("use_ijars", Value::Bool(o.b("use_ijars", true))),
                m(
                    "use_header_compilation",
                    Value::Bool(o.b("java_header_compilation", true)),
                ),
                m("use_header_compilation_direct_deps", Value::Bool(true)),
                m("reduce_java_classpath", sv(&reduce_cp)),
                m(
                    "enforce_explicit_java_test_deps",
                    Value::Bool(o.b("explicit_java_test_deps", false)),
                ),
                m("generate_java_deps", Value::Bool(gen_java_deps)),
                m(
                    "disallow_java_import_exports",
                    Value::Bool(o.b("incompatible_disallow_java_import_exports", false)),
                ),
                m(
                    "auto_create_java_test_deploy_jars",
                    Value::Bool(o.b("experimental_java_test_auto_create_deploy_jar", false)),
                ),
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
        "py" => {
            let build_zip = match o.raw("build_python_zip") {
                Some(v) => match v.trim().to_ascii_lowercase().as_str() {
                    "true" | "yes" | "1" => true,
                    "false" | "no" | "0" => false,
                    _ => cfg_is_windows(),
                },
                None => cfg_is_windows(),
            };
            mk_struct(vec![
                ("default_python_version", sv("PY3")),
                ("build_python_zip", Value::Bool(build_zip)),
                (
                    "default_to_explicit_init_py",
                    Value::Bool(o.b("incompatible_default_to_explicit_init_py", false)),
                ),
            ])
        }
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
                f("signing_certificate_name", o.opt_s("ios_signing_cert_name")),
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
        ("proto", "proto_toolchain_for_j2objc") => opt("proto_toolchain_for_j2objc")
            .or_else(|| tools("tools/j2objc:j2objc_proto_toolchain")),
        ("cpp", "crosstool_top") => {
            if o.has_explicit("crosstool_top")
                || !o.b("incompatible_enable_cc_toolchain_resolution", true)
            {
                opt("crosstool_top")
            } else {
                None
            }
        }
        ("cpp", "libc_top") => o
            .raw("grte_top")
            .and_then(|raw| parse_libc_top_label(&raw).ok().flatten()),
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
        ("py", "native_rules_allowlist") => opt("python_native_rules_allowlist"),
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
        let core_cfg = BuildConfigurationKey {
            core_only: true,
            ..Default::default()
        };
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
        let mut cfg = BuildConfigurationKey {
            compilation_mode: "fastbuild".into(),
            ..Default::default()
        };
        set_scalar(&mut cfg, "ios_simulator_device", "iPhone 15");
        set_scalar(&mut cfg, "ios_simulator_version", "17.2");
        set_scalar(&mut cfg, "ios_memleaks", "true");
        let objc = fragment_checked("objc", &cfg).unwrap().unwrap();
        assert_eq!(
            struct_field(&objc, "copts_for_current_compilation_mode"),
            &mk_list(vec![sv("-O0"), sv("-DDEBUG=1")])
        );
        assert_eq!(
            struct_field(&objc, "ios_simulator_device"),
            &sv("iPhone 15")
        );
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
                value: crate::build_options::ConfigValue::List(vec!["--foo=a,b".into(), "".into()]),
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

    #[test]
    fn cpp_fragment_unsplit_opts_grte_top_and_fdo_validation() {
        let mut cfg = BuildConfigurationKey::default();
        cfg.native.insert(
            "copt".into(),
            crate::NativeSetting {
                raw: "-Wl,-rpath,/opt".into(),
                value: crate::build_options::ConfigValue::List(vec!["-Wl,-rpath,/opt".into()]),
            },
        );
        set_scalar(&mut cfg, "experimental_omitfp", "true");
        set_scalar(&mut cfg, "grte_top", "//sysroot/glibc:custom");
        let cpp = fragment_checked("cpp", &cfg).unwrap().unwrap();
        assert_eq!(
            struct_field(&cpp, "copts"),
            &mk_list(vec![
                sv("-fomit-frame-pointer"),
                sv("-fasynchronous-unwind-tables"),
                sv("-DNO_FRAME_POINTER"),
                sv("-Wl,-rpath,/opt"),
            ])
        );
        assert_eq!(
            struct_field(&cpp, "linkopts"),
            &mk_list(vec![sv("-Wl,--eh-frame-hdr")])
        );
        assert_eq!(
            struct_field(&cpp, "libc_top"),
            &Value::Label(Label::parse_canonical("@@//sysroot/glibc:everything").unwrap())
        );

        set_scalar(&mut cfg, "grte_top", "default");
        let cpp_def = fragment_checked("cpp", &cfg).unwrap().unwrap();
        assert_eq!(struct_field(&cpp_def, "libc_top"), &Value::None);

        let mut fdo_bad = BuildConfigurationKey::default();
        set_scalar(&mut fdo_bad, "fdo_instrument", "prof");
        assert!(fragment_checked("cpp", &fdo_bad).is_err());

        fdo_bad.native.insert(
            "copt".into(),
            crate::NativeSetting {
                raw: "-Wno-error".into(),
                value: crate::build_options::ConfigValue::List(vec!["-Wno-error".into()]),
            },
        );
        assert!(fragment_checked("cpp", &fdo_bad).is_ok());
    }

    #[test]
    fn java_fragment_tokenize_options_depset_and_plugins() {
        assert_eq!(
            tokenize_java_options("a\nb \"\\n\\\"\\\\\" 'x y'").unwrap(),
            vec!["a\nb", "\\n\"\\", "x y"]
        );
        let mut cfg = BuildConfigurationKey::default();
        cfg.native.insert(
            "javacopt".into(),
            crate::NativeSetting {
                raw: "-Xlint:all,serial -Werror".into(),
                value: crate::build_options::ConfigValue::List(vec![
                    "-Xlint:all,serial".into(),
                    "-Werror".into(),
                ]),
            },
        );
        cfg.native.insert(
            "jvmopt".into(),
            crate::NativeSetting {
                raw: "-Dfoo=a,b".into(),
                value: crate::build_options::ConfigValue::List(vec!["-Dfoo=a,b".into(), "".into()]),
            },
        );
        set_scalar(&mut cfg, "java_deps", "false");
        set_scalar(&mut cfg, "experimental_java_classpath", "bazel");
        set_scalar(&mut cfg, "plugin", "tools:my_plugin");
        set_scalar(&mut cfg, "java_launcher", "//tools/jdk:custom_launcher");

        let java = fragment_checked("java", &cfg).unwrap().unwrap();
        assert_eq!(
            struct_field(&java, "default_javac_flags"),
            &mk_list(vec![sv("-Xlint:all,serial"), sv("-Werror")])
        );
        let Value::Depset(ds) = struct_field(&java, "default_javac_flags_depset") else {
            panic!("expected depset");
        };
        assert_eq!(ds.order, DepsetOrder::Preorder);
        assert_eq!(ds.to_list(), vec![sv("-Xlint:all,serial -Werror")]);
        assert_eq!(
            struct_field(&java, "default_jvm_opts"),
            &mk_list(vec![sv("-Dfoo=a,b"), sv("")])
        );
        assert_eq!(
            struct_field(&java, "plugins"),
            &mk_list(vec![Value::Label(
                Label::parse_canonical("@@//tools:my_plugin").unwrap()
            )])
        );
        assert_eq!(
            late_bound_label("java", "launcher", &cfg),
            Some(Label::parse_canonical("@@//tools/jdk:custom_launcher").unwrap())
        );
    }

    #[test]
    fn py_fragment_build_python_zip_tristate_and_explicit_init_py() {
        let mut cfg = BuildConfigurationKey::default();
        set_scalar(&mut cfg, "build_python_zip", "yes");
        set_scalar(&mut cfg, "incompatible_default_to_explicit_init_py", "true");
        set_scalar(
            &mut cfg,
            "python_native_rules_allowlist",
            "//tools/py:allowlist",
        );
        let py = fragment_checked("py", &cfg).unwrap().unwrap();
        assert_eq!(struct_field(&py, "build_python_zip"), &Value::Bool(true));
        assert_eq!(
            struct_field(&py, "default_to_explicit_init_py"),
            &Value::Bool(true)
        );
        assert_eq!(
            late_bound_label("py", "native_rules_allowlist", &cfg),
            Some(Label::parse_canonical("@@//tools/py:allowlist").unwrap())
        );

        set_scalar(&mut cfg, "build_python_zip", "no");
        let py_no = fragment_checked("py", &cfg).unwrap().unwrap();
        assert_eq!(
            struct_field(&py_no, "build_python_zip"),
            &Value::Bool(false)
        );
    }

    #[test]
    fn fragment_helper_and_declared_fragments_surface_validation_errors() {
        let ok_cfg = BuildConfigurationKey::default();
        assert!(fragment("unknown_fragment", &ok_cfg).unwrap().is_none());
        assert!(fragment("cpp", &ok_cfg).unwrap().is_some());

        let mut bad_fdo = BuildConfigurationKey::default();
        set_scalar(&mut bad_fdo, "fdo_instrument", "prof_a");
        set_scalar(&mut bad_fdo, "cs_fdo_instrument", "prof_b");
        let fdo_err = fragment("cpp", &bad_fdo).unwrap_err();
        assert!(
            fdo_err.contains("Cannot instrument both regular and CS FDO"),
            "{fdo_err}"
        );
        assert_eq!(
            validate_declared_fragments(&["coverage".into(), "cpp".into()], &bad_fdo).unwrap_err(),
            fdo_err
        );

        let mut bad_apple = BuildConfigurationKey::default();
        set_scalar(&mut bad_apple, "apple_platform_type", "catalyst");
        bad_apple.native.insert(
            "catalyst_cpus".into(),
            crate::NativeSetting {
                raw: "arm64".into(),
                value: crate::build_options::ConfigValue::List(vec!["arm64".into()]),
            },
        );
        let apple_err = fragment("apple", &bad_apple).unwrap_err();
        assert!(apple_err.contains("catalyst_arm64"), "{apple_err}");

        let bad_objc = BuildConfigurationKey::target("darwin_arm64", "bogus_mode");
        let objc_err = fragment("objc", &bad_objc).unwrap_err();
        assert!(
            objc_err.contains("Invalid compilation mode: 'bogus_mode'"),
            "{objc_err}"
        );
    }

    #[test]
    fn list_unsplit_preserves_commas_across_build_options_options_map_and_transitions() {
        // 1. CLI BuildOptions + mirrored comma-joined cfg.options (as analysis_phase constructs).
        let mut cli_cfg = BuildConfigurationKey::target("k8", "fastbuild");
        cli_cfg
            .options
            .insert("copt".into(), "-Wl,-rpath,/opt,-O2".into());
        cli_cfg.native.insert(
            "copt".into(),
            crate::NativeSetting {
                raw: "-Wl,-rpath,/opt,-O2".into(),
                value: crate::build_options::ConfigValue::List(vec![
                    "-Wl,-rpath,/opt".into(),
                    "-O2".into(),
                ]),
            },
        );
        cli_cfg
            .options
            .insert("host_copt".into(), "-Wl,-rpath,/host".into());
        cli_cfg.native.insert(
            "host_copt".into(),
            crate::NativeSetting {
                raw: "-Wl,-rpath,/host".into(),
                value: crate::build_options::ConfigValue::List(vec!["-Wl,-rpath,/host".into()]),
            },
        );
        assert_eq!(
            Opts::new(&cli_cfg).list_unsplit("copt"),
            vec!["-Wl,-rpath,/opt".to_string(), "-O2".to_string()]
        );

        // Exec configuration uses host_copt + -g0 rather than target cfg.options["copt"].
        let exec_cfg = cli_cfg.to_exec();
        let expected_exec = if cfg!(windows) {
            vec!["-Wl,-rpath,/host".to_string()]
        } else {
            vec!["-Wl,-rpath,/host".to_string(), "-g0".to_string()]
        };
        assert_eq!(Opts::new(&exec_cfg).list_unsplit("copt"), expected_exec);

        // 2. Direct cfg.options string map without BuildOptions / cfg.native.
        let mut map_only = BuildConfigurationKey::target("k8", "fastbuild");
        map_only
            .options
            .insert("copt".into(), "-Wl,-rpath,/opt".into());
        map_only
            .options
            .insert("custom_unsplit".into(), "-Wl,-rpath,/custom".into());
        assert_eq!(
            Opts::new(&map_only).list_unsplit("copt"),
            vec!["-Wl,-rpath,/opt".to_string()]
        );
        assert_eq!(
            Opts::new(&map_only).list_unsplit("custom_unsplit"),
            vec!["-Wl,-rpath,/custom".to_string()]
        );

        // 3. Direct cfg.options override on top of an existing BuildOptions / cfg.native entry.
        let mut overridden = cli_cfg.clone();
        overridden
            .options
            .insert("copt".into(), "-Wl,-rpath,/override".into());
        assert_eq!(
            Opts::new(&overridden).list_unsplit("copt"),
            vec!["-Wl,-rpath,/override".to_string()]
        );

        // 4. Starlark transition mutating //command_line_option:copt and javacopt with commas.
        let mut transitioned = cli_cfg.clone();
        crate::transition::set_setting(
            &mut transitioned,
            "//command_line_option:copt",
            &mk_list(vec![sv("-Wl,-rpath,/trans"), sv("-DFOO=a, b"), sv("-O3")]),
        )
        .unwrap();
        assert_eq!(
            Opts::new(&transitioned).list_unsplit("copt"),
            vec![
                "-Wl,-rpath,/trans".to_string(),
                "-DFOO=a, b".to_string(),
                "-O3".to_string(),
            ]
        );
        assert_eq!(
            transitioned.build_options().get("copt"),
            Some(&crate::build_options::ConfigValue::List(vec![
                "-Wl,-rpath,/trans".to_string(),
                "-DFOO=a, b".to_string(),
                "-O3".to_string(),
            ]))
        );
    }
}
