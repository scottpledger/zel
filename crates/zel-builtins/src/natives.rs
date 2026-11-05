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
        "java_common_internal_do_not_use._incompatible_java_info_merge_runtime_module_flags" => Value::Bool(false),
        "java_common_internal_do_not_use.incompatible_disable_non_executable_java_binary" => Value::Bool(false),
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
fn dotted_version(pos: &[Value]) -> Result<Value, StarlarkError> {
    match pos.first() {
        Some(Value::String(s)) => {
            if s.is_empty() || !s.chars().next().unwrap().is_ascii_digit() {
                return err(format!("Dotted version components must start with a number: {s}"));
            }
            Ok(Value::Host("dotted_version".into(), s.clone()))
        }
        Some(Value::None) | None => Ok(Value::None),
        Some(v @ Value::Host(k, _)) if k == "dotted_version" => Ok(v.clone()),
        Some(other) => err(format!("dotted_version: got value of type '{}', want 'string'", other.type_name())),
    }
}

/// Numeric components of a dotted version; each component's leading digits, then its alpha tail.
fn components(v: &str) -> Vec<(u64, String)> {
    v.split('.')
        .map(|c| {
            let digits: String = c.chars().take_while(|ch| ch.is_ascii_digit()).collect();
            let rest: String = c.chars().skip(digits.len()).collect();
            (digits.parse().unwrap_or(0), rest)
        })
        .collect()
}

fn compare(a: &str, b: &str) -> i64 {
    let (x, y) = (components(a), components(b));
    for i in 0..x.len().max(y.len()) {
        let ca = x.get(i).cloned().unwrap_or((0, String::new()));
        let cb = y.get(i).cloned().unwrap_or((0, String::new()));
        match ca.0.cmp(&cb.0) {
            std::cmp::Ordering::Equal => {}
            o => return o as i64,
        }
        // An alpha tail (`beta`) sorts before the bare number.
        match (ca.1.is_empty(), cb.1.is_empty()) {
            (true, false) => return 1,
            (false, true) => return -1,
            _ => match ca.1.cmp(&cb.1) {
                std::cmp::Ordering::Equal => {}
                o => return o as i64,
            },
        }
    }
    0
}

/// `host:builtins.<ns>.<fn>`.
pub fn call(rest: &str, pos: Vec<Value>, _kw: Kwargs) -> Result<Value, StarlarkError> {
    if let Some(v) = flag_native(rest) {
        return Ok(v);
    }
    use crate::cc_features::{self, FeatureConfiguration, Variables};
    let kwv = |k: &str, i: usize| _kw.get(k).cloned().or_else(|| pos.get(i).cloned());
    let fc_of = |v: Option<Value>| -> Result<Arc<FeatureConfiguration>, StarlarkError> {
        v.and_then(|v| cc_features::lookup::<FeatureConfiguration>(&v, cc_features::KIND_FEATURE_CONFIGURATION))
            .ok_or_else(|| StarlarkError("feature_configuration must be a FeatureConfiguration".into()))
    };
    let action_of = |v: Option<Value>| match v {
        Some(Value::String(s)) => Ok(s),
        _ => Err(StarlarkError("action_name must be a string".into())),
    };
    let vars_of = |v: Option<Value>| -> Result<Arc<Variables>, StarlarkError> {
        match v {
            Some(v) => cc_features::variables_of(&v).ok_or_else(|| StarlarkError("variables must be CcToolchainVariables".into())),
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
        "apple_common.dotted_version" => dotted_version(&pos),
        n => err(format!(
            "{n} is not implemented yet in zel (native part of Bazel's builtins; see crates/zel-builtins)"
        )),
    }
}

/// Methods/attributes of `DottedVersion` objects (`method:dotted_version.<m>`, `attr:dotted_version.<a>`).
pub fn dotted_version_member(is_method: bool, member: &str, pos: &[Value]) -> Result<Value, StarlarkError> {
    let Some(Value::Host(_, v)) = pos.first() else { return err("invalid DottedVersion") };
    match (is_method, member) {
        (true, "compare_to") => match pos.get(1) {
            Some(Value::Host(k, o)) if k == "dotted_version" => Ok(Value::Int(compare(v, o).into())),
            _ => err("DottedVersion.compare_to: argument must be a DottedVersion"),
        },
        (false, "compare_to") => Ok(Value::BuiltinFunction("host:method:dotted_version.compare_to".into())),
        (_, "__str__") | (false, "version") => Ok(Value::String(v.clone())),
        _ => err(format!("'DottedVersion' value has no field or method '{member}'")),
    }
}

#[allow(dead_code)]
fn _unused(_: Arc<()>) {}
