
// ------------------------------------------------------------------------------------------
// Methods

fn host_method(
    st: &Arc<CtxState>,
    ctx: &mut EvalContext,
    kind: &str,
    method: &str,
    recv: &Value,
    pos: &[Value],
    kw: &BTreeMap<String, Value>,
) -> HostResult {
    match kind {
        "actions" => actions_method(st, ctx, method, pos, kw),
        "ctx" => ctx_method(st, ctx, method, pos, kw),
        "constraint_collection" => {
            let cc = crate::constraint_collection_of(recv)
                .ok_or_else(|| StarlarkError("invalid ConstraintCollection".into()))?;
            match method {
                "get" => {
                    check_kwargs("get", pos, kw, &["constraint"])?;
                    let c = arg(pos, kw, 0, "constraint").ok_or_else(|| {
                        StarlarkError("get() missing 1 required positional argument: constraint".into())
                    })?;
                    crate::constraint_collection_get(&cc, c).map_err(StarlarkError)
                }
                "has" => {
                    check_kwargs("has", pos, kw, &["constraint"])?;
                    let c = arg(pos, kw, 0, "constraint").ok_or_else(|| {
                        StarlarkError("has() missing 1 required positional argument: constraint".into())
                    })?;
                    crate::constraint_collection_has(&cc, c)
                        .map(Value::Bool)
                        .map_err(StarlarkError)
                }
                "has_constraint_value" => {
                    check_kwargs("has_constraint_value", pos, kw, &["constraint_value"])?;
                    let cv = arg(pos, kw, 0, "constraint_value").ok_or_else(|| {
                        StarlarkError(
                            "has_constraint_value() missing 1 required positional argument: constraint_value".into(),
                        )
                    })?;
                    crate::constraint_collection_has_value(&cc, cv)
                        .map(Value::Bool)
                        .map_err(StarlarkError)
                }
                other => err(format!("'ConstraintCollection' value has no field or method '{other}'")),
            }
        }
        "args" => args_host_method(recv, ctx, method, pos, kw),
        "runfiles" => {
            let r = runfiles_of(recv).ok_or_else(|| StarlarkError("invalid runfiles".into()))?;
            match method {
                "merge" => {
                    check_kwargs("merge", pos, kw, &["other"])?;
                    let o = arg(pos, kw, 0, "other")
                        .and_then(runfiles_of)
                        .ok_or_else(|| StarlarkError("in call to merge(), parameter 'other' must be runfiles".into()))?;
                    Ok(runfiles_value(r.merge(&o)))
                }
                "merge_all" => {
                    check_kwargs("merge_all", pos, kw, &["other"])?;
                    let items = arg(pos, kw, 0, "other").and_then(sequence).ok_or_else(|| {
                        StarlarkError("in call to merge_all(), parameter 'other' must be a sequence or depset of runfiles".into())
                    })?;
                    let mut out = (*r).clone();
                    for i in items {
                        let o = runfiles_of(&i).ok_or_else(|| {
                            StarlarkError(format!(
                                "expected value of type 'runfiles' for a member of parameter 'other' but got {} instead",
                                type_name(&i)
                            ))
                        })?;
                        out = out.merge(&o);
                    }
                    Ok(runfiles_value(out))
                }
                other => err(format!("'runfiles' value has no field or method '{other}'")),
            }
        }
        "template_dict" => {
            let Value::Host(k, id) = recv else {
                return err("invalid TemplateDict");
            };
            if k != "template_dict" || id.is_empty() {
                return err("invalid TemplateDict");
            }
            let d = template_dict_of(recv).ok_or_else(|| StarlarkError("invalid TemplateDict".into()))?;
            let key = want_str(
                arg(pos, kw, 0, "key").ok_or_else(|| StarlarkError(format!("{method}() missing 1 required positional argument: key")))?,
                method,
                "key",
            )?;
            match method {
                "add" => {
                    check_kwargs("add", pos, kw, &["key", "value"])?;
                    let v = want_str(arg(pos, kw, 1, "value").ok_or_else(|| StarlarkError("add() missing 1 required positional argument: value".into()))?, "add", "value")?;
                    d.lock().unwrap().push(crate::objects::TemplateDictEntry::Literal(key, v));
                    Ok(recv.clone())
                }
                "add_joined" => {
                    if pos.len() > 2 {
                        return err(format!(
                            "add_joined() accepts no more than 2 positional arguments but got {}",
                            pos.len()
                        ));
                    }
                    check_kwargs("add_joined", pos, kw, &["key", "values", "join_with", "map_each", "uniquify", "format_joined", "allow_closure"])?;
                    let values = arg(pos, kw, 1, "values")
                        .cloned()
                        .ok_or_else(|| StarlarkError("add_joined() missing 1 required positional argument: values".into()))?;
                    if !matches!(values, Value::Depset(_)) {
                        return err(format!(
                            "in call to add_joined(), parameter 'values' got value of type '{}', want 'depset'",
                            type_name(&values)
                        ));
                    }
                    let _ = want_str(
                        kw.get("join_with").ok_or_else(|| StarlarkError("add_joined() missing 1 required named-only argument: 'join_with'".into()))?,
                        "add_joined",
                        "join_with",
                    )?;
                    let Some(map_fn) = kw.get("map_each") else {
                        return err("add_joined() missing 1 required named-only argument: 'map_each'");
                    };
                    if let Value::UserFunction(u) = map_fn {
                        let req_pos = u
                            .params
                            .iter()
                            .filter(|p| !p.is_args && !p.is_kwargs && !p.is_kw_only && p.default.is_none())
                            .count();
                        if req_pos > 1 {
                            return err("map_each must be a function accepting a single positional argument");
                        }
                    }
                    if let Some(fj) = none_or(kw.get("format_joined")) {
                        let s = want_str(fj, "add_joined", "format_joined")?;
                        crate::args::validate_format_str("format_joined", &s)?;
                    }
                    let mut akw = kw.clone();
                    akw.remove("key");
                    akw.remove("values");
                    akw.insert("omit_if_empty".into(), Value::Bool(false));
                    d.lock().unwrap().push(crate::objects::TemplateDictEntry::Joined {
                        key,
                        values,
                        kw: akw,
                    });
                    Ok(recv.clone())
                }
                other => err(format!("'TemplateDict' value has no field or method '{other}'")),
            }
        }
        "template_ctx" => template_ctx_method(st, ctx, method, pos, kw),
        "configuration" => match method {
            "is_tool_configuration" => Ok(Value::Bool(st.cfg.is_exec)),
            "is_sibling_repository_layout" => {
                check_kwargs("is_sibling_repository_layout", pos, kw, &[])?;
                Ok(Value::Bool(st.inputs.sibling_repository_layout))
            }
            // `--stamp` defaults to false for `bazel build`.
            "stamp_binaries" => Ok(Value::Bool(
                st.cfg
                    .options
                    .get("stamp")
                    .is_some_and(|v| parse_bazel_bool(v) == Some(true))
                    && !st.cfg.is_exec,
            )),
            "runfiles_enabled" => Ok(Value::Bool(st.cfg.env.runfiles_enabled)),
            // Genfiles are merged into bin in this implementation.
            "has_separate_genfiles_directory" => Ok(Value::Bool(false)),
            "disabled_features" => {
                check_kwargs("disabled_features", pos, kw, &[])?;
                let fs = FeatureSet::parse(&st.cfg.global_features());
                Ok(mk_list(fs.off.into_iter().map(Value::String).collect()))
            }
            other => err(format!("'configuration' value has no field or method '{other}'")),
        },
        "toolchains" => match method {
            "toolchain_types" => {
                check_kwargs("toolchain_types", pos, kw, &[])?;
                if !has_toolchain_context(st) {
                    return Ok(mk_list(vec![]));
                }
                let list: Vec<Value> = st
                    .inputs
                    .toolchains
                    .keys()
                    .filter_map(|k| Label::parse_canonical(k).ok())
                    .map(Value::Label)
                    .collect();
                Ok(mk_list(list))
            }
            other => err(format!("'toolchains' value has no field or method '{other}'")),
        },
        "exec_group_toolchains" => match method {
            "toolchain_types" => {
                check_kwargs("toolchain_types", pos, kw, &[])?;
                let Value::Host(_, group) = recv else { return err("invalid exec group toolchains") };
                let tcs = st.inputs.exec_group_toolchains.get(group);
                let list: Vec<Value> = tcs
                    .into_iter()
                    .flat_map(|m| m.keys())
                    .filter_map(|k| Label::parse_canonical(k).ok())
                    .map(Value::Label)
                    .collect();
                Ok(mk_list(list))
            }
            other => err(format!("'exec_group_toolchains' value has no field or method '{other}'")),
        },
        "rule_toolchains" => match method {
            "toolchain_types" => {
                check_kwargs("toolchain_types", pos, kw, &[])?;
                let list: Vec<Value> = st
                    .base_toolchains
                    .keys()
                    .filter_map(|k| Label::parse_canonical(k).ok())
                    .map(Value::Label)
                    .collect();
                Ok(mk_list(list))
            }
            other => err(format!("'toolchains' value has no field or method '{other}'")),
        },
        "rule_exec_group_toolchains" => match method {
            "toolchain_types" => {
                check_kwargs("toolchain_types", pos, kw, &[])?;
                let Value::Host(_, group) = recv else { return err("invalid exec group toolchains") };
                let tcs = st.base_exec_group_toolchains.get(group);
                let list: Vec<Value> = tcs
                    .into_iter()
                    .flat_map(|m| m.keys())
                    .filter_map(|k| Label::parse_canonical(k).ok())
                    .map(Value::Label)
                    .collect();
                Ok(mk_list(list))
            }
            other => err(format!("'exec_group_toolchains' value has no field or method '{other}'")),
        },
        other => err(format!("'{other}' value has no field or method '{method}'")),
    }
}

/// A depset as the `NestedSet` it is in Bazel (elements: `(exec path, owner repository)` of files).
fn depset_nested(
    d: &Arc<zel_starlark::Depset>,
    memo: &mut std::collections::HashMap<u64, crate::nested_set::NSet<(String, String)>>,
) -> crate::nested_set::NSet<(String, String)> {
    use crate::nested_set::{
        NSet, COMPILE_ORDER, LINK_ORDER, NAIVE_LINK_ORDER, STABLE_ORDER,
    };
    if let Some(s) = memo.get(&d.id) {
        return s.clone();
    }
    let order = match d.order {
        zel_starlark::DepsetOrder::Default => STABLE_ORDER,
        zel_starlark::DepsetOrder::Postorder => COMPILE_ORDER,
        zel_starlark::DepsetOrder::Topological => LINK_ORDER,
        zel_starlark::DepsetOrder::Preorder => NAIVE_LINK_ORDER,
    };
    let mut direct: Vec<(String, String)> = d
        .direct
        .iter()
        .filter_map(artifact_of)
        .map(|a| Runfiles::elem(&a))
        .collect();
    let mut trans: Vec<_> = d.transitive.iter().map(|t| depset_nested(t, memo)).collect();
    // `Depset` reverses `direct` and `transitive` at construction when `Topological`; undo so
    // `NSet::build(LINK_ORDER, ...)` reverses once.
    if d.order == zel_starlark::DepsetOrder::Topological {
        direct.reverse();
        trans.reverse();
    }
    let s = NSet::build(order, direct, trans);
    memo.insert(d.id, s.clone());
    s
}

fn collect_attr_runfiles(st: &Arc<CtxState>, data: bool) -> Runfiles {
    let mut out = Runfiles::default();
    let always_files_in_data = st
        .cfg
        .bool_option("incompatible_always_include_files_in_data", true);
    for attr in ["data", "srcs", "deps"] {
        let is_data_attr = attr == "data";
        if let Some(ts) = st.deps.get(attr) {
            for t in ts {
                if !is_data_attr && (t.rule_class == "source file" || t.rule_class == "generated file") {
                    continue;
                }
                if is_data_attr && always_files_in_data {
                    out = out.merge(&Runfiles {
                        artifacts: t.default_info.files.clone(),
                        ..Default::default()
                    });
                }
                out = out.merge(&target_runfiles(t, is_data_attr || data));
            }
        }
    }
    out
}

/// Runfiles contributed by a dependency (`Runfiles.Builder.addTarget`): its `data_runfiles` /
/// `default_runfiles`, or, for targets without runfiles (plain files), the files themselves.
pub fn target_runfiles(t: &ConfiguredTarget, data: bool) -> Runfiles {
    // `Runfiles.Builder.addTargetIncludingFileTargets`: file targets (source/generated files)
    // have no `RunfilesProvider` in Bazel and only contribute their `files` to *data* runfiles.
    if t.rule_class == "source file" || t.rule_class == "generated file" {
        return if data {
            Runfiles { artifacts: t.default_info.files.clone(), ..Default::default() }
        } else {
            Runfiles::default()
        };
    }
    let mut rf = if let Some(Value::ProviderInstance { fields, .. }) = t.providers.get("DefaultInfo") {
        let key = if data { "data_runfiles" } else { "default_runfiles" };
        if let Some(r) = fields.get(key).and_then(runfiles_of) {
            (*r).clone()
        } else {
            Runfiles { artifacts: t.default_info.files.clone(), ..Default::default() }
        }
    } else {
        Runfiles { artifacts: t.default_info.files.clone(), ..Default::default() }
    };
    if data {
        // `--incompatible_always_include_files_in_data` (default true in Bazel 9):
        // targets in `data` also contribute their `DefaultInfo.files`.
        rf = rf.merge(&Runfiles { artifacts: t.default_info.files.clone(), ..Default::default() });
    }
    rf
}

fn repo_mapping_for_target(st: &CtxState) -> zel_cmdline::RepositoryMapping {
    let owner = st.label.repository().clone();
    let entries = st
        .inputs
        .resolver
        .repo_mapping_entries(owner.name())
        .into_iter()
        .map(|(a, c)| (a, zel_cmdline::RepositoryName::create_unvalidated(c)))
        .collect();
    zel_cmdline::RepositoryMapping::new(entries, owner)
}

fn insert_target_location_keys(map: &mut BTreeMap<Label, Vec<Artifact>>, t: &ConfiguredTarget, files: Vec<Artifact>) {
    map.entry(t.label.clone()).or_default().extend(files.iter().cloned());
    for al in &t.alias_targets {
        map.entry(al.clone()).or_default().extend(files.iter().cloned());
    }
}

fn ctx_method(
    st: &Arc<CtxState>,
    _ctx: &mut EvalContext,
    method: &str,
    pos: &[Value],
    kw: &BTreeMap<String, Value>,
) -> HostResult {
    match method {
        // `StarlarkRuleContext.tokenize`: ShellUtils.tokenize.
        "tokenize" => {
            st.check_mutable("tokenize")?;
            check_kwargs("tokenize", pos, kw, &["option"])?;
            let v = arg(pos, kw, 0, "option").ok_or_else(|| StarlarkError("tokenize() missing 1 required positional argument: option".into()))?;
            let s = want_str(v, "tokenize", "option")?;
            shell_tokenize(&s)
                .map(|t| mk_list(t.into_iter().map(Value::String).collect()))
                .map_err(|e| StarlarkError(format!("{e} while tokenizing '{s}'")))
        }
        "target_platform_has_constraint" => {
            check_kwargs("target_platform_has_constraint", pos, kw, &["constraint_value"])?;
            let cv = arg(pos, kw, 0, "constraint_value")
                .ok_or_else(|| StarlarkError("target_platform_has_constraint() missing 1 required positional argument: constraint_value".into()))?;
            // A `ConstraintValueInfo` provider instance (`ctx.attr.cv[platform_common.ConstraintValueInfo]`).
            let label = match cv {
                Value::ProviderInstance { fields, .. } => match fields.get("label") {
                    Some(Value::Label(l)) => l.clone(),
                    _ => return err("in call to target_platform_has_constraint(), parameter 'constraint_value' is not a ConstraintValueInfo"),
                },
                Value::Label(l) => l.clone(),
                other => {
                    return err(format!(
                        "in call to target_platform_has_constraint(), parameter 'constraint_value' got value of type '{}', want 'ConstraintValueInfo'",
                        type_name(other)
                    ))
                }
            };
            st.inputs.resolver.target_platform_has_constraint(&st.cfg, &label).map(Value::Bool).map_err(StarlarkError)
        }
        "runfiles" => {
            st.check_mutable("runfiles")?;
            check_kwargs(
                "runfiles",
                pos,
                kw,
                &["files", "transitive_files", "collect_data", "collect_default", "symlinks", "root_symlinks", "skip_conflict_checking"],
            )?;
            let skip_conflict_checking = arg(pos, kw, 6, "skip_conflict_checking").map(|v| v.is_truthy()).unwrap_or(false);
            let mut out = Runfiles::default();
            let mut transitive_sets: Vec<crate::nested_set::NSet<(String, String)>> = Vec::new();
            let mut collected: Vec<Runfiles> = Vec::new();
            // `StarlarkRuleContext.runfiles`: collect_data, then collect_default, then files,
            // then transitive_files, then symlinks, then root_symlinks.
            for (idx, data) in [(2usize, true), (3, false)] {
                if arg(pos, kw, idx, if data { "collect_data" } else { "collect_default" }).map(|v| v.is_truthy()).unwrap_or(false) {
                    out = out.merge(&collect_attr_runfiles(st, data));
                    let always_files_in_data = st
                        .cfg
                        .bool_option("incompatible_always_include_files_in_data", true);
                    for attr in ["data", "srcs", "deps"] {
                        let is_data_attr = attr == "data";
                        if let Some(ts) = st.deps.get(attr) {
                            for t in ts {
                                if is_data_attr && always_files_in_data {
                                    collected.push(Runfiles {
                                        artifacts: t.default_info.files.clone(),
                                        ..Default::default()
                                    });
                                }
                                collected.push(target_runfiles(t, is_data_attr || data));
                            }
                        }
                    }
                }
            }
            for c in &collected {
                transitive_sets.push(c.nested_set());
            }
            let explicit: Vec<Artifact> = match arg(pos, kw, 0, "files") {
                Some(f) => files_in(f, "files")?,
                None => Vec::new(),
            };
            if !explicit.is_empty() {
                out = out.merge(&Runfiles { artifacts: explicit.clone(), ..Default::default() });
            }
            if let Some(t) = none_or(arg(pos, kw, 1, "transitive_files")) {
                let Value::Depset(d) = t else {
                    return err(format!(
                        "in call to runfiles(), parameter 'transitive_files' got value of type '{}', want 'depset or NoneType'",
                        type_name(t)
                    ));
                };
                if !matches!(d.order, DepsetOrder::Default | DepsetOrder::Postorder) {
                    return err(format!(
                        "order '{}' is invalid for transitive_files",
                        d.order.as_str()
                    ));
                }
                let trans_files = files_in(t, "transitive_files")?;
                if !trans_files.is_empty() {
                    out = out.merge(&Runfiles { artifacts: trans_files, ..Default::default() });
                }
                let mut memo = std::collections::HashMap::new();
                transitive_sets.push(depset_nested(d, &mut memo));
            }
            let mut has_symlinks = false;
            for (idx, name, root) in [(4usize, "symlinks", false), (5, "root_symlinks", true)] {
                if let Some(s) = none_or(arg(pos, kw, idx, name)) {
                    let (target, entries_vec) = if root {
                        (&mut out.root_symlinks, &mut out.root_symlink_entries)
                    } else {
                        (&mut out.symlinks, &mut out.symlink_entries)
                    };
                    match s {
                        Value::Dict(d) => {
                            let items = d.lock().unwrap().items();
                            if !items.is_empty() {
                                has_symlinks = true;
                            }
                            for (k, v) in items {
                                let Value::String(k) = k else { return err(format!("{name} keys must be strings")) };
                                let af = want_file(&v, "runfiles", name)?;
                                entries_vec.push((k.clone(), af.clone()));
                                target.insert(k, af);
                            }
                        }
                        other => {
                            let entries = sequence(other).unwrap_or_default();
                            if !entries.is_empty() {
                                has_symlinks = true;
                            }
                            for e in entries {
                                let Value::Struct(sv) = &e else {
                                    return err(format!("expected a dict or a depset of SymlinkEntry for {name}"));
                                };
                                let p = sv.get("path")?;
                                let f = sv.get("target_file")?;
                                let k = want_str(&p, "runfiles", name)?;
                                let af = want_file(&f, "runfiles", name)?;
                                entries_vec.push((k.clone(), af.clone()));
                                target.insert(k, af);
                            }
                        }
                    }
                }
            }
            if has_symlinks && !skip_conflict_checking {
                out.conflict_policy_error = true;
            }
            out.nested = Runfiles::build_nested(&explicit, transitive_sets);
            Ok(runfiles_value(out))
        }
        "expand_location" => {
            st.check_mutable("expand_location")?;
            check_kwargs("expand_location", pos, kw, &["input", "targets", "short_paths"])?;
            let input = want_str(
                arg(pos, kw, 0, "input").ok_or_else(|| StarlarkError("expand_location() missing 1 required positional argument: input".into()))?,
                "expand_location",
                "input",
            )?;
            // `StarlarkRuleContext.makeLabelMap`: only `targets` (default []), rejects duplicates,
            // follows aliases, and under `--incompatible_locations_prefers_executable` prefers the
            // executable when `files_to_build` is not a singleton.
            let prefers_exe = st.cfg.bool_option("incompatible_locations_prefers_executable", true);
            let mut map: BTreeMap<Label, Vec<Artifact>> = BTreeMap::new();
            if let Some(t) = none_or(arg(pos, kw, 1, "targets")) {
                let items = sequence(t).ok_or_else(|| {
                    StarlarkError(format!(
                        "in call to expand_location(), parameter 'targets' got value of type '{}', want 'sequence'",
                        type_name(t)
                    ))
                })?;
                for item in items {
                    let ct = target_of(&item).ok_or_else(|| {
                        StarlarkError(format!(
                            "expected value of type 'Target' for a member of parameter 'targets' but got {} instead",
                            type_name(&item)
                        ))
                    })?;
                    let key_label = ct.alias_chain.first().cloned().unwrap_or_else(|| ct.label.clone());
                    if map.contains_key(&key_label) {
                        return err(format!("Label '{}' is found more than once in 'targets' list.", key_label.canonical_form()));
                    }
                    let files = target_files(&ct);
                    let exe = ct.default_info.executable.clone();
                    let expansion = if prefers_exe && exe.is_some() && files.len() != 1 {
                        vec![exe.unwrap()]
                    } else {
                        files
                    };
                    map.insert(key_label, expansion);
                }
            }
            // `short_paths` (keyword-only, undocumented): runfiles paths instead of exec paths.
            let mode = if arg(pos, kw, usize::MAX, "short_paths").map(|v| v.is_truthy()).unwrap_or(false) {
                LocationMode::Runfiles
            } else {
                LocationMode::Exec
            };
            let repo_map = st.repo_mapping();
            expand_locations_with_mapping(&input, &map, &st.label, mode, &st.inputs.workspace_name, Some(&repo_map))
                .map(Value::String)
                .map_err(StarlarkError)
        }
        "expand_make_variables" => {
            st.check_mutable("expand_make_variables")?;
            check_kwargs("expand_make_variables", pos, kw, &["attribute_name", "command", "additional_substitutions"])?;
            let attr_name = want_str(arg(pos, kw, 0, "attribute_name").ok_or_else(|| StarlarkError("expand_make_variables() missing argument: attribute_name".into()))?, "expand_make_variables", "attribute_name")?;
            let command = want_str(arg(pos, kw, 1, "command").ok_or_else(|| StarlarkError("expand_make_variables() missing argument: command".into()))?, "expand_make_variables", "command")?;
            let extra = match arg(pos, kw, 2, "additional_substitutions") {
                Some(v) => str_dict(v, "additional_substitutions")?,
                None => BTreeMap::new(),
            };
            let lookup = |n: &str| extra.get(n).cloned().or_else(|| st.make_vars.get(n).cloned());
            match expand_make_variables(&command, &lookup) {
                Ok(s) => Ok(Value::String(s)),
                Err(e) => {
                    st.rule_errors.lock().unwrap().push(format!("in {attr_name} attribute of {} rule {}: {e}", st.rule.rule_class, st.label.canonical_form()));
                    Ok(Value::String(command))
                }
            }
        }
        // `StarlarkRuleContext.resolveCommand`: expands `$(location)` / make variables in a shell
        // command and returns `(inputs, [shell, "-c", command], input_manifests)`.
        "resolve_command" => {
            st.check_mutable("resolve_command")?;
            check_kwargs(
                "resolve_command",
                pos,
                kw,
                &["command", "attribute", "expand_locations", "make_variables", "tools", "label_dict", "execution_requirements"],
            )?;
            let command = match arg(pos, kw, 0, "command") {
                Some(v) => want_str(v, "resolve_command", "command")?,
                None => String::new(),
            };
            let attr_name = match arg(pos, kw, 1, "attribute") {
                Some(Value::None) | None => None,
                Some(v) => Some(want_str(v, "resolve_command", "attribute")?),
            };
            let expand_loc = arg(pos, kw, 2, "expand_locations").map(|v| v.is_truthy()).unwrap_or(false);
            let make_vars = match arg(pos, kw, 3, "make_variables") {
                Some(Value::None) | None => None,
                Some(v) => Some(str_dict(v, "make_variables")?),
            };
            let mut map: BTreeMap<Label, Vec<Artifact>> = BTreeMap::new();
            let mut inputs: Vec<Artifact> = Vec::new();
            if let Some(Value::Dict(d)) = arg(pos, kw, 5, "label_dict") {
                for (k, v) in d.lock().unwrap().entries.iter() {
                    let Value::Label(key_label) = k else {
                        return err(format!("invalid key {} in 'label_dict'", k.repr()));
                    };
                    let items = sequence(v).ok_or_else(|| {
                        StarlarkError(format!("invalid value {} in 'label_dict': expected iterable, but got '{}'", v.repr(), type_name(v)))
                    })?;
                    let mut files = Vec::new();
                    for f in items {
                        let a = artifact_of(&f).ok_or_else(|| StarlarkError(format!("invalid value {} in 'label_dict'", v.repr())))?;
                        files.push(a);
                    }
                    map.entry(key_label.clone()).or_default().extend(files);
                }
            }
            for t in arg(pos, kw, 4, "tools").and_then(sequence).unwrap_or_default() {
                let ct = target_of(&t).ok_or_else(|| StarlarkError("resolve_command: tools must be targets".into()))?;
                // `CommandHelper`: skip tool deps without `FilesToRunProvider`.
                let Some(Value::ProviderInstance { fields, .. }) = ct.providers.get("DefaultInfo").and_then(|d| match d {
                    Value::ProviderInstance { fields, .. } => fields.get("files_to_run"),
                    _ => None,
                }) else {
                    continue;
                };
                let files_to_build = target_files(&ct);
                inputs.extend(files_to_build.iter().cloned());
                let key_label = ct.dependency_label().clone();
                if let Some(exe) = fields.get("executable").and_then(artifact_of) {
                    map.entry(key_label).or_default().push(exe.clone());
                    if let Some(rs) = &ct.runfiles_support {
                        inputs.push(rs.manifest.clone());
                        inputs.extend(rs.runfiles.artifacts.iter().cloned());
                        inputs.push(rs.executable.clone());
                    } else if let Some(r) = fields.get("_runfiles").and_then(runfiles_of) {
                        inputs.extend(r.artifacts.iter().cloned());
                    }
                } else {
                    map.entry(key_label).or_default().extend(files_to_build);
                }
            }
            let mut cmd = command;
            if expand_loc {
                let repo_map = st.repo_mapping();
                let attr_s = attr_name.as_deref().unwrap_or("command");
                cmd = expand_locations_with_mapping(&cmd, &map, &st.label, LocationMode::Exec, &st.inputs.workspace_name, Some(&repo_map))
                    .map_err(|e| StarlarkError(format!("attribute {attr_s}: {e}")))?;
            }
            if let Some(extra) = make_vars {
                let attr_s = attr_name.as_deref().unwrap_or("command");
                let lookup = |n: &str| extra.get(n).cloned().or_else(|| st.make_vars.get(n).cloned());
                cmd = expand_make_variables(&cmd, &lookup).map_err(|e| StarlarkError(format!("attribute {attr_s}: {e}")))?;
            }
            let shell_exe = st
                .cfg
                .option_str("shell_executable")
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "/bin/bash".to_string());
            Ok(Value::Tuple(vec![
                mk_list(dedup(inputs).iter().map(file_value).collect()),
                mk_list(vec![Value::String(shell_exe), Value::String("-c".into()), Value::String(cmd)]),
                mk_list(vec![]),
            ]))
        }
        "coverage_instrumented" => {
            st.check_mutable("coverage_instrumented")?;
            check_kwargs("coverage_instrumented", pos, kw, &["target"])?;
            let target_ct = match none_or(arg(pos, kw, 0, "target")) {
                Some(t) => Some(target_of(t).ok_or_else(|| {
                    StarlarkError(format!(
                        "in call to coverage_instrumented(), parameter 'target' got value of type '{}', want 'Target or NoneType'",
                        type_name(t)
                    ))
                })?),
                None => None,
            };
            if !coverage_enabled(&st.cfg) || st.cfg.is_exec {
                return Ok(Value::Bool(false));
            }
            if let Some(ct) = target_ct {
                let has_provider = ct.instrumented_files.is_some()
                    || ct.providers.contains_key("InstrumentedFilesInfo");
                return Ok(Value::Bool(
                    has_provider
                        && should_include_local_sources(
                            &st.cfg,
                            ct.actual_target_label(),
                            ct.is_test,
                        ),
                ));
            }
            let is_test = st.rule.is_test_rule();
            Ok(Value::Bool(should_include_local_sources(
                &st.cfg, &st.label, is_test,
            )))
        }
        "resolve_tools" => {
            st.check_mutable("resolve_tools")?;
            check_kwargs("resolve_tools", pos, kw, &["tools"])?;
            if st.cfg.bool_option("incompatible_disallow_ctx_resolve_tools", true) {
                return err(
                    "Pass an executable or tools argument to ctx.actions.run or ctx.actions.run_shell instead of calling ctx.resolve_tools.\nUse --noincompatible_disallow_ctx_resolve_tools to temporarily disable this check.",
                );
            }
            let mut files = Vec::new();
            for t in arg(pos, kw, 0, "tools").and_then(sequence).unwrap_or_default() {
                let ct = target_of(&t).ok_or_else(|| StarlarkError("resolve_tools: tools must be targets".into()))?;
                files.extend(target_files(&ct));
                if let Some(rs) = &ct.runfiles_support {
                    files.push(rs.manifest.clone());
                    files.extend(rs.runfiles.artifacts.iter().cloned());
                    files.push(rs.executable.clone());
                }
            }
            Ok(Value::Tuple(vec![files_depset(&dedup(files)), mk_list(vec![])]))
        }
        "created_actions" => {
            st.check_mutable("created_actions")?;
            check_kwargs("created_actions", pos, kw, &[])?;
            let testable = st
                .rule
                .extra
                .rule_class
                .as_ref()
                .and_then(|rc| rc.starlark.as_ref())
                .and_then(|sp| sp.kwargs.get("_skylark_testable"))
                .is_some_and(|v| matches!(v, Value::Bool(true)));
            if !testable {
                return Ok(Value::None);
            }
            let mut by_file = StarlarkDict::new();
            let mut seen = std::collections::HashSet::new();
            for a in st.actions.lock().unwrap().iter() {
                let av = action_value(Arc::new(a.clone()));
                for out in &a.outputs {
                    if seen.insert(out.exec_path.path_string().to_string()) {
                        let _ = by_file.insert(file_value(out), av.clone());
                    }
                }
            }
            let d = Value::Dict(Arc::new(Mutex::new(by_file)));
            d.freeze();
            Ok(provider_instance("Actions", vec![("by_file", d)]))
        }
        "check_placeholders" => {
            st.check_mutable("check_placeholders")?;
            check_kwargs("check_placeholders", pos, kw, &["template", "allowed_placeholders"])?;
            let template = want_str(
                arg(pos, kw, 0, "template").ok_or_else(|| StarlarkError("check_placeholders() missing 1 required positional argument: template".into()))?,
                "check_placeholders",
                "template",
            )?;
            let allowed_v = arg(pos, kw, 1, "allowed_placeholders")
                .ok_or_else(|| StarlarkError("check_placeholders() missing 1 required positional argument: allowed_placeholders".into()))?;
            let allowed: BTreeSet<String> = sequence(allowed_v)
                .ok_or_else(|| StarlarkError("check_placeholders(): allowed_placeholders must be a sequence".into()))?
                .into_iter()
                .map(|x| want_str(&x, "check_placeholders", "allowed_placeholders"))
                .collect::<Result<_, _>>()?;
            let mut rest = template.as_str();
            while let Some(i) = rest.find("%{") {
                let after = &rest[i + 2..];
                if let Some(j) = after.find('}') {
                    let ph = &after[..j];
                    if !allowed.contains(ph) {
                        return Ok(Value::Bool(false));
                    }
                    rest = &after[j + 1..];
                } else {
                    break;
                }
            }
            Ok(Value::Bool(true))
        }
        "package_relative_label" => {
            st.check_mutable("package_relative_label")?;
            check_kwargs("package_relative_label", pos, kw, &["input"])?;
            let input = arg(pos, kw, 0, "input")
                .ok_or_else(|| StarlarkError("package_relative_label() missing 1 required positional argument: input".into()))?;
            match input {
                Value::Label(l) => Ok(Value::Label(l.clone())),
                Value::String(s) => {
                    let rm = st.repo_mapping();
                    crate::location::parse_label_with_mapping(s, st.label.package_identifier(), Some(&rm))
                        .map(Value::Label)
                        .map_err(|e| StarlarkError(format!("invalid label in ctx.package_relative_label: {e}")))
                }
                other => err(format!(
                    "in call to package_relative_label(), parameter 'input' got value of type '{}', want 'Label or string'",
                    type_name(other)
                )),
            }
        }
        other => err(format!("'ctx' value has no field or method '{other}'")),
    }
}

fn path_of_frag(f: &PathFragment) -> String {
    f.path_string().to_string()
}

fn starts_with_dir(frag: &str, dir: &str) -> bool {
    dir.is_empty() || frag == dir || frag.starts_with(&format!("{dir}/"))
}

fn declare(st: &Arc<CtxState>, what: &str, pos: &[Value], kw: &BTreeMap<String, Value>, tree: bool, symlink: bool) -> HostResult {
    declare_ext(st, what, pos, kw, tree, symlink, false)
}

fn declare_ext(
    st: &Arc<CtxState>,
    what: &str,
    pos: &[Value],
    kw: &BTreeMap<String, Value>,
    tree: bool,
    symlink: bool,
    constant_metadata: bool,
) -> HostResult {
    let func = format!("declare_{what}");
    st.check_mutable(&format!("actions.{func}"))?;
    check_kwargs(&func, pos, kw, &["filename", "sibling"])?;
    let filename = want_str(
        arg(pos, kw, 0, "filename").ok_or_else(|| StarlarkError(format!("{func}() missing 1 required positional argument: filename")))?,
        &func,
        "filename",
    )?;
    if symlink && !st.inputs.allow_unresolved_symlinks {
        return err("actions.declare_symlink() is not allowed; use the --allow_unresolved_symlinks command line option");
    }
    let pkg_dir = st.package_directory();
    let pkg = path_of_frag(&pkg_dir);
    let frag = match none_or(arg(pos, kw, 1, "sibling")) {
        None => pkg_dir.get_relative_str(&filename),
        Some(s) => {
            let sib = want_file(s, &func, "sibling")?;
            let orig = sib
                .output_dir_relative_path(st.inputs.sibling_repository_layout)
                .path_string()
                .to_string();
            let dir = orig.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
            if dir.is_empty() {
                PathFragment::create(&filename)
            } else {
                PathFragment::create(&format!("{dir}/{filename}"))
            }
        }
    };
    let fs = path_of_frag(&frag);
    let uplevel = fs == ".." || fs.starts_with("../") || fs.contains("/../");
    if symlink {
        if uplevel {
            return err(format!(
                "the output symlink '{fs}' contains uplevel references for target '{}'",
                st.label.canonical_form()
            ));
        }
    } else if uplevel || !starts_with_dir(&fs, &pkg) {
        return err(format!(
            "the output {} '{fs}' is not under package directory '{pkg}' for target '{}'",
            if tree { "directory" } else { "artifact" },
            st.label.canonical_form()
        ));
    }
    let mut a = st.derived(frag);
    if tree {
        a.is_tree_artifact = true;
        if st.declared_files.lock().unwrap().contains(a.exec_path.path_string()) {
            return err(format!("'{filename}' has already been declared as a regular file, not directory."));
        }
        st.declared_trees.lock().unwrap().insert(a.exec_path.path_string().to_string());
    } else {
        if st.declared_trees.lock().unwrap().contains(a.exec_path.path_string()) {
            return err(format!("'{filename}' has already been declared as a directory, not a regular file."));
        }
        st.declared_files.lock().unwrap().insert(a.exec_path.path_string().to_string());
    }
    if symlink {
        a.is_unresolved_symlink = true;
    }
    if constant_metadata {
        a.is_constant_metadata = true;
    }
    st.declared_artifacts.lock().unwrap().push(a.clone());
    Ok(file_value(&a))
}

fn command_line(
    args: &Value,
    what: &str,
    dir_inputs: &mut Vec<Artifact>,
) -> Result<Vec<CommandLineArgs>, StarlarkError> {
    let items = sequence(args).ok_or_else(|| {
        StarlarkError(format!(
            "in call to {what}(), parameter 'arguments' got value of type '{}', want 'sequence'",
            type_name(args)
        ))
    })?;
    let mut out: Vec<CommandLineArgs> = Vec::new();
    let mut strings: Vec<String> = Vec::new();
    for v in items {
        match &v {
            Value::String(s) => strings.push(s.clone()),
            Value::Host(k, _) if k == "args" => {
                if !strings.is_empty() {
                    out.push(CommandLineArgs { args: std::mem::take(&mut strings), param_file: None });
                }
                let a = args_of(&v).unwrap();
                let guard = a.lock().unwrap();
                dir_inputs.extend(guard.directory_artifacts.iter().cloned());
                out.push(guard.finish());
            }
            other => {
                return err(format!(
                    "expected list of strings or ctx.actions.args() for arguments instead of {}",
                    type_name(other)
                ))
            }
        }
    }
    if !strings.is_empty() {
        out.push(CommandLineArgs { args: strings, param_file: None });
    }
    Ok(out)
}

fn substitute_progress(msg: &str, label: &Label, inputs: &[Artifact], outputs: &[Artifact]) -> String {
    msg.replace("%{label}", &label.canonical_form())
        .replace("%{input}", inputs.first().map(|a| a.root_relative_path.path_string()).unwrap_or(""))
        .replace("%{output}", outputs.first().map(|a| a.root_relative_path.path_string()).unwrap_or(""))
}

fn actions_method(
    st: &Arc<CtxState>,
    ctx: &mut EvalContext,
    method: &str,
    pos: &[Value],
    kw: &BTreeMap<String, Value>,
) -> HostResult {
    match method {
        "declare_file" => declare(st, "file", pos, kw, false, false),
        "declare_directory" => declare(st, "directory", pos, kw, true, false),
        "declare_symlink" => declare(st, "symlink", pos, kw, false, true),
        // `declare_shareable_artifact(path)`: an output at an arbitrary root-relative path of the
        // configuration's bin directory (used for solib symlinks, linkstamps, ...).
        "declare_shareable_artifact" | "declare_shareable_directory" => {
            st.check_mutable(&format!("actions.{method}"))?;
            let path = want_str(
                arg(pos, kw, 0, "path").ok_or_else(|| StarlarkError(format!("{method}() missing 1 required positional argument: path")))?,
                method,
                "path",
            )?;
            let mut a = st.derived(PathFragment::create(&path));
            if method == "declare_shareable_directory" {
                a.is_tree_artifact = true;
                st.declared_trees.lock().unwrap().insert(a.exec_path.path_string().to_string());
            }
            Ok(file_value(&a))
        }
        "args" => {
            check_kwargs("args", pos, kw, &[])?;
            Ok(args_value(ArgsBuilder::default()))
        }
        "do_nothing" => {
            st.check_mutable("actions.do_nothing")?;
            check_kwargs("do_nothing", pos, kw, &["mnemonic", "inputs"])?;
            let mnemonic = want_str(
                arg(pos, kw, 0, "mnemonic").ok_or_else(|| StarlarkError("do_nothing() missing 1 required positional argument: mnemonic".into()))?,
                "do_nothing",
                "mnemonic",
            )?;
            let inputs = match none_or(arg(pos, kw, 1, "inputs")) {
                Some(v) => files_in(v, "inputs")?,
                None => Vec::new(),
            };
            let mut a = Action::new(
                st.label.clone(),
                &mnemonic,
                ActionKind::Spawn { executable: String::new(), arguments: Vec::new(), shell_command: None },
            );
            a.is_pseudo = true;
            a.inputs = inputs;
            st.register(a)?;
            Ok(Value::None)
        }
        "write" => {
            st.check_mutable("actions.write")?;
            check_kwargs("write", pos, kw, &["output", "content", "is_executable", "mnemonic", "execution_requirements"])?;
            let out = want_file(arg(pos, kw, 0, "output").ok_or_else(|| StarlarkError("write() missing 1 required positional argument: output".into()))?, "write", "output")?;
            check_output(&out, "write")?;
            let content = arg(pos, kw, 1, "content").ok_or_else(|| StarlarkError("write() missing 1 required positional argument: content".into()))?;
            let is_exec = arg(pos, kw, 2, "is_executable").map(|v| v.is_truthy()).unwrap_or(false);
            let mnemonic = match none_or(arg(pos, kw, 3, "mnemonic")) {
                Some(m) => want_str(m, "write", "mnemonic")?,
                None => "FileWrite".to_string(),
            };
            let mut write_inputs: Vec<Artifact> = Vec::new();
            let content = match content {
                Value::String(s) => FileContent::Literal(s.clone()),
                v @ Value::Host(k, _) if k == "args" => {
                    let b = args_of(v).unwrap();
                    let b = b.lock().unwrap();
                    let a = b.finish();
                    // `set_param_file_format` alone selects the file format (Args.getParameterFileType).
                    let format = b.format.or_else(|| a.param_file.as_ref().map(|p| p.format)).unwrap_or(ParamFileFormat::Shell);
                    write_inputs.extend(b.directory_artifacts.iter().cloned());
                    FileContent::Args {
                        args: b.args.clone(),
                        format,
                        flag_groups: b.groups.clone(),
                    }
                }
                other => {
                    return err(format!(
                        "in call to write(), parameter 'content' got value of type '{}', want 'string or Args'",
                        type_name(other)
                    ))
                }
            };
            let mut a = Action::new(st.label.clone(), &mnemonic, ActionKind::FileWrite { content, is_executable: is_exec });
            a.progress_message = Some(format!("{} {}", if is_exec { "Writing script" } else { "Writing file" }, out.root_relative_path));
            if let Some(er) = none_or(arg(pos, kw, 4, "execution_requirements")) {
                a.execution_requirements = str_dict(er, "execution_requirements")?;
            }
            a.inputs.extend(write_inputs);
            a.outputs.push(out);
            st.register(a)?;
            Ok(Value::None)
        }
        "expand_template" => {
            st.check_mutable("actions.expand_template")?;
            check_kwargs("expand_template", pos, kw, &["template", "output", "substitutions", "is_executable", "computed_substitutions"])?;
            let template = want_file(arg(pos, kw, 0, "template").ok_or_else(|| StarlarkError("expand_template() missing 1 required positional argument: template".into()))?, "expand_template", "template")?;
            let out = want_file(arg(pos, kw, 1, "output").ok_or_else(|| StarlarkError("expand_template() missing 1 required positional argument: output".into()))?, "expand_template", "output")?;
            check_output(&out, "expand_template")?;
            let subs = match arg(pos, kw, 2, "substitutions") {
                Some(v) => str_dict_ordered(v, "substitutions")?,
                None => Vec::new(),
            };
            let is_exec = arg(pos, kw, 3, "is_executable").map(|v| v.is_truthy()).unwrap_or(false);
            let mut computed = Vec::new();
            if let Some(cs) = none_or(arg(pos, kw, 4, "computed_substitutions")) {
                match cs {
                    Value::Host(k, id) if k == "template_dict" => {
                        for entry in crate::objects::template_dict_entries(id).unwrap_or_default() {
                            match entry {
                                crate::objects::TemplateDictEntry::Literal(k, v) => {
                                    computed.push((k, v));
                                }
                                crate::objects::TemplateDictEntry::Joined { key, values, kw: akw } => {
                                    let mut b = ArgsBuilder::default();
                                    args_method(&mut b, ctx, "add_joined", &[values], &akw)?;
                                    computed.push((key, b.args.pop().unwrap_or_default()));
                                }
                            }
                        }
                    }
                    other => {
                        return err(format!(
                            "in call to expand_template(), parameter 'computed_substitutions' got value of type '{}', want 'TemplateDict or NoneType'",
                            type_name(other)
                        ));
                    }
                }
            }
            let mut seen_keys = std::collections::HashSet::new();
            for (k, _) in subs.iter().chain(computed.iter()) {
                if !seen_keys.insert(k.clone()) {
                    return err(format!("Multiple entries with same key: {k}"));
                }
            }
            let mut a = Action::new(
                st.label.clone(),
                "TemplateExpand",
                ActionKind::TemplateExpand { template: template.clone(), substitutions: subs, is_executable: is_exec, computed_substitutions: computed },
            );
            a.progress_message = Some(format!("Expanding template {}", out.root_relative_path));
            a.inputs.push(template);
            a.outputs.push(out);
            st.register(a)?;
            Ok(Value::None)
        }
        "transform_version_file" | "transform_info_file" => {
            st.check_mutable(&format!("actions.{method}"))?;
            check_kwargs(method, pos, kw, &["transform_func", "template", "output_file_name"])?;
            let func = arg(pos, kw, 0, "transform_func").cloned().ok_or_else(|| StarlarkError(format!("{method}() missing argument: transform_func")))?;
            if !matches!(func, Value::UserFunction(_) | Value::BuiltinFunction(_)) {
                return Err(StarlarkError(format!(
                    "in call to {method}(), parameter 'transform_func' got value of type '{}', want 'callable'",
                    func.type_name()
                )));
            }
            let template = want_file(arg(pos, kw, 1, "template").ok_or_else(|| StarlarkError(format!("{method}() missing argument: template")))?, method, "template")?;
            let name = want_str(arg(pos, kw, 2, "output_file_name").ok_or_else(|| StarlarkError(format!("{method}() missing argument: output_file_name")))?, method, "output_file_name")?;
            let is_volatile = method == "transform_version_file";
            let status_file = Artifact {
                is_source: false,
                is_tree_artifact: false,
                root: ArtifactRoot::Bin,
                is_unresolved_symlink: false,
                is_constant_metadata: is_volatile,
                exec_path: PathFragment::create(if is_volatile {
                    "bazel-out/volatile-status.txt"
                } else {
                    "bazel-out/stable-status.txt"
                }),
                root_relative_path: PathFragment::create(if is_volatile {
                    "volatile-status.txt"
                } else {
                    "stable-status.txt"
                }),
                owner_label: st.label.clone(),
            };
            let out_v = declare_ext(
                st,
                "file",
                &[Value::String(name)],
                &BTreeMap::new(),
                false,
                false,
                is_volatile,
            )?;
            let out = want_file(&out_v, method, "output_file_name")?;
            let func_key = format!("{}:{method}:{func:?}", st.rule.rule_class);
            let globals = ctx.globals.clone();
            let host = ctx.host.clone();
            let semantics = ctx.semantics.clone();
            let translator = crate::BuildInfoTranslator::new(
                func_key,
                Arc::new(move |status_map: &BTreeMap<String, String>| {
                    let mut eval = EvalContext::new(PathFragment::empty(), None);
                    eval.semantics = semantics.clone();
                    eval.thread.semantics = semantics.clone();
                    for (k, v) in &globals {
                        eval.globals.insert(k.clone(), v.clone());
                    }
                    eval.host = host.clone();
                    let mut d = StarlarkDict::new();
                    for (k, v) in status_map {
                        let _ = d.insert(Value::String(k.clone()), Value::String(v.clone()));
                    }
                    let arg_dict = Value::Dict(Arc::new(Mutex::new(d)));
                    let result = eval
                        .call_value(func.clone(), vec![arg_dict], BTreeMap::new())
                        .map_err(|e| e.0)?;
                    str_dict_ordered(&result, "transform_func result").map_err(|e| e.0)
                }),
            );
            // `BuildInfoFileWriteAction`: mnemonic `TranslateBuildInfo`.
            let mut a = Action::new(
                st.label.clone(),
                "TranslateBuildInfo",
                ActionKind::BuildInfoTranslate {
                    template: template.clone(),
                    status_file: status_file.clone(),
                    is_volatile,
                    translator,
                },
            );
            a.progress_message = Some(
                if is_volatile {
                    "Translating volatile BuildInfo file"
                } else {
                    "Translating stable BuildInfo file"
                }
                .to_string(),
            );
            a.inputs.push(status_file);
            a.inputs.push(template);
            a.outputs.push(out);
            st.register(a)?;
            Ok(out_v)
        }
        "template_dict" => Ok(crate::objects::new_template_dict()),
        "symlink" => {
            st.check_mutable("actions.symlink")?;
            check_kwargs("symlink", pos, kw, &["output", "target_file", "target_path", "is_executable", "progress_message", "target_type", "use_exec_root_for_source"])?;
            let out = want_file(arg(pos, kw, 0, "output").ok_or_else(|| StarlarkError("symlink() missing 1 required positional argument: output".into()))?, "symlink", "output")?;
            let tf = none_or(arg(pos, kw, 1, "target_file"));
            let tp = none_or(arg(pos, kw, 2, "target_path"));
            let tt = none_or(kw.get("target_type"));
            let is_exec = arg(pos, kw, 3, "is_executable").map(|v| v.is_truthy()).unwrap_or(false);
            let use_exec_root_for_source = none_or(kw.get("use_exec_root_for_source")).map(|v| v.is_truthy()).unwrap_or(false);
            if tf.is_some() == tp.is_some() {
                return err("Exactly one of \"target_file\" or \"target_path\" is required");
            }
            let progress = none_or(arg(pos, kw, 4, "progress_message"))
                .map(|v| want_str(v, "symlink", "progress_message"))
                .transpose()?;
            let mut a;
            if let Some(tf) = tf {
                if tt.is_some() {
                    return err("\"target_type\" cannot be used with \"target_file\"");
                }
                let input = want_file(tf, "symlink", "target_file")?;
                if out.is_unresolved_symlink {
                    return err("symlink() with \"target_file\" param requires that \"output\" be declared as a file or directory, not a symlink (did you mean to use declare_file() or declare_directory() instead of declare_symlink()?)");
                }
                if input.is_tree_artifact != out.is_tree_artifact {
                    let it = if input.is_tree_artifact { "directory" } else { "file" };
                    let ot = if out.is_tree_artifact { "directory" } else { "file" };
                    return err(format!("symlink() with \"target_file\" {it} param requires that \"output\" be declared as a {it} (did you mean to use declare_{it}() instead of declare_{ot}()?)"));
                }
                if is_exec && out.is_tree_artifact {
                    return err("symlink() with \"output\" directory param cannot be executable");
                }
                a = Action::new(
                    st.label.clone(),
                    // `SymlinkAction.getMnemonic`: `TargetType.EXECUTABLE` (`is_executable = True`).
                    if is_exec { "ExecutableSymlink" } else { "Symlink" },
                    ActionKind::Symlink { target_file: Some(input.clone()), target_path: None, is_executable: is_exec },
                );
                a.symlink_target_type = if is_exec { crate::SymlinkTargetType::Executable } else { crate::SymlinkTargetType::Other };
                a.use_exec_root_for_source = use_exec_root_for_source;
                a.inputs.push(input);
            } else {
                if !out.is_unresolved_symlink {
                    return err("symlink() with \"target_path\" param requires that \"output\" be declared as a symlink, not a file or directory (did you mean to use declare_symlink() instead of declare_file() or declare_directory()?)");
                }
                if is_exec {
                    return err("\"is_executable\" cannot be True when using \"target_path\"");
                }
                let mut sym_tt = crate::SymlinkTargetType::Unspecified;
                if let Some(t) = tt {
                    let t = want_str(t, "symlink", "target_type")?;
                    sym_tt = match t.as_str() {
                        "file" => crate::SymlinkTargetType::File,
                        "directory" => crate::SymlinkTargetType::Directory,
                        _ => return err("\"target_type\" must be one of \"file\" or \"directory\""),
                    };
                }
                let path = want_str(tp.unwrap(), "symlink", "target_path")?;
                a = Action::new(
                    st.label.clone(),
                    "UnresolvedSymlink",
                    ActionKind::Symlink { target_file: None, target_path: Some(path), is_executable: false },
                );
                a.symlink_target_type = sym_tt;
            }
            a.outputs.push(out.clone());
            a.progress_message = Some(match progress {
                Some(p) => substitute_progress(&p, &st.label, &a.inputs, &a.outputs),
                None => format!("Creating symlink {}", out.root_relative_path),
            });
            st.register(a)?;
            Ok(Value::None)
        }
        "run" => run_action(st, ctx, false, pos, kw),
        "run_shell" => run_action(st, ctx, true, pos, kw),
        "map_directory" => map_directory_action(st, ctx, pos, kw),
        other => err(format!("'actions' value has no field or method '{other}'")),
    }
}

fn check_output(out: &Artifact, func: &str) -> Result<(), StarlarkError> {
    if out.is_source {
        return err(format!("in call to {func}(), parameter 'output' may not be a source file"));
    }
    Ok(())
}

const RUN_PARAMS: &[&str] = &[
    "outputs", "inputs", "unused_inputs_list", "executable", "tools", "arguments", "mnemonic",
    "progress_message", "use_default_shell_env", "env", "execution_requirements", "input_manifests",
    "exec_group", "shadowed_action", "resource_set", "toolchain", "stdout",
];
const RUN_SHELL_PARAMS: &[&str] = &[
    "outputs", "inputs", "tools", "arguments", "mnemonic", "command", "progress_message",
    "use_default_shell_env", "env", "execution_requirements", "input_manifests", "exec_group",
    "shadowed_action", "resource_set", "toolchain",
];

fn run_action(
    st: &Arc<CtxState>,
    ctx: &mut EvalContext,
    shell: bool,
    pos: &[Value],
    kw: &BTreeMap<String, Value>,
) -> HostResult {
    let func = if shell { "run_shell" } else { "run" };
    st.check_mutable(&format!("actions.{func}"))?;
    let params = if shell { RUN_SHELL_PARAMS } else { RUN_PARAMS };
    check_kwargs(func, &[], kw, params)?;
    if !pos.is_empty() {
        return err(format!("{func}() accepts no positional arguments but got {}", pos.len()));
    }
    let get = |n: &str| none_or(kw.get(n));

    // outputs
    let outputs_v = kw
        .get("outputs")
        .ok_or_else(|| StarlarkError(format!("{func}() missing 1 required keyword-only argument: outputs")))?;
    let outputs = files_in(outputs_v, "outputs")?;
    let stdout = match get("stdout") {
        Some(v) => {
            let a = want_file(v, func, "stdout")?;
            if a.is_source {
                return err("param 'stdout' may not be a source file");
            }
            if a.is_tree_artifact {
                return err("param 'stdout' may not be a declared directory");
            }
            if a.is_unresolved_symlink {
                return err("param 'stdout' may not be a declared symlink");
            }
            if outputs.iter().any(|o| o.exec_path == a.exec_path) {
                return err(format!(
                    "file '{}' passed to 'stdout' may not also be listed in 'outputs'",
                    a.exec_path
                ));
            }
            Some(a)
        }
        None => None,
    };
    if outputs.is_empty() && stdout.is_none() {
        return err("param 'outputs' may not be empty while 'stdout' is not set");
    }

    // inputs
    let mut inputs: Vec<Artifact> = match get("inputs") {
        Some(v) => files_in(v, "inputs")?,
        None => Vec::new(),
    };
    if !shell {
        if let Some(u) = get("unused_inputs_list") {
            let ua = want_file(u, func, "unused_inputs_list")?;
            if ua.is_source {
                return err("param 'unused_inputs_list' may not be a source file");
            }
            if ua.is_tree_artifact {
                return err("param 'unused_inputs_list' may not be a declared directory");
            }
        }
    }
    let mut shadowed_env: BTreeMap<String, String> = BTreeMap::new();
    if let Some(sa) = get("shadowed_action") {
        let act = action_of(sa).ok_or_else(|| {
            StarlarkError(format!(
                "in call to {func}(), parameter 'shadowed_action' got value of type '{}', want 'Action or NoneType'",
                type_name(sa)
            ))
        })?;
        inputs.extend(act.inputs.iter().cloned());
        let env_map = match &act.action_env {
            Some(ae) => ae.fixed_env(),
            None => &act.env,
        };
        for (k, v) in env_map {
            shadowed_env.insert(k.clone(), v.clone());
        }
    }
    let mut tool_files: Vec<Artifact> = Vec::new();

    // tools
    if let Some(t) = get("tools") {
        let items = match t {
            Value::Depset(_) | Value::List(_) | Value::Tuple(_) => sequence(t).unwrap(),
            other => {
                return err(format!(
                    "expected value of type 'sequence or depset' for 'tools' but got {} instead",
                    type_name(other)
                ))
            }
        };
        for item in items {
            if let Some(a) = artifact_of(&item) {
                tool_files.push(a);
            } else if let Value::ProviderInstance { provider_name, fields } = &item {
                if provider_name != "FilesToRunProvider" {
                    return err(format!("expected value of type 'File, FilesToRunProvider or Depset of Files' for a member of parameter 'tools' but got {} instead", provider_name));
                }
                add_files_to_run_inputs(fields, &mut tool_files);
            } else if let Value::Depset(d) = &item {
                for f in d.to_list() {
                    tool_files.push(artifact_of(&f).ok_or_else(|| StarlarkError("expected value of type 'File, FilesToRunProvider or Depset of Files' for a member of parameter 'tools' but depset has non-File member".into()))?);
                }
            } else {
                return err(format!(
                    "expected value of type 'File, FilesToRunProvider or Depset of Files' for a member of parameter 'tools' but got {} instead",
                    type_name(&item)
                ));
            }
        }
    }

    // arguments
    let mut dir_inputs: Vec<Artifact> = Vec::new();
    let arguments = match get("arguments") {
        Some(v) => command_line(v, func, &mut dir_inputs)?,
        None => Vec::new(),
    };
    inputs.extend(dir_inputs);

    let mut spawn_exec: String;
    let mut spawn_args: Vec<CommandLineArgs> = Vec::new();
    let mut shell_command = None;
    if shell {
        let cmd = kw
            .get("command")
            .ok_or_else(|| StarlarkError("run_shell() missing 1 required keyword-only argument: command".into()))?;
        let pad = !arguments.is_empty();
        let shell_exe = st
            .cfg
            .option_str("shell_executable")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/bash".to_string());
        match cmd {
            Value::String(c) => {
                spawn_exec = shell_exe;
                let mut head = vec!["-c".to_string(), c.clone()];
                if pad {
                    head.push(String::new());
                }
                spawn_args.push(CommandLineArgs { args: head, param_file: None });
                shell_command = Some(c.clone());
            }
            Value::List(_) | Value::Tuple(_) => {
                return err("'command' must be of type string. passing a sequence of strings as 'command' is deprecated. To temporarily disable this check, set --incompatible_run_shell_command_string=false.");
            }
            other => {
                return err(format!(
                    "expected string or list of strings for command instead of {}",
                    type_name(other)
                ))
            }
        }
        spawn_args.extend(arguments);
    } else {
        let ex = kw
            .get("executable")
            .ok_or_else(|| StarlarkError("run() missing 1 required keyword-only argument: executable".into()))?;
        match ex {
            Value::String(s) => spawn_exec = s.clone(),
            v if artifact_of(v).is_some() => {
                let a = artifact_of(v).unwrap();
                spawn_exec = crate::location::callable_path(a.exec_path.path_string());
                // `StarlarkActionFactory.run`: if `executable` came from a dependency's
                // `FilesToRunProvider`, include its runfiles too (`getExecutableRunfiles`).
                for deps in st.deps.values() {
                    for dep in deps {
                        if let Some(Value::ProviderInstance { fields, .. }) = dep.providers.get("DefaultInfo") {
                            if let Some(Value::ProviderInstance { provider_name, fields: ftr }) = fields.get("files_to_run") {
                                if provider_name == "FilesToRunProvider"
                                    && ftr.get("executable").and_then(artifact_of).as_ref().map(|e| &e.exec_path) == Some(&a.exec_path)
                                {
                                    add_files_to_run_inputs(ftr, &mut tool_files);
                                }
                            }
                        }
                    }
                }
                tool_files.push(a);
            }
            Value::ProviderInstance { provider_name, fields } if provider_name == "FilesToRunProvider" => {
                match fields.get("executable").and_then(artifact_of) {
                    Some(a) => {
                        spawn_exec = crate::location::callable_path(a.exec_path.path_string());
                        add_files_to_run_inputs(fields, &mut tool_files);
                    }
                    None => return err("run(): FilesToRunProvider has no executable"),
                }
            }
            other => {
                return err(format!(
                    "expected file or string for \"executable\" but got {} instead",
                    type_name(other)
                ))
            }
        }
        spawn_args.extend(arguments);
    }
    let _ = &mut spawn_exec;

    // mnemonic
    let mnemonic = match get("mnemonic") {
        Some(m) => {
            let m = want_str(m, func, "mnemonic")?;
            if m.is_empty() || !m.chars().all(|c| c.is_alphanumeric()) {
                return err(format!("mnemonic must only contain letters and/or digits, and have non-zero length, was: \"{m}\""));
            }
            m
        }
        None => "Action".to_string(),
    };

    let mut env = shadowed_env;
    if let Some(v) = get("env") {
        env.extend(str_dict(v, "env")?);
    }
    let mut exec_req = match get("execution_requirements") {
        Some(v) => str_dict(v, "execution_requirements")?
            .into_iter()
            .filter(|(k, _)| is_legal_exec_info_key(k))
            .collect(),
        None => BTreeMap::new(),
    };
    if st.cfg.bool_option("incompatible_allow_tags_propagation", true) {
        if let Some(zel_packages::attr::AttrValue::StrList(tags)) = st.rule.attr_or_default("tags") {
            for tag in tags {
                if is_legal_exec_info_key(&tag) {
                    exec_req.entry(tag).or_insert_with(String::new);
                }
            }
        }
    }
    let mut exec_group = match get("exec_group") {
        Some(g) => {
            let g = want_str(g, func, "exec_group")?;
            if !st.exec_groups.contains(&g) {
                return err(format!("Action declared for non-existent exec group '{g}'."));
            }
            if g == "default-exec-group" || !is_valid_starlark_ident(&g) {
                return err(format!("Invalid name for exec group '{g}'."));
            }
            Some(g)
        }
        None => None,
    };
    if let Some(tc_v) = get("toolchain") {
        let k = transform_toolchain_key(st, ctx, tc_v)?;
        let matches_key = |m: &BTreeMap<String, Value>| {
            m.contains_key(&k)
                || (k.starts_with("//") && m.keys().any(|c| c.starts_with("@@") && c.ends_with(&k)))
        };
        if let Some(ref eg) = exec_group {
            let in_eg = st
                .inputs
                .exec_group_toolchains
                .get(eg)
                .is_some_and(matches_key);
            if !in_eg {
                return err(format!(
                    "Action declared for exec group '{eg}', which does not have toolchain '{k}'"
                ));
            }
        } else if matches_key(&st.inputs.toolchains) {
            // Default exec group declares this toolchain.
        } else {
            let matching_egs: Vec<String> = st
                .inputs
                .exec_group_toolchains
                .iter()
                .filter(|(_, m)| matches_key(m))
                .map(|(g, _)| g.clone())
                .collect();
            if matching_egs.len() == 1 {
                exec_group = Some(matching_egs[0].clone());
            } else if matching_egs.is_empty() {
                return err(format!(
                    "Action declared for non-existent toolchain '{k}'."
                ));
            } else {
                return err(format!(
                    "Toolchain '{k}' is declared in multiple exec groups [{}]; specify 'exec_group' explicitly",
                    matching_egs.join(", ")
                ));
            }
        }
    }
    inputs.extend(tool_files.iter().cloned());
    let mut resource_set = ResourceSet::default();
    if let Some(rs) = get("resource_set") {
        let os = if cfg!(target_os = "macos") { "osx" } else { "linux" };
        let d = match rs {
            Value::Dict(_) => rs.clone(),
            f @ (Value::UserFunction(_) | Value::BuiltinFunction(_)) => {
                if let Value::UserFunction(u) = f {
                    if !u.closure.scopes.is_empty() || u.name == "<lambda>" {
                        return err(format!(
                            "to avoid unintended retention of analysis data structures, the function (declared at {}) must be declared by a top-level def statement",
                            u.location
                        ));
                    }
                }
                ctx.call_value(
                    f.clone(),
                    vec![Value::String(os.to_string()), Value::Int(BigInt::from_i64(inputs.len() as i64))],
                    BTreeMap::new(),
                )?
            }
            other => {
                return err(format!(
                    "in call to {func}(), parameter 'resource_set' got value of type '{}', want 'callable, dict or NoneType'",
                    type_name(other)
                ))
            }
        };
        let Value::Dict(dd) = d else { return err("resource_set callback must return a dict") };
        let items_rs = dd.lock().unwrap().items();
        let mut illegal = Vec::new();
        for (k, _) in &items_rs {
            let Value::String(k) = k else { return err("resource_set keys must be strings") };
            if !matches!(k.as_str(), "cpu" | "memory" | "local_test") {
                illegal.push(k.clone());
            }
        }
        if !illegal.is_empty() {
            return err(format!("Illegal resource keys: ({})", illegal.join(",")));
        }
        for (k, v) in items_rs {
            let Value::String(k) = k else { unreachable!() };
            let num = match &v {
                Value::Int(i) => i.to_f64(),
                Value::Float(f) => *f,
                _ => return err(format!("Illegal resource value type for key {k}: got {}, want int or float", type_name(&v))),
            };
            match k.as_str() {
                "cpu" => resource_set.cpu = num,
                "memory" => resource_set.memory_mb = num,
                "local_test" => resource_set.local_test_count = num as i32,
                _ => unreachable!(),
            }
        }
    }
    let progress = get("progress_message")
        .map(|p| want_str(p, func, "progress_message"))
        .transpose()?
        .map(|p| substitute_progress(&p, &st.label, &inputs, &outputs));
    let mut all_outputs = outputs;
    if let Some(s) = &stdout {
        all_outputs.push(s.clone());
    }
    let mut a = Action::new(
        st.label.clone(),
        &mnemonic,
        ActionKind::Spawn { executable: spawn_exec, arguments: spawn_args, shell_command },
    );
    a.progress_message = progress;
    a.env = env;
    a.execution_requirements = exec_req;
    a.inputs = inputs;
    a.tools = dedup(tool_files);
    a.outputs = all_outputs;
    a.resource_set = resource_set;
    a.use_default_shell_env = get("use_default_shell_env").map(|v| v.is_truthy()).unwrap_or(false);
    a.exec_group = exec_group;
    st.register(a)?;
    Ok(Value::None)
}

fn is_legal_exec_info_key(tag: &str) -> bool {
    tag.starts_with("block-")
        || tag.starts_with("requires-")
        || tag.starts_with("no-")
        || tag.starts_with("supports-")
        || tag.starts_with("disable-")
        || tag.starts_with("cpu:")
        || tag.starts_with("resources:")
        || tag == "local"
        || tag == "worker-key-mnemonic"
}

fn is_valid_starlark_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else { return false };
    (first.is_ascii_alphabetic() || first == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn add_files_to_run_inputs(fields: &BTreeMap<String, Value>, out: &mut Vec<Artifact>) {
    if let Some(e) = fields.get("executable").and_then(artifact_of) {
        out.push(e);
    }
    if let Some(m) = fields.get("runfiles_manifest").and_then(artifact_of) {
        out.push(m);
    }
    if let Some(rm) = fields.get("repo_mapping_manifest").and_then(artifact_of) {
        out.push(rm);
    }
    if let Some(r) = fields.get("_runfiles").and_then(runfiles_of) {
        out.extend(r.artifacts.iter().cloned());
        out.extend(r.symlinks.values().cloned());
        out.extend(r.root_symlinks.values().cloned());
    }
}

const MAP_DIRECTORY_PARAMS: &[&str] = &[
    "input_directories",
    "additional_inputs",
    "output_directories",
    "tools",
    "additional_params",
    "execution_requirements",
    "exec_group",
    "toolchain",
    "use_default_shell_env",
    "env",
    "mnemonic",
    "resource_set",
    "implementation",
];

fn validate_map_dir_inputs_dict(
    entries: Vec<(Value, Value)>,
    what: &str,
    all_inputs: &mut Vec<Artifact>,
    tool_files: Option<&mut Vec<Artifact>>,
) -> Result<Vec<(Value, Value)>, StarlarkError> {
    let mut tool_out = tool_files;
    let mut validated = Vec::with_capacity(entries.len());
    for (k, v) in entries {
        let ks = want_str(&k, "map_directory", what)?;
        if let Some(a) = artifact_of(&v) {
            all_inputs.push(a.clone());
            if let Some(tf) = tool_out.as_deref_mut() {
                tf.push(a);
            }
        } else if let Value::ProviderInstance {
            provider_name,
            fields,
        } = &v
        {
            if provider_name != "FilesToRunProvider" {
                return err(format!(
                    "Expected one of [File, FilesToRunProvider, depset]; but got {provider_name} in {what}['{ks}']."
                ));
            }
            let mut ftr_files = Vec::new();
            add_files_to_run_inputs(fields, &mut ftr_files);
            all_inputs.extend(ftr_files.iter().cloned());
            if let Some(tf) = tool_out.as_deref_mut() {
                tf.extend(ftr_files);
            }
        } else if let Value::Depset(d) = &v {
            for item in d.to_list() {
                let a = artifact_of(&item).ok_or_else(|| {
                    StarlarkError(format!(
                        "Expected a Depset of Files in {what}['{ks}'], got member of type {}",
                        type_name(&item)
                    ))
                })?;
                all_inputs.push(a.clone());
                if let Some(tf) = tool_out.as_deref_mut() {
                    tf.push(a);
                }
            }
        } else {
            return err(format!(
                "Expected one of [File, FilesToRunProvider, depset]; but got {} in {what}['{ks}'].",
                type_name(&v)
            ));
        }
        v.freeze();
        validated.push((Value::String(ks), v));
    }
    Ok(validated)
}

#[derive(Clone)]
struct StarlarkMapDirectoryExpander {
    base_state: Arc<CtxState>,
    input_directories: Vec<(String, Artifact)>,
    output_directories: Vec<(String, Artifact)>,
    additional_inputs: Vec<(Value, Value)>,
    tools: Vec<(Value, Value)>,
    additional_params: Vec<(Value, Value)>,
    implementation: Value,
    expanded_mnemonic: String,
    env: BTreeMap<String, String>,
    use_default_shell_env: bool,
    execution_requirements: BTreeMap<String, String>,
    exec_group: Option<String>,
    resource_set: ResourceSet,
    fingerprint: String,
}

impl std::fmt::Debug for StarlarkMapDirectoryExpander {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StarlarkMapDirectoryExpander")
            .field("owner", &self.base_state.label)
            .field("expanded_mnemonic", &self.expanded_mnemonic)
            .finish()
    }
}

struct TemplateCallState {
    sub_st: Arc<CtxState>,
    allowed_output_dirs: BTreeSet<String>,
    expanded_mnemonic: String,
    env: BTreeMap<String, String>,
    use_default_shell_env: bool,
    execution_requirements: BTreeMap<String, String>,
    exec_group: Option<String>,
    resource_set: ResourceSet,
}

impl crate::MapDirectoryExpander for StarlarkMapDirectoryExpander {
    fn key_fingerprint(&self) -> String {
        self.fingerprint.clone()
    }

    fn expand(
        &self,
        discovered_children: &BTreeMap<String, Vec<(String, Artifact)>>,
    ) -> Result<Vec<crate::ActionAnalysisMetadata>, String> {
        let b = &self.base_state;
        let sub_st = Arc::new(CtxState {
            rule: b.rule.clone(),
            label: b.label.clone(),
            cfg: b.cfg.clone(),
            inputs: b.inputs.clone(),
            pkg_dir: b.pkg_dir.clone(),
            output_to_genfiles: b.output_to_genfiles,
            exec_groups: b.exec_groups.clone(),
            deps: b.deps.clone(),
            attr_struct: b.attr_struct.clone(),
            file_struct: b.file_struct.clone(),
            files_struct: b.files_struct.clone(),
            executable_struct: b.executable_struct.clone(),
            outputs_struct: b.outputs_struct.clone(),
            predeclared: Vec::new(),
            default_executable: None,
            default_executable_accessed: Mutex::new(false),
            actions: Mutex::new(Vec::new()),
            graph: Mutex::new(ActionGraph::new()),
            declared_trees: Mutex::new(BTreeSet::new()),
            declared_files: Mutex::new(BTreeSet::new()),
            declared_artifacts: Mutex::new(Vec::new()),
            rule_errors: Mutex::new(Vec::new()),
            make_vars: b.make_vars.clone(),
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
        });

        let allowed_output_dirs: BTreeSet<String> = self
            .output_directories
            .iter()
            .map(|(_, a)| a.exec_path.path_string().to_string())
            .collect();

        let tcall = Arc::new(TemplateCallState {
            sub_st: sub_st.clone(),
            allowed_output_dirs,
            expanded_mnemonic: self.expanded_mnemonic.clone(),
            env: self.env.clone(),
            use_default_shell_env: self.use_default_shell_env,
            execution_requirements: self.execution_requirements.clone(),
            exec_group: self.exec_group.clone(),
            resource_set: self.resource_set.clone(),
        });

        let mut in_dirs_entries = Vec::with_capacity(self.input_directories.len());
        for (k, dir) in &self.input_directories {
            let children_vals: Vec<Value> = discovered_children
                .get(k)
                .map(|kids| {
                    kids.iter()
                        .map(|(rel, art)| crate::objects::tree_file_value(art, rel.clone()))
                        .collect()
                })
                .unwrap_or_default();
            in_dirs_entries.push((
                Value::String(k.clone()),
                expanded_directory_value(dir.clone(), children_vals),
            ));
        }
        let out_dirs_entries: Vec<(Value, Value)> = self
            .output_directories
            .iter()
            .map(|(k, dir)| (Value::String(k.clone()), file_value(dir)))
            .collect();

        let mut cb_kw = BTreeMap::new();
        cb_kw.insert("input_directories".to_string(), mk_dict_frozen(in_dirs_entries));
        cb_kw.insert("output_directories".to_string(), mk_dict_frozen(out_dirs_entries));
        cb_kw.insert(
            "additional_inputs".to_string(),
            mk_dict_frozen(self.additional_inputs.clone()),
        );
        cb_kw.insert("tools".to_string(), mk_dict_frozen(self.tools.clone()));
        cb_kw.insert(
            "additional_params".to_string(),
            mk_dict_frozen(self.additional_params.clone()),
        );

        let mut ectx = EvalContext::new(b.pkg_dir.clone(), None);
        ectx.file_name = b
            .rule
            .extra
            .location
            .clone()
            .unwrap_or_else(|| "BUILD".to_string());
        let tcall2 = tcall.clone();
        ectx.host = Some(Arc::new(move |c, name, mut pos, kw| {
            if let Some(rest) = name.strip_prefix("method:") {
                let (kind, method) = rest.split_once('.').unwrap_or((rest, ""));
                if kind == "template_ctx" {
                    if !pos.is_empty() {
                        pos.remove(0);
                    }
                    return eval_template_ctx_method(&tcall2, c, method, &pos, &kw);
                }
            }
            dispatch(&tcall2.sub_st, c, name, pos, kw)
        }));

        let tctx = Value::Host("template_ctx".into(), "0".into());
        let ret = ectx
            .call_value(self.implementation.clone(), vec![tctx], cb_kw)
            .map_err(|e| e.0)?;
        *sub_st.frozen.lock().unwrap() = true;
        ectx.mutability.freeze();

        if ret != Value::None {
            let (fn_name, fn_loc) = match &self.implementation {
                Value::UserFunction(u) => (u.name.clone(), u.location.to_string()),
                Value::BuiltinFunction(n) => (n.clone(), "<builtin>".to_string()),
                _ => ("<callable>".to_string(), "<unknown>".to_string()),
            };
            return Err(format!(
                "actions.map_directory() implementation {fn_name} at {fn_loc} may not return a non-None value (got {})",
                ret.repr()
            ));
        }

        sub_st.validate_artifacts()?;
        let actions = sub_st.actions.lock().unwrap();
        let mut lowered = Vec::new();
        for a in actions.iter() {
            lowered.extend(a.lower());
        }
        Ok(lowered)
    }
}

fn mk_dict_frozen(entries: Vec<(Value, Value)>) -> Value {
    let d = mk_dict(entries);
    d.freeze();
    d
}

fn map_directory_action(
    st: &Arc<CtxState>,
    ctx: &mut EvalContext,
    pos: &[Value],
    kw: &BTreeMap<String, Value>,
) -> HostResult {
    if !st.cfg.bool_option("experimental_allow_map_directory", true) {
        return err(
            "actions.map_directory() is an experimental API and is subjected to change. Please set the flag --experimental_allow_map_directory to enable it.",
        );
    }
    st.check_mutable("actions.map_directory")?;
    check_kwargs("map_directory", &[], kw, MAP_DIRECTORY_PARAMS)?;
    if !pos.is_empty() {
        return err(format!(
            "map_directory() accepts no positional arguments but got {}",
            pos.len()
        ));
    }
    let want_dict = |name: &str, required: bool| -> Result<Vec<(Value, Value)>, StarlarkError> {
        match kw.get(name) {
            Some(Value::Dict(d)) => Ok(d.lock().unwrap().items()),
            Some(other) => err(format!(
                "in call to map_directory(), parameter '{name}' got value of type '{}', want 'dict'",
                type_name(other)
            )),
            None if required => err(format!(
                "map_directory() missing 1 required keyword-only argument: {name}"
            )),
            None => Ok(Vec::new()),
        }
    };
    let in_dirs_raw = want_dict("input_directories", true)?;
    let add_in_raw = want_dict("additional_inputs", false)?;
    let out_dirs_raw = want_dict("output_directories", true)?;
    let tools_raw = want_dict("tools", false)?;
    let add_params_raw = want_dict("additional_params", false)?;
    let imp = kw.get("implementation").cloned().ok_or_else(|| {
        StarlarkError(
            "map_directory() missing 1 required keyword-only argument: implementation".into(),
        )
    })?;

    if in_dirs_raw.is_empty() {
        return err("actions.map_directory() requires at least one input.");
    }
    if out_dirs_raw.is_empty() {
        return err("actions.map_directory() requires at least one output.");
    }

    let (imp_name, imp_loc) = match &imp {
        Value::UserFunction(u) => {
            if !u.closure.scopes.is_empty() || u.name == "<lambda>" {
                return err(format!(
                    "Error in map_directory: to avoid unintended retention of analysis data structures, the function (declared at {}) must be declared by a top-level def statement",
                    u.location
                ));
            }
            (u.name.clone(), u.location.to_string())
        }
        other => {
            return err(format!(
                "in call to map_directory(), parameter 'implementation' got value of type '{}', want 'function'",
                type_name(other)
            ));
        }
    };

    let mut all_inputs: Vec<Artifact> = Vec::new();
    let mut tool_files: Vec<Artifact> = Vec::new();

    let mut input_directories = Vec::with_capacity(in_dirs_raw.len());
    for (k, v) in in_dirs_raw {
        let ks = want_str(&k, "map_directory", "input_directories")?;
        let af = want_file(&v, "map_directory", "input_directories")?;
        if !af.is_tree_artifact {
            return err(format!(
                "Expected a directory (SpecialArtifact) in input_directories['{ks}'], got '{}'",
                af.exec_path
            ));
        }
        all_inputs.push(af.clone());
        input_directories.push((ks, af));
    }

    let additional_inputs =
        validate_map_dir_inputs_dict(add_in_raw, "additional_inputs", &mut all_inputs, None)?;

    let mut output_directories = Vec::with_capacity(out_dirs_raw.len());
    let mut outputs_vec = Vec::with_capacity(out_dirs_raw.len());
    for (k, v) in out_dirs_raw {
        let ks = want_str(&k, "map_directory", "output_directories")?;
        let af = want_file(&v, "map_directory", "output_directories")?;
        if !af.is_tree_artifact || af.is_source {
            return err(format!(
                "Expected a declared output directory in output_directories['{ks}'], got '{}'",
                af.exec_path
            ));
        }
        outputs_vec.push(af.clone());
        output_directories.push((ks, af));
    }

    let tools = validate_map_dir_inputs_dict(
        tools_raw,
        "tools",
        &mut all_inputs,
        Some(&mut tool_files),
    )?;

    let mut additional_params = Vec::with_capacity(add_params_raw.len());
    for (k, v) in add_params_raw {
        let ks = want_str(&k, "map_directory", "additional_params")?;
        match &v {
            Value::Bool(_) | Value::Int(_) | Value::String(_) => {}
            other => {
                return err(format!(
                    "Expected one of [bool, int, string]; but got {} in additional_params['{ks}'].",
                    type_name(other)
                ));
            }
        }
        additional_params.push((Value::String(ks), v));
    }

    let get = |k: &str| none_or(kw.get(k));
    let expanded_mnemonic = match get("mnemonic") {
        Some(m) => {
            let m = want_str(m, "map_directory", "mnemonic")?;
            if m.is_empty() || !m.chars().all(|c| c.is_alphanumeric()) {
                return err(format!(
                    "mnemonic must only contain letters and/or digits, and have non-zero length, was: \"{m}\""
                ));
            }
            m
        }
        None => "ExpandedTemplateAction".to_string(),
    };

    let mut env = BTreeMap::new();
    if let Some(v) = get("env") {
        env.extend(str_dict(v, "env")?);
    }
    let use_default_shell_env = get("use_default_shell_env")
        .map(|v| v.is_truthy())
        .unwrap_or(false);

    let mut exec_req = match get("execution_requirements") {
        Some(v) => str_dict(v, "execution_requirements")?
            .into_iter()
            .filter(|(k, _)| is_legal_exec_info_key(k))
            .collect(),
        None => BTreeMap::new(),
    };
    if st.cfg.bool_option("incompatible_allow_tags_propagation", true) {
        if let Some(zel_packages::attr::AttrValue::StrList(tags)) = st.rule.attr_or_default("tags")
        {
            for tag in tags {
                if is_legal_exec_info_key(&tag) {
                    exec_req.entry(tag).or_insert_with(String::new);
                }
            }
        }
    }

    let mut exec_group = match get("exec_group") {
        Some(g) => {
            let g = want_str(g, "map_directory", "exec_group")?;
            if !st.exec_groups.contains(&g) {
                return err(format!("Action declared for non-existent exec group '{g}'."));
            }
            if g == "default-exec-group" || !is_valid_starlark_ident(&g) {
                return err(format!("Invalid name for exec group '{g}'."));
            }
            Some(g)
        }
        None => None,
    };
    if let Some(tc_v) = get("toolchain") {
        let k = transform_toolchain_key(st, ctx, tc_v)?;
        let matches_key = |m: &BTreeMap<String, Value>| {
            m.contains_key(&k)
                || (k.starts_with("//") && m.keys().any(|c| c.starts_with("@@") && c.ends_with(&k)))
        };
        if let Some(ref eg) = exec_group {
            let in_eg = st
                .inputs
                .exec_group_toolchains
                .get(eg)
                .is_some_and(matches_key);
            if !in_eg {
                return err(format!(
                    "Action declared for exec group '{eg}', which does not have toolchain '{k}'"
                ));
            }
        } else if matches_key(&st.inputs.toolchains) {
            // Default exec group declares this toolchain.
        } else {
            let matching_egs: Vec<String> = st
                .inputs
                .exec_group_toolchains
                .iter()
                .filter(|(_, m)| matches_key(m))
                .map(|(g, _)| g.clone())
                .collect();
            if matching_egs.len() == 1 {
                exec_group = Some(matching_egs[0].clone());
            } else if matching_egs.is_empty() {
                return err(format!(
                    "Action declared for non-existent toolchain '{k}'."
                ));
            } else {
                return err(format!(
                    "Toolchain '{k}' is declared in multiple exec groups [{}]; specify 'exec_group' explicitly",
                    matching_egs.join(", ")
                ));
            }
        }
    }

    let mut resource_set = ResourceSet::default();
    if let Some(rs) = get("resource_set") {
        let os = if cfg!(target_os = "macos") { "osx" } else { "linux" };
        let d = match rs {
            Value::Dict(_) => rs.clone(),
            f @ (Value::UserFunction(_) | Value::BuiltinFunction(_)) => {
                if let Value::UserFunction(u) = f {
                    if !u.closure.scopes.is_empty() || u.name == "<lambda>" {
                        return err(format!(
                            "to avoid unintended retention of analysis data structures, the function (declared at {}) must be declared by a top-level def statement",
                            u.location
                        ));
                    }
                }
                ctx.call_value(
                    f.clone(),
                    vec![
                        Value::String(os.to_string()),
                        Value::Int(BigInt::from_i64(all_inputs.len() as i64)),
                    ],
                    BTreeMap::new(),
                )?
            }
            other => {
                return err(format!(
                    "in call to map_directory(), parameter 'resource_set' got value of type '{}', want 'callable, dict or NoneType'",
                    type_name(other)
                ));
            }
        };
        let Value::Dict(dd) = d else {
            return err("resource_set callback must return a dict");
        };
        let items_rs = dd.lock().unwrap().items();
        let mut illegal = Vec::new();
        for (k, _) in &items_rs {
            let Value::String(k) = k else {
                return err("resource_set keys must be strings");
            };
            if !matches!(k.as_str(), "cpu" | "memory" | "local_test") {
                illegal.push(k.clone());
            }
        }
        if !illegal.is_empty() {
            return err(format!("Illegal resource keys: ({})", illegal.join(",")));
        }
        for (k, v) in items_rs {
            let Value::String(k) = k else { unreachable!() };
            let num = match &v {
                Value::Int(i) => i.to_f64(),
                Value::Float(f) => *f,
                _ => {
                    return err(format!(
                        "Illegal resource value type for key {k}: got {}, want int or float",
                        type_name(&v)
                    ));
                }
            };
            match k.as_str() {
                "cpu" => resource_set.cpu = num,
                "memory" => resource_set.memory_mb = num,
                "local_test" => resource_set.local_test_count = num as i32,
                _ => unreachable!(),
            }
        }
    }

    let mut fp = zel_util::Fingerprint::new();
    fp.add_str(&imp_name).add_str(&imp_loc).add_str(&expanded_mnemonic);
    for (k, v) in &additional_params {
        fp.add_str(&k.repr()).add_str(&v.repr());
    }
    for (k, v) in &additional_inputs {
        fp.add_str(&k.repr()).add_str(&v.repr());
    }
    for (k, v) in &tools {
        fp.add_str(&k.repr()).add_str(&v.repr());
    }
    let fingerprint = fp.hex_digest_and_reset();

    let expander = Arc::new(StarlarkMapDirectoryExpander {
        base_state: st.clone(),
        input_directories: input_directories.clone(),
        output_directories: output_directories.clone(),
        additional_inputs,
        tools,
        additional_params,
        implementation: imp,
        expanded_mnemonic: expanded_mnemonic.clone(),
        env: env.clone(),
        use_default_shell_env,
        execution_requirements: exec_req.clone(),
        exec_group: exec_group.clone(),
        resource_set: resource_set.clone(),
        fingerprint,
    });

    let mut a = Action::new(
        st.label.clone(),
        "StarlarkMapActionTemplate",
        ActionKind::MapDirectory {
            input_directories,
            output_directories,
            expanded_mnemonic,
            expander: crate::MapDirectoryExpanderHandle(expander),
        },
    );
    a.env = env;
    a.execution_requirements = exec_req;
    a.inputs = all_inputs;
    a.tools = dedup(tool_files);
    a.outputs = outputs_vec;
    a.resource_set = resource_set;
    a.use_default_shell_env = use_default_shell_env;
    a.exec_group = exec_group;
    st.register(a)?;
    Ok(Value::None)
}

fn eval_template_ctx_method(
    tcall: &TemplateCallState,
    ctx: &mut EvalContext,
    method: &str,
    pos: &[Value],
    kw: &BTreeMap<String, Value>,
) -> HostResult {
    let st = &tcall.sub_st;
    match method {
        "declare_file" => {
            check_kwargs("declare_file", pos, kw, &["filename", "directory"])?;
            if pos.len() > 1 {
                return err(format!(
                    "declare_file() accepts no more than 1 positional argument but got {}",
                    pos.len()
                ));
            }
            let filename = want_str(
                pos.first().or_else(|| kw.get("filename")).ok_or_else(|| {
                    StarlarkError(
                        "declare_file() missing 1 required positional argument: filename".into(),
                    )
                })?,
                "declare_file",
                "filename",
            )?;
            let dir_v = kw.get("directory").ok_or_else(|| {
                StarlarkError(
                    "declare_file() missing 1 required keyword-only argument: directory".into(),
                )
            })?;
            let dir = want_file(dir_v, "declare_file", "directory")?;
            if !dir.is_tree_artifact
                || dir.is_source
                || !tcall
                    .allowed_output_dirs
                    .contains(dir.exec_path.path_string())
            {
                return err(format!(
                    "Cannot declare file `{filename}` in non-output directory {}",
                    dir_v.repr()
                ));
            }
            let norm = filename.replace('\\', "/");
            if norm.is_empty()
                || norm.starts_with('/')
                || norm.split('/').any(|seg| seg.is_empty() || seg == "." || seg == "..")
            {
                return err(format!(
                    "declare_file(): invalid relative filename '{filename}'"
                ));
            }
            let rel = dir.root_relative_path.get_relative_str(&norm);
            let mut a = st.derived(rel);
            a.root = dir.root;
            st.declared_files
                .lock()
                .unwrap()
                .insert(a.exec_path.path_string().to_string());
            st.declared_artifacts.lock().unwrap().push(a.clone());
            Ok(crate::objects::tree_file_value(&a, norm))
        }
        "args" => {
            check_kwargs("args", pos, kw, &[])?;
            Ok(args_value(ArgsBuilder::default()))
        }
        "run" => {
            check_kwargs(
                "run",
                &[],
                kw,
                &[
                    "outputs",
                    "inputs",
                    "executable",
                    "tools",
                    "arguments",
                    "progress_message",
                ],
            )?;
            if !pos.is_empty() {
                return err(format!(
                    "run() accepts no positional arguments but got {}",
                    pos.len()
                ));
            }
            let out_v = kw.get("outputs").ok_or_else(|| {
                StarlarkError("run() missing 1 required keyword-only argument: outputs".into())
            })?;
            let out_items = sequence(out_v).ok_or_else(|| {
                StarlarkError(format!(
                    "in call to run(), parameter 'outputs' got value of type '{}', want 'sequence'",
                    type_name(out_v)
                ))
            })?;
            {
                let declared = st.declared_files.lock().unwrap();
                for item in &out_items {
                    let af = want_file(item, "run", "outputs")?;
                    if !declared.contains(af.exec_path.path_string()) {
                        return err(format!(
                            "Action has conflicting output '{}' that is an output of another action, thus causing an action conflict. `template_ctx.run()` should only use outputs declared by `template_ctx.declare_file()` within the same Starlark implementation function.",
                            af.exec_path
                        ));
                    }
                }
            }
            let mut sub_kw = kw.clone();
            sub_kw.insert(
                "mnemonic".to_string(),
                Value::String(tcall.expanded_mnemonic.clone()),
            );
            sub_kw.insert(
                "use_default_shell_env".to_string(),
                Value::Bool(tcall.use_default_shell_env),
            );
            if !tcall.env.is_empty() {
                sub_kw.insert(
                    "env".to_string(),
                    mk_dict(
                        tcall
                            .env
                            .iter()
                            .map(|(k, v)| (Value::String(k.clone()), Value::String(v.clone())))
                            .collect(),
                    ),
                );
            }
            if !tcall.execution_requirements.is_empty() {
                sub_kw.insert(
                    "execution_requirements".to_string(),
                    mk_dict(
                        tcall
                            .execution_requirements
                            .iter()
                            .map(|(k, v)| (Value::String(k.clone()), Value::String(v.clone())))
                            .collect(),
                    ),
                );
            }
            if let Some(ref eg) = tcall.exec_group {
                sub_kw.insert("exec_group".to_string(), Value::String(eg.clone()));
            }
            run_action(st, ctx, false, &[], &sub_kw)?;
            if let Some(last) = st.actions.lock().unwrap().last_mut() {
                last.resource_set = tcall.resource_set.clone();
            }
            Ok(Value::None)
        }
        other => err(format!(
            "'template_ctx' value has no field or method '{other}'"
        )),
    }
}

fn template_ctx_method(
    _st: &Arc<CtxState>,
    _ctx: &mut EvalContext,
    method: &str,
    _pos: &[Value],
    _kw: &BTreeMap<String, Value>,
) -> HostResult {
    err(format!(
        "cannot call template_ctx.{method}() outside of a map_directory implementation"
    ))
}
