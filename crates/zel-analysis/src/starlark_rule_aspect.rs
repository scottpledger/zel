// Aspects (`aspect()`): application of a Starlark-defined aspect to a configured target with
// propagation along `attr_aspects`, `required_providers` filtering, `requires`, aspect
// attributes/parameters, `ctx.rule` and the merging of aspect providers/actions into the
// target. Textually included from `starlark_rule.rs` (shares its private helpers).
//
// Mirrors `AspectFunction` / `StarlarkAspectFactory` / `ConfiguredAspect`.

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
    if let (Some(allowed), AttrValue::Str(s)) = (&attr.allowed_values, &v) {
        if !allowed.contains(s) {
            return Err(format!(
                "invalid value in '{}' attribute: has to be one of {} instead of '{s}'",
                attr.name,
                allowed.iter().map(|a| format!("'{a}'")).collect::<Vec<_>>().join(", ")
            ));
        }
    }
    Ok(v)
}

/// The parameters of `def` as set by `rule`'s same-named attributes (aspect attached to an
/// attribute of `rule`).
fn aspect_params_from_rule(def: &zel_packages::aspect::AspectDef, rule: &RuleInstance) -> BTreeMap<String, AttrValue> {
    let mut m = BTreeMap::new();
    for a in &def.attrs {
        if a.name.starts_with('_') {
            continue;
        }
        if let Some(v) = rule.extra.attr_values.get(&a.name) {
            m.insert(a.name.clone(), v.clone());
        }
    }
    m
}

fn target_matches_required_providers(ct: &ConfiguredTarget, groups: &[Vec<String>]) -> bool {
    groups.is_empty()
        || groups.iter().any(|g| {
            g.iter().all(|k| ct.providers.contains_key(&zel_packages::provider::resolve_provider_ref(k)))
        })
}

/// Applies the aspect `aspect_v` (and, recursively along `attr_aspects`, to the dependencies of
/// `ct`) with parameters taken from the attributes of the consuming rule of the same name.
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
    let def = zel_packages::aspect::aspect_of(aspect_v)
        .ok_or_else(|| format!("expected an aspect, got value of type '{}'", aspect_v.type_name()))?;
    let mut ct = ct;
    // `requires`: those aspects run first on the same target; their providers are visible.
    for r in &def.requires {
        ct = apply_aspect_with_params(inputs, ct, r, params)?;
    }
    let resolver = inputs.resolver.clone();
    let Some(rule) = resolver.rule_instance(&ct.label)? else {
        // Files / package groups: aspects only apply to rule targets.
        return Ok(ct);
    };
    let Some(rc) = rule.extra.rule_class.clone() else { return Ok(ct) };
    if !target_matches_required_providers(&ct, &def.required_providers) {
        return Ok(ct);
    }
    let sig = format!(
        "{}|{}#{}|{}|{}|{}|{}",
        ct.label.canonical_form(),
        ct.configuration.mnemonic(),
        ct.configuration.short_hash(),
        def.id,
        ct.providers.keys().cloned().collect::<Vec<_>>().join(","),
        ct.typed_actions.len(),
        params.iter().map(|(k, v)| format!("{k}={}", v.to_value().repr())).collect::<Vec<_>>().join(";")
    );
    if let Some(hit) = resolver.aspect_cache_get(&sig) {
        return Ok(hit);
    }
    let aspect_id = def.display_id();
    let loc = rule.extra.location.clone().map(|l| format!("{l}: ")).unwrap_or_default();
    let prefix = format!("{loc}in {aspect_id} aspect on {} rule {}: ", rc.name, ct.label.canonical_form());
    let wrap = |e: String| {
        if is_contextual_error(&e) || (e.starts_with(&loc) && e.contains("aspect on")) {
            e
        } else {
            format!("{prefix}{e}")
        }
    };
    let cfg = ct.configuration.clone();
    let pkg_dir = ct.label.package_identifier().package_fragment().clone();
    let out_root = ArtifactRoot::Bin;

    // 1. `ctx.rule`: the target's attributes, with the aspect propagated along `attr_aspects`.
    let def2 = def.clone();
    let hook = move |attr_name: &str, dep: Arc<ConfiguredTarget>, _c: &BuildConfigurationKey| {
        if def2.attr_aspects.iter().any(|a| a == "*" || a == attr_name) {
            apply_aspect_with_params(inputs, dep, aspect_v, params)
        } else {
            Ok(dep)
        }
    };
    let rule_bundle = convert_rule_attrs(&rule, &rc.attrs, &cfg, inputs, &pkg_dir, out_root, &wrap, Some(&hook))?;

    // 2. The aspect's own attributes (implicit dependencies and parameters).
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
            .or_else(|| rule.extra.attr_values.get(&a.name).cloned())
            .or_else(|| a.default.clone());
        match v {
            Some(v) => {
                if let (Some(allowed), AttrValue::Str(s)) = (&a.allowed_values, &v) {
                    if !allowed.contains(s) {
                        return Err(wrap(format!(
                            "invalid value in '{}' attribute: has to be one of {} instead of '{s}'",
                            a.name,
                            allowed.iter().map(|x| format!("'{x}'")).collect::<Vec<_>>().join(", ")
                        )));
                    }
                }
                arule.extra.attr_values.insert(a.name.clone(), v);
            }
            None => {
                return Err(wrap(format!("aspect parameter '{}' is mandatory but has no value", a.name)));
            }
        }
    }
    let aspect_bundle = convert_rule_attrs(&arule, &aspect_attr_map, &cfg, inputs, &pkg_dir, out_root, &wrap, None)?;

    // 3. Toolchains of the aspect.
    let mut ainputs = inputs.clone();
    ainputs.toolchains = BTreeMap::new();
    ainputs.extra_make_variables = BTreeMap::new();
    if !def.toolchains.is_empty() {
        let (tcs, vars) = resolver
            .aspect_toolchains(&ct.label, &def.toolchains, &def.exec_compatible_with, &cfg)
            .map_err(wrap)?;
        ainputs.toolchains = tcs;
        ainputs.extra_make_variables = vars;
    }

    let mut make_vars = BTreeMap::new();
    make_vars.insert("COMPILATION_MODE".to_string(), cfg.compilation_mode.clone());
    make_vars.insert("BINDIR".to_string(), format!("bazel-out/{}/bin", cfg.mnemonic()));
    make_vars.insert("GENDIR".to_string(), format!("bazel-out/{}/bin", cfg.mnemonic()));
    make_vars.insert("TARGET_CPU".to_string(), cfg.cpu.clone());
    for (k, v) in &ainputs.extra_make_variables {
        make_vars.insert(k.clone(), v.clone());
    }
    let rule_struct = mk_struct(vec![
        ("kind", Value::String(rule.rule_class.clone())),
        ("attr", rule_bundle.attr_struct()),
        ("file", Value::Struct(Arc::new(StructValue::new(rule_bundle.file_fields.clone())))),
        ("files", Value::Struct(Arc::new(StructValue::new(rule_bundle.files_fields.clone())))),
        ("executable", Value::Struct(Arc::new(StructValue::new(rule_bundle.exe_fields.clone())))),
    ]);
    let mut aspect_ids: Vec<String> = def.requires.iter().filter_map(|r| zel_packages::aspect::aspect_of(r)).map(|r| r.display_id()).collect();
    aspect_ids.push(aspect_id.clone());
    let st = Arc::new(CtxState {
        rule: rule.clone(),
        label: ct.label.clone(),
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
        graph: Mutex::new(ActionGraph::new()),
        declared_trees: Mutex::new(BTreeSet::new()),
        make_vars,
        frozen: Mutex::new(false),
        rule_struct: Some(rule_struct),
        aspect_ids,
        aspect_fragments: def.fragments.clone(),
        split_attr: Value::None,
    });

    // 4. Run the implementation: `impl(target, ctx)`.
    let mut ectx = EvalContext::new(pkg_dir.clone(), None);
    ectx.file_name = rule.extra.location.clone().unwrap_or_else(|| "BUILD".to_string());
    let st2 = st.clone();
    ectx.host = Some(Arc::new(move |c, n, p, k| dispatch(&st2, c, n, p, k)));
    let target_v = target_value(ct.clone());
    let ret = ectx
        .call_value(def.implementation.clone(), vec![target_v, Value::Host("ctx".into(), "0".into())], BTreeMap::new())
        .map_err(|e| wrap(format!("\n{}", e.0)));
    *st.frozen.lock().unwrap() = true;
    let ret = ret?;

    // 5. Providers (`ConfiguredAspect`).
    let mut declared: BTreeMap<String, Value> = BTreeMap::new();
    match &ret {
        Value::ProviderInstance { provider_name, .. } if provider_name == "struct" => {
            return Err(wrap("Returning a struct from an aspect implementation function is deprecated.".into()));
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
            return Err(wrap("Returning a struct from an aspect implementation function is deprecated.".into()));
        }
        Value::None => {}
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
    let typed: Vec<Action> = st.actions.lock().unwrap().clone();
    let mut out = (*ct).clone();
    out.aspect_name = Some(aspect_id.clone());
    for a in &typed {
        out.actions.extend(a.lower());
    }
    out.typed_actions = typed;
    for (k, v) in declared {
        if k == "OutputGroupInfo" {
            merge_output_groups(&mut out, &v).map_err(wrap)?;
            continue;
        }
        if out.providers.contains_key(&k) {
            return Err(wrap(format!(
                "Provider '{}' provided by the aspect is already provided by the target {}",
                zel_packages::provider::provider_display_name(&k),
                ct.label.canonical_form()
            )));
        }
        out.providers.insert(k, v);
    }
    let out = Arc::new(out);
    resolver.aspect_cache_put(sig, out.clone());
    Ok(out)
}

/// Merges an aspect's `OutputGroupInfo` into `ct` (union of files per group).
fn merge_output_groups(ct: &mut ConfiguredTarget, v: &Value) -> Result<(), String> {
    let Value::ProviderInstance { fields, .. } = v else { return Ok(()) };
    for (g, val) in fields {
        let files = match val {
            Value::Depset(_) | Value::List(_) | Value::Tuple(_) => files_in(val, &format!("output group '{g}'")).map_err(|e| e.0)?,
            other => {
                return Err(format!(
                    "expected value of type 'sequence or depset' for output group '{g}' but got {} instead",
                    type_name(other)
                ))
            }
        };
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
        .filter(|(g, _)| g.as_str() != "default")
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
