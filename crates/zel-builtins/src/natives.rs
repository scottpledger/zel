//! `_builtins.internal.*` natives other than `cc_internal`: semantic-flag reads with the Bazel 9
//! defaults, Apple `DottedVersion`, and explicit "not implemented" errors for the rest.

use crate::Kwargs;
use std::sync::Arc;
use zel_starlark::{StarlarkError, Value};

fn err<T>(m: impl Into<String>) -> Result<T, StarlarkError> {
    Err(StarlarkError(m.into()))
}

/// Semantic-flag natives (`BuildLanguageOptions`, defaults of Bazel 9).
fn flag_native(name: &str) -> Option<Value> {
    Some(match name {
        "java_common_internal_do_not_use._google_legacy_api_enabled" => Value::Bool(false),
        "java_common_internal_do_not_use._incompatible_java_info_merge_runtime_module_flags" => {
            Value::Bool(false)
        }
        "java_common_internal_do_not_use.incompatible_disable_non_executable_java_binary" => {
            Value::Bool(false)
        }
        "cc_common.check_experimental_cc_shared_library" => Value::Bool(true),
        "cc_common.incompatible_disable_objc_library_transition" => Value::Bool(false),
        "cc_common.add_go_exec_groups_to_binary_rules" => Value::Bool(false),
        "cc_common.do_not_use_tools_cpp_compiler_present" => Value::Bool(false),
        "py_builtins.is_bzlmod_enabled" => Value::Bool(true),
        "py_builtins.are_action_listeners_enabled" => Value::Bool(false),
        _ => return None,
    })
}

/// `apple_common.dotted_version(str)`: a `DottedVersion` host object.
fn dotted_version(pos: &[Value], kw: &Kwargs) -> Result<Value, StarlarkError> {
    if pos.len() + kw.len() > 1 {
        return err("dotted_version() accepts no more than 1 positional argument");
    }
    let arg = pos.first().or_else(|| kw.get("version"));
    match arg {
        Some(Value::String(s)) => {
            zel_starlark::eval::builtins::parse_dotted_version(s).map_err(StarlarkError)?;
            Ok(Value::Host("dotted_version".into(), s.clone()))
        }
        Some(other) => err(format!(
            "in call to dotted_version(), parameter 'version' got value of type '{}', want 'string'",
            other.type_name()
        )),
        None => err("dotted_version() missing 1 required positional argument: version"),
    }
}

/// `host:builtins.<ns>.<fn>`.
pub fn call(rest: &str, pos: Vec<Value>, _kw: Kwargs) -> Result<Value, StarlarkError> {
    if let Some(v) = flag_native(rest) {
        return Ok(v);
    }
    use crate::cc_features::{self, FeatureConfiguration, Variables};
    let kwv = |k: &str, i: usize| _kw.get(k).cloned().or_else(|| pos.get(i).cloned());
    let fc_of = |v: Option<Value>| -> Result<Arc<FeatureConfiguration>, StarlarkError> {
        v.and_then(|v| {
            cc_features::lookup::<FeatureConfiguration>(&v, cc_features::KIND_FEATURE_CONFIGURATION)
        })
        .ok_or_else(|| StarlarkError("feature_configuration must be a FeatureConfiguration".into()))
    };
    let action_of = |v: Option<Value>| match v {
        Some(Value::String(s)) => Ok(s),
        _ => Err(StarlarkError("action_name must be a string".into())),
    };
    let vars_of = |v: Option<Value>| -> Result<Arc<Variables>, StarlarkError> {
        match v {
            Some(v) => cc_features::variables_of(&v)
                .ok_or_else(|| StarlarkError("variables must be CcToolchainVariables".into())),
            None => Err(StarlarkError("variables is required".into())),
        }
    };
    match rest {
        "cc_common.get_tool_for_action" => {
            Ok(Value::String(fc_of(kwv("feature_configuration", 0))?.tool_path_for_action(&action_of(kwv("action_name", 1))?)?))
        }
        "cc_common.get_execution_requirements" | "cc_common.get_tool_requirement_for_action" => {
            let r = fc_of(kwv("feature_configuration", 0))?.tool_requirements_for_action(&action_of(kwv("action_name", 1))?)?;
            Ok(Value::Tuple(r.into_iter().map(Value::String).collect()))
        }
        "cc_common.action_is_enabled" => {
            Ok(Value::Bool(fc_of(kwv("feature_configuration", 0))?.action_is_configured(&action_of(kwv("action_name", 1))?)))
        }
        "cc_common.get_memory_inefficient_command_line" => {
            let fc = fc_of(kwv("feature_configuration", 0))?;
            let vars = vars_of(kwv("variables", 2))?;
            let action = action_of(kwv("action_name", 1))?;
            let line = fc
                .command_line(&action, &vars, &|v| crate::artifact_path(v))
                .map_err(|e| StarlarkError(format!("{} (while expanding action '{action}'; variables: {:?})", e.0, vars.flattened().keys().collect::<Vec<_>>())))?;
            Ok(Value::Tuple(line.into_iter().map(Value::String).collect()))
        }
        "cc_common.get_environment_variables" => {
            let fc = fc_of(kwv("feature_configuration", 0))?;
            let vars = vars_of(kwv("variables", 2))?;
            let env = fc.environment(&action_of(kwv("action_name", 1))?, &vars, &|v| crate::artifact_path(v))?;
            let mut d = zel_starlark::StarlarkDict::new();
            for (k, v) in env {
                d.insert(Value::String(k), Value::String(v))?;
            }
            Ok(Value::Dict(Arc::new(std::sync::Mutex::new(d))))
        }
        "cc_common.empty_variables" => Ok(cc_features::make_variables(None, Default::default())),
        "cc_common.legacy_cc_flags_make_variable_do_not_use" => {
            let tc = kwv("cc_toolchain", 0)
                .ok_or_else(|| StarlarkError("cc_toolchain is required".into()))?;
            let val = match cc_features::field(&tc, "_legacy_cc_flags_make_variable") {
                Some(Value::String(s)) => s,
                _ => String::new(),
            };
            Ok(Value::String(val))
        }
        "apple_common.dotted_version" => dotted_version(&pos, &_kw),
        n => err(format!(
            "{n} is not implemented yet in zel (native part of Bazel's builtins; see crates/zel-builtins)"
        )),
    }
}

/// Methods/attributes of `DottedVersion` objects (`method:dotted_version.<m>`, `attr:dotted_version.<a>`).
pub fn dotted_version_member(
    is_method: bool,
    member: &str,
    pos: &[Value],
) -> Result<Value, StarlarkError> {
    let Some(Value::Host(_, v)) = pos.first() else {
        return err("invalid DottedVersion");
    };
    match (is_method, member) {
        (true, "compare_to") => match pos.get(1) {
            Some(Value::Host(k, o)) if k == "dotted_version" => Ok(Value::Int(
                zel_starlark::eval::builtins::dotted_version_compare_to(v, o).into(),
            )),
            _ => err("DottedVersion.compare_to: argument must be a DottedVersion"),
        },
        (false, "compare_to") => Ok(Value::BuiltinFunction(
            "host:method:dotted_version.compare_to".into(),
        )),
        (_, "__str__") | (false, "version") => Ok(Value::String(v.clone())),
        _ => err(format!(
            "'DottedVersion' value has no field or method '{member}'"
        )),
    }
}

#[allow(dead_code)]
fn _unused(_: Arc<()>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;
    use std::collections::BTreeMap;
    use std::hash::{Hash, Hasher};

    fn hash_val(v: &Value) -> u64 {
        let mut h = DefaultHasher::new();
        v.hash(&mut h);
        h.finish()
    }

    fn dv(s: &str) -> Value {
        call(
            "apple_common.dotted_version",
            vec![Value::String(s.to_string())],
            BTreeMap::new(),
        )
        .unwrap()
    }

    #[test]
    fn dotted_version_equality_and_hash_match_compare_to() {
        for (a, b) in [
            ("0", "0.0.0"),
            ("0.1", "0.01"),
            ("0.2", "0.2.0"),
            ("1.2", "1.2.0"),
            ("10", "10.0"),
        ] {
            let va = dv(a);
            let vb = dv(b);
            assert_eq!(
                zel_starlark::eval::builtins::dotted_version_compare_to(a, b),
                0,
                "{a} vs {b}"
            );
            assert_eq!(va, vb, "{a} == {b}");
            assert_eq!(hash_val(&va), hash_val(&vb), "hash({a}) == hash({b})");
            assert_eq!(va.to_string(), a);
            assert_eq!(vb.to_string(), b);
        }
        let v1 = dv("1.0alpha");
        let v2 = dv("1.0");
        assert_ne!(v1, v2);
    }

    #[test]
    fn dotted_version_comparison_equality_and_validation() {
        assert_eq!(dv("1.0"), dv("1"));
        assert_eq!(dv("0.1"), dv("0.01"));
        assert_eq!(dv("1.2x"), dv("1.2x0"));
        assert_eq!(dv("1.2x"), dv("1.2x0.0"));
        assert_eq!(dv("1.2.experimental"), dv("1.2"));
        assert_eq!(dv("10.10.simulator.internal"), dv("10.10"));

        let mut dict = zel_starlark::StarlarkDict::new();
        dict.insert(dv("1.0"), Value::String("ok".into())).unwrap();
        assert_eq!(dict.get(&dv("1")), Some(&Value::String("ok".into())));

        assert!(
            zel_starlark::eval::builtins::compare_values(&dv("1.2beta2"), &dv("1.2beta10"))
                .unwrap()
                .is_lt()
        );
        assert!(
            zel_starlark::eval::builtins::compare_values(&dv("1.0beta2"), &dv("1.0"))
                .unwrap()
                .is_lt()
        );

        for bad in ["", "1.", ".1", "1..2", "2:3", "1.-1", "2147483648", "1beta2147483648"] {
            assert!(
                call(
                    "apple_common.dotted_version",
                    vec![Value::String(bad.to_string())],
                    BTreeMap::new(),
                )
                .is_err(),
                "expected error for '{bad}'"
            );
        }
        assert!(call("apple_common.dotted_version", vec![Value::None], BTreeMap::new()).is_err());
    }
}
