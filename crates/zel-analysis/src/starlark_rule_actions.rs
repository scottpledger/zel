
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
                    check_kwargs("add_joined", pos, kw, &["key", "values", "join_with", "map_each", "uniquify", "format_joined", "allow_closure"])?;
                    let values = arg(pos, kw, 1, "values").cloned().ok_or_else(|| StarlarkError("add_joined() missing 1 required positional argument: values".into()))?;
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
            "is_sibling_repository_layout" => Ok(Value::Bool(false)),
            // `--stamp` defaults to false for `bazel build`.
            "stamp_binaries" => Ok(Value::Bool(st.cfg.options.get("stamp").is_some_and(|v| v == "true" || v == "1") && !st.cfg.is_exec)),
            "runfiles_enabled" => Ok(Value::Bool(st.cfg.env.runfiles_enabled)),
            // Genfiles are merged into bin in this implementation.
            "has_separate_genfiles_directory" => Ok(Value::Bool(false)),
            other => err(format!("'configuration' value has no field or method '{other}'")),
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
    for attr in ["srcs", "deps", "data"] {
        if let Some(ts) = st.deps.get(attr) {
            for t in ts {
                out = out.merge(&target_runfiles(t, data));
            }
        }
    }
    out
}

/// Runfiles contributed by a dependency (`Runfiles.Builder.addTarget`): its `data_runfiles` /
/// `default_runfiles`, or, for targets without runfiles (plain files), the files themselves.
pub fn target_runfiles(t: &ConfiguredTarget, data: bool) -> Runfiles {
    if let Some(Value::ProviderInstance { fields, .. }) = t.providers.get("DefaultInfo") {
        let key = if data { "data_runfiles" } else { "default_runfiles" };
        if let Some(r) = fields.get(key).and_then(runfiles_of) {
            // A rule's `RunfilesProvider` is authoritative even when empty (its `DefaultInfo.files`
            // are not implicitly runfiles): only targets without one fall back to their files.
            return (*r).clone();
        }
    }
    // `Runfiles.Builder.addTargetIncludingFileTargets`: file targets (source/generated files)
    // only contribute to the *data* runfiles; the default mapping yields nothing for them.
    if !data && (t.rule_class == "source file" || t.rule_class == "generated file") {
        return Runfiles::default();
    }
    Runfiles { artifacts: t.default_info.files.clone(), ..Default::default() }
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
            let v = arg(pos, kw, 0, "option").ok_or_else(|| StarlarkError("tokenize() missing 1 required positional argument: option".into()))?;
            let s = want_str(v, "tokenize", "option")?;
            shell_tokenize(&s)
                .map(|t| mk_list(t.into_iter().map(Value::String).collect()))
                .map_err(StarlarkError)
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
            let mut r = Runfiles::default();
            if let Some(f) = arg(pos, kw, 0, "files") {
                r.artifacts = files_in(f, "files")?;
            }
            if let Some(t) = none_or(arg(pos, kw, 1, "transitive_files")) {
                let Value::Depset(_) = t else {
                    return err(format!(
                        "in call to runfiles(), parameter 'transitive_files' got value of type '{}', want 'depset or NoneType'",
                        type_name(t)
                    ));
                };
                r.artifacts.extend(files_in(t, "transitive_files")?);
            }
            for (idx, name, root) in [(4usize, "symlinks", false), (5, "root_symlinks", true)] {
                if let Some(s) = none_or(arg(pos, kw, idx, name)) {
                    let target = if root { &mut r.root_symlinks } else { &mut r.symlinks };
                    match s {
                        Value::Dict(d) => {
                            for (k, v) in d.lock().unwrap().items() {
                                let Value::String(k) = k else { return err(format!("{name} keys must be strings")) };
                                target.insert(k, want_file(&v, "runfiles", name)?);
                            }
                        }
                        other => {
                            for e in sequence(other).unwrap_or_default() {
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
            let mut out = r;
            // `Runfiles.Builder` structure of the artifact set: the collected runfiles of the
            // dependencies and the `transitive_files` set are transitive children, `files` direct.
            let mut transitive_sets: Vec<crate::nested_set::NSet<(String, String)>> = Vec::new();
            let direct_files: Vec<Artifact> = out.artifacts.clone();
            let mut collected: Vec<Runfiles> = Vec::new();
            for (idx, data) in [(2usize, true), (3, false)] {
                if arg(pos, kw, idx, if data { "collect_data" } else { "collect_default" }).map(|v| v.is_truthy()).unwrap_or(false) {
                    out = out.merge(&collect_attr_runfiles(st, data));
                    for attr in ["data", "srcs", "deps"] {
                        if let Some(ts) = st.deps.get(attr) {
                            for t in ts {
                                collected.push(target_runfiles(t, data));
                            }
                        }
                    }
                }
            }
            for c in &collected {
                transitive_sets.push(c.nested_set());
            }
            if let Some(Value::Depset(d)) = none_or(arg(pos, kw, 1, "transitive_files")) {
                let mut memo = std::collections::HashMap::new();
                transitive_sets.push(depset_nested(d, &mut memo));
                // `files` of the call are direct elements: take them from the explicit list only.
            }
            let explicit: Vec<Artifact> = match arg(pos, kw, 0, "files") {
                Some(f) => files_in(f, "files")?,
                None => Vec::new(),
            };
            let _ = direct_files;
            out.nested = Runfiles::build_nested(&explicit, transitive_sets);
            Ok(runfiles_value(out))
        }
        "expand_location" => {
            check_kwargs("expand_location", pos, kw, &["input", "targets", "short_paths"])?;
            let input = want_str(
                arg(pos, kw, 0, "input").ok_or_else(|| StarlarkError("expand_location() missing 1 required positional argument: input".into()))?,
                "expand_location",
                "input",
            )?;
            let mut map: BTreeMap<Label, Vec<Artifact>> = BTreeMap::new();
            for attr in ["srcs", "deps", "tools"] {
                if let Some(ts) = st.deps.get(attr) {
                    for t in ts {
                        map.entry(t.label.clone()).or_default().extend(target_files(t));
                    }
                }
            }
            if let Some(t) = arg(pos, kw, 1, "targets") {
                for item in sequence(t).unwrap_or_default() {
                    let ct = target_of(&item).ok_or_else(|| {
                        StarlarkError(format!(
                            "expected value of type 'Target' for a member of parameter 'targets' but got {} instead",
                            type_name(&item)
                        ))
                    })?;
                    map.insert(ct.label.clone(), target_files(&ct));
                }
            }
            // `short_paths` (keyword-only, undocumented): runfiles paths instead of exec paths.
            let mode = if arg(pos, kw, usize::MAX, "short_paths").map(|v| v.is_truthy()).unwrap_or(false) {
                LocationMode::Runfiles
            } else {
                LocationMode::Exec
            };
            expand_locations(&input, &map, &st.label, mode, &st.inputs.workspace_name)
                .map(Value::String)
                .map_err(StarlarkError)
        }
        "expand_make_variables" => {
            check_kwargs("expand_make_variables", pos, kw, &["attribute_name", "command", "additional_substitutions"])?;
            let attr_name = want_str(arg(pos, kw, 0, "attribute_name").ok_or_else(|| StarlarkError("expand_make_variables() missing argument: attribute_name".into()))?, "expand_make_variables", "attribute_name")?;
            let command = want_str(arg(pos, kw, 1, "command").ok_or_else(|| StarlarkError("expand_make_variables() missing argument: command".into()))?, "expand_make_variables", "command")?;
            let extra = match arg(pos, kw, 2, "additional_substitutions") {
                Some(v) => str_dict(v, "additional_substitutions")?,
                None => BTreeMap::new(),
            };
            let lookup = |n: &str| extra.get(n).cloned().or_else(|| st.make_vars.get(n).cloned());
            expand_make_variables(&command, &lookup)
                .map(Value::String)
                .map_err(|e| StarlarkError(format!("attribute {attr_name}: {e}")))
        }
        // `StarlarkRuleContext.resolveCommand`: expands `$(location)` / make variables in a shell
        // command and returns `(inputs, ["/bin/bash", "-c", command], input_manifests)`.
        "resolve_command" => {
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
                Some(Value::None) | None => "command".to_string(),
                Some(v) => want_str(v, "resolve_command", "attribute")?,
            };
            let expand_loc = arg(pos, kw, 2, "expand_locations").map(|v| v.is_truthy()).unwrap_or(false);
            let extra = match arg(pos, kw, 3, "make_variables") {
                Some(Value::None) | None => BTreeMap::new(),
                Some(v) => str_dict(v, "make_variables")?,
            };
            let mut map: BTreeMap<Label, Vec<Artifact>> = BTreeMap::new();
            let mut inputs: Vec<Artifact> = Vec::new();
            for t in arg(pos, kw, 4, "tools").and_then(sequence).unwrap_or_default() {
                let ct = target_of(&t).ok_or_else(|| StarlarkError("resolve_command: tools must be targets".into()))?;
                let files = target_files(&ct);
                inputs.extend(files.iter().cloned());
                map.insert(ct.label.clone(), files);
            }
            if let Some(Value::Dict(d)) = arg(pos, kw, 5, "label_dict") {
                for (k, v) in d.lock().unwrap().entries.iter() {
                    let key_label = match k {
                        Value::Label(l) => l.clone(),
                        other => target_of(other)
                            .ok_or_else(|| StarlarkError("resolve_command: label_dict keys must be Labels or targets".into()))?
                            .label
                            .clone(),
                    };
                    let mut files = Vec::new();
                    for f in sequence(v).unwrap_or_default() {
                        if let Some(a) = artifact_of(&f) {
                            files.push(a);
                        }
                    }
                    inputs.extend(files.iter().cloned());
                    map.insert(key_label, files);
                }
            }
            let mut cmd = command;
            if expand_loc {
                cmd = expand_locations(&cmd, &map, &st.label, LocationMode::Exec, &st.inputs.workspace_name)
                    .map_err(|e| StarlarkError(format!("attribute {attr_name}: {e}")))?;
            }
            let lookup = |n: &str| extra.get(n).cloned().or_else(|| st.make_vars.get(n).cloned());
            let cmd = expand_make_variables(&cmd, &lookup).map_err(|e| StarlarkError(format!("attribute {attr_name}: {e}")))?;
            Ok(Value::Tuple(vec![
                mk_list(dedup(inputs).iter().map(file_value).collect()),
                mk_list(vec![Value::String("/bin/bash".into()), Value::String("-c".into()), Value::String(cmd)]),
                mk_list(vec![]),
            ]))
        }
        "coverage_instrumented" => Ok(Value::Bool(coverage_enabled(&st.cfg))),
        "resolve_tools" => {
            check_kwargs("resolve_tools", pos, kw, &["tools"])?;
            let mut files = Vec::new();
            for t in arg(pos, kw, 0, "tools").and_then(sequence).unwrap_or_default() {
                let ct = target_of(&t).ok_or_else(|| StarlarkError("resolve_tools: tools must be targets".into()))?;
                files.extend(target_files(&ct));
            }
            Ok(mk_struct(vec![
                ("inputs", files_depset(&dedup(files))),
                ("input_manifests", mk_list(vec![])),
            ]))
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
        // Outputs next to a sibling of an external-repository target carry `external/<repo>/`.
        let repo_prefix = format!("external/{}/", st.label.package_identifier().repository().name());
        let rel = fs.strip_prefix(repo_prefix.as_str()).unwrap_or(&fs);
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
        st.declared_trees.lock().unwrap().insert(a.exec_path.path_string().to_string());
    } else if st.declared_trees.lock().unwrap().contains(a.exec_path.path_string()) {
        return err(format!("'{filename}' has already been declared as a directory, not a regular file."));
    }
    if symlink {
        a.is_unresolved_symlink = true;
    }
    Ok(file_value(&a))
}

fn command_line(
    args: &Value,
    what: &str,
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
                out.push(a.lock().unwrap().finish());
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
            if arg(pos, kw, 0, "mnemonic").is_none() {
                return err("do_nothing() missing 1 required positional argument: mnemonic");
            }
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
                    FileContent::Args { args: a.args, format }
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
        // `transform_version_file` / `transform_info_file` read the workspace status files at
        // execution time in Bazel. Here `transform_func` is applied at analysis time to the
        // status of an unstamped build (constant `BUILD_TIMESTAMP`, ...); stamped builds
        // (`--stamp`) are not supported.
        "transform_version_file" | "transform_info_file" => {
            st.check_mutable(&format!("actions.{method}"))?;
            check_kwargs(method, pos, kw, &["transform_func", "template", "output_file_name"])?;
            let func = arg(pos, kw, 0, "transform_func").cloned().ok_or_else(|| StarlarkError(format!("{method}() missing argument: transform_func")))?;
            let template = want_file(arg(pos, kw, 1, "template").ok_or_else(|| StarlarkError(format!("{method}() missing argument: template")))?, method, "template")?;
            let name = want_str(arg(pos, kw, 2, "output_file_name").ok_or_else(|| StarlarkError(format!("{method}() missing argument: output_file_name")))?, method, "output_file_name")?;
            let status: Vec<(&str, &str)> = if method == "transform_version_file" {
                vec![("BUILD_TIMESTAMP", "0"), ("FORMATTED_DATE", "1970 Jan 01 00 00 00 Thu"), ("BUILD_SCM_REVISION", "0"), ("BUILD_SCM_STATUS", "")]
            } else {
                vec![("BUILD_EMBED_LABEL", ""), ("BUILD_HOST", "hostname"), ("BUILD_USER", "username")]
            };
            let mut d = StarlarkDict::new();
            for (k, v) in status {
                let _ = d.insert(Value::String(k.into()), Value::String(v.into()));
            }
            let arg_dict = Value::Dict(Arc::new(Mutex::new(d)));
            let result = ctx.call_value(func, vec![arg_dict], BTreeMap::new())?;
            let subs = str_dict_ordered(&result, "transform_func result")?;
            let out_v = declare(st, "file", &[Value::String(name)], &BTreeMap::new(), false, false)?;
            let out = want_file(&out_v, method, "output_file_name")?;
            // `BuildInfoFileWriteAction`: mnemonic `TranslateBuildInfo`.
            let mut a = Action::new(
                st.label.clone(),
                "TranslateBuildInfo",
                ActionKind::TemplateExpand { template: template.clone(), substitutions: subs, is_executable: false, computed_substitutions: Vec::new() },
            );
            a.progress_message = Some(
                if method == "transform_version_file" { "Translating volatile BuildInfo file" } else { "Translating stable BuildInfo file" }.to_string(),
            );
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
                a.inputs.push(input);
            } else {
                if !out.is_unresolved_symlink {
                    return err("symlink() with \"target_path\" param requires that \"output\" be declared as a symlink, not a file or directory (did you mean to use declare_symlink() instead of declare_file() or declare_directory()?)");
                }
                if is_exec {
                    return err("\"is_executable\" cannot be True when using \"target_path\"");
                }
                if let Some(t) = tt {
                    let t = want_str(t, "symlink", "target_type")?;
                    if t != "file" && t != "directory" {
                        return err("\"target_type\" must be one of \"file\" or \"directory\"");
                    }
                }
                let path = want_str(tp.unwrap(), "symlink", "target_path")?;
                a = Action::new(
                    st.label.clone(),
                    "UnresolvedSymlink",
                    ActionKind::Symlink { target_file: None, target_path: Some(path), is_executable: false },
                );
            }
            a.progress_message = Some(progress.unwrap_or_else(|| "Creating symlink {output}".replace("{output}", out.root_relative_path.path_string())));
            a.outputs.push(out);
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
    let arguments = match get("arguments") {
        Some(v) => command_line(v, func)?,
        None => Vec::new(),
    };

    let mut spawn_exec: String;
    let mut spawn_args: Vec<CommandLineArgs> = Vec::new();
    let mut shell_command = None;
    if shell {
        let cmd = kw
            .get("command")
            .ok_or_else(|| StarlarkError("run_shell() missing 1 required keyword-only argument: command".into()))?;
        let pad = !arguments.is_empty();
        match cmd {
            Value::String(c) => {
                spawn_exec = "/bin/bash".to_string();
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
                spawn_exec = a.exec_path.path_string().to_string();
                tool_files.push(a);
            }
            Value::ProviderInstance { provider_name, fields } if provider_name == "FilesToRunProvider" => {
                match fields.get("executable").and_then(artifact_of) {
                    Some(a) => {
                        spawn_exec = a.exec_path.path_string().to_string();
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
            if m.is_empty() || !m.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' ) {
                return err(format!("Action mnemonic '{m}' must only contain [A-Za-z0-9_]"));
            }
            m
        }
        None => "Action".to_string(),
    };

    let env = match get("env") {
        Some(v) => str_dict(v, "env")?,
        None => BTreeMap::new(),
    };
    let exec_req = match get("execution_requirements") {
        Some(v) => str_dict(v, "execution_requirements")?,
        None => BTreeMap::new(),
    };
    let exec_group = match get("exec_group") {
        Some(g) => {
            let g = want_str(g, func, "exec_group")?;
            if g != "default" && !st.exec_groups.contains(&g) {
                return err(format!("Action declared for non-existent exec group '{g}'."));
            }
            Some(g)
        }
        None => None,
    };
    let mut resource_set = ResourceSet::default();
    if let Some(rs) = get("resource_set") {
        let os = if cfg!(target_os = "macos") { "osx" } else { "linux" };
        let d = match rs {
            Value::Dict(_) => rs.clone(),
            f @ (Value::UserFunction(_) | Value::BuiltinFunction(_)) => ctx.call_value(
                f.clone(),
                vec![Value::String(os.to_string()), Value::Int(BigInt::from_i64(inputs.len() as i64))],
                BTreeMap::new(),
            )?,
            other => {
                return err(format!(
                    "in call to {func}(), parameter 'resource_set' got value of type '{}', want 'callable, dict or NoneType'",
                    type_name(other)
                ))
            }
        };
        let Value::Dict(dd) = d else { return err("resource_set callback must return a dict") };
        for (k, v) in dd.lock().unwrap().items() {
            let Value::String(k) = k else { return err("resource_set keys must be strings") };
            let num = match &v {
                Value::Int(i) => i.to_f64(),
                Value::Float(f) => *f,
                _ => return err(format!("resource_set value for '{k}' must be a number")),
            };
            match k.as_str() {
                "cpu" => resource_set.cpu = num,
                "memory" => resource_set.memory_mb = num,
                "local_test" => resource_set.local_test_count = num as i32,
                other => return err(format!("Illegal resource keys: ({other})")),
            }
        }
    }

    inputs.extend(tool_files.iter().cloned());
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

fn add_files_to_run_inputs(fields: &BTreeMap<String, Value>, out: &mut Vec<Artifact>) {
    if let Some(e) = fields.get("executable").and_then(artifact_of) {
        out.push(e);
    }
    if let Some(r) = fields.get("_runfiles").and_then(runfiles_of) {
        out.extend(r.artifacts.iter().cloned());
        out.extend(r.symlinks.values().cloned());
        out.extend(r.root_symlinks.values().cloned());
    }
}
