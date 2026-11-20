
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
        "args" => {
            let a = args_of(recv).ok_or_else(|| StarlarkError("invalid Args".into()))?;
            let mut b = a.lock().unwrap();
            args_method(&mut b, ctx, method, pos, kw)?;
            Ok(recv.clone())
        }
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
                    d.lock().unwrap().push((key, v));
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
                    if !kw.contains_key("join_with") {
                        return err("add_joined() missing 1 required named-only argument: 'join_with'");
                    }
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
                    let mut b = ArgsBuilder::default();
                    let mut akw = kw.clone();
                    akw.remove("key");
                    akw.remove("values");
                    akw.insert("omit_if_empty".into(), Value::Bool(false));
                    args_method(&mut b, ctx, "add_joined", &[values], &akw)?;
                    d.lock().unwrap().push((key, b.args.pop().unwrap_or_default()));
                    Ok(recv.clone())
                }
                other => err(format!("'TemplateDict' value has no field or method '{other}'")),
            }
        }
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
    use crate::nested_set::{NSet, COMPILE_ORDER, STABLE_ORDER};
    if let Some(s) = memo.get(&d.id) {
        return s.clone();
    }
    let order = if d.order == zel_starlark::DepsetOrder::Default { STABLE_ORDER } else { COMPILE_ORDER };
    let direct: Vec<(String, String)> = d.direct.iter().filter_map(artifact_of).map(|a| Runfiles::elem(&a)).collect();
    let trans: Vec<_> = d.transitive.iter().map(|t| depset_nested(t, memo)).collect();
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
                    let target = if root { &mut out.root_symlinks } else { &mut out.symlinks };
                    match s {
                        Value::Dict(d) => {
                            let items = d.lock().unwrap().items();
                            if !items.is_empty() {
                                has_symlinks = true;
                            }
                            for (k, v) in items {
                                let Value::String(k) = k else { return err(format!("{name} keys must be strings")) };
                                target.insert(k, want_file(&v, "runfiles", name)?);
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
                                target.insert(want_str(&p, "runfiles", name)?, want_file(&f, "runfiles", name)?);
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
        "coverage_instrumented" => Ok(Value::Bool(coverage_enabled(&st.cfg))),
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
            Ok(Value::None)
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
    let pkg = path_of_frag(&st.pkg_dir);
    let frag = match none_or(arg(pos, kw, 1, "sibling")) {
        None => st.pkg_dir.get_relative_str(&filename),
        Some(s) => {
            let sib = want_file(s, &func, "sibling")?;
            let orig = sib.root_relative_path.path_string().to_string();
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
    } else if uplevel || !{
        let rel = if st.inputs.sibling_repository_layout {
            fs.as_str()
        } else {
            // Outputs next to a sibling of an external-repository target carry `external/<repo>/`.
            let repo_prefix = format!("external/{}/", st.label.package_identifier().repository().name());
            fs.strip_prefix(repo_prefix.as_str()).unwrap_or(&fs)
        };
        starts_with_dir(rel, &pkg)
    } {
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
            let content = match content {
                Value::String(s) => FileContent::Literal(s.clone()),
                v @ Value::Host(k, _) if k == "args" => {
                    let b = args_of(v).unwrap();
                    let b = b.lock().unwrap();
                    let a = b.finish();
                    // `set_param_file_format` alone selects the file format (Args.getParameterFileType).
                    let format = b.format.or_else(|| a.param_file.as_ref().map(|p| p.format)).unwrap_or(ParamFileFormat::Shell);
                    FileContent::Args {
                        args: b.param_file_args(format),
                        format,
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
            let subs = str_dict_ordered(arg(pos, kw, 2, "substitutions").ok_or_else(|| StarlarkError("expand_template() missing 1 required positional argument: substitutions".into()))?, "substitutions")?;
            let is_exec = arg(pos, kw, 3, "is_executable").map(|v| v.is_truthy()).unwrap_or(false);
            let mut computed = Vec::new();
            if let Some(Value::Host(k, id)) = none_or(arg(pos, kw, 4, "computed_substitutions")) {
                if k == "template_dict" {
                    computed = crate::objects::template_dict_entries(id).unwrap_or_default();
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

    let env = match get("env") {
        Some(v) => str_dict(v, "env")?,
        None => BTreeMap::new(),
    };
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
    let exec_group = match get("exec_group") {
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
