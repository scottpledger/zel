
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
    if name == "builtins.cc_common.check_experimental_cc_shared_library" {
        let enabled = crate::fragments::Opts::new(&st.cfg).b("experimental_cc_shared_library", true);
        return if enabled {
            Ok(Value::None)
        } else {
            err("Target/Rule requires --experimental_cc_shared_library to be enabled")
        };
    }
    if name == "builtins.cc_common.incompatible_disable_objc_library_transition" {
        return Ok(Value::Bool(
            crate::fragments::Opts::new(&st.cfg)
                .b("incompatible_disable_objc_library_transition", true),
        ));
    }
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
        return host_index(st, ctx, kind, &pos[0], &pos[1]);
    }
    if let Some(kind) = name.strip_prefix("contains:") {
        return host_contains(st, ctx, kind, &pos[0], &pos[1]);
    }
    if name == "call:subrule" {
        return call_subrule(st, ctx, pos, kw);
    }
    if name == "call:pkg_spec_contains" {
        let encoded = match pos.first() {
            Some(Value::Host(k, s)) if k == "pkg_spec_contains" => s.as_str(),
            _ => "",
        };
        let target_val = pos.get(1).or_else(|| kw.get("target")).ok_or_else(|| {
            StarlarkError("contains() missing 1 required positional argument: target".into())
        })?;
        let target_label = match target_val {
            Value::Label(l) => l.clone(),
            v @ Value::Host(k, _) if k == "target" => target_of(v)
                .map(|ct| ct.label.clone())
                .ok_or_else(|| StarlarkError("contains(): invalid Target".into()))?,
            Value::String(s) => {
                let rm = st.repo_mapping();
                crate::location::parse_label_with_mapping(
                    s,
                    st.label.package_identifier(),
                    Some(&rm),
                )
                .map_err(|e| StarlarkError(format!("invalid label '{s}': {e}")))?
            }
            other => {
                if let Some(ct) = target_of(other) {
                    ct.label.clone()
                } else {
                    return err(format!(
                        "in call to contains(), parameter 'target' got value of type '{}', want 'Target, Label, or string'",
                        other.type_name()
                    ));
                }
            }
        };
        let groups = zel_packages::pkg_spec::PackageGroupContents::decode_canonical_groups(encoded);
        let target_pkg = target_label.package_identifier();
        return Ok(Value::Bool(groups.iter().any(|g| g.contains(target_pkg))));
    }
    if name == "proto_common_do_not_use.incompatible_enable_proto_toolchain_resolution" {
        return Ok(Value::Bool(true));
    }
    if name == "proto_common_do_not_use.external_proto_infos" {
        return Ok(Value::Tuple(zel_packages::provider::external_proto_infos()));
    }
    if name.starts_with("call:") && name != "call:provider" && name != "call:provider_raw" {
        return err(format!("'{}' object is not callable", pos.first().map(|v| v.type_name()).unwrap_or("host_object")));
    }
    if name == "config_common.toolchain_type" {
        check_kwargs("toolchain_type", &pos, &kw, &["name", "mandatory"])?;
        let raw_name = arg(&pos, &kw, 0, "name").ok_or_else(|| {
            StarlarkError("toolchain_type() missing 1 required positional argument: name".into())
        })?;
        let tc_lbl = match raw_name {
            Value::Label(l) => l.clone(),
            Value::String(s) => {
                let (pkg, mapping) = match ctx.current_label_ctx() {
                    Some((p, m)) => (p, m),
                    None => (st.label.package_identifier().clone(), st.repo_mapping()),
                };
                zel_cmdline::Label::parse_with_package_context(s, &pkg, &mapping)
                    .map_err(|e| StarlarkError(format!("Unable to parse toolchain_type label '{s}': {e}")))?
            }
            other => {
                return err(format!(
                    "in call to toolchain_type(), parameter 'name' got value of type '{}', want 'string or Label'",
                    other.type_name()
                ))
            }
        };
        let mandatory = match arg(&pos, &kw, 1, "mandatory") {
            None => true,
            Some(Value::Bool(b)) => *b,
            Some(other) => {
                return err(format!(
                    "in call to toolchain_type(), parameter 'mandatory' got value of type '{}', want 'bool'",
                    other.type_name()
                ))
            }
        };
        let mut f: BTreeMap<String, Value> = BTreeMap::new();
        f.insert("toolchain_type".into(), Value::Label(tc_lbl));
        f.insert("mandatory".into(), Value::Bool(mandatory));
        return Ok(Value::Struct(Arc::new(StructValue::new(f))));
    }
    if name == "config_common.config_feature_flag_transition" {
        check_kwargs("config_feature_flag_transition", &pos, &kw, &["attribute"])?;
        let attr_name = want_str(
            arg(&pos, &kw, 0, "attribute").ok_or_else(|| {
                StarlarkError("config_feature_flag_transition() missing 1 required positional argument: attribute".into())
            })?,
            "config_feature_flag_transition",
            "attribute",
        )?;
        let mut f: BTreeMap<String, Value> = BTreeMap::new();
        f.insert("attribute".into(), Value::String(attr_name));
        f.insert("_config_feature_flag_transition".into(), Value::Bool(true));
        f.insert("_transition".into(), Value::Bool(true));
        return Ok(Value::Struct(Arc::new(StructValue::new(f))));
    }
    if name == "coverage_common.instrumented_files_info" {
        return instrumented_files_info(st, &pos, &kw);
    }
    if let Some(frag_fn) = name.strip_prefix("fragment.") {
        return match frag_fn {
            "apple.multi_arch_platform" => {
                check_kwargs("multi_arch_platform", &pos, &kw, &["platform_type"])?;
                let pt = want_str(
                    arg(&pos, &kw, 0, "platform_type").ok_or_else(|| {
                        StarlarkError(
                            "multi_arch_platform() missing 1 required positional argument: platform_type"
                                .into(),
                        )
                    })?,
                    "multi_arch_platform",
                    "platform_type",
                )?;
                crate::fragments::apple_multi_arch_platform(&st.cfg, &pt).map_err(StarlarkError)
            }
            "objc.target_should_alwayslink" => {
                if let Some((pkg, _)) = ctx.current_label_ctx() {
                    let repo = pkg.repository().name();
                    if repo != "_builtins"
                        && !repo.starts_with("apple_support")
                        && !repo.starts_with("build_bazel_rules_apple")
                    {
                        return err(format!("Rule in '{pkg}' cannot use private API"));
                    }
                }
                check_kwargs("target_should_alwayslink", &pos, &kw, &["ctx"])?;
                let ctx_arg = arg(&pos, &kw, 0, "ctx").ok_or_else(|| {
                    StarlarkError(
                        "target_should_alwayslink() missing 1 required positional argument: ctx"
                            .into(),
                    )
                })?;
                if !matches!(ctx_arg, Value::Host(k, _) if k == "ctx") {
                    return err(format!(
                        "in call to target_should_alwayslink(), parameter 'ctx' got value of type '{}', want 'ctx'",
                        ctx_arg.type_name()
                    ));
                }
                if st.rule.attrs.contains_key("alwayslink") {
                    if let Value::Struct(ref s) = st.attr_struct {
                        if let Some(Value::Bool(b)) = s.fields.get("alwayslink") {
                            return Ok(Value::Bool(*b));
                        }
                    }
                    if let Some(Value::Bool(b)) = st.rule.attrs.get("alwayslink") {
                        return Ok(Value::Bool(*b));
                    }
                }
                let by_default = st
                    .cfg
                    .options
                    .get("incompatible_objc_alwayslink_by_default")
                    .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
                    .or_else(|| {
                        match st
                            .cfg
                            .build_options()
                            .get("incompatible_objc_alwayslink_by_default")
                        {
                            Some(crate::build_options::ConfigValue::Scalar(s)) => {
                                Some(matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
                            }
                            _ => None,
                        }
                    })
                    .unwrap_or(false);
                Ok(Value::Bool(by_default))
            }
            "proto.strict_proto_deps"
            | "proto.strict_public_imports"
            | "proto.experimental_proto_descriptorsets_include_source_info" => {
                if let Some((pkg, _)) = ctx.current_label_ctx() {
                    let repo = pkg.repository().name();
                    let allowed = repo == "_builtins"
                        || repo == "bazel_tools"
                        || repo.starts_with("protobuf")
                        || repo.starts_with("com_google_protobuf")
                        || repo.starts_with("rules_proto");
                    if !allowed {
                        return err(format!("Rule in '{pkg}' cannot use private API"));
                    }
                }
                check_kwargs(frag_fn, &pos, &kw, &[])?;
                match frag_fn {
                    "proto.strict_proto_deps" => {
                        crate::fragments::strict_deps_mode_upper(&st.cfg, "strict_proto_deps", "error")
                            .map(Value::String)
                            .map_err(StarlarkError)
                    }
                    "proto.strict_public_imports" => {
                        crate::fragments::strict_deps_mode_upper(&st.cfg, "strict_public_imports", "off")
                            .map(Value::String)
                            .map_err(StarlarkError)
                    }
                    _ => {
                        let on = st
                            .cfg
                            .options
                            .get("experimental_proto_descriptor_sets_include_source_info")
                            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
                            .or_else(|| {
                                match st.cfg.build_options().get(
                                    "experimental_proto_descriptor_sets_include_source_info",
                                ) {
                                    Some(crate::build_options::ConfigValue::Scalar(s)) => Some(
                                        matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes"),
                                    ),
                                    _ => None,
                                }
                            })
                            .unwrap_or(false);
                        Ok(Value::Bool(on))
                    }
                }
            }
            other => err(format!("unknown fragment method '{other}'")),
        };
    }
    if name == "Label" {
        let v = pos.first().or_else(|| kw.get("input")).ok_or_else(|| {
            StarlarkError("Label() missing 1 required positional argument: input".into())
        })?;
        return match v {
            Value::Label(_) => Ok(v.clone()),
            Value::String(s) => {
                let (pkg, mapping) = match ctx.current_label_ctx() {
                    Some((p, m)) => (p, m),
                    None => {
                        let owner = st.label.package_identifier();
                        let ordered: Vec<(String, zel_cmdline::RepositoryName)> = st
                            .inputs
                            .resolver
                            .repo_mapping_entries(owner.repository().name())
                            .into_iter()
                            .map(|(a, c)| (a, zel_cmdline::RepositoryName::create_unvalidated(c)))
                            .collect();
                        (
                            owner.clone(),
                            zel_cmdline::RepositoryMapping::new_ordered(
                                ordered,
                                owner.repository().clone(),
                            ),
                        )
                    }
                };
                zel_cmdline::Label::parse_with_package_context(s, &pkg, &mapping)
                    .map(Value::Label)
                    .map_err(|e| StarlarkError(e.to_string()))
            }
            other => Err(StarlarkError(format!(
                "Label() got {} for 'input', want string or Label",
                other.type_name()
            ))),
        };
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
                "rule '{}' must declare subrule in 'subrules' (subrule called from rule '{}' which does not declare it in 'subrules')",
                st.rule.rule_class, st.rule.rule_class
            ));
        }
    }
    if !def.attrs.is_empty() {
        for a in &def.attrs {
            if a.name.starts_with('_') && kw.contains_key(&a.name) {
                return err(format!(
                    "got invalid named argument: '{}' is an implicit dependency and cannot be overridden",
                    a.name
                ));
            }
        }
        let amap: BTreeMap<String, Attribute> = def.attrs.iter().map(|a| (a.name.clone(), a.clone())).collect();
        let mut arule = st.rule.clone();
        arule.extra.attr_values.clear();
        let wrap = |e: String| e;
        let bundle = convert_rule_attrs(&arule, &amap, &st.cfg, &st.inputs, &st.pkg_dir, ArtifactRoot::Bin, &wrap, None)
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
    let prev = st.in_subrule.swap(true, std::sync::atomic::Ordering::Relaxed);
    let res = ctx.call_value(def.implementation.clone(), args, kw);
    st.in_subrule.store(prev, std::sync::atomic::Ordering::Relaxed);
    res
}

/// Whether `--collect_code_coverage` is on in `cfg`.
fn coverage_enabled(cfg: &BuildConfigurationKey) -> bool {
    cfg.bool_option("collect_code_coverage", false)
}

/// `RegexFilter.isIncluded(label.toString())` for `--instrumentation_filter`.
fn instrumentation_filter_includes(cfg: &BuildConfigurationKey, label: &Label) -> bool {
    let spec = cfg
        .option_str("instrumentation_filter")
        .unwrap_or_else(|| "-/javatests[/:],-/test/java[/:]".to_string());
    let mut pieces = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = spec.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == ',' && !(i > 0 && chars[i - 1] == '\\') {
            pieces.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    pieces.push(cur);
    let mut inclusions = Vec::new();
    let mut exclusions = Vec::new();
    for raw_piece in pieces {
        let mut p = raw_piece.replace("\\,", ",");
        let excluded = p.starts_with('-');
        if excluded || p.starts_with('+') {
            p.remove(0);
        }
        if p.is_empty() {
            continue;
        }
        // `build_options::regex_filter_string` normalises patterns using `(?>...)`; `java_regex`
        // uses non-capturing `(?:...)` groups with identical substring match semantics here.
        let normalized = p.replace("(?>", "(?:");
        if let Ok(re) = zel_util::java_regex::Regex::new(&normalized) {
            if excluded {
                exclusions.push(re);
            } else {
                inclusions.push(re);
            }
        }
    }
    let label_str = format!(
        "{}:{}",
        label.package_identifier().canonical_form(),
        label.name()
    );
    if exclusions.iter().any(|r| r.is_match(&label_str)) {
        return false;
    }
    inclusions.is_empty() || inclusions.iter().any(|r| r.is_match(&label_str))
}

/// `InstrumentedFilesCollector.shouldIncludeLocalSources`.
fn should_include_local_sources(cfg: &BuildConfigurationKey, label: &Label, is_test: bool) -> bool {
    (cfg.bool_option("instrument_test_targets", false) || !is_test)
        && instrumentation_filter_includes(cfg, label)
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
        let is_test = st
            .rule
            .extra
            .rule_class
            .as_ref()
            .is_some_and(|rc| rc.is_test);
        let include_generated = st
            .cfg
            .bool_option("experimental_collect_code_coverage_for_generated_files", false);
        if should_include_local_sources(&st.cfg, &st.label, is_test) {
            for a in &sources {
                let Some(Value::Struct(fs)) = Some(&st.files_struct) else { continue };
                // `InstrumentedFilesCollector.getPrerequisitesForAttributes`: attributes the rule class
                // does not define (`getAttributeByNameMaybe`) are skipped.
                let Some(list) = fs.fields.get(a) else { continue };
                for f in files_in(list, a)? {
                    if !(f.is_source || include_generated) {
                        continue;
                    }
                    let ext_ok = match &extensions {
                        None => true,
                        Some(exts) => exts.iter().any(|e| {
                            f.exec_path
                                .path_string()
                                .ends_with(&format!(".{}", e.trim_start_matches('.')))
                        }),
                    };
                    if ext_ok {
                        instrumented.push(f);
                    }
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

/// Fragments declared on the running rule or aspect (`fragments = [...]`).
fn declared_fragments(st: &CtxState) -> Vec<String> {
    if st.rule_struct.is_some() {
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
    }
}

fn is_known_fragment(name: &str) -> bool {
    matches!(
        name,
        "cpp" | "platform" | "java" | "bazel_py" | "py" | "proto" | "objc" | "apple" | "coverage"
    )
}

fn fragment_value(st: &CtxState, name: &str) -> Result<Value, StarlarkError> {
    let declared = declared_fragments(st);
    let universal = matches!(name, "coverage");
    if !universal && !declared.iter().any(|d| d == name) {
        if is_known_fragment(name) {
            return err(format!(
                "{} has to declare '{name}' as a required fragment in order to access it. Please update the 'fragments' argument of the rule definition (for example: fragments = [\"{name}\"])",
                st.rule.rule_class
            ));
        }
        return err(format!(
            "{} has to declare '{name}' as a required fragment in order to access it.",
            st.rule.rule_class
        ));
    }
    match crate::fragments::fragment(name, &st.cfg) {
        Ok(Some(v)) => Ok(v),
        Ok(None) => err(format!("unknown configuration fragment '{name}'")),
        Err(e) => err(e),
    }
}

fn has_toolchain_context(st: &CtxState) -> bool {
    if !st.inputs.toolchains.is_empty() {
        return true;
    }
    if st.rule_struct.is_some() {
        return !st.aspect_toolchains_declared.is_empty();
    }
    st.rule
        .extra
        .rule_class
        .as_ref()
        .and_then(|rc| rc.starlark.as_ref())
        .and_then(|sp| sp.kwargs.get("toolchains"))
        .and_then(sequence)
        .is_some_and(|v| !v.is_empty())
}

fn parse_bzl_or_rule_label(
    st: &CtxState,
    ctx: &EvalContext,
    raw: &str,
) -> Result<String, StarlarkError> {
    if let Some((pkg, mapping)) = ctx.current_label_ctx() {
        return zel_cmdline::Label::parse_with_package_context(raw, &pkg, &mapping)
            .map(|l| l.canonical_form())
            .map_err(|e| StarlarkError(format!("Unable to parse toolchain label '{raw}': {e}")));
    }
    if let Some(bzl_lbl) = st
        .rule
        .extra
        .rule_class
        .as_ref()
        .and_then(|rc| rc.starlark.as_ref())
        .and_then(|sp| Label::parse_canonical(&sp.defined_in).ok())
    {
        let owner = bzl_lbl.package_identifier();
        let entries = st
            .inputs
            .resolver
            .repo_mapping_entries(owner.repository().name());
        let mut map = std::collections::HashMap::new();
        for (a, c) in entries {
            if let Ok(rn) = zel_cmdline::RepositoryName::create(&c) {
                map.insert(a, rn);
            }
        }
        let mapping = zel_cmdline::RepositoryMapping::new(map, owner.repository().clone());
        return zel_cmdline::Label::parse_with_package_context(raw, owner, &mapping)
            .map(|l| l.canonical_form())
            .map_err(|e| StarlarkError(format!("Unable to parse toolchain label '{raw}': {e}")));
    }
    let rm = st.repo_mapping();
    crate::location::parse_label_with_mapping(raw, st.label.package_identifier(), Some(&rm))
        .map(|l| l.canonical_form())
        .map_err(|e| StarlarkError(format!("Unable to parse toolchain label '{raw}': {e}")))
}

fn transform_toolchain_key(
    st: &CtxState,
    ctx: &EvalContext,
    key: &Value,
) -> Result<String, StarlarkError> {
    match key {
        Value::Label(l) => Ok(l.canonical_form()),
        Value::ProviderInstance { provider_name, fields } if provider_name == "ToolchainTypeInfo" => {
            match fields.get("type_label") {
                Some(Value::Label(l)) => Ok(l.canonical_form()),
                _ => err("invalid ToolchainTypeInfo"),
            }
        }
        Value::Struct(s)
            if s.fields.contains_key("toolchain_type")
                || s.fields.contains_key("_toolchain_type")
                || s.fields.contains_key("name") =>
        {
            match s.fields.get("toolchain_type").or_else(|| s.fields.get("name")) {
                Some(Value::Label(l)) => Ok(l.canonical_form()),
                Some(Value::String(raw)) => parse_bzl_or_rule_label(st, ctx, raw),
                _ => err("invalid toolchain type key"),
            }
        }
        Value::String(raw) => parse_bzl_or_rule_label(st, ctx, raw),
        o => err(format!(
            "Toolchains only supports indexing by toolchain type, got {} instead",
            o.type_name()
        )),
    }
}

fn host_attr(st: &Arc<CtxState>, kind: &str, attr: &str, pos: &[Value]) -> HostResult {
    let recv = &pos[0];
    match kind {
        "file" => {
            let a = artifact_of(recv).ok_or_else(|| StarlarkError("invalid File".into()))?;
            file_attr(&a, attr).map_err(StarlarkError)
        }
        "action" => {
            let a = action_of(recv).ok_or_else(|| StarlarkError("invalid Action".into()))?;
            action_attr(&a, attr)
        }
        "expanded_directory" => {
            let (dir, children) = expanded_directory_of(recv)
                .ok_or_else(|| StarlarkError("invalid ExpandedDirectory".into()))?;
            match attr {
                "directory" => Ok(file_value(&dir)),
                "children" => Ok(mk_list(children)),
                other => file_attr(&dir, other).map_err(|_| {
                    StarlarkError(format!(
                        "'ExpandedDirectory' value has no field or method '{other}'"
                    ))
                }),
            }
        }
        "analysis_failure" => {
            let (lbl, msg) = analysis_failure_of(recv)
                .ok_or_else(|| StarlarkError("invalid AnalysisFailure".into()))?;
            match attr {
                "label" => Ok(Value::Label(lbl)),
                "message" => Ok(Value::String(msg)),
                other => err(format!(
                    "'AnalysisFailure' value has no field or method '{other}'"
                )),
            }
        }
        "template_ctx" => match attr {
            "declare_file" | "args" | "run" => {
                Ok(Value::BuiltinFunction(format!("host:method:template_ctx.{attr}")))
            }
            other => err(format!(
                "'template_ctx' value has no field or method '{other}'"
            )),
        },
        "target" => {
            let ct = target_of(recv).ok_or_else(|| StarlarkError("invalid Target".into()))?;
            target_attr(&ct, attr)
        }
        "runfiles" => {
            let r = runfiles_of(recv).ok_or_else(|| StarlarkError("invalid runfiles".into()))?;
            match attr {
                "files" => Ok(files_depset(&r.artifacts)),
                "symlinks" => {
                    let entries: Vec<Value> = r
                        .ordered_symlinks()
                        .into_iter()
                        .map(|(k, v)| {
                            mk_struct(vec![
                                ("path", Value::String(k)),
                                ("target_file", file_value(&v)),
                            ])
                        })
                        .collect();
                    Ok(Value::Depset(Arc::new(Depset::new(
                        DepsetOrder::Default,
                        entries,
                        Vec::new(),
                    )?)))
                }
                "root_symlinks" => {
                    let entries: Vec<Value> = r
                        .ordered_root_symlinks()
                        .into_iter()
                        .map(|(k, v)| {
                            mk_struct(vec![
                                ("path", Value::String(k)),
                                ("target_file", file_value(&v)),
                            ])
                        })
                        .collect();
                    Ok(Value::Depset(Arc::new(Depset::new(
                        DepsetOrder::Default,
                        entries,
                        Vec::new(),
                    )?)))
                }
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
            "host_path_separator" => Ok(Value::String(if cfg!(windows) { ";" } else { ":" }.into())),
            "short_id" => Ok(Value::String(st.cfg.short_hash())),
            "bin_dir" => Ok(mk_struct(vec![("path", Value::String(st.bin_dir()))])),
            "genfiles_dir" => Ok(mk_struct(vec![("path", Value::String(st.genfiles_dir()))])),
            // Methods are first-class values in Starlark: `ctx.configuration.stamp_binaries`
            // (rules_python reads it without calling it) is a (truthy) bound builtin.
            "stamp_binaries"
            | "is_tool_configuration"
            | "runfiles_enabled"
            | "has_separate_genfiles_directory"
            | "is_sibling_repository_layout"
            | "disabled_features" => {
                Ok(Value::BuiltinFunction(format!("host:method:configuration.{attr}")))
            }
            other => err(format!("'configuration' value has no field or method '{other}'")),
        },
        "fragments" => fragment_value(st, attr),
        "toolchains" | "rule_toolchains" | "exec_group_toolchains" | "rule_exec_group_toolchains" => {
            if attr == "toolchain_types" {
                Ok(Value::BuiltinFunction(format!("host:method:{kind}.toolchain_types")))
            } else {
                err(format!("'{kind}' value has no field or method '{attr}'"))
            }
        }
        "exec_groups" | "rule_exec_groups" => err(format!("'{kind}' value has no field or method '{attr}'")),
        "constraint_collection" => {
            let cc = crate::constraint_collection_of(recv)
                .ok_or_else(|| StarlarkError("invalid ConstraintCollection".into()))?;
            match attr {
                "constraint_settings" => Ok(mk_list(crate::constraint_collection_settings(&cc))),
                "get" | "has" | "has_constraint_value" => {
                    Ok(Value::BuiltinFunction(format!("host:method:constraint_collection.{attr}")))
                }
                _ => err(format!("'ConstraintCollection' value has no field or method '{attr}'")),
            }
        }
        "ctx" => ctx_attr(st, attr),
        other => err(format!("'{other}' value has no field or method '{attr}'")),
    }
}

const ALIAS_CLASS_OVERRIDES: &[&str] = &[
    "AliasProvider",
    "LateBoundAliasProvider",
    "VisibilityProvider",
    "RequiredConfigFragmentsProvider",
    "BuildSettingProvider",
];

fn starlark_visible_providers(ct: &ConfiguredTarget) -> &BTreeMap<String, Value> {
    &ct.get_actual().providers
}

fn is_alias_override_provider(ct: &ConfiguredTarget, pk: &str) -> bool {
    (ct.is_alias() || ct.is_late_bound_alias) && ALIAS_CLASS_OVERRIDES.contains(&pk)
}

fn action_attr(act: &Arc<Action>, attr: &str) -> HostResult {
    match attr {
        "mnemonic" => Ok(Value::String(act.mnemonic.clone())),
        "inputs" => Ok(files_depset(&act.inputs)),
        "outputs" => Ok(files_depset(&act.outputs)),
        "env" => {
            let env_map = match &act.action_env {
                Some(ae) => ae.fixed_env(),
                None => &act.env,
            };
            Ok(mk_dict(
                env_map
                    .iter()
                    .map(|(k, v)| (Value::String(k.clone()), Value::String(v.clone())))
                    .collect(),
            ))
        }
        "execution_info" => Ok(mk_dict(
            act.execution_requirements
                .iter()
                .map(|(k, v)| (Value::String(k.clone()), Value::String(v.clone())))
                .collect(),
        )),
        "argv" => match &act.kind {
            ActionKind::Spawn { .. } => Ok(mk_list(
                act.all_arguments().into_iter().map(Value::String).collect(),
            )),
            _ => Ok(Value::None),
        },
        "args" => match &act.kind {
            ActionKind::Spawn { arguments, .. } => {
                let mut list = Vec::with_capacity(arguments.len());
                for ca in arguments {
                    let b = ArgsBuilder {
                        args: ca.args.clone(),
                        param_file: ca.param_file.clone(),
                        format: ca.param_file.as_ref().map(|p| p.format),
                        ..Default::default()
                    };
                    list.push(args_value(b));
                }
                Ok(mk_list(list))
            }
            _ => Ok(Value::None),
        },
        "content" => match &act.kind {
            ActionKind::FileWrite { content, .. } => {
                if let FileContent::Args { args, .. } = content {
                    if args.iter().any(|s| s.starts_with(crate::args::TREE_MARKER)) {
                        return Ok(Value::None);
                    }
                }
                Ok(Value::String(
                    String::from_utf8_lossy(&content.render()).to_string(),
                ))
            }
            _ => Ok(Value::None),
        },
        "substitutions" => match &act.kind {
            ActionKind::TemplateExpand {
                substitutions,
                computed_substitutions,
                ..
            } => Ok(mk_dict(
                substitutions
                    .iter()
                    .chain(computed_substitutions.iter())
                    .map(|(k, v)| (Value::String(k.clone()), Value::String(v.clone())))
                    .collect(),
            )),
            _ => Ok(Value::None),
        },
        other => err(format!("'Action' value has no field or method '{other}'")),
    }
}

fn target_attr(ct: &Arc<ConfiguredTarget>, attr: &str) -> HostResult {
    let actual = ct.get_actual();
    if ct
        .configuration
        .bool_option("incompatible_disable_target_default_provider_fields", false)
        && matches!(
            attr,
            "files" | "files_to_run" | "default_runfiles" | "data_runfiles" | "output_groups"
        )
    {
        return err(
            "Accessing the default provider in this manner is deprecated and will be removed soon. \
             It may be temporarily re-enabled by setting \
             --incompatible_disable_target_default_provider_fields=false. See \
             https://github.com/bazelbuild/bazel/issues/20183 for details.",
        );
    }
    match attr {
        "label" => Ok(Value::Label(ct.actual_target_label().clone())),
        "actions" => Ok(mk_list(
            ct.typed_actions
                .iter()
                .filter(|a| {
                    a.mnemonic != "SourceSymlinkManifest" && a.mnemonic != "RepoMappingManifest"
                })
                .map(|a| action_value(Arc::new(a.clone())))
                .collect(),
        )),
        "files" => Ok(files_depset(&target_files(ct))),
        "files_to_run" => actual
            .providers
            .get("DefaultInfo")
            .and_then(|d| match d {
                Value::ProviderInstance { fields, .. } => fields.get("files_to_run").cloned(),
                _ => None,
            })
            .ok_or_else(|| StarlarkError("target has no files_to_run".into())),
        "default_runfiles" | "data_runfiles" => actual
            .providers
            .get("DefaultInfo")
            .and_then(|d| match d {
                Value::ProviderInstance { fields, .. } => fields.get(attr).cloned(),
                _ => None,
            })
            .ok_or_else(|| StarlarkError(format!("target has no {attr}"))),
        "output_groups" => Ok(actual
            .providers
            .get("OutputGroupInfo")
            .cloned()
            .unwrap_or_else(|| Value::ProviderInstance {
                provider_name: "OutputGroupInfo".to_string(),
                fields: BTreeMap::new(),
            })),
        other => {
            // Legacy access to provider fields by name, excluding class-level alias overrides
            // (`AliasConfiguredTarget.get`).
            for (pk, p) in starlark_visible_providers(ct) {
                if is_alias_override_provider(ct, pk) {
                    continue;
                }
                if let Value::ProviderInstance { fields, .. } = p {
                    if let Some(v) = fields.get(other) {
                        return Ok(v.clone());
                    }
                }
            }
            err(format!(
                "<target {}> (rule '{}') doesn't have provider '{other}'",
                actual.label.canonical_form(),
                actual.rule_class
            ))
        }
    }
}

fn host_index(
    _st: &Arc<CtxState>,
    ctx: &EvalContext,
    kind: &str,
    recv: &Value,
    key: &Value,
) -> HostResult {
    match kind {
        "target" => {
            let ct = target_of(recv).ok_or_else(|| StarlarkError("invalid Target".into()))?;
            let pk = zel_packages::provider::provider_key_of(key).ok_or_else(|| {
                StarlarkError(format!(
                    "unhashable or invalid provider key of type '{}'; expected a provider",
                    key.type_name()
                ))
            })?;
            let actual = ct.get_actual();
            let found = if is_alias_override_provider(&ct, &pk) {
                None
            } else {
                actual.providers.get(&pk).cloned()
            };
            found.ok_or_else(|| {
                StarlarkError(format!(
                    "<target {}> (rule '{}') doesn't contain declared provider '{}'",
                    actual.label.canonical_form(),
                    actual.rule_class,
                    zel_packages::provider::provider_display_name(&pk)
                ))
            })
        }
        "constraint_collection" => {
            let cc = crate::constraint_collection_of(recv)
                .ok_or_else(|| StarlarkError("invalid ConstraintCollection".into()))?;
            crate::constraint_collection_get(&cc, key).map_err(StarlarkError)
        }
        "toolchains" => {
            let st = _st;
            if !has_toolchain_context(st) {
                return err("Toolchains are not valid in this context");
            }
            let k = transform_toolchain_key(st, ctx, key)?;
            let k = if k.starts_with("//") {
                st.inputs.toolchains.keys().find(|c| c.starts_with("@@") && c.ends_with(&k)).cloned().unwrap_or(k)
            } else {
                k
            };
            st.inputs.toolchains.get(&k).cloned().ok_or_else(|| {
                StarlarkError(format!(
                    "In {} rule {}, toolchain type {} was requested but only types [{}] are configured",
                    st.rule.rule_class,
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
            let k = transform_toolchain_key(_st, ctx, key)?;
            let tcs = _st.inputs.exec_group_toolchains.get(group).cloned().unwrap_or_default();
            tcs.get(&k).cloned().ok_or_else(|| {
                StarlarkError(format!(
                    "In {} rule {}, exec group '{group}': toolchain type {k} was requested but only types [{}] are configured",
                    _st.rule.rule_class,
                    _st.label.canonical_form(),
                    tcs.keys().cloned().collect::<Vec<_>>().join(", ")
                ))
            })
        }
        "rule_toolchains" => {
            if _st.base_toolchains.is_empty() {
                return err("Toolchains are not valid in this context");
            }
            let k = transform_toolchain_key(_st, ctx, key)?;
            let k = if k.starts_with("//") {
                _st.base_toolchains.keys().find(|c| c.starts_with("@@") && c.ends_with(&k)).cloned().unwrap_or(k)
            } else {
                k
            };
            _st.base_toolchains.get(&k).cloned().ok_or_else(|| {
                StarlarkError(format!(
                    "In {} rule {}, toolchain type {} was requested but only types [{}] are configured",
                    _st.rule.rule_class,
                    _st.label.canonical_form(),
                    k,
                    _st.base_toolchains.keys().cloned().collect::<Vec<_>>().join(", ")
                ))
            })
        }
        "rule_exec_groups" => {
            let Value::String(name) = key else {
                return err(format!("invalid exec group key of type '{}'", key.type_name()));
            };
            if !_st.base_exec_groups.contains(name) {
                return err(format!(
                    "In rule '{}', unrecognized exec group '{name}'. Available exec groups: [{}]",
                    _st.label.canonical_form(),
                    _st.base_exec_groups.iter().cloned().collect::<Vec<_>>().join(", ")
                ));
            }
            Ok(mk_struct(vec![
                ("toolchains", Value::Host("rule_exec_group_toolchains".into(), name.clone())),
                (
                    "exec_platform",
                    _st.base_exec_group_platforms.get(name).map(|p| Value::String(p.clone())).unwrap_or(Value::None),
                ),
            ]))
        }
        "rule_exec_group_toolchains" => {
            let Value::Host(_, group) = recv else { return err("invalid exec group toolchains") };
            let k = transform_toolchain_key(_st, ctx, key)?;
            let tcs = _st.base_exec_group_toolchains.get(group).cloned().unwrap_or_default();
            tcs.get(&k).cloned().ok_or_else(|| {
                StarlarkError(format!(
                    "In {} rule {}, exec group '{group}': toolchain type {k} was requested but only types [{}] are configured",
                    _st.rule.rule_class,
                    _st.label.canonical_form(),
                    tcs.keys().cloned().collect::<Vec<_>>().join(", ")
                ))
            })
        }
        other => err(format!("type '{other}' is not indexable")),
    }
}

fn host_contains(
    st: &Arc<CtxState>,
    ctx: &EvalContext,
    kind: &str,
    recv: &Value,
    key: &Value,
) -> HostResult {
    match kind {
        "target" => {
            let ct = target_of(recv).ok_or_else(|| StarlarkError("invalid Target".into()))?;
            let pk = zel_packages::provider::provider_key_of(key)
                .ok_or_else(|| StarlarkError("'in <target>' requires a provider as left operand".into()))?;
            let has = !is_alias_override_provider(&ct, &pk)
                && ct.get_actual().providers.contains_key(&pk);
            Ok(Value::Bool(has))
        }
        "constraint_collection" => {
            let cc = crate::constraint_collection_of(recv)
                .ok_or_else(|| StarlarkError("invalid ConstraintCollection".into()))?;
            crate::constraint_collection_has(&cc, key)
                .map(Value::Bool)
                .map_err(StarlarkError)
        }
        "toolchains" => {
            if !has_toolchain_context(st) {
                return Ok(Value::Bool(false));
            }
            let k = transform_toolchain_key(st, ctx, key)?;
            let k = if k.starts_with("//") {
                st.inputs
                    .toolchains
                    .keys()
                    .find(|c| c.starts_with("@@") && c.ends_with(&k))
                    .cloned()
                    .unwrap_or(k)
            } else {
                k
            };
            Ok(Value::Bool(st.inputs.toolchains.contains_key(&k)))
        }
        "exec_groups" => Ok(Value::Bool(matches!(key, Value::String(n) if st.exec_groups.contains(n)))),
        "exec_group_toolchains" => {
            let Value::Host(_, group) = recv else { return Ok(Value::Bool(false)) };
            let k = transform_toolchain_key(st, ctx, key)?;
            Ok(Value::Bool(st.inputs.exec_group_toolchains.get(group).map(|m| m.contains_key(&k)).unwrap_or(false)))
        }
        "rule_toolchains" => {
            if st.base_toolchains.is_empty() {
                return Ok(Value::Bool(false));
            }
            let k = transform_toolchain_key(st, ctx, key)?;
            Ok(Value::Bool(st.base_toolchains.contains_key(&k)))
        }
        "rule_exec_groups" => Ok(Value::Bool(matches!(key, Value::String(n) if st.base_exec_groups.contains(n)))),
        "rule_exec_group_toolchains" => {
            let Value::Host(_, group) = recv else { return Ok(Value::Bool(false)) };
            let k = transform_toolchain_key(st, ctx, key)?;
            Ok(Value::Bool(st.base_exec_group_toolchains.get(group).map(|m| m.contains_key(&k)).unwrap_or(false)))
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
            if st.rule_struct.is_some() {
                return err("'outputs' is not defined");
            }
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
        "workspace_name" => {
            let ws = if st.label.package_identifier().repository().is_main() {
                st.inputs.workspace_name.clone()
            } else {
                st.label.package_identifier().repository().name().to_string()
            };
            Value::String(ws)
        }
        "bin_dir" => mk_struct(vec![("path", Value::String(st.bin_dir()))]),
        "genfiles_dir" => mk_struct(vec![("path", Value::String(st.genfiles_dir()))]),
        "info_file" => file_value(&Artifact {
            is_source: false,
            is_tree_artifact: false,
            root: ArtifactRoot::Bin,
            is_unresolved_symlink: false,
            is_constant_metadata: false,
            exec_path: PathFragment::create("bazel-out/stable-status.txt"),
            root_relative_path: PathFragment::create("stable-status.txt"),
            owner_label: st.label.clone(),
        }),
        "version_file" => file_value(&Artifact {
            is_source: false,
            is_tree_artifact: false,
            root: ArtifactRoot::Bin,
            is_unresolved_symlink: false,
            is_constant_metadata: true,
            exec_path: PathFragment::create("bazel-out/volatile-status.txt"),
            root_relative_path: PathFragment::create("volatile-status.txt"),
            owner_label: st.label.clone(),
        }),
        "build_file_path" => {
            if st
                .cfg
                .bool_option("incompatible_stop_exporting_build_file_path", true)
            {
                return err(
                    "Deprecated API: ctx.build_file_path has been removed. Use ctx.label.package + '/BUILD' instead of ctx.build_file_path.\nUse --noincompatible_stop_exporting_build_file_path to temporarily continue using it.",
                );
            }
            let fname = st
                .rule
                .extra
                .location
                .as_deref()
                .and_then(|loc| loc.split(':').next())
                .and_then(|p| p.rsplit('/').next())
                .filter(|b| *b == "BUILD.bazel" || *b == "BUILD")
                .unwrap_or("BUILD");
            Value::String(
                st.label
                    .package_identifier()
                    .package_fragment()
                    .get_relative_str(fname)
                    .path_string()
                    .to_string(),
            )
        }
        "var" => mk_dict(
            st.make_vars
                .iter()
                .map(|(k, v)| (Value::String(k.clone()), Value::String(v.clone())))
                .collect(),
        ),
        "fragments" => Value::Host("fragments".into(), "0".into()),
        "features" => Value::List(Arc::new(Mutex::new(StarlarkList::new(
            feature_list(st, false).into_iter().map(Value::String).collect(),
        )))),
        "disabled_features" => Value::List(Arc::new(Mutex::new(StarlarkList::new(
            feature_list(st, true).into_iter().map(Value::String).collect(),
        )))),
        "aspect_ids" => {
            if st.rule_struct.is_none() {
                return err("'aspect_ids' is only available in aspect implementations");
            }
            mk_list(st.aspect_ids.iter().map(|s| Value::String(s.clone())).collect())
        }
        "rule" => match &st.rule_struct {
            Some(r) => r.clone(),
            None => return err("'rule' is only available in aspect implementations"),
        },
        "build_setting_value" => build_setting_value(st)?,
        "created_actions" | "check_placeholders" | "package_relative_label" => {
            Value::BuiltinFunction(format!("host:method:ctx.{attr}"))
        }
        other => return err(format!("'ctx' value has no field or method '{other}'")),
    })
}

/// `FeatureSet`: disjoint sorted `on` and `off` feature sets matching `FeatureSet.java`.
struct FeatureSet {
    on: BTreeSet<String>,
    off: BTreeSet<String>,
}

impl FeatureSet {
    fn parse(features: &[String]) -> Self {
        let mut state: BTreeMap<String, bool> = BTreeMap::new();
        for f in features {
            if let Some(rest) = f.strip_prefix('-') {
                state.insert(rest.to_string(), false);
            } else if f == "no_layering_check" {
                state.insert("layering_check".to_string(), false);
            } else {
                state.entry(f.clone()).or_insert(true);
            }
        }
        Self::from_map(state)
    }

    fn from_map(state: BTreeMap<String, bool>) -> Self {
        let mut on = BTreeSet::new();
        let mut off = BTreeSet::new();
        for (k, v) in state {
            if v {
                on.insert(k);
            } else {
                off.insert(k);
            }
        }
        Self { on, off }
    }

    fn merge(coarse: Self, fine: Self) -> Self {
        let mut state = BTreeMap::new();
        for f in coarse.on {
            state.insert(f, true);
        }
        for f in coarse.off {
            state.insert(f, false);
        }
        for f in fine.on {
            state.insert(f, true);
        }
        for f in fine.off {
            state.insert(f, false);
        }
        Self::from_map(state)
    }

    fn merge_with_global(base: Self, global: Self) -> Self {
        let mut state = BTreeMap::new();
        for f in global.on {
            state.insert(f, true);
        }
        for f in base.on {
            state.insert(f, true);
        }
        for f in base.off {
            state.insert(f, false);
        }
        for f in global.off {
            state.insert(f, false);
        }
        Self::from_map(state)
    }
}

fn feature_list(st: &Arc<CtxState>, disabled: bool) -> Vec<String> {
    let rule = &st.rule;
    let pkg_fs = FeatureSet::parse(&rule.extra.pkg_features);
    // `features` is configurable: resolve a select() against the rule's configuration.
    let resolve_list = |av: Option<&AttrValue>| -> Vec<String> {
        let resolved = match av {
            Some(v @ AttrValue::Select(_)) => {
                crate::select::resolve_select(v, "features", rule, &st.cfg, st.inputs.resolver.as_ref()).ok()
            }
            other => other.cloned(),
        };
        match resolved {
            Some(AttrValue::StrList(l)) => l,
            _ => Vec::new(),
        }
    };
    let all_attr = resolve_list(rule.extra.attr_values.get("features"));
    let base = match &rule.extra.own_features {
        Some(own_av) => {
            let own = resolve_list(Some(own_av));
            let pkg_only: Vec<String> = all_attr
                .into_iter()
                .filter(|f| !own.contains(f))
                .collect();
            FeatureSet::merge(
                FeatureSet::merge(pkg_fs, FeatureSet::parse(&pkg_only)),
                FeatureSet::parse(&own),
            )
        }
        None => FeatureSet::merge(pkg_fs, FeatureSet::parse(&all_attr)),
    };
    let global_fs = FeatureSet::parse(&st.cfg.global_features());
    let merged = FeatureSet::merge_with_global(base, global_fs);
    if disabled {
        merged.off.into_iter().collect()
    } else {
        merged.on.into_iter().collect()
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
    let (ty, allow_multiple) = match spec.kwargs.get("build_setting") {
        Some(Value::Struct(s)) => {
            let t = match s.fields.get("_build_setting") {
                Some(Value::String(t)) => t.clone(),
                _ => "string".to_string(),
            };
            let mult = matches!(s.fields.get("allow_multiple"), Some(Value::Bool(true)));
            (t, mult)
        }
        _ => ("string".to_string(), false),
    };
    let key = st.label.canonical_form();
    if let Some(raw) = st.cfg.flag_values.get(&key) {
        if allow_multiple {
            let mut items = Vec::new();
            for part in raw.split(',').filter(|s| !s.is_empty()) {
                items.push(convert_flag_value(&ty, part).map_err(StarlarkError)?);
            }
            return Ok(mk_list(items));
        }
        return convert_flag_value(&ty, raw).map_err(StarlarkError);
    }
    match st.attr_struct {
        Value::Struct(ref s) => {
            let mut def = s
                .fields
                .get("build_setting_default")
                .cloned()
                .ok_or_else(|| StarlarkError("build setting has no default".into()))?;
            if ty == "string_set" {
                if let Some(items) = sequence(&def) {
                    let set = zel_starlark::StarlarkSet::from_elements(items)?;
                    let v = Value::Set(Arc::new(Mutex::new(set)));
                    v.freeze();
                    def = v;
                }
            }
            if allow_multiple {
                Ok(mk_list(vec![def]))
            } else if ty == "string_set" && !matches!(def, Value::Set(_)) {
                let mut set = zel_starlark::StarlarkSet::new();
                if let Ok(seq) = zel_starlark::eval::builtins::value_to_sequence(&def) {
                    for x in seq {
                        let _ = set.add(x);
                    }
                }
                let val = Value::Set(Arc::new(Mutex::new(set)));
                val.freeze();
                Ok(val)
            } else {
                Ok(def)
            }
        }
        _ => err("invalid attr"),
    }
}

/// The `config.<type>` of a build-setting rule (`string`, `bool`, `int`, `string_list`, ...), if `rule` is one.
pub fn build_setting_type_of(rule: &zel_packages::RuleInstance) -> Option<String> {
    let rc = rule.extra.rule_class.as_ref()?;
    if !rc.build_setting {
        return None;
    }
    if rule.rule_class == "label_flag" || rule.rule_class == "label_setting" {
        return Some("label".to_string());
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

/// Parses Bazel's boolean flag tokens (`BooleanConverter`), case-insensitively accepting
/// `true`/`1`/`yes`/`t`/`y` and `false`/`0`/`no`/`f`/`n`.
fn parse_bazel_bool(raw: &str) -> Option<bool> {
    match raw.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "t" | "y" => Some(true),
        "false" | "0" | "no" | "f" | "n" => Some(false),
        _ => None,
    }
}

/// Java `toString` of a command-line value of a build setting of type `ty` (what `BuildOptions`' Starlark
/// options map holds): `[a, b]` for lists, canonical integers/booleans, the raw text otherwise.
pub fn render_flag_value(ty: &str, raw: &str) -> String {
    match ty {
        "int" => raw.parse::<i64>().map(|i| i.to_string()).unwrap_or_else(|_| raw.to_string()),
        "bool" => parse_bazel_bool(raw)
            .map(|b| b.to_string())
            .unwrap_or_else(|| raw.to_string()),
        "string_list" | "string_set" => {
            if raw.is_empty() {
                "[]".to_string()
            } else if ty == "string_set" {
                let mut seen = Vec::new();
                for s in raw.split(',') {
                    if !seen.contains(&s) {
                        seen.push(s);
                    }
                }
                format!("[{}]", seen.join(", "))
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
        "int" => {
            let trimmed = raw.trim();
            let neg = trimmed.starts_with('-');
            let digits = trimmed.strip_prefix('-').or_else(|| trimmed.strip_prefix('+')).unwrap_or(trimmed);
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!("expected value of type 'int' for flag value '{raw}'"));
            }
            let signed = if neg { format!("-{digits}") } else { digits.to_string() };
            let bi = BigInt::from_str_radix(&signed, 10)
                .map_err(|_| format!("expected value of type 'int' for flag value '{raw}'"))?;
            Value::Int(bi)
        }
        "bool" => Value::Bool(
            parse_bazel_bool(raw)
                .ok_or_else(|| format!("expected a boolean for flag value '{raw}'"))?,
        ),
        "string_list" => {
            if raw.is_empty() {
                mk_list(vec![])
            } else {
                mk_list(raw.split(',').map(|s| Value::String(s.to_string())).collect())
            }
        }
        "string_set" => {
            let items: Vec<Value> = if raw.is_empty() {
                Vec::new()
            } else {
                raw.split(',').map(|s| Value::String(s.to_string())).collect()
            };
            let set = zel_starlark::StarlarkSet::from_elements(items).map_err(|e| e.0)?;
            let v = Value::Set(Arc::new(Mutex::new(set)));
            v.freeze();
            v
        }
        _ => Value::String(raw.to_string()),
    })
}
