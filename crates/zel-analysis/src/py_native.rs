// Analysis-time natives of `_builtins.internal.py_builtins` (`PyBuiltins.java`) that need the rule
// context. Included into `starlark_rule.rs`.
//
// `create_repo_mapping_manifest` uses `crate::repo_mapping`; `regex_match` only knows the `(?U)\w+` pattern used by
// rules_python, and the `__init__.py` generation (`merge_runfiles_with_generated_inits_...`) is
// computed when the runfiles are merged rather than when the final manifest is built.

fn py_native(
    st: &Arc<CtxState>,
    ctx: &mut EvalContext,
    name: &str,
    pos: &[Value],
    kw: &BTreeMap<String, Value>,
) -> Option<HostResult> {
    let n = name.strip_prefix("builtins.").unwrap_or(name).strip_prefix("py_builtins.")?;
    let a = |i: usize, k: &str| kw.get(k).or_else(|| pos.get(i)).filter(|v| !matches!(v, Value::None));
    Some(match n {
        "is_singleton_depset" => match a(0, "value") {
            Some(Value::Depset(d)) => Ok(Value::Bool(d.to_list().len() == 1)),
            _ => err("is_singleton_depset: expected a depset"),
        },
        "regex_match" => {
            let (subject, pattern) = match (a(0, "subject").and_then(|v| v.as_str()), a(1, "pattern").and_then(|v| v.as_str())) {
                (Some(s), Some(p)) => (s, p),
                _ => return Some(err("regex_match: subject and pattern must be strings")),
            };
            match zel_util::java_regex::Regex::new(pattern) {
                Ok(re) => Ok(Value::Bool(re.full_match(subject))),
                Err(e) => err(format!("py_builtins.regex_match: invalid regex '{pattern}': {e}")),
            }
        }
        "get_rule_name" => Ok(Value::String(st.rule.rule_class.clone())),
        "get_current_os_name" => Ok(Value::String(
            if cfg!(target_os = "macos") {
                "osx"
            } else if cfg!(target_os = "windows") {
                "windows"
            } else {
                "linux"
            }
            .to_string(),
        )),
        "get_label_repo_runfiles_path" => match a(0, "label") {
            Some(Value::Label(l)) => {
                let repo = l.package_identifier().repository().name().to_string();
                let pkg = l.package_identifier().package_fragment().path_string().to_string();
                Ok(Value::String(match (repo.is_empty(), pkg.is_empty()) {
                    (true, _) => pkg,
                    (false, true) => format!("../{repo}"),
                    (false, false) => format!("../{repo}/{pkg}"),
                }))
            }
            _ => err("get_label_repo_runfiles_path: expected a Label"),
        },
        "add_py_extra_pseudo_action" => Ok(Value::None),
        "declare_constant_metadata_file" => {
            let fname = match a(1, "name") {
                Some(Value::String(s)) => s.clone(),
                _ => return Some(err("declare_constant_metadata_file: name must be a string")),
            };
            Ok(file_value(&st.derived(PathFragment::create(&format!(
                "{}{}{fname}",
                st.label.package_identifier().package_fragment().path_string(),
                if st.label.package_identifier().package_fragment().path_string().is_empty() { "" } else { "/" },
            )))))
        }
        "copy_without_caching" => (|| {
            let from = a(1, "read_from").and_then(artifact_of).ok_or_else(|| StarlarkError("copy_without_caching: read_from must be a File".into()))?;
            let to = a(2, "write_to").and_then(artifact_of).ok_or_else(|| StarlarkError("copy_without_caching: write_to must be a File".into()))?;
            let mut act = Action::new(
                st.label.clone(),
                "PyCopyWithoutCaching",
                ActionKind::Spawn {
                    executable: "/bin/cp".to_string(),
                    arguments: vec![CommandLineArgs { args: vec![jpath(&from), jpath(&to)], param_file: None }],
                    shell_command: None,
                },
            );
            act.progress_message = Some(format!("Copying {} to {} (uncachable action)", jpath(&from), jpath(&to)));
            act.execution_requirements = [("no-cache".to_string(), String::new())].into_iter().collect();
            act.inputs = vec![from];
            act.outputs = vec![to];
            st.register(act)?;
            Ok(Value::None)
        })(),
        "create_repo_mapping_manifest" => (|| {
            let out = a(2, "output").and_then(artifact_of).ok_or_else(|| StarlarkError("create_repo_mapping_manifest: output must be a File".into()))?;
            let rf = a(1, "runfiles").and_then(runfiles_of).ok_or_else(|| StarlarkError("create_repo_mapping_manifest: runfiles must be a runfiles object".into()))?;
            let entries = st.repo_mapping_for(&rf);
            let mut act = Action::new(
                st.label.clone(),
                "RepoMappingManifest",
                ActionKind::FileWrite { content: FileContent::Literal(crate::repo_mapping::repo_mapping_text(&entries)), is_executable: false },
            );
            act.progress_message = Some(format!("Writing repo mapping manifest for {}", st.label.canonical_form()));
            act.outputs = vec![out];
            st.register(act)?;
            Ok(Value::None)
        })(),
        "merge_runfiles_with_generated_inits_empty_files_supplier" => (|| {
            let rf = a(1, "runfiles").and_then(runfiles_of).ok_or_else(|| StarlarkError("runfiles must be a runfiles object".into()))?;
            if rf.is_empty() {
                return err("input runfiles cannot be empty");
            }
            let mut out = (*rf).clone();
            out.generate_init_py_files = true;
            Ok(runfiles_value(out))
        })(),
        "expand_location_and_make_variables" => (|| {
            let attr = a(1, "attribute_name").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
            let expr = a(2, "expression").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
            let mut map: BTreeMap<Label, Vec<Artifact>> = BTreeMap::new();
            if let Some(ts) = a(3, "targets") {
                for t in items(ts) {
                    if let Some(ct) = target_of(&t) {
                        insert_target_location_keys(&mut map, &ct, target_files(&ct));
                    }
                }
            }
            let _ = &ctx;
            let mapping = repo_mapping_for_target(st);
            let lookup = |n: &str| st.make_vars.get(n).cloned();
            let s = expand_make_variables(&expr, &lookup).map_err(|e| StarlarkError(format!("attribute {attr}: {e}")))?;
            crate::location::expand_locations_with_mapping(&s, &map, &st.label, LocationMode::Runfiles, &st.inputs.workspace_name, Some(&mapping))
                .map(Value::String)
                .map_err(StarlarkError)
        })(),
        // SourceManifestAction with ManifestType.SOURCES_ONLY: one runfiles path per line.
        "create_sources_only_manifest" => (|| {
            let out = a(2, "output").and_then(artifact_of).ok_or_else(|| StarlarkError("create_sources_only_manifest: output must be a File".into()))?;
            let rf = a(1, "runfiles").and_then(runfiles_of).ok_or_else(|| StarlarkError("create_sources_only_manifest: runfiles must be a runfiles object".into()))?;
            let mut text = String::new();
            for k in rf.manifest_entries(&st.inputs.workspace_name).keys() {
                text.push_str(k);
                text.push('\n');
            }
            let mut act = Action::new(
                st.label.clone(),
                "PackagingSourcesManifest",
                ActionKind::FileWrite { content: FileContent::Literal(text), is_executable: false },
            );
            act.progress_message = Some("Creating file sources list".to_string());
            act.inputs = rf.artifacts.clone();
            act.outputs = vec![out];
            st.register(act)?;
            Ok(Value::None)
        })(),
        _ => return None,
    })
}
