//! Port of `CcStarlarkInternal.java` (the object returned by `cc_common.internal_DO_NOT_USE()`).

use crate::Kwargs;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use zel_starlark::{StarlarkDict, StarlarkError, StarlarkList, StructValue, Value};

/// Every method of `CcStarlarkInternal` that rules_cc's Starlark may call (the
/// `@StarlarkMethod` names, in source order).
pub const FUNCTIONS: &[&str] = &[
    "check_private_api",
    "cc_toolchain_variables",
    "combine_cc_toolchain_variables",
    "intern_string_sequence_variable_value",
    "solib_symlink_action",
    "dynamic_library_symlink",
    "dynamic_library_symlink2",
    "dynamic_library_soname",
    "aspect_class",
    "cc_toolchain_features",
    "is_package_headers_checking_mode_set",
    "package_headers_checking_mode",
    "is_package_headers_checking_mode_set_for_aspect",
    "package_headers_checking_mode_for_aspect",
    "stl_computed_default",
    "get_artifact_name_for_category",
    "get_artifact_name_extension_for_category",
    "absolute_symlink",
    "maybe_hash_preserve_extension",
    "intern_seq",
    "get_link_args",
    "wrap_link_actions",
    "actions2ctx_cheat",
    "exec_os",
    "rule_class",
    "collect_per_file_lto_backend_opts",
    "is_tree_artifact",
    "compute_output_name_prefix_dir",
    "create_cc_compile_action",
    "create_cc_compile_action_template",
    "freeze",
    "check_toplevel",
    "per_file_copts",
    "declare_compile_output_file",
    "declare_other_output_file",
    "create_lto_backend_action",
    "create_lto_backend_action_template",
    "create_header_info",
    "create_header_info_with_deps",
    "expand_and_tokenize",
];

/// The struct returned by `cc_common.internal_DO_NOT_USE()`: each method is a host builtin
/// `host:cc_internal.<name>`.
pub fn internal_struct() -> Value {
    let mut f = BTreeMap::new();
    for n in FUNCTIONS {
        f.insert(
            (*n).to_string(),
            Value::BuiltinFunction(format!("host:cc_internal.{n}")),
        );
    }
    Value::Struct(Arc::new(StructValue::new(f)))
}

fn err<T>(m: impl Into<String>) -> Result<T, StarlarkError> {
    Err(StarlarkError(m.into()))
}

fn kwarg(pos: &[Value], kw: &Kwargs, i: usize, name: &str) -> Option<Value> {
    kw.get(name).cloned().or_else(|| pos.get(i).cloned())
}

fn list(v: Vec<Value>) -> Value {
    Value::List(Arc::new(Mutex::new(StarlarkList::new(v))))
}

fn seq(v: &Value) -> Result<Vec<Value>, StarlarkError> {
    match v {
        Value::List(l) => Ok(l.lock().unwrap().elements.clone()),
        Value::Tuple(t) => Ok(t.clone()),
        Value::None => Ok(vec![]),
        other => err(format!(
            "got value of type '{}', want 'sequence'",
            other.type_name()
        )),
    }
}

use crate::cc_features;

fn variables_from_dict(
    parent: Option<std::sync::Arc<cc_features::Variables>>,
    v: &Value,
) -> Result<Value, StarlarkError> {
    let mut map = BTreeMap::new();
    if let Value::Dict(d) = v {
        for (k, val) in d.lock().unwrap().entries.iter() {
            let Value::String(k) = k else {
                return err("cc_toolchain_variables: keys must be strings");
            };
            map.insert(k.clone(), val.clone());
        }
    }
    Ok(cc_features::make_variables(parent, map))
}

/// Methods of the native `CcToolchainFeatures` object.
pub fn features_method(m: &str, pos: Vec<Value>, kw: Kwargs) -> Result<Value, StarlarkError> {
    let recv = pos.first().cloned().unwrap_or(Value::None);
    let tf = cc_features::lookup::<std::sync::Arc<cc_features::ToolchainFeatures>>(
        &recv,
        cc_features::KIND_FEATURES,
    )
    .ok_or_else(|| StarlarkError("invalid CcToolchainFeatures".into()))?;
    let tf: std::sync::Arc<cc_features::ToolchainFeatures> = (*tf).clone();
    match m {
        "configure_features" => {
            let req = kw
                .get("requested_features")
                .cloned()
                .or_else(|| pos.get(1).cloned())
                .unwrap_or(Value::None);
            let names: Vec<String> = cc_features::items(&req)
                .iter()
                .filter_map(|v| {
                    if let Value::String(s) = v {
                        Some(s.clone())
                    } else {
                        None
                    }
                })
                .collect();
            let fc = tf.configure(&names)?;
            Ok(cc_features::register(
                cc_features::KIND_FEATURE_CONFIGURATION,
                fc,
            ))
        }
        "default_features_and_action_configs" => Ok(list(
            tf.default_features_and_action_configs()
                .into_iter()
                .map(Value::String)
                .collect(),
        )),
        other => err(format!(
            "'CcToolchainFeatures' value has no method '{other}'"
        )),
    }
}

/// Methods of the native `FeatureConfiguration` object.
pub fn feature_configuration_method(
    m: &str,
    pos: Vec<Value>,
    kw: Kwargs,
) -> Result<Value, StarlarkError> {
    let recv = pos.first().cloned().unwrap_or(Value::None);
    let fc = cc_features::lookup::<cc_features::FeatureConfiguration>(
        &recv,
        cc_features::KIND_FEATURE_CONFIGURATION,
    )
    .ok_or_else(|| StarlarkError("invalid FeatureConfiguration".into()))?;
    let feature = match kw.get("feature").or_else(|| pos.get(1)) {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    match m {
        "is_enabled" => Ok(Value::Bool(fc.is_enabled(&feature))),
        "is_requested" => Ok(Value::Bool(fc.requested.contains(&feature))),
        other => err(format!(
            "'FeatureConfiguration' value has no method '{other}'"
        )),
    }
}

/// Native backing representation of Bazel's `HeaderInfo` (`CcCompilationContext.HeaderInfo`).
#[derive(Debug, Clone)]
pub struct HeaderInfoData {
    pub id: u64,
    pub header_module: Value,
    pub pic_header_module: Value,
    pub separate_module: Value,
    pub separate_pic_module: Value,
    pub modular_public_headers: Vec<Value>,
    pub modular_private_headers: Vec<Value>,
    pub textual_headers: Vec<Value>,
    pub separate_module_headers: Vec<Value>,
    pub deps: Vec<Arc<HeaderInfoData>>,
    pub transitive_headers: Vec<Value>,
}

impl HeaderInfoData {
    fn direct_modular_headers(&self) -> Vec<Value> {
        let mut out = Vec::new();
        for v in self
            .modular_public_headers
            .iter()
            .chain(&self.modular_private_headers)
            .chain(&self.separate_module_headers)
        {
            if !out.contains(v) {
                out.push(v.clone());
            }
        }
        out
    }
}

static NEXT_HEADER_INFO_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn frozen_list_value(elems: Vec<Value>) -> Value {
    let mut nl = StarlarkList::new(elems);
    nl.freeze();
    Value::List(Arc::new(Mutex::new(nl)))
}

fn is_inc_header(v: &Value) -> bool {
    if let Some(p) = crate::artifact_path(v) {
        return p.ends_with(".inc");
    }
    match v {
        Value::String(s) => s.ends_with(".inc"),
        Value::Struct(s) => s
            .fields
            .get("short_path")
            .or_else(|| s.fields.get("path"))
            .or_else(|| s.fields.get("basename"))
            .and_then(|pv| match pv {
                Value::String(st) => Some(st.ends_with(".inc")),
                _ => None,
            })
            .unwrap_or(false),
        _ => false,
    }
}

fn dedupe_preserving_order(items: impl IntoIterator<Item = Value>) -> Vec<Value> {
    let mut out = Vec::new();
    for v in items {
        if !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

fn compute_transitive_headers(
    direct_modular: &[Value],
    deps: &[Arc<HeaderInfoData>],
) -> Vec<Value> {
    if deps.is_empty() {
        return direct_modular.to_vec();
    }
    let largest_dep = deps
        .iter()
        .max_by_key(|d| d.transitive_headers.len())
        .expect("non-empty deps");
    let mut out: Vec<Value> = Vec::new();
    let in_largest = |v: &Value| largest_dep.transitive_headers.contains(v);
    for h in direct_modular {
        if !in_largest(h) && !out.contains(h) {
            out.push(h.clone());
        }
    }
    let mut visited_ids = std::collections::HashSet::new();
    visited_ids.insert(largest_dep.id);
    fn dfs(
        node: &HeaderInfoData,
        visited: &mut std::collections::HashSet<u64>,
        largest_headers: &[Value],
        out: &mut Vec<Value>,
    ) {
        if !visited.insert(node.id) {
            return;
        }
        for h in node.direct_modular_headers() {
            if !largest_headers.contains(&h) && !out.contains(&h) {
                out.push(h);
            }
        }
        for child in &node.deps {
            dfs(child, visited, largest_headers, out);
        }
    }
    for dep in deps {
        dfs(
            dep,
            &mut visited_ids,
            &largest_dep.transitive_headers,
            &mut out,
        );
    }
    out.extend(largest_dep.transitive_headers.iter().cloned());
    out
}

/// Looks up the native `HeaderInfoData` backing a `HeaderInfo` Starlark value.
pub fn get_header_info(v: &Value) -> Option<Arc<HeaderInfoData>> {
    cc_features::lookup::<HeaderInfoData>(v, "HeaderInfo")
}

/// Resolves Starlark field access (`header_info.<field>`) on `HeaderInfo`.
pub fn header_info_attr(attr: &str, pos: &[Value]) -> Result<Value, StarlarkError> {
    let recv = pos.first().cloned().unwrap_or(Value::None);
    let hi = get_header_info(&recv)
        .ok_or_else(|| StarlarkError("invalid HeaderInfo receiver".into()))?;
    match attr {
        "header_module" => Ok(hi.header_module.clone()),
        "pic_header_module" => Ok(hi.pic_header_module.clone()),
        "separate_module" => Ok(hi.separate_module.clone()),
        "separate_pic_module" => Ok(hi.separate_pic_module.clone()),
        "modular_public_headers" => Ok(frozen_list_value(hi.modular_public_headers.clone())),
        "modular_private_headers" => Ok(frozen_list_value(hi.modular_private_headers.clone())),
        "textual_headers" => Ok(frozen_list_value(hi.textual_headers.clone())),
        "separate_module_headers" => Ok(frozen_list_value(hi.separate_module_headers.clone())),
        other => err(format!(
            "'HeaderInfo' value has no field or method '{other}'"
        )),
    }
}

pub fn call(name: &str, pos: Vec<Value>, kw: Kwargs) -> Result<Value, StarlarkError> {
    match name {
        // `Dict.immutableCopyOf` / `StarlarkList.immutableCopyOf`.
        "freeze" => {
            let v = kwarg(&pos, &kw, 0, "value")
                .ok_or_else(|| StarlarkError("freeze() missing argument: value".into()))?;
            Ok(match &v {
                Value::Dict(d) => {
                    let mut nd = StarlarkDict::new();
                    for (k, val) in d.lock().unwrap().entries.iter() {
                        nd.insert(k.clone(), val.clone())?;
                    }
                    nd.freeze();
                    Value::Dict(Arc::new(Mutex::new(nd)))
                }
                Value::List(l) => {
                    let mut nl = StarlarkList::new(l.lock().unwrap().elements.clone());
                    nl.freeze();
                    Value::List(Arc::new(Mutex::new(nl)))
                }
                Value::Tuple(t) => {
                    let mut nl = StarlarkList::new(t.clone());
                    nl.freeze();
                    Value::List(Arc::new(Mutex::new(nl)))
                }
                Value::Set(s) => {
                    let mut nl = StarlarkList::new(s.lock().unwrap().elements.clone());
                    nl.freeze();
                    Value::List(Arc::new(Mutex::new(nl)))
                }
                other => other.clone(),
            })
        }
        // The Java implementation verifies the calling package against an allowlist; callers
        // here are always rules_cc/rules_java themselves, so the check always passes.
        "check_private_api" => Ok(Value::None),
        // Interning only affects memory sharing in Bazel.
        "intern_seq" => kwarg(&pos, &kw, 0, "seq")
            .ok_or_else(|| StarlarkError("intern_seq() missing argument: seq".into())),
        "create_header_info" => {
            const ALLOWED: &[&str] = &[
                "header_module",
                "pic_header_module",
                "modular_public_headers",
                "modular_private_headers",
                "textual_headers",
                "separate_module_headers",
                "separate_module",
                "separate_pic_module",
            ];
            if !pos.is_empty() {
                return err("create_header_info() accepts no positional arguments");
            }
            for k in kw.keys() {
                if !ALLOWED.contains(&k.as_str()) {
                    return err(format!(
                        "create_header_info() got unexpected keyword argument '{k}'"
                    ));
                }
            }
            let header_module = kw.get("header_module").cloned().unwrap_or(Value::None);
            let pic_header_module = kw.get("pic_header_module").cloned().unwrap_or(Value::None);
            let separate_module = kw.get("separate_module").cloned().unwrap_or(Value::None);
            let separate_pic_module = kw
                .get("separate_pic_module")
                .cloned()
                .unwrap_or(Value::None);
            if !matches!(separate_module, Value::None) && matches!(header_module, Value::None) {
                return err("separate_module cannot be used without header_module");
            }
            if !matches!(separate_pic_module, Value::None)
                && matches!(pic_header_module, Value::None)
            {
                return err("separate_pic_module cannot be used without pic_header_module");
            }
            let raw_pub = seq(kw.get("modular_public_headers").unwrap_or(&Value::None))?;
            let raw_priv = seq(kw.get("modular_private_headers").unwrap_or(&Value::None))?;
            let raw_tex = seq(kw.get("textual_headers").unwrap_or(&Value::None))?;
            let raw_sep = seq(kw.get("separate_module_headers").unwrap_or(&Value::None))?;
            let mut pub_hdrs = Vec::new();
            let mut priv_hdrs = Vec::new();
            let mut tex_hdrs = raw_tex;
            for h in raw_pub {
                if is_inc_header(&h) {
                    tex_hdrs.push(h);
                } else {
                    pub_hdrs.push(h);
                }
            }
            for h in raw_priv {
                if is_inc_header(&h) {
                    tex_hdrs.push(h);
                } else {
                    priv_hdrs.push(h);
                }
            }
            let modular_public_headers = dedupe_preserving_order(pub_hdrs);
            let modular_private_headers = dedupe_preserving_order(priv_hdrs);
            let textual_headers = dedupe_preserving_order(tex_hdrs);
            let separate_module_headers = dedupe_preserving_order(raw_sep);
            let id = NEXT_HEADER_INFO_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut data = HeaderInfoData {
                id,
                header_module,
                pic_header_module,
                separate_module,
                separate_pic_module,
                modular_public_headers,
                modular_private_headers,
                textual_headers,
                separate_module_headers,
                deps: Vec::new(),
                transitive_headers: Vec::new(),
            };
            data.transitive_headers = data.direct_modular_headers();
            Ok(cc_features::register("HeaderInfo", data))
        }
        "create_header_info_with_deps" => {
            for k in kw.keys() {
                if !matches!(k.as_str(), "header_info" | "deps" | "merged_deps") {
                    return err(format!(
                        "create_header_info_with_deps() got unexpected keyword argument '{k}'"
                    ));
                }
            }
            let hi_val = kwarg(&pos, &kw, 0, "header_info").unwrap_or(Value::None);
            let Some(base) = get_header_info(&hi_val) else {
                return err("create_header_info_with_deps: header_info must be a HeaderInfo");
            };
            let raw_deps = seq(&kwarg(&pos, &kw, 1, "deps").unwrap_or(Value::None))?;
            let raw_merged = seq(&kwarg(&pos, &kw, 2, "merged_deps").unwrap_or(Value::None))?;
            let mut deps = Vec::with_capacity(raw_deps.len());
            for d in &raw_deps {
                let Some(hd) = get_header_info(d) else {
                    return err("create_header_info_with_deps: deps must contain HeaderInfo");
                };
                deps.push(hd);
            }
            let mut merged = Vec::with_capacity(raw_merged.len());
            for m in &raw_merged {
                let Some(hm) = get_header_info(m) else {
                    return err(
                        "create_header_info_with_deps: merged_deps must contain HeaderInfo",
                    );
                };
                merged.push(hm);
            }
            let mut pub_hdrs = base.modular_public_headers.clone();
            let mut priv_hdrs = base.modular_private_headers.clone();
            let mut tex_hdrs = base.textual_headers.clone();
            let mut all_deps: Vec<Arc<HeaderInfoData>> = base.deps.clone();
            let mut seen_dep_ids: std::collections::HashSet<u64> =
                all_deps.iter().map(|d| d.id).collect();
            for m in &merged {
                pub_hdrs.extend(m.modular_public_headers.iter().cloned());
                priv_hdrs.extend(m.modular_private_headers.iter().cloned());
                tex_hdrs.extend(m.textual_headers.iter().cloned());
                for md in &m.deps {
                    if seen_dep_ids.insert(md.id) {
                        all_deps.push(Arc::clone(md));
                    }
                }
            }
            for d in deps {
                if seen_dep_ids.insert(d.id) {
                    all_deps.push(d);
                }
            }
            let modular_public_headers = dedupe_preserving_order(pub_hdrs);
            let modular_private_headers = dedupe_preserving_order(priv_hdrs);
            let textual_headers = dedupe_preserving_order(tex_hdrs);
            let id = NEXT_HEADER_INFO_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut data = HeaderInfoData {
                id,
                header_module: base.header_module.clone(),
                pic_header_module: base.pic_header_module.clone(),
                separate_module: base.separate_module.clone(),
                separate_pic_module: base.separate_pic_module.clone(),
                modular_public_headers,
                modular_private_headers,
                textual_headers,
                separate_module_headers: base.separate_module_headers.clone(),
                deps: all_deps,
                transitive_headers: Vec::new(),
            };
            let direct_modular = data.direct_modular_headers();
            data.transitive_headers = compute_transitive_headers(&direct_modular, &data.deps);
            Ok(cc_features::register("HeaderInfo", data))
        }
        "is_tree_artifact" => {
            if let Some(v) = pos.first() {
                if !matches!(v, Value::Host(k, _) if k == "file") {
                    return err(format!(
                        "in call to is_tree_artifact(), parameter 'artifact' got value of type '{}', want 'File'",
                        v.type_name()
                    ));
                }
            }
            if let Some(k) = kw.keys().next() {
                if k == "artifact" {
                    return err(
                        "is_tree_artifact() got named argument for positional-only parameter 'artifact'",
                    );
                }
                return err(format!(
                    "is_tree_artifact() got unexpected keyword argument '{k}'"
                ));
            }
            if pos.len() > 1 {
                return err(format!(
                    "is_tree_artifact() accepts no more than 1 positional argument but got {}",
                    pos.len()
                ));
            }
            if pos.is_empty() {
                return err("is_tree_artifact() missing 1 required positional argument: artifact");
            }
            err("cc_internal.is_tree_artifact requires an analysis-time File")
        }
        "cc_toolchain_variables" => {
            let vars = kwarg(&pos, &kw, 0, "vars").unwrap_or(Value::None);
            Ok(variables_from_dict(None, &vars)?)
        }
        "combine_cc_toolchain_variables" => {
            if kw.keys().any(|k| k != "parent") {
                return err("combine_cc_toolchain_variables: unexpected keyword argument");
            }
            let parent_val = kwarg(&pos, &kw, 0, "parent").ok_or_else(|| {
                StarlarkError("combine_cc_toolchain_variables: missing parent argument".into())
            })?;
            let parent = cc_features::variables_of(&parent_val).ok_or_else(|| {
                StarlarkError(
                    "combine_cc_toolchain_variables: parent must be CcToolchainVariables".into(),
                )
            })?;
            let extra_start = if kw.contains_key("parent") { 0 } else { 1 };
            let mut map = BTreeMap::new();
            for v in pos.iter().skip(extra_start) {
                let vars = cc_features::variables_of(v).ok_or_else(|| {
                    StarlarkError(
                        "combine_cc_toolchain_variables: expected CcToolchainVariables".into(),
                    )
                })?;
                for (k, val) in &vars.map {
                    if map.contains_key(k) {
                        return err("Cannot overwrite existing variables");
                    }
                    map.insert(k.clone(), val.clone());
                }
            }
            Ok(cc_features::make_variables(Some(parent), map))
        }
        "intern_string_sequence_variable_value" => kwarg(&pos, &kw, 0, "string_sequence")
            .ok_or_else(|| StarlarkError("missing argument: string_sequence".into())),
        "cc_toolchain_features" => {
            let info = kwarg(&pos, &kw, 0, "toolchain_config_info").unwrap_or(Value::None);
            let dir = match kwarg(&pos, &kw, 1, "tools_directory") {
                Some(Value::String(s)) => s,
                Some(Value::Label(l)) => l.canonical_form(),
                Some(other) => crate::artifact_path(&other).unwrap_or_default(),
                None => String::new(),
            };
            let exec_os = match cc_features::field(&info, "_exec_os_DO_NOT_USE") {
                Some(Value::String(s)) => Some(s),
                _ => None,
            };
            let get =
                |n: &str| cc_features::items(&cc_features::field(&info, n).unwrap_or(Value::None));
            let tf = cc_features::ToolchainFeatures::new_with_os(
                &get("_features_DO_NOT_USE"),
                &get("_action_configs_DO_NOT_USE"),
                &get("_artifact_name_patterns_DO_NOT_USE"),
                &dir,
                exec_os.as_deref(),
                &|v| crate::artifact_path(v),
            )?;
            Ok(cc_features::register(
                cc_features::KIND_FEATURES,
                std::sync::Arc::new(tf),
            ))
        }
        "get_artifact_name_for_category" | "get_artifact_name_extension_for_category" => {
            let tc = kwarg(&pos, &kw, 0, "cc_toolchain").unwrap_or(Value::None);
            let category = match kwarg(&pos, &kw, 1, "category") {
                Some(Value::String(s)) => s,
                _ => return err(format!("{name}: category must be a string")),
            };
            let feats = cc_features::field(&tc, "_toolchain_features")
                .and_then(|f| {
                    cc_features::lookup::<std::sync::Arc<cc_features::ToolchainFeatures>>(
                        &f,
                        cc_features::KIND_FEATURES,
                    )
                })
                .ok_or_else(|| {
                    StarlarkError(format!("{name}: cc_toolchain has no toolchain features"))
                })?;
            if name == "get_artifact_name_for_category" {
                let out = match kwarg(&pos, &kw, 2, "output_name") {
                    Some(Value::String(s)) => s,
                    _ => {
                        return err("get_artifact_name_for_category: output_name must be a string")
                    }
                };
                Ok(Value::String(
                    feats.artifact_name_for_category(&category, &out)?,
                ))
            } else {
                Ok(Value::String(
                    feats.artifact_name_extension_for_category(&category)?,
                ))
            }
        }
        "maybe_hash_preserve_extension" => {
            let f = match kwarg(&pos, &kw, 0, "filename") {
                Some(Value::String(s)) => s,
                _ => return err("maybe_hash_preserve_extension: filename must be a string"),
            };
            if f.len() <= 255 {
                return Ok(Value::String(f));
            }
            let hashed = zel_util::to_hex(&zel_util::sha256::digest(f.as_bytes()));
            let ext = std::path::Path::new(&f)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            Ok(Value::String(if ext.is_empty() {
                hashed
            } else {
                format!("{hashed}.{ext}")
            }))
        }
        "stl_computed_default" => Ok(Value::BuiltinFunction(
            "cc_internal.stl_computed_default_fn".into(),
        )),
        "exec_os" => Ok(Value::String(
            if cfg!(target_os = "macos") {
                "DARWIN"
            } else if cfg!(windows) {
                "WINDOWS"
            } else if cfg!(target_os = "freebsd") {
                "FREEBSD"
            } else if cfg!(target_os = "openbsd") {
                "OPENBSD"
            } else if cfg!(target_os = "linux") {
                "LINUX"
            } else {
                "UNKNOWN"
            }
            .to_string(),
        )),
        "check_toplevel" => {
            let fn_val = kwarg(&pos, &kw, 0, "fn").unwrap_or(Value::None);
            match &fn_val {
                Value::UserFunction(f) => {
                    let is_top = f.closure.scopes.is_empty()
                        && f.name != "<lambda>"
                        && f.module
                            .as_ref()
                            .is_some_and(|m| m.globals.lock().unwrap().get(&f.name) == Some(&fn_val));
                    if !is_top {
                        return err("Passed function must be top-level functions.");
                    }
                    Ok(Value::None)
                }
                other => err(format!(
                    "in call to check_toplevel(), parameter 'fn' got value of type '{}', want 'function'",
                    other.type_name()
                )),
            }
        }
        n if FUNCTIONS.contains(&n) => err(format!(
            "cc_internal.{n} is not implemented yet in zel (port of CcStarlarkInternal.{n})"
        )),
        n => err(format!("cc_internal has no method '{n}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_info_roundtrip() {
        let hi = call("create_header_info", vec![], Kwargs::new()).unwrap();
        let hi_dup = call("create_header_info", vec![], Kwargs::new()).unwrap();
        assert_ne!(hi, hi_dup, "HeaderInfo instances have unique identity");
        let mut kw = Kwargs::new();
        kw.insert("header_info".into(), hi);
        kw.insert("deps".into(), list(vec![]));
        let hi2 = call("create_header_info_with_deps", vec![], kw).unwrap();
        assert!(get_header_info(&hi2).is_some());
    }

    #[test]
    fn header_info_inc_promotion_dedup_and_merged_deps() {
        let mut kw = Kwargs::new();
        kw.insert(
            "modular_public_headers".into(),
            list(vec![
                Value::String("a.h".into()),
                Value::String("a.h".into()),
                Value::String("inline.inc".into()),
            ]),
        );
        kw.insert(
            "modular_private_headers".into(),
            list(vec![
                Value::String("priv.inc".into()),
                Value::String("p.h".into()),
            ]),
        );
        let base = call("create_header_info", vec![], kw).unwrap();
        let bd = get_header_info(&base).unwrap();
        assert_eq!(bd.modular_public_headers, vec![Value::String("a.h".into())]);
        assert_eq!(
            bd.modular_private_headers,
            vec![Value::String("p.h".into())]
        );
        assert_eq!(
            bd.textual_headers,
            vec![
                Value::String("inline.inc".into()),
                Value::String("priv.inc".into())
            ]
        );

        let mut kw_exp = Kwargs::new();
        kw_exp.insert(
            "modular_public_headers".into(),
            list(vec![Value::String("exported.h".into())]),
        );
        kw_exp.insert(
            "textual_headers".into(),
            list(vec![Value::String("exp_tex.h".into())]),
        );
        let exported = call("create_header_info", vec![], kw_exp).unwrap();

        let mut kw_merge = Kwargs::new();
        kw_merge.insert("header_info".into(), base);
        kw_merge.insert("deps".into(), list(vec![exported.clone()]));
        kw_merge.insert("merged_deps".into(), list(vec![exported]));
        let merged = call("create_header_info_with_deps", vec![], kw_merge).unwrap();
        let md = get_header_info(&merged).unwrap();
        assert_eq!(
            md.modular_public_headers,
            vec![
                Value::String("a.h".into()),
                Value::String("exported.h".into())
            ]
        );
        assert_eq!(
            md.textual_headers,
            vec![
                Value::String("inline.inc".into()),
                Value::String("priv.inc".into()),
                Value::String("exp_tex.h".into()),
            ]
        );
        assert!(header_info_attr("deps", std::slice::from_ref(&merged)).is_err());
        assert!(header_info_attr("merged_deps", std::slice::from_ref(&merged)).is_err());

        let mut bad_mod = Kwargs::new();
        bad_mod.insert("separate_module".into(), Value::String("sep.pcm".into()));
        assert!(call("create_header_info", vec![], bad_mod).is_err());
    }

    #[test]
    fn unported_is_explicit_error() {
        let e = call("get_link_args", vec![], Kwargs::new()).unwrap_err();
        assert!(e.0.contains("not implemented"));
    }

    #[test]
    fn freeze_list() {
        let mut kw = Kwargs::new();
        kw.insert("value".into(), list(vec![Value::String("a".into())]));
        let Value::List(l) = call("freeze", vec![], kw).unwrap() else {
            panic!()
        };
        assert!(l.lock().unwrap().check_mutable().is_err());

        // Tuples freeze into immutable lists (matching CcStarlarkInternal.freeze)
        let mut kw_t = Kwargs::new();
        kw_t.insert(
            "value".into(),
            Value::Tuple(vec![Value::String("b".into())]),
        );
        let Value::List(lt) = call("freeze", vec![], kw_t).unwrap() else {
            panic!()
        };
        assert!(lt.lock().unwrap().is_frozen());
    }

    #[test]
    fn header_info_fields_are_immutable_lists() {
        let hi = call("create_header_info", vec![], Kwargs::new()).unwrap();
        for k in [
            "modular_public_headers",
            "modular_private_headers",
            "textual_headers",
            "separate_module_headers",
        ] {
            let Value::List(l) = header_info_attr(k, std::slice::from_ref(&hi)).unwrap() else {
                panic!("{k} should be a list");
            };
            assert!(l.lock().unwrap().is_frozen());
        }
    }

    #[test]
    fn is_tree_artifact_enforces_positional_only_file_parameter() {
        let mut kw_artifact = Kwargs::new();
        kw_artifact.insert("artifact".into(), Value::None);
        assert_eq!(
            call("is_tree_artifact", vec![], kw_artifact).unwrap_err().0,
            "is_tree_artifact() got named argument for positional-only parameter 'artifact'"
        );

        let mut kw_unknown = Kwargs::new();
        kw_unknown.insert("file".into(), Value::None);
        assert_eq!(
            call("is_tree_artifact", vec![], kw_unknown).unwrap_err().0,
            "is_tree_artifact() got unexpected keyword argument 'file'"
        );

        assert_eq!(
            call("is_tree_artifact", vec![], Kwargs::new())
                .unwrap_err()
                .0,
            "is_tree_artifact() missing 1 required positional argument: artifact"
        );

        assert_eq!(
            call(
                "is_tree_artifact",
                vec![Value::String("x".into())],
                Kwargs::new()
            )
            .unwrap_err()
            .0,
            "in call to is_tree_artifact(), parameter 'artifact' got value of type 'string', want 'File'"
        );
    }

    #[test]
    fn check_toplevel_and_dotted_version_and_cc_flags() {
        let mut ctx = zel_starlark::EvalContext::default();
        ctx.host = Some(Arc::new(|_ctx, name, pos, kw| {
            crate::call(name, pos, kw)
                .unwrap_or_else(|| Err(StarlarkError(format!("unknown host {name}"))))
        }));
        ctx.globals.insert(
            "check_toplevel".into(),
            Value::BuiltinFunction("host:cc_internal.check_toplevel".into()),
        );
        ctx.globals.insert(
            "dotted_version".into(),
            Value::BuiltinFunction("host:builtins.apple_common.dotted_version".into()),
        );
        ctx.eval_file_checked(
            r#"
def top_fn():
    pass

check_toplevel(top_fn)

v1 = dotted_version("1.2")
v2 = dotted_version(version = "1.10")
if not (v1 < v2) or v1.compare_to(v2) >= 0:
    fail("dotted_version comparison failed")
"#,
            None,
        )
        .unwrap();

        let err_lambda = ctx
            .eval_file_checked("check_toplevel(lambda: None)\n", None)
            .unwrap_err()
            .join("\n");
        assert!(
            err_lambda.contains("Passed function must be top-level functions."),
            "{err_lambda}"
        );

        assert_eq!(
            crate::call(
                "builtins.cc_common.check_experimental_cc_shared_library",
                vec![],
                Kwargs::new()
            )
            .unwrap()
            .unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            crate::call(
                "builtins.cc_common.incompatible_disable_objc_library_transition",
                vec![],
                Kwargs::new()
            )
            .unwrap()
            .unwrap(),
            Value::Bool(true)
        );
    }
}
