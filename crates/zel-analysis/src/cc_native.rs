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
    for piece in split_unescaped_commas(spec) {
        let excluded = piece.starts_with('-');
        let raw_pat = if excluded {
            &piece[1..]
        } else {
            piece.strip_prefix('+').unwrap_or(&piece)
        };
        if raw_pat.is_empty() {
            continue;
        }
        let re = zel_util::java_regex::Regex::new(raw_pat)
            .map_err(|e| StarlarkError(format!("invalid per-file regex '{raw_pat}': {e}")))?;
        if excluded {
            excs.push(re);
        } else {
            incs.push(re);
        }
    }
    let matches_one = |s: &str| -> bool {
        (incs.is_empty() || incs.iter().any(|r| r.find(s).is_some()))
            && !excs.iter().any(|r| r.find(s).is_some())
    };
    if let Some(lbl) = label {
        let canon = lbl.canonical_form();
        let label_str = canon
            .strip_prefix("@@")
            .filter(|r| r.starts_with("//"))
            .unwrap_or(&canon);
        if matches_one(label_str) {
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

fn parse_per_label_entry(entry: &str) -> (String, Vec<String>) {
    // `PerLabelOptions.toString()` rendering stored in `BuildOptions`:
    // `<RegexFilter.toString()> Options: [<opt1>, <opt2>, ...]`, where `RegexFilter.toString()`
    // is empty or starts with `(?:(?>` / `-(?:(?>`.
    if let Some(without_bracket) = entry.strip_suffix(']') {
        if let Some((filter_part, opts_inner)) = without_bracket.split_once(" Options: [") {
            if filter_part.is_empty()
                || filter_part.starts_with("(?:(?>")
                || filter_part.starts_with("-(?:(?>")
            {
                let opts = if opts_inner.is_empty() {
                    Vec::new()
                } else {
                    opts_inner
                        .split(", ")
                        .filter(|s| !s.trim().is_empty())
                        .map(str::to_string)
                        .collect()
                };
                return (filter_part.to_string(), opts);
            }
        }
    }
    let chars: Vec<char> = entry.chars().collect();
    let at_pos = (0..chars.len()).find(|&i| chars[i] == '@' && (i == 0 || chars[i - 1] != '\\'));
    match at_pos {
        None => (entry.replace("\\@", "@"), Vec::new()),
        Some(idx) => {
            let filter_spec = chars[..idx].iter().collect::<String>().replace("\\@", "@");
            let opts_spec: String = chars[idx + 1..].iter().collect();
            let opts = split_unescaped_commas(&opts_spec)
                .into_iter()
                .filter(|s| !s.trim().is_empty())
                .collect();
            (filter_spec, opts)
        }
    }
}

/// `PerLabelOptions.PerLabelOptionsConverter.reverseForStarlark`: reconstructs `"<filter>@<opts>"`
/// when only the `BuildOptions`-converted `PerLabelOptions.toString()` representation is present.
pub(crate) fn reverse_per_label_for_starlark(entry: &str) -> String {
    if entry.contains(" Options: [") && entry.ends_with(']') {
        let (filter_spec, opts) = parse_per_label_entry(entry);
        if !filter_spec.is_empty() || !opts.is_empty() {
            let escaped = opts
                .iter()
                .map(|o| o.replace(',', "\\,"))
                .collect::<Vec<_>>()
                .join(",");
            return format!("{filter_spec}@{escaped}");
        }
    }
    entry.to_string()
}

fn cc_eval_per_file_opts(
    entries: &[String],
    file: &Artifact,
    label: Option<&Label>,
) -> Result<Vec<Value>, StarlarkError> {
    let exec_path = file.exec_path.path_string();
    let mut out = Vec::new();
    for entry in entries {
        if entry.is_empty() {
            continue;
        }
        let (filter_spec, opts) = parse_per_label_entry(entry);
        if opts.is_empty() {
            continue;
        }
        if cc_per_label_matches(&filter_spec, exec_path, label)? {
            for opt in opts {
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
            let entries = crate::fragments::Opts::new(&st.cfg).per_label_list("per_file_copt");
            cc_eval_per_file_opts(&entries, &src, lbl.as_ref()).map(mk_list)
        }
        "collect_per_file_lto_backend_opts" => {
            let obj = pos
                .get(1)
                .or_else(|| kw.get("obj"))
                .or_else(|| kw.get("object"))
                .and_then(artifact_of)
                .ok_or_else(|| StarlarkError("collect_per_file_lto_backend_opts: obj must be a File".into()));
            let obj = match obj {
                Ok(a) => a,
                Err(e) => return Some(Err(e)),
            };
            let entries = crate::fragments::Opts::new(&st.cfg).per_label_list("per_file_ltobackendopt");
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

/// `FileType.hasExtension(path, ext)` (`FileType.java:116-127`): ASCII case-insensitive suffix
/// match on Windows, exact byte suffix match on Unix. Uses byte slices so multibyte UTF-8 paths
/// never panic.
fn file_type_has_ext(path: &str, ext: &str, windows: bool) -> bool {
    let pb = path.as_bytes();
    let eb = ext.as_bytes();
    if pb.len() < eb.len() {
        return false;
    }
    if windows {
        pb[pb.len() - eb.len()..].eq_ignore_ascii_case(eb)
    } else {
        pb.ends_with(eb)
    }
}

fn file_type_has_any_ext(path: &str, exts: &[&str], windows: bool) -> bool {
    exts.iter().any(|ext| file_type_has_ext(path, ext, windows))
}

/// `CppCompileActionBuilder.getActionName`: determines the default action name for `source`
/// when no explicit `action_name` is supplied.
fn cc_default_compile_action(
    source: &str,
    fc: &FeatureConfiguration,
) -> Result<&'static str, StarlarkError> {
    cc_default_compile_action_for_os(source, fc, cfg!(windows))
}

fn cc_default_compile_action_for_os(
    source: &str,
    fc: &FeatureConfiguration,
    windows: bool,
) -> Result<&'static str, StarlarkError> {
    // `CppFileTypes.PIC_ASSEMBLER` (`CppFileTypes.java:136-146`):
    // `hasExtension(path, ".pic.s") && path.endsWith(".s")`.
    if file_type_has_ext(source, ".pic.s", windows) && source.ends_with(".s") {
        return err(format!(
            "Assembler source '{source}' should not have '.pic.s' extension"
        ));
    }
    // Exact evaluation order of `CppCompileActionBuilder.getActionName` (`CppCompileActionBuilder.java:244-276`)
    // against `CppFileTypes` (`CppFileTypes.java:30-264`):
    // - `CPP_MODULE_MAP`, `CPP_HEADER`, `OBJC_SOURCE`, `OBJCPP_SOURCE`, `.asm` in `ASSEMBLER`,
    //   `CLIF_INPUT_PROTO`, and `CPP_MODULE` use `FileType.of(...)` (`FileType.hasExtension`,
    //   case-insensitive on Windows and case-sensitive on Unix).
    // - `C_SOURCE` (`.c`), `CPP_SOURCE` (`.cc`, `.cpp`, `.cxx`, `.c++`, `.C`, `.cu`, `.cl`),
    //   `.s` in `ASSEMBLER`, and `ASSEMBLER_WITH_C_PREPROCESSOR` (`.S`) explicitly override
    //   `FileType.apply` in `CppFileTypes.java` with `path.endsWith(...)` so they remain
    //   case-sensitive even on Windows (verified by `CppFileTypesTest.testCaseSensitiveCFiles`
    //   and `CppFileTypesTest.testCaseSensitiveAssemblyFiles`).
    if file_type_has_ext(source, ".cppmap", windows) {
        Ok("c++-module-compile")
    } else if file_type_has_any_ext(
        source,
        &[
            ".h", ".hh", ".hpp", ".ipp", ".hxx", ".h++", ".inc", ".inl", ".tlh", ".tli", ".H",
            ".tcc",
        ],
        windows,
    ) {
        if fc.is_enabled("parse_headers") {
            Ok("c++-header-parsing")
        } else {
            err(format!(
                "Header file '{source}' cannot be compiled without 'parse_headers' feature enabled"
            ))
        }
    } else if source.ends_with(".c") {
        Ok("c-compile")
    } else if [".cc", ".cpp", ".cxx", ".c++", ".C", ".cu", ".cl"]
        .iter()
        .any(|ext| source.ends_with(ext))
    {
        Ok("c++-compile")
    } else if file_type_has_ext(source, ".m", windows) {
        Ok("objc-compile")
    } else if file_type_has_ext(source, ".mm", windows) {
        Ok("objc++-compile")
    } else if source.ends_with(".s") || file_type_has_ext(source, ".asm", windows) {
        Ok("assemble")
    } else if source.ends_with(".S") {
        Ok("preprocess-assemble")
    } else if file_type_has_ext(source, ".ipb", windows) {
        Ok("clif-match")
    } else if file_type_has_any_ext(source, &[".pcm", ".gcm", ".ifc"], windows) {
        Ok("c++-module-codegen")
    } else {
        err(format!("Unrecognized source file type for '{source}'"))
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
        .map_err(|e| StarlarkError(format!("invalid regular expression '{pat}': {e}")))?;
    let buckets = fc
        .per_feature_command_line(action_name, vars, &path_of)
        .map_err(|e| StarlarkError(e.0))?;
    let mut out = Vec::new();
    for (feat, flags) in buckets {
        if feat == "unfiltered_compile_flags" {
            out.extend(flags);
        } else {
            for f in flags {
                if !re.full_match(&f) {
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
    let param_file = if fc.is_enabled("compiler_param_file") {
        Some(crate::action::ParamFileInfo {
            flag_format: "@%s".to_string(),
            format: ParamFileFormat::GccQuoted,
            use_always: true,
            leading_args: 0,
            flag_groups: Vec::new(),
            explicit_path: Some(format!("{}.params", output_tree.exec_path.path_string())),
        })
    } else {
        None
    };
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
            arguments: vec![CommandLineArgs { args: argv, param_file }],
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
    let vars: Arc<Variables> = cc_kw(pos, kw, 2, "build_variables")
        .or_else(|| cc_kw(pos, kw, 3, "build_variables"))
        .or_else(|| cc_kw(pos, kw, 100, "compile_build_variables"))
        .and_then(cc_features::variables_of)
        .ok_or_else(|| StarlarkError("create_lto_backend_action: invalid build_variables".into()))?;
    let outputs = if let Some(outs_val) = cc_kw(pos, kw, 7, "outputs") {
        let outs = cc_artifacts(Some(outs_val));
        if outs.is_empty() {
            return err("create_lto_backend_action: outputs must not be empty");
        }
        outs
    } else {
        let object_file = cc_kw(pos, kw, 100, "object_file")
            .and_then(artifact_of)
            .ok_or_else(|| StarlarkError("create_lto_backend_action: object_file must be a File".into()))?;
        let mut outs = vec![object_file];
        if let Some(dwo) = cc_kw(pos, kw, 100, "dwo_file").and_then(artifact_of) {
            outs.push(dwo);
        }
        outs
    };
    let mut inputs = if let Some(ins_val) = cc_kw(pos, kw, 4, "inputs") {
        let mut ins = cc_artifacts(Some(ins_val));
        ins.extend(cc_artifacts(cc_kw(pos, kw, 5, "all_bitcode_files")));
        if let Some(imp) = cc_kw(pos, kw, 6, "imports").and_then(artifact_of) {
            ins.push(imp);
        }
        ins
    } else {
        let bitcode = cc_kw(pos, kw, 100, "bitcode_file")
            .and_then(artifact_of)
            .ok_or_else(|| StarlarkError("create_lto_backend_action: bitcode_file must be a File".into()))?;
        let mut ins = vec![bitcode];
        for k in ["index", "imports"] {
            if let Some(a) = cc_kw(pos, kw, 100, k).and_then(artifact_of) {
                ins.push(a);
            }
        }
        ins.extend(cc_artifacts(cc_kw(pos, kw, 100, "bitcode_inputs")));
        ins.extend(cc_artifacts(cc_kw(pos, kw, 100, "all_bitcode_files")));
        ins
    };
    let action_name = "lto-backend";
    if !fc.action_is_configured(action_name) {
        return err("Action lto-backend does not have an enabled configuration in the toolchain.");
    }
    let path_of = |v: &Value| artifact_of(v).map(|a| a.exec_path.path_string().to_string());
    let tool = fc.tool_path_for_action(action_name).map_err(|e| StarlarkError(e.0))?;
    let argv = fc.command_line(action_name, &vars, &path_of).map_err(|e| StarlarkError(e.0))?;
    let exec_reqs = cc_build_exec_reqs(&fc, action_name, None);
    let mut env = cc_build_compile_env(&fc, action_name, &vars, &exec_reqs)?;
    if let Some(Value::Dict(d)) = cc_kw(pos, kw, 8, "env") {
        for (k, v) in &d.lock().unwrap().entries {
            if let Some(ks) = k.as_str() {
                env.insert(ks.to_string(), v.as_str().unwrap_or("").to_string());
            }
        }
    }
    let mut tools = Vec::new();
    if let Some(tc) = cc_kw(pos, kw, 100, "cc_toolchain") {
        for f in ["_compiler_files", "compiler_files", "_all_files", "all_files"] {
            tools.extend(cc_artifacts(field(tc, f).as_ref()));
        }
    }
    let primary_out = outputs[0].exec_path.path_string().to_string();
    let mut a = Action::new(
        st.label.clone(),
        "CcLtoBackendCompile",
        ActionKind::Spawn {
            executable: tool,
            arguments: vec![CommandLineArgs { args: argv, param_file: None }],
            shell_command: None,
        },
    );
    a.progress_message = Some(format!("LTO Backend Compile {primary_out}"));
    a.env = env;
    a.execution_requirements = exec_reqs;
    inputs.extend(tools.iter().cloned());
    a.inputs = dedup(inputs);
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
    let param_format = match cc_kw(pos, kw, 3, "parameter_file_type") {
        Some(Value::String(s)) => Some(match s.as_str() {
            "UNQUOTED" => ParamFileFormat::Multiline,
            "GCC_QUOTED" => ParamFileFormat::GccQuoted,
            "SHELL_QUOTED" => ParamFileFormat::Shell,
            "WINDOWS" => ParamFileFormat::Windows,
            other => {
                return err(format!(
                    "No enum constant ParameterFile.ParameterFileType.{other}"
                ))
            }
        }),
        Some(other) => {
            return err(format!(
                "get_link_args: parameter_file_type must be string or None, got {}",
                other.type_name()
            ))
        }
        None => None,
    };
    let mut argv = fc
        .command_line(&action_name, &vars, &path_of)
        .map_err(|e| StarlarkError(e.0))?;
    let mut builder = ArgsBuilder::default();
    if let Some(format) = param_format {
        if let Some(p) = argv
            .iter()
            .find(|s| s.contains("LINKER_PARAM_FILE_PLACEHOLDER"))
        {
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
    if let Some(linker_param_file) = vars
        .get_string_if_available("linker_param_file", &path_of)
        .map_err(|e| StarlarkError(e.0))?
    {
        argv.retain(|s| !s.contains(&linker_param_file));
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
    use zel_builtins::cc_features::ToolchainFeatures;
    use zel_starlark::StructValue;

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

    fn test_struct(fields: Vec<(&str, Value)>) -> Value {
        Value::Struct(Arc::new(StructValue::new(
            fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )))
    }

    #[test]
    fn copts_filter_drops_full_matches_keeps_others_and_exempts_unfiltered_compile_flags() {
        let opt_fg = test_struct(vec![(
            "flags",
            mk_list(vec![
                Value::String("-Wall".into()),
                Value::String("-O2".into()),
                Value::String("-Werror=return-type".into()),
                Value::String("-fno-exceptions".into()),
            ]),
        )]);
        let opt_fs = test_struct(vec![
            ("actions", mk_list(vec![Value::String("c-compile".into())])),
            ("flag_groups", mk_list(vec![opt_fg])),
        ]);
        let opt_feat = test_struct(vec![
            ("name", Value::String("opt_flags".into())),
            ("enabled", Value::Bool(true)),
            ("flag_sets", mk_list(vec![opt_fs])),
        ]);

        let unfilt_fg = test_struct(vec![(
            "flags",
            mk_list(vec![
                Value::String("-O2".into()),
                Value::String("--sysroot=/sdk".into()),
            ]),
        )]);
        let unfilt_fs = test_struct(vec![
            ("actions", mk_list(vec![Value::String("c-compile".into())])),
            ("flag_groups", mk_list(vec![unfilt_fg])),
        ]);
        let unfilt_feat = test_struct(vec![
            ("name", Value::String("unfiltered_compile_flags".into())),
            ("enabled", Value::Bool(true)),
            ("flag_sets", mk_list(vec![unfilt_fs])),
        ]);

        let tf = Arc::new(
            ToolchainFeatures::new(&[opt_feat, unfilt_feat], &[], &[], "", &|_| None).unwrap(),
        );
        let fc = tf
            .configure(&tf.default_features_and_action_configs())
            .unwrap();
        let vars = cc_features::variables_of(&cc_features::make_variables(None, BTreeMap::new()))
            .unwrap();

        assert_eq!(
            cc_filtered_compile_argv(&fc, "c-compile", &vars, None).unwrap(),
            vec![
                "-Wall",
                "-O2",
                "-Werror=return-type",
                "-fno-exceptions",
                "-O2",
                "--sysroot=/sdk",
            ]
        );
        assert_eq!(
            cc_filtered_compile_argv(&fc, "c-compile", &vars, Some("")).unwrap(),
            vec![
                "-Wall",
                "-O2",
                "-Werror=return-type",
                "-fno-exceptions",
                "-O2",
                "--sysroot=/sdk",
            ]
        );

        // `CoptsFilter.passesFilter` drops flags matching the whole regex (`!Matcher.matches()`)
        // while `CompileCommandLine` preserves `unfiltered_compile_flags`.
        assert_eq!(
            cc_filtered_compile_argv(&fc, "c-compile", &vars, Some("-O[0-9]|-fno-exceptions"))
                .unwrap(),
            vec!["-Wall", "-Werror=return-type", "-O2", "--sysroot=/sdk"]
        );

        // Substring-only pattern does not drop `-Werror=return-type`; full-string pattern does.
        assert_eq!(
            cc_filtered_compile_argv(&fc, "c-compile", &vars, Some("-Werror")).unwrap(),
            vec![
                "-Wall",
                "-O2",
                "-Werror=return-type",
                "-fno-exceptions",
                "-O2",
                "--sysroot=/sdk",
            ]
        );
        assert_eq!(
            cc_filtered_compile_argv(&fc, "c-compile", &vars, Some("-Werror.*")).unwrap(),
            vec!["-Wall", "-O2", "-fno-exceptions", "-O2", "--sysroot=/sdk"]
        );

        let bad = cc_filtered_compile_argv(&fc, "c-compile", &vars, Some("[unclosed")).unwrap_err();
        assert!(
            bad.0.contains("invalid regular expression '[unclosed'"),
            "{}",
            bad.0
        );
    }

    #[test]
    fn get_link_args_matches_link_command_line_param_file_and_variable_filtering() {
        let base_fg = test_struct(vec![(
            "flags",
            mk_list(vec![
                Value::String("-o".into()),
                Value::String("out/app".into()),
                Value::String("-lm".into()),
            ]),
        )]);
        let param_fg = test_struct(vec![
            (
                "expand_if_available",
                Value::String("linker_param_file".into()),
            ),
            (
                "flags",
                mk_list(vec![Value::String("-Wl,@%{linker_param_file}%%1".into())]),
            ),
        ]);
        let link_fs = test_struct(vec![
            (
                "actions",
                mk_list(vec![Value::String("c++-link-executable".into())]),
            ),
            ("flag_groups", mk_list(vec![base_fg.clone(), param_fg])),
        ]);
        let link_feat = test_struct(vec![
            ("name", Value::String("link_flags".into())),
            ("enabled", Value::Bool(true)),
            ("flag_sets", mk_list(vec![link_fs])),
        ]);
        let tf = Arc::new(ToolchainFeatures::new(&[link_feat], &[], &[], "", &|_| None).unwrap());
        let fc = tf
            .configure(&tf.default_features_and_action_configs())
            .unwrap();
        let fc_val = cc_features::register(cc_features::KIND_FEATURE_CONFIGURATION, fc);

        // 1. Standard rules_cc link: `linker_param_file = "LINKER_PARAM_FILE_PLACEHOLDER"` + `GCC_QUOTED`.
        let vars_placeholder = cc_features::make_variables(
            None,
            BTreeMap::from([(
                "linker_param_file".to_string(),
                Value::String("LINKER_PARAM_FILE_PLACEHOLDER".into()),
            )]),
        );
        let res = cc_get_link_args(
            &[],
            &BTreeMap::from([
                (
                    "action_name".to_string(),
                    Value::String("c++-link-executable".into()),
                ),
                ("feature_configuration".to_string(), fc_val.clone()),
                ("build_variables".to_string(), vars_placeholder.clone()),
                (
                    "parameter_file_type".to_string(),
                    Value::String("GCC_QUOTED".into()),
                ),
            ]),
        )
        .unwrap();
        let built = args_of(&res).unwrap();
        let built = built.lock().unwrap();
        assert_eq!(built.args, vec!["-o", "out/app", "-lm"]);
        let pf = built.param_file.as_ref().expect("expected ParamFileInfo");
        assert_eq!(pf.flag_format, "-Wl,@%s%%1");
        assert_eq!(pf.format, ParamFileFormat::GccQuoted);
        assert!(pf.use_always);
        drop(built);

        // 2. Toolchain without a param-file feature: `getParamFileInfo()` returns `None` and
        //    remaining link flags stay inline on `args`.
        let no_pf_fs = test_struct(vec![
            (
                "actions",
                mk_list(vec![Value::String("c++-link-executable".into())]),
            ),
            ("flag_groups", mk_list(vec![base_fg])),
        ]);
        let no_pf_feat = test_struct(vec![
            ("name", Value::String("link_flags_no_pf".into())),
            ("enabled", Value::Bool(true)),
            ("flag_sets", mk_list(vec![no_pf_fs])),
        ]);
        let no_pf_tf =
            Arc::new(ToolchainFeatures::new(&[no_pf_feat], &[], &[], "", &|_| None).unwrap());
        let no_pf_fc = no_pf_tf
            .configure(&no_pf_tf.default_features_and_action_configs())
            .unwrap();
        let no_pf_fc_val = cc_features::register(cc_features::KIND_FEATURE_CONFIGURATION, no_pf_fc);
        let res_no_pf = cc_get_link_args(
            &[],
            &BTreeMap::from([
                (
                    "action_name".to_string(),
                    Value::String("c++-link-executable".into()),
                ),
                ("feature_configuration".to_string(), no_pf_fc_val),
                ("build_variables".to_string(), vars_placeholder.clone()),
                (
                    "parameter_file_type".to_string(),
                    Value::String("GCC_QUOTED".into()),
                ),
            ]),
        )
        .unwrap();
        let built_no_pf = args_of(&res_no_pf).unwrap();
        let built_no_pf = built_no_pf.lock().unwrap();
        assert!(built_no_pf.param_file.is_none());
        assert_eq!(built_no_pf.args, vec!["-o", "out/app", "-lm"]);
        drop(built_no_pf);

        // 3. When `linker_param_file` is not available in `build_variables` (including when
        //    overridden with `None` in an extension), `getParamCommandLine` does NOT strip a
        //    literal `"LINKER_PARAM_FILE_PLACEHOLDER"` flag.
        let lit_fg = test_struct(vec![(
            "flags",
            mk_list(vec![
                Value::String("-o".into()),
                Value::String("@LINKER_PARAM_FILE_PLACEHOLDER".into()),
            ]),
        )]);
        let lit_fs = test_struct(vec![
            (
                "actions",
                mk_list(vec![Value::String("c++-link-executable".into())]),
            ),
            ("flag_groups", mk_list(vec![lit_fg])),
        ]);
        let lit_feat = test_struct(vec![
            ("name", Value::String("literal_pf".into())),
            ("enabled", Value::Bool(true)),
            ("flag_sets", mk_list(vec![lit_fs])),
        ]);
        let lit_tf =
            Arc::new(ToolchainFeatures::new(&[lit_feat], &[], &[], "", &|_| None).unwrap());
        let lit_fc = lit_tf
            .configure(&lit_tf.default_features_and_action_configs())
            .unwrap();
        let lit_fc_val = cc_features::register(cc_features::KIND_FEATURE_CONFIGURATION, lit_fc);
        let parent_vars = cc_features::variables_of(&vars_placeholder).unwrap();
        let cleared_vars = cc_features::make_variables(
            Some(parent_vars),
            BTreeMap::from([("linker_param_file".to_string(), Value::None)]),
        );
        let res_lit = cc_get_link_args(
            &[],
            &BTreeMap::from([
                (
                    "action_name".to_string(),
                    Value::String("c++-link-executable".into()),
                ),
                ("feature_configuration".to_string(), lit_fc_val.clone()),
                ("build_variables".to_string(), cleared_vars),
                (
                    "parameter_file_type".to_string(),
                    Value::String("UNQUOTED".into()),
                ),
            ]),
        )
        .unwrap();
        let built_lit = args_of(&res_lit).unwrap();
        let built_lit = built_lit.lock().unwrap();
        assert_eq!(
            built_lit.args,
            vec!["-o", "@LINKER_PARAM_FILE_PLACEHOLDER"]
        );
        assert_eq!(
            built_lit.param_file.as_ref().map(|p| p.flag_format.as_str()),
            Some("@%s")
        );
        drop(built_lit);

        // 4. Non-scalar `linker_param_file` fails expansion just like `getStringValue`.
        let seq_vars = cc_features::make_variables(
            None,
            BTreeMap::from([(
                "linker_param_file".to_string(),
                mk_list(vec![Value::String("a".into())]),
            )]),
        );
        let seq_err = cc_get_link_args(
            &[],
            &BTreeMap::from([
                (
                    "action_name".to_string(),
                    Value::String("c++-link-executable".into()),
                ),
                ("feature_configuration".to_string(), fc_val),
                ("build_variables".to_string(), seq_vars),
            ]),
        )
        .unwrap_err();
        assert!(
            seq_err
                .0
                .contains("Cannot expand variable 'linker_param_file': expected string"),
            "{}",
            seq_err.0
        );
    }

    struct EmptyDepResolver;

    impl DepResolver for EmptyDepResolver {
        fn configured_target(
            &self,
            _label: &Label,
            _cfg: &BuildConfigurationKey,
        ) -> Result<Arc<ConfiguredTarget>, String> {
            Err("unexpected dependency lookup".into())
        }
    }

    fn cc_test_ctx_state(label: &Label) -> Arc<CtxState> {
        let rule = RuleInstance {
            label: label.clone(),
            rule_class: "cc_library".into(),
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
            inputs: AnalysisInputs::new(Arc::new(EmptyDepResolver)),
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
    fn compile_action_template_honors_compiler_param_file_and_lto_backends_stay_inline() {
        let label = Label::parse_canonical("//pkg:tree_cc").unwrap();
        let tool = test_struct(vec![("path", Value::String("/usr/bin/gcc".into()))]);
        let compile_ac = test_struct(vec![
            ("action_name", Value::String("c++-compile".into())),
            ("enabled", Value::Bool(true)),
            ("tools", mk_list(vec![tool.clone()])),
        ]);
        let lto_ac = test_struct(vec![
            ("action_name", Value::String("lto-backend".into())),
            ("enabled", Value::Bool(true)),
            ("tools", mk_list(vec![tool])),
        ]);
        let compile_fg = test_struct(vec![(
            "flags",
            mk_list(vec![Value::String("-O2".into()), Value::String("-Wall".into())]),
        )]);
        let compile_fs = test_struct(vec![
            (
                "actions",
                mk_list(vec![
                    Value::String("c++-compile".into()),
                    Value::String("lto-backend".into()),
                ]),
            ),
            ("flag_groups", mk_list(vec![compile_fg])),
        ]);
        let flags_feat = test_struct(vec![
            ("name", Value::String("compile_flags".into())),
            ("enabled", Value::Bool(true)),
            ("flag_sets", mk_list(vec![compile_fs])),
        ]);
        let pf_feat = test_struct(vec![
            ("name", Value::String("compiler_param_file".into())),
            ("enabled", Value::Bool(false)),
        ]);

        let tf = Arc::new(
            ToolchainFeatures::new(
                &[flags_feat, pf_feat],
                &[compile_ac, lto_ac],
                &[],
                "",
                &|_| None,
            )
            .unwrap(),
        );
        let mut with_pf = tf.default_features_and_action_configs();
        with_pf.push("compiler_param_file".into());
        let fc_pf = cc_features::register(
            cc_features::KIND_FEATURE_CONFIGURATION,
            tf.configure(&with_pf).unwrap(),
        );
        let fc_no_pf = cc_features::register(
            cc_features::KIND_FEATURE_CONFIGURATION,
            tf.configure(&tf.default_features_and_action_configs()).unwrap(),
        );
        let vars = cc_features::make_variables(None, BTreeMap::new());

        let mut src_tree = Artifact::derived(
            "k8-fastbuild",
            PathFragment::create("pkg/gen_srcs"),
            label.clone(),
        );
        src_tree.is_tree_artifact = true;
        let mut out_tree_pf = Artifact::derived(
            "k8-fastbuild",
            PathFragment::create("pkg/gen_objs_pf"),
            label.clone(),
        );
        out_tree_pf.is_tree_artifact = true;
        let mut out_tree_inline = Artifact::derived(
            "k8-fastbuild",
            PathFragment::create("pkg/gen_objs_inline"),
            label.clone(),
        );
        out_tree_inline.is_tree_artifact = true;
        let mut lto_out_tree = Artifact::derived(
            "k8-fastbuild",
            PathFragment::create("pkg/lto_objs"),
            label.clone(),
        );
        lto_out_tree.is_tree_artifact = true;

        // 1. `CppCompileActionTemplate` with `compiler_param_file` enabled emits `<output>.params`.
        let st = cc_test_ctx_state(&label);
        cc_create_compile_action_template(
            &st,
            &[],
            &BTreeMap::from([
                ("feature_configuration".to_string(), fc_pf.clone()),
                ("compile_build_variables".to_string(), vars.clone()),
                ("source".to_string(), file_value(&src_tree)),
                ("output_files".to_string(), file_value(&out_tree_pf)),
                ("action_name".to_string(), Value::String("c++-compile".into())),
            ]),
        )
        .unwrap();

        // 2. `CppCompileActionTemplate` without `compiler_param_file` keeps flags inline.
        cc_create_compile_action_template(
            &st,
            &[],
            &BTreeMap::from([
                ("feature_configuration".to_string(), fc_no_pf),
                ("compile_build_variables".to_string(), vars.clone()),
                ("source".to_string(), file_value(&src_tree)),
                ("output_files".to_string(), file_value(&out_tree_inline)),
                ("action_name".to_string(), Value::String("c++-compile".into())),
            ]),
        )
        .unwrap();

        // 3. `LtoBackendArtifacts.getLtoBackendCommandLine` in Bazel 9.3.2 never uses `compiler_param_file`.
        let bc_file = Artifact::derived(
            "k8-fastbuild",
            PathFragment::create("pkg/a.bc"),
            label.clone(),
        );
        let obj_file = Artifact::derived(
            "k8-fastbuild",
            PathFragment::create("pkg/a.o"),
            label.clone(),
        );
        cc_create_lto_backend_action(
            &st,
            &[],
            &BTreeMap::from([
                ("feature_configuration".to_string(), fc_pf.clone()),
                ("build_variables".to_string(), vars.clone()),
                ("bitcode_file".to_string(), file_value(&bc_file)),
                ("object_file".to_string(), file_value(&obj_file)),
            ]),
        )
        .unwrap();

        let mut bc_tree = Artifact::derived(
            "k8-fastbuild",
            PathFragment::create("pkg/bc_tree"),
            label.clone(),
        );
        bc_tree.is_tree_artifact = true;
        cc_create_lto_backend_action_template(
            &st,
            &[],
            &BTreeMap::from([
                ("feature_configuration".to_string(), fc_pf),
                ("build_variables".to_string(), vars),
                ("bitcode_file".to_string(), file_value(&bc_tree)),
                ("object_file".to_string(), file_value(&lto_out_tree)),
            ]),
        )
        .unwrap();

        let actions = st.actions.lock().unwrap();
        assert_eq!(actions.len(), 4);

        match &actions[0].kind {
            ActionKind::Spawn { arguments, .. } => {
                let pf = arguments[0].param_file.as_ref().expect("expected param_file");
                assert_eq!(pf.format, ParamFileFormat::GccQuoted);
                assert!(pf.use_always);
                assert_eq!(
                    pf.explicit_path.as_deref(),
                    Some("bazel-out/k8-fastbuild/bin/pkg/gen_objs_pf.params")
                );
            }
            other => panic!("expected Spawn, got {other:?}"),
        }
        match &actions[1].kind {
            ActionKind::Spawn { arguments, .. } => {
                assert!(arguments[0].param_file.is_none());
            }
            other => panic!("expected Spawn, got {other:?}"),
        }
        match &actions[2].kind {
            ActionKind::Spawn { arguments, .. } => {
                assert!(arguments[0].param_file.is_none());
            }
            other => panic!("expected Spawn, got {other:?}"),
        }
        match &actions[3].kind {
            ActionKind::Spawn { arguments, .. } => {
                assert!(arguments[0].param_file.is_none());
            }
            other => panic!("expected Spawn, got {other:?}"),
        }
    }

    #[test]
    fn declared_rule_fragments_surface_validation_errors_during_analysis() {
        let mut ectx = EvalContext::default();
        ectx.eval_str("def _impl(ctx):\n    return []\n").unwrap();
        let impl_fn = ectx.globals.get("_impl").cloned().unwrap();
        let spec = Arc::new(StarlarkRuleSpec {
            implementation: impl_fn,
            kwargs: BTreeMap::from([(
                "fragments".to_string(),
                mk_list(vec![Value::String("cpp".into())]),
            )]),
            defined_in: "//pkg:defs.bzl".into(),
            definition_digest: String::new(),
            definition_stack: Vec::new(),
            initializers: Vec::new(),
            build_setting_flag: false,
        });
        let rc = Arc::new(zel_packages::RuleClass {
            name: "my_cpp_rule".into(),
            attrs: BTreeMap::new(),
            attr_order: Vec::new(),
            implicit: BTreeMap::new(),
            is_test: false,
            is_executable: false,
            open: false,
            implicit_outputs: Vec::new(),
            outputs_fn: None,
            is_starlark: true,
            build_setting: false,
            build_setting_is_flag: false,
            starlark: Some(spec),
        });
        let label = Label::parse_canonical("//pkg:t").unwrap();
        let rule = RuleInstance {
            label,
            rule_class: "my_cpp_rule".into(),
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
            extra: zel_packages::RuleExtra {
                rule_class: Some(rc),
                ..Default::default()
            },
        };
        let mut bad_cfg = BuildConfigurationKey::default();
        bad_cfg.native.insert(
            "fdo_instrument".into(),
            crate::NativeSetting {
                raw: "prof".into(),
                value: crate::build_options::ConfigValue::Scalar("prof".into()),
            },
        );
        let err = analyze_starlark_rule(
            &rule,
            &bad_cfg,
            AnalysisInputs::new(Arc::new(EmptyDepResolver)),
        )
        .unwrap_err();
        assert!(
            err.contains("Cannot instrument FDO without --copt=-Wno-error"),
            "{err}"
        );
    }

    #[test]
    fn per_file_opts_escaped_comma_filters_and_ipp_header_parsing_parity() {
        let label = Label::parse_canonical("//pkg:lib").unwrap();

        // 1. `BuildOptions`-converted `PerLabelOptions.toString()` entries (even when `raw` is
        // empty) and escaped commas `\,` in both inclusion and exclusion regex filters.
        let (_, pfc_opt) = crate::build_options::find_option("per_file_copt").unwrap();
        let (_, lto_opt) = crate::build_options::find_option("per_file_ltobackendopt").unwrap();
        let (_, host_pfc_opt) = crate::build_options::find_option("host_per_file_copt").unwrap();

        let pfc_raw = vec!["pkg/.*\\.cc,-pkg/skip\\,me\\.cc@-DFROM_BO=1,-DPAIR=x\\,y".to_string()];
        let pfc_converted = crate::build_options::convert_raws(
            pfc_opt,
            &pfc_raw,
            &crate::build_options::canonical_label_default,
            "k8",
        )
        .unwrap();
        let lto_raw = vec![
            "pkg/.*a\\,1\\.o,-pkg/skip\\.o@-O3,-mllvm".to_string(),
            "pkg/extra\\.o@-fno-lto".to_string(),
        ];
        let lto_converted = crate::build_options::convert_raws(
            lto_opt,
            &lto_raw,
            &crate::build_options::canonical_label_default,
            "k8",
        )
        .unwrap();
        let host_pfc_raw = vec!["pkg/tool\\.cc@-DHOST_PFC=1".to_string()];
        let host_pfc_converted = crate::build_options::convert_raws(
            host_pfc_opt,
            &host_pfc_raw,
            &crate::build_options::canonical_label_default,
            "k8",
        )
        .unwrap();

        // Test converted `BuildOptions` values directly (with `raw: ""`) to prove `PerLabelOptions`
        // `toString()` entries are parsed and applied rather than dropped.
        let mut bo_only_cfg = BuildConfigurationKey::target("k8", "fastbuild");
        bo_only_cfg.native.insert(
            "per_file_copt".into(),
            crate::NativeSetting {
                raw: String::new(),
                value: pfc_converted.clone(),
            },
        );
        bo_only_cfg.native.insert(
            "per_file_ltobackendopt".into(),
            crate::NativeSetting {
                raw: String::new(),
                value: lto_converted.clone(),
            },
        );
        bo_only_cfg.native.insert(
            "host_per_file_copt".into(),
            crate::NativeSetting {
                raw: String::new(),
                value: host_pfc_converted,
            },
        );

        let mut st = Arc::try_unwrap(cc_test_ctx_state(&label)).ok().unwrap();
        st.cfg = bo_only_cfg.clone();
        let st = Arc::new(st);
        let mut ectx = EvalContext::default();
        let cpp_frag = Value::None;

        let match_src = Artifact::source(PathFragment::create("pkg/keep,ok.cc"), label.clone());
        let skip_src = Artifact::source(PathFragment::create("pkg/skip,me.cc"), label.clone());
        let res_match = cc_native(
            &st,
            &mut ectx,
            "cc_internal.per_file_copts",
            &[cpp_frag.clone(), file_value(&match_src), Value::None],
            &BTreeMap::new(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            res_match,
            mk_list(vec![
                Value::String("-DFROM_BO=1".into()),
                Value::String("-DPAIR=x,y".into()),
            ])
        );
        let res_skip = cc_native(
            &st,
            &mut ectx,
            "cc_internal.per_file_copts",
            &[cpp_frag.clone(), file_value(&skip_src), Value::None],
            &BTreeMap::new(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(res_skip, mk_list(vec![]));

        let obj_match = Artifact::derived(
            "k8-fastbuild",
            PathFragment::create("pkg/_objs/lib/a,1.o"),
            label.clone(),
        );
        let obj_skip = Artifact::derived(
            "k8-fastbuild",
            PathFragment::create("pkg/_objs/lib/skip.o"),
            label.clone(),
        );
        let lto_match = cc_native(
            &st,
            &mut ectx,
            "cc_internal.collect_per_file_lto_backend_opts",
            &[cpp_frag.clone(), file_value(&obj_match)],
            &BTreeMap::new(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            lto_match,
            mk_list(vec![
                Value::String("-O3".into()),
                Value::String("-mllvm".into()),
            ])
        );
        let lto_skip = cc_native(
            &st,
            &mut ectx,
            "cc_internal.collect_per_file_lto_backend_opts",
            &[],
            &BTreeMap::from([
                ("cpp_config".to_string(), cpp_frag.clone()),
                ("obj".to_string(), file_value(&obj_skip)),
            ]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(lto_skip, mk_list(vec![]));

        // Exec config uses `host_per_file_copt` rather than target `per_file_copt`.
        let mut exec_st = Arc::try_unwrap(cc_test_ctx_state(&label)).ok().unwrap();
        exec_st.cfg = bo_only_cfg.to_exec();
        let exec_st = Arc::new(exec_st);
        let tool_src = Artifact::source(PathFragment::create("pkg/tool.cc"), label.clone());
        let exec_res = cc_native(
            &exec_st,
            &mut ectx,
            "cc_internal.per_file_copts",
            &[cpp_frag, file_value(&tool_src), Value::None],
            &BTreeMap::new(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(exec_res, mk_list(vec![Value::String("-DHOST_PFC=1".into())]));

        // 2. Multiple CLI `NativeSetting` entries (including when `cfg.options` holds either
        // `\x1f`-joined or legacy `,`-joined mirrors), Starlark transitions reading & appending
        // entries, and `to_exec()` isolating target `per_file_copt` / `per_file_ltobackendopt`
        // while preserving multiple `host_per_file_copt` entries.
        let (_, pfc_opt) = crate::build_options::find_option("per_file_copt").unwrap();
        let (_, hpfc_opt) = crate::build_options::find_option("host_per_file_copt").unwrap();
        let (_, pflto_opt) = crate::build_options::find_option("per_file_ltobackendopt").unwrap();
        let multi_pfc = vec![
            r"pkg/.*\.cc,-pkg/skip\,me\.cc@-O2,-DFOO=a\,b".to_string(),
            r"pkg/keep\,ok\.cc@-DSECOND_PFC=1".to_string(),
        ];
        let multi_hpfc = vec![
            r"pkg/tool.*\.cc,-pkg/tool_skip\.cc@-DHOST_FIRST=1".to_string(),
            r"pkg/tool\.cc@-DHOST_SECOND=a\,b".to_string(),
        ];
        let multi_lto = vec![
            r".*a\,1.*\.o,-.*skip\.o@-O3".to_string(),
            r".*a\,1.*\.o@-mllvm".to_string(),
        ];
        let multi_pfc_conv = crate::build_options::convert_raws(
            pfc_opt,
            &multi_pfc,
            &crate::build_options::canonical_label_default,
            "k8",
        )
        .unwrap();
        let multi_hpfc_conv = crate::build_options::convert_raws(
            hpfc_opt,
            &multi_hpfc,
            &crate::build_options::canonical_label_default,
            "k8",
        )
        .unwrap();
        let multi_lto_conv = crate::build_options::convert_raws(
            pflto_opt,
            &multi_lto,
            &crate::build_options::canonical_label_default,
            "k8",
        )
        .unwrap();

        for comma_mirrored in [false, true] {
            let mut cli_cfg = BuildConfigurationKey::target("k8", "fastbuild");
            cli_cfg.native.insert(
                "per_file_copt".into(),
                crate::NativeSetting {
                    raw: crate::build_options::encode_unsplit_list(&multi_pfc),
                    value: multi_pfc_conv.clone(),
                },
            );
            cli_cfg.native.insert(
                "host_per_file_copt".into(),
                crate::NativeSetting {
                    raw: crate::build_options::encode_unsplit_list(&multi_hpfc),
                    value: multi_hpfc_conv.clone(),
                },
            );
            cli_cfg.native.insert(
                "per_file_ltobackendopt".into(),
                crate::NativeSetting {
                    raw: crate::build_options::encode_unsplit_list(&multi_lto),
                    value: multi_lto_conv.clone(),
                },
            );
            if comma_mirrored {
                cli_cfg
                    .options
                    .insert("per_file_copt".into(), multi_pfc.join(","));
                cli_cfg
                    .options
                    .insert("host_per_file_copt".into(), multi_hpfc.join(","));
                cli_cfg
                    .options
                    .insert("per_file_ltobackendopt".into(), multi_lto.join(","));
            } else {
                cli_cfg.options.insert(
                    "per_file_copt".into(),
                    crate::build_options::encode_unsplit_list(&multi_pfc),
                );
                cli_cfg.options.insert(
                    "host_per_file_copt".into(),
                    crate::build_options::encode_unsplit_list(&multi_hpfc),
                );
                cli_cfg.options.insert(
                    "per_file_ltobackendopt".into(),
                    crate::build_options::encode_unsplit_list(&multi_lto),
                );
            }
            assert!(!cli_cfg.has_options_override("per_file_copt"));
            assert!(!cli_cfg.has_options_override("host_per_file_copt"));
            assert!(!cli_cfg.has_options_override("per_file_ltobackendopt"));
            assert_eq!(
                crate::transition::setting_value(&cli_cfg, "//command_line_option:per_file_copt"),
                mk_list(vec![
                    Value::String(multi_pfc[0].clone()),
                    Value::String(multi_pfc[1].clone()),
                ])
            );
            assert_eq!(
                crate::transition::setting_value(
                    &cli_cfg,
                    "//command_line_option:per_file_ltobackendopt"
                ),
                mk_list(vec![
                    Value::String(multi_lto[0].clone()),
                    Value::String(multi_lto[1].clone()),
                ])
            );

            let mut cli_st = Arc::try_unwrap(cc_test_ctx_state(&label)).ok().unwrap();
            cli_st.cfg = cli_cfg.clone();
            let cli_st = Arc::new(cli_st);
            let cpp_frag_cli = Value::Host("fragment".into(), "cpp".into());
            let cli_copt = cc_native(
                &cli_st,
                &mut ectx,
                "cc_internal.per_file_copts",
                &[cpp_frag_cli.clone(), file_value(&match_src), Value::None],
                &BTreeMap::new(),
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                cli_copt,
                mk_list(vec![
                    Value::String("-O2".into()),
                    Value::String("-DFOO=a,b".into()),
                    Value::String("-DSECOND_PFC=1".into()),
                ])
            );
            let cli_lto = cc_native(
                &cli_st,
                &mut ectx,
                "cc_internal.collect_per_file_lto_backend_opts",
                &[cpp_frag_cli.clone(), file_value(&obj_match)],
                &BTreeMap::new(),
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                cli_lto,
                mk_list(vec![
                    Value::String("-O3".into()),
                    Value::String("-mllvm".into()),
                ])
            );

            // Starlark transition appends a 3rd target `per_file_copt`; `to_exec()` must NOT leak
            // target `per_file_copt` or `per_file_ltobackendopt`, and must apply both
            // `host_per_file_copt` entries.
            let mut tr_cfg = cli_cfg.clone();
            crate::transition::set_setting(
                &mut tr_cfg,
                "//command_line_option:per_file_copt",
                &mk_list(vec![
                    Value::String(multi_pfc[0].clone()),
                    Value::String(multi_pfc[1].clone()),
                    Value::String(r"pkg/tool\.cc@-DLEAKED_TARGET=1".into()),
                ]),
            )
            .unwrap();
            let exec_from_tr = tr_cfg.to_exec();
            assert_eq!(
                crate::transition::setting_value(
                    &exec_from_tr,
                    "//command_line_option:per_file_copt"
                ),
                mk_list(vec![
                    Value::String(multi_hpfc[0].clone()),
                    Value::String(multi_hpfc[1].clone()),
                ])
            );
            assert_eq!(
                crate::transition::setting_value(
                    &exec_from_tr,
                    "//command_line_option:per_file_ltobackendopt"
                ),
                mk_list(vec![])
            );
            let mut exec_tr_st = Arc::try_unwrap(cc_test_ctx_state(&label)).ok().unwrap();
            exec_tr_st.cfg = exec_from_tr;
            let exec_tr_st = Arc::new(exec_tr_st);
            let exec_copt = cc_native(
                &exec_tr_st,
                &mut ectx,
                "cc_internal.per_file_copts",
                &[cpp_frag_cli.clone(), file_value(&tool_src), Value::None],
                &BTreeMap::new(),
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                exec_copt,
                mk_list(vec![
                    Value::String("-DHOST_FIRST=1".into()),
                    Value::String("-DHOST_SECOND=a,b".into()),
                ])
            );
            let exec_lto = cc_native(
                &exec_tr_st,
                &mut ectx,
                "cc_internal.collect_per_file_lto_backend_opts",
                &[cpp_frag_cli, file_value(&obj_match)],
                &BTreeMap::new(),
            )
            .unwrap()
            .unwrap();
            assert_eq!(exec_lto, mk_list(vec![]));
        }

        // 3. All 12 `CppFileTypes.CPP_HEADER` extensions (including `.ipp`) map to
        // `c++-header-parsing` when `parse_headers` is enabled and fail cleanly when disabled,
        // and multibyte UTF-8 filenames never panic on Windows `.cppmap` suffix matching.
        let tool = test_struct(vec![("path", Value::String("/usr/bin/g++".into()))]);
        let hdr_ac = test_struct(vec![
            ("action_name", Value::String("c++-header-parsing".into())),
            ("enabled", Value::Bool(true)),
            ("tools", mk_list(vec![tool])),
        ]);
        let ph_feat = test_struct(vec![
            ("name", Value::String("parse_headers".into())),
            ("enabled", Value::Bool(false)),
        ]);
        let tf = Arc::new(ToolchainFeatures::new(&[ph_feat], &[hdr_ac], &[], "", &|_| None).unwrap());
        let fc_disabled = tf.configure(&tf.default_features_and_action_configs()).unwrap();
        let mut with_ph = tf.default_features_and_action_configs();
        with_ph.push("parse_headers".into());
        let fc_enabled = tf.configure(&with_ph).unwrap();

        for ext in [
            "h", "hh", "hpp", "ipp", "hxx", "h++", "inc", "inl", "tlh", "tli", "H", "tcc",
        ] {
            let hdr_path = format!("pkg/tmpl.{ext}");
            assert_eq!(
                cc_default_compile_action(&hdr_path, &fc_enabled).unwrap(),
                "c++-header-parsing",
                "extension .{ext} should use c++-header-parsing"
            );
            let disabled_err = cc_default_compile_action(&hdr_path, &fc_disabled).unwrap_err();
            assert!(
                disabled_err
                    .0
                    .contains("cannot be compiled without 'parse_headers' feature enabled"),
                "{disabled_err:?}"
            );
        }

        // Multibyte UTF-8 paths where `source.len() - 7` falls mid-codepoint (2-, 3-, and 4-byte
        // UTF-8 sequences) must never panic under Windows case-insensitive `.cppmap` detection.
        for (utf8_src, expected) in [
            ("aéé.cc", "c++-compile"),
            ("ééé.cc", "c++-compile"),
            ("日本語.h", "c++-header-parsing"),
            ("🦀🦀.cpp", "c++-compile"),
            ("модуль.cppmap", "c++-module-compile"),
        ] {
            for win in [false, true] {
                assert_eq!(
                    cc_default_compile_action_for_os(utf8_src, &fc_enabled, win).unwrap(),
                    expected,
                    "utf8_src={utf8_src} windows={win}"
                );
            }
        }
        assert_eq!(
            cc_default_compile_action_for_os("модуль.CPPMAP", &fc_enabled, true).unwrap(),
            "c++-module-compile"
        );
        assert!(cc_default_compile_action_for_os("модуль.CPPMAP", &fc_enabled, false).is_err());

        // 4. Exhaustive `CppFileTypes` + `FileType.hasExtension` + `CppCompileActionBuilder.getActionName`
        // table across `windows = false` and `windows = true`.
        let cases: &[(&str, Option<&str>, Option<&str>)] = &[
            ("pkg/src.c", Some("c-compile"), Some("c-compile")),
            ("pkg/src.C", Some("c++-compile"), Some("c++-compile")),
            ("pkg/src.cc", Some("c++-compile"), Some("c++-compile")),
            ("pkg/src.CC", None, None),
            ("pkg/src.cpp", Some("c++-compile"), Some("c++-compile")),
            ("pkg/src.CPP", None, None),
            ("pkg/src.cxx", Some("c++-compile"), Some("c++-compile")),
            ("pkg/src.CXX", None, None),
            ("pkg/src.c++", Some("c++-compile"), Some("c++-compile")),
            ("pkg/src.C++", None, None),
            ("pkg/src.cu", Some("c++-compile"), Some("c++-compile")),
            ("pkg/src.CU", None, None),
            ("pkg/src.cl", Some("c++-compile"), Some("c++-compile")),
            ("pkg/src.CL", None, None),
            (
                "pkg/hdr.h",
                Some("c++-header-parsing"),
                Some("c++-header-parsing"),
            ),
            (
                "pkg/hdr.H",
                Some("c++-header-parsing"),
                Some("c++-header-parsing"),
            ),
            (
                "pkg/hdr.hpp",
                Some("c++-header-parsing"),
                Some("c++-header-parsing"),
            ),
            ("pkg/hdr.HPP", None, Some("c++-header-parsing")),
            (
                "pkg/hdr.ipp",
                Some("c++-header-parsing"),
                Some("c++-header-parsing"),
            ),
            ("pkg/hdr.IPP", None, Some("c++-header-parsing")),
            ("pkg/src.s", Some("assemble"), Some("assemble")),
            (
                "pkg/src.S",
                Some("preprocess-assemble"),
                Some("preprocess-assemble"),
            ),
            ("pkg/src.pic.s", None, None),
            ("pkg/src.PIC.s", Some("assemble"), None),
            (
                "pkg/src.pic.S",
                Some("preprocess-assemble"),
                Some("preprocess-assemble"),
            ),
            ("pkg/src.asm", Some("assemble"), Some("assemble")),
            ("pkg/src.ASM", None, Some("assemble")),
            ("pkg/src.m", Some("objc-compile"), Some("objc-compile")),
            ("pkg/src.M", None, Some("objc-compile")),
            ("pkg/src.mm", Some("objc++-compile"), Some("objc++-compile")),
            ("pkg/src.MM", None, Some("objc++-compile")),
            ("pkg/src.ipb", Some("clif-match"), Some("clif-match")),
            ("pkg/src.IPB", None, Some("clif-match")),
            (
                "pkg/mod.pcm",
                Some("c++-module-codegen"),
                Some("c++-module-codegen"),
            ),
            ("pkg/mod.PCM", None, Some("c++-module-codegen")),
            ("pkg/mod.cppm", None, None),
            ("pkg/mod.CPPM", None, None),
            (
                "pkg/mod.cppmap",
                Some("c++-module-compile"),
                Some("c++-module-compile"),
            ),
            ("pkg/mod.CPPMAP", None, Some("c++-module-compile")),
            ("pkg.c/no_ext", None, None),
        ];
        for &(path, unix_exp, win_exp) in cases {
            assert_eq!(
                cc_default_compile_action_for_os(path, &fc_enabled, false).ok(),
                unix_exp,
                "unix mismatch for {path}"
            );
            assert_eq!(
                cc_default_compile_action_for_os(path, &fc_enabled, true).ok(),
                win_exp,
                "windows mismatch for {path}"
            );
        }
    }
}
