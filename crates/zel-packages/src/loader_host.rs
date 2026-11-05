// Copyright 2026 The Bazel Authors. All rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//    http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

// Host-function half of `loader.rs` (included textually): native functions available to
// BUILD/.bzl files, rule instantiation, symbolic macros.

type HostResult = Result<Value, StarlarkError>;

fn herr(ctx: &EvalContext, msg: impl AsRef<str>) -> StarlarkError {
    StarlarkError(format!("{}: {}", fmt_loc(&ctx.last_call_loc), msg.as_ref()))
}

fn type_mismatch(param: &str, v: &Value, want: &str, func: &str) -> String {
    format!(
        "in call to {func}(), parameter '{param}' got value of type '{}', want '{want}'",
        v.type_name()
    )
}

fn str_list_arg(v: &Value, what: &str) -> Result<Vec<String>, String> {
    sl_values(v).map_err(|_| {
        format!(
            "expected value of type 'list(string)' for {what}, but got {} ({})",
            v.repr(),
            v.type_name()
        )
    })
}

fn to_macro_value(v: &AttrValue) -> Value {
    match v {
        AttrValue::Label(Some(l)) | AttrValue::Output(Some(l)) => Value::Label(l.clone()),
        AttrValue::LabelList(l) | AttrValue::OutputList(l) => {
            mk_list(l.iter().map(|x| Value::Label(x.clone())).collect())
        }
        AttrValue::LabelKeyedStrDict(d) => mk_dict(
            d.iter()
                .map(|(k, v)| (Value::Label(k.clone()), Value::String(v.clone())))
                .collect(),
        ),
        AttrValue::StrKeyedLabelDict(d) => mk_dict(
            d.iter()
                .map(|(k, v)| (Value::String(k.clone()), Value::Label(v.clone())))
                .collect(),
        ),
        other => other.to_value(),
    }
}

/// `nameIsWithinMacroNamespace`.
fn name_within_macro_namespace(name: &str, macro_name: &str) -> bool {
    if name == macro_name {
        return true;
    }
    if let Some(suffix) = name.strip_prefix(macro_name) {
        return suffix.len() >= 2
            && (suffix.starts_with('_') || suffix.starts_with('.') || suffix.starts_with('-'));
    }
    false
}

/// `StarlarkNativeModule.starlarkifyValue` for `existing_rule(s)`.
fn starlarkify(v: &AttrValue, pkg: &PackageIdentifier) -> Option<Value> {
    fn lab(l: &Label, pkg: &PackageIdentifier) -> Value {
        if l.package_identifier() == pkg {
            Value::String(format!(":{}", l.name()))
        } else if l.repository() == pkg.repository() {
            Value::String(format!("//{}:{}", l.package_name(), l.name()))
        } else {
            Value::String(l.canonical_form())
        }
    }
    Some(match v {
        AttrValue::Label(Some(l)) | AttrValue::Output(Some(l)) => lab(l, pkg),
        AttrValue::Label(None) | AttrValue::Output(None) => return None,
        AttrValue::LabelList(l) | AttrValue::OutputList(l) => {
            Value::Tuple(l.iter().map(|x| lab(x, pkg)).collect())
        }
        AttrValue::StrList(l) => Value::Tuple(l.iter().map(|s| Value::String(s.clone())).collect()),
        AttrValue::LabelKeyedStrDict(d) => mk_dict(
            d.iter()
                .map(|(k, v)| (lab(k, pkg), Value::String(v.clone())))
                .collect(),
        ),
        AttrValue::Tristate(t) => int(i64::from(*t)),
        other => other.to_value(),
    })
}

impl PackageLoader {
    fn dispatch_host(
        self: &Arc<Self>,
        ctx: &mut EvalContext,
        state: Option<&Arc<Mutex<PackageState>>>,
        owner: &PackageIdentifier,
        name: &str,
        pos: Vec<Value>,
        kw: BTreeMap<String, Value>,
    ) -> HostResult {
        if crate::loader::is_external_host_call(name) {
            if let Some(h) = self.cfg.external_host.clone() {
                return h(ctx, name, pos, kw);
            }
        }
        if let Some(r) = zel_builtins::call(name, pos.clone(), kw.clone()) {
            return r;
        }
        // `--incompatible_enable_proto_toolchain_resolution` defaults to true in Bazel 9.
        if name == "proto_common_do_not_use.incompatible_enable_proto_toolchain_resolution" {
            return Ok(Value::Bool(true));
        }
        if name == "visibility" {
            return Ok(Value::None);
        }
        if name == "cc_common.internal_DO_NOT_USE" {
            return Ok(zel_builtins::cc_internal::internal_struct());
        }
        if name.starts_with("cc_internal.") {
            if let Some(r) = zel_builtins::call(name, pos.clone(), kw.clone()) {
                return r;
            }
        }
        match name {
            "rule" => return self.define_rule(ctx, owner, pos, kw),
            "macro" => return self.define_macro(ctx, owner, pos, kw),
            "aspect" => return self.define_aspect(ctx, owner, pos, kw),
            "subrule" => return self.define_subrule(ctx, owner, pos, kw),
            "Label" => return self.label_fn(ctx, owner, &pos),
            n if n.starts_with("attr.") => return self.attr_fn(ctx, &n[5..], pos, kw),
            "call:rule" => return self.call_starlark_rule(ctx, state, pos, kw),
            "provider" => return crate::provider::define_provider(ctx, pos, kw),
            "transition" => {
                let mut f: BTreeMap<String, Value> = BTreeMap::new();
                for (i, k) in ["implementation", "inputs", "outputs"].iter().enumerate() {
                    if let Some(v) = arg(&pos, &kw, i, k) {
                        f.insert((*k).to_string(), v.clone());
                    }
                }
                f.insert("_transition".into(), Value::Bool(true));
                // `StarlarkDefinedConfigTransition.getLocation()` (printed by `cquery --transitions`).
                f.insert("location".into(), Value::String(fmt_loc(&ctx.last_call_loc)));
                return Ok(Value::Struct(Arc::new(StructValue::new(f))));
            }
            "analysis_test_transition" => {
                let settings = arg(&pos, &kw, 0, "settings").cloned().ok_or_else(|| {
                    herr(ctx, "analysis_test_transition() missing 1 required positional argument: settings")
                })?;
                if !matches!(settings, Value::Dict(_)) {
                    return Err(herr(ctx, type_mismatch("settings", &settings, "dict", "analysis_test_transition")));
                }
                let mut f: BTreeMap<String, Value> = BTreeMap::new();
                f.insert("settings".into(), settings);
                f.insert("_analysis_test_transition".into(), Value::Bool(true));
                return Ok(Value::Struct(Arc::new(StructValue::new(f))));
            }
            "configuration_field" => {
                let mut f: BTreeMap<String, Value> = BTreeMap::new();
                for (i, k) in ["fragment", "name"].iter().enumerate() {
                    match arg(&pos, &kw, i, k) {
                        Some(v @ Value::String(_)) => {
                            f.insert((*k).to_string(), v.clone());
                        }
                        _ => {
                            return Err(herr(
                                ctx,
                                format!("configuration_field() missing or non-string argument '{k}'"),
                            ))
                        }
                    }
                }
                f.insert("_configuration_field".into(), Value::Bool(true));
                return Ok(Value::Struct(Arc::new(StructValue::new(f))));
            }
            "exec_group" => {
                let mut f = kw.clone();
                f.insert("_exec_group".into(), Value::Bool(true));
                return Ok(Value::Struct(Arc::new(StructValue::new(f))));
            }
            "apple_common.apple_toolchain" => {
                return Ok(Value::Struct(Arc::new(StructValue::default())));
            }
            "config_common.toolchain_type" => {
                let mut f: BTreeMap<String, Value> = BTreeMap::new();
                if let Some(v) = arg(&pos, &kw, 0, "name") {
                    f.insert("name".into(), v.clone());
                }
                f.insert(
                    "mandatory".into(),
                    kw.get("mandatory").cloned().unwrap_or(Value::Bool(true)),
                );
                f.insert("_toolchain_type".into(), Value::Bool(true));
                return Ok(Value::Struct(Arc::new(StructValue::new(f))));
            }
            n if n.starts_with("config.") => {
                let mut f = kw.clone();
                f.insert("_build_setting".into(), Value::String(n[7..].to_string()));
                return Ok(Value::Struct(Arc::new(StructValue::new(f))));
            }
            "call:provider" => return crate::provider::call_provider(ctx, false, pos, kw),
            "call:provider_raw" => return crate::provider::call_provider(ctx, true, pos, kw),
            "call:macro" => return self.call_macro(ctx, state, pos, kw),
            _ => {}
        }
        if let Some(class) = name.strip_prefix("rule.") {
            let rc = self
                .rule_classes
                .get(class)
                .cloned()
                .ok_or_else(|| herr(ctx, format!("unknown rule class '{class}'")))?;
            let st = state.ok_or_else(|| {
                herr(
                    ctx,
                    format!(
                        "{class}() can only be used while evaluating a BUILD file (and macros called from it)"
                    ),
                )
            })?;
            return self.instantiate_rule(ctx, st, &rc, &pos, kw);
        }
        let Some(st) = state else {
            return Err(herr(
                ctx,
                format!(
                    "{name}() can only be used while evaluating a BUILD file (and macros called from it)"
                ),
            ));
        };
        match name {
            "glob" => self.native_glob(ctx, st, &pos, &kw),
            "subpackages" => self.native_subpackages(ctx, st, &pos, &kw),
            "package" => self.native_package(ctx, st, &pos, &kw),
            "exports_files" => self.native_exports_files(ctx, st, &pos, &kw),
            "package_group" => self.native_package_group(ctx, st, &pos, &kw),
            "licenses" => {
                let l = arg(&pos, &kw, 0, "license_types")
                    .ok_or_else(|| herr(ctx, "licenses() missing 1 required positional argument: license_types"))?;
                let l = str_list_arg(l, "'licenses' operand").map_err(|e| herr(ctx, e))?;
                st.lock().unwrap().args.licenses = l;
                Ok(Value::None)
            }
            "existing_rule" => {
                let n = arg(&pos, &kw, 0, "name")
                    .ok_or_else(|| herr(ctx, "existing_rule() missing 1 required positional argument: name"))?;
                let Value::String(n) = n else {
                    return Err(herr(ctx, type_mismatch("name", n, "string", "existing_rule")));
                };
                let s = st.lock().unwrap();
                Ok(self.existing_rule_view(&s, n).unwrap_or(Value::None))
            }
            "existing_rules" => {
                let s = st.lock().unwrap();
                let mut items = Vec::new();
                for (n, t) in &s.targets {
                    if let Target::Rule(_) = t {
                        if let Some(v) = self.existing_rule_view(&s, n) {
                            items.push((Value::String(n.clone()), v));
                        }
                    }
                }
                Ok(mk_dict(items))
            }
            "package_name" => Ok(Value::String(
                st.lock().unwrap().id.package_fragment().path_string().to_string(),
            )),
            "repository_name" => Ok(Value::String(
                format!("@{}", st.lock().unwrap().id.repository().name()),
            )),
            "repo_name" => Ok(Value::String(
                st.lock().unwrap().id.repository().name().to_string(),
            )),
            "module_name" | "module_version" => Ok(Value::None),
            "package_default_visibility" => {
                let s = st.lock().unwrap();
                let labels = s
                    .args
                    .default_visibility
                    .clone()
                    .unwrap_or_else(|| vec![Label::parse_canonical("//visibility:private").unwrap()]);
                Ok(mk_list(labels.into_iter().map(Value::Label).collect()))
            }
            "package_relative_label" => {
                let v = arg(&pos, &kw, 0, "input")
                    .ok_or_else(|| herr(ctx, "package_relative_label() missing 1 required positional argument: input"))?;
                match v {
                    Value::Label(_) => Ok(v.clone()),
                    Value::String(s) => {
                        let s2 = st.lock().unwrap();
                        let cctx = ConvertCtx {
                            pkg: &s2.id,
                            mapping: &s2.mapping,
                            rule_class: "package_relative_label",
                        };
                        convert_label(s, &cctx)
                            .map(Value::Label)
                            .map_err(|e| herr(ctx, e))
                    }
                    other => Err(herr(
                        ctx,
                        format!(
                            "in call to package_relative_label(), parameter 'input' got value of type '{}', want 'string or Label'",
                            other.type_name()
                        ),
                    )),
                }
            }
            "environment_group" => self.native_environment_group(ctx, st, &pos, &kw),
            other => Err(herr(ctx, format!("unknown native function '{other}'"))),
        }
    }

    fn label_fn(&self, ctx: &EvalContext, owner: &PackageIdentifier, pos: &[Value]) -> HostResult {
        let v = pos
            .first()
            .ok_or_else(|| herr(ctx, "Label() missing 1 required positional argument: input"))?;
        match v {
            Value::Label(_) => Ok(v.clone()),
            Value::String(s) => {
                // A function defined in a .bzl resolves labels against that .bzl's repository,
                // even when it is called from a BUILD file of another repository.
                let defining = ctx.current_label_ctx();
                let (pkg, mapping) = match &defining {
                    Some((p, m)) => (p, m.clone()),
                    None => (owner, self.cfg.lookup.resolver.mapping(owner.repository())),
                };
                let cctx = ConvertCtx {
                    pkg,
                    mapping: &mapping,
                    rule_class: "Label",
                };
                convert_label(s, &cctx).map(Value::Label).map_err(|e| herr(ctx, e))
            }
            other => Err(herr(
                ctx,
                format!(
                    "in call to Label(), parameter 'input' got value of type '{}', want 'string or Label'",
                    other.type_name()
                ),
            )),
        }
    }

    // ------------------------------------------------------------------ native functions

    fn native_glob(
        &self,
        ctx: &EvalContext,
        st: &Arc<Mutex<PackageState>>,
        pos: &[Value],
        kw: &BTreeMap<String, Value>,
    ) -> HostResult {
        if pos.len() > 1 {
            return Err(herr(
                ctx,
                format!(
                    "glob() accepts no more than 1 positional argument but got {}",
                    pos.len()
                ),
            ));
        }
        check_kwargs("glob", kw, &["include", "exclude", "exclude_directories", "allow_empty"])
            .map_err(|e| herr(ctx, e))?;
        let include = match arg(pos, kw, 0, "include") {
            Some(v) => str_list_arg(v, "'glob' argument").map_err(|e| herr(ctx, e))?,
            None => Vec::new(),
        };
        let exclude = match kw.get("exclude") {
            Some(v) => str_list_arg(v, "'glob' argument").map_err(|e| herr(ctx, e))?,
            None => Vec::new(),
        };
        let op = match kw.get("exclude_directories") {
            None => GlobOp::Files,
            Some(Value::Int(i)) => {
                if i.is_zero() {
                    GlobOp::FilesAndDirs
                } else {
                    GlobOp::Files
                }
            }
            Some(other) => {
                return Err(herr(
                    ctx,
                    type_mismatch("exclude_directories", other, "int", "glob"),
                ))
            }
        };
        let allow_empty = match kw.get("allow_empty") {
            None => self.cfg.allow_empty_glob,
            Some(Value::Bool(b)) => *b,
            Some(other) => {
                return Err(herr(
                    ctx,
                    format!(
                        "expected boolean for argument `allow_empty`, got `{}`",
                        other.repr()
                    ),
                ))
            }
        };
        let matches = self.run_glob(ctx, st, &include, &exclude, op, allow_empty)?;
        Ok(mk_list(
            matches
                .into_iter()
                .map(|m| {
                    if m.starts_with('@') {
                        Value::String(format!(":{m}"))
                    } else {
                        Value::String(m)
                    }
                })
                .collect(),
        ))
    }

    fn run_glob(
        &self,
        ctx: &EvalContext,
        st: &Arc<Mutex<PackageState>>,
        include: &[String],
        exclude: &[String],
        op: GlobOp,
        allow_empty: bool,
    ) -> Result<Vec<String>, StarlarkError> {
        for p in include.iter().chain(exclude.iter()) {
            if let Some(e) = glob::check_pattern_for_error(p) {
                return Err(herr(ctx, format!("{e} (in glob pattern '{p}')")));
            }
        }
        let (pkg_dir, is_root_pkg) = {
            let s = st.lock().unwrap();
            (
                s.pkg_dir.clone(),
                s.id.repository().is_main() && s.id.package_fragment().is_empty(),
            )
        };
        let gctx = GlobContext {
            fs: self.fs(),
            package_dir: &pkg_dir,
            skip_convenience_symlinks: is_root_pkg,
        };
        let r = glob::evaluate(&gctx, include, exclude, op);
        if !allow_empty {
            let what = if op == GlobOp::Subpackages { "subpackages" } else { "glob" };
            if let Some(p) = r.empty_includes.first() {
                return Err(herr(
                    ctx,
                    if op == GlobOp::Subpackages {
                        format!("subpackages pattern '{p}' didn't match anything, but allow_empty is set to False (the default value)")
                    } else {
                        format!("glob pattern '{p}' didn't match anything, but allow_empty is set to False (the default value of allow_empty can be set with --incompatible_disallow_empty_glob).")
                    },
                ));
            }
            if r.all_excluded || (include.is_empty() && r.matches.is_empty()) {
                let _ = what;
                return Err(herr(
                    ctx,
                    if op == GlobOp::Subpackages {
                        "all subpackages in subpackages() have been excluded, but allow_empty is set to False ".to_string()
                    } else {
                        "all files in the glob have been excluded, but allow_empty is set to False (the default value of allow_empty can be set with --incompatible_disallow_empty_glob).".to_string()
                    },
                ));
            }
        }
        Ok(r.matches)
    }

    fn native_subpackages(
        &self,
        ctx: &EvalContext,
        st: &Arc<Mutex<PackageState>>,
        pos: &[Value],
        kw: &BTreeMap<String, Value>,
    ) -> HostResult {
        if pos.len() > 1 {
            return Err(herr(
                ctx,
                format!(
                    "subpackages() accepts no more than 1 positional argument but got {}",
                    pos.len()
                ),
            ));
        }
        check_kwargs("subpackages", kw, &["include", "exclude", "allow_empty"])
            .map_err(|e| herr(ctx, e))?;
        let include = match arg(pos, kw, 0, "include") {
            Some(v) => str_list_arg(v, "'subpackages' argument").map_err(|e| herr(ctx, e))?,
            None => {
                return Err(herr(
                    ctx,
                    "subpackages() missing 1 required named argument: include",
                ))
            }
        };
        let exclude = match kw.get("exclude") {
            Some(v) => str_list_arg(v, "'subpackages' argument").map_err(|e| herr(ctx, e))?,
            None => Vec::new(),
        };
        let allow_empty = match kw.get("allow_empty") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(other) => {
                return Err(herr(
                    ctx,
                    type_mismatch("allow_empty", other, "bool", "subpackages"),
                ))
            }
        };
        let m = self.run_glob(ctx, st, &include, &exclude, GlobOp::Subpackages, allow_empty)?;
        Ok(mk_list(m.into_iter().map(Value::String).collect()))
    }

    fn native_package(
        &self,
        ctx: &EvalContext,
        st: &Arc<Mutex<PackageState>>,
        pos: &[Value],
        kw: &BTreeMap<String, Value>,
    ) -> HostResult {
        let mut s = st.lock().unwrap();
        if !s.macro_stack.is_empty() {
            return Err(herr(
                ctx,
                "package() can only be used while evaluating a BUILD file (not a symbolic macro)",
            ));
        }
        if s.package_called {
            return Err(herr(ctx, "'package' can only be used once per BUILD file"));
        }
        if !pos.is_empty() {
            return Err(herr(ctx, "package() accepts no positional arguments"));
        }
        if kw.is_empty() {
            return Err(herr(
                ctx,
                "at least one argument must be given to the 'package' function",
            ));
        }
        s.package_called = true;
        let id = s.id.clone();
        let mapping = s.mapping.clone();
        let cctx = ConvertCtx {
            pkg: &id,
            mapping: &mapping,
            rule_class: "package",
        };
        apply_package_args(&mut s.args, kw, &cctx).map_err(|e| herr(ctx, e))?;
        Ok(Value::None)
    }

    fn native_exports_files(
        &self,
        ctx: &EvalContext,
        st: &Arc<Mutex<PackageState>>,
        pos: &[Value],
        kw: &BTreeMap<String, Value>,
    ) -> HostResult {
        if pos.len() > 1 {
            return Err(herr(
                ctx,
                format!("exports_files() accepts no more than 1 positional argument but got {}", pos.len()),
            ));
        }
        check_kwargs("exports_files", kw, &["srcs", "visibility", "licenses"])
            .map_err(|e| herr(ctx, e))?;
        let srcs = arg(pos, kw, 0, "srcs")
            .ok_or_else(|| herr(ctx, "exports_files() missing 1 required positional argument: srcs"))?;
        let files = str_list_arg(srcs, "'exports_files' operand").map_err(|e| herr(ctx, e))?;
        let mut s = st.lock().unwrap();
        let id = s.id.clone();
        let mapping = s.mapping.clone();
        let cctx = ConvertCtx {
            pkg: &id,
            mapping: &mapping,
            rule_class: "exports_files",
        };
        let explicit_vis = match kw.get("visibility") {
            None | Some(Value::None) => None,
            Some(v) => {
                let a = Attribute::new("visibility", AttrType::LabelList);
                match convert_value(v, &a, &cctx).map_err(|e| herr(ctx, e))? {
                    AttrValue::LabelList(l) => {
                        parse_visibility(&l).map_err(|e| herr(ctx, e))?;
                        Some(l)
                    }
                    _ => None,
                }
            }
        };
        let mut visibility = explicit_vis
            .clone()
            .unwrap_or_else(|| vec![Label::parse_canonical("//visibility:public").unwrap()]);
        if let Some((_, def_pkg)) = s.macro_stack.last() {
            let extra = Label::create(def_pkg.clone(), "__pkg__").map_err(|e| herr(ctx, e.to_string()))?;
            visibility.push(extra);
        }
        let loc = fmt_loc(&ctx.last_call_loc);
        for f in files {
            if let Err(e) = zel_cmdline::validator::validate_target_name(&f) {
                return Err(herr(ctx, e));
            }
            match s.targets.get(&f) {
                Some(Target::InputFile { .. }) => {
                    if let Some(prev) = s.exported_files.get(&f) {
                        if explicit_vis.is_some() && *prev != visibility {
                            return Err(herr(
                                ctx,
                                format!("visibility for exported file '{f}' declared twice"),
                            ));
                        }
                    }
                }
                Some(_) => {
                    let e = Self::conflict_message(&s, &f, "source file", None);
                    return Err(herr(ctx, e));
                }
                None => {
                    let label = Label::create(id.clone(), &f).map_err(|e| herr(ctx, e.to_string()))?;
                    s.targets.insert(f.clone(), Target::InputFile { label });
                    s.target_locations.insert(f.clone(), loc.clone());
                    s.any_target_declared = true;
                }
            }
            s.exported_files.insert(f, visibility.clone());
        }
        Ok(Value::None)
    }

    fn conflict_message(s: &PackageState, name: &str, kind: &str, extra: Option<&str>) -> String {
        let existing = &s.targets[name];
        let subject = format!("{kind} '{name}'{}", extra.unwrap_or(""));
        let object = match existing {
            Target::OutputFile { generating_rule, .. } => {
                format!("generated file from rule '{}'", generating_rule.name())
            }
            other => other.target_kind(),
        };
        format!(
            "{subject} conflicts with existing {object}, defined at {}",
            s.target_locations.get(name).cloned().unwrap_or_default()
        )
    }

    fn native_package_group(
        &self,
        ctx: &EvalContext,
        st: &Arc<Mutex<PackageState>>,
        pos: &[Value],
        kw: &BTreeMap<String, Value>,
    ) -> HostResult {
        if !pos.is_empty() {
            return Err(herr(ctx, "package_group() accepts no positional arguments"));
        }
        check_kwargs("package_group", kw, &["name", "packages", "includes"])
            .map_err(|e| herr(ctx, e))?;
        let Some(name_v) = kw.get("name") else {
            return Err(herr(ctx, "package_group() missing 1 required named argument: name"));
        };
        let Value::String(name) = name_v else {
            return Err(herr(ctx, type_mismatch("name", name_v, "string", "package_group")));
        };
        let packages = match kw.get("packages") {
            Some(v) => str_list_arg(v, "'packages' argument").map_err(|e| herr(ctx, e))?,
            None => Vec::new(),
        };
        let mut s = st.lock().unwrap();
        let id = s.id.clone();
        let mapping = s.mapping.clone();
        let label = Label::create(id.clone(), name).map_err(|e| {
            herr(ctx, format!("package group has invalid name: {name}: {e}"))
        })?;
        let includes: Vec<Label> = match kw.get("includes") {
            Some(v) => {
                let cctx = ConvertCtx {
                    pkg: &id,
                    mapping: &mapping,
                    rule_class: "package_group",
                };
                match convert_plain(v, AttrType::LabelList, "includes", &cctx).map_err(|e| herr(ctx, e))? {
                    AttrValue::LabelList(l) => l,
                    _ => Vec::new(),
                }
            }
            None => Vec::new(),
        };
        for spec in &packages {
            PackageSpec::parse(&mapping, id.repository(), spec).map_err(|e| {
                herr(ctx, format!("package group '{}': {e}", label.canonical_form()))
            })?;
        }
        if s.targets.contains_key(name) {
            let e = Self::conflict_message(&s, name, "package group", None);
            return Err(herr(ctx, e));
        }
        let loc = fmt_loc(&ctx.last_call_loc);
        s.targets.insert(
            name.clone(),
            Target::PackageGroup {
                label,
                packages,
            },
        );
        s.target_locations.insert(name.clone(), loc);
        s.group_includes.insert(name.clone(), includes);
        s.any_target_declared = true;
        Ok(Value::None)
    }

    fn native_environment_group(
        &self,
        ctx: &EvalContext,
        st: &Arc<Mutex<PackageState>>,
        pos: &[Value],
        kw: &BTreeMap<String, Value>,
    ) -> HostResult {
        if !pos.is_empty() {
            return Err(herr(ctx, "environment_group() accepts no positional arguments"));
        }
        check_kwargs("environment_group", kw, &["name", "environments", "defaults"])
            .map_err(|e| herr(ctx, e))?;
        let mut s = st.lock().unwrap();
        let id = s.id.clone();
        let mapping = s.mapping.clone();
        let cctx = ConvertCtx {
            pkg: &id,
            mapping: &mapping,
            rule_class: "environment_group",
        };
        let name = match kw.get("name") {
            Some(Value::String(n)) => n.clone(),
            Some(other) => {
                return Err(herr(ctx, type_mismatch("name", other, "string", "environment_group")))
            }
            None => return Err(herr(ctx, "environment_group() missing 1 required named argument: name")),
        };
        let get_labels = |key: &str| -> Result<Vec<Label>, StarlarkError> {
            match kw.get(key) {
                Some(v) => match convert_plain(v, AttrType::LabelList, key, &cctx).map_err(|e| herr(ctx, e))? {
                    AttrValue::LabelList(l) => Ok(l),
                    _ => Ok(Vec::new()),
                },
                None => Err(herr(
                    ctx,
                    format!("environment_group() missing 1 required named argument: {key}"),
                )),
            }
        };
        let environments = get_labels("environments")?;
        let defaults = get_labels("defaults")?;
        for d in &defaults {
            if !environments.contains(d) {
                return Err(herr(
                    ctx,
                    format!(
                        "default environment '{}' is not a member of environment group '{name}'",
                        d.canonical_form()
                    ),
                ));
            }
        }
        if s.environment_groups.contains_key(&name) {
            return Err(herr(
                ctx,
                format!("environment group '{name}' conflicts with an existing environment group"),
            ));
        }
        if s.targets.contains_key(&name) {
            let e = Self::conflict_message(&s, &name, "environment group", None);
            return Err(herr(ctx, e));
        }
        let label = Label::create(id.clone(), &name).map_err(|e| herr(ctx, e.to_string()))?;
        let loc = fmt_loc(&ctx.last_call_loc);
        s.targets.insert(
            name.clone(),
            Target::EnvironmentGroup { label, environments: environments.clone(), defaults: defaults.clone() },
        );
        s.target_locations.insert(name.clone(), loc);
        s.any_target_declared = true;
        s.environment_groups.insert(name, (environments, defaults));
        Ok(Value::None)
    }

    fn existing_rule_view(&self, s: &PackageState, name: &str) -> Option<Value> {
        let Some(Target::Rule(r)) = s.targets.get(name) else {
            return None;
        };
        let mut items = vec![
            (Value::String("name".into()), Value::String(name.to_string())),
            (Value::String("kind".into()), Value::String(r.rule_class.clone())),
        ];
        let rc = r.extra.rule_class.as_ref();
        // Attributes in schema order (BTreeMap): explicit values, otherwise defaults.
        let mut names: BTreeSet<String> = r.extra.attr_values.keys().cloned().collect();
        if let Some(rc) = rc {
            names.extend(rc.attrs.keys().cloned());
        }
        for n in names {
            if n == "name" || !n.chars().next().map(|c| c.is_alphabetic()).unwrap_or(false) {
                continue;
            }
            let v = r.attr_or_default(&n)?;
            if let Some(val) = starlarkify(&v, &s.id) {
                items.push((Value::String(n), val));
            }
        }
        let d = mk_dict(items);
        d.freeze();
        Some(d)
    }

    // ------------------------------------------------------------------ rule instantiation

    fn instantiate_rule(
        &self,
        ctx: &mut EvalContext,
        st: &Arc<Mutex<PackageState>>,
        rc: &Arc<RuleClass>,
        pos: &[Value],
        kw: BTreeMap<String, Value>,
    ) -> HostResult {
        if !pos.is_empty() {
            return Err(herr(ctx, format!("{}() accepts no positional arguments", rc.name)));
        }
        let loc = fmt_loc(&ctx.last_call_loc);
        let mut s = st.lock().unwrap();
        let id = s.id.clone();
        let mapping = s.mapping.clone();
        let cctx = ConvertCtx {
            pkg: &id,
            mapping: &mapping,
            rule_class: &rc.name,
        };

        // name
        let name = match kw.get("name") {
            None | Some(Value::None) => {
                return Err(herr(
                    ctx,
                    format!("missing value for mandatory attribute 'name' in '{}' rule", rc.name),
                ))
            }
            Some(Value::String(n)) => n.clone(),
            Some(other) => {
                return Err(herr(
                    ctx,
                    format!(
                        "expected value of type 'string' for attribute 'name' in '{}' rule, but got {} ({})",
                        rc.name,
                        other.repr(),
                        other.type_name()
                    ),
                ))
            }
        };
        let label = Label::create(id.clone(), &name).map_err(|e| herr(ctx, e.to_string()))?;

        // `AttributeProvider`: the legacy spelling `applicable_licenses` means `package_metadata`.
        let mut kw = kw;
        if let Some(v) = kw.remove(RuleClass_APPLICABLE_METADATA_ATTR_ALT) {
            kw.entry("package_metadata".to_string()).or_insert(v);
        }
        // attributes
        let mut values: BTreeMap<String, AttrValue> = BTreeMap::new();
        for (k, v) in &kw {
            if k == "name" {
                continue;
            }
            if matches!(v, Value::None) {
                if rc.attr(k).map(|a| a.mandatory).unwrap_or(false) {
                    return Err(herr(
                        ctx,
                        format!("missing value for mandatory attribute '{k}' in '{}' rule", rc.name),
                    ));
                }
                if rc.attr(k).is_none() && !rc.open {
                    return Err(herr(ctx, no_such_attribute(&label, k, rc)));
                }
                continue;
            }
            let attr = match rc.attr(k) {
                Some(a) if !k.starts_with('_') => a.clone(),
                Some(_) | None if rc.open && !k.starts_with('_') => {
                    infer_attribute(k, v, &rc.name).map_err(|e| herr(ctx, e))?
                }
                _ => {
                    return Err(herr(ctx, no_such_attribute(&label, k, rc)))
                }
            };
            let conv = convert_value(v, &attr, &cctx).map_err(|e| herr(ctx, e))?;
            if let (Some(allowed), AttrValue::Str(sv)) = (&attr.allowed_values, &conv) {
                if !allowed.contains(sv) {
                    let list = allowed
                        .iter()
                        .map(|a| format!("'{a}'"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(herr(
                        ctx,
                        format!(
                            "invalid value in '{k}' attribute: has to be one of {list} instead of '{sv}'"
                        ),
                    ));
                }
            }
            if attr.non_empty && conv.is_empty() {
                return Err(herr(
                    ctx,
                    format!(
                        "in {k} attribute of {} rule {}: attribute must be non empty",
                        rc.name,
                        label.canonical_form()
                    ),
                ));
            }
            values.insert(k.clone(), sort_order_independent(conv, &attr));
        }
        for (an, a) in &rc.attrs {
            if a.mandatory && an != "name" && !values.contains_key(an) {
                return Err(herr(
                    ctx,
                    format!("missing value for mandatory attribute '{an}' in '{}' rule", rc.name),
                ));
            }
        }

        // Package-level defaults.
        let visibility_explicit = values.contains_key("visibility");
        let mut effective_vis: Vec<Label> = match values.get("visibility") {
            Some(AttrValue::LabelList(l)) => l.clone(),
            Some(AttrValue::Select(_)) => Vec::new(),
            _ => s.args.default_visibility.clone().unwrap_or_default(),
        };
        if let Some(AttrValue::LabelList(l)) = values.get("visibility") {
            if let Err(e) = parse_visibility(l) {
                return Err(herr(ctx, e));
            }
        }
        let macro_ctx = s.macro_stack.last().cloned();
        if let Some((_, def_pkg)) = &macro_ctx {
            if let Ok(extra) = Label::create(def_pkg.clone(), "__pkg__") {
                if !effective_vis.iter().any(|l| l.canonical_form() == "//visibility:public") {
                    effective_vis.push(extra);
                }
            }
        }
        if !visibility_explicit {
            if let Some(dv) = &s.args.default_visibility {
                values.insert("visibility".into(), AttrValue::LabelList(dv.clone()));
            }
        }
        // Attributes populated from `package()` defaults are not *explicitly specified*
        // (`Rule.isAttributeValueExplicitlySpecified`): `testonly`/`deprecation` are
        // `ComputedDefault`s over `PackageArgs`, so query output omits them.
        let mut pkg_default_attrs: Vec<String> = Vec::new();
        let mut own_features: Option<AttrValue> = None;
        if rc.attr("testonly").is_some() && !values.contains_key("testonly") {
            if let Some(t) = s.args.default_testonly {
                values.insert("testonly".into(), AttrValue::Bool(t));
                pkg_default_attrs.push("testonly".into());
            }
        }
        if rc.attr("deprecation").is_some() && !values.contains_key("deprecation") {
            if let Some(d) = &s.args.default_deprecation {
                values.insert("deprecation".into(), AttrValue::Str(d.clone()));
                pkg_default_attrs.push("deprecation".into());
            }
        }
        if !values.contains_key("compatible_with") && !s.args.default_compatible_with.is_empty() {
            values.insert(
                "compatible_with".into(),
                AttrValue::LabelList(s.args.default_compatible_with.clone()),
            );
            pkg_default_attrs.push("compatible_with".into());
        }
        if !values.contains_key("restricted_to") && !s.args.default_restricted_to.is_empty() {
            values.insert(
                "restricted_to".into(),
                AttrValue::LabelList(s.args.default_restricted_to.clone()),
            );
            pkg_default_attrs.push("restricted_to".into());
        }
        if !s.args.features.is_empty() {
            // (A configurable `features = select(...)` keeps its select; package defaults are
            // merged only into plain lists.)
            if !matches!(values.get("features"), Some(AttrValue::Select(_))) {
                // Bazel keeps the attribute as written and merges the package features elsewhere:
                // remember what query must show.
                match values.get("features") {
                    Some(own) => own_features = Some(own.clone()),
                    None => pkg_default_attrs.push("features".into()),
                }
                let mut f = match values.get("features") {
                    Some(AttrValue::StrList(l)) => l.clone(),
                    _ => Vec::new(),
                };
                for x in &s.args.features {
                    if !f.contains(x) {
                        f.push(x.clone());
                    }
                }
                values.insert("features".into(), AttrValue::StrList(f));
            }
        }
        if !values.contains_key("package_metadata") && !s.args.default_package_metadata.is_empty() {
            values.insert(
                "package_metadata".into(),
                AttrValue::LabelList(s.args.default_package_metadata.clone()),
            );
            pkg_default_attrs.push("package_metadata".into());
        }

        // Non-fatal validations.
        let mut soft_errors: Vec<String> = Vec::new();
        if rc.attr("size").is_some() {
            let size = match values.get("size").cloned().or_else(|| rc.attr("size").map(|a| a.default_value())) {
                Some(AttrValue::Str(v)) => v,
                _ => "medium".to_string(),
            };
            if !["small", "medium", "large", "enormous"].contains(&size.as_str()) {
                soft_errors.push(format!("In rule '{name}', size '{size}' is not a valid size."));
            }
        }
        if let Some(AttrValue::Str(t)) = values.get("timeout") {
            if !t.is_empty() && !["short", "moderate", "long", "eternal"].contains(&t.as_str()) {
                soft_errors.push(format!("In rule '{name}', timeout '{t}' is not a valid timeout."));
            }
        }
        for (an, v) in &values {
            let mut check = |labels: &[Label]| {
                let mut seen = BTreeSet::new();
                let mut reported = BTreeSet::new();
                for l in labels {
                    let f = l.canonical_form();
                    if !seen.insert(f.clone()) && reported.insert(f.clone()) {
                        soft_errors.push(format!(
                            "Label '{f}' is duplicated in the '{an}' attribute of rule '{name}'"
                        ));
                    }
                }
            };
            match v {
                AttrValue::LabelList(l) => check(l),
                AttrValue::Select(parts) => {
                    for p in parts {
                        match p {
                            SelectPart::Plain(AttrValue::LabelList(l)) => check(l),
                            SelectPart::Select { entries, .. } => {
                                for (_, ev) in entries {
                                    if let AttrValue::LabelList(l) = ev {
                                        check(l)
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        // Outputs.
        let mut out_labels: Vec<Label> = Vec::new();
        for a in rc.attrs.values() {
            match values.get(&a.name) {
                Some(AttrValue::Output(Some(l))) => out_labels.push(l.clone()),
                Some(AttrValue::OutputList(l)) => out_labels.extend(l.iter().cloned()),
                _ => {}
            }
        }
        for (_, tmpl) in &rc.implicit_outputs {
            let mut text = tmpl.replace("%{name}", &name);
            for (an, av) in &values {
                if let AttrValue::Str(sv) = av {
                    text = text.replace(&format!("%{{{an}}}"), sv);
                }
            }
            let l = Label::create(id.clone(), &text).map_err(|e| herr(ctx, e.to_string()))?;
            out_labels.push(l);
        }

        // Name conflicts (TargetRecorder.checkRuleAndOutputs).
        let kind = format!("{} rule", rc.name);
        if s.targets.contains_key(&name) {
            // `NameConflictException` is an `EvalException` of the rule function: it carries the
            // Starlark traceback of the BUILD call.
            let e = Self::conflict_message(&s, &name, &kind, None);
            let trace = ctx.thread.format_stack_trace_at(&ctx.last_call_loc);
            return Err(StarlarkError(format!("{trace}Error in {}: {e}", rc.name)));
        }
        if let Some(mloc) = s.macro_names.get(&name) {
            let in_own_macro = macro_ctx.as_ref().map(|(m, _)| m == &name).unwrap_or(false);
            if !in_own_macro {
                return Err(herr(
                    ctx,
                    format!(
                        "{kind} '{name}' conflicts with an existing macro (and was not created by it), defined at {mloc}"
                    ),
                ));
            }
        }
        let mut seen_outs: BTreeMap<String, ()> = BTreeMap::new();
        let mut new_prefixes: Vec<(String, String)> = Vec::new();
        for ol in &out_labels {
            let on = ol.name().to_string();
            if seen_outs.insert(on.clone(), ()).is_some() {
                return Err(herr(
                    ctx,
                    format!("rule '{name}' has more than one generated file named '{on}'"),
                ));
            }
            if s.targets.contains_key(&on) {
                let e = Self::conflict_message(&s, &on, "generated file", Some(&format!(" in rule '{name}'")));
                return Err(herr(ctx, e));
            }
            if on == name {
                return Err(herr(
                    ctx,
                    format!("generated file '{on}' in rule '{name}' conflicts with existing {kind}, defined at {loc}"),
                ));
            }
            if let Some(other) = s.output_prefixes.get(&on) {
                return Err(herr(
                    ctx,
                    format!(
                        "output file '{on}' of rule '{name}' conflicts with output file '{other}' of rule '{}'",
                        s.targets.get(other).and_then(|t| match t {
                            Target::OutputFile { generating_rule, .. } => Some(generating_rule.name().to_string()),
                            _ => None,
                        }).unwrap_or_default()
                    ),
                ));
            }
            if on.contains('/') {
                let parts: Vec<&str> = on.split('/').collect();
                for k in 1..parts.len() {
                    let prefix = parts[..k].join("/");
                    if seen_outs.contains_key(&prefix) {
                        return Err(herr(
                            ctx,
                            format!("rule '{name}' has conflicting output files '{on}' and '{prefix}'"),
                        ));
                    }
                    if let Some(Target::OutputFile { generating_rule, .. }) = s.targets.get(&prefix) {
                        return Err(herr(
                            ctx,
                            format!(
                                "output file '{on}' of rule '{name}' conflicts with output file '{prefix}' of rule '{}'",
                                generating_rule.name()
                            ),
                        ));
                    }
                    new_prefixes.push((prefix, on.clone()));
                }
            }
        }
        // Same file as input and output.
        for v in values.values() {
            for l in v.labels() {
                if l.package_identifier() == &id && seen_outs.contains_key(l.name()) {
                    return Err(herr(
                        ctx,
                        format!(
                            "rule '{name}' has file '{}' as both an input and an output",
                            l.name()
                        ),
                    ));
                }
            }
        }
        for (p, o) in new_prefixes {
            s.output_prefixes.entry(p).or_insert(o);
        }

        // Build the rule.
        let flat = |keys: &[&str]| -> Vec<String> {
            let mut out = Vec::new();
            for k in keys {
                if let Some(v) = values.get(*k) {
                    out.extend(v.flat_strings());
                }
            }
            out
        };
        let srcs = flat(&["srcs", "src"]);
        let deps = flat(&["deps", "actual", "crate"]);
        let tools = flat(&["tools", "tool", "exec_tools"]);
        let data = flat(&["data"]);
        let outs: Vec<String> = out_labels.iter().map(|l| l.name().to_string()).collect();
        let cmd = match values.get("cmd") {
            Some(AttrValue::Str(c)) => Some(c.clone()),
            _ => None,
        };
        let testonly = matches!(values.get("testonly"), Some(AttrValue::Bool(true)));
        let executable = match values.get("executable") {
            Some(AttrValue::Bool(b)) => *b,
            _ => rc.is_executable,
        };
        let mut attrs: BTreeMap<String, Value> = BTreeMap::new();
        attrs.insert("name".into(), Value::String(name.clone()));
        for (k, v) in &values {
            attrs.insert(k.clone(), v.to_value());
        }
        let generator_name = macro_ctx.as_ref().map(|(m, _)| m.clone()).or_else(|| {
            match values.get("generator_name") {
                Some(AttrValue::Str(s)) if !s.is_empty() => Some(s.clone()),
                _ => {
                    if !ctx.thread.frames.is_empty() {
                        let macro_name = ctx.thread.frames.first().and_then(|f| {
                            match f.locals.lock().unwrap().get("name") {
                                Some(Value::String(s)) => Some(s.clone()),
                                _ => None,
                            }
                        });
                        macro_name.or_else(|| Some(name.clone()))
                    } else {
                        None
                    }
                }
            }
        });
        let macro_frames = s.macro_frames.clone();
        let generator_function = match values.get("generator_function") {
            Some(AttrValue::Str(s)) if !s.is_empty() => Some(s.clone()),
            _ => {
                if let Some((_, sym, _)) = macro_frames.last() {
                    Some(sym.clone())
                } else if let Some(f) = ctx.thread.frames.first() {
                    Some(f.function_name.clone())
                } else {
                    None
                }
            }
        };
        let rule_loc = if let Some((call, _, _)) = macro_frames.first() {
            call.clone()
        } else if let Some(f) = ctx.thread.frames.first() {
            f.location.to_string()
        } else {
            loc.clone()
        };
        // `Rule.java`: the generator location is made relative with the quirk
        // `absolutePath.substring(absolutePath.indexOf(packageName))`.
        let generator_location = match values.get("generator_location") {
            Some(AttrValue::Str(s)) if !s.is_empty() => Some(s.clone()),
            _ => {
                let raw = if let Some((call, _, _)) = macro_frames.first() {
                    Some(call.clone())
                } else {
                    ctx.thread.frames.first().map(|f| f.location.to_string())
                };
                raw.map(|raw| {
                    let pkg_str = id.package_fragment().path_string();
                    if !pkg_str.is_empty() {
                        match raw.find(pkg_str) {
                            Some(pos) => raw[pos..].to_string(),
                            None => raw,
                        }
                    } else if let Some(pos) = raw.find("BUILD") {
                        raw[pos..].to_string()
                    } else {
                        raw
                    }
                })
            }
        };
        let mut instantiation_stack = Vec::new();
        if let Some((call, _, _)) = macro_frames.first() {
            instantiation_stack.push((call.clone(), "<toplevel>".to_string()));
            for (_, sym, dloc) in &macro_frames {
                instantiation_stack.push((dloc.clone(), sym.clone()));
            }
            // The call inside the macro's implementation function.
            let imp_name = ctx.thread.frames.last().map(|f| f.function_name.clone()).unwrap_or_default();
            instantiation_stack.push((loc.clone(), imp_name));
        } else if ctx.thread.frames.is_empty() {
            instantiation_stack.push((loc.clone(), "<toplevel>".to_string()));
        } else {
            instantiation_stack.push((ctx.thread.frames[0].location.to_string(), "<toplevel>".to_string()));
            for i in 1..ctx.thread.frames.len() {
                instantiation_stack.push((ctx.thread.frames[i].location.to_string(), ctx.thread.frames[i - 1].function_name.clone()));
            }
            let last_func = ctx.thread.frames.last().unwrap().function_name.clone();
            instantiation_stack.push((loc.clone(), last_func));
        }
        let rule = RuleInstance {
            label: label.clone(),
            rule_class: rc.name.clone(),
            attrs,
            srcs,
            deps,
            outs,
            tools,
            data,
            visibility: effective_vis.iter().map(|l| l.canonical_form()).collect(),
            cmd,
            executable,
            testonly,
            extra: RuleExtra {
                rule_class: Some(Arc::clone(rc)),
                attr_values: values,
                location: Some(rule_loc.clone()),
                generator_name: generator_name.clone(),
                generator_function,
                generator_location,
                instantiation_stack,
                macro_instance: generator_name.clone(),
                effective_visibility: effective_vis,
                visibility_explicit,
                pkg_default_attrs,
                own_features,
            },
        };
        for ol in out_labels {
            s.targets.insert(
                ol.name().to_string(),
                Target::OutputFile {
                    label: ol.clone(),
                    generating_rule: label.clone(),
                },
            );
            s.target_locations.insert(ol.name().to_string(), rule_loc.clone());
            // Only symbolic macros (`macro()`) have a namespace; legacy macros may name targets freely.
            if let (Some(m), true) = (&generator_name, macro_ctx.is_some()) {
                if !name_within_macro_namespace(ol.name(), m) {
                    s.macro_violations.insert(ol.name().to_string(), m.clone());
                }
            }
        }
        if let (Some(m), true) = (&generator_name, macro_ctx.is_some()) {
            if !name_within_macro_namespace(&name, m) {
                s.macro_violations.insert(name.clone(), m.clone());
            }
        }
        s.targets.insert(name.clone(), Target::Rule(rule));
        s.target_locations.insert(name, rule_loc.clone());
        s.any_target_declared = true;
        for e in soft_errors {
            s.error(Some(loc.clone()), e);
        }
        Ok(Value::None)
    }

    // ------------------------------------------------------------------ rule()/attr/macro

    fn attr_fn(
        &self,
        ctx: &EvalContext,
        kind: &str,
        pos: Vec<Value>,
        kw: BTreeMap<String, Value>,
    ) -> HostResult {
        let allowed: &[&str] = match kind {
            "int" | "string" | "bool" => &["default", "doc", "mandatory", "values", "configurable"],
            "string_list" | "int_list" => &["mandatory", "allow_empty", "default", "doc", "configurable"],
            "string_dict" | "string_list_dict" => &["allow_empty", "mandatory", "default", "doc", "configurable"],
            "label" => &[
                "default", "doc", "executable", "allow_files", "allow_single_file", "mandatory",
                "providers", "allow_rules", "cfg", "aspects", "flags", "configurable",
            ],
            "label_list" | "label_keyed_string_dict" | "string_keyed_label_dict" => &[
                "allow_empty", "default", "doc", "allow_files", "allow_rules", "providers", "flags",
                "mandatory", "cfg", "aspects", "configurable",
            ],
            "output" => &["doc", "mandatory", "configurable"],
            "output_list" => &["allow_empty", "doc", "mandatory", "configurable"],
            other => {
                return Err(herr(ctx, format!("attr.{other}() is not supported by this loader")))
            }
        };
        let fname = format!("attr.{kind}");
        check_kwargs(&fname, &kw, allowed).map_err(|e| herr(ctx, e))?;
        let mut fields: BTreeMap<String, Value> = BTreeMap::new();
        fields.insert("_attr_type".into(), Value::String(kind.to_string()));
        if let Some(d) = pos.first() {
            if allowed.contains(&"default") {
                fields.insert("default".into(), d.clone());
            }
        }
        for (k, v) in kw {
            fields.insert(k, v);
        }
        Ok(Value::Struct(Arc::new(StructValue::new(fields))))
    }

    fn attr_from_value(
        &self,
        name: &str,
        v: &Value,
        pkg: &PackageIdentifier,
    ) -> Result<Attribute, String> {
        let Value::Struct(s) = v else {
            return Err(format!(
                "got {} for dictionary value of attrs entry '{name}', want Attribute",
                v.type_name()
            ));
        };
        let Some(Value::String(kind)) = s.fields.get("_attr_type") else {
            return Err(format!(
                "got struct for dictionary value of attrs entry '{name}', want Attribute"
            ));
        };
        let ty = match kind.as_str() {
            "int" => AttrType::Int,
            "string" => AttrType::String,
            "bool" => AttrType::Bool,
            "string_list" => AttrType::StringList,
            "string_dict" => AttrType::StringDict,
            "string_list_dict" => AttrType::StringListDict,
            "label" => AttrType::Label,
            "label_list" => AttrType::LabelList,
            "label_keyed_string_dict" => AttrType::LabelKeyedStringDict,
            "string_keyed_label_dict" => AttrType::StringKeyedLabelDict,
            "int_list" => AttrType::IntList,
            "output" => AttrType::Output,
            "output_list" => AttrType::OutputList,
            other => return Err(format!("unsupported attribute type attr.{other}")),
        };
        let mut a = Attribute::new(name, ty);
        if let Some(Value::Bool(true)) = s.fields.get("mandatory") {
            a.mandatory = true;
        }
        if matches!(s.fields.get("allow_empty"), Some(Value::Bool(false)))
            || matches!(s.fields.get("non_empty"), Some(Value::Bool(true)))
        {
            a.non_empty = true;
        }
        if let Some(vals) = s.fields.get("values") {
            if let Ok(l) = sl_values(vals) {
                if !l.is_empty() {
                    a.allowed_values = Some(l);
                }
            }
        }
        if let Some(Value::Bool(true)) = s.fields.get("executable") {
            a.executable = true;
        }
        match s.fields.get("cfg") {
            Some(Value::String(c)) if c == "exec" || c == "host" => a.cfg_exec = true,
            Some(Value::String(_)) | Some(Value::None) | None => {}
            // `config.exec()` / `config.exec(exec_group = ...)`, `config.target()`, `config.none()`.
            Some(Value::Struct(cs)) if cs.fields.contains_key("_build_setting") && !cs.fields.contains_key("implementation") => {
                match cs.fields.get("_build_setting") {
                    Some(Value::String(k)) if k == "exec" => {
                        a.cfg_exec = true;
                        if let Some(Value::String(g)) = cs.fields.get("exec_group") {
                            a.cfg_exec_group = Some(g.clone());
                        }
                    }
                    _ => {}
                }
            }
            Some(other) => a.cfg_transition = Some(other.clone()),
        }
        if let Some(p) = s.fields.get("providers") {
            let items = zel_starlark::eval::builtins::value_to_sequence(p).unwrap_or_default();
            let all_lists = !items.is_empty()
                && items.iter().all(|i| matches!(i, Value::List(_) | Value::Tuple(_)));
            let groups: Vec<Vec<Value>> = if all_lists {
                items
                    .iter()
                    .map(|i| zel_starlark::eval::builtins::value_to_sequence(i).unwrap_or_default())
                    .collect()
            } else if items.is_empty() {
                Vec::new()
            } else {
                vec![items]
            };
            a.providers = groups
                .iter()
                .map(|g| g.iter().filter_map(crate::provider::provider_ref_of).collect())
                .collect();
        }
        if let Some(asp) = s.fields.get("aspects") {
            a.aspects = zel_starlark::eval::builtins::value_to_sequence(asp).unwrap_or_default();
        }
        if let Some(v @ (Value::List(_) | Value::Tuple(_))) = s.fields.get("allow_rules") {
            if let Ok(l) = sl_values(v) {
                a.allowed_rule_classes = Some(l);
            }
        }
        match s.fields.get("allow_single_file") {
            Some(Value::Bool(true)) => a.allow_single_file = true,
            // `allow_single_file = [".so", ...]`: a single file restricted to these extensions.
            Some(v @ (Value::List(_) | Value::Tuple(_))) => {
                a.allow_single_file = true;
                if let Ok(l) = sl_values(v) {
                    a.allowed_files = crate::attr::FileTypes::Extensions(l);
                }
            }
            _ => {}
        }
        // `configurable = False` (symbolic macro attributes): no `select()` values.
        if let Some(Value::Bool(false)) = s.fields.get("configurable") {
            a.configurable = false;
        }
        if matches!(a.ty, AttrType::Label | AttrType::LabelList)
            && matches!(s.fields.get("allow_files"), None | Some(Value::None))
            && matches!(s.fields.get("allow_single_file"), None | Some(Value::None))
        {
            a.files_unspecified = true;
        }
        match s.fields.get("allow_files") {
            Some(Value::Bool(false)) => a.allowed_files = crate::attr::FileTypes::NoFiles,
            Some(Value::List(_)) | Some(Value::Tuple(_)) => {
                if let Ok(l) = sl_values(s.fields.get("allow_files").unwrap()) {
                    a.allowed_files = crate::attr::FileTypes::Extensions(l);
                }
            }
            _ => {}
        }
        if let Some(Value::Struct(cf)) = s.fields.get("default") {
            if cf.fields.contains_key("_configuration_field") {
                if let (Some(Value::String(frag)), Some(Value::String(n))) =
                    (cf.fields.get("fragment"), cf.fields.get("name"))
                {
                    a.late_bound = Some((frag.clone(), n.clone()));
                }
            }
        }
        if let Some(d @ Value::UserFunction(_)) = s.fields.get("default") {
            a.computed_default = Some(d.clone());
        }
        if let Some(d) = s.fields.get("default") {
            let skip = a.late_bound.is_some()
                || matches!(d, Value::None | Value::UserFunction(_) | Value::BuiltinFunction(_));
            if !skip {
                if a.mandatory {
                    return Err(format!(
                        "attribute '{name}' has mandatory=True and a default value"
                    ));
                }
                let mapping = self.cfg.lookup.resolver.mapping(pkg.repository());
                let cctx = ConvertCtx {
                    pkg,
                    mapping: &mapping,
                    rule_class: "rule",
                };
                a.default = Some(
                    convert_value(d, &a, &cctx)
                        .map_err(|e| format!("for attribute '{name}': {e}"))?,
                );
            }
        }
        Ok(a)
    }

    /// `subrule(implementation, attrs, toolchains, fragments)`.
    fn define_subrule(
        &self,
        ctx: &mut EvalContext,
        owner: &PackageIdentifier,
        pos: Vec<Value>,
        kw: BTreeMap<String, Value>,
    ) -> HostResult {
        check_kwargs("subrule", &kw, &["implementation", "attrs", "toolchains", "fragments", "subrules"])
            .map_err(|e| herr(ctx, e))?;
        let implementation = arg(&pos, &kw, 0, "implementation")
            .cloned()
            .ok_or_else(|| herr(ctx, "subrule() missing 1 required positional argument: implementation"))?;
        let mut attrs = Vec::new();
        if let Some(a) = kw.get("attrs") {
            let Value::Dict(d) = a else {
                return Err(herr(ctx, type_mismatch("attrs", a, "dict", "subrule")));
            };
            for (k, v) in d.lock().unwrap().items() {
                let Value::String(k) = k else { return Err(herr(ctx, "attrs keys must be strings")) };
                if !k.starts_with('_') {
                    return Err(herr(
                        ctx,
                        format!("illegal attribute name '{k}': subrule attributes must be implicit (start with '_')"),
                    ));
                }
                attrs.push(self.attr_from_value(&k, &v, owner).map_err(|e| herr(ctx, e))?);
            }
        }
        let seq = |k: &str| match kw.get(k) {
            None | Some(Value::None) => Vec::new(),
            Some(v) => zel_starlark::eval::builtins::value_to_sequence(v).unwrap_or_default(),
        };
        Ok(crate::aspect::register_subrule(crate::aspect::SubruleDef {
            implementation,
            attrs,
            toolchains: seq("toolchains"),
            fragments: seq("fragments").iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        }))
    }

    /// `aspect(implementation, attr_aspects, attrs, required_providers, ...)`.
    fn define_aspect(
        &self,
        ctx: &mut EvalContext,
        owner: &PackageIdentifier,
        pos: Vec<Value>,
        kw: BTreeMap<String, Value>,
    ) -> HostResult {
        check_kwargs(
            "aspect",
            &kw,
            &[
                "implementation", "attr_aspects", "attrs", "required_providers",
                "required_aspect_providers", "provides", "requires", "propagation_predicate",
                "fragments", "host_fragments", "toolchains", "doc", "apply_to_generating_rules",
                "exec_compatible_with", "exec_groups", "subrules", "toolchains_aspects",
            ],
        )
        .map_err(|e| herr(ctx, e))?;
        let implementation = arg(&pos, &kw, 0, "implementation")
            .cloned()
            .ok_or_else(|| herr(ctx, "aspect() missing 1 required positional argument: implementation"))?;
        let strs = |k: &str| -> Result<Vec<String>, String> {
            match kw.get(k) {
                None | Some(Value::None) => Ok(Vec::new()),
                Some(v) => sl_values(v).map_err(|e| format!("for parameter '{k}': {e}")),
            }
        };
        let attr_aspects = match kw.get("attr_aspects") {
            Some(Value::String(s)) => vec![s.clone()],
            _ => strs("attr_aspects").map_err(|e| herr(ctx, e))?,
        };
        let groups = |k: &str| -> Vec<Vec<String>> {
            let Some(p) = kw.get(k) else { return Vec::new() };
            let items = zel_starlark::eval::builtins::value_to_sequence(p).unwrap_or_default();
            let all_lists = !items.is_empty() && items.iter().all(|i| matches!(i, Value::List(_) | Value::Tuple(_)));
            if all_lists {
                items
                    .iter()
                    .map(|i| {
                        zel_starlark::eval::builtins::value_to_sequence(i)
                            .unwrap_or_default()
                            .iter()
                            .filter_map(crate::provider::provider_ref_of)
                            .collect()
                    })
                    .collect()
            } else if items.is_empty() {
                Vec::new()
            } else {
                // A flat list means "any one of these providers".
                items.iter().filter_map(crate::provider::provider_ref_of).map(|p| vec![p]).collect()
            }
        };
        let mut attrs = Vec::new();
        if let Some(a) = kw.get("attrs") {
            let Value::Dict(d) = a else {
                return Err(herr(ctx, type_mismatch("attrs", a, "dict", "aspect")));
            };
            for (k, v) in d.lock().unwrap().items() {
                let Value::String(k) = k else {
                    return Err(herr(ctx, "attrs keys must be strings"));
                };
                let attr = self.attr_from_value(&k, &v, owner).map_err(|e| herr(ctx, e))?;
                if !k.starts_with('_') {
                    let simple = matches!(attr.ty, AttrType::Bool | AttrType::Int | AttrType::String);
                    if !simple || attr.allowed_values.is_none() {
                        return Err(herr(
                            ctx,
                            format!(
                                "Aspect parameter attribute '{k}' must have type 'bool', 'int' or 'string' and use the 'values' restriction."
                            ),
                        ));
                    }
                }
                attrs.push(attr);
            }
        }
        let seq = |k: &str| match kw.get(k) {
            None | Some(Value::None) => Vec::new(),
            Some(v) => zel_starlark::eval::builtins::value_to_sequence(v).unwrap_or_default(),
        };
        let provides: Vec<String> =
            seq("provides").iter().filter_map(crate::provider::provider_ref_of).collect();
        let def = crate::aspect::AspectDef {
            id: 0,
            implementation,
            attr_aspects,
            attrs,
            required_providers: groups("required_providers"),
            required_aspect_providers: groups("required_aspect_providers"),
            provides,
            requires: seq("requires"),
            toolchains: seq("toolchains"),
            fragments: strs("fragments").map_err(|e| herr(ctx, e))?,
            apply_to_generating_rules: matches!(kw.get("apply_to_generating_rules"), Some(Value::Bool(true))),
            exec_compatible_with: seq("exec_compatible_with")
                .iter()
                .filter_map(|v| match v {
                    Value::Label(l) => Some(l.canonical_form()),
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            defined_in: Mutex::new(String::new()),
            name: Mutex::new(None),
            doc: match kw.get("doc") {
                Some(Value::String(s)) => s.clone(),
                _ => String::new(),
            },
        };
        Ok(crate::aspect::register_aspect(def))
    }

    fn define_rule(
        &self,
        ctx: &mut EvalContext,
        owner: &PackageIdentifier,
        pos: Vec<Value>,
        kw: BTreeMap<String, Value>,
    ) -> HostResult {
        check_kwargs(
            "rule",
            &kw,
            &[
                "implementation", "test", "attrs", "outputs", "executable", "build_setting", "cfg",
                "fragments", "host_fragments", "toolchains", "doc", "provides", "exec_compatible_with",
                "analysis_test", "output_to_genfiles", "exec_groups", "compile_one_filetype",
                "name", "initializer", "parent", "extendable", "subrules", "_skylark_testable",
            ],
        )
        .map_err(|e| herr(ctx, e))?;
        let implementation = arg(&pos, &kw, 0, "implementation")
            .cloned()
            .ok_or_else(|| herr(ctx, "rule() missing 1 required positional argument: implementation"))?;
        let mut rest_kwargs = kw.clone();
        rest_kwargs.remove("implementation");
        let flag = |k: &str| matches!(kw.get(k), Some(Value::Bool(true)));
        let test = flag("test") || flag("analysis_test");
        let executable = flag("executable") || test;
        let mut attrs = Vec::new();
        if let Some(a) = kw.get("attrs") {
            let Value::Dict(d) = a else {
                return Err(herr(ctx, type_mismatch("attrs", a, "dict", "rule")));
            };
            for (k, v) in d.lock().unwrap().items() {
                let Value::String(k) = k else {
                    return Err(herr(ctx, "attrs keys must be strings"));
                };
                attrs.push(self.attr_from_value(&k, &v, owner).map_err(|e| herr(ctx, e))?);
            }
        }
        let mut outputs = Vec::new();
        if let Some(o) = kw.get("outputs") {
            match o {
                Value::None => {}
                Value::Dict(d) => {
                    for (k, v) in d.lock().unwrap().items() {
                        match (k, v) {
                            (Value::String(k), Value::String(v)) => outputs.push((k, v)),
                            _ => return Err(herr(ctx, "outputs must be a dict of strings")),
                        }
                    }
                }
                other => return Err(herr(ctx, type_mismatch("outputs", other, "dict", "rule"))),
            }
        }
        let build_setting = kw.get("build_setting").map(|v| !matches!(v, Value::None)).unwrap_or(false);
        if build_setting {
            let bs_ty = match kw.get("build_setting") {
                Some(Value::Struct(s)) => match s.fields.get("_build_setting") {
                    Some(Value::String(t)) => t.clone(),
                    _ => "string".to_string(),
                },
                _ => "string".to_string(),
            };
            let ty = match bs_ty.as_str() {
                "int" => AttrType::Int,
                "bool" => AttrType::Bool,
                "string_list" => AttrType::StringList,
                "label" => AttrType::Label,
                _ => AttrType::String,
            };
            if !attrs.iter().any(|a: &Attribute| a.name == "build_setting_default") {
                attrs.push(Attribute::new("build_setting_default", ty).mandatory());
            }
            // Build settings implicitly get a `help` string attribute.
            if !attrs.iter().any(|a: &Attribute| a.name == "help") {
                attrs.push(Attribute::new("help", AttrType::String));
            }
        }
        let mut definition_stack = Vec::new();
        if ctx.thread.frames.is_empty() {
            definition_stack.push((fmt_loc(&ctx.last_call_loc), "<toplevel>".to_string()));
        } else {
            definition_stack.push((ctx.thread.frames[0].location.to_string(), "<toplevel>".to_string()));
            for i in 1..ctx.thread.frames.len() {
                definition_stack.push((ctx.thread.frames[i].location.to_string(), ctx.thread.frames[i - 1].function_name.clone()));
            }
            let last_func = ctx.thread.frames.last().unwrap().function_name.clone();
            definition_stack.push((fmt_loc(&ctx.last_call_loc), last_func));
        }
        let mut reg = self.registry.lock().unwrap();
        reg.rules.push(RuleDef {
            name: None,
            attrs,
            executable,
            test,
            outputs,
            build_setting,
            class: None,
            implementation,
            kwargs: rest_kwargs,
            definition_stack,
        });
        Ok(Value::Host("rule".to_string(), (reg.rules.len() - 1).to_string()))
    }

    fn call_starlark_rule(
        &self,
        ctx: &mut EvalContext,
        state: Option<&Arc<Mutex<PackageState>>>,
        mut pos: Vec<Value>,
        kw: BTreeMap<String, Value>,
    ) -> HostResult {
        let Some(Value::Host(_, id)) = pos.first().cloned() else {
            return Err(herr(ctx, "internal error: rule call without receiver"));
        };
        pos.remove(0);
        let idx: usize = id.parse().unwrap_or(usize::MAX);
        let class = {
            let reg = self.registry.lock().unwrap();
            reg.rules.get(idx).and_then(|r| r.class.clone())
        };
        let Some(rc) = class else {
            return Err(herr(
                ctx,
                "Invalid rule class hasn't been exported by a bzl file",
            ));
        };
        let Some(st) = state else {
            return Err(herr(
                ctx,
                format!(
                    "{}() can only be used while evaluating a BUILD file (and macros called from it)",
                    rc.name
                ),
            ));
        };
        if rc.is_test && !rc.name.ends_with("_test") {
            return Err(herr(
                ctx,
                format!(
                    "Invalid rule class name '{}', test rule class names must end with '_test'",
                    rc.name
                ),
            ));
        }
        self.instantiate_rule(ctx, st, &rc, &pos, kw)
    }

    fn define_macro(
        &self,
        ctx: &mut EvalContext,
        owner: &PackageIdentifier,
        pos: Vec<Value>,
        kw: BTreeMap<String, Value>,
    ) -> HostResult {
        if !pos.is_empty() {
            return Err(herr(ctx, "macro() accepts no positional arguments"));
        }
        check_kwargs(
            "macro",
            &kw,
            &["implementation", "attrs", "inherit_attrs", "finalizer", "doc"],
        )
        .map_err(|e| herr(ctx, e))?;
        let imp = kw
            .get("implementation")
            .cloned()
            .ok_or_else(|| herr(ctx, "macro() missing 1 required named argument: implementation"))?;
        if !matches!(imp, Value::UserFunction(_)) {
            return Err(herr(ctx, type_mismatch("implementation", &imp, "function", "macro")));
        }
        let mut attrs = vec![
            Attribute::new("name", AttrType::String).mandatory().nonconfigurable(),
            Attribute::new("visibility", AttrType::LabelList).nonconfigurable(),
        ];
        if let Some(ia) = kw.get("inherit_attrs") {
            let reg = self.registry.lock().unwrap();
            match ia {
                Value::Host(k, id) if k == "rule" => {
                    if let Ok(idx) = id.parse::<usize>() {
                        if let Some(r) = reg.rules.get(idx) {
                            for a in &r.attrs {
                                if a.name != "name" && a.name != "visibility" {
                                    attrs.push(a.clone());
                                }
                            }
                        }
                    }
                }
                Value::Host(k, id) if k == "macro" => {
                    if let Ok(idx) = id.parse::<usize>() {
                        if let Some(m) = reg.macros.get(idx) {
                            for a in &m.attrs {
                                if a.name != "name" && a.name != "visibility" {
                                    attrs.push(a.clone());
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(a) = kw.get("attrs") {
            let Value::Dict(d) = a else {
                return Err(herr(ctx, type_mismatch("attrs", a, "dict", "macro")));
            };
            for (k, v) in d.lock().unwrap().items() {
                let Value::String(k) = k else {
                    return Err(herr(ctx, "attrs keys must be strings"));
                };
                if k == "name" {
                    return Err(herr(ctx, "Cannot declare a macro attribute named 'name'"));
                }
                let attr = self.attr_from_value(&k, &v, owner).map_err(|e| herr(ctx, e))?;
                attrs.retain(|x| x.name != k);
                attrs.push(attr);
            }
        }
        let mut reg = self.registry.lock().unwrap();
        reg.macros.push(MacroDef {
            name: None,
            attrs,
            implementation: imp,
            def_pkg: owner.clone(),
            def_location: {
                // `macro_name = macro(...)`: Bazel reports the start of the defining statement.
                let l = fmt_loc(&ctx.last_call_loc);
                match l.rsplitn(3, ':').collect::<Vec<_>>()[..] {
                    [_, line, file] => format!("{file}:{line}:1"),
                    _ => l,
                }
            },
        });
        Ok(Value::Host("macro".to_string(), (reg.macros.len() - 1).to_string()))
    }

    fn call_macro(
        &self,
        ctx: &mut EvalContext,
        state: Option<&Arc<Mutex<PackageState>>>,
        mut pos: Vec<Value>,
        kw: BTreeMap<String, Value>,
    ) -> HostResult {
        let Some(Value::Host(_, id)) = pos.first().cloned() else {
            return Err(herr(ctx, "internal error: macro call without receiver"));
        };
        pos.remove(0);
        let idx: usize = id.parse().unwrap_or(usize::MAX);
        let (mname, attrs) = {
            let reg = self.registry.lock().unwrap();
            let Some(m) = reg.macros.get(idx) else {
                return Err(herr(ctx, "unknown macro"));
            };
            (m.name.clone(), m.attrs.clone())
        };
        let Some(mname) = mname else {
            return Err(herr(ctx, "Invalid macro hasn't been exported by a bzl file"));
        };
        let Some(st) = state else {
            return Err(herr(
                ctx,
                "Cannot instantiate a macro when loading a .bzl file. Macros may only be instantiated while evaluating a BUILD file or another macro.",
            ));
        };
        if !pos.is_empty() {
            return Err(herr(ctx, format!("{mname}() accepts no positional arguments")));
        }
        let loc = fmt_loc(&ctx.last_call_loc);
        let mut s = st.lock().unwrap();
        let id_pkg = s.id.clone();
        let mapping = s.mapping.clone();
        let cctx = ConvertCtx {
            pkg: &id_pkg,
            mapping: &mapping,
            rule_class: &mname,
        };
        let fix = |m: String| m.replace(&format!("in '{mname}' rule"), &format!("in '{mname}' macro"));
        let by_name: BTreeMap<&str, &Attribute> = attrs.iter().map(|a| (a.name.as_str(), a)).collect();
        let mut values: BTreeMap<String, AttrValue> = BTreeMap::new();
        for (k, v) in &kw {
            let Some(a) = by_name.get(k.as_str()) else {
                return Err(herr(ctx, format!("no such attribute '{k}' in '{mname}' macro")));
            };
            if matches!(v, Value::None) {
                if a.mandatory {
                    return Err(herr(
                        ctx,
                        format!("missing value for mandatory attribute '{k}' in '{mname}' macro"),
                    ));
                }
                continue;
            }
            values.insert(
                k.clone(),
                convert_value(v, a, &cctx).map_err(|e| herr(ctx, fix(e)))?,
            );
        }
        for a in &attrs {
            if a.mandatory && !values.contains_key(&a.name) {
                return Err(herr(
                    ctx,
                    format!("missing value for mandatory attribute '{}' in '{mname}' macro", a.name),
                ));
            }
        }
        let AttrValue::Str(inst_name) = values.get("name").cloned().unwrap() else {
            unreachable!()
        };
        Label::create(id_pkg.clone(), &inst_name).map_err(|e| herr(ctx, e.to_string()))?;
        if s.targets.contains_key(&inst_name) {
            return Err(herr(
                ctx,
                format!("macro '{inst_name}' conflicts with an existing target."),
            ));
        }
        if let Some(prev) = s.macro_names.get(&inst_name) {
            return Err(herr(
                ctx,
                format!(
                    "macro '{inst_name}' conflicts with an existing macro (and was not created by it), defined at {prev}"
                ),
            ));
        }
        for a in &attrs {
            if !values.contains_key(&a.name) {
                let dv = if a.name == "visibility" { s.args.default_visibility.clone() } else { None };
                values.insert(a.name.clone(), dv.map(AttrValue::LabelList).unwrap_or_else(|| a.default_value()));
            }
        }
        // `MacroClass.instantiateAndAddMacro`: the macro's `visibility` is what was passed (else the
        // package default, or private inside another macro) concatenated with the instantiation
        // site's package (`RuleVisibility.concatWithPackage`).
        {
            let public = Label::parse_canonical("//visibility:public").unwrap();
            let private = Label::parse_canonical("//visibility:private").unwrap();
            let parent_macro = s.macro_stack.last().cloned();
            let parsed: Vec<Label> = match kw.get("visibility") {
                Some(v) if !matches!(v, Value::None) => match values.get("visibility") {
                    Some(AttrValue::LabelList(l)) => l.clone(),
                    _ => Vec::new(),
                },
                _ => match (&parent_macro, &s.args.default_visibility) {
                    (None, Some(d)) => d.clone(),
                    _ => vec![private.clone()],
                },
            };
            let loc_pkg = match &parent_macro {
                None => id_pkg.clone(),
                Some((_, def_pkg)) => def_pkg.clone(),
            };
            let site = Label::create(loc_pkg, "__pkg__").unwrap();
            let mut out: Vec<Label> = if parsed.iter().any(|l| *l == public) {
                vec![public.clone()]
            } else {
                let mut o: Vec<Label> = parsed.iter().filter(|l| **l != private).cloned().collect();
                if !o.contains(&site) {
                    o.push(site);
                }
                o
            };
            out.dedup();
            if values.contains_key("visibility") || attrs.iter().any(|a| a.name == "visibility") {
                values.insert("visibility".to_string(), AttrValue::LabelList(out));
            }
        }
        values.remove("name");
        s.macro_names.insert(inst_name.clone(), loc.clone());
        let parent = s.macro_stack.last().map(|(n, _)| n.clone());
        s.macro_queue.push_back(MacroCall {
            macro_id: idx,
            name: inst_name,
            values,
            parent,
            location: loc,
        });
        s.any_target_declared = true;
        Ok(Value::None)
    }

    /// Expands queued symbolic macros (FIFO, including macros declared by macros).
    fn run_macro_queue(
        self: &Arc<Self>,
        ctx: &mut EvalContext,
        st: &Arc<Mutex<PackageState>>,
    ) -> bool {
        loop {
            let call = {
                let mut s = st.lock().unwrap();
                match s.macro_queue.pop_front() {
                    Some(c) => {
                        let def_pkg = self
                            .registry
                            .lock()
                            .unwrap()
                            .macros
                            .get(c.macro_id)
                            .map(|m| m.def_pkg.clone())
                            .unwrap();
                        s.macro_stack.push((c.name.clone(), def_pkg));
                        let (sym, dloc) = {
                            let reg = self.registry.lock().unwrap();
                            let m = &reg.macros[c.macro_id];
                            (m.name.clone().unwrap_or_default(), m.def_location.clone())
                        };
                        s.macro_frames.push((c.location.clone(), sym, dloc));
                        c
                    }
                    None => return true,
                }
            };
            let (imp, def_name) = {
                let reg = self.registry.lock().unwrap();
                let m = &reg.macros[call.macro_id];
                (m.implementation.clone(), m.name.clone().unwrap_or_default())
            };
            let mut kwargs: BTreeMap<String, Value> = BTreeMap::new();
            kwargs.insert("name".into(), Value::String(call.name.clone()));
            for (k, v) in &call.values {
                kwargs.insert(k.clone(), to_macro_value(v));
            }
            let _ = (&call.parent, &call.location, &def_name);
            let res = ctx.call_value(imp, Vec::new(), kwargs);
            let mut s = st.lock().unwrap();
            s.macro_stack.pop();
            s.macro_frames.pop();
            if let Err(e) = res {
                let (loc, msg) = split_location(&dedup_location(&e.0));
                s.error(loc, msg);
                return false;
            }
        }
    }

    /// Final checks after all BUILD/macro code ran (`Package.Builder.beforeBuild`).
    fn finish_package(&self, st: &Arc<Mutex<PackageState>>) {
        let mut s = st.lock().unwrap();
        let id = s.id.clone();
        let default_vis = s.args.default_visibility.clone();
        // `test_suite`s with empty `tests` get `$implicit_tests`: the package's non-manual tests
        // matching the suite's tags (`TestSuiteImplicitTestsAccumulator`), sorted.
        {
            let tests: Vec<(Label, Vec<String>)> = s
                .targets
                .values()
                .filter_map(|t| match t {
                    Target::Rule(r) if r.extra.rule_class.as_ref().map(|rc| rc.is_test && rc.name != "test_suite").unwrap_or(false) => {
                        let mut tags = match r.extra.attr_values.get("tags") {
                            Some(AttrValue::StrList(l)) => l.clone(),
                            _ => Vec::new(),
                        };
                        if tags.iter().any(|t| t == "manual") {
                            return None;
                        }
                        let size = match r.extra.attr_values.get("size") {
                            Some(AttrValue::Str(z)) => z.clone(),
                            _ => r
                                .extra
                                .rule_class
                                .as_ref()
                                .and_then(|rc| rc.attr("size"))
                                .and_then(|a| match a.default_value() {
                                    AttrValue::Str(z) => Some(z),
                                    _ => None,
                                })
                                .unwrap_or_default(),
                        };
                        tags.push(size);
                        Some((r.label.clone(), tags))
                    }
                    _ => None,
                })
                .collect();
            for t in s.targets.values_mut() {
                let Target::Rule(r) = t else { continue };
                let is_native_suite = r.rule_class == "test_suite"
                    && r.extra.rule_class.as_ref().map(|rc| !rc.is_starlark && rc.implicit.contains_key("$implicit_tests")).unwrap_or(false);
                if !is_native_suite {
                    continue;
                }
                let empty = match r.extra.attr_values.get("tests") {
                    None => true,
                    Some(v) => v.is_empty(),
                };
                if !empty {
                    continue;
                }
                let suite_tags = match r.extra.attr_values.get("tags") {
                    Some(AttrValue::StrList(l)) => l.clone(),
                    _ => Vec::new(),
                };
                let mut labels: Vec<Label> =
                    tests.iter().filter(|(_, tt)| test_matches_suite_tags(tt, &suite_tags)).map(|(l, _)| l.clone()).collect();
                labels.sort_by(java_label_cmp);
                r.extra.attr_values.insert("$implicit_tests".into(), AttrValue::LabelList(labels));
            }
        }
        // Collect in-package label references.
        let mut refs: Vec<(Label, String)> = Vec::new();
        for (_, t) in s.targets.iter() {
            if let Target::Rule(r) = t {
                let rloc = r.extra.location.clone().unwrap_or_default();
                for (an, v) in &r.extra.attr_values {
                    let is_output = r
                        .extra
                        .rule_class
                        .as_ref()
                        .and_then(|rc| rc.attr(an))
                        .map(|a| matches!(a.ty, AttrType::Output | AttrType::OutputList))
                        .unwrap_or(false);
                    if is_output {
                        continue;
                    }
                    for l in v.labels() {
                        refs.push((l, rloc.clone()));
                    }
                }
            }
        }
        let mut reported: BTreeSet<String> = BTreeSet::new();
        for (l, rloc) in refs {
            if l.package_identifier() != &id {
                continue;
            }
            if s.targets.contains_key(l.name()) {
                continue;
            }
            if let Some(msg) = self.cfg.lookup.label_crossing_subpackage_error(&l) {
                if reported.insert(msg.clone()) {
                    s.error(Some(rloc), msg);
                }
                continue;
            }
            let abs = s.pkg_dir.get_relative_str(l.name());
            if self.fs().stat(&abs, true).is_ok() {
                s.targets.insert(
                    l.name().to_string(),
                    Target::InputFile { label: l.clone() },
                );
                if let Some(dv) = &default_vis {
                    s.exported_files
                        .entry(l.name().to_string())
                        .or_insert_with(|| dv.clone());
                }
            }
        }
    }
}

/// `Label.compareTo`: repository name, then package path segment-wise, then target name.
pub fn java_label_cmp(a: &Label, b: &Label) -> std::cmp::Ordering {
    let key = |l: &Label| -> (String, Vec<String>, String) {
        (
            l.repository().name().to_string(),
            l.package_name().split('/').filter(|s| !s.is_empty()).map(str::to_string).collect(),
            l.name().to_string(),
        )
    };
    key(a).cmp(&key(b))
}

/// `BuildType.convertFromBuildLangType`: plain list values of `orderIndependent()` attributes
/// (`tags`, `features`, `visibility`, `tests`, ...) are canonicalised to natural order. Selects keep
/// their order.
fn sort_order_independent(v: AttrValue, attr: &Attribute) -> AttrValue {
    if !attr.order_independent {
        return v;
    }
    match v {
        AttrValue::StrList(mut l) => {
            l.sort();
            AttrValue::StrList(l)
        }
        AttrValue::LabelList(mut l) => {
            l.sort_by(java_label_cmp);
            AttrValue::LabelList(l)
        }
        other => other,
    }
}

/// `TestTargetUtils.sortTagsBySense` + `testMatchesFilters`: whether a test's tags (plus its size)
/// satisfy a test_suite's tag filter (`-tag` excludes, any other tag is required).
fn test_matches_suite_tags(test_tags: &[String], suite_tags: &[String]) -> bool {
    suite_tags.iter().all(|t| match t.strip_prefix('-') {
        Some(ex) => !test_tags.iter().any(|x| x == ex),
        None => test_tags.iter().any(|x| x == t),
    })
}

/// Collapses `loc: loc: msg` into `loc: msg` (host calls get the location prefixed twice when the
/// interpreter also prefixes identifier calls).
#[allow(non_upper_case_globals)]
const RuleClass_APPLICABLE_METADATA_ATTR_ALT: &str = "applicable_licenses";

fn dedup_location(msg: &str) -> String {
    let (l1, rest) = split_location(msg);
    if let Some(l1) = l1 {
        let (l2, rest2) = split_location(&rest);
        if l2.as_deref() == Some(l1.as_str()) {
            return format!("{l1}: {rest2}");
        }
    }
    msg.to_string()
}

/// `AttributeProvider`: `<label>: no such attribute '<k>' in '<class>' rule` plus
/// `SpellChecker.didYouMean` over the documented attributes (not `$`/`:` private, not the
/// `undocumented("internal")` `generator_*` ones).
fn no_such_attribute(label: &Label, k: &str, rc: &RuleClass) -> String {
    let mut best: Option<&String> = None;
    let mut best_d = std::cmp::min(5, (k.chars().count() + 1) / 2);
    let input = k.to_lowercase();
    for cand in &rc.attr_order {
        if cand.starts_with('$') || cand.starts_with(':') || cand.starts_with("generator_") {
            continue;
        }
        if let Some(d) = crate::resolve::edit_distance(&input, &cand.to_lowercase(), best_d) {
            if d < best_d {
                best_d = d;
                best = Some(cand);
            }
        }
    }
    let dym = best.map(|s| format!(" (did you mean '{s}'?)")).unwrap_or_default();
    format!("{}: no such attribute '{k}' in '{}' rule{dym}", label.canonical_form(), rc.name)
}
