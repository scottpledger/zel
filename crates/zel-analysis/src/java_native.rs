// Analysis-time natives of `_builtins.internal.java_common_internal_do_not_use`
// (`JavaStarlarkCommon.java`) that need the rule context. Included into `starlark_rule.rs`.

fn java_native(
    st: &Arc<CtxState>,
    name: &str,
    pos: &[Value],
    kw: &BTreeMap<String, Value>,
) -> Option<HostResult> {
    let n = name.strip_prefix("builtins.").unwrap_or(name).strip_prefix("java_common_internal_do_not_use.")?;
    let a = |i: usize, k: &str| kw.get(k).or_else(|| pos.get(i));
    Some(match n {
        "_check_java_toolchain_is_declared_on_rule" => Ok(Value::None),
        "target_kind" => Ok(Value::String(
            a(0, "target").and_then(target_of).map(|t| t.rule_class.clone()).unwrap_or_default(),
        )),
        "get_runtime_classpath_for_archive" => {
            let cp = a(0, "runtime_classpath").cloned().unwrap_or(Value::None);
            let ex: Vec<Artifact> = cc_artifacts(a(1, "excluded_artifacts"));
            if ex.is_empty() {
                Ok(cp)
            } else {
                let keep: Vec<Value> = cc_features::items(&cp)
                    .into_iter()
                    .filter(|v| artifact_of(v).map(|x| !ex.contains(&x)).unwrap_or(true))
                    .collect();
                Depset::new(DepsetOrder::Default, keep, vec![]).map(|d| Value::Depset(Arc::new(d)))
            }
        }
        "check_provider_instances" => (|| {
            let providers = a(0, "providers").map(cc_features::items).unwrap_or_default();
            let what = a(1, "what").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
            let ptype = a(2, "provider_type").ok_or_else(|| StarlarkError("missing provider_type".into()))?;
            let want = zel_packages::provider::provider_key_of(ptype)
                .ok_or_else(|| StarlarkError(format!("wanted Provider, got {}", type_name(ptype))))?;
            for (i, e) in providers.iter().enumerate() {
                let ok = match e {
                    Value::ProviderInstance { provider_name, .. } => {
                        zel_packages::provider::resolve_instance_key(provider_name) == want
                    }
                    _ => false,
                };
                if !ok {
                    let got = match e {
                        Value::ProviderInstance { provider_name, .. } => {
                            zel_packages::provider::provider_display_name(provider_name).to_string()
                        }
                        o => type_name(o).to_string(),
                    };
                    return err(format!(
                        "at index {i} of {what}, got element of type {got}, want {}",
                        zel_packages::provider::provider_display_name(&want)
                    ));
                }
            }
            Ok(Value::None)
        })(),
        "collect_native_deps_dirs" => {
            let libs = a(0, "libraries").map(cc_features::items).unwrap_or_default();
            let mut dirs: Vec<String> = Vec::new();
            for l in libs {
                let Value::ProviderInstance { fields, .. } = &l else { continue };
                let f = ["interface_library", "dynamic_library"]
                    .iter()
                    .find_map(|k| fields.get(*k).and_then(artifact_of));
                let Some(art) = f else { continue };
                let fname = art.exec_path.path_string().rsplit('/').next().unwrap_or("").to_string();
                if fname.ends_with(".ifso") || fname.ends_with(".tbd") || fname.ends_with(".lib") {
                    continue;
                }
                let shared = fname.ends_with(".so") || fname.ends_with(".dylib") || fname.ends_with(".dll") || fname.contains(".so.");
                if !shared {
                    return Some(err(format!("not a shared library :{}", art.exec_path.path_string())));
                }
                let rp = art.root_relative_path.path_string().to_string();
                let dir = rp.rsplit_once('/').map(|x| x.0.to_string()).unwrap_or_default();
                if !dirs.contains(&dir) {
                    dirs.push(dir);
                }
            }
            Ok(mk_list(dirs.into_iter().map(Value::String).collect()))
        }
        "expand_java_opts" => (|| {
            let attr = a(1, "attr").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
            let tokenize = a(2, "tokenize").map(|v| v.is_truthy()).unwrap_or(false);
            let exec_paths = a(3, "exec_paths").map(|v| v.is_truthy()).unwrap_or(false);
            let raw: Vec<String> = match &st.attr_struct {
                Value::Struct(s) => match s.fields.get(&attr) {
                    Some(Value::List(l)) => {
                        let elems = &l.lock().unwrap().elements;
                        let mut strs = Vec::with_capacity(elems.len());
                        for elem in elems {
                            match elem.as_str() {
                                Some(st_s) => strs.push(st_s.to_string()),
                                None => {
                                    return err(format!(
                                        "expand_java_opts: attribute '{attr}' must be a list of strings"
                                    ))
                                }
                            }
                        }
                        strs
                    }
                    Some(other) => {
                        return err(format!(
                            "expand_java_opts: expected string list for attribute '{attr}', got {}",
                            type_name(other)
                        ))
                    }
                    None => {
                        return err(format!(
                            "expand_java_opts: rule has no string list attribute '{attr}'"
                        ))
                    }
                },
                _ => return err("expand_java_opts: invalid ctx.attr"),
            };
            let mut map: BTreeMap<Label, Vec<Artifact>> = BTreeMap::new();
            let mut dep_attrs = vec!["srcs", "deps", "implementation_deps", "tools", "resources"];
            if !exec_paths {
                dep_attrs.push("data");
            }
            for k in dep_attrs {
                if let Some(ts) = st.deps.get(k) {
                    for t in ts {
                        let files = if matches!(k, "deps" | "tools") {
                            if let Some(exe) = &t.get_actual().default_info.executable {
                                vec![exe.clone()]
                            } else {
                                target_files(t)
                            }
                        } else {
                            target_files(t)
                        };
                        insert_target_location_keys(&mut map, t, files);
                    }
                }
            }
            let pkg_prefix = st.pkg_dir.path_string();
            for out_art in &st.predeclared {
                let rr = out_art.root_relative_path.path_string();
                let rel_name = if pkg_prefix.is_empty() {
                    rr
                } else {
                    rr.strip_prefix(&format!("{pkg_prefix}/")).unwrap_or(rr)
                };
                if let Ok(out_lbl) = Label::create(st.label.package_identifier().clone(), rel_name) {
                    map.entry(out_lbl).or_default().push(out_art.clone());
                }
            }
            let mode = if exec_paths { LocationMode::Exec } else { LocationMode::Runfiles };
            let lookup = |n: &str| st.make_vars.get(n).cloned();
            let repo_map = st.repo_mapping();
            let mut out = Vec::new();
            for s in raw {
                let expanded = match expand_with_locations(
                    &s,
                    &lookup,
                    &map,
                    &st.label,
                    mode,
                    &st.inputs.workspace_name,
                    Some(&repo_map),
                    cfg!(windows),
                ) {
                    Ok(res) => res.text,
                    Err(e) => {
                        st.rule_errors.lock().unwrap().push(format!(
                            "in {attr} attribute of {} rule {}: {e}",
                            st.rule.rule_class,
                            st.label.canonical_form()
                        ));
                        s
                    }
                };
                if tokenize {
                    let mut toks = Vec::new();
                    if let Err(e) = crate::fragments::shell_tokenize_into(&mut toks, &expanded) {
                        st.rule_errors.lock().unwrap().push(format!(
                            "in {attr} attribute of {} rule {}: {e}",
                            st.rule.rule_class,
                            st.label.canonical_form()
                        ));
                    }
                    for t in toks {
                        out.push(Value::String(t));
                    }
                } else {
                    out.push(Value::String(expanded));
                }
            }
            Ok(mk_list(out))
        })(),
        "tokenize_javacopts" => {
            let opts = a(0, "options");
            Ok(mk_list(
                java_tokenize_opts(opts)
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ))
        }
        "create_header_compilation_action" => java_create_header_action(st, pos, kw),
        "create_compilation_action" => java_create_compile_action(st, pos, kw),
        "create_jvm_compile_action" => java_create_jvm_compile_action(st, pos, kw),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------------------------
// Java compile actions: JavaCompileActionBuilder / JavaHeaderCompileAction / JavaCompilationHelper.
// ---------------------------------------------------------------------------------------------

fn jf(v: &Value, k: &str) -> Option<Value> {
    field(v, k).filter(|x| !matches!(x, Value::None))
}

fn jstrs(v: Option<&Value>) -> Vec<String> {
    v.map(|v| items(v).iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// `JavaHelper.tokenizeJavaOptions`: reverses a `Depset`/`NestedSet` (preserving order for plain
/// sequences) and tokenizes each string on space/tab (`ShellUtils.tokenize` on UTF-16 code units).
fn java_tokenize_opts(v: Option<&Value>) -> Vec<String> {
    let mut raw = jstrs(v);
    if matches!(v, Some(Value::Depset(_))) {
        raw.reverse();
    }
    crate::fragments::java_helper_tokenize_options(&raw)
}

fn jpath(a: &Artifact) -> String {
    a.exec_path.path_string().to_string()
}

/// `CustomCommandLine.Builder` conveniences (flags are only added when they have values).
#[derive(Default)]
struct JCmd(Vec<String>);

impl JCmd {
    fn exec_path(&mut self, flag: &str, a: Option<&Artifact>) {
        if let Some(a) = a {
            self.0.push(flag.to_string());
            self.0.push(jpath(a));
        }
    }
    fn exec_paths(&mut self, flag: &str, arts: &[Artifact]) {
        if !arts.is_empty() {
            self.0.push(flag.to_string());
            self.0.extend(arts.iter().map(jpath));
        }
    }
    fn all(&mut self, flag: &str, vals: &[String]) {
        if !vals.is_empty() {
            self.0.push(flag.to_string());
            self.0.extend(vals.iter().cloned());
        }
    }
    fn opt(&mut self, flag: &str, v: Option<&str>) {
        if let Some(v) = v {
            self.0.push(flag.to_string());
            self.0.push(v.to_string());
        }
    }
}

fn param_info() -> crate::action::ParamFileInfo {
    // ParamFileInfo.builder(UNQUOTED).setUseAlways(true)
    crate::action::ParamFileInfo {
        flag_format: "@%s".to_string(),
        format: crate::action::ParamFileFormat::Multiline,
        use_always: true,
        leading_args: 0,
        flag_groups: Vec::new(),
        explicit_path: None,
    }
}

/// `JavaToolchainTool.getCommandLine()` and `.addInputs()`: the command prefix (`java -jar x.jar`
/// for jars) and the files the tool needs.
fn java_tool_command(tool: &Value, toolchain: &Value) -> Result<(Vec<String>, Vec<Artifact>), StarlarkError> {
    let ftr = jf(tool, "tool").ok_or_else(|| StarlarkError("java_toolchain tool has no 'tool'".into()))?;
    let Value::ProviderInstance { fields, .. } = &ftr else {
        return err("java_toolchain tool 'tool' is not a FilesToRunProvider");
    };
    let exe = fields
        .get("executable")
        .and_then(artifact_of)
        .ok_or_else(|| StarlarkError("java_toolchain tool does not have an executable".into()))?;
    let jvm_opts = jstrs(jf(tool, "jvm_opts").as_ref());
    let mut inputs: Vec<Artifact> = cc_artifacts(jf(tool, "data").as_ref());
    add_files_to_run_inputs(fields, &mut inputs);
    let mut cmd: Vec<String> = Vec::new();
    if exe.exec_path.path_string().ends_with(".jar") {
        let rt = jf(toolchain, "java_runtime").ok_or_else(|| StarlarkError("java_toolchain has no java_runtime".into()))?;
        let java = match jf(&rt, "java_executable_exec_path") {
            Some(Value::String(s)) => s,
            _ => return err("java_runtime has no java_executable_exec_path"),
        };
        cmd.push(java);
        cmd.extend(jstrs(jf(toolchain, "jvm_opt").as_ref()));
        cmd.extend(jvm_opts);
        cmd.push("-jar".into());
        cmd.push(jpath(&exe));
        inputs.extend(cc_artifacts(jf(&rt, "files").as_ref()));
    } else {
        cmd.push(jpath(&exe));
        cmd.extend(jvm_opts);
    }
    Ok((cmd, inputs))
}

fn java_label_arg(l: &Label) -> String {
    if l.package_identifier().repository().name().is_empty() {
        format!("//{}:{}", l.package_identifier().package_fragment().path_string(), l.name())
    } else {
        // `@`-prefixed strings are expanded as param files by the tools; escape with a second `@`.
        format!("@{}", l.canonical_form())
    }
}

fn is_platforms_os_setting(label: &Label) -> bool {
    let repo = label.package_identifier().repository().name();
    let pkg = label.package_identifier().package_fragment().path_string();
    pkg == "os"
        && label.name() == "os"
        && (repo == "platforms"
            || repo.starts_with("platforms+")
            || repo.starts_with("platforms~"))
}

fn is_darwin_os_value(label: &Label) -> bool {
    let repo = label.package_identifier().repository().name();
    let pkg = label.package_identifier().package_fragment().path_string();
    pkg == "os"
        && matches!(label.name(), "osx" | "macos")
        && (repo == "platforms"
            || repo.starts_with("platforms+")
            || repo.starts_with("platforms~"))
}

fn java_exec_platform_label<'a>(
    cfg: &'a BuildConfigurationKey,
    inputs: &'a AnalysisInputs,
) -> Option<&'a str> {
    if cfg
        .options
        .get("incompatible_auto_exec_groups")
        .is_some_and(|v| v == "true")
    {
        if let Some(ep) = inputs
            .exec_group_platforms
            .get("@@bazel_tools//tools/jdk:toolchain_type")
            .or_else(|| {
                inputs
                    .exec_group_platforms
                    .get("@bazel_tools//tools/jdk:toolchain_type")
            })
            .or_else(|| {
                inputs
                    .exec_group_platforms
                    .iter()
                    .find(|(k, _)| {
                        k.ends_with("//tools/jdk:toolchain_type")
                            || k.ends_with("//toolchains:toolchain_type")
                    })
                    .map(|(_, v)| v)
            })
        {
            return Some(ep.as_str());
        }
    }
    inputs
        .exec_platform
        .as_deref()
        .or_else(|| cfg.options.get("host_platform").map(String::as_str))
}

fn platform_info_os_is_macos(ct: &ConfiguredTarget) -> Option<bool> {
    let Value::ProviderInstance { fields, .. } = ct.providers.get("PlatformInfo")? else {
        return None;
    };
    let Value::Dict(d) = fields.get("constraints")? else {
        return None;
    };
    for (k, v) in d.lock().unwrap().items() {
        if let (Value::Label(setting), Value::Label(cv)) = (k, v) {
            if is_platforms_os_setting(&setting) {
                return Some(is_darwin_os_value(&cv));
            }
        }
    }
    None
}

fn utf8_env_for_inputs(
    cfg: &BuildConfigurationKey,
    inputs: &AnalysisInputs,
    host_is_macos: bool,
) -> BTreeMap<String, String> {
    let mac = java_exec_platform_label(cfg, inputs)
        .and_then(|raw| Label::parse_canonical(raw).ok())
        .and_then(|label| {
            let core_cfg = BuildConfigurationKey::target(&cfg.cpu, "fastbuild").to_core_only();
            inputs.resolver.configured_target(&label, &core_cfg).ok()
        })
        .and_then(|ct| platform_info_os_is_macos(&ct))
        .unwrap_or(host_is_macos);
    let mut m = BTreeMap::new();
    m.insert(
        "LC_CTYPE".to_string(),
        if mac { "en_US.UTF-8" } else { "C.UTF-8" }.to_string(),
    );
    m
}

fn utf8_env(st: &Arc<CtxState>) -> BTreeMap<String, String> {
    utf8_env_for_inputs(&st.cfg, &st.inputs, cfg!(target_os = "macos"))
}

fn normalize_strict_deps_mode(raw: Option<&Value>) -> Result<String, StarlarkError> {
    let Some(v) = raw else {
        return Ok("ERROR".to_string());
    };
    let Some(s) = v.as_str() else {
        return Err(StarlarkError("strict_deps_mode must be a string".into()));
    };
    match s.to_ascii_uppercase().as_str() {
        "DEFAULT" | "STRICT" | "ERROR" => Ok("ERROR".to_string()),
        "WARN" => Ok("WARN".to_string()),
        "OFF" => Ok("OFF".to_string()),
        _ => Err(StarlarkError(format!("StrictDepsMode {s} not allowed"))),
    }
}

fn is_legal_exec_req_tag(tag: &str) -> bool {
    tag == "local"
        || tag.starts_with("no-")
        || tag.starts_with("requires-")
        || tag.starts_with("block-")
        || tag.starts_with("supports-")
        || tag.starts_with("disable-")
        || tag.starts_with("cpu:")
}

fn apply_java_rule_tags_and_exec_group(
    st: &CtxState,
    a: &mut Action,
    reject_supports_path_mapping: bool,
) -> Result<(), StarlarkError> {
    if st.inputs.allow_tags_propagation {
        for tag in st.rule.rule_tags() {
            if !is_legal_exec_req_tag(&tag) {
                continue;
            }
            if reject_supports_path_mapping && tag == "supports-path-mapping" {
                return err("Tag 'supports-path-mapping' is not permitted on Java resource jar action");
            }
            if let Some((k, v)) = tag.split_once(':') {
                if k == "cpu" {
                    a.execution_requirements
                        .insert("cpu".to_string(), v.to_string());
                    continue;
                }
            }
            a.execution_requirements.insert(tag, String::new());
        }
    }
    let java_group = st
        .exec_groups
        .iter()
        .find(|g| *g == "java_toolchain" || g.contains("java_toolchain"))
        .cloned();
    if let Some(grp) = java_group {
        if let Some(plat) = st.inputs.exec_group_platforms.get(&grp) {
            a.exec_platform = Some(plat.clone());
        }
        a.exec_group = Some(grp);
    }
    Ok(())
}


/// `JavaCompilationHelper.getExecutionInfo` / `JavaHeaderCompileAction`: path mapping, the
/// toolchain's worker capabilities (compile actions only) and, with
/// `--experimental_inmemory_jdeps_files` (default true), the inlined `.jdeps` output.
fn java_exec_info(st: &Arc<CtxState>, toolchain: &Value, workers: bool, jdeps: Option<&Artifact>) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("supports-path-mapping".to_string(), "1".to_string());
    if workers {
        for (field, key) in [
            ("_javac_supports_workers", "supports-workers"),
            ("_javac_supports_multiplex_workers", "supports-multiplex-workers"),
            ("_javac_supports_worker_cancellation", "supports-worker-cancellation"),
            ("_javac_supports_worker_multiplex_sandboxing", "supports-multiplex-sandboxing"),
        ] {
            if jf(toolchain, field).is_some_and(|v| v.is_truthy()) {
                m.insert(key.to_string(), "1".to_string());
            }
        }
    }
    let inmemory = st.cfg.options.get("experimental_inmemory_jdeps_files").is_none_or(|v| v == "true" || v == "1");
    if let (true, Some(d)) = (inmemory, jdeps) {
        m.insert("internal-inline-outputs".to_string(), d.exec_path.path_string().to_string());
    }
    m
}

fn java_progress(prefix: &str, out: &Artifact, nsrc: usize, njars: usize, processors: &[String]) -> String {
    let mut s = format!("{prefix} {} (", out.root_relative_path.path_string());
    let mut first = true;
    for (n, name) in [(nsrc, "source file"), (njars, "source jar")] {
        if n > 0 {
            if !first {
                s.push_str(", ");
            }
            first = false;
            s.push_str(&format!("{n} {name}{}", if n > 1 { "s" } else { "" }));
        }
    }
    s.push(')');
    if !processors.is_empty() {
        let short: Vec<&str> = processors.iter().map(|p| p.rsplit('.').next().unwrap_or(p)).collect();
        s.push_str(&format!(" and running annotation processors ({})", short.join(", ")));
    }
    s
}

/// `JavaPluginData` of a `JavaPluginInfo` field (`plugins` / `api_generating_plugins`).
fn plugin_data(plugin_info: Option<&Value>, which: &str) -> (Vec<String>, Vec<Artifact>, Vec<Artifact>) {
    let Some(pi) = plugin_info else { return (vec![], vec![], vec![]) };
    let Some(d) = jf(pi, which) else { return (vec![], vec![], vec![]) };
    (
        jstrs(jf(&d, "processor_classes").as_ref()),
        cc_artifacts(jf(&d, "processor_jars").as_ref()),
        cc_artifacts(jf(&d, "processor_data").as_ref()),
    )
}

fn boot_class_path(v: Option<&Value>, toolchain: &Value) -> (Vec<Artifact>, Vec<Artifact>, Vec<Artifact>, Option<String>) {
    // JavaCompilationHelper.getBootclasspathOrDefault: the toolchain's when the target gives none.
    let given = v.filter(|v| matches!(v, Value::ProviderInstance { .. })).cloned().filter(|b| {
        !cc_artifacts(jf(b, "bootclasspath").as_ref()).is_empty()
            || !cc_artifacts(jf(b, "_auxiliary").as_ref()).is_empty()
            || !cc_artifacts(jf(b, "_system_inputs").as_ref()).is_empty()
            || jf(b, "_system_path").is_some()
    });
    let fallback = jf(toolchain, "_bootclasspath_info");
    let Some(b) = given.as_ref().or(fallback.as_ref()) else {
        return (vec![], vec![], vec![], None);
    };
    let boot = cc_artifacts(jf(b, "bootclasspath").as_ref());
    let aux = cc_artifacts(jf(b, "_auxiliary").as_ref());
    let sys_inputs = cc_artifacts(jf(b, "_system_inputs").as_ref());
    let sys_path = match jf(b, "_system_path") {
        Some(Value::String(s)) => Some(s),
        _ => None,
    };
    (boot, aux, sys_inputs, sys_path)
}

/// `JavaTargetAttributes.build()`: places `direct_jars` ahead of `compile_time_classpath` in
/// naive link (preorder) order, deduplicating by exec path.
fn prepend_direct_jars_to_classpath(
    aux: &[Artifact],
    direct_jars: &[Artifact],
    classpath: &[Artifact],
) -> Vec<Artifact> {
    dedup(
        aux.iter()
            .chain(direct_jars.iter())
            .chain(classpath.iter())
            .cloned()
            .collect(),
    )
}

/// `create_header_compilation_action` (`JavaCompilationHelper.createHeaderCompilationAction` +
/// `JavaHeaderCompileAction.Builder.build`).
fn java_create_header_action(st: &Arc<CtxState>, pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    let g = |i: usize, k: &str| kw.get(k).or_else(|| pos.get(i)).filter(|v| !matches!(v, Value::None));
    let toolchain = g(1, "java_toolchain").ok_or_else(|| StarlarkError("missing java_toolchain".into()))?;
    let out_jar = g(2, "header_jar").and_then(artifact_of).ok_or_else(|| StarlarkError("header_jar must be a File".into()))?;
    let deps_proto = g(3, "header_deps_proto").and_then(artifact_of).ok_or_else(|| StarlarkError("header_deps_proto must be a File".into()))?;
    let (proc_classes, proc_jars, proc_data) = plugin_data(g(4, "plugin_info"), "api_generating_plugins");
    let sources = cc_artifacts(g(5, "source_files"));
    let source_jars = cc_artifacts(g(6, "source_jars"));
    let classpath = cc_artifacts(g(7, "compile_time_classpath"));
    let raw_direct_jars = cc_artifacts(g(8, "direct_jars"));
    let (boot, aux, _sys, _sysp) = boot_class_path(g(9, "boot_class_path"), toolchain);
    let dep_artifacts = cc_artifacts(g(10, "compile_time_java_deps"));
    let javacopts = crate::javac_options::JavacOptions::normalize_release_only(&java_tokenize_opts(
        g(11, "javac_opts"),
    ));
    let strict = normalize_strict_deps_mode(g(12, "strict_deps_mode"))?;
    let target_label = match g(13, "target_label") {
        Some(Value::Label(l)) => Some(l.clone()),
        _ => None,
    };
    let injecting = g(14, "injecting_rule_kind").and_then(|v| v.as_str().map(str::to_string));
    let enable_direct = g(15, "enable_direct_classpath").map(|v| v.is_truthy()).unwrap_or(true);
    let additional_inputs = cc_artifacts(g(16, "additional_inputs"));
    let header_compilation_jar = g(17, "header_compilation_jar").and_then(artifact_of);
    let hdr_direct = cc_artifacts(g(18, "header_compilation_direct_deps"));
    let gensrc_output_jar = g(19, "gensrc_output_jar")
        .or_else(|| g(19, "output_gensrc_jar"))
        .and_then(artifact_of);
    let resource_output_jar = g(20, "resource_output_jar").and_then(artifact_of);
    let manifest_proto = g(21, "manifest_proto").and_then(artifact_of);
    let additional_outputs = cc_artifacts(g(22, "additional_outputs"));
    let has_ap_outputs = gensrc_output_jar.is_some()
        || resource_output_jar.is_some()
        || manifest_proto.is_some()
        || !additional_outputs.is_empty();

    let builtin = jstrs(jf(toolchain, "_header_compiler_builtin_processors").as_ref());
    let direct_tool = jf(toolchain, "_header_compiler_direct");
    let use_direct_cp = enable_direct && proc_classes.is_empty() && !has_ap_outputs;
    let use_hcd = !has_ap_outputs
        && direct_tool.is_some()
        && proc_classes.iter().all(|p| builtin.contains(p));
    let tool = if use_hcd { direct_tool } else { jf(toolchain, "_header_compiler") }
        .ok_or_else(|| StarlarkError("java_toolchain has no header compiler".into()))?;
    let (exe_line, tool_inputs) = java_tool_command(&tool, toolchain)?;
    let tools_jars = cc_artifacts(jf(toolchain, "tools").as_ref());

    let full_cp = prepend_direct_jars_to_classpath(&aux, &raw_direct_jars, &classpath);
    let (direct_jars, dep_artifacts) = if strict == "OFF" {
        (vec![], vec![])
    } else {
        (
            if aux.is_empty() {
                raw_direct_jars
            } else {
                aux.iter().cloned().chain(raw_direct_jars).collect()
            },
            dep_artifacts,
        )
    };

    let mut c = JCmd::default();
    c.exec_path("--output", Some(&out_jar));
    c.exec_path("--header_compilation_output", header_compilation_jar.as_ref());
    c.exec_path("--gensrc_output", gensrc_output_jar.as_ref());
    c.exec_path("--resource_output", resource_output_jar.as_ref());
    c.exec_path("--output_manifest_proto", manifest_proto.as_ref());
    c.exec_path("--output_deps", Some(&deps_proto));
    c.exec_paths("--bootclasspath", &boot);
    c.exec_paths("--sources", &sources);
    c.exec_paths("--source_jars", &source_jars);
    c.opt("--injecting_rule_kind", injecting.as_deref());
    // `JavaCompilationHelper.createHeaderCompilationAction`: dagger needs `-Aexperimental_turbine_hjar`.
    let add_hjar_opt = proc_classes.iter().any(|p| p == "dagger.internal.codegen.ComponentProcessor");
    if !javacopts.is_empty() || add_hjar_opt {
        c.0.push("--javacopts".into());
        c.0.extend(javacopts.iter().cloned());
        if add_hjar_opt {
            c.0.push("-Aexperimental_turbine_hjar".into());
        }
        // terminates --javacopts to support javac flags that start with `--`
        c.0.push("--".into());
    }
    if let Some(l) = &target_label {
        c.0.push("--target_label".into());
        c.0.push(java_label_arg(l));
    }

    let mut inputs: Vec<Artifact> = Vec::new();
    inputs.extend(additional_inputs);
    inputs.extend(boot);
    inputs.extend(source_jars.iter().cloned());
    inputs.extend(sources.iter().cloned());
    inputs.extend(tools_jars.iter().cloned());
    inputs.extend(tool_inputs);

    let mut outputs = vec![out_jar.clone(), deps_proto.clone()];
    for extra_out in [
        header_compilation_jar,
        gensrc_output_jar,
        resource_output_jar,
        manifest_proto,
    ]
    .into_iter()
    .flatten()
    {
        outputs.push(extra_out);
    }
    outputs.extend(additional_outputs);

    if !use_direct_cp {
        // Annotation-processing header compilation runs through `JavaCompileAction` (`JavacTurbine`).
        if !use_hcd {
            inputs.extend(proc_jars.iter().cloned());
            inputs.extend(proc_data.iter().cloned());
        }
        let builtin_used: Vec<String> = builtin
            .iter()
            .filter(|p| proc_classes.contains(p))
            .cloned()
            .collect();
        c.all("--builtin_processors", &builtin_used);
        c.all("--processors", &proc_classes);
        if !use_hcd {
            c.exec_paths("--processorpath", &proc_jars);
        }
        if strict != "OFF" {
            c.exec_paths("--direct_dependencies", &direct_jars);
        }
        let incompatible = jstrs(jf(toolchain, "_reduced_classpath_incompatible_processors").as_ref());
        let mut mode = java_classpath_mode(st);
        if proc_classes.iter().any(|p| incompatible.contains(p)) || strict == "OFF" {
            mode = "OFF".to_string();
        }
        return java_register_compile(
            st,
            "JavacTurbine",
            &exe_line,
            c.0,
            full_cp,
            direct_jars,
            dep_artifacts,
            vec![],
            inputs,
            tools_jars,
            outputs,
            Some(deps_proto.clone()),
            &mode,
            utf8_env(st),
            java_exec_info(st, toolchain, false, Some(&deps_proto)),
            java_progress("Compiling Java headers", &out_jar, sources.len(), source_jars.len(), &proc_classes),
        );
    }
    let cp = if !hdr_direct.is_empty() || full_cp.is_empty() { hdr_direct } else { full_cp };
    inputs.extend(cp.iter().cloned());
    c.exec_paths("--classpath", &cp);
    c.opt("--reduce_classpath_mode", Some("NONE"));
    java_register(
        st,
        "Turbine",
        &exe_line,
        vec![c.0],
        inputs,
        vec![],
        outputs,
        java_progress("Compiling Java headers", &out_jar, sources.len(), source_jars.len(), &proc_classes),
        java_exec_info(st, toolchain, false, Some(&deps_proto)),
    )
}

#[allow(clippy::too_many_arguments)]
fn java_register(
    st: &Arc<CtxState>,
    mnemonic: &str,
    exe_line: &[String],
    param_lines: Vec<Vec<String>>,
    inputs: Vec<Artifact>,
    tools: Vec<Artifact>,
    outputs: Vec<Artifact>,
    progress: String,
    info: BTreeMap<String, String>,
) -> HostResult {
    java_register_ext(st, mnemonic, exe_line, param_lines, vec![], inputs, tools, outputs, progress, info)
}

#[allow(clippy::too_many_arguments)]
fn java_register_ext(
    st: &Arc<CtxState>,
    mnemonic: &str,
    exe_line: &[String],
    param_lines: Vec<Vec<String>>,
    extra_lines: Vec<CommandLineArgs>,
    inputs: Vec<Artifact>,
    tools: Vec<Artifact>,
    outputs: Vec<Artifact>,
    progress: String,
    info: BTreeMap<String, String>,
) -> HostResult {
    let (exe, rest) = exe_line.split_first().ok_or_else(|| StarlarkError("empty java tool command line".into()))?;
    let mut arguments: Vec<CommandLineArgs> = Vec::new();
    if !rest.is_empty() {
        arguments.push(CommandLineArgs { args: rest.to_vec(), param_file: None });
    }
    for l in param_lines {
        arguments.push(CommandLineArgs { args: l, param_file: Some(param_info()) });
    }
    for mut e in extra_lines {
        if e.param_file.is_none() {
            e.param_file = Some(param_info());
        }
        arguments.push(e);
    }
    let mut a = Action::new(
        st.label.clone(),
        mnemonic,
        ActionKind::Spawn { executable: exe.clone(), arguments, shell_command: None },
    );
    a.progress_message = Some(progress);
    a.env = utf8_env(st).into_iter().collect();
    a.execution_requirements = info;
    let mut all_inputs = inputs;
    all_inputs.extend(tools.iter().cloned());
    a.inputs = dedup(all_inputs);
    a.tools = dedup(tools);
    a.outputs = outputs;
    a.use_default_shell_env = true;
    apply_java_rule_tags_and_exec_group(st, &mut a, false)?;
    st.register(a)?;
    Ok(Value::None)
}

/// `JavaConfiguration.getReduceJavaClasspath` (`--experimental_java_classpath`, default `bazel`):
/// `OFF`, `JAVABUILDER`, `BAZEL` or `BAZEL_NO_FALLBACK`.
fn java_classpath_mode(st: &Arc<CtxState>) -> String {
    let raw = st
        .cfg
        .options
        .get("experimental_java_classpath")
        .cloned()
        .or_else(|| match st.cfg.build_options().get("experimental_java_classpath") {
            Some(crate::build_options::ConfigValue::Scalar(s)) => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "bazel".to_string())
        .to_ascii_uppercase();
    if matches!(raw.as_str(), "OFF" | "JAVABUILDER" | "BAZEL" | "BAZEL_NO_FALLBACK") {
        raw
    } else {
        "BAZEL".to_string()
    }
}

/// Registers a `JavaCompileAction` (`JavaCompileAction.java`): the command lines are the
/// executable, the flag line, the classpath line (`getFullClasspathLine`, only when there is a
/// classpath or a classpath mode) and the extra lines; with `BAZEL` / `BAZEL_NO_FALLBACK` the
/// executor re-writes the classpath line per attempt (reduced classpath, then the full-classpath
/// fallback) from the attached [`JavaCompileSpec`].
#[allow(clippy::too_many_arguments)]
fn java_register_compile(
    st: &Arc<CtxState>,
    mnemonic: &str,
    exe_line: &[String],
    flag_line: Vec<String>,
    classpath: Vec<Artifact>,
    direct_jars: Vec<Artifact>,
    dep_artifacts: Vec<Artifact>,
    extra_lines: Vec<CommandLineArgs>,
    mandatory: Vec<Artifact>,
    tools: Vec<Artifact>,
    outputs: Vec<Artifact>,
    deps_proto: Option<Artifact>,
    mode: &str,
    env: BTreeMap<String, String>,
    exec_requirements: BTreeMap<String, String>,
    progress: String,
) -> HostResult {
    let (exe, rest) = exe_line.split_first().ok_or_else(|| StarlarkError("empty java tool command line".into()))?;
    // `JavaCompileActionBuilder.build` / `createJvmCompileAction` invariants.
    let mode = if deps_proto.is_none() { "OFF" } else { mode };
    let dep_artifacts = if mode == "OFF" { Vec::new() } else { dep_artifacts };
    let mut arguments: Vec<CommandLineArgs> = Vec::new();
    if !rest.is_empty() {
        arguments.push(CommandLineArgs { args: rest.to_vec(), param_file: None });
    }
    arguments.push(CommandLineArgs { args: flag_line, param_file: Some(param_info()) });
    let classpath_arg_index = arguments.len();
    if !classpath.is_empty() || mode != "OFF" {
        let mut cpl = JCmd::default();
        cpl.exec_paths("--classpath", &classpath);
        if mode == "JAVABUILDER" {
            cpl.0.push("--reduce_classpath_mode".into());
            cpl.0.push("JAVABUILDER_REDUCED".into());
            cpl.exec_paths("--deps_artifacts", &dep_artifacts);
        }
        arguments.push(CommandLineArgs { args: cpl.0, param_file: Some(param_info()) });
    }
    for mut e in extra_lines {
        if e.param_file.is_none() {
            e.param_file = Some(param_info());
        }
        arguments.push(e);
    }
    let mut a = Action::new(
        st.label.clone(),
        mnemonic,
        ActionKind::Spawn { executable: exe.clone(), arguments, shell_command: None },
    );
    a.progress_message = Some(progress);
    a.env = env;
    a.execution_requirements = exec_requirements;
    // `allInputs`: mandatory inputs, tools, the (transitive) classpath and the dependency .jdeps.
    let mut all_inputs = mandatory.clone();
    all_inputs.extend(tools.iter().cloned());
    all_inputs.extend(classpath.iter().cloned());
    all_inputs.extend(dep_artifacts.iter().cloned());
    a.inputs = dedup(all_inputs);
    if matches!(mode, "BAZEL" | "BAZEL_NO_FALLBACK") {
        if let Some(dp) = &deps_proto {
            let paths = |v: &[Artifact]| -> Vec<String> { v.iter().map(jpath).collect() };
            let mut mand = mandatory;
            mand.extend(tools.iter().cloned());
            a.java_compile = Some(crate::JavaCompileSpec {
                classpath_mode: mode.to_string(),
                classpath_arg_index,
                classpath_params_path: String::new(),
                transitive_inputs: paths(&classpath),
                direct_jars: paths(&direct_jars),
                dependency_artifacts: paths(&dep_artifacts),
                mandatory_inputs: paths(&mand),
                deps_proto: jpath(dp),
            });
        }
    }
    a.tools = dedup(tools);
    a.outputs = outputs;
    a.use_default_shell_env = true;
    apply_java_rule_tags_and_exec_group(st, &mut a, false)?;
    st.register(a)?;
    Ok(Value::None)
}

/// `JavaStarlarkCommon.createJvmCompileAction` (`java_common_internal_do_not_use`): a generic
/// `JavaCompileAction` whose command lines are supplied by the caller.
fn java_create_jvm_compile_action(st: &Arc<CtxState>, pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    let g = |i: usize, k: &str| kw.get(k).or_else(|| pos.get(i)).filter(|v| !matches!(v, Value::None));
    let outputs = cc_artifacts(g(1, "outputs"));
    let mut mandatory = cc_artifacts(g(2, "mandatory_inputs"));
    let mut transitive = cc_artifacts(g(3, "transitive_inputs"));
    let mut direct_jars = cc_artifacts(g(4, "direct_jars"));
    let mut tools: Vec<Artifact> = Vec::new();
    let mut exe_line: Vec<String> = Vec::new();
    match g(5, "executable") {
        Some(Value::String(s)) => exe_line.push(s.clone()),
        Some(v) if artifact_of(v).is_some() => {
            let a = artifact_of(v).unwrap();
            exe_line.push(jpath(&a));
            tools.push(a);
        }
        Some(Value::ProviderInstance { provider_name, fields }) if provider_name == "FilesToRunProvider" => {
            let e = fields
                .get("executable")
                .and_then(artifact_of)
                .ok_or_else(|| StarlarkError("executable must be a single file".into()))?;
            exe_line.push(jpath(&e));
            add_files_to_run_inputs(fields, &mut tools);
        }
        Some(other) => {
            return err(format!("expected file, FilesToRunProvider, or string for executable, got: {}", type_name(other)))
        }
        None => return err("create_jvm_compile_action: missing executable"),
    }
    match g(6, "tools") {
        Some(Value::Depset(_)) => tools.extend(cc_artifacts(g(6, "tools"))),
        Some(v) => {
            for t in items(v) {
                match &t {
                    Value::ProviderInstance { provider_name, fields } if provider_name == "FilesToRunProvider" => {
                        add_files_to_run_inputs(fields, &mut tools)
                    }
                    Value::Depset(_) => tools.extend(cc_artifacts(Some(&t))),
                    other => match artifact_of(other) {
                        Some(a) => tools.push(a),
                        None => {
                            return err(format!(
                                "expected File, FilesToRunProvider, or depset in tools, got: {}",
                                type_name(other)
                            ))
                        }
                    },
                }
            }
        }
        None => {}
    }
    exe_line.extend(jstrs(g(18, "executable_args")));
    let mut arg_lines: Vec<CommandLineArgs> = Vec::new();
    if let Some(v) = g(7, "arguments") {
        for a in items(v) {
            if let Some(args) = args_of(&a) {
                arg_lines.push(args.lock().unwrap().finish());
            }
        }
    }
    let mnemonic = g(8, "mnemonic").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "Javac".into());
    let progress = g(9, "progress_message").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    let env = match g(10, "env") {
        Some(v) => str_dict(v, "env")?,
        None => BTreeMap::new(),
    };
    let exec_req = match g(11, "execution_requirements") {
        Some(v) => str_dict(v, "execution_requirements")?,
        None => BTreeMap::new(),
    };
    let deps_proto = g(13, "output_deps_proto").and_then(artifact_of);
    let mut dep_artifacts = cc_artifacts(g(14, "dependency_artifacts"));
    let enable_direct = g(16, "enable_direct_classpath").map(|v| v.is_truthy()).unwrap_or(false);
    let reduce_incompatible = g(17, "reduce_classpath_incompatible").map(|v| v.is_truthy()).unwrap_or(false);
    let config_mode = java_classpath_mode(st);
    let mode: String;
    if enable_direct {
        // Direct classpath (turbine-style): the classpath goes on the flag line and the reduced
        // classpath machinery is off.
        let classpath = if !direct_jars.is_empty() || transitive.is_empty() { direct_jars.clone() } else { transitive.clone() };
        mandatory.extend(classpath.iter().cloned());
        if let Some(first) = arg_lines.first_mut() {
            if !classpath.is_empty() {
                first.args.push("--classpath".into());
                first.args.extend(classpath.iter().map(jpath));
            }
            first.args.push("--reduce_classpath_mode".into());
            first.args.push("NONE".into());
        }
        transitive = Vec::new();
        direct_jars = Vec::new();
        mode = "OFF".into();
    } else {
        mode = if deps_proto.is_some() && !reduce_incompatible { config_mode } else { "OFF".into() };
    }
    if mode == "OFF" {
        dep_artifacts = Vec::new();
    }
    let flag_line = if arg_lines.is_empty() { CommandLineArgs { args: vec![], param_file: None } } else { arg_lines.remove(0) };
    java_register_compile(
        st,
        &mnemonic,
        &exe_line,
        flag_line.args,
        transitive,
        direct_jars,
        dep_artifacts,
        arg_lines,
        mandatory,
        tools,
        outputs,
        deps_proto,
        &mode,
        env,
        exec_req,
        progress,
    )
}

/// `create_compilation_action` (`JavaCompilationHelper.createCompileAction` +
/// `JavaCompileActionBuilder.build`), with the reduced-classpath modes of
/// `--experimental_java_classpath`.
fn java_create_compile_action(st: &Arc<CtxState>, pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    let g = |i: usize, k: &str| kw.get(k).or_else(|| pos.get(i)).filter(|v| !matches!(v, Value::None));
    let toolchain = g(1, "java_toolchain").ok_or_else(|| StarlarkError("missing java_toolchain".into()))?;
    let output = g(2, "output").and_then(artifact_of).ok_or_else(|| StarlarkError("output must be a File".into()))?;
    let manifest = g(3, "manifest_proto").and_then(artifact_of);
    let (mut proc_classes, mut proc_jars, mut proc_data) = plugin_data(g(4, "plugin_info"), "plugins");
    let mut classpath = cc_artifacts(g(5, "compile_time_classpath"));
    let mut raw_direct_jars = cc_artifacts(g(6, "direct_jars"));
    let (boot, aux, sys_inputs, sys_path) = boot_class_path(g(7, "boot_class_path"), toolchain);
    let builder_jvm_flags = jstrs(g(8, "javabuilder_jvm_flags"));
    let dep_artifacts = cc_artifacts(g(9, "compile_time_java_deps"));
    let mut raw_javacopts = java_tokenize_opts(g(10, "javac_opts"));
    let strict = normalize_strict_deps_mode(g(11, "strict_deps_mode"))?;
    let target_label = match g(12, "target_label") {
        Some(Value::Label(l)) => Some(l.clone()),
        _ => Some(st.label.clone()),
    };
    let deps_proto = g(13, "deps_proto").and_then(artifact_of);
    let gen_class = g(14, "gen_class").and_then(artifact_of);
    let gen_source = g(15, "gen_source").and_then(artifact_of);
    let native_header = g(16, "native_header").and_then(artifact_of);
    let sources = cc_artifacts(g(17, "sources"));
    let source_jars = cc_artifacts(g(18, "source_jars"));
    let resources = cc_artifacts(g(19, "resources"));
    let resource_jars = cc_artifacts(g(20, "resource_jars"));
    let classpath_resources = cc_artifacts(g(21, "classpath_resources"));
    let sourcepath = cc_artifacts(g(22, "sourcepath"));
    let injecting = g(23, "injecting_rule_kind").and_then(|v| v.as_str().map(str::to_string));
    let enable_jspecify = g(24, "enable_jspecify").map(Value::is_truthy).unwrap_or(true);
    let additional_inputs = cc_artifacts(g(26, "additional_inputs"));
    let additional_outputs = cc_artifacts(g(27, "additional_outputs"));
    let extra_args: Vec<CommandLineArgs> = match g(28, "extra_args") {
        Some(v) => {
            let mut out = Vec::new();
            for a in items(v) {
                if let Some(args) = args_of(&a) {
                    out.push(args.lock().unwrap().finish());
                }
            }
            out
        }
        None => vec![],
    };

    let jopts = crate::fragments::Opts::new(&st.cfg);
    if enable_jspecify && jopts.b("experimental_enable_jspecify", true) && !sources.is_empty() {
        if let Some(jsp) = jf(toolchain, "_jspecify_info") {
            let (jp_cls, jp_jars, jp_data) = plugin_data(jf(&jsp, "processor").as_ref(), "plugins");
            for cls in jp_cls {
                if !proc_classes.contains(&cls) {
                    proc_classes.push(cls);
                }
            }
            proc_jars.extend(jp_jars);
            proc_data.extend(jp_data);
            let jsp_jars = cc_artifacts(jf(&jsp, "implicit_deps").as_ref());
            raw_direct_jars.extend(jsp_jars.iter().cloned());
            classpath.extend(jsp_jars);
            raw_javacopts.extend(java_tokenize_opts(jf(&jsp, "javacopts").as_ref()));
        }
    }

    // `JavaCompilationHelper.createCompileAction`: the GenClass action reads the compile output.
    if let Some(gc) = &gen_class {
        java_gen_jar_action(st, toolchain, &output, manifest.as_ref(), gc)?;
    }
    // A separate resource jar: javac writes `<name>-class.jar`; singlejar merges it with the
    // resources into the declared output (`ResourceJarActionBuilder`).
    let mut output = output;
    if !resources.is_empty() || !resource_jars.is_empty() || !classpath_resources.is_empty() {
        let original = output.clone();
        let rel = original.root_relative_path.path_string().to_string();
        let new_rel = match rel.rsplit_once('.') {
            Some((stem, ext)) => format!("{stem}-class.{ext}"),
            None => format!("{rel}-class"),
        };
        output = st.derived(PathFragment::create(&new_rel));
        let mut rjars = resource_jars.clone();
        rjars.push(output.clone());
        let mut res_pairs: Vec<(String, Artifact)> = Vec::new();
        for r in &resources {
            res_pairs.push((java_resource_path(st, r)?, r.clone()));
        }
        java_resource_jar_action(st, toolchain, &original, &res_pairs, &classpath_resources, &rjars, &additional_inputs)?;
    }
    let builder = jf(toolchain, "_javabuilder").ok_or_else(|| StarlarkError("java_toolchain has no javabuilder".into()))?;
    let (mut exe_line, tool_inputs) = java_tool_command(&builder, toolchain)?;
    let is_vanilla = exe_line
        .iter()
        .any(|part| part.contains("VanillaJavaBuilder"));
    let javacopts = if is_vanilla {
        crate::javac_options::JavacOptions::normalize_vanilla(&raw_javacopts)
    } else {
        crate::javac_options::JavacOptions::normalize_default(&raw_javacopts)
    };
    // `javaToolchain.getJavaBuilder().withAdditionalJvmFlags(javaBuilderJvmFlags)`: flags precede `-jar`.
    if !builder_jvm_flags.is_empty() {
        let pos_jar = exe_line.iter().position(|s| s == "-jar").unwrap_or(exe_line.len());
        for (i, f) in builder_jvm_flags.iter().enumerate() {
            exe_line.insert(pos_jar + i, f.clone());
        }
    }
    let tools_jars = cc_artifacts(jf(toolchain, "tools").as_ref());
    let incompatible = jstrs(jf(toolchain, "_reduced_classpath_incompatible_processors").as_ref());
    let full_cp = prepend_direct_jars_to_classpath(&aux, &raw_direct_jars, &classpath);
    let strict_off = strict == "OFF";
    let (direct_jars, dep_artifacts) = if strict_off { (vec![], vec![]) } else { (raw_direct_jars, dep_artifacts) };
    let mut mode = java_classpath_mode(st);
    if proc_classes.iter().any(|p| incompatible.contains(p)) || strict_off || deps_proto.is_none() {
        mode = "OFF".to_string();
    }
    let direct_all: Vec<Artifact> = if aux.is_empty() { direct_jars } else { aux.iter().cloned().chain(direct_jars).collect() };

    let mut coverage_inputs: Vec<Artifact> = Vec::new();
    let mut post_processor_args: Vec<String> = Vec::new();
    if jopts.b("collect_code_coverage", false) && !sources.is_empty() {
        let jacoco_art = jf(toolchain, "_jacocorunner")
            .and_then(|jr| {
                artifact_of(&jr).or_else(|| {
                    jf(&jr, "executable")
                        .or_else(|| jf(&jr, "tool").and_then(|t| jf(&t, "executable")))
                        .and_then(|v| artifact_of(&v))
                })
            });
        if let Some(jrunner) = jacoco_art {
            let out_rel = output.root_relative_path.path_string().to_string();
            let cov_rel = match out_rel.rsplit_once('.') {
                Some((stem, _)) => format!("{stem}-paths-for-coverage.txt"),
                None => format!("{out_rel}-paths-for-coverage.txt"),
            };
            let cov_file = st.derived(PathFragment::create(&cov_rel));
            let mut cov_body = sources.iter().map(jpath).collect::<Vec<_>>().join("\n");
            if !cov_body.is_empty() {
                cov_body.push('\n');
            }
            let mut fw = Action::new(
                st.label.clone(),
                "FileWrite",
                ActionKind::FileWrite {
                    content: crate::action::FileContent::Literal(cov_body),
                    is_executable: false,
                },
            );
            fw.outputs = vec![cov_file.clone()];
            st.register(fw)?;
            post_processor_args.push("--post_processor".into());
            post_processor_args.push(jpath(&jrunner));
            post_processor_args.push(jpath(&cov_file));
            coverage_inputs.push(jrunner);
            coverage_inputs.push(cov_file);
        }
    }

    let fix_deps_tool = st
        .cfg
        .options
        .get("experimental_fix_deps_tool")
        .cloned()
        .or_else(|| match st.cfg.build_options().get("experimental_fix_deps_tool") {
            Some(crate::build_options::ConfigValue::Scalar(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "add_dep".to_string());

    let mut c = JCmd::default();
    c.exec_path("--output", Some(&output));
    c.exec_path("--native_header_output", native_header.as_ref());
    c.exec_path("--generated_sources_output", gen_source.as_ref());
    c.exec_path("--output_manifest_proto", manifest.as_ref());
    c.0.push("--compress_jar".into());
    c.exec_path("--output_deps_proto", deps_proto.as_ref());
    c.exec_paths("--bootclasspath", &boot);
    if let Some(sp) = &sys_path {
        c.0.push("--system".into());
        c.0.push(sp.clone());
    }
    c.exec_paths("--sourcepath", &sourcepath);
    c.exec_paths("--processorpath", &proc_jars);
    c.all("--processors", &proc_classes);
    c.exec_paths("--source_jars", &source_jars);
    c.exec_paths("--sources", &sources);
    if !javacopts.is_empty() {
        c.0.push("--javacopts".into());
        c.0.extend(javacopts.iter().cloned());
        c.0.push("--".into());
    }
    if let Some(l) = &target_label {
        c.0.push("--target_label".into());
        c.0.push(java_label_arg(l));
    }
    c.opt("--injecting_rule_kind", injecting.as_deref());
    if !strict_off {
        c.opt("--strict_java_deps", Some(&strict));
        c.exec_paths("--direct_dependencies", &direct_all);
    }
    c.opt("--experimental_fix_deps_tool", Some(&fix_deps_tool));
    c.0.extend(post_processor_args);

    // `JavaCompileActionBuilder`'s mandatory inputs.
    let mut inputs: Vec<Artifact> = Vec::new();
    inputs.extend(proc_jars);
    inputs.extend(proc_data);
    inputs.extend(source_jars.iter().cloned());
    inputs.extend(sources.iter().cloned());
    if let Some(rt) = jf(toolchain, "java_runtime") {
        inputs.extend(cc_artifacts(jf(&rt, "files").as_ref()));
    }
    inputs.extend(boot);
    inputs.extend(sourcepath);
    inputs.extend(additional_inputs);
    inputs.extend(sys_inputs);
    inputs.extend(tool_inputs);
    inputs.extend(coverage_inputs);

    let mut outs = vec![output.clone()];
    outs.extend(additional_outputs);
    for o in [deps_proto.clone(), native_header, gen_source, manifest].into_iter().flatten() {
        outs.push(o);
    }
    let javac_exec_info = java_exec_info(st, toolchain, true, deps_proto.as_ref());
    java_register_compile(
        st,
        "Javac",
        &exe_line,
        c.0,
        full_cp,
        direct_all,
        dep_artifacts,
        extra_args,
        inputs,
        tools_jars,
        outs,
        deps_proto,
        &mode,
        utf8_env(st),
        javac_exec_info,
        java_progress("Building", &output, sources.len(), source_jars.len(), &proc_classes),
    )
}

// ---- resources: JavaHelper.getJavaResourcePath / BazelJavaSemantics.getDefaultJavaResourcePath

const JAVA_KNOWN_SOURCE_ROOTS: &[&str] = &["java", "javatests", "src", "testsrc"];

/// `JavaUtil.javaSegmentIndexImpl`.
fn java_segment_index_impl(segs: &[&str]) -> i64 {
    let Some(mut root) = segs.iter().position(|s| JAVA_KNOWN_SOURCE_ROOTS.contains(s)) else { return -1 };
    let is_src = segs[root] == "src";
    let mut check_maven: i64 = if is_src { root as i64 } else { -1 };
    if root == 0 || is_src {
        let max = segs.len() as i64 - 2;
        let mut i = root as i64 + 1;
        while i <= max {
            let seg = segs[i as usize];
            if seg == "src" || (is_src && (seg == "javatests" || seg == "java")) {
                let next = segs[i as usize + 1];
                if next == "com" || next == "org" || next == "net" {
                    root = i as usize;
                } else if seg == "src" {
                    check_maven = i;
                }
                break;
            }
            i += 1;
        }
    }
    let mut root = root as i64;
    if check_maven >= 0 && (check_maven as usize) + 2 < segs.len() {
        let next = segs[check_maven as usize + 1];
        if next == "main" || next == "test" {
            let next = segs[check_maven as usize + 2];
            if next == "java" || next == "resources" {
                root = check_maven + 2;
            }
        }
    }
    root
}

/// `JavaUtil.getJavaPath`: the path below the Java source root, if there is one.
fn java_path(path: &str) -> Option<String> {
    let norm = PathFragment::create(path);
    let norm_str = norm.path_string();
    if norm_str.is_empty() {
        return None;
    }
    let segs: Vec<&str> = norm_str.split('/').collect();
    let idx = java_segment_index_impl(&segs);
    if idx >= 0 {
        Some(segs[(idx as usize + 1)..].join("/"))
    } else {
        None
    }
}

/// `BazelJavaSemantics.getDefaultJavaResourcePath`.
fn default_java_resource_path(path: &str) -> String {
    let norm = PathFragment::create(path);
    let norm_str = norm.path_string();
    let segs: Vec<&str> = norm_str.split('/').collect();
    for i in 0..segs.len().saturating_sub(2) {
        if segs[i] == "src" && segs[i + 2] == "resources" {
            return segs[(i + 3)..].join("/");
        }
    }
    java_path(norm_str).unwrap_or_else(|| norm_str.to_string())
}

/// `JavaHelper.getJavaResourcePath`.
fn java_resource_path(st: &Arc<CtxState>, r: &Artifact) -> Result<String, StarlarkError> {
    // `Artifact.getOutputDirRelativePath`: includes `external/<repo>/` unless sibling layout.
    let mut p = if r.is_source { jpath(r) } else { r.root_relative_path.path_string().to_string() };
    let repo = st.label.package_identifier().repository().name().to_string();
    if !repo.is_empty() {
        let prefix = if st.inputs.sibling_repository_layout {
            format!("../{repo}/")
        } else {
            format!("external/{repo}/")
        };
        if let Some(rest) = p.strip_prefix(&prefix) {
            p = rest.to_string();
        }
    }
    let p_frag = PathFragment::create(&p);
    let p_norm = p_frag.path_string().to_string();
    let explicit_strip = match st.rule.extra.attr_values.get("resource_strip_prefix") {
        Some(v @ AttrValue::Select(_)) => crate::select::resolve_select(
            v,
            "resource_strip_prefix",
            &st.rule,
            &st.cfg,
            st.inputs.resolver.as_ref(),
        )
        .ok(),
        other => other.cloned(),
    };
    match explicit_strip {
        Some(AttrValue::Str(prefix)) => {
            let pre_frag = PathFragment::create(&prefix);
            let pre_norm = pre_frag.path_string();
            if pre_norm.is_empty() || pre_norm == "." {
                return Ok(p_norm);
            }
            if p_norm == pre_norm {
                return Ok(String::new());
            }
            if let Some(rest) = p_norm.strip_prefix(&format!("{pre_norm}/")) {
                return Ok(rest.to_string());
            }
            st.rule_errors.lock().unwrap().push(format!(
                "in resource_strip_prefix attribute of {} rule {}: Resource file '{}' is not under the specified prefix to strip",
                st.rule.rule_class,
                st.label.canonical_form(),
                p_norm
            ));
            Ok(p_norm)
        }
        _ => Ok(default_java_resource_path(&p_norm)),
    }
}

/// `ResourceJarActionBuilder.build`: a singlejar action packaging resources into `output`.
fn java_resource_jar_action(
    st: &Arc<CtxState>,
    toolchain: &Value,
    output: &Artifact,
    resources: &[(String, Artifact)],
    classpath_resources: &[Artifact],
    resource_jars: &[Artifact],
    additional_inputs: &[Artifact],
) -> Result<(), StarlarkError> {
    let single = jf(toolchain, "single_jar").ok_or_else(|| StarlarkError("java_toolchain has no single_jar".into()))?;
    let Value::ProviderInstance { fields, .. } = &single else { return err("single_jar is not a FilesToRunProvider") };
    let exe = fields
        .get("executable")
        .and_then(artifact_of)
        .ok_or_else(|| StarlarkError("single_jar has no executable".into()))?;
    let mut cmd: Vec<String> = vec![
        "--normalize".into(),
        "--dont_change_compression".into(),
        "--exclude_build_data".into(),
        "--no_strip_module_info".into(),
        "--output".into(),
        jpath(output),
    ];
    if !resource_jars.is_empty() {
        cmd.push("--sources".into());
        cmd.extend(resource_jars.iter().map(jpath));
    }
    if !resources.is_empty() {
        cmd.push("--resources".into());
        for (path, art) in resources {
            let ep = jpath(art);
            cmd.push(if &ep == path { ep } else { format!("{ep}:{path}") });
        }
    }
    if !classpath_resources.is_empty() {
        cmd.push("--classpath_resources".into());
        cmd.extend(classpath_resources.iter().map(jpath));
    }
    let mut a = Action::new(
        st.label.clone(),
        "JavaResourceJar",
        ActionKind::Spawn {
            executable: jpath(&exe),
            arguments: vec![CommandLineArgs {
                args: cmd,
                param_file: Some(crate::action::ParamFileInfo {
                    flag_format: "@%s".to_string(),
                    format: crate::action::ParamFileFormat::Shell,
                    use_always: false,
                    leading_args: 0,
                    flag_groups: Vec::new(),
                    explicit_path: None,
                }),
            }],
            shell_command: None,
        },
    );
    a.progress_message = Some("Building Java resource jar".to_string());
    // `ResourceJarActionBuilder.EXECUTION_INFO`.
    a.execution_requirements.insert("supports-path-mapping".to_string(), "1".to_string());
    let mut inputs: Vec<Artifact> = resources.iter().map(|(_, r)| r.clone()).collect();
    inputs.extend(resource_jars.iter().cloned());
    inputs.extend(additional_inputs.iter().cloned());
    inputs.extend(classpath_resources.iter().cloned());
    add_files_to_run_inputs(fields, &mut inputs);
    a.tools = vec![exe.clone()];
    a.inputs = dedup(inputs);
    a.outputs = vec![output.clone()];
    a.use_default_shell_env = true;
    apply_java_rule_tags_and_exec_group(st, &mut a, true)?;
    st.register(a)?;
    Ok(())
}

/// `JavaCompilationHelper.createGenJarAction`: runs the GenClass tool on the compile output.
fn java_gen_jar_action(
    st: &Arc<CtxState>,
    toolchain: &Value,
    class_jar: &Artifact,
    manifest: Option<&Artifact>,
    gen_class_jar: &Artifact,
) -> Result<(), StarlarkError> {
    let manifest = manifest.ok_or_else(|| StarlarkError("gen_class requires a manifest_proto".into()))?;
    let genclass = jf(toolchain, "_gen_class")
        .as_ref()
        .and_then(artifact_of)
        .ok_or_else(|| StarlarkError("java_toolchain has no genclass jar".into()))?;
    let rt = jf(toolchain, "java_runtime").ok_or_else(|| StarlarkError("java_toolchain has no java_runtime".into()))?;
    let java = match jf(&rt, "java_executable_exec_path") {
        Some(Value::String(s)) => s,
        _ => return err("java_runtime has no java_executable_exec_path"),
    };
    let mut args: Vec<String> = jstrs(jf(toolchain, "jvm_opt").as_ref());
    args.push("-jar".into());
    args.push(jpath(&genclass));
    args.extend([
        "--manifest_proto".into(),
        jpath(manifest),
        "--class_jar".into(),
        jpath(class_jar),
        "--output_jar".into(),
        jpath(gen_class_jar),
    ]);
    let mut a = Action::new(
        st.label.clone(),
        "JavaSourceJar",
        ActionKind::Spawn {
            executable: java,
            arguments: vec![CommandLineArgs { args, param_file: None }],
            shell_command: None,
        },
    );
    a.progress_message = Some(format!("Building genclass jar {}", gen_class_jar.root_relative_path));
    let mut inputs = vec![manifest.clone(), class_jar.clone(), genclass];
    inputs.extend(cc_artifacts(jf(&rt, "files").as_ref()));
    a.inputs = dedup(inputs);
    a.outputs = vec![gen_class_jar.clone()];
    a.env = utf8_env(st);
    a.use_default_shell_env = true;
    st.register(a)?;
    Ok(())
}

#[cfg(test)]
mod java_compile_lowering_tests {
    use super::*;
    use crate::ActionAnalysisMetadata;

    struct NoopResolver;

    impl DepResolver for NoopResolver {
        fn configured_target(
            &self,
            _label: &Label,
            _cfg: &BuildConfigurationKey,
        ) -> Result<Arc<ConfiguredTarget>, String> {
            Err("unexpected dependency lookup".into())
        }
    }

    fn test_ctx_state(label: &Label) -> Arc<CtxState> {
        let rule = RuleInstance {
            label: label.clone(),
            rule_class: "java_library".into(),
            attrs: BTreeMap::new(),
            srcs: Vec::new(),
            deps: Vec::new(),
            outs: Vec::new(),
            tools: Vec::new(),
            data: Vec::new(),
            visibility: Vec::new(),
            cmd: None,
            executable: false,
            testonly: false,
            extra: Default::default(),
        };
        let empty_struct = Value::Struct(Arc::new(StructValue::new(BTreeMap::new())));
        Arc::new(CtxState {
            rule,
            label: label.clone(),
            cfg: BuildConfigurationKey::default(),
            inputs: AnalysisInputs::new(Arc::new(NoopResolver)),
            pkg_dir: PathFragment::create("pkg"),
            output_to_genfiles: false,
            exec_groups: BTreeSet::new(),
            deps: BTreeMap::new(),
            attr_struct: empty_struct.clone(),
            file_struct: empty_struct.clone(),
            files_struct: empty_struct.clone(),
            executable_struct: empty_struct.clone(),
            outputs_struct: empty_struct,
            predeclared: Vec::new(),
            default_executable: None,
            default_executable_accessed: Mutex::new(false),
            actions: Mutex::new(Vec::new()),
            graph: Mutex::new(ActionGraph::new()),
            declared_trees: Mutex::new(BTreeSet::new()),
            declared_files: Mutex::new(BTreeSet::new()),
            declared_artifacts: Mutex::new(Vec::new()),
            rule_errors: Mutex::new(Vec::new()),
            make_vars: BTreeMap::new(),
            frozen: Mutex::new(false),
            rule_struct: None,
            aspect_ids: Vec::new(),
            aspect_fragments: Vec::new(),
            aspect_toolchains_declared: Vec::new(),
            base_toolchains: BTreeMap::new(),
            base_exec_groups: BTreeSet::new(),
            base_exec_group_toolchains: BTreeMap::new(),
            base_exec_group_platforms: BTreeMap::new(),
            split_attr: Value::None,
            in_subrule: std::sync::atomic::AtomicBool::new(false),
        })
    }

    #[test]
    fn java_register_compile_classpath_arg_index_accounts_for_leading_jvm_args() {
        let label = Label::parse_canonical("//pkg:lib").unwrap();
        let st = test_ctx_state(&label);
        let out_jar = Artifact::derived(
            "k8-fastbuild",
            PathFragment::create("pkg/liblib.jar"),
            label.clone(),
        );
        let jdeps = Artifact::derived(
            "k8-fastbuild",
            PathFragment::create("pkg/liblib.jdeps"),
            label.clone(),
        );
        let dep_jar = Artifact::derived(
            "k8-fastbuild",
            PathFragment::create("pkg/libdep.jar"),
            label.clone(),
        );
        let dep_jdeps = Artifact::derived(
            "k8-fastbuild",
            PathFragment::create("pkg/libdep.jdeps"),
            label.clone(),
        );
        let src = Artifact::source(PathFragment::create("pkg/A.java"), label);

        java_register_compile(
            &st,
            "Javac",
            &[
                "external/jdk/bin/java".into(),
                "-Xms64m".into(),
                "-jar".into(),
                "JavaBuilder.jar".into(),
            ],
            vec![
                "--output".into(),
                out_jar.exec_path.path_string().to_string(),
                "--sources".into(),
                src.exec_path.path_string().to_string(),
            ],
            vec![dep_jar.clone()],
            vec![dep_jar],
            vec![dep_jdeps],
            Vec::new(),
            vec![src],
            Vec::new(),
            vec![out_jar.clone(), jdeps.clone()],
            Some(jdeps),
            "BAZEL",
            BTreeMap::new(),
            BTreeMap::new(),
            "Building pkg/liblib.jar".into(),
        )
        .unwrap();

        let actions = st.actions.lock().unwrap();
        assert_eq!(actions.len(), 1);
        let lowered = actions[0].lower();
        assert_eq!(lowered.len(), 2, "{lowered:?}");

        let flags_params = format!("{}-0.params", out_jar.exec_path.path_string());
        let cp_params = format!("{}-1.params", out_jar.exec_path.path_string());

        match &lowered[0] {
            ActionAnalysisMetadata::FileWriteAction {
                output, content, ..
            } => {
                assert_eq!(output.exec_path.path_string(), flags_params);
                let text = std::str::from_utf8(content).unwrap();
                assert!(text.contains("--output\n"), "{text}");
                assert!(text.contains("--sources\n"), "{text}");
            }
            other => panic!("expected FileWriteAction for flags params, got {other:?}"),
        }

        match &lowered[1] {
            ActionAnalysisMetadata::SpawnAction {
                argv, java_compile, ..
            } => {
                assert_eq!(
                    argv,
                    &vec![
                        "external/jdk/bin/java".to_string(),
                        "-Xms64m".to_string(),
                        "-jar".to_string(),
                        "JavaBuilder.jar".to_string(),
                        format!("@{flags_params}"),
                        format!("@{cp_params}"),
                    ]
                );
                let jc = java_compile.as_ref().expect("expected JavaCompileSpec");
                assert_eq!(jc.classpath_arg_index, 2);
                assert_eq!(jc.classpath_params_path, cp_params);
            }
            other => panic!("expected SpawnAction, got {other:?}"),
        }
    }
}
