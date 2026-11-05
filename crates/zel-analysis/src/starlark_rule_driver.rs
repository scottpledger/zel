
// ------------------------------------------------------------------------------------------
// Driver

/// Attributes not exposed through `ctx.attr` (`visibility`-like metadata Bazel hides).
const HIDDEN_ATTRS: &[&str] = &["visibility", "compatible_with", "restricted_to", "package_metadata", "aspect_hints"];

fn pkg_relative(label: &Label, pkg_dir: &PathFragment) -> PathFragment {
    let _ = pkg_dir;
    label.package_identifier().package_fragment().get_relative_str(label.name())
}

/// Substitutes `%{name}` and `%{attr}` in implicit output templates (`ImplicitOutputsFunction`).
fn expand_output_template(tmpl: &str, rule: &RuleInstance) -> String {
    let mut out = String::new();
    let mut rest = tmpl;
    while let Some(i) = rest.find("%{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        if let Some(j) = after.find('}') {
            let key = &after[..j];
            if key == "name" {
                out.push_str(rule.label.name());
            } else if let Some(AttrValue::Str(s)) = rule.extra.attr_values.get(key) {
                out.push_str(s);
            }
            rest = &after[j + 1..];
        } else {
            out.push_str(&rest[i..]);
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

/// Whether `e` is already a complete rule error (`<loc>: in <rule> rule //x: ...`), e.g. the
/// failure of a dependency, which Bazel reports once for the failing target and does not wrap
/// again for its dependents.
pub fn is_contextual_error(e: &str) -> bool {
    let head = e.split_once(" rule ").map(|(h, _)| h).unwrap_or("");
    if head.is_empty() {
        return false;
    }
    // `in <class>` or `<file>:<line>:<col>: in <class>` / `... in <attr> attribute of <class>`.
    head.starts_with("in ")
        || head.rsplit_once(": in ").map(|(loc, _)| {
            let mut it = loc.rsplitn(3, ':');
            matches!((it.next(), it.next()), (Some(c), Some(l)) if c.chars().all(|x| x.is_ascii_digit()) && l.chars().all(|x| x.is_ascii_digit()) && !c.is_empty() && !l.is_empty())
        })
        .unwrap_or(false)
}

struct ConvertedAttr {
    value: Value,
    deps: Vec<Arc<ConfiguredTarget>>,
    /// Split-transition attributes: split key → value (`ctx.split_attr`).
    split: Option<Value>,
}

/// Converts an attribute value, resolving label dependencies in the configuration(s) produced by
/// the attribute's transition (`cfg = "exec"`, Starlark transition, split transition).
/// Split transitions yield `split` (key → value) in addition to the flattened `value`
/// (`ctx.attr.<name>` lists the targets of all splits; `ctx.split_attr.<name>` is keyed).
#[allow(clippy::too_many_arguments)]
fn attr_to_value(
    attr: &Attribute,
    av: &AttrValue,
    rule: &RuleInstance,
    cfg: &BuildConfigurationKey,
    inputs: &AnalysisInputs,
    pkg_dir: &PathFragment,
    out_root: ArtifactRoot,
    attrs_for_transition: Option<&Value>,
    hook: Option<&AspectHook>,
) -> Result<ConvertedAttr, String> {
    let cfgs: Vec<(Option<String>, BuildConfigurationKey)> = if attr.cfg_exec {
        vec![(
            None,
            match attr
                .cfg_exec_group
                .as_ref()
                .and_then(|g| inputs.exec_group_platforms.get(g))
                .or(inputs.exec_platform.as_ref())
            {
                Some(p) => cfg.to_exec_for(p),
                None => cfg.to_exec(),
            },
        )]
    } else if let Some(t) = &attr.cfg_transition {
        crate::transition::apply_transition_split(t, cfg, attrs_for_transition)?
    } else {
        vec![(None, cfg.clone())]
    };
    if cfgs.len() == 1 && cfgs[0].0.is_none() {
        return attr_to_value_in(attr, av, rule, cfg, &cfgs[0].1, inputs, pkg_dir, out_root, attrs_for_transition, hook);
    }
    // Split transition: one dependency set per key.
    let mut deps: Vec<Arc<ConfiguredTarget>> = Vec::new();
    let mut flat: Vec<Value> = Vec::new();
    let mut flat_dict: Vec<(Value, Value)> = Vec::new();
    let mut split: Vec<(Value, Value)> = Vec::new();
    for (key, dcfg) in &cfgs {
        let conv = attr_to_value_in(attr, av, rule, cfg, dcfg, inputs, pkg_dir, out_root, attrs_for_transition, hook)?;
        deps.extend(conv.deps.iter().cloned());
        match &conv.value {
            Value::None => {}
            Value::List(_) => flat.extend(sequence(&conv.value).unwrap_or_default()),
            Value::Dict(d) => flat_dict.extend(d.lock().unwrap().items()),
            other => flat.push(other.clone()),
        }
        let k = match key {
            Some(k) if !k.is_empty() => Value::String(k.clone()),
            _ => Value::None,
        };
        // Splits with no dependencies are omitted from `split_attr`.
        if !conv.deps.is_empty() {
            split.push((k, conv.value));
        }
    }
    let value = if !flat_dict.is_empty() { mk_dict(flat_dict) } else { mk_list(flat) };
    Ok(ConvertedAttr { value, deps, split: Some(mk_dict(split)) })
}

#[allow(clippy::too_many_arguments)]
fn attr_to_value_in(
    attr: &Attribute,
    av: &AttrValue,
    rule: &RuleInstance,
    _cfg: &BuildConfigurationKey,
    dep_cfg: &BuildConfigurationKey,
    inputs: &AnalysisInputs,
    pkg_dir: &PathFragment,
    out_root: ArtifactRoot,
    _attrs_for_transition: Option<&Value>,
    hook: Option<&AspectHook>,
) -> Result<ConvertedAttr, String> {
    let cfg = _cfg;
    let resolve = |l: &Label| -> Result<Arc<ConfiguredTarget>, String> {
        let mut ct = inputs.resolver.configured_target(l, dep_cfg)?;
        inputs.resolver.check_dependency(rule, &attr.name, &ct)?;
        // Aspects declared on the attribute (`attr.label(aspects = [...])`), then the aspect
        // currently propagating along this attribute (`attr_aspects`).
        for asp in &attr.aspects {
            ct = apply_aspect(inputs, ct, asp)?;
        }
        if let Some(h) = hook {
            ct = h(&attr.name, ct, dep_cfg)?;
        }
        if !attr.providers.is_empty() {
            // Source/generated files are accepted when the attribute allows files.
            let is_file = ct.rule_class == "source file" || ct.rule_class == "generated file";
            let ok = attr.providers.iter().any(|group| group.iter().all(|k| ct.providers.contains_key(&zel_packages::provider::resolve_provider_ref(k))));
            // An allowed rule class (`allow_rules`) is accepted even without the providers.
            let allowed_class = attr.allowed_rule_classes.as_ref().map(|l| l.contains(&ct.rule_class)).unwrap_or(false);
            if !ok && !allowed_class && !(is_file && !matches!(attr.allowed_files, zel_packages::attr::FileTypes::NoFiles)) {
                let desc = attr
                    .providers
                    .iter()
                    .map(|g| {
                        g.iter()
                            .map(|k| {
                                let key = zel_packages::provider::resolve_provider_ref(k);
                                format!("'{}'", zel_packages::provider::provider_display_name(&key))
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .collect::<Vec<_>>()
                    .join(" or ");
                let loc = rule.extra.location.clone().map(|l| format!("{l}: ")).unwrap_or_default();
                return Err(format!(
                    "{loc}in {} attribute of {} rule {}: '{}' does not have mandatory providers: {desc}",
                    attr.name,
                    rule.rule_class,
                    rule.label.canonical_form(),
                    ct.label.canonical_form()
                ));
            }
        }
        Ok(ct)
    };
    let out_art = |l: &Label| {
        Artifact::derived_in_root(&cfg.mnemonic(), out_root, pkg_relative(l, pkg_dir), rule.label.clone())
    };
    Ok(match av {
        AttrValue::Int(i) => ConvertedAttr { value: Value::Int(BigInt::from_i64(*i)), deps: vec![], split: None },
        AttrValue::Tristate(i) => ConvertedAttr { value: Value::Int(BigInt::from_i64(*i as i64)), deps: vec![], split: None },
        AttrValue::Str(s) => ConvertedAttr { value: Value::String(s.clone()), deps: vec![], split: None },
        AttrValue::Bool(b) => ConvertedAttr { value: Value::Bool(*b), deps: vec![], split: None },
        AttrValue::StrList(l) => ConvertedAttr {
            value: mk_list(l.iter().map(|s| Value::String(s.clone())).collect()),
            deps: vec![],
            split: None,
        },
        AttrValue::IntList(l) => ConvertedAttr {
            value: mk_list(l.iter().map(|n| Value::Int(BigInt::from_i64(*n))).collect()),
            deps: vec![],
            split: None,
        },
        AttrValue::StrKeyedLabelDict(items) => {
            let mut cts = Vec::new();
            let mut entries = Vec::new();
            for (s, l) in items {
                let ct = resolve(l)?;
                entries.push((Value::String(s.clone()), target_value(ct.clone())));
                cts.push(ct);
            }
            ConvertedAttr { value: mk_dict(entries), deps: cts, split: None }
        }
        AttrValue::StrDict(d) => ConvertedAttr {
            value: mk_dict(d.iter().map(|(k, v)| (Value::String(k.clone()), Value::String(v.clone()))).collect()),
            deps: vec![],
            split: None,
        },
        AttrValue::StrListDict(d) => ConvertedAttr {
            value: mk_dict(
                d.iter()
                    .map(|(k, v)| {
                        (Value::String(k.clone()), mk_list(v.iter().map(|s| Value::String(s.clone())).collect()))
                    })
                    .collect(),
            ),
            deps: vec![],
            split: None,
        },
        AttrValue::Label(None) => ConvertedAttr { value: Value::None, deps: vec![], split: None },
        AttrValue::Label(Some(l)) => {
            let ct = resolve(l)?;
            ConvertedAttr { value: target_value(ct.clone()), deps: vec![ct], split: None }
        }
        AttrValue::LabelList(ls) => {
            let mut cts = Vec::new();
            for l in ls {
                cts.push(resolve(l)?);
            }
            ConvertedAttr {
                value: mk_list(cts.iter().map(|c| target_value(c.clone())).collect()),
                deps: cts,
                split: None,
            }
        }
        AttrValue::LabelKeyedStrDict(items) => {
            let mut cts = Vec::new();
            let mut entries = Vec::new();
            for (l, s) in items {
                let ct = resolve(l)?;
                entries.push((target_value(ct.clone()), Value::String(s.clone())));
                cts.push(ct);
            }
            ConvertedAttr { value: mk_dict(entries), deps: cts, split: None }
        }
        AttrValue::Output(None) => ConvertedAttr { value: Value::None, deps: vec![], split: None },
        AttrValue::Output(Some(l)) => ConvertedAttr { value: file_value(&out_art(l)), deps: vec![], split: None },
        AttrValue::OutputList(ls) => ConvertedAttr {
            value: mk_list(ls.iter().map(|l| file_value(&out_art(l))).collect()),
            deps: vec![],
            split: None,
        },
        AttrValue::Select(_) => return Err(format!("internal error: unresolved select() in attribute '{}'", attr.name)),
    })
}

fn files_of_deps(deps: &[Arc<ConfiguredTarget>]) -> Vec<Artifact> {
    let mut out = Vec::new();
    for d in deps {
        out.extend(target_files(d));
    }
    dedup(out)
}


type AspectHook<'a> = dyn Fn(&str, Arc<ConfiguredTarget>, &BuildConfigurationKey) -> Result<Arc<ConfiguredTarget>, String> + 'a;

struct AttrBundle {
    attr_fields: BTreeMap<String, Value>,
    file_fields: BTreeMap<String, Value>,
    files_fields: BTreeMap<String, Value>,
    exe_fields: BTreeMap<String, Value>,
    outputs_fields: BTreeMap<String, Value>,
    deps_by_attr: BTreeMap<String, Vec<Arc<ConfiguredTarget>>>,
    predeclared: Vec<Artifact>,
    /// `ctx.split_attr`: attribute → (split key → target(s)).
    split_fields: BTreeMap<String, Value>,
}

impl AttrBundle {
    fn attr_struct(&self) -> Value {
        Value::Struct(Arc::new(StructValue::new(self.attr_fields.clone())))
    }
}

/// Converts the attribute values of `rule` for the attribute schema `attrs` into the
/// `ctx.attr` / `ctx.file` / `ctx.files` / `ctx.executable` / `ctx.outputs` structs, resolving
/// dependencies (and applying the attributes' own `aspects`). `hook` additionally post-processes
/// every resolved dependency (used to propagate an aspect along its `attr_aspects`).
#[allow(clippy::too_many_arguments)]
fn convert_rule_attrs(
    rule: &RuleInstance,
    attrs: &BTreeMap<String, Attribute>,
    cfg: &BuildConfigurationKey,
    inputs: &AnalysisInputs,
    pkg_dir: &PathFragment,
    out_root: ArtifactRoot,
    wrap: &dyn Fn(String) -> String,
    hook: Option<&AspectHook>,
) -> Result<AttrBundle, String> {
    let mut attr_fields: BTreeMap<String, Value> = BTreeMap::new();
    let mut file_fields: BTreeMap<String, Value> = BTreeMap::new();
    let mut files_fields: BTreeMap<String, Value> = BTreeMap::new();
    let mut exe_fields: BTreeMap<String, Value> = BTreeMap::new();
    let mut outputs_fields: BTreeMap<String, Value> = BTreeMap::new();
    let mut deps_by_attr: BTreeMap<String, Vec<Arc<ConfiguredTarget>>> = BTreeMap::new();
    let mut split_fields: BTreeMap<String, Value> = BTreeMap::new();
    let mut predeclared: Vec<Artifact> = Vec::new();
    let resolver = inputs.resolver.clone();
    // Plain (non-label) attribute values, passed to attribute transitions as `attr`.
    let raw_attrs = {
        let mut m = BTreeMap::new();
        for (name, attr) in attrs {
            if matches!(attr.ty, AttrType::Label | AttrType::LabelList | AttrType::LabelKeyedStringDict | AttrType::StringKeyedLabelDict | AttrType::Output | AttrType::OutputList) {
                continue;
            }
            let av = rule.extra.attr_values.get(name).cloned().unwrap_or_else(|| attr.default_value());
            if let Ok(av) = crate::select::resolve_select(&av, name, rule, cfg, resolver.as_ref()) {
                let dummy_inputs = inputs;
                if let Ok(c) = attr_to_value(attr, &av, rule, cfg, dummy_inputs, pkg_dir, out_root, None, hook) {
                    m.insert(name.clone(), c.value);
                }
            }
        }
        Value::Struct(Arc::new(StructValue::new(m)))
    };
    for (name, attr) in attrs {
        if HIDDEN_ATTRS.contains(&name.as_str()) {
            continue;
        }
        let explicit = if name == "name" {
            Some(AttrValue::Str(rule.label.name().to_string()))
        } else {
            rule.extra.attr_values.get(name).cloned()
        };
        let av = match explicit {
            Some(v) => v,
            None => match &attr.late_bound {
                // `default = configuration_field(fragment, name)`: the label stored in the
                // configuration option `name` (e.g. `--crosstool_top`), if any.
                Some((frag, opt)) => {
                    let l = crate::fragments::late_bound_label(frag, opt, cfg);
                    match attr.ty {
                        AttrType::LabelList => AttrValue::LabelList(l.into_iter().collect()),
                        _ => AttrValue::Label(l),
                    }
                }
                None => match &attr.computed_default {
                    Some(f) => computed_default_value(f, attr, name, &raw_attrs).map_err(wrap)?,
                    None => attr.default_value(),
                },
            },
        };
        let av = crate::select::resolve_select(&av, name, rule, cfg, resolver.as_ref()).map_err(wrap)?;
        let conv = attr_to_value(attr, &av, rule, cfg, inputs, pkg_dir, out_root, Some(&raw_attrs), hook).map_err(wrap)?;
        match attr.ty {
            AttrType::Label | AttrType::LabelList | AttrType::LabelKeyedStringDict | AttrType::StringKeyedLabelDict => {
                let files = files_of_deps(&conv.deps);
                files_fields.insert(name.clone(), mk_list(files.iter().map(file_value).collect()));
                if attr.allow_single_file || attr.executable {
                    let single = if files.len() == 1 {
                        Some(files[0].clone())
                    } else if conv.deps.is_empty() || !attr.allow_single_file {
                        // An `executable = True` attribute only needs the dependency's
                        // executable (`ctx.executable`); the single-file requirement is
                        // `allow_single_file`'s.
                        None
                    } else {
                        return Err(wrap(format!(
                            "'{}' produces {} files, but attribute '{name}' of rule {} requires exactly one",
                            conv.deps[0].label.canonical_form(),
                            files.len(),
                            rule.label.canonical_form()
                        )));
                    };
                    if attr.allow_single_file {
                        file_fields.insert(name.clone(), single.as_ref().map(file_value).unwrap_or(Value::None));
                    }
                    if attr.executable {
                        let exe = match conv.deps.first() {
                            None => Value::None,
                            Some(d) => match &d.default_info.executable {
                                Some(e) => file_value(e),
                                // FilesToRunProvider.fromSingleExecutableArtifact: a source file is its
                                // own executable.
                                None if d.rule_class == "source file" && d.default_info.files.len() == 1 => {
                                    file_value(&d.default_info.files[0])
                                }
                                None => {
                                    return Err(wrap(format!(
                                        "'{}' does not have mandatory executable output for attribute '{name}'",
                                        d.label.canonical_form()
                                    )))
                                }
                            },
                        };
                        exe_fields.insert(name.clone(), exe);
                    }
                }
                deps_by_attr.insert(name.clone(), conv.deps.clone());
            }
            AttrType::Output | AttrType::OutputList => {
                outputs_fields.insert(name.clone(), conv.value.clone());
                for v in sequence(&conv.value).unwrap_or_else(|| vec![conv.value.clone()]) {
                    if let Some(a) = artifact_of(&v) {
                        predeclared.push(a);
                    }
                }
            }
            _ => {}
        }
        if let Some(sv) = &conv.split {
            split_fields.insert(name.clone(), sv.clone());
        }
        attr_fields.insert(name.clone(), conv.value);
    }
    Ok(AttrBundle { attr_fields, file_fields, files_fields, exe_fields, outputs_fields, deps_by_attr, predeclared, split_fields })
}

/// Analyses a Starlark-defined rule: runs its `implementation` with a real `ctx`.
pub fn analyze_starlark_rule(
    rule: &RuleInstance,
    cfg: &BuildConfigurationKey,
    inputs: AnalysisInputs,
) -> Result<ConfiguredTarget, String> {
    let rc = rule
        .extra
        .rule_class
        .clone()
        .ok_or_else(|| format!("rule {} has no rule class", rule.label))?;
    let spec: Arc<StarlarkRuleSpec> = rc
        .starlark
        .clone()
        .ok_or_else(|| format!("rule class '{}' is not defined in Starlark", rc.name))?;
    let label = rule.label.clone();
    let prefix = match &rule.extra.location {
        Some(l) => format!("{l}: in {} rule {}: ", rc.name, label.canonical_form()),
        None => format!("in {} rule {}: ", rc.name, label.canonical_form()),
    };
    let wrap = |e: String| {
        if is_contextual_error(&e) {
            return e;
        }
        // Bazel prints the traceback on the line after `in <kind> rule <label>: `, without the
        // `<file>:<line>:<col>: ` position the evaluator puts in front of it.
        const TB: &str = ": Traceback (most recent call last):\n";
        if let Some(i) = e.find(TB) {
            let head = &e[..i];
            if !head.is_empty() && !head.contains(['\n', ' ']) {
                return format!("{prefix}\n{}", &e[i + 2..]);
            }
        }
        format!("{prefix}{e}")
    };
    let pkg_dir = label.package_identifier().package_fragment().clone();
    let output_to_genfiles = spec.kwargs.get("output_to_genfiles").map(|v| v.is_truthy()).unwrap_or(false);
    let out_root = if output_to_genfiles { ArtifactRoot::Genfiles } else { ArtifactRoot::Bin };
    let exec_groups: BTreeSet<String> = match spec.kwargs.get("exec_groups") {
        Some(Value::Dict(d)) => d.lock().unwrap().items().into_iter().filter_map(|(k, _)| k.as_str().map(str::to_string)).collect(),
        _ => BTreeSet::new(),
    };

    // 1. Attributes.
    let AttrBundle {
        attr_fields,
        file_fields,
        files_fields,
        exe_fields,
        mut outputs_fields,
        deps_by_attr,
        mut predeclared,
        split_fields,
    } = convert_rule_attrs(rule, &rc.attrs, cfg, &inputs, &pkg_dir, out_root, &wrap, None)?;
    // `BaseRuleClasses.TestBaseRule`: the hidden test tool attributes are prerequisites (exec
    // configuration) of every test rule; resolving them records the dependency edges (and the
    // transitive packages / actions of the tools) like Bazel's dependency resolution does.
    if rc.is_test {
        let mut implicit: BTreeMap<String, Attribute> = BTreeMap::new();
        for n in [
            "$test_wrapper",
            "$xml_writer",
            "$test_runtime",
            "$test_setup_script",
            "$xml_generator_script",
            "$collect_coverage_script",
            ":coverage_support",
        ] {
            if !rc.attrs.contains_key(n) {
                if let Some(a) = rc.implicit.get(n) {
                    let mut a = a.clone();
                    a.cfg_exec = true;
                    implicit.insert(n.to_string(), a);
                }
            }
        }
        if !implicit.is_empty() {
            let _ = convert_rule_attrs(rule, &implicit, cfg, &inputs, &pkg_dir, out_root, &wrap, None);
        }
    }
    let cc_cov_script: Option<Artifact> = exe_fields.get("_collect_cc_coverage").and_then(artifact_of);
    // Implicit outputs (`rule(outputs = {...})`).
    for (key, tmpl) in &rc.implicit_outputs {
        let fname = expand_output_template(tmpl, rule);
        let a = Artifact::derived_in_root(
            &cfg.mnemonic(),
            out_root,
            pkg_dir.get_relative_str(&fname),
            label.clone(),
        );
        outputs_fields.insert(key.clone(), file_value(&a));
        predeclared.push(a);
    }
    let default_executable = if rc.is_executable || rc.is_test {
        Some(Artifact::derived_in_root(
            &cfg.mnemonic(),
            out_root,
            pkg_dir.get_relative_str(label.name()),
            label.clone(),
        ))
    } else {
        None
    };

    // 2. Make variables.
    let mut make_vars = BTreeMap::new();
    make_vars.insert("COMPILATION_MODE".to_string(), cfg.compilation_mode.clone());
    make_vars.insert("BINDIR".to_string(), format!("bazel-out/{}/bin", cfg.mnemonic()));
    make_vars.insert("GENDIR".to_string(), format!("bazel-out/{}/bin", cfg.mnemonic()));
    make_vars.insert("TARGET_CPU".to_string(), cfg.cpu.clone());
    for (k, v) in &inputs.extra_make_variables {
        make_vars.insert(k.clone(), v.clone());
    }
    // `--define` values are Make variables too.
    for (k, v) in &cfg.user_defines {
        make_vars.insert(k.clone(), v.clone());
    }

    let st = Arc::new(CtxState {
        rule: rule.clone(),
        label: label.clone(),
        cfg: cfg.clone(),
        inputs,
        pkg_dir: pkg_dir.clone(),
        output_to_genfiles,
        exec_groups,
        deps: deps_by_attr,
        attr_struct: Value::Struct(Arc::new(StructValue::new(attr_fields))),
        file_struct: Value::Struct(Arc::new(StructValue::new(file_fields))),
        files_struct: Value::Struct(Arc::new(StructValue::new(files_fields))),
        executable_struct: Value::Struct(Arc::new(StructValue::new(exe_fields))),
        outputs_struct: Value::Struct(Arc::new(StructValue::new(outputs_fields))),
        predeclared,
        default_executable,
        default_executable_accessed: Mutex::new(false),
        actions: Mutex::new(Vec::new()),
        graph: Mutex::new(ActionGraph::new()),
        declared_trees: Mutex::new(BTreeSet::new()),
        make_vars,
        frozen: Mutex::new(false),
        rule_struct: None,
        aspect_ids: Vec::new(),
        aspect_fragments: Vec::new(),
        split_attr: Value::Struct(Arc::new(StructValue::new(split_fields))),
    });

    // 3. Run the implementation function.
    let mut ectx = EvalContext::new(pkg_dir.clone(), None);
    ectx.file_name = rule.extra.location.clone().unwrap_or_else(|| "BUILD".to_string());
    let st2 = st.clone();
    ectx.host = Some(Arc::new(move |c, n, p, k| dispatch(&st2, c, n, p, k)));
    let ret = ectx
        .call_value(spec.implementation.clone(), vec![Value::Host("ctx".into(), "0".into())], BTreeMap::new())
        .map_err(|e| wrap(e.0));
    *st.frozen.lock().unwrap() = true;
    let ret = ret?;

    // 4. Providers.
    finish_target(&st, &rc, rule, cfg, ret)
        .map(|mut ct| {
            ct.collect_cc_coverage = cc_cov_script;
            ct
        })
        .map_err(wrap)
}

/// Evaluates a computed attribute default (`default = fn`): the function's parameters are bound
/// to the rule's (non-label) attribute values, as Bazel's `StarlarkComputedDefault` does.
fn computed_default_value(
    f: &Value,
    attr: &Attribute,
    name: &str,
    raw_attrs: &Value,
) -> Result<AttrValue, String> {
    let Value::UserFunction(def) = f else { return Err(format!("attribute '{name}': invalid computed default")) };
    let mut kw = BTreeMap::new();
    if let Value::Struct(s) = raw_attrs {
        for p in &def.params {
            if let Some(v) = s.fields.get(&p.name) {
                kw.insert(p.name.clone(), v.clone());
            }
        }
    }
    let mut ctx = EvalContext::default();
    let r = ctx
        .call_value(f.clone(), vec![], kw)
        .map_err(|e| format!("in computed default of attribute '{name}': {}", e.0))?;
    let bad = |r: &Value| format!("attribute '{name}': computed default returned {}", type_name(r));
    Ok(match (&attr.ty, &r) {
        (AttrType::Label, Value::None) => AttrValue::Label(None),
        (AttrType::Label, Value::Label(l)) => AttrValue::Label(Some(l.clone())),
        (AttrType::LabelList, v) => {
            let items = sequence(v).ok_or_else(|| bad(&r))?;
            let mut out = Vec::new();
            for i in items {
                match i {
                    Value::Label(l) => out.push(l),
                    _ => return Err(bad(&r)),
                }
            }
            AttrValue::LabelList(out)
        }
        (AttrType::Bool, Value::Bool(b)) => AttrValue::Bool(*b),
        (AttrType::String, Value::String(s)) => AttrValue::Str(s.clone()),
        _ => return Err(bad(&r)),
    })
}

fn provider_key(v: &Value) -> Result<String, String> {
    match v {
        Value::ProviderInstance { provider_name, .. } => {
            // An instance created while its .bzl was still loading carries the placeholder key;
            // the provider has been exported since (module-level instances like
            // _DEFAULT = MyInfo(...)).
            let resolved = zel_packages::provider::resolve_instance_key(provider_name);
            let provider_name = &resolved;
            if provider_name.starts_with("%anonymous#") {
                return Err("The rule implementation function returned an instance of an unnamed provider. A provider becomes named by being assigned to a global variable in a .bzl file.".into());
            }
            Ok(provider_name.clone())
        }
        other => Err(format!(
            "Rule should return a struct or a list, but got {}",
            type_name(other)
        )),
    }
}

fn get_field<'a>(fields: &'a BTreeMap<String, Value>, k: &str) -> Option<&'a Value> {
    match fields.get(k) {
        Some(Value::None) | None => None,
        Some(v) => Some(v),
    }
}

fn finish_target(
    st: &Arc<CtxState>,
    rc: &Arc<zel_packages::RuleClass>,
    rule: &RuleInstance,
    cfg: &BuildConfigurationKey,
    ret: Value,
) -> Result<ConfiguredTarget, String> {
    let label = &st.label;
    let mut declared: BTreeMap<String, Value> = BTreeMap::new();
    match &ret {
        Value::None => {}
        Value::ProviderInstance { .. } => {
            declared.insert(provider_key(&ret)?, ret.clone());
        }
        Value::List(_) | Value::Tuple(_) => {
            for p in sequence(&ret).unwrap() {
                if !matches!(p, Value::ProviderInstance { .. }) {
                    return Err(format!(
                        "expected value of type 'Info' for result of rule implementation function but got {} instead",
                        type_name(&p)
                    ));
                }
                let k = provider_key(&p)?;
                if declared.insert(k.clone(), p).is_some() {
                    return Err(format!("Multiple conflicting returned providers with key {k}"));
                }
            }
        }
        Value::Struct(_) => {
            return Err("Returning a struct from a rule implementation function is deprecated.".to_string())
        }
        other => return Err(format!("Rule should return a struct or a list, but got {}", type_name(other))),
    }

    let analysis_test = rc
        .starlark
        .as_ref()
        .and_then(|s| s.kwargs.get("analysis_test"))
        .map(|v| v.is_truthy())
        .unwrap_or(false);
    let mut analysis_test_exe: Option<Artifact> = None;
    if analysis_test {
        if !st.actions.lock().unwrap().is_empty() {
            return Err(format!(
                "The implementation of analysis test rule '{}' may not register actions; it must return AnalysisTestResultInfo",
                rule.rule_class
            ));
        }
        let Some(Value::ProviderInstance { fields, .. }) = declared.get("AnalysisTestResultInfo") else {
            return Err(format!(
                "The analysis test rule '{}' must return an AnalysisTestResultInfo provider",
                rule.rule_class
            ));
        };
        let success = match fields.get("success") {
            Some(Value::Bool(b)) => *b,
            other => {
                return Err(format!(
                    "in call to AnalysisTestResultInfo(), parameter 'success' got value of type '{}', want 'bool'",
                    other.map(type_name).unwrap_or("NoneType")
                ))
            }
        };
        let message = match fields.get("message") {
            Some(Value::String(m)) => m.clone(),
            other => {
                return Err(format!(
                    "in call to AnalysisTestResultInfo(), parameter 'message' got value of type '{}', want 'string'",
                    other.map(type_name).unwrap_or("NoneType")
                ))
            }
        };
        let exe = st.default_executable.clone().ok_or("analysis test rules must be executable")?;
        let script = if success {
            "#!/bin/sh\nexit 0\n".to_string()
        } else {
            format!("#!/bin/sh\ncat <<'__ZEL_ANALYSIS_TEST_MESSAGE__'\n{message}\n__ZEL_ANALYSIS_TEST_MESSAGE__\nexit 1\n")
        };
        let mut a = Action::new(
            label.clone(),
            "FileWrite",
            ActionKind::FileWrite { content: FileContent::Literal(script), is_executable: true },
        );
        a.progress_message = Some(format!("Creating analysis test script {}", exe.exec_path));
        a.outputs.push(exe.clone());
        st.register(a).map_err(|e| e.0)?;
        analysis_test_exe = Some(exe);
    }
    let default_info = declared.remove("DefaultInfo");
    let run_environment = match declared.get("RunEnvironmentInfo") {
        Some(Value::ProviderInstance { fields, .. }) => {
            let mut env = BTreeMap::new();
            if let Some(Value::Dict(d)) = get_field(fields, "environment") {
                for (k, v) in d.lock().unwrap().items() {
                    match (k, v) {
                        (Value::String(k), Value::String(v)) => {
                            env.insert(k, v);
                        }
                        (k, v) => {
                            return Err(format!(
                                "expected dict of string to string for 'environment' of RunEnvironmentInfo, got {} to {}",
                                type_name(&k),
                                type_name(&v)
                            ))
                        }
                    }
                }
            } else if let Some(other) = get_field(fields, "environment") {
                return Err(format!(
                    "for 'environment' of RunEnvironmentInfo got value of type '{}', want 'dict'",
                    type_name(other)
                ));
            }
            let mut inherited = Vec::new();
            if let Some(v) = get_field(fields, "inherited_environment") {
                for x in sequence(v).ok_or_else(|| {
                    format!("for 'inherited_environment' of RunEnvironmentInfo got value of type '{}', want 'sequence'", type_name(v))
                })? {
                    match x {
                        Value::String(s) => inherited.push(s),
                        other => {
                            return Err(format!(
                                "expected string in 'inherited_environment' of RunEnvironmentInfo, got {}",
                                type_name(&other)
                            ))
                        }
                    }
                }
            }
            Some(crate::RunEnvironment { environment: env, inherited_environment: inherited })
        }
        _ => None,
    };
    let instrumented = match declared.get("InstrumentedFilesInfo") {
        Some(Value::ProviderInstance { fields, .. }) => {
            let files = |k: &str| -> Result<Vec<Artifact>, String> {
                match get_field(fields, k) {
                    Some(v) => files_in(v, k).map_err(|e| e.0),
                    None => Ok(Vec::new()),
                }
            };
            Some(crate::InstrumentedFiles {
                instrumented_files: files("instrumented_files")?,
                metadata_files: files("metadata_files")?,
                environment: match get_field(fields, "coverage_environment") {
                    Some(v @ Value::Dict(_)) => str_dict(v, "coverage_environment").map_err(|e| e.0)?,
                    _ => BTreeMap::new(),
                },
                support_files: files("coverage_support_files")?,
            })
        }
        _ => None,
    };
    let fields = match &default_info {
        Some(Value::ProviderInstance { fields, .. }) => fields.clone(),
        _ => BTreeMap::new(),
    };
    let is_exec_rule = rc.is_executable || rc.is_test;
    let mut executable = match get_field(&fields, "executable") {
        Some(v) => Some(artifact_of(v).ok_or_else(|| {
            format!(
                "in call to DefaultInfo(), parameter 'executable' got value of type '{}', want 'File or NoneType'",
                type_name(v)
            )
        })?),
        None => None,
    };
    if executable.is_none() {
        executable = analysis_test_exe.clone();
    }
    if let Some(e) = &executable {
        if e.owner_label != *label || e.is_source {
            return Err(format!(
                "'executable' provided by an executable rule '{}' should be created by the same rule.",
                rule.rule_class
            ));
        }
    }
    let accessed = *st.default_executable_accessed.lock().unwrap();
    if let (Some(e), true, true) = (&executable, is_exec_rule, accessed) {
        let d = st.default_executable.as_ref().unwrap();
        if e.exec_path != d.exec_path {
            return Err(format!(
                "The rule '{}' both accesses 'ctx.outputs.executable' and provides a different executable '{}'. Do not use 'ctx.output.executable'.",
                rule.rule_class,
                e.root_relative_path
            ));
        }
    }
    if executable.is_none() && is_exec_rule {
        if accessed {
            executable = st.default_executable.clone();
        } else {
            return Err(format!(
                "The rule '{}' is executable. It needs to create an executable File and pass it as the 'executable' parameter to the DefaultInfo it returns.",
                rule.rule_class
            ));
        }
    }

    // files
    let mut files_to_build: Vec<Artifact> = st.predeclared.clone();
    if let Some(e) = &executable {
        files_to_build.push(e.clone());
    }
    if let Some(f) = get_field(&fields, "files") {
        if !matches!(f, Value::Depset(_)) {
            return Err(format!(
                "in call to DefaultInfo(), parameter 'files' got value of type '{}', want 'depset or NoneType'",
                type_name(f)
            ));
        }
        files_to_build = files_in(f, "files").map_err(|e| e.0)?;
    }
    let files_to_build = dedup(files_to_build);

    // runfiles
    let rf = |k: &str| -> Result<Option<Runfiles>, String> {
        match get_field(&fields, k) {
            None => Ok(None),
            Some(v) => runfiles_of(v).map(|r| Some((*r).clone())).ok_or_else(|| {
                format!("in call to DefaultInfo(), parameter '{k}' got value of type '{}', want 'runfiles or NoneType'", type_name(v))
            }),
        }
    };
    let mut stateless = rf("runfiles")?;
    let data_rf = rf("data_runfiles")?;
    let default_rf = rf("default_runfiles")?;
    if stateless.is_some() && (data_rf.is_some() || default_rf.is_some()) {
        return Err("Cannot specify the provider 'runfiles' together with 'data_runfiles' or 'default_runfiles'".to_string());
    }
    if stateless.is_none() && data_rf.is_none() && default_rf.is_none() {
        stateless = Some(Runfiles::default());
    }
    let with_exe = |r: &Runfiles| -> Runfiles {
        match &executable {
            None => r.clone(),
            Some(e) => {
                // `Builder.addArtifact(executable).merge(runfiles)`: the executable is a direct
                // element next to the merged set.
                let mut m = Runfiles { artifacts: vec![e.clone()], ..Default::default() }.merge(r);
                m.nested = Runfiles::build_nested(std::slice::from_ref(e), vec![r.nested_set()]);
                m
            }
        }
    };
    let is_exec_or_test = executable.is_some() || rc.is_test;
    let (default_runfiles, data_runfiles) = match stateless {
        Some(s) => {
            let m = with_exe(&s);
            (m.clone(), m)
        }
        None => {
            let mut d = default_rf.unwrap_or_default();
            if is_exec_or_test {
                d = with_exe(&d);
            }
            (d, data_rf.unwrap_or_default())
        }
    };
    if rc.is_test && default_runfiles.is_empty() {
        return Err("Test rules have to define runfiles".to_string());
    }

    let mut extra_actions: Vec<Action> = Vec::new();
    let mut runfiles_support = None;
    if is_exec_or_test && !default_runfiles.is_empty() {
        let exe = executable.clone().ok_or("executable must not be null")?;
        let manifest = Artifact::derived_in_root(
            &cfg.mnemonic(),
            if st.output_to_genfiles { ArtifactRoot::Genfiles } else { ArtifactRoot::Bin },
            PathFragment::create(&format!("{}.runfiles_manifest", exe.root_relative_path)),
            label.clone(),
        );
        let mut a = Action::new(
            label.clone(),
            "SourceSymlinkManifest",
            ActionKind::FileWrite {
                content: FileContent::Literal(default_runfiles.manifest_text(&st.inputs.workspace_name)),
                is_executable: false,
            },
        );
        a.progress_message = Some(format!("Creating source manifest for {}", label.canonical_form()));
        a.inputs = default_runfiles.artifacts.clone();
        a.outputs.push(manifest.clone());
        st.register(a.clone()).map_err(|e| e.0)?;
        let _ = &mut extra_actions;
        // `RunfilesSupport.createRepoMappingManifestAction`: <executable>.repo_mapping in bin.
        let repo_mapping = st.repo_mapping_for(&default_runfiles);
        let rm_artifact = Artifact::derived_in_root(
            &cfg.mnemonic(),
            if st.output_to_genfiles { ArtifactRoot::Genfiles } else { ArtifactRoot::Bin },
            PathFragment::create(&format!("{}.repo_mapping", exe.root_relative_path)),
            label.clone(),
        );
        let mut rma = Action::new(
            label.clone(),
            "RepoMappingManifest",
            ActionKind::FileWrite {
                content: FileContent::Literal(crate::repo_mapping::repo_mapping_text(&repo_mapping)),
                is_executable: false,
            },
        );
        rma.progress_message = Some(format!("Writing repo mapping manifest for {}", label.canonical_form()));
        rma.outputs.push(rm_artifact.clone());
        st.register(rma).map_err(|e| e.0)?;
        runfiles_support = Some(crate::RunfilesSupport {
            repo_mapping,
            repo_mapping_manifest: Some(rm_artifact),
            runfiles: default_runfiles.clone(),
            runfiles_dir: format!("{}.runfiles", exe.exec_path),
            executable: exe,
            manifest,
            workspace_name: st.inputs.workspace_name.clone(),
        });
    }

    let files_to_run = provider_instance(
        "FilesToRunProvider",
        vec![
            ("executable", executable.as_ref().map(file_value).unwrap_or(Value::None)),
            (
                "runfiles_manifest",
                runfiles_support.as_ref().map(|r| file_value(&r.manifest)).unwrap_or(Value::None),
            ),
            (
                "repo_mapping_manifest",
                runfiles_support.as_ref().and_then(|r| r.repo_mapping_manifest.as_ref()).map(file_value).unwrap_or(Value::None),
            ),
            ("_runfiles", runfiles_value(default_runfiles.clone())),
        ],
    );
    let default_info_value = provider_instance(
        "DefaultInfo",
        vec![
            ("files", files_depset(&files_to_build)),
            ("files_to_run", files_to_run),
            ("data_runfiles", runfiles_value(data_runfiles.clone())),
            ("default_runfiles", runfiles_value(default_runfiles.clone())),
        ],
    );

    // Output groups.
    let mut groups: BTreeMap<String, Vec<Artifact>> = BTreeMap::new();
    if let Some(Value::ProviderInstance { fields, .. }) = declared.get("OutputGroupInfo") {
        for (g, v) in fields {
            let files = match v {
                Value::Depset(_) | Value::List(_) | Value::Tuple(_) => files_in(v, &format!("output group '{g}'")).map_err(|e| e.0)?,
                other => {
                    return Err(format!(
                        "expected value of type 'sequence or depset' for output group '{g}' but got {} instead",
                        type_name(other)
                    ))
                }
            };
            groups.insert(g.clone(), files);
        }
    }
    groups.entry("default".to_string()).or_insert_with(|| files_to_build.clone());

    // `provides`
    if let Some(p) = rc.starlark.as_ref().and_then(|s| s.kwargs.get("provides")) {
        for item in sequence(p).unwrap_or_default() {
            if let Some(k) = zel_packages::provider::provider_key_of(&item) {
                if k != "DefaultInfo" && !declared.contains_key(&k) {
                    return Err(format!(
                        "rule advertised the '{}' provider, but this provider was not among those returned",
                        zel_packages::provider::provider_display_name(&k)
                    ));
                }
            }
        }
    }

    // Conflict-checked actions → both models.
    let typed: Vec<Action> = st.actions.lock().unwrap().clone();
    let mut lowered = Vec::new();
    for a in &typed {
        lowered.extend(a.lower());
    }
    let mut providers = declared;
    if rc.build_setting {
        let v = build_setting_value(st).map_err(|e| e.0)?;
        providers.insert("BuildSettingProvider".to_string(), provider_instance("BuildSettingProvider", vec![("value", v)]));
    }
    providers.insert("DefaultInfo".to_string(), default_info_value);
    if !providers.contains_key("OutputGroupInfo") {
        providers.insert("OutputGroupInfo".to_string(), provider_instance("OutputGroupInfo", vec![]));
    }

    let mut ct = ConfiguredTarget::new(label.clone(), cfg.clone(), rule.rule_class.clone());
    ct.default_info = DefaultInfo {
        files: files_to_build,
        data_runfiles: data_runfiles.artifacts.clone(),
        default_runfiles: default_runfiles.artifacts.clone(),
        executable,
    };
    ct.output_groups = OutputGroupInfo { groups };
    ct.actions = lowered;
    ct.typed_actions = typed;
    ct.providers = providers;
    ct.runfiles_support = runfiles_support;
    ct.run_environment = run_environment;
    ct.instrumented_files = instrumented;
    ct.testonly = rule.testonly;
    ct.is_test = rc.is_test;
    Ok(ct)
}

/// Builds the `DefaultInfo` provider instance for a native rule.
pub fn default_info_provider(
    files: &[Artifact],
    default_runfiles: Runfiles,
    data_runfiles: Runfiles,
    executable: Option<Artifact>,
) -> Value {
    let files_to_run = provider_instance(
        "FilesToRunProvider",
        vec![
            ("executable", executable.as_ref().map(file_value).unwrap_or(Value::None)),
            ("runfiles_manifest", Value::None),
            ("repo_mapping_manifest", Value::None),
            ("_runfiles", runfiles_value(default_runfiles.clone())),
        ],
    );
    provider_instance(
        "DefaultInfo",
        vec![
            ("files", files_depset(files)),
            ("files_to_run", files_to_run),
            ("data_runfiles", runfiles_value(data_runfiles)),
            ("default_runfiles", runfiles_value(default_runfiles)),
        ],
    )
}
