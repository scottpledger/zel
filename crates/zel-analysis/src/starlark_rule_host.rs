
// ------------------------------------------------------------------------------------------
// Host dispatch

type HostResult = Result<Value, StarlarkError>;

fn dispatch(
    st: &Arc<CtxState>,
    ctx: &mut EvalContext,
    name: &str,
    mut pos: Vec<Value>,
    kw: BTreeMap<String, Value>,
) -> HostResult {
    if let Some(r) = cc_native(st, ctx, name, &pos, &kw) {
        return r;
    }
    zel_builtins::set_artifact_path_resolver(|v| artifact_of(v).map(|a| a.exec_path.path_string().to_string()));
    // Native part of Bazel's builtins (`cc_internal`, `_builtins.internal.*`, DottedVersion).
    if let Some(r) = zel_builtins::call(name, pos.clone(), kw.clone()) {
        return r;
    }
    if let Some(rest) = name.strip_prefix("attr:") {
        let (kind, attr) = rest.split_once('.').unwrap_or((rest, ""));
        return host_attr(st, kind, attr, &pos);
    }
    if let Some(rest) = name.strip_prefix("method:") {
        let (kind, method) = rest.split_once('.').unwrap_or((rest, ""));
        let recv = if pos.is_empty() { Value::None } else { pos.remove(0) };
        return host_method(st, ctx, kind, method, &recv, &pos, &kw);
    }
    if let Some(kind) = name.strip_prefix("index:") {
        return host_index(st, kind, &pos[0], &pos[1]);
    }
    if let Some(kind) = name.strip_prefix("contains:") {
        return host_contains(st, kind, &pos[0], &pos[1]);
    }
    if name == "call:subrule" {
        return call_subrule(st, ctx, pos, kw);
    }
    if name.starts_with("call:") && name != "call:provider" && name != "call:provider_raw" {
        return err(format!("'{}' object is not callable", pos.first().map(|v| v.type_name()).unwrap_or("host_object")));
    }
    if name == "config_common.toolchain_type" {
        let mut f: BTreeMap<String, Value> = BTreeMap::new();
        if let Some(v) = arg(&pos, &kw, 0, "name") {
            f.insert("name".into(), v.clone());
        }
        f.insert("mandatory".into(), kw.get("mandatory").cloned().unwrap_or(Value::Bool(true)));
        f.insert("_toolchain_type".into(), Value::Bool(true));
        return Ok(Value::Struct(Arc::new(StructValue::new(f))));
    }
    if name == "coverage_common.instrumented_files_info" {
        return instrumented_files_info(st, &pos, &kw);
    }
    // Fall back to the generic builtins the loader normally provides (provider(), ...).
    zel_packages::provider::host_dispatch(ctx, name, pos, kw)
}

/// Calls a subrule from a rule/aspect implementation: `implementation(ctx, *args, **kwargs,
/// **implicit_attrs)` where the implicit (`_x`) attributes are resolved like rule attributes
/// (labels → targets, or files with `allow_single_file` / `executable`).
fn call_subrule(
    st: &Arc<CtxState>,
    ctx: &mut EvalContext,
    mut pos: Vec<Value>,
    mut kw: BTreeMap<String, Value>,
) -> HostResult {
    let recv = if pos.is_empty() { Value::None } else { pos.remove(0) };
    let def = zel_packages::aspect::subrule_of(&recv)
        .ok_or_else(|| StarlarkError("internal error: call on an unknown subrule".into()))?;
    let declared: Vec<String> = st
        .rule
        .extra
        .rule_class
        .as_ref()
        .and_then(|rc| rc.starlark.as_ref())
        .and_then(|sp| sp.kwargs.get("subrules"))
        .and_then(sequence)
        .unwrap_or_default()
        .iter()
        .filter_map(|v| match v {
            Value::Host(k, id) if k == "subrule" => Some(id.clone()),
            _ => None,
        })
        .collect();
    if let Value::Host(_, id) = &recv {
        if st.rule_struct.is_none() && !declared.contains(id) {
            return err(format!(
                "subrule called from rule '{}' which does not declare it in 'subrules'",
                st.rule.rule_class
            ));
        }
    }
    if !def.attrs.is_empty() {
        let amap: BTreeMap<String, Attribute> = def.attrs.iter().map(|a| (a.name.clone(), a.clone())).collect();
        let mut arule = st.rule.clone();
        arule.extra.attr_values.clear();
        let wrap = |e: String| e;
        let bundle = convert_rule_attrs(&arule, &amap, &st.cfg, &st.inputs, &st.pkg_dir, st.root(), &wrap, None)
            .map_err(StarlarkError)?;
        for a in &def.attrs {
            let v = if a.executable {
                bundle.exe_fields.get(&a.name)
            } else if a.allow_single_file {
                bundle.file_fields.get(&a.name)
            } else {
                bundle.attr_fields.get(&a.name)
            };
            if let Some(v) = v {
                kw.entry(a.name.clone()).or_insert_with(|| v.clone());
            }
        }
    }
    let mut args = vec![Value::Host("ctx".into(), "0".into())];
    args.extend(pos);
    ctx.call_value(def.implementation.clone(), args, kw)
}

/// Whether `--collect_code_coverage` is on in `cfg`.
fn coverage_enabled(cfg: &BuildConfigurationKey) -> bool {
    matches!(cfg.options.get("collect_code_coverage").map(String::as_str), Some("true") | Some("1"))
}

/// `coverage_common.instrumented_files_info(ctx, source_attributes, dependency_attributes,
/// extensions, metadata_files, baseline_coverage_files)`: the `InstrumentedFilesInfo` of this
/// rule — its own sources (filtered by `extensions`) plus the instrumented files of the
/// dependency attributes. Empty unless coverage collection is enabled.
fn instrumented_files_info(st: &Arc<CtxState>, pos: &[Value], kw: &BTreeMap<String, Value>) -> HostResult {
    check_kwargs(
        "instrumented_files_info",
        pos,
        kw,
        &["ctx", "source_attributes", "dependency_attributes", "extensions", "metadata_files", "baseline_coverage_files", "coverage_support_files", "coverage_environment", "reported_to_actual_sources"],
    )?;
    let names = |i: usize, k: &str| -> Result<Vec<String>, StarlarkError> {
        match none_or(arg(pos, kw, i, k)) {
            None => Ok(Vec::new()),
            Some(v) => sequence(v)
                .ok_or_else(|| StarlarkError(format!("for parameter '{k}' expected a list of strings")))?
                .into_iter()
                .map(|x| match x {
                    Value::String(s) => Ok(s),
                    o => err(format!("for parameter '{k}' expected string, got {}", type_name(&o))),
                })
                .collect(),
        }
    };
    let sources = names(1, "source_attributes")?;
    let deps = names(2, "dependency_attributes")?;
    let extensions: Option<Vec<String>> = match none_or(arg(pos, kw, 3, "extensions")) {
        None => None,
        Some(_) => Some(names(3, "extensions")?),
    };
    let mut instrumented: Vec<Artifact> = Vec::new();
    let mut metadata: Vec<Artifact> = match none_or(arg(pos, kw, 4, "metadata_files")) {
        Some(v) => files_in(v, "metadata_files")?,
        None => Vec::new(),
    };
    if coverage_enabled(&st.cfg) {
        for a in &sources {
            let Some(Value::Struct(fs)) = Some(&st.files_struct) else { continue };
            // `InstrumentedFilesCollector.getPrerequisitesForAttributes`: attributes the rule class
            // does not define (`getAttributeByNameMaybe`) are skipped.
            let Some(list) = fs.fields.get(a) else { continue };
            for f in files_in(list, a)? {
                let ext_ok = match &extensions {
                    None => true,
                    Some(exts) => exts.is_empty() || exts.iter().any(|e| f.exec_path.path_string().ends_with(&format!(".{}", e.trim_start_matches('.')))),
                };
                if ext_ok {
                    instrumented.push(f);
                }
            }
        }
        for a in &deps {
            for d in st.deps.get(a).into_iter().flatten() {
                if let Some(i) = &d.instrumented_files {
                    instrumented.extend(i.instrumented_files.iter().cloned());
                    metadata.extend(i.metadata_files.iter().cloned());
                }
            }
        }
    } else {
        metadata.clear();
    }
    let instrumented = dedup(instrumented);
    let metadata = dedup(metadata);
    // `InstrumentedFilesInfoBuilder`: coverage environment and support files of the dependencies,
    // then the rule's own (which win on conflicting keys).
    let mut cov_env: BTreeMap<String, String> = BTreeMap::new();
    let mut support: Vec<Artifact> = Vec::new();
    if coverage_enabled(&st.cfg) {
        for a in &deps {
            for d in st.deps.get(a).into_iter().flatten() {
                if let Some(i) = &d.instrumented_files {
                    cov_env.extend(i.environment.iter().map(|(k, v)| (k.clone(), v.clone())));
                    support.extend(i.support_files.iter().cloned());
                }
            }
        }
        if let Some(v) = none_or(arg(pos, kw, 7, "coverage_environment")) {
            cov_env.extend(str_dict(v, "coverage_environment")?);
        }
        if let Some(v) = none_or(arg(pos, kw, 6, "coverage_support_files")) {
            support.extend(files_in(v, "coverage_support_files").unwrap_or_default());
        }
    }
    let support = dedup(support);
    let mut env_dict = StarlarkDict::new();
    for (k, v) in &cov_env {
        let _ = env_dict.insert(Value::String(k.clone()), Value::String(v.clone()));
    }
    Ok(provider_instance(
        "InstrumentedFilesInfo",
        vec![
            ("instrumented_files", files_depset(&instrumented)),
            ("metadata_files", files_depset(&metadata)),
            ("coverage_environment", Value::Dict(Arc::new(Mutex::new(env_dict)))),
            ("coverage_support_files", files_depset(&support)),
        ],
    ))
}

/// The rule's declared configuration fragments (`rule(fragments = [...])`) with their option
/// values taken from the configuration.
fn fragments_struct(st: &CtxState) -> Value {
    let declared: Vec<String> = if st.rule_struct.is_some() {
        // An aspect sees the fragments the aspect declares.
        st.aspect_fragments.clone()
    } else {
        st.rule
            .extra
            .rule_class
            .as_ref()
            .and_then(|rc| rc.starlark.as_ref())
            .and_then(|sp| sp.kwargs.get("fragments"))
            .and_then(sequence)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    };
    let mut fields: Vec<(&str, Value)> = Vec::new();
    for f in &declared {
        let v = match f.as_str() {
            "coverage" => mk_struct(vec![("collect_code_coverage", Value::Bool(coverage_enabled(&st.cfg)))]),
            other => crate::fragments::fragment(other, &st.cfg).unwrap_or_else(|| mk_struct(vec![])),
        };
        fields.push((f.as_str(), v));
    }
    mk_struct(fields)
}

fn host_attr(st: &Arc<CtxState>, kind: &str, attr: &str, pos: &[Value]) -> HostResult {
    let recv = &pos[0];
    match kind {
        "file" => {
            let a = artifact_of(recv).ok_or_else(|| StarlarkError("invalid File".into()))?;
            file_attr(&a, attr).map_err(StarlarkError)
        }
        "target" => {
            let ct = target_of(recv).ok_or_else(|| StarlarkError("invalid Target".into()))?;
            target_attr(&ct, attr)
        }
        "runfiles" => {
            let r = runfiles_of(recv).ok_or_else(|| StarlarkError("invalid runfiles".into()))?;
            match attr {
                "files" => Ok(files_depset(&r.artifacts)),
                "symlinks" => Ok(Value::Depset(Arc::new(Depset::new(
                    DepsetOrder::Default,
                    r.symlinks
                        .iter()
                        .map(|(k, v)| {
                            mk_struct(vec![("path", Value::String(k.clone())), ("target_file", file_value(v))])
                        })
                        .collect::<Vec<_>>()
                        .into_iter()
                        .map(|v| v)
                        .collect(),
                    Vec::new(),
                )?))),
                "root_symlinks" => Ok(Value::Depset(Arc::new(Depset::new(
                    DepsetOrder::Default,
                    r.root_symlinks
                        .iter()
                        .map(|(k, v)| {
                            mk_struct(vec![("path", Value::String(k.clone())), ("target_file", file_value(v))])
                        })
                        .collect(),
                    Vec::new(),
                )?))),
                "empty_filenames" => Ok(Value::Depset(Arc::new(Depset::new(
                    DepsetOrder::Default,
                    r.empty_filenames.iter().map(|s| Value::String(s.clone())).collect(),
                    Vec::new(),
                )?))),
                other => err(format!("'runfiles' value has no field or method '{other}'")),
            }
        }
        "outputs" => {
            if attr == "executable" {
                if let Some(e) = &st.default_executable {
                    *st.default_executable_accessed.lock().unwrap() = true;
                    return Ok(file_value(e));
                }
            }
            match &st.outputs_struct {
                Value::Struct(s) => match s.fields.get(attr) {
                    Some(v) => Ok(v.clone()),
                    None => err(format!("'struct' value has no field or method '{attr}'")),
                },
                _ => err("invalid ctx.outputs"),
            }
        }
        "actions" => err(format!("'actions' value has no field or method '{attr}'")),
        "configuration" => match attr {
            "coverage_enabled" => Ok(Value::Bool(coverage_enabled(&st.cfg))),
            // `BuildConfigurationValue.getLocalShellEnvironment`: the fixed part of the action
            // environment (the variables inherited from the client are only known at execution).
            "default_shell_env" => Ok(mk_dict(
                st.cfg
                    .action_environment()
                    .fixed_env()
                    .iter()
                    .map(|(k, v)| (Value::String(k.clone()), Value::String(v.clone())))
                    .collect(),
            )),
            // `BuildConfigurationValue.getTestEnv`: the fixed variables of `--test_env`.
            "test_env" => Ok(mk_dict(
                st.cfg
                    .test_action_environment()
                    .fixed_env()
                    .iter()
                    .map(|(k, v)| (Value::String(k.clone()), Value::String(v.clone())))
                    .collect(),
            )),
            "host_path_separator" => Ok(Value::String(":".into())),
            "short_id" => Ok(Value::String(st.cfg.mnemonic())),
            // Methods are first-class values in Starlark: `ctx.configuration.stamp_binaries`
            // (rules_python reads it without calling it) is a (truthy) bound builtin.
            "stamp_binaries" | "is_tool_configuration" | "runfiles_enabled" | "has_separate_genfiles_directory" | "is_sibling_repository_layout" => {
                Ok(Value::BuiltinFunction(format!("host:method:configuration.{attr}")))
            }
            other => err(format!("'configuration' value has no field or method '{other}'")),
        },
        "toolchains" | "exec_groups" => err(format!("'{kind}' value has no field or method '{attr}'")),
        "ctx" => ctx_attr(st, attr),
        other => err(format!("'{other}' value has no field or method '{attr}'")),
    }
}

fn target_attr(ct: &Arc<ConfiguredTarget>, attr: &str) -> HostResult {
    match attr {
        "label" => Ok(Value::Label(ct.label.clone())),
        "files" => Ok(files_depset(&target_files(ct))),
        "files_to_run" => ct
            .providers
            .get("DefaultInfo")
            .and_then(|d| match d {
                Value::ProviderInstance { fields, .. } => fields.get("files_to_run").cloned(),
                _ => None,
            })
            .ok_or_else(|| StarlarkError("target has no files_to_run".into())),
        "default_runfiles" | "data_runfiles" => ct
            .providers
            .get("DefaultInfo")
            .and_then(|d| match d {
                Value::ProviderInstance { fields, .. } => fields.get(attr).cloned(),
                _ => None,
            })
            .ok_or_else(|| StarlarkError(format!("target has no {attr}"))),
        other => {
            // Legacy access to provider fields by name (`target.output_groups`, ...).
            for p in ct.providers.values() {
                if let Value::ProviderInstance { fields, .. } = p {
                    if let Some(v) = fields.get(other) {
                        return Ok(v.clone());
                    }
                }
            }
            err(format!(
                "<target {}> (rule '{}') doesn't have provider '{other}'",
                ct.label.canonical_form(),
                ct.rule_class
            ))
        }
    }
}

fn host_index(_st: &Arc<CtxState>, kind: &str, recv: &Value, key: &Value) -> HostResult {
    match kind {
        "target" => {
            let ct = target_of(recv).ok_or_else(|| StarlarkError("invalid Target".into()))?;
            let pk = zel_packages::provider::provider_key_of(key).ok_or_else(|| {
                StarlarkError(format!(
                    "unhashable or invalid provider key of type '{}'; expected a provider",
                    key.type_name()
                ))
            })?;
            ct.providers.get(&pk).cloned().ok_or_else(|| {
                StarlarkError(format!(
                    "<target {}> (rule '{}') doesn't contain declared provider '{}'",
                    ct.label.canonical_form(),
                    ct.rule_class,
                    zel_packages::provider::provider_display_name(&pk)
                ))
            })
        }
        "toolchains" => {
            let st = _st;
            let k = match key {
                Value::Label(l) => l.canonical_form(),
                Value::String(s) => norm_tc_key(s),
                Value::Struct(s) => match s.fields.get("name") {
                    Some(Value::Label(l)) => l.canonical_form(),
                    Some(Value::String(s)) => norm_tc_key(s),
                    _ => return err("invalid toolchain type key"),
                },
                o => return err(format!("invalid toolchain type key of type '{}'", o.type_name())),
            };
            // A main-repo-relative string key ("//pkg:type") used inside an external rule's
            // .bzl names that repository's type: match by suffix.
            let k = if k.starts_with("//") {
                st.inputs.toolchains.keys().find(|c| c.starts_with("@@") && c.ends_with(&k)).cloned().unwrap_or(k)
            } else {
                k
            };
            st.inputs.toolchains.get(&k).cloned().ok_or_else(|| {
                StarlarkError(format!(
                    "In rule '{}', toolchain type {} was requested but only types [{}] are configured",
                    st.label.canonical_form(),
                    k,
                    st.inputs.toolchains.keys().cloned().collect::<Vec<_>>().join(", ")
                ))
            })
        }
        "exec_groups" => {
            let Value::String(name) = key else {
                return err(format!("invalid exec group key of type '{}'", key.type_name()));
            };
            if !_st.exec_groups.contains(name) {
                return err(format!(
                    "In rule '{}', unrecognized exec group '{name}'. Available exec groups: [{}]",
                    _st.label.canonical_form(),
                    _st.exec_groups.iter().cloned().collect::<Vec<_>>().join(", ")
                ));
            }
            Ok(mk_struct(vec![
                ("toolchains", Value::Host("exec_group_toolchains".into(), name.clone())),
                (
                    "exec_platform",
                    _st.inputs.exec_group_platforms.get(name).map(|p| Value::String(p.clone())).unwrap_or(Value::None),
                ),
            ]))
        }
        "exec_group_toolchains" => {
            let Value::Host(_, group) = recv else { return err("invalid exec group toolchains") };
            let k = match key {
                Value::Label(l) => l.canonical_form(),
                Value::String(s) => norm_tc_key(s),
                Value::Struct(s) => match s.fields.get("name") {
                    Some(Value::Label(l)) => l.canonical_form(),
                    Some(Value::String(s)) => norm_tc_key(s),
                    _ => return err("invalid toolchain type key"),
                },
                o => return err(format!("invalid toolchain type key of type '{}'", o.type_name())),
            };
            let tcs = _st.inputs.exec_group_toolchains.get(group).cloned().unwrap_or_default();
            tcs.get(&k).cloned().ok_or_else(|| {
                StarlarkError(format!(
                    "In rule '{}', exec group '{group}': toolchain type {k} was requested but only types [{}] are configured",
                    _st.label.canonical_form(),
                    tcs.keys().cloned().collect::<Vec<_>>().join(", ")
                ))
            })
        }
        other => err(format!("type '{other}' is not indexable")),
    }
}

fn host_contains(st: &Arc<CtxState>, kind: &str, recv: &Value, key: &Value) -> HostResult {
    match kind {
        "target" => {
            let ct = target_of(recv).ok_or_else(|| StarlarkError("invalid Target".into()))?;
            let pk = zel_packages::provider::provider_key_of(key)
                .ok_or_else(|| StarlarkError("'in <target>' requires a provider as left operand".into()))?;
            Ok(Value::Bool(ct.providers.contains_key(&pk)))
        }
        "toolchains" => {
            let k = match key {
                Value::Label(l) => l.canonical_form(),
                Value::String(s) => s.clone(),
                _ => return Ok(Value::Bool(false)),
            };
            Ok(Value::Bool(st.inputs.toolchains.contains_key(&k)))
        }
        "exec_groups" => Ok(Value::Bool(matches!(key, Value::String(n) if st.exec_groups.contains(n)))),
        "exec_group_toolchains" => {
            let Value::Host(_, group) = recv else { return Ok(Value::Bool(false)) };
            let k = match key {
                Value::Label(l) => l.canonical_form(),
                Value::String(s) => s.clone(),
                _ => return Ok(Value::Bool(false)),
            };
            Ok(Value::Bool(st.inputs.exec_group_toolchains.get(group).map(|m| m.contains_key(&k)).unwrap_or(false)))
        }
        other => err(format!("type '{other}' is not iterable")),
    }
}

fn ctx_attr(st: &Arc<CtxState>, attr: &str) -> HostResult {
    Ok(match attr {
        "label" => Value::Label(st.label.clone()),
        "attr" => st.attr_struct.clone(),
        "split_attr" => match &st.split_attr {
            Value::None => return err("'split_attr' is available only in rule implementations"),
            v => v.clone(),
        },
        "file" => st.file_struct.clone(),
        "files" => st.files_struct.clone(),
        "executable" => st.executable_struct.clone(),
        "outputs" => {
            if st.default_executable.is_some() {
                // `ctx.outputs.executable` is only created (and flagged as accessed) on access.
                return Ok(Value::Host("outputs".into(), "0".into()));
            }
            st.outputs_struct.clone()
        }
        "actions" => Value::Host("actions".into(), "0".into()),
        "toolchains" => Value::Host("toolchains".into(), "0".into()),
        "exec_groups" => Value::Host("exec_groups".into(), "0".into()),
        "configuration" => Value::Host("configuration".into(), "0".into()),
        "workspace_name" => Value::String(st.inputs.workspace_name.clone()),
        "bin_dir" => mk_struct(vec![("path", Value::String(st.bin_dir()))]),
        "genfiles_dir" => mk_struct(vec![("path", Value::String(st.genfiles_dir()))]),
        "build_file_path" => Value::String(
            st.label.package_identifier().package_fragment().get_relative_str("BUILD").path_string().to_string(),
        ),
        "var" => mk_dict(
            st.make_vars
                .iter()
                .map(|(k, v)| (Value::String(k.clone()), Value::String(v.clone())))
                .collect(),
        ),
        "fragments" => fragments_struct(st),
        "features" => Value::List(Arc::new(Mutex::new(StarlarkList::new(
            feature_list(st, false).into_iter().map(Value::String).collect(),
        )))),
        "disabled_features" => Value::List(Arc::new(Mutex::new(StarlarkList::new(
            feature_list(st, true).into_iter().map(Value::String).collect(),
        )))),
        "aspect_ids" => mk_list(st.aspect_ids.iter().map(|s| Value::String(s.clone())).collect()),
        "rule" => match &st.rule_struct {
            Some(r) => r.clone(),
            None => mk_struct(vec![
                ("kind", Value::String(st.rule.rule_class.clone())),
                ("attr", st.attr_struct.clone()),
            ]),
        },
        "build_setting_value" => build_setting_value(st)?,
        other => return err(format!("'ctx' value has no field or method '{other}'")),
    })
}

fn feature_list(st: &Arc<CtxState>, disabled: bool) -> Vec<String> {
    let rule = &st.rule;
    // `features` is configurable: resolve a select() against the rule's configuration.
    let resolved = match rule.extra.attr_values.get("features") {
        Some(v @ AttrValue::Select(_)) => {
            crate::select::resolve_select(v, "features", rule, &st.cfg, st.inputs.resolver.as_ref()).ok()
        }
        other => other.cloned(),
    };
    match resolved.as_ref() {
        Some(AttrValue::StrList(l)) => l
            .iter()
            .filter(|f| f.starts_with('-') == disabled)
            .map(|f| f.trim_start_matches('-').to_string())
            .collect(),
        _ => Vec::new(),
    }
}

fn build_setting_value(st: &Arc<CtxState>) -> HostResult {
    let rc = st.rule.extra.rule_class.as_ref().unwrap();
    if !rc.build_setting {
        return err(format!(
            "attempting to access 'build_setting_value' of non-build setting {}",
            st.label.canonical_form()
        ));
    }
    let spec = rc.starlark.as_ref().unwrap();
    let ty = match spec.kwargs.get("build_setting") {
        Some(Value::Struct(s)) => match s.fields.get("_build_setting") {
            Some(Value::String(t)) => t.clone(),
            _ => "string".to_string(),
        },
        _ => "string".to_string(),
    };
    let key = st.label.canonical_form();
    if let Some(raw) = st.cfg.flag_values.get(&key) {
        return convert_flag_value(&ty, raw).map_err(StarlarkError);
    }
    match st.attr_struct {
        Value::Struct(ref s) => s
            .fields
            .get("build_setting_default")
            .cloned()
            .ok_or_else(|| StarlarkError("build setting has no default".into())),
        _ => err("invalid attr"),
    }
}

/// The `config.<type>` of a build-setting rule (`string`, `bool`, `int`, `string_list`, ...), if `rule` is one.
pub fn build_setting_type_of(rule: &zel_packages::RuleInstance) -> Option<String> {
    let rc = rule.extra.rule_class.as_ref()?;
    if !rc.build_setting {
        return None;
    }
    let spec = rc.starlark.as_ref()?;
    Some(match spec.kwargs.get("build_setting") {
        Some(Value::Struct(s)) => match s.fields.get("_build_setting") {
            Some(Value::String(t)) => t.clone(),
            _ => "string".to_string(),
        },
        _ => "string".to_string(),
    })
}

/// Java `toString` of a command-line value of a build setting of type `ty` (what `BuildOptions`' Starlark
/// options map holds): `[a, b]` for lists, canonical integers/booleans, the raw text otherwise.
pub fn render_flag_value(ty: &str, raw: &str) -> String {
    match ty {
        "int" => raw.parse::<i64>().map(|i| i.to_string()).unwrap_or_else(|_| raw.to_string()),
        "bool" => match raw {
            "true" | "1" | "yes" => "true".to_string(),
            "false" | "0" | "no" => "false".to_string(),
            _ => raw.to_string(),
        },
        "string_list" => {
            if raw.is_empty() {
                "[]".to_string()
            } else {
                format!("[{}]", raw.split(',').collect::<Vec<_>>().join(", "))
            }
        }
        _ => raw.to_string(),
    }
}

/// Converts a command-line flag value for a build setting of type `ty` (`config.<ty>`).
pub fn convert_flag_value(ty: &str, raw: &str) -> Result<Value, String> {
    Ok(match ty {
        "int" => Value::Int(BigInt::from_i64(
            raw.parse::<i64>().map_err(|_| format!("expected value of type 'int' for flag value '{raw}'"))?,
        )),
        "bool" => match raw {
            "true" | "1" | "yes" => Value::Bool(true),
            "false" | "0" | "no" => Value::Bool(false),
            _ => return Err(format!("expected a boolean for flag value '{raw}'")),
        },
        "string_list" => mk_list(raw.split(',').map(|s| Value::String(s.to_string())).collect()),
        _ => Value::String(raw.to_string()),
    })
}

fn norm_tc_key(s: &str) -> String {
    if s.starts_with('@') && !s.starts_with("@@") && s.contains("//") { format!("@{s}") } else { s.to_string() }
}
