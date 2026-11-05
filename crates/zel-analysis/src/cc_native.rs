// Analysis-time natives of `cc_common.internal_DO_NOT_USE()` (`CcStarlarkInternal.java`) that need
// the rule context (declare outputs, register actions). Included into `starlark_rule.rs`.

use zel_builtins::cc_features::{self, field, items, FeatureConfiguration, Variables};

fn cc_kw<'a>(pos: &'a [Value], kw: &'a BTreeMap<String, Value>, i: usize, name: &str) -> Option<&'a Value> {
    kw.get(name).or_else(|| pos.get(i)).filter(|v| !matches!(v, Value::None))
}

fn cc_artifacts(v: Option<&Value>) -> Vec<Artifact> {
    v.map(|v| items(v).iter().filter_map(artifact_of).collect()).unwrap_or_default()
}

/// Handles `cc_internal.<name>` natives that need analysis state; `None` if `name` is not one.
fn cc_native(
    st: &Arc<CtxState>,
    _ctx: &mut EvalContext,
    name: &str,
    pos: &[Value],
    kw: &BTreeMap<String, Value>,
) -> Option<HostResult> {
    if name == "method:tree_expander.expand" {
        // Directory contents are only known at execution time: a tree artifact expands to
        // itself (callers pass its path to tools that expand it).
        return Some(match pos.get(1) {
            Some(v @ Value::Host(k, _)) if k == "file" => Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new(vec![v.clone()]))))),
            _ => err("TreeArtifactExpander.expand: expected a File"),
        });
    }
    if let Some(r) = java_native(st, name, pos, kw) {
        return Some(r);
    }
    if let Some(r) = py_native(st, _ctx, name, pos, kw) {
        return Some(r);
    }
    let n = name.strip_prefix("cc_internal.")?;
    Some(match n {
        // The wrapped factory only changes how link outputs are rooted; the plain factory serves.
        "wrap_link_actions" => Ok(pos.first().cloned().unwrap_or(Value::Host("actions".into(), "0".into()))),
        "actions2ctx_cheat" => Ok(Value::Host("ctx".into(), "0".into())),
        "rule_class" => Ok(Value::String(st.rule.rule_class.clone())),
        "aspect_class" => Ok(Value::None),
        "declare_compile_output_file" => cc_declare_compile_output(st, pos, kw),
        "declare_other_output_file" => cc_declare_other_output(st, pos, kw),
        "create_cc_compile_action" => cc_create_compile_action(st, pos, kw),
        "get_link_args" => cc_get_link_args(pos, kw),
        "compute_output_name_prefix_dir" => cc_compute_output_name_prefix_dir(st, pos, kw),
        "dynamic_library_symlink" => cc_dynamic_library_symlink(st, pos, kw),
        "dynamic_library_symlink2" => cc_dynamic_library_symlink2(st, pos, kw),
        "dynamic_library_soname" => cc_dynamic_library_soname(st, pos, kw),
        "solib_symlink_action" => cc_solib_symlink_action(st, pos, kw),
        "absolute_symlink" => cc_absolute_symlink(st, pos, kw),
        "check_toplevel" => Ok(Value::None),
        "expand_and_tokenize" => cc_expand_and_tokenize(st, pos, kw),
        "per_file_copts" | "collect_per_file_lto_backend_opts" => Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new(vec![]))))),
        _ => return None,
    })
}

fn cc_declare_compile_output(st: &Arc<CtxState>, pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    let label = match cc_kw(pos, kw, 1, "label") {
        Some(Value::Label(l)) => l.clone(),
        _ => st.label.clone(),
    };
    let output_name = match cc_kw(pos, kw, 2, "output_name") {
        Some(Value::String(s)) => s.clone(),
        _ => return err("declare_compile_output_file: output_name must be a string"),
    };
    // `CppHelper.getCompileOutputArtifact`: <package>/_objs/<target name>/<output_name>.
    let pkg = label.package_identifier().package_fragment().path_string().to_string();
    let rel = if pkg.is_empty() { format!("_objs/{}/{output_name}", label.name()) } else { format!("{pkg}/_objs/{}/{output_name}", label.name()) };
    Ok(file_value(&st.derived(PathFragment::create(&rel))))
}

fn cc_declare_other_output(st: &Arc<CtxState>, pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    let output_name = match cc_kw(pos, kw, 1, "output_name") {
        Some(Value::String(s)) => s.clone(),
        _ => return err("declare_other_output_file: output_name must be a string"),
    };
    let obj = cc_kw(pos, kw, 2, "object_file")
        .and_then(artifact_of)
        .ok_or_else(|| StarlarkError("declare_other_output_file: object_file must be a File".into()))?;
    let orig = obj.root_relative_path.path_string().to_string();
    let dir = orig.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
    let rel = if dir.is_empty() { output_name } else { format!("{dir}/{output_name}") };
    Ok(file_value(&st.derived(PathFragment::create(&rel))))
}

/// The action name of a compile for `source` when the caller did not give one
/// (`CppCompileActionBuilder`/`CppHelper.getActionNameForSource`-style).
fn cc_default_compile_action(source: &str) -> &'static str {
    let ext = source.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "c" => "c-compile",
        "s" => "assemble",
        "S" => "preprocess-assemble",
        "h" | "hh" | "hpp" | "hxx" | "inc" | "tcc" | "H" => "c++-header-parsing",
        _ => "c++-compile",
    }
}

fn cc_create_compile_action(st: &Arc<CtxState>, pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    let fc = cc_kw(pos, kw, 5, "feature_configuration")
        .and_then(|v| cc_features::lookup::<FeatureConfiguration>(v, cc_features::KIND_FEATURE_CONFIGURATION))
        .ok_or_else(|| StarlarkError("create_cc_compile_action: invalid feature_configuration".into()))?;
    let vars: Arc<Variables> = cc_kw(pos, kw, 100, "compile_build_variables")
        .and_then(cc_features::variables_of)
        .ok_or_else(|| StarlarkError("create_cc_compile_action: invalid compile_build_variables".into()))?;
    let source = cc_kw(pos, kw, 100, "source")
        .and_then(artifact_of)
        .ok_or_else(|| StarlarkError("create_cc_compile_action: source must be a File".into()))?;
    let action_name = match cc_kw(pos, kw, 100, "action_name") {
        Some(Value::String(s)) => s.clone(),
        _ => cc_default_compile_action(source.exec_path.path_string()).to_string(),
    };
    let path_of = |v: &Value| artifact_of(v).map(|a| a.exec_path.path_string().to_string());
    let tool = fc.tool_path_for_action(&action_name).map_err(|e| StarlarkError(e.0))?;
    let argv = fc.command_line(&action_name, &vars, &path_of).map_err(|e| StarlarkError(e.0))?;
    let env = fc.environment(&action_name, &vars, &path_of).map_err(|e| StarlarkError(e.0))?;
    let exec_req = fc.tool_requirements_for_action(&action_name).unwrap_or_default();

    // Outputs.
    let mut outputs: Vec<Artifact> = Vec::new();
    for k in ["output_file", "dotd_file", "diagnostics_file", "gcno_file", "dwo_file", "lto_indexing_file"] {
        if let Some(a) = cc_kw(pos, kw, 100, k).and_then(artifact_of) {
            outputs.push(a);
        }
    }
    outputs.extend(cc_artifacts(cc_kw(pos, kw, 100, "additional_outputs")));
    if outputs.is_empty() {
        return err("create_cc_compile_action: no outputs");
    }

    // Inputs: source, additional inputs, headers of the compilation context, toolchain files.
    let mut inputs: Vec<Artifact> = vec![source.clone()];
    inputs.extend(cc_artifacts(cc_kw(pos, kw, 100, "additional_compilation_inputs")));
    inputs.extend(cc_artifacts(cc_kw(pos, kw, 100, "additional_compilation_inputs_set")));
    inputs.extend(cc_artifacts(cc_kw(pos, kw, 100, "additional_prunable_headers")));
    inputs.extend(cc_artifacts(cc_kw(pos, kw, 100, "build_info_header_files")));
    inputs.extend(cc_artifacts(cc_kw(pos, kw, 100, "module_files")));
    if let Some(ctxv) = cc_kw(pos, kw, 1, "cc_compilation_context") {
        for f in ["headers", "_non_code_inputs", "_module_files", "direct_textual_headers", "_exporting_module_map_files"] {
            inputs.extend(cc_artifacts(field(ctxv, f).as_ref()));
        }
        // This target's own module map (CppCompileAction adds the compilation context's module map).
        if let Some(a) = field(ctxv, "_module_map").as_ref().and_then(|m| field(m, "file")).as_ref().and_then(artifact_of) {
            inputs.push(a);
        }
        // The module maps of direct dependencies (`-fmodule-map-file=` / layering_check).
        // (`_direct_module_maps` is a depset of module map *files*.)
        inputs.extend(cc_artifacts(field(ctxv, "_direct_module_maps").as_ref()));
    }
    // This target's own module map and its separate (".sep") variant.
    for k in ["cpp_module_map", "separate_cpp_module_map"] {
        if let Some(a) = cc_kw(pos, kw, 100, k).and_then(|m| field(m, "file")).as_ref().and_then(artifact_of) {
            inputs.push(a);
        }
    }
    let mut tools: Vec<Artifact> = Vec::new();
    if let Some(tc) = cc_kw(pos, kw, 2, "cc_toolchain") {
        for f in ["_compiler_files", "compiler_files", "_builtin_include_files"] {
            tools.extend(cc_artifacts(field(tc, f).as_ref()));
        }
    }
    let label_name = source.exec_path.path_string().to_string();
    let prefix = match cc_kw(pos, kw, 100, "progress_message_prefix") {
        Some(Value::String(s)) => s.clone(),
        _ => "Compiling".to_string(),
    };
    let mut a = Action::new(
        st.label.clone(),
        "CppCompile",
        ActionKind::Spawn {
            executable: tool,
            arguments: vec![CommandLineArgs { args: argv, param_file: None }],
            shell_command: None,
        },
    );
    a.progress_message = Some(format!("{prefix} {label_name}"));
    a.env = env.into_iter().collect();
    a.execution_requirements = exec_req.into_iter().map(|r| (r, String::new())).collect();
    inputs.extend(tools.iter().cloned());
    a.inputs = inputs;
    a.tools = dedup(tools);
    a.outputs = outputs;
    a.use_default_shell_env = true;
    st.register(a)?;
    Ok(Value::None)
}

/// `CcStarlarkInternal.getArgs`: the link command line of a feature configuration as an `Args`
/// object (with a param file when the toolchain splits long link command lines).
fn cc_get_link_args(pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    let action_name = match cc_kw(pos, kw, 0, "action_name") {
        Some(Value::String(s)) => s.clone(),
        _ => return err("get_link_args: action_name must be a string"),
    };
    let fc = cc_kw(pos, kw, 1, "feature_configuration")
        .and_then(|v| cc_features::lookup::<FeatureConfiguration>(v, cc_features::KIND_FEATURE_CONFIGURATION))
        .ok_or_else(|| StarlarkError("get_link_args: invalid feature_configuration".into()))?;
    let vars: Arc<Variables> = cc_kw(pos, kw, 2, "build_variables")
        .and_then(cc_features::variables_of)
        .ok_or_else(|| StarlarkError("get_link_args: invalid build_variables".into()))?;
    let path_of = |v: &Value| artifact_of(v).map(|a| a.exec_path.path_string().to_string());
    let mut argv = fc.command_line(&action_name, &vars, &path_of).map_err(|e| StarlarkError(e.0))?;
    let param_type = match cc_kw(pos, kw, 3, "parameter_file_type") {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    };
    let mut builder = ArgsBuilder::default();
    if let Some(t) = param_type {
        // The linker_param_file flag is dropped from the file's contents; its format string
        // (`LINKER_PARAM_FILE_PLACEHOLDER`) becomes the flag used to pass the file.
        let placeholder = argv.iter().find(|s| s.contains("LINKER_PARAM_FILE_PLACEHOLDER")).cloned();
        if let Some(var) = vars.flattened().get("linker_param_file").and_then(|v| artifact_of(v).map(|a| a.exec_path.path_string().to_string()).or_else(|| if let Value::String(s) = v { Some(s.clone()) } else { None })) {
            argv.retain(|s| !s.contains(&var));
        }
        let flag_format = match placeholder {
            Some(p) => p.replace('%', "%%").replace("LINKER_PARAM_FILE_PLACEHOLDER", "%s"),
            None => "@%s".to_string(),
        };
        let format = match t.as_str() {
            "UNQUOTED" => ParamFileFormat::Multiline,
            _ => ParamFileFormat::Shell,
        };
        builder.param_file = Some(crate::action::ParamFileInfo { flag_format, format, use_always: true, leading_args: 0 });
        builder.format = Some(format);
    }
    builder.args = argv;
    Ok(args_value(builder))
}


fn cc_str(pos: &[Value], kw: &BTreeMap<String, Value>, i: usize, name: &str, func: &str) -> Result<String, StarlarkError> {
    match cc_kw(pos, kw, i, name) {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => err(format!("{func}: {name} must be a string")),
    }
}

/// `CcStarlarkInternal.computeOutputNamePrefixDir`: only objc rules (`*_objc_arc` purposes or
/// mnemonics) get an `arc`/`non_arc` prefix.
fn cc_compute_output_name_prefix_dir(st: &Arc<CtxState>, pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    let mnemonic = match cc_kw(pos, kw, 1, "purpose") {
        Some(Value::String(p)) => p.clone(),
        _ => st.cfg.mnemonic(),
    };
    Ok(Value::String(
        if mnemonic.ends_with("_objc_arc") { if mnemonic.ends_with("_non_objc_arc") { "non_arc" } else { "arc" } } else { "" }.to_string(),
    ))
}

/// `Actions.escapedPath`.
fn cc_escaped_path(p: &str) -> String {
    let mut o = String::with_capacity(p.len());
    for c in p.chars() {
        match c {
            '_' => o.push_str("_U"),
            '/' => o.push_str("_S"),
            '\\' => o.push_str("_B"),
            ':' => o.push_str("_C"),
            '@' => o.push_str("_A"),
            c => o.push(c),
        }
    }
    o
}

/// `SolibSymlinkAction.getDynamicLibrarySoname`.
fn cc_soname(library_path: &str, preserve_name: bool, mnemonic: &str) -> String {
    if preserve_name {
        library_path.rsplit('/').next().unwrap_or(library_path).to_string()
    } else {
        let mangling = mnemonic.find("ST-").map(|i| format!("{}_", &mnemonic[i..])).unwrap_or_default();
        format!("lib{mangling}{}", cc_escaped_path(library_path))
    }
}

/// `SolibSymlinkAction.maybeHashPreserveExtension`.
fn cc_maybe_hash_name(f: &str) -> String {
    if f.len() <= 255 {
        return f.to_string();
    }
    let hashed = zel_util::to_hex(&zel_util::sha256::digest(f.as_bytes()));
    let base = f.rsplit('/').next().unwrap_or(f);
    match base.rsplit_once('.') {
        Some((n, ext)) if !n.is_empty() || !ext.is_empty() => format!("{hashed}.{ext}"),
        _ => hashed,
    }
}

fn cc_solib_symlink(st: &Arc<CtxState>, library: Artifact, link_name: &str) -> HostResult {
    if library.root_relative_path.path_string().starts_with("_solib_") {
        return err(format!("Library '{}' is already a solib symlink", library.root_relative_path.path_string()));
    }
    let sym = st.derived(PathFragment::create(link_name));
    let mut a = Action::new(st.label.clone(), "SolibSymlink", ActionKind::Symlink { target_file: Some(library.clone()), target_path: None, is_executable: false });
    a.inputs.push(library);
    a.outputs.push(sym.clone());
    st.register(a)?;
    Ok(file_value(&sym))
}

fn cc_artifact_arg(pos: &[Value], kw: &BTreeMap<String, Value>, i: usize, name: &str, func: &str) -> Result<Artifact, StarlarkError> {
    cc_kw(pos, kw, i, name).and_then(artifact_of).ok_or_else(|| StarlarkError(format!("{func}: {name} must be a File")))
}

/// `SolibSymlinkAction.getDynamicLibrarySymlink(ctx, solibDir, library, preserveName, prefixConsumer)`.
fn cc_dynamic_library_symlink(st: &Arc<CtxState>, pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    let library = cc_artifact_arg(pos, kw, 1, "library", "dynamic_library_symlink")?;
    let solib_dir = cc_str(pos, kw, 2, "solib_directory", "dynamic_library_symlink")?;
    let preserve = cc_kw(pos, kw, 3, "preserve_name").map(|v| v.is_truthy()).unwrap_or(false);
    let prefix_consumer = cc_kw(pos, kw, 4, "prefix_consumer").map(|v| v.is_truthy()).unwrap_or(false);
    let lib_path = library.root_relative_path.path_string().to_string();
    let escaped_rule = cc_escaped_path(&format!("_{}", st.label.canonical_form()));
    let soname = cc_soname(&lib_path, preserve, &st.cfg.mnemonic());
    let mangled = if preserve {
        let parent = lib_path.rsplit_once('/').map(|x| x.0).unwrap_or("");
        let escaped_lib = cc_escaped_path(&format!("_{parent}"));
        let full = if prefix_consumer { format!("{escaped_rule}__{escaped_lib}") } else { escaped_lib };
        format!("{solib_dir}/{}/{soname}", cc_maybe_hash_name(&full))
    } else {
        let filename = if prefix_consumer { format!("{escaped_rule}__{soname}") } else { soname };
        format!("{solib_dir}/{}", cc_maybe_hash_name(&filename))
    };
    cc_solib_symlink(st, library, &mangled)
}

/// `SolibSymlinkAction.getDynamicLibrarySymlink(ctx, solibDir, library, path)`.
fn cc_dynamic_library_symlink2(st: &Arc<CtxState>, pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    let library = cc_artifact_arg(pos, kw, 1, "library", "dynamic_library_symlink2")?;
    let solib_dir = cc_str(pos, kw, 2, "solib_directory", "dynamic_library_symlink2")?;
    let path = cc_str(pos, kw, 3, "path", "dynamic_library_symlink2")?;
    cc_solib_symlink(st, library, &format!("{solib_dir}/{path}"))
}

fn cc_dynamic_library_soname(st: &Arc<CtxState>, pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    let path = cc_str(pos, kw, 1, "path", "dynamic_library_soname")?;
    let preserve = cc_kw(pos, kw, 2, "preserve_name").map(|v| v.is_truthy()).unwrap_or(false);
    Ok(Value::String(cc_soname(&path, preserve, &st.cfg.mnemonic())))
}

/// `SolibSymlinkAction.getCppRuntimeSymlink`.
fn cc_solib_symlink_action(st: &Arc<CtxState>, pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    let library = cc_artifact_arg(pos, kw, 1, "artifact", "solib_symlink_action")?;
    let solib_dir = cc_str(pos, kw, 2, "solib_directory", "solib_symlink_action")?;
    let dir = match cc_kw(pos, kw, 3, "runtime_solib_dir_base") {
        Some(Value::String(s)) => s.clone(),
        _ => solib_dir,
    };
    let base = library.root_relative_path.path_string().rsplit('/').next().unwrap_or("").to_string();
    cc_solib_symlink(st, library, &format!("{dir}/{base}"))
}

/// `SymlinkAction.toAbsolutePath`.
fn cc_absolute_symlink(st: &Arc<CtxState>, pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    let out = cc_artifact_arg(pos, kw, 1, "output", "absolute_symlink")?;
    let target = cc_str(pos, kw, 2, "target_path", "absolute_symlink")?;
    let progress = cc_str(pos, kw, 3, "progress_message", "absolute_symlink")?;
    let mut a = Action::new(st.label.clone(), "Symlink", ActionKind::Symlink { target_file: None, target_path: Some(target), is_executable: false });
    a.progress_message = Some(progress);
    a.outputs.push(out);
    st.register(a)?;
    Ok(Value::None)
}

/// `CcStarlarkInternal.expandAndTokenize`: make-variable + `$(location)` expansion, then shell
/// tokenization of every flag.
fn cc_expand_and_tokenize(st: &Arc<CtxState>, pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    let attr = cc_str(pos, kw, 1, "attr", "expand_and_tokenize")?;
    let flags: Vec<String> = cc_kw(pos, kw, 2, "flags").map(|v| items(v).iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
    if flags.is_empty() {
        return Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new(vec![])))));
    }
    let mut map: BTreeMap<Label, Vec<Artifact>> = BTreeMap::new();
    for k in ["srcs", "non_arc_srcs", "hdrs", "data", "additional_linker_inputs"] {
        if let Some(ts) = st.deps.get(k) {
            for t in ts {
                map.entry(t.label.clone()).or_default().extend(target_files(t));
            }
        }
    }
    let lookup = |n: &str| st.make_vars.get(n).cloned();
    let mut out = Vec::new();
    for s in flags {
        let s = expand_make_variables(&s, &lookup).map_err(|e| StarlarkError(format!("attribute {attr}: {e}")))?;
        let s = expand_locations(&s, &map, &st.label, LocationMode::Exec, &st.inputs.workspace_name).map_err(StarlarkError)?;
        for t in shell_tokenize(&s).map_err(|e| StarlarkError(format!("attribute {attr}: {e}")))? {
            out.push(Value::String(t));
        }
    }
    Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new(out)))))
}
