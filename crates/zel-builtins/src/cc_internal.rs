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
            let mut f: BTreeMap<String, Value> = BTreeMap::new();
            for k in [
                "header_module",
                "pic_header_module",
                "separate_module",
                "separate_pic_module",
            ] {
                f.insert(k.into(), kw.get(k).cloned().unwrap_or(Value::None));
            }
            for k in [
                "modular_public_headers",
                "modular_private_headers",
                "textual_headers",
                "separate_module_headers",
            ] {
                let mut nl = StarlarkList::new(seq(kw.get(k).unwrap_or(&Value::None))?);
                nl.freeze();
                f.insert(k.into(), Value::List(Arc::new(Mutex::new(nl))));
            }
            let mut empty_deps = StarlarkList::new(vec![]);
            empty_deps.freeze();
            let mut empty_merged = StarlarkList::new(vec![]);
            empty_merged.freeze();
            f.insert("deps".into(), Value::List(Arc::new(Mutex::new(empty_deps))));
            f.insert(
                "merged_deps".into(),
                Value::List(Arc::new(Mutex::new(empty_merged))),
            );
            f.insert("_zel_native".into(), Value::String("HeaderInfo".into()));
            Ok(Value::Struct(Arc::new(StructValue::new(f))))
        }
        "create_header_info_with_deps" => {
            let hi = kwarg(&pos, &kw, 0, "header_info").unwrap_or(Value::None);
            let Value::Struct(s) = hi else {
                return err("create_header_info_with_deps: header_info must be a HeaderInfo");
            };
            let mut f = s.fields.clone();
            let mut deps = StarlarkList::new(seq(kw.get("deps").unwrap_or(&Value::None))?);
            deps.freeze();
            let mut merged_deps =
                StarlarkList::new(seq(kw.get("merged_deps").unwrap_or(&Value::None))?);
            merged_deps.freeze();
            f.insert("deps".into(), Value::List(Arc::new(Mutex::new(deps))));
            f.insert(
                "merged_deps".into(),
                Value::List(Arc::new(Mutex::new(merged_deps))),
            );
            Ok(Value::Struct(Arc::new(StructValue::new(f))))
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
            let parent = kwarg(&pos, &kw, 0, "parent").unwrap_or(Value::None);
            let parent = match &parent {
                Value::None => None,
                p => Some(cc_features::variables_of(p).ok_or_else(|| {
                    StarlarkError(
                        "combine_cc_toolchain_variables: parent must be CcToolchainVariables"
                            .into(),
                    )
                })?),
            };
            // Every further positional argument is a CcToolchainVariables (or a list of them).
            let mut to_merge: Vec<Value> = Vec::new();
            let extra: Vec<Value> = if let Some(v) = kw.get("variables") {
                vec![v.clone()]
            } else {
                pos.iter().skip(1).cloned().collect()
            };
            for e in extra {
                match cc_features::variables_of(&e) {
                    Some(_) => to_merge.push(e),
                    None => to_merge.extend(cc_features::items(&e)),
                }
            }
            let mut map = BTreeMap::new();
            for v in to_merge {
                let vars = cc_features::variables_of(&v).ok_or_else(|| {
                    StarlarkError(
                        "combine_cc_toolchain_variables: expected CcToolchainVariables".into(),
                    )
                })?;
                for (k, val) in vars.flattened() {
                    map.insert(k, val);
                }
            }
            Ok(cc_features::make_variables(parent, map))
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
            let get =
                |n: &str| cc_features::items(&cc_features::field(&info, n).unwrap_or(Value::None));
            let tf = cc_features::ToolchainFeatures::new(
                &get("_features_DO_NOT_USE"),
                &get("_action_configs_DO_NOT_USE"),
                &get("_artifact_name_patterns_DO_NOT_USE"),
                &dir,
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
        "exec_os" => {
            // `getOsFromConstraintsOrHost(execution platform)`: the host OS name.
            Ok(Value::String(
                if cfg!(target_os = "macos") {
                    "osx"
                } else if cfg!(windows) {
                    "windows"
                } else {
                    "linux"
                }
                .to_string(),
            ))
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
        let mut kw = Kwargs::new();
        kw.insert("header_info".into(), hi);
        kw.insert("deps".into(), list(vec![]));
        let hi2 = call("create_header_info_with_deps", vec![], kw).unwrap();
        assert!(matches!(hi2, Value::Struct(_)));
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
        let Value::Struct(s) = hi else { panic!() };
        for k in [
            "modular_public_headers",
            "modular_private_headers",
            "textual_headers",
            "separate_module_headers",
            "deps",
            "merged_deps",
        ] {
            let Value::List(l) = s.fields.get(k).unwrap() else {
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
}
