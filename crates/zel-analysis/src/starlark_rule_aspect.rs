// Aspects (`aspect()`): application of a Starlark-defined aspect to a configured target with
// propagation along `attr_aspects`, `required_providers` filtering, `requires`, aspect
// attributes/parameters, `ctx.rule` and the merging of aspect providers/actions into the
// target. Textually included from `starlark_rule.rs` (shares its private helpers).
//
// Mirrors `AspectContext`, `AspectResolutionHelpers`, `ConfiguredAspect`, `MergedConfiguredTarget`.

fn validate_aspect_allowed_value(attr: &Attribute, v: &AttrValue) -> Result<(), String> {
    let Some(allowed) = &attr.allowed_values else {
        return Ok(());
    };
    let rendered = match v {
        AttrValue::Str(s) => s.clone(),
        AttrValue::Int(n) => n.to_string(),
        AttrValue::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        _ => return Ok(()),
    };
    if !allowed.contains(&rendered) {
        return Err(format!(
            "invalid value in '{}' attribute: has to be one of {} instead of '{rendered}'",
            attr.name,
            allowed
                .iter()
                .map(|a| format!("'{a}'"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(())
}

/// Converts a `--aspects_parameters` style string into the value of aspect attribute `attr`.
fn aspect_param_value(attr: &Attribute, raw: &str) -> Result<AttrValue, String> {
    let bad = || {
        format!(
            "invalid value '{raw}' for aspect parameter '{}' (type {})",
            attr.name,
            attr.ty.name()
        )
    };
    let v = match attr.ty {
        AttrType::String => AttrValue::Str(raw.to_string()),
        AttrType::Int => AttrValue::Int(raw.parse::<i64>().map_err(|_| bad())?),
        AttrType::Bool => AttrValue::Bool(match raw {
            "true" | "True" | "1" => true,
            "false" | "False" | "0" => false,
            _ => return Err(bad()),
        }),
        _ => return Err(bad()),
    };
    validate_aspect_allowed_value(attr, &v)?;
    Ok(v)
}

fn target_matches_required_providers(ct: &ConfiguredTarget, groups: &[Vec<String>]) -> bool {
    groups.is_empty()
        || groups.iter().any(|g| {
            g.iter().all(|k| ct.providers.contains_key(&zel_packages::provider::resolve_provider_ref(k)))
        })
}

fn aspect_requires_other(outer: &zel_packages::aspect::AspectDef, inner_id: usize) -> bool {
    outer.requires.iter().any(|r| {
        zel_packages::aspect::aspect_of(r).is_some_and(|d| d.id == inner_id || aspect_requires_other(&d, inner_id))
    })
}

fn is_aspect_required_by_any(
    candidate_id: usize,
    selected: &[(Value, Arc<zel_packages::aspect::AspectDef>)],
) -> bool {
    selected.iter().any(|(_, d)| aspect_requires_other(d, candidate_id))
}

/// Flattens `aspect_v` and its `requires` DAG into bottom-up order, checking for cycles
/// (`AspectCollection.create` / `InconsistentAspectOrderException`).
fn collect_aspect_path(
    aspect_v: &Value,
    visiting: &mut Vec<usize>,
    visited: &mut Vec<usize>,
    out: &mut Vec<(Value, Arc<zel_packages::aspect::AspectDef>)>,
) -> Result<(), String> {
    let def = zel_packages::aspect::aspect_of(aspect_v)
        .ok_or_else(|| format!("expected an aspect, got value of type '{}'", aspect_v.type_name()))?;
    if visiting.contains(&def.id) {
        return Err(format!(
            "Aspect {} has a cycle in its 'requires' dependencies",
            def.display_id()
        ));
    }
    if visited.contains(&def.id) {
        return Ok(());
    }
    visiting.push(def.id);
    for r in &def.requires {
        collect_aspect_path(r, visiting, visited, out)?;
    }
    visiting.pop();
    visited.push(def.id);
    out.push((aspect_v.clone(), def));
    Ok(())
}

/// `AspectResolutionHelpers.collectPropagatingAspects`: walks `aspects_path` backwards
/// (top-down), skipping aspects that own `attr_name`, keeping aspects whose `attr_aspects` match
/// OR that are required by an already-selected aspect, then reverses back to path order.
fn collect_propagating_aspects(
    aspects_path: &[(Value, Arc<zel_packages::aspect::AspectDef>)],
    attr_name: &str,
) -> Vec<(Value, Arc<zel_packages::aspect::AspectDef>)> {
    let mut selected: Vec<(Value, Arc<zel_packages::aspect::AspectDef>)> = Vec::new();
    for (v, def) in aspects_path.iter().rev() {
        // Do not propagate over an attribute owned by the aspect itself.
        if def.attrs.iter().any(|a| a.name == attr_name) {
            continue;
        }
        let matches_attr = def.attr_aspects.iter().any(|a| a == "*" || a == attr_name);
        if matches_attr || is_aspect_required_by_any(def.id, &selected) {
            selected.push((v.clone(), def.clone()));
        }
    }
    selected.reverse();
    selected
}

/// Applies the aspect `aspect_v` (and, recursively along `attr_aspects`, to the dependencies of
/// `ct`).
pub fn apply_aspect(
    inputs: &AnalysisInputs,
    ct: Arc<ConfiguredTarget>,
    aspect_v: &Value,
) -> Result<Arc<ConfiguredTarget>, String> {
    apply_aspect_with_params(inputs, ct, aspect_v, &BTreeMap::new())
}

/// Like [`apply_aspect`] with explicit aspect `params` (`--aspects_parameters`).
pub fn apply_aspect_with_params(
    inputs: &AnalysisInputs,
    ct: Arc<ConfiguredTarget>,
    aspect_v: &Value,
    params: &BTreeMap<String, AttrValue>,
) -> Result<Arc<ConfiguredTarget>, String> {
    let mut path = Vec::new();
    collect_aspect_path(aspect_v, &mut Vec::new(), &mut Vec::new(), &mut path)?;
    apply_aspect_path(inputs, ct, &path, params)
}

fn apply_aspect_path(
    inputs: &AnalysisInputs,
    base_ct: Arc<ConfiguredTarget>,
    path: &[(Value, Arc<zel_packages::aspect::AspectDef>)],
    params: &BTreeMap<String, AttrValue>,
) -> Result<Arc<ConfiguredTarget>, String> {
    if path.is_empty() {
        return Ok(base_ct);
    }
    let resolver = inputs.resolver.clone();
    let Some(rule) = resolver.rule_instance(base_ct.actual_target_label())? else {
        // Files / package groups: aspects only apply to rule targets.
        return Ok(base_ct);
    };
    let Some(rc) = rule.extra.rule_class.clone() else {
        return Ok(base_ct);
    };

    // `AspectResolutionHelpers.computeAspectCollection`: filter top-down so an aspect required
    // by an already-kept outer aspect is retained if its own dependencies are present.
    let mut kept: Vec<(Value, Arc<zel_packages::aspect::AspectDef>)> = Vec::new();
    for (v, def) in path.iter().rev() {
        if target_matches_required_providers(&base_ct, &def.required_providers)
            || is_aspect_required_by_any(def.id, &kept)
        {
            kept.push((v.clone(), def.clone()));
        }
    }
    kept.reverse();
    if kept.is_empty() {
        return Ok(base_ct);
    }

    let mut current_ct = base_ct.clone();
    let is_alias = base_ct.is_alias();

    for idx in 0..kept.len() {
        let (_, ref def) = kept[idx];
        // Re-check `required_providers` / `required_aspect_providers` against providers accumulated
        // so far (including `requires` base aspects).
        if !target_matches_required_providers(&current_ct, &def.required_providers) {
            continue;
        }
        if !def.required_aspect_providers.is_empty()
            && !target_matches_required_providers(&current_ct, &def.required_aspect_providers)
        {
            continue;
        }

        let applied_so_far = &kept[..=idx];
        let aspect_id = def.display_id();
        let sig = format!(
            "{}|{}#{}|{}|{}|{}|{}",
            current_ct.label.canonical_form(),
            current_ct.configuration.mnemonic(),
            current_ct.configuration.short_hash(),
            applied_so_far.iter().map(|(_, d)| d.display_id()).collect::<Vec<_>>().join(","),
            current_ct.providers.keys().cloned().collect::<Vec<_>>().join(","),
            current_ct.typed_actions.len(),
            params.iter().map(|(k, v)| format!("{k}={}", v.to_value().repr())).collect::<Vec<_>>().join(";")
        );
        if let Some(hit) = resolver.aspect_cache_get(&sig) {
            current_ct = hit;
            continue;
        }

        let loc = rule.extra.location.clone().map(|l| format!("{l}: ")).unwrap_or_default();
        let prefix = format!(
            "{loc}in {aspect_id} aspect on {} rule {}: ",
            rc.name,
            current_ct.label.canonical_form()
        );
        let wrap = |e: String| {
            if is_contextual_error(&e) || (e.starts_with(&loc) && e.contains("aspect on")) {
                e
            } else {
                format!("{prefix}{e}")
            }
        };
        let cfg = current_ct.configuration.clone();
        let pkg_dir = current_ct.actual_target_label().package_identifier().package_fragment().clone();
        let out_root = ArtifactRoot::Bin;

        // 1. `ctx.rule`: rule attributes + base aspects' attributes (`mergeRuleAndBaseAspectsAttributes`),
        // with propagating aspects applied along matching `attr_aspects`.
        let path_for_hook: Vec<(Value, Arc<zel_packages::aspect::AspectDef>)> = applied_so_far.to_vec();
        let hook = move |attr_name: &str, dep: Arc<ConfiguredTarget>, _c: &BuildConfigurationKey| {
            let prop = collect_propagating_aspects(&path_for_hook, attr_name);
            if prop.is_empty() {
                Ok(dep)
            } else {
                apply_aspect_path(inputs, dep, &prop, params)
            }
        };
        let mut rule_attrs = rc.attrs.clone();
        // Base aspects' attributes (first name wins; rule attributes already in `rule_attrs` win).
        for (_, base_def) in &kept[..idx] {
            for a in &base_def.attrs {
                rule_attrs.entry(a.name.clone()).or_insert_with(|| a.clone());
            }
        }
        rule_attrs.retain(|_, a| {
            !a.name.starts_with('_')
                || applied_so_far
                    .iter()
                    .any(|(_, d)| d.attr_aspects.iter().any(|x| x == "*" || x == &a.name))
        });
        let rule_bundle =
            convert_rule_attrs(&rule, &rule_attrs, &cfg, inputs, &pkg_dir, out_root, &wrap, Some(&hook))?;

        // 2. The main aspect's own attributes (implicit dependencies and parameters).
        let mut aspect_attr_map: BTreeMap<String, Attribute> = BTreeMap::new();
        for a in &def.attrs {
            aspect_attr_map.insert(a.name.clone(), a.clone());
        }
        let mut arule = rule.clone();
        arule.extra.attr_values.clear();
        for a in &def.attrs {
            if a.name.starts_with('_') {
                continue;
            }
            let v = params
                .get(&a.name)
                .cloned()
                .or_else(|| a.default.clone());
            match v {
                Some(v) => {
                    validate_aspect_allowed_value(a, &v).map_err(wrap)?;
                    arule.extra.attr_values.insert(a.name.clone(), v);
                }
                None => {
                    return Err(wrap(format!(
                        "aspect parameter '{}' is mandatory but has no value",
                        a.name
                    )));
                }
            }
        }
        let aspect_bundle =
            convert_rule_attrs(&arule, &aspect_attr_map, &cfg, inputs, &pkg_dir, out_root, &wrap, None)?;

        // 3. Toolchains of the aspect (isolated from base target's toolchains).
        let mut ainputs = inputs.clone();
        ainputs.toolchains = BTreeMap::new();
        ainputs.extra_make_variables = BTreeMap::new();
        if !def.toolchains.is_empty() {
            let (tcs, vars) = resolver
                .aspect_toolchains(&current_ct.label, &def.toolchains, &def.exec_compatible_with, &cfg)
                .map_err(wrap)?;
            ainputs.toolchains = tcs;
            ainputs.extra_make_variables = vars;
        }

        // Aspect `ctx.var` omits `--define`; `ctx.rule.var` includes `--define` and base target's toolchains.
        let mut make_vars = BTreeMap::new();
        make_vars.insert("COMPILATION_MODE".to_string(), cfg.compilation_mode.clone());
        make_vars.insert("BINDIR".to_string(), format!("bazel-out/{}/bin", cfg.mnemonic()));
        make_vars.insert("GENDIR".to_string(), format!("bazel-out/{}/bin", cfg.mnemonic()));
        make_vars.insert("TARGET_CPU".to_string(), cfg.cpu.clone());
        for (k, v) in &ainputs.extra_make_variables {
            make_vars.insert(k.clone(), v.clone());
        }
        let mut rule_make_vars = BTreeMap::new();
        rule_make_vars.insert("COMPILATION_MODE".to_string(), cfg.compilation_mode.clone());
        rule_make_vars.insert("BINDIR".to_string(), format!("bazel-out/{}/bin", cfg.mnemonic()));
        rule_make_vars.insert("GENDIR".to_string(), format!("bazel-out/{}/bin", cfg.mnemonic()));
        rule_make_vars.insert("TARGET_CPU".to_string(), cfg.cpu.clone());
        for (k, v) in &inputs.extra_make_variables {
            rule_make_vars.insert(k.clone(), v.clone());
        }
        for (k, v) in &cfg.user_defines {
            rule_make_vars.insert(k.clone(), v.clone());
        }

        let rule_struct = mk_struct(vec![
            ("kind", Value::String(rule.rule_class.clone())),
            ("attr", rule_bundle.attr_struct()),
            ("file", Value::Struct(Arc::new(StructValue::new(rule_bundle.file_fields.clone())))),
            ("files", Value::Struct(Arc::new(StructValue::new(rule_bundle.files_fields.clone())))),
            ("executable", Value::Struct(Arc::new(StructValue::new(rule_bundle.exe_fields.clone())))),
            ("toolchains", Value::Host("rule_toolchains".into(), "0".into())),
            ("exec_groups", Value::Host("rule_exec_groups".into(), "0".into())),
            (
                "var",
                mk_dict(
                    rule_make_vars
                        .into_iter()
                        .map(|(k, v)| (Value::String(k), Value::String(v)))
                        .collect(),
                ),
            ),
        ]);
        let aspect_ids: Vec<String> = applied_so_far.iter().map(|(_, d)| d.display_id()).collect();
        let base_exec_groups: BTreeSet<String> = inputs.exec_group_toolchains.keys().cloned().collect();

        // Seed the aspect's ActionGraph with the base target's actions so conflicts against the
        // underlying rule or earlier aspects are detected.
        let mut graph = ActionGraph::new();
        for existing in &current_ct.typed_actions {
            let _ = graph.register(existing);
        }

        let st = Arc::new(CtxState {
            rule: rule.clone(),
            label: current_ct.label.clone(),
            cfg: cfg.clone(),
            inputs: ainputs,
            pkg_dir: pkg_dir.clone(),
            output_to_genfiles: false,
            exec_groups: BTreeSet::new(),
            deps: aspect_bundle.deps_by_attr.clone(),
            attr_struct: aspect_bundle.attr_struct(),
            file_struct: Value::Struct(Arc::new(StructValue::new(aspect_bundle.file_fields.clone()))),
            files_struct: Value::Struct(Arc::new(StructValue::new(aspect_bundle.files_fields.clone()))),
            executable_struct: Value::Struct(Arc::new(StructValue::new(aspect_bundle.exe_fields.clone()))),
            outputs_struct: Value::Struct(Arc::new(StructValue::new(BTreeMap::new()))),
            predeclared: Vec::new(),
            default_executable: None,
            default_executable_accessed: Mutex::new(false),
            actions: Mutex::new(Vec::new()),
            graph: Mutex::new(graph),
            declared_trees: Mutex::new(BTreeSet::new()),
            declared_files: Mutex::new(BTreeSet::new()),
            declared_artifacts: Mutex::new(Vec::new()),
            rule_errors: Mutex::new(
                rule_bundle
                    .attr_errors
                    .iter()
                    .chain(aspect_bundle.attr_errors.iter())
                    .cloned()
                    .collect(),
            ),
            make_vars,
            frozen: Mutex::new(false),
            rule_struct: Some(rule_struct),
            aspect_ids,
            aspect_fragments: def.fragments.clone(),
            aspect_toolchains_declared: def.toolchains.clone(),
            base_toolchains: inputs.toolchains.clone(),
            base_exec_groups,
            base_exec_group_toolchains: inputs.exec_group_toolchains.clone(),
            base_exec_group_platforms: inputs.exec_group_platforms.clone(),
            split_attr: Value::None,
            in_subrule: std::sync::atomic::AtomicBool::new(false),
        });

        // 4. Run the implementation: `impl(target, ctx)`.
        let mut ectx = EvalContext::new(pkg_dir.clone(), None);
        ectx.file_name = rule.extra.location.clone().unwrap_or_else(|| "BUILD".to_string());
        let st2 = st.clone();
        ectx.host = Some(Arc::new(move |c, n, p, k| dispatch(&st2, c, n, p, k)));
        let target_v = target_value(current_ct.clone());
        let ret = ectx
            .call_value(
                def.implementation.clone(),
                vec![target_v, Value::Host("ctx".into(), "0".into())],
                BTreeMap::new(),
            )
            .map_err(|e| wrap(format!("\n{}", e.0)));
        *st.frozen.lock().unwrap() = true;
        ectx.mutability.freeze();
        let ret = ret?;
        ret.freeze();

        // 5. Providers (`ConfiguredAspect`).
        let mut declared: BTreeMap<String, Value> = BTreeMap::new();
        match &ret {
            Value::ProviderInstance { provider_name, .. } if provider_name == "struct" => {
                return Err(wrap(
                    "Returning a struct from an aspect implementation function is deprecated.".into(),
                ));
            }
            Value::ProviderInstance { .. } => {
                declared.insert(provider_key(&ret).map_err(wrap)?, ret.clone());
            }
            Value::List(_) | Value::Tuple(_) => {
                for (i, p) in sequence(&ret).unwrap().into_iter().enumerate() {
                    if !matches!(p, Value::ProviderInstance { .. }) {
                        return Err(wrap(format!(
                            "\nA return value of an aspect implementation function should be a sequence of declared providers, instead got a {} at index {i}",
                            type_name(&p)
                        )));
                    }
                    let k = provider_key(&p).map_err(wrap)?;
                    if declared.insert(k.clone(), p).is_some() {
                        return Err(wrap(format!("Multiple conflicting returned providers with key {k}")));
                    }
                }
            }
            Value::Struct(_) => {
                return Err(wrap(
                    "Returning a struct from an aspect implementation function is deprecated.".into(),
                ));
            }
            other => {
                return Err(wrap(format!(
                    "Aspect implementation should return a list, or a provider instance, but got {}",
                    type_name(other)
                )));
            }
        }
        for k in def.provides.iter().map(|p| zel_packages::provider::resolve_provider_ref(p)) {
            if !declared.contains_key(&k) {
                return Err(wrap(format!(
                    "Aspect '{aspect_id}', declared to provide '{}', but did not provide it",
                    zel_packages::provider::provider_display_name(&k)
                )));
            }
        }
        if !st.rule_errors.lock().unwrap().is_empty() {
            return Err(wrap(st.rule_errors.lock().unwrap().join("\n")));
        }
        st.validate_artifacts().map_err(wrap)?;

        let typed: Vec<Action> = st.actions.lock().unwrap().clone();
        let mut out = (*current_ct).clone();
        out.aspect_name = Some(aspect_id.clone());
        // `ConfiguredAspect.forAlias`: aspects forwarded through an alias drop the real aspect's actions.
        if !is_alias {
            for a in &typed {
                out.actions.extend(a.lower());
            }
            out.typed_actions.extend(typed);
        }
        for (k, v) in declared {
            if k == "OutputGroupInfo" {
                merge_output_groups(&mut out, &v).map_err(wrap)?;
                continue;
            }
            if k == "InstrumentedFilesInfo" {
                merge_instrumented_files(&mut out, &v).map_err(wrap)?;
                continue;
            }
            if out.providers.contains_key(&k) {
                return Err(wrap(format!(
                    "Provider '{}' provided by the aspect is already provided by the target {}",
                    zel_packages::provider::provider_display_name(&k),
                    current_ct.label.canonical_form()
                )));
            }
            out.providers.insert(k, v);
        }
        let next_ct = Arc::new(out);
        resolver.aspect_cache_put(sig, next_ct.clone());
        current_ct = next_ct;
    }

    Ok(current_ct)
}

/// Merges an aspect's `OutputGroupInfo` into `ct` (union of files per group).
fn merge_output_groups(ct: &mut ConfiguredTarget, v: &Value) -> Result<(), String> {
    let Value::ProviderInstance { fields, .. } = v else { return Ok(()) };
    let mut explicit_groups: BTreeSet<String> = match ct.providers.get("OutputGroupInfo") {
        Some(Value::ProviderInstance { fields: existing, .. }) => existing.keys().cloned().collect(),
        _ => BTreeSet::new(),
    };
    for (g, val) in fields {
        let files = match val {
            Value::Depset(_) | Value::List(_) | Value::Tuple(_) => {
                files_in(val, &format!("output group '{g}'")).map_err(|e| e.0)?
            }
            other => {
                return Err(format!(
                    "expected value of type 'sequence or depset' for output group '{g}' but got {} instead",
                    type_name(other)
                ))
            }
        };
        explicit_groups.insert(g.clone());
        let entry = ct.output_groups.groups.entry(g.clone()).or_default();
        entry.extend(files);
        let merged = dedup(std::mem::take(entry));
        *entry = merged;
    }
    // Keep the provider instance consistent with the merged groups.
    let fields: Vec<(String, Value)> = ct
        .output_groups
        .groups
        .iter()
        .filter(|(g, _)| explicit_groups.contains(*g))
        .map(|(g, f)| (g.clone(), files_depset(f)))
        .collect();
    ct.providers.insert(
        "OutputGroupInfo".to_string(),
        Value::ProviderInstance {
            provider_name: "OutputGroupInfo".to_string(),
            fields: fields.into_iter().collect(),
        },
    );
    Ok(())
}

/// Merges an aspect's `InstrumentedFilesInfo` into `ct`.
fn merge_instrumented_files(ct: &mut ConfiguredTarget, v: &Value) -> Result<(), String> {
    let Value::ProviderInstance { fields, .. } = v else {
        return Ok(());
    };
    let files = |k: &str| -> Result<Vec<Artifact>, String> {
        match get_field(fields, k) {
            Some(val) => files_in(val, k).map_err(|e| e.0),
            None => Ok(Vec::new()),
        }
    };
    let asp_inst = files("instrumented_files")?;
    let asp_meta = files("metadata_files")?;
    let asp_env = match get_field(fields, "coverage_environment") {
        Some(val @ Value::Dict(_)) => str_dict(val, "coverage_environment").map_err(|e| e.0)?,
        _ => BTreeMap::new(),
    };
    let asp_support = files("coverage_support_files")?;

    let mut merged = ct.instrumented_files.clone().unwrap_or_default();
    merged.instrumented_files.extend(asp_inst);
    merged.instrumented_files = dedup(merged.instrumented_files);
    merged.metadata_files.extend(asp_meta);
    merged.metadata_files = dedup(merged.metadata_files);
    merged.environment.extend(asp_env);
    merged.support_files.extend(asp_support);
    merged.support_files = dedup(merged.support_files);

    let mut env_dict = StarlarkDict::new();
    for (k, val) in &merged.environment {
        let _ = env_dict.insert(Value::String(k.clone()), Value::String(val.clone()));
    }
    ct.providers.insert(
        "InstrumentedFilesInfo".to_string(),
        provider_instance(
            "InstrumentedFilesInfo",
            vec![
                ("instrumented_files", files_depset(&merged.instrumented_files)),
                ("metadata_files", files_depset(&merged.metadata_files)),
                ("coverage_environment", Value::Dict(Arc::new(Mutex::new(env_dict)))),
                ("coverage_support_files", files_depset(&merged.support_files)),
            ],
        ),
    );
    ct.instrumented_files = Some(merged);
    Ok(())
}

/// Applies the top-level aspect `aspect_v` (`--aspects`) with raw `--aspects_parameters`
/// values converted to the aspect attributes' types.
pub fn apply_aspect_cli(
    inputs: &AnalysisInputs,
    ct: Arc<ConfiguredTarget>,
    aspect_v: &Value,
    raw_params: &BTreeMap<String, String>,
) -> Result<Arc<ConfiguredTarget>, String> {
    let def = zel_packages::aspect::aspect_of(aspect_v)
        .ok_or_else(|| format!("expected an aspect, got value of type '{}'", aspect_v.type_name()))?;
    let mut params = BTreeMap::new();
    for a in &def.attrs {
        if a.name.starts_with('_') {
            continue;
        }
        if let Some(raw) = raw_params.get(&a.name) {
            params.insert(a.name.clone(), aspect_param_value(a, raw)?);
        }
    }
    apply_aspect_with_params(inputs, ct, aspect_v, &params)
}
