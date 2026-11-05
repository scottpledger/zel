// Analysis-time natives of `_builtins.internal.java_common_internal_do_not_use`
// (`JavaStarlarkCommon.java`) that need the rule context. Included into `starlark_rule.rs`.
//
// Ported: expand_java_opts, check_provider_instances, get_runtime_classpath_for_archive,
// target_kind, collect_native_deps_dirs, _check_java_toolchain_is_declared_on_rule.
// Not yet ported (explicit errors): create_compilation_action, create_header_compilation_action,
// create_jvm_compile_action (JavaCompileActionBuilder / JavaHeaderCompileAction).

/// `ShellUtils.tokenize`: Bourne-shell-like word splitting with quotes and backslash escapes.
fn shell_tokenize(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' {
                    match chars.next() {
                        Some(n) => cur.push(n),
                        None => return Err("unterminated escape".into()),
                    }
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    in_word = true;
                }
                '\\' => {
                    in_word = true;
                    match chars.next() {
                        Some(n) => cur.push(n),
                        None => return Err("unterminated escape".into()),
                    }
                }
                c if c.is_whitespace() => {
                    if in_word {
                        out.push(std::mem::take(&mut cur));
                        in_word = false;
                    }
                }
                c => {
                    cur.push(c);
                    in_word = true;
                }
            },
        }
    }
    if quote.is_some() {
        return Err(format!("unterminated quotation in: {s}"));
    }
    if in_word {
        out.push(cur);
    }
    Ok(out)
}

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
                    Some(Value::List(l)) => l.lock().unwrap().elements.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
                    Some(Value::String(x)) => vec![x.clone()],
                    _ => vec![],
                },
                _ => vec![],
            };
            let mut map: BTreeMap<Label, Vec<Artifact>> = BTreeMap::new();
            for k in ["srcs", "deps", "data", "tools", "resources"] {
                if let Some(ts) = st.deps.get(k) {
                    for t in ts {
                        map.entry(t.label.clone()).or_default().extend(target_files(t));
                    }
                }
            }
            let mode = if exec_paths { LocationMode::Exec } else { LocationMode::Runfiles };
            let lookup = |n: &str| st.make_vars.get(n).cloned();
            let mut out = Vec::new();
            for s in raw {
                let s = expand_make_variables(&s, &lookup).map_err(|e| StarlarkError(format!("attribute {attr}: {e}")))?;
                let s = expand_locations(&s, &map, &st.label, mode, &st.inputs.workspace_name).map_err(StarlarkError)?;
                if tokenize {
                    for t in shell_tokenize(&s).map_err(|e| StarlarkError(format!("attribute {attr}: {e}")))? {
                        out.push(Value::String(t));
                    }
                } else {
                    out.push(Value::String(s));
                }
            }
            Ok(mk_list(out))
        })(),
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

/// `JavaHelper.tokenizeJavaOptions(NestedSet)`: the depset's options in reverse order, each
/// shell-tokenized (kept verbatim when tokenization fails).
fn java_tokenize_opts(v: Option<&Value>) -> Vec<String> {
    let mut raw = jstrs(v);
    raw.reverse();
    let mut out = Vec::new();
    for o in raw {
        match shell_tokenize(&o) {
            Ok(t) => out.extend(t),
            Err(_) => out.push(o),
        }
    }
    out
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

fn utf8_env(st: &Arc<CtxState>) -> BTreeMap<String, String> {
    let mac = st.cfg.platforms.iter().any(|p| p.contains("osx") || p.contains("macos")) || cfg!(target_os = "macos");
    let mut m = BTreeMap::new();
    m.insert("LC_CTYPE".to_string(), if mac { "en_US.UTF-8" } else { "C.UTF-8" }.to_string());
    m
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
    let inmemory = st.cfg.options.get("experimental_inmemory_jdeps_files").map_or(true, |v| v == "true" || v == "1");
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
        !cc_artifacts(jf(b, "bootclasspath").as_ref()).is_empty() || jf(b, "_system_path").is_some()
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

/// `create_header_compilation_action` (`JavaCompilationHelper.createHeaderCompilationAction` +
/// `JavaHeaderCompileAction.Builder.build`) for the direct-classpath (turbine) path.
fn java_create_header_action(st: &Arc<CtxState>, pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    let g = |i: usize, k: &str| kw.get(k).or_else(|| pos.get(i)).filter(|v| !matches!(v, Value::None));
    let toolchain = g(1, "java_toolchain").ok_or_else(|| StarlarkError("missing java_toolchain".into()))?;
    let out_jar = g(2, "header_jar").and_then(artifact_of).ok_or_else(|| StarlarkError("header_jar must be a File".into()))?;
    let deps_proto = g(3, "header_deps_proto").and_then(artifact_of).ok_or_else(|| StarlarkError("header_deps_proto must be a File".into()))?;
    let (proc_classes, proc_jars, proc_data) = plugin_data(g(4, "plugin_info"), "api_generating_plugins");
    let sources = cc_artifacts(g(5, "source_files"));
    let source_jars = cc_artifacts(g(6, "source_jars"));
    let classpath = cc_artifacts(g(7, "compile_time_classpath"));
    let direct_jars = cc_artifacts(g(8, "direct_jars"));
    let (boot, _aux, _sys, _sysp) = boot_class_path(g(9, "boot_class_path"), toolchain);
    let javacopts = java_tokenize_opts(g(11, "javac_opts"));
    let strict = g(12, "strict_deps_mode").and_then(|v| v.as_str().map(|s| s.to_uppercase())).unwrap_or_else(|| "ERROR".into());
    let target_label = match g(13, "target_label") {
        Some(Value::Label(l)) => Some(l.clone()),
        _ => None,
    };
    let injecting = g(14, "injecting_rule_kind").and_then(|v| v.as_str().map(str::to_string));
    let enable_direct = g(15, "enable_direct_classpath").map(|v| v.is_truthy()).unwrap_or(true);
    let additional_inputs = cc_artifacts(g(16, "additional_inputs"));
    let header_compilation_jar = g(17, "header_compilation_jar").and_then(artifact_of);
    let hdr_direct = cc_artifacts(g(18, "header_compilation_direct_deps"));

    let builtin = jstrs(jf(toolchain, "_header_compiler_builtin_processors").as_ref());
    let direct_tool = jf(toolchain, "_header_compiler_direct");
    let use_direct_cp = enable_direct && proc_classes.is_empty();
    let use_hcd = direct_tool.is_some() && proc_classes.iter().all(|p| builtin.contains(p));
    let tool = if use_hcd { direct_tool } else { jf(toolchain, "_header_compiler") }
        .ok_or_else(|| StarlarkError("java_toolchain has no header compiler".into()))?;
    let (exe_line, tool_inputs) = java_tool_command(&tool, toolchain)?;
    let tools_jars = cc_artifacts(jf(toolchain, "tools").as_ref());

    let (direct_jars, dep_artifacts_ignored) = if strict == "OFF" { (vec![], true) } else { (direct_jars, false) };
    let _ = dep_artifacts_ignored;

    let mut c = JCmd::default();
    c.exec_path("--output", Some(&out_jar));
    c.exec_path("--header_compilation_output", header_compilation_jar.as_ref());
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
    if let Some(h) = &header_compilation_jar {
        outputs.push(h.clone());
    }

    if !use_direct_cp {
        // Annotation-processing header compilation runs through the javac-based action.
        if !use_hcd {
            inputs.extend(proc_jars.iter().cloned());
            inputs.extend(proc_data.iter().cloned());
        }
        let builtin_used: Vec<String> = proc_classes.iter().filter(|p| builtin.contains(p)).cloned().collect();
        c.all("--builtin_processors", &builtin_used);
        c.all("--processors", &proc_classes);
        if !use_hcd {
            c.exec_paths("--processorpath", &proc_jars);
        }
        if strict != "OFF" {
            c.exec_paths("--direct_dependencies", &direct_jars);
        }
        let mut cp = JCmd::default();
        cp.exec_paths("--classpath", &classpath);
        inputs.extend(classpath.iter().cloned());
        return java_register(
            st,
            "JavacTurbine",
            &exe_line,
            vec![c.0, cp.0],
            inputs,
            tools_jars,
            outputs,
            java_progress("Compiling Java headers", &out_jar, sources.len(), source_jars.len(), &proc_classes),
            java_exec_info(st, toolchain, false, Some(&deps_proto)),
        );
    }
    let cp = if !hdr_direct.is_empty() || classpath.is_empty() { hdr_direct } else { classpath };
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
    st.register(a)?;
    Ok(Value::None)
}

/// `JavaConfiguration.getReduceJavaClasspath` (`--experimental_java_classpath`, default `bazel`):
/// `OFF`, `JAVABUILDER`, `BAZEL` or `BAZEL_NO_FALLBACK`.
fn java_classpath_mode(st: &Arc<CtxState>) -> String {
    match st.cfg.options.get("experimental_java_classpath").map(|v| v.to_uppercase()) {
        Some(m) if matches!(m.as_str(), "OFF" | "JAVABUILDER" | "BAZEL" | "BAZEL_NO_FALLBACK") => m,
        _ => "BAZEL".to_string(),
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
    let (proc_classes, proc_jars, proc_data) = plugin_data(g(4, "plugin_info"), "plugins");
    let classpath = cc_artifacts(g(5, "compile_time_classpath"));
    let direct_jars = cc_artifacts(g(6, "direct_jars"));
    let (boot, aux, sys_inputs, sys_path) = boot_class_path(g(7, "boot_class_path"), toolchain);
    let builder_jvm_flags = jstrs(g(8, "javabuilder_jvm_flags"));
    let dep_artifacts = cc_artifacts(g(9, "compile_time_java_deps"));
    let javacopts = java_tokenize_opts(g(10, "javac_opts"));
    let strict = g(11, "strict_deps_mode").and_then(|v| v.as_str().map(|s| s.to_uppercase())).unwrap_or_else(|| "ERROR".into());
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
    // `javaToolchain.getJavaBuilder().withAdditionalJvmFlags(javaBuilderJvmFlags)`: flags precede `-jar`.
    if !builder_jvm_flags.is_empty() {
        let pos_jar = exe_line.iter().position(|s| s == "-jar").unwrap_or(exe_line.len());
        for (i, f) in builder_jvm_flags.iter().enumerate() {
            exe_line.insert(pos_jar + i, f.clone());
        }
    }
    let tools_jars = cc_artifacts(jf(toolchain, "tools").as_ref());
    let incompatible = jstrs(jf(toolchain, "_reduced_classpath_incompatible_processors").as_ref());
    // JavaCompileActionBuilder.build: strict deps OFF ignores direct jars and dependency artifacts;
    // incompatible processors, strict deps OFF or a missing .jdeps turn the classpath mode OFF.
    let strict_off = strict == "OFF";
    let (direct_jars, dep_artifacts) = if strict_off { (vec![], vec![]) } else { (direct_jars, dep_artifacts) };
    let mut mode = java_classpath_mode(st);
    if proc_classes.iter().any(|p| incompatible.contains(p)) || strict_off || deps_proto.is_none() {
        mode = "OFF".to_string();
    }
    // JavaCompilationHelper: with bootclasspath auxiliary entries they are classpath entries and
    // direct jars too.
    let direct_all: Vec<Artifact> = if aux.is_empty() { direct_jars } else { aux.iter().cloned().chain(direct_jars).collect() };

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
    c.opt("--experimental_fix_deps_tool", Some("add_dep"));

    // Full classpath (bootclasspath auxiliary entries precede the compile-time classpath).
    let mut full_cp: Vec<Artifact> = aux;
    full_cp.extend(classpath);

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
    let segs: Vec<&str> = path.split('/').collect();
    let mut idx = -1;
    if path.starts_with("third_party/java") && segs.len() >= 2 {
        let nested = java_segment_index_impl(&segs[2..]);
        if nested >= 0 {
            idx = nested + 2;
        }
    }
    if idx < 0 {
        idx = java_segment_index_impl(&segs);
    }
    if idx >= 0 {
        Some(segs[(idx as usize + 1)..].join("/"))
    } else {
        None
    }
}

/// `BazelJavaSemantics.getDefaultJavaResourcePath`.
fn default_java_resource_path(path: &str) -> String {
    let segs: Vec<&str> = path.split('/').collect();
    for i in 0..segs.len().saturating_sub(2) {
        if segs[i] == "src" && segs[i + 2] == "resources" {
            return segs[(i + 3)..].join("/");
        }
    }
    java_path(path).unwrap_or_else(|| path.to_string())
}

/// `JavaHelper.getJavaResourcePath`.
fn java_resource_path(st: &Arc<CtxState>, r: &Artifact) -> Result<String, StarlarkError> {
    // `Artifact.getOutputDirRelativePath`: includes `external/<repo>/` for external repositories.
    let mut p = if r.is_source { jpath(r) } else { r.root_relative_path.path_string().to_string() };
    let repo = st.label.package_identifier().repository().name().to_string();
    if !repo.is_empty() {
        let prefix = format!("external/{repo}/");
        if let Some(rest) = p.strip_prefix(&prefix) {
            p = rest.to_string();
        }
    }
    match st.rule.extra.attr_values.get("resource_strip_prefix") {
        Some(AttrValue::Str(prefix)) if !prefix.is_empty() => {
            let pre = format!("{}/", prefix.trim_end_matches('/'));
            match p.strip_prefix(&pre) {
                Some(rest) => Ok(rest.to_string()),
                None => err(format!("attribute resource_strip_prefix: Resource file '{p}' is not under the specified prefix to strip")),
            }
        }
        _ => Ok(default_java_resource_path(&p)),
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
    a.use_default_shell_env = true;
    st.register(a)?;
    Ok(())
}
