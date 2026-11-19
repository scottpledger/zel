// Analysis-time natives of `cc_common.internal_DO_NOT_USE()` (`CcStarlarkInternal.java`) that need
// the rule context (declare outputs, register actions). Included into `starlark_rule.rs`.

use zel_builtins::cc_features::{self, field, items, FeatureConfiguration, Variables};

fn cc_kw<'a>(pos: &'a [Value], kw: &'a BTreeMap<String, Value>, i: usize, name: &str) -> Option<&'a Value> {
    kw.get(name).or_else(|| pos.get(i)).filter(|v| !matches!(v, Value::None))
}

fn cc_artifacts(v: Option<&Value>) -> Vec<Artifact> {
    v.map(|v| items(v).iter().filter_map(artifact_of).collect()).unwrap_or_default()
}

fn host_exec_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "DARWIN"
    } else if cfg!(windows) {
        "WINDOWS"
    } else if cfg!(target_os = "freebsd") {
        "FREEBSD"
    } else if cfg!(target_os = "openbsd") {
        "OPENBSD"
    } else if cfg!(target_os = "linux") {
        "LINUX"
    } else {
        "UNKNOWN"
    }
}

fn os_from_constraint_collection(cc: &crate::ConstraintCollectionData) -> Option<&'static str> {
    for (setting, val) in &cc.entries {
        let Some(Value::Label(sl)) = field(setting, "label") else {
            continue;
        };
        if sl.name() == "os" && sl.package_identifier().package_fragment().path_string() == "os" {
            if let Some(Value::Label(vl)) = field(val, "label") {
                return Some(match vl.name() {
                    "macos" | "osx" | "ios" | "tvos" | "watchos" | "visionos" => "DARWIN",
                    "windows" => "WINDOWS",
                    "freebsd" => "FREEBSD",
                    "openbsd" => "OPENBSD",
                    "linux" | "android" => "LINUX",
                    _ => "UNKNOWN",
                });
            }
        }
    }
    cc.parent
        .as_deref()
        .and_then(os_from_constraint_collection)
}

fn exec_os_for_ctx(st: &CtxState) -> &'static str {
    if let Some(ep) = &st.inputs.exec_platform {
        if let Ok(lbl) = Label::parse_canonical(ep) {
            if let Ok(ct) = st.inputs.resolver.configured_target(&lbl, &st.cfg) {
                if let Some(pi) = ct.get_actual().providers.get("@@//_builtins:PlatformInfo") {
                    if let Some(cval) = field(pi, "constraints") {
                        if let Some(cc) = crate::constraint_collection_of(&cval) {
                            if let Some(os) = os_from_constraint_collection(&cc) {
                                return os;
                            }
                        }
                    }
                }
            }
        }
        let lower = ep.to_ascii_lowercase();
        if lower.contains("windows") {
            return "WINDOWS";
        }
        if lower.contains("macos") || lower.contains("osx") || lower.contains("darwin") {
            return "DARWIN";
        }
        if lower.contains("freebsd") {
            return "FREEBSD";
        }
        if lower.contains("openbsd") {
            return "OPENBSD";
        }
        if lower.contains("linux") {
            return "LINUX";
        }
    }
    host_exec_os()
}

fn cc_per_label_matches(spec: &str, exec_path: &str, label: Option<&Label>) -> Result<bool, StarlarkError> {
    let mut incs = Vec::new();
    let mut excs = Vec::new();
    for raw_pat in spec.split(',').filter(|p| !p.is_empty()) {
        if let Some(neg) = raw_pat.strip_prefix('-') {
            let re = zel_util::java_regex::Regex::new(neg)
                .map_err(|e| StarlarkError(format!("invalid per-file regex '{neg}': {e}")))?;
            excs.push(re);
        } else {
            let pos_pat = raw_pat.strip_prefix('+').unwrap_or(raw_pat);
            let re = zel_util::java_regex::Regex::new(pos_pat)
                .map_err(|e| StarlarkError(format!("invalid per-file regex '{pos_pat}': {e}")))?;
            incs.push(re);
        }
    }
    let matches_one = |s: &str| -> bool {
        (incs.is_empty() || incs.iter().any(|r| r.find(s).is_some()))
            && !excs.iter().any(|r| r.find(s).is_some())
    };
    if let Some(lbl) = label {
        let canon = lbl.canonical_form();
        let noncanon = format!("//{}:{}", lbl.package_identifier().package_fragment().path_string(), lbl.name());
        if matches_one(&canon) || matches_one(&noncanon) {
            return Ok(true);
        }
    }
    Ok(matches_one(exec_path))
}

fn split_unescaped_commas(s: &str) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&',') {
            cur.push(',');
            chars.next();
        } else if c == ',' {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

fn cc_eval_per_file_opts(
    entries: &[String],
    file: &Artifact,
    label: Option<&Label>,
) -> Result<Vec<Value>, StarlarkError> {
    let exec_path = file.exec_path.path_string();
    let mut out = Vec::new();
    for entry in entries {
        let Some((filter_spec, opts_spec)) = entry.split_once('@') else {
            continue;
        };
        if filter_spec.is_empty() || opts_spec.is_empty() {
            continue;
        }
        if cc_per_label_matches(filter_spec, exec_path, label)? {
            for opt in split_unescaped_commas(opts_spec) {
                out.push(Value::String(opt));
            }
        }
    }
    Ok(out)
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
            Some(v @ Value::Host(k, _)) if k == "file" => Ok(mk_list(vec![v.clone()])),
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
        "aspect_class" => {
            if st.rule_struct.is_none() {
                Ok(Value::None)
            } else if let Some(id) = st.aspect_ids.last() {
                let cls = match id.split_once('%') {
                    Some((_, second)) => second,
                    None => id.as_str(),
                };
                Ok(Value::String(cls.to_string()))
            } else {
                Ok(Value::None)
            }
        }
        "stl_computed_default" if !pos.is_empty() || !kw.is_empty() => {
            let tags = cc_kw(pos, kw, 0, "tags");
            let has_stl = tags
                .map(|v| items(v).iter().any(|x| x.as_str() == Some("__CC_STL__")))
                .unwrap_or(false);
            if has_stl {
                Ok(Value::None)
            } else {
                Label::parse_canonical("@@//third_party/stl")
                    .map(Value::Label)
                    .map_err(|e| StarlarkError(e.to_string()))
            }
        }
        "stl_computed_default_fn" => {
            let tags = cc_kw(pos, kw, 0, "tags");
            let has_stl = tags
                .map(|v| items(v).iter().any(|x| x.as_str() == Some("__CC_STL__")))
                .unwrap_or(false);
            if has_stl {
                Ok(Value::None)
            } else {
                Label::parse_canonical("@@//third_party/stl")
                    .map(Value::Label)
                    .map_err(|e| StarlarkError(e.to_string()))
            }
        }
        "exec_os" => Ok(Value::String(exec_os_for_ctx(st).to_string())),
        "is_package_headers_checking_mode_set" => {
            if st.rule_struct.is_some() {
                return Some(err("is_package_headers_checking_mode_set: expected rule context, got aspect context"));
            }
            Ok(Value::Bool(st.rule.extra.pkg_default_hdrs_check.is_some()))
        }
        "package_headers_checking_mode" => {
            if st.rule_struct.is_some() {
                return Some(err("package_headers_checking_mode: expected rule context, got aspect context"));
            }
            Ok(Value::String(
                st.rule
                    .extra
                    .pkg_default_hdrs_check
                    .clone()
                    .unwrap_or_else(|| "strict".to_string()),
            ))
        }
        "is_package_headers_checking_mode_set_for_aspect" => {
            if st.rule_struct.is_none() {
                return Some(err("is_package_headers_checking_mode_set_for_aspect: expected aspect context"));
            }
            Ok(Value::Bool(st.rule.extra.pkg_default_hdrs_check.is_some()))
        }
        "package_headers_checking_mode_for_aspect" => {
            if st.rule_struct.is_none() {
                return Some(err("package_headers_checking_mode_for_aspect: expected aspect context"));
            }
            Ok(Value::String(
                st.rule
                    .extra
                    .pkg_default_hdrs_check
                    .clone()
                    .unwrap_or_else(|| "strict".to_string()),
            ))
        }
        "declare_compile_output_file" => cc_declare_compile_output(st, pos, kw),
        "declare_other_output_file" => cc_declare_other_output(st, pos, kw),
        "create_cc_compile_action" => cc_create_compile_action(st, pos, kw),
        "create_cc_compile_action_template" => cc_create_compile_action_template(st, pos, kw),
        "create_lto_backend_action" => cc_create_lto_backend_action(st, pos, kw),
        "create_lto_backend_action_template" => cc_create_lto_backend_action_template(st, pos, kw),
        "get_link_args" => cc_get_link_args(pos, kw),
        "compute_output_name_prefix_dir" => cc_compute_output_name_prefix_dir(st, pos, kw),
        "dynamic_library_symlink" => cc_dynamic_library_symlink(st, pos, kw),
        "dynamic_library_symlink2" => cc_dynamic_library_symlink2(st, pos, kw),
        "dynamic_library_soname" => cc_dynamic_library_soname(st, pos, kw),
        "solib_symlink_action" => cc_solib_symlink_action(st, pos, kw),
        "absolute_symlink" => cc_absolute_symlink(st, pos, kw),
        "check_toplevel" => Ok(Value::None),
        "is_tree_artifact" => cc_is_tree_artifact(pos, kw),
        "expand_and_tokenize" => cc_expand_and_tokenize(st, pos, kw),
        "per_file_copts" => {
            let src = cc_kw(pos, kw, 1, "source_file")
                .and_then(artifact_of)
                .ok_or_else(|| StarlarkError("per_file_copts: source_file must be a File".into()));
            let src = match src {
                Ok(a) => a,
                Err(e) => return Some(Err(e)),
            };
            let lbl = match cc_kw(pos, kw, 2, "label") {
                Some(Value::Label(l)) => Some(l.clone()),
                _ => None,
            };
            let entries = crate::fragments::Opts::new(&st.cfg).list_unsplit("per_file_copt");
            cc_eval_per_file_opts(&entries, &src, lbl.as_ref()).map(mk_list)
        }
        "collect_per_file_lto_backend_opts" => {
            let obj = cc_kw(pos, kw, 1, "object")
                .and_then(artifact_of)
                .ok_or_else(|| StarlarkError("collect_per_file_lto_backend_opts: object must be a File".into()));
            let obj = match obj {
                Ok(a) => a,
                Err(e) => return Some(Err(e)),
            };
            let entries = crate::fragments::Opts::new(&st.cfg).list_unsplit("per_file_ltobackendopt");
            cc_eval_per_file_opts(&entries, &obj, None).map(mk_list)
        }
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

/// `CppCompileActionBuilder.initializeActionName`: determines the default action name for
/// `source` when no explicit `action_name` is supplied.
fn cc_default_compile_action(source: &str, fc: &FeatureConfiguration) -> Result<&'static str, StarlarkError> {
    if source.ends_with(".cppmap") {
        return Ok("c++-module-compile");
    }
    if source.ends_with(".pic.s") {
        return err(format!(
            "Assembler source '{source}' should not have '.pic.s' extension"
        ));
    }
    let ext = source.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "h" | "hh" | "hpp" | "hxx" | "h++" | "inc" | "inl" | "tlh" | "tli" | "H" | "tcc" => {
            if fc.is_enabled("parse_headers") {
                Ok("c++-header-parsing")
            } else {
                err(format!(
                    "Header file '{source}' cannot be compiled without 'parse_headers' feature enabled"
                ))
            }
        }
        "c" => Ok("c-compile"),
        "cc" | "cpp" | "cxx" | "c++" | "C" | "cu" | "cl" => Ok("c++-compile"),
        "m" => Ok("objc-compile"),
        "mm" => Ok("objc++-compile"),
        "s" | "asm" => Ok("assemble"),
        "S" => Ok("preprocess-assemble"),
        "ipb" => Ok("clif-match"),
        "pcm" | "gcm" | "ifc" => Ok("c++-module-codegen"),
        _ => err(format!("Unrecognized source file type for '{source}'")),
    }
}

fn cc_default_compile_mnemonic(action_name: &str) -> &'static str {
    match action_name {
        "objc-compile" | "objc++-compile" => "ObjcCompile",
        "linkstamp-compile" => "CppLinkstampCompile",
        "c++-header-analysis" => "CppHeaderAnalysis",
        "c++-header-parsing" => "CppCompileHeader",
        "c++-deps-scanning" | "c-deps-scanning" | "objc-deps-scanning" | "objc++-deps-scanning" => {
            "CppDepsScanning"
        }
        _ => "CppCompile",
    }
}

fn cc_filtered_compile_argv(
    fc: &FeatureConfiguration,
    action_name: &str,
    vars: &Variables,
    copts_filter: Option<&str>,
) -> Result<Vec<String>, StarlarkError> {
    let path_of = |v: &Value| artifact_of(v).map(|a| a.exec_path.path_string().to_string());
    let Some(pat) = copts_filter.filter(|s| !s.is_empty()) else {
        return fc
            .command_line(action_name, vars, &path_of)
            .map_err(|e| StarlarkError(e.0));
    };
    let re = zel_util::java_regex::Regex::new(pat)
        .map_err(|e| StarlarkError(format!("invalid copts_filter regex '{pat}': {e}")))?;
    let buckets = fc
        .per_feature_command_line(action_name, vars, &path_of)
        .map_err(|e| StarlarkError(e.0))?;
    let mut out = Vec::new();
    for (feat, flags) in buckets {
        if feat == "unfiltered_compile_flags" {
            out.extend(flags);
        } else {
            for f in flags {
                if re.find(&f).is_some() {
                    out.push(f);
                }
            }
        }
    }
    Ok(out)
}

fn cc_build_exec_reqs(
    fc: &FeatureConfiguration,
    action_name: &str,
    caller_reqs: Option<&Value>,
) -> BTreeMap<String, String> {
    let mut exec_reqs = BTreeMap::new();
    if let Some(Value::Dict(d)) = caller_reqs {
        for (k, v) in &d.lock().unwrap().entries {
            if let Some(ks) = k.as_str() {
                exec_reqs.insert(ks.to_string(), v.as_str().unwrap_or("").to_string());
            }
        }
    }
    for r in fc.tool_requirements_for_action(action_name).unwrap_or_default() {
        exec_reqs.entry(r).or_default();
    }
    exec_reqs
}

fn cc_build_compile_env(
    fc: &FeatureConfiguration,
    action_name: &str,
    vars: &Variables,
    exec_reqs: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, StarlarkError> {
    let path_of = |v: &Value| artifact_of(v).map(|a| a.exec_path.path_string().to_string());
    let mut env = BTreeMap::new();
    if !exec_reqs.contains_key("requires-darwin") {
        env.insert("PWD".to_string(), "/proc/self/cwd".to_string());
    }
    for (k, v) in fc
        .environment(action_name, vars, &path_of)
        .map_err(|e| StarlarkError(e.0))?
    {
        env.insert(k, v);
    }
    Ok(env)
}

fn cc_collect_compilation_context_inputs(ctxv: Option<&Value>, inputs: &mut Vec<Artifact>) {
    let Some(ctxv) = ctxv else {
        return;
    };
    for f in [
        "headers",
        "_non_code_inputs",
        "_module_files",
        "direct_textual_headers",
        "_exporting_module_map_files",
    ] {
        inputs.extend(cc_artifacts(field(ctxv, f).as_ref()));
    }
    if let Some(hi) = field(ctxv, "_header_info")
        .as_ref()
        .and_then(zel_builtins::cc_internal::get_header_info)
    {
        for h in &hi.transitive_headers {
            if let Some(a) = artifact_of(h) {
                inputs.push(a);
            }
        }
        for h in &hi.textual_headers {
            if let Some(a) = artifact_of(h) {
                inputs.push(a);
            }
        }
    }
    if let Some(a) = field(ctxv, "_module_map")
        .as_ref()
        .and_then(|m| field(m, "file"))
        .as_ref()
        .and_then(artifact_of)
    {
        inputs.push(a);
    }
    inputs.extend(cc_artifacts(field(ctxv, "_direct_module_maps").as_ref()));
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
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => cc_default_compile_action(source.exec_path.path_string(), &fc)?.to_string(),
    };
    if !fc.action_is_configured(&action_name) {
        return err(format!(
            "Action {action_name} does not have an enabled configuration in the toolchain."
        ));
    }
    let copts_filter = cc_kw(pos, kw, 100, "copts_filter").and_then(|v| v.as_str());
    let tool = fc.tool_path_for_action(&action_name).map_err(|e| StarlarkError(e.0))?;
    let argv = cc_filtered_compile_argv(&fc, &action_name, &vars, copts_filter)?;
    let exec_reqs = cc_build_exec_reqs(&fc, &action_name, cc_kw(pos, kw, 100, "execution_requirements"));
    let env = cc_build_compile_env(&fc, &action_name, &vars, &exec_reqs)?;

    // Exact output ordering (`CppCompileAction.getOutputs`):
    // `[output_file, gcno_file, ...additional_outputs, dotd_file, diagnostics_file, dwo_file, lto_indexing_file]`.
    let output_file = cc_kw(pos, kw, 100, "output_file").and_then(artifact_of);
    let mut outputs: Vec<Artifact> = Vec::new();
    if let Some(a) = &output_file {
        outputs.push(a.clone());
    }
    if let Some(a) = cc_kw(pos, kw, 100, "gcno_file").and_then(artifact_of) {
        outputs.push(a);
    }
    outputs.extend(cc_artifacts(cc_kw(pos, kw, 100, "additional_outputs")));
    for k in ["dotd_file", "diagnostics_file", "dwo_file", "lto_indexing_file"] {
        if let Some(a) = cc_kw(pos, kw, 100, k).and_then(artifact_of) {
            outputs.push(a);
        }
    }
    if outputs.is_empty() {
        return err("create_cc_compile_action: no outputs");
    }

    let param_file = if fc.is_enabled("compiler_param_file") {
        let primary = output_file.as_ref().unwrap_or(&outputs[0]);
        Some(crate::action::ParamFileInfo {
            flag_format: "@%s".to_string(),
            format: ParamFileFormat::GccQuoted,
            use_always: true,
            leading_args: 0,
            flag_groups: Vec::new(),
            explicit_path: Some(format!("{}.params", primary.exec_path.path_string())),
        })
    } else {
        None
    };

    // Inputs: source, additional inputs, headers of the compilation context, toolchain files.
    let mut inputs: Vec<Artifact> = vec![source.clone()];
    inputs.extend(cc_artifacts(cc_kw(pos, kw, 100, "additional_compilation_inputs")));
    inputs.extend(cc_artifacts(cc_kw(pos, kw, 100, "additional_compilation_inputs_set")));
    inputs.extend(cc_artifacts(cc_kw(pos, kw, 100, "additional_prunable_headers")));
    inputs.extend(cc_artifacts(cc_kw(pos, kw, 100, "build_info_header_files")));
    inputs.extend(cc_artifacts(cc_kw(pos, kw, 100, "module_files")));
    cc_collect_compilation_context_inputs(cc_kw(pos, kw, 1, "cc_compilation_context"), &mut inputs);
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
    let mnemonic = match cc_kw(pos, kw, 100, "mnemonic") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => cc_default_compile_mnemonic(&action_name).to_string(),
    };
    let mut a = Action::new(
        st.label.clone(),
        &mnemonic,
        ActionKind::Spawn {
            executable: tool,
            arguments: vec![CommandLineArgs { args: argv, param_file }],
            shell_command: None,
        },
    );
    a.progress_message = Some(format!("{prefix} {label_name}"));
    a.env = env;
    a.execution_requirements = exec_reqs;
    inputs.extend(tools.iter().cloned());
    a.inputs = inputs;
    a.tools = dedup(tools);
    a.outputs = outputs;
    a.use_default_shell_env = cc_kw(pos, kw, 100, "use_default_shell_env")
        .map(Value::is_truthy)
        .unwrap_or(true);
    st.register(a)?;
    Ok(Value::None)
}

fn cc_create_compile_action_template(st: &Arc<CtxState>, pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    let fc = cc_kw(pos, kw, 5, "feature_configuration")
        .and_then(|v| cc_features::lookup::<FeatureConfiguration>(v, cc_features::KIND_FEATURE_CONFIGURATION))
        .ok_or_else(|| StarlarkError("create_cc_compile_action_template: invalid feature_configuration".into()))?;
    let vars: Arc<Variables> = cc_kw(pos, kw, 100, "compile_build_variables")
        .and_then(cc_features::variables_of)
        .ok_or_else(|| StarlarkError("create_cc_compile_action_template: invalid compile_build_variables".into()))?;
    let source = cc_kw(pos, kw, 100, "source")
        .and_then(artifact_of)
        .ok_or_else(|| StarlarkError("create_cc_compile_action_template: source must be a File".into()))?;
    if !source.is_tree_artifact {
        return err("create_cc_compile_action_template: source must be a tree artifact");
    }
    let output_tree = cc_kw(pos, kw, 100, "output_files")
        .or_else(|| cc_kw(pos, kw, 100, "output_file"))
        .and_then(artifact_of)
        .ok_or_else(|| StarlarkError("create_cc_compile_action_template: output_files must be a tree artifact".into()))?;
    if !output_tree.is_tree_artifact {
        return err("create_cc_compile_action_template: output_files must be a tree artifact");
    }
    let action_name = match cc_kw(pos, kw, 100, "action_name") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => "c++-compile".to_string(),
    };
    if !fc.action_is_configured(&action_name) {
        return err(format!(
            "Action {action_name} does not have an enabled configuration in the toolchain."
        ));
    }
    let copts_filter = cc_kw(pos, kw, 100, "copts_filter").and_then(|v| v.as_str());
    let tool = fc.tool_path_for_action(&action_name).map_err(|e| StarlarkError(e.0))?;
    let argv = cc_filtered_compile_argv(&fc, &action_name, &vars, copts_filter)?;
    let exec_reqs = cc_build_exec_reqs(&fc, &action_name, cc_kw(pos, kw, 100, "execution_requirements"));
    let env = cc_build_compile_env(&fc, &action_name, &vars, &exec_reqs)?;
    let mut outputs = vec![output_tree];
    if let Some(dotd_tree) = cc_kw(pos, kw, 100, "dotd_tree_artifact")
        .or_else(|| cc_kw(pos, kw, 100, "dotd_file"))
        .and_then(artifact_of)
    {
        outputs.push(dotd_tree);
    }
    if let Some(diag_tree) = cc_kw(pos, kw, 100, "diagnostics_output_tree")
        .or_else(|| cc_kw(pos, kw, 100, "diagnostics_file"))
        .and_then(artifact_of)
    {
        outputs.push(diag_tree);
    }
    if let Some(lto_tree) = cc_kw(pos, kw, 100, "lto_indexing_tree_artifact")
        .or_else(|| cc_kw(pos, kw, 100, "lto_indexing_file"))
        .and_then(artifact_of)
    {
        outputs.push(lto_tree);
    }
    let mut inputs = vec![source.clone()];
    inputs.extend(cc_artifacts(cc_kw(pos, kw, 100, "additional_compilation_inputs")));
    inputs.extend(cc_artifacts(cc_kw(pos, kw, 100, "additional_compilation_inputs_set")));
    cc_collect_compilation_context_inputs(cc_kw(pos, kw, 1, "cc_compilation_context"), &mut inputs);
    let mut tools = Vec::new();
    if let Some(tc) = cc_kw(pos, kw, 2, "cc_toolchain") {
        for f in ["_compiler_files", "compiler_files", "_builtin_include_files"] {
            tools.extend(cc_artifacts(field(tc, f).as_ref()));
        }
    }
    let mut a = Action::new(
        st.label.clone(),
        cc_default_compile_mnemonic(&action_name),
        ActionKind::Spawn {
            executable: tool,
            arguments: vec![CommandLineArgs { args: argv, param_file: None }],
            shell_command: None,
        },
    );
    a.progress_message = Some(format!("Compiling {}", source.exec_path.path_string()));
    a.env = env;
    a.execution_requirements = exec_reqs;
    inputs.extend(tools.iter().cloned());
    a.inputs = inputs;
    a.tools = dedup(tools);
    a.outputs = outputs;
    a.use_default_shell_env = true;
    st.register(a)?;
    Ok(Value::None)
}

fn cc_create_lto_backend_action(st: &Arc<CtxState>, pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    let fc = cc_kw(pos, kw, 1, "feature_configuration")
        .and_then(|v| cc_features::lookup::<FeatureConfiguration>(v, cc_features::KIND_FEATURE_CONFIGURATION))
        .ok_or_else(|| StarlarkError("create_lto_backend_action: invalid feature_configuration".into()))?;
    let vars: Arc<Variables> = cc_kw(pos, kw, 3, "build_variables")
        .or_else(|| cc_kw(pos, kw, 100, "compile_build_variables"))
        .and_then(cc_features::variables_of)
        .ok_or_else(|| StarlarkError("create_lto_backend_action: invalid build_variables".into()))?;
    let bitcode = cc_kw(pos, kw, 100, "bitcode_file")
        .and_then(artifact_of)
        .ok_or_else(|| StarlarkError("create_lto_backend_action: bitcode_file must be a File".into()))?;
    let object_file = cc_kw(pos, kw, 100, "object_file")
        .and_then(artifact_of)
        .ok_or_else(|| StarlarkError("create_lto_backend_action: object_file must be a File".into()))?;
    let action_name = "lto-backend";
    if !fc.action_is_configured(action_name) {
        return err("Action lto-backend does not have an enabled configuration in the toolchain.");
    }
    let path_of = |v: &Value| artifact_of(v).map(|a| a.exec_path.path_string().to_string());
    let tool = fc.tool_path_for_action(action_name).map_err(|e| StarlarkError(e.0))?;
    let argv = fc.command_line(action_name, &vars, &path_of).map_err(|e| StarlarkError(e.0))?;
    let exec_reqs = cc_build_exec_reqs(&fc, action_name, None);
    let env = cc_build_compile_env(&fc, action_name, &vars, &exec_reqs)?;
    let param_file = if fc.is_enabled("compiler_param_file") {
        Some(crate::action::ParamFileInfo {
            flag_format: "@%s".to_string(),
            format: ParamFileFormat::GccQuoted,
            use_always: true,
            leading_args: 0,
            flag_groups: Vec::new(),
            explicit_path: Some(format!("{}.params", object_file.exec_path.path_string())),
        })
    } else {
        None
    };
    let mut outputs = vec![object_file.clone()];
    if let Some(dwo) = cc_kw(pos, kw, 100, "dwo_file").and_then(artifact_of) {
        outputs.push(dwo);
    }
    let mut inputs = vec![bitcode.clone()];
    for k in ["index", "imports"] {
        if let Some(a) = cc_kw(pos, kw, 100, k).and_then(artifact_of) {
            inputs.push(a);
        }
    }
    inputs.extend(cc_artifacts(cc_kw(pos, kw, 100, "bitcode_inputs")));
    let mut tools = Vec::new();
    if let Some(tc) = cc_kw(pos, kw, 2, "cc_toolchain") {
        for f in ["_compiler_files", "compiler_files", "_all_files", "all_files"] {
            tools.extend(cc_artifacts(field(tc, f).as_ref()));
        }
    }
    let mut a = Action::new(
        st.label.clone(),
        "CcLtoBackendCompile",
        ActionKind::Spawn {
            executable: tool,
            arguments: vec![CommandLineArgs { args: argv, param_file }],
            shell_command: None,
        },
    );
    a.progress_message = Some(format!("LTO Backend Compile {}", object_file.exec_path.path_string()));
    a.env = env;
    a.execution_requirements = exec_reqs;
    inputs.extend(tools.iter().cloned());
    a.inputs = inputs;
    a.tools = dedup(tools);
    a.outputs = outputs;
    a.use_default_shell_env = true;
    st.register(a)?;
    Ok(Value::None)
}

fn cc_create_lto_backend_action_template(st: &Arc<CtxState>, pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    let fc = cc_kw(pos, kw, 1, "feature_configuration")
        .and_then(|v| cc_features::lookup::<FeatureConfiguration>(v, cc_features::KIND_FEATURE_CONFIGURATION))
        .ok_or_else(|| StarlarkError("create_lto_backend_action_template: invalid feature_configuration".into()))?;
    let vars: Arc<Variables> = cc_kw(pos, kw, 3, "build_variables")
        .or_else(|| cc_kw(pos, kw, 100, "compile_build_variables"))
        .and_then(cc_features::variables_of)
        .ok_or_else(|| StarlarkError("create_lto_backend_action_template: invalid build_variables".into()))?;
    let bitcode = cc_kw(pos, kw, 100, "bitcode_file")
        .or_else(|| cc_kw(pos, kw, 100, "bitcode_directory"))
        .and_then(artifact_of)
        .ok_or_else(|| StarlarkError("create_lto_backend_action_template: bitcode_file must be a tree artifact".into()))?;
    let object_dir = cc_kw(pos, kw, 100, "object_file")
        .or_else(|| cc_kw(pos, kw, 100, "object_directory"))
        .and_then(artifact_of)
        .ok_or_else(|| StarlarkError("create_lto_backend_action_template: object_file must be a tree artifact".into()))?;
    if !bitcode.is_tree_artifact || !object_dir.is_tree_artifact {
        return err("create_lto_backend_action_template: bitcode and object artifacts must be tree artifacts");
    }
    let action_name = "lto-backend";
    if !fc.action_is_configured(action_name) {
        return err("Action lto-backend does not have an enabled configuration in the toolchain.");
    }
    let path_of = |v: &Value| artifact_of(v).map(|a| a.exec_path.path_string().to_string());
    let tool = fc.tool_path_for_action(action_name).map_err(|e| StarlarkError(e.0))?;
    let argv = fc.command_line(action_name, &vars, &path_of).map_err(|e| StarlarkError(e.0))?;
    let exec_reqs = cc_build_exec_reqs(&fc, action_name, None);
    let env = cc_build_compile_env(&fc, action_name, &vars, &exec_reqs)?;
    let mut outputs = vec![object_dir.clone()];
    if let Some(dwo) = cc_kw(pos, kw, 100, "dwo_file")
        .or_else(|| cc_kw(pos, kw, 100, "dwo_directory"))
        .and_then(artifact_of)
    {
        outputs.push(dwo);
    }
    let mut inputs = vec![bitcode];
    for k in ["index", "imports"] {
        if let Some(a) = cc_kw(pos, kw, 100, k).and_then(artifact_of) {
            inputs.push(a);
        }
    }
    let mut tools = Vec::new();
    if let Some(tc) = cc_kw(pos, kw, 2, "cc_toolchain") {
        for f in ["_compiler_files", "compiler_files", "_all_files", "all_files"] {
            tools.extend(cc_artifacts(field(tc, f).as_ref()));
        }
    }
    let mut a = Action::new(
        st.label.clone(),
        "CcLtoBackendCompile",
        ActionKind::Spawn {
            executable: tool,
            arguments: vec![CommandLineArgs { args: argv, param_file: None }],
            shell_command: None,
        },
    );
    a.progress_message = Some(format!("LTO Backend Compile {}", object_dir.exec_path.path_string()));
    a.env = env;
    a.execution_requirements = exec_reqs;
    inputs.extend(tools.iter().cloned());
    a.inputs = inputs;
    a.tools = dedup(tools);
    a.outputs = outputs;
    a.use_default_shell_env = false;
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
        Some(other) => return err(format!("get_link_args: parameter_file_type must be string or None, got {}", other.type_name())),
        None => None,
    };
    let placeholder = argv
        .iter()
        .find(|s| s.contains("LINKER_PARAM_FILE_PLACEHOLDER"))
        .cloned();
    if let Some(var) = vars.flattened().get("linker_param_file").and_then(|v| {
        artifact_of(v)
            .map(|a| a.exec_path.path_string().to_string())
            .or_else(|| v.as_str().map(str::to_string))
    }) {
        argv.retain(|s| !s.contains(&var));
    } else if placeholder.is_some() {
        argv.retain(|s| !s.contains("LINKER_PARAM_FILE_PLACEHOLDER"));
    }
    let mut builder = ArgsBuilder::default();
    if let Some(t) = param_type {
        let format = match t.as_str() {
            "UNQUOTED" => ParamFileFormat::Multiline,
            "GCC_QUOTED" => ParamFileFormat::GccQuoted,
            "SHELL_QUOTED" => ParamFileFormat::Shell,
            "WINDOWS" => ParamFileFormat::Windows,
            other => {
                return err(format!(
                    "No enum constant ParameterFile.ParameterFileType.{other}"
                ))
            }
        };
        if let Some(p) = placeholder {
            let flag_format = p
                .replace('%', "%%")
                .replace("LINKER_PARAM_FILE_PLACEHOLDER", "%s");
            builder.param_file = Some(crate::action::ParamFileInfo {
                flag_format,
                format,
                use_always: true,
                leading_args: 0,
                flag_groups: Vec::new(),
                explicit_path: None,
            });
            builder.format = Some(format);
        }
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
    let norm = PathFragment::create(library_path);
    let norm_str = norm.path_string();
    if preserve_name {
        norm_str.rsplit('/').next().unwrap_or(norm_str).to_string()
    } else {
        let mangling = mnemonic.find("ST-").map(|i| format!("{}_", &mnemonic[i..])).unwrap_or_default();
        format!("lib{mangling}{}", cc_escaped_path(norm_str))
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
    let norm_link = PathFragment::create(link_name);
    let sym = st.derived(norm_link);
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
    let solib_dir = PathFragment::create(&cc_str(pos, kw, 2, "solib_directory", "dynamic_library_symlink")?)
        .path_string()
        .to_string();
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
    let solib_dir = PathFragment::create(&cc_str(pos, kw, 2, "solib_directory", "dynamic_library_symlink2")?)
        .path_string()
        .to_string();
    let path = PathFragment::create(&cc_str(pos, kw, 3, "path", "dynamic_library_symlink2")?)
        .path_string()
        .to_string();
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
        Some(Value::String(s)) => PathFragment::create(s).path_string().to_string(),
        _ => PathFragment::create(&solib_dir).path_string().to_string(),
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

/// `CcStarlarkInternal.isTreeArtifact`: `@Param(name = "artifact", allowedTypes = {Artifact.class})`
/// (positional-only, mandatory `File`).
fn cc_is_tree_artifact(pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    let first = match pos.first() {
        Some(v) => Some(want_file(v, "is_tree_artifact", "artifact")?),
        None => None,
    };
    if let Some(k) = kw.keys().next() {
        if k == "artifact" {
            return err(
                "is_tree_artifact() got named argument for positional-only parameter 'artifact'",
            );
        }
        return err(format!(
            "is_tree_artifact() got unexpected keyword argument '{k}'"
        ));
    }
    if pos.len() > 1 {
        return err(format!(
            "is_tree_artifact() accepts no more than 1 positional argument but got {}",
            pos.len()
        ));
    }
    let Some(artifact) = first else {
        return err("is_tree_artifact() missing 1 required positional argument: artifact");
    };
    Ok(Value::Bool(artifact.is_tree_artifact))
}

/// `CcStarlarkInternal.expandAndTokenize`: make-variable + `$(location)` expansion, then shell
/// tokenization of every flag.
fn cc_expand_and_tokenize(st: &Arc<CtxState>, pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    let attr = cc_str(pos, kw, 1, "attr", "expand_and_tokenize")?;
    let flags: Vec<String> = cc_kw(pos, kw, 2, "flags").map(|v| items(v).iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
    if flags.is_empty() {
        return Ok(mk_list(vec![]));
    }
    let mut map: BTreeMap<Label, Vec<Artifact>> = BTreeMap::new();
    for k in ["srcs", "non_arc_srcs", "hdrs", "data", "additional_linker_inputs"] {
        if let Some(ts) = st.deps.get(k) {
            for t in ts {
                insert_target_location_keys(&mut map, t, target_files(t));
            }
        }
    }
    let lookup = |n: &str| st.make_vars.get(n).cloned();
    let repo_map = st.repo_mapping();
    let mut out = Vec::new();
    for s in flags {
        let s = expand_with_locations(
            &s,
            &lookup,
            &map,
            &st.label,
            LocationMode::Exec,
            &st.inputs.workspace_name,
            Some(&repo_map),
            cfg!(windows),
        )
        .map_err(|e| StarlarkError(format!("attribute {attr}: {e}")))?
        .text;
        for t in shell_tokenize(&s).map_err(|e| StarlarkError(format!("attribute {attr}: {e}")))? {
            out.push(Value::String(t));
        }
    }
    Ok(mk_list(out))
}

#[cfg(test)]
mod cc_native_tests {
    use super::*;
    use crate::ArtifactRoot;

    #[test]
    fn is_tree_artifact_matches_bazel_positional_only_signature() {
        let owner = Label::parse_canonical("//pkg:target").unwrap();
        let reg = Artifact::derived_in_root(
            "k8-fastbuild",
            ArtifactRoot::Bin,
            PathFragment::create("pkg/file.cc"),
            owner.clone(),
        );
        let mut tree = Artifact::derived_in_root(
            "k8-fastbuild",
            ArtifactRoot::Bin,
            PathFragment::create("pkg/tree_dir"),
            owner,
        );
        tree.is_tree_artifact = true;

        let reg_val = file_value(&reg);
        let tree_val = file_value(&tree);

        assert_eq!(
            cc_is_tree_artifact(&[tree_val.clone()], &BTreeMap::new()).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            cc_is_tree_artifact(&[reg_val.clone()], &BTreeMap::new()).unwrap(),
            Value::Bool(false)
        );

        // Bazel 9.3.2 CcStarlarkInternal.isTreeArtifact declares @Param(name = "artifact")
        // with default named = false (positional-only), so `artifact = ...` must fail rather
        // than silently returning False.
        let mut named_artifact = BTreeMap::new();
        named_artifact.insert("artifact".to_string(), tree_val.clone());
        let err_named = cc_is_tree_artifact(&[], &named_artifact).unwrap_err();
        assert_eq!(
            err_named.0,
            "is_tree_artifact() got named argument for positional-only parameter 'artifact'"
        );

        let mut named_file = BTreeMap::new();
        named_file.insert("file".to_string(), tree_val.clone());
        let err_file = cc_is_tree_artifact(&[], &named_file).unwrap_err();
        assert_eq!(
            err_file.0,
            "is_tree_artifact() got unexpected keyword argument 'file'"
        );

        let err_missing = cc_is_tree_artifact(&[], &BTreeMap::new()).unwrap_err();
        assert_eq!(
            err_missing.0,
            "is_tree_artifact() missing 1 required positional argument: artifact"
        );

        let err_extra = cc_is_tree_artifact(&[tree_val, reg_val], &BTreeMap::new()).unwrap_err();
        assert_eq!(
            err_extra.0,
            "is_tree_artifact() accepts no more than 1 positional argument but got 2"
        );

        let err_type =
            cc_is_tree_artifact(&[Value::String("not_a_file".into())], &BTreeMap::new())
                .unwrap_err();
        assert_eq!(
            err_type.0,
            "in call to is_tree_artifact(), parameter 'artifact' got value of type 'string', want 'File'"
        );
    }
}
