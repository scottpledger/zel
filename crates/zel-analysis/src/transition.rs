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

//! Starlark configuration transitions (`transition(implementation, inputs, outputs)`) and
//! the native exec transition. Option keys are `//command_line_option:<name>` for native
//! options and canonical labels for Starlark build settings.

use crate::{BuildConfigurationKey, EXPLICIT_IN_OUTPUT_PATH};
use std::collections::BTreeMap;
use zel_starlark::{BigInt, EvalContext, Value};

const CLO: &str = "//command_line_option:";

fn to_value(v: &str, list: bool) -> Value {
    if list {
        Value::List(std::sync::Arc::new(std::sync::Mutex::new(zel_starlark::StarlarkList::new(
            v.split(',').filter(|s| !s.is_empty()).map(|s| Value::String(s.to_string())).collect(),
        ))))
    } else {
        Value::String(v.to_string())
    }
}

/// Reads the current value of setting `key` from `cfg` (`None` for an unset native option).
pub fn setting_value(cfg: &BuildConfigurationKey, key: &str) -> Value {
    if let Some(name) = key.strip_prefix(CLO) {
        return match name {
            "compilation_mode" => Value::String(cfg.compilation_mode.clone()),
            "cpu" => Value::String(cfg.cpu.clone()),
            "platforms" => Value::List(std::sync::Arc::new(std::sync::Mutex::new(zel_starlark::StarlarkList::new(
                cfg.platforms.iter().map(|p| Value::String(p.clone())).collect(),
            )))),
            "define" => Value::List(std::sync::Arc::new(std::sync::Mutex::new(zel_starlark::StarlarkList::new(
                cfg.user_defines.iter().map(|(k, v)| Value::String(format!("{k}={v}"))).collect(),
            )))),
            other => match cfg.options.get(other) {
                Some(v) => to_value(v, matches!(other, "copt" | "cxxopt" | "conlyopt" | "linkopt" | "features" | "extra_toolchains" | "extra_execution_platforms")),
                None => Value::String(String::new()),
            },
        };
    }
    match cfg.flag_values.get(key) {
        Some(v) => Value::String(v.clone()),
        None => Value::None,
    }
}

fn stringify(v: &Value) -> Result<String, String> {
    Ok(match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Label(l) => l.canonical_form(),
        Value::List(_) | Value::Tuple(_) => zel_starlark::eval::builtins::value_to_sequence(v)
            .map_err(|e| e.0)?
            .iter()
            .map(stringify)
            .collect::<Result<Vec<_>, _>>()?
            .join(","),
        other => return Err(format!("cannot use value of type '{}' as a build setting value", other.type_name())),
    })
}

/// Keeps `cfg.env` (the structured environment options) in step with a transition writing one of
/// the environment-related native options.
fn sync_env_config(cfg: &mut BuildConfigurationKey, name: &str, value: &Value) {
    let list = |v: &Value| -> Vec<zel_util::action_env::EnvVar> {
        let items = match v {
            Value::List(_) | Value::Tuple(_) => zel_starlark::eval::builtins::value_to_sequence(v).unwrap_or_default(),
            other => vec![other.clone()],
        };
        items
            .iter()
            .filter_map(|i| stringify(i).ok())
            .filter_map(|s| zel_util::action_env::EnvVar::parse(&s).ok())
            .collect()
    };
    match name {
        "action_env" => cfg.env.action_env = list(value),
        "host_action_env" => cfg.env.host_action_env = list(value),
        "test_env" => cfg.env.test_env = list(value),
        "incompatible_strict_action_env" => {
            if let Value::Bool(b) = value {
                cfg.env.strict_action_env = *b;
            }
        }
        _ => {}
    }
}

/// Native options holding lists (`List.toString` rendering in the `ST-<hash>` input).
fn is_list_option(name: &str) -> bool {
    matches!(
        name,
        "platforms"
            | "define"
            | "copt"
            | "cxxopt"
            | "conlyopt"
            | "linkopt"
            | "ltoopt"
            | "features"
            | "host_features"
            | "host_copt"
            | "host_cxxopt"
            | "host_conlyopt"
            | "host_linkopt"
            | "extra_toolchains"
            | "extra_execution_platforms"
            | "action_env"
            | "host_action_env"
            | "test_env"
            | "test_arg"
            | "javacopt"
    )
}

/// Java `toString` rendering of a build-setting value, as hashed by
/// `OutputPathMnemonicComputer.hashChosenOptions`.
pub fn render_java(v: &Value, native_list: bool) -> Result<String, String> {
    Ok(match v {
        Value::List(_) | Value::Tuple(_) => {
            let items = zel_starlark::eval::builtins::value_to_sequence(v)
                .map_err(|e| e.0)?
                .iter()
                .map(|x| render_java(x, false))
                .collect::<Result<Vec<_>, _>>()?;
            crate::render_list(&items)
        }
        other if native_list => crate::render_list(&[stringify(other)?]),
        other => stringify(other)?,
    })
}

/// Writes `value` for `key` into `cfg`, recording the difference to the baseline configuration
/// in `transition_changes` (`OptionsDiff`; `cpu`/`compilation_mode`/`platform_suffix` are
/// explicit in the directory name and excluded).
pub fn set_setting(cfg: &mut BuildConfigurationKey, key: &str, value: &Value) -> Result<(), String> {
    let name = key.strip_prefix(CLO);
    // A Starlark flag whose input had no explicit value reads as None; passing that through
    // leaves the flag at its default.
    if name.is_none() && matches!(value, Value::None) {
        return Ok(());
    }
    let before = setting_value(cfg, key);
    let native_list = name.map(is_list_option).unwrap_or(false);
    let before_java = match (&before, name) {
        (Value::None, _) => None,
        (Value::String(s), Some(_)) if s.is_empty() && !native_list => None,
        (b, _) => Some(render_java(b, native_list)?),
    };
    let s = stringify(value)?;
    let new_java = render_java(value, native_list)?;
    if let Some(name) = name {
        match name {
            "compilation_mode" => cfg.compilation_mode = s.clone(),
            "cpu" => cfg.cpu = s.clone(),
            "platforms" => cfg.platforms = s.split(',').filter(|x| !x.is_empty()).map(str::to_string).collect(),
            "define" => {
                cfg.user_defines.clear();
                for d in s.split(',').filter(|x| !x.is_empty()) {
                    let (k, v) = d.split_once('=').unwrap_or((d, ""));
                    cfg.user_defines.insert(k.to_string(), v.to_string());
                }
            }
            other => {
                cfg.options.insert(other.to_string(), s.clone());
                sync_env_config(cfg, other, value);
            }
        }
    } else {
        cfg.flag_values.insert(key.to_string(), s.clone());
    }
    if EXPLICIT_IN_OUTPUT_PATH.contains(&key) {
        return Ok(());
    }
    let baseline = cfg.transition_baseline.entry(key.to_string()).or_insert(before_java).clone();
    if baseline.as_deref() == Some(new_java.as_str())
        || (baseline.is_none() && !native_list && matches!(new_java.as_str(), "" | "false") && name.is_some())
    {
        cfg.transition_changes.remove(key);
    } else {
        cfg.transition_changes.insert(key.to_string(), new_java);
    }
    Ok(())
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(x) => zel_starlark::eval::builtins::value_to_sequence(x)
            .unwrap_or_default()
            .iter()
            .filter_map(|e| match e {
                Value::String(s) => Some(s.clone()),
                Value::Label(l) => Some(l.canonical_form()),
                _ => None,
            })
            .collect(),
        None => Vec::new(),
    }
}

/// Normalizes a transition key: `//command_line_option:x` stays, `--x` is rejected by Bazel.
fn norm_key(k: &str) -> String {
    k.to_string()
}

/// Applies the Starlark transition `trans` (the struct produced by `transition(...)`) to `cfg`.
/// `attrs` is the `attr` struct passed to rule-level/attribute transitions. A split transition
/// (implementation returning a dict of dicts) is an error here; see [`apply_transition_split`].
pub fn apply_transition(
    trans: &Value,
    cfg: &BuildConfigurationKey,
    attrs: Option<&Value>,
) -> Result<BuildConfigurationKey, String> {
    let mut v = apply_transition_split(trans, cfg, attrs)?;
    if v.len() == 1 && v[0].0.is_none() {
        Ok(v.remove(0).1)
    } else {
        Err("split transitions are only supported on attributes".to_string())
    }
}

/// Applies a (possibly split) Starlark transition. Non-split transitions return a single entry
/// with key `None`; split transitions (`{"key": {setting: value, ...}, ...}`) return one entry per
/// key in dict order (`ctx.split_attr` is keyed by these).
pub fn apply_transition_split(
    trans: &Value,
    cfg: &BuildConfigurationKey,
    attrs: Option<&Value>,
) -> Result<Vec<(Option<String>, BuildConfigurationKey)>, String> {
    let Value::Struct(s) = trans else {
        return Err(format!("unsupported transition value of type '{}'", trans.type_name()));
    };
    // `analysis_test_transition(settings = {...})`: sets the given build settings.
    if let Some(Value::Dict(d)) = s.fields.get("settings").filter(|_| s.fields.contains_key("_analysis_test_transition")) {
        let mut out = cfg.clone();
        for (k, v) in d.lock().unwrap().items() {
            let Some(k) = k.as_str().map(str::to_string) else {
                return Err("analysis_test_transition settings keys must be strings".to_string());
            };
            set_setting(&mut out, &k, &v)?;
        }
        // Like Bazel's `PATCH_TRANSITION_KEY`: a split with a single anonymous entry, so that
        // `ctx.attr.<name>` is a list (`target_under_test[0]`).
        return Ok(vec![(Some(String::new()), out)]);
    }
    let Some(imp) = s.fields.get("implementation") else {
        // `config.exec()` / `config.target()` / `config.none()` are handled by the caller; a
        // struct without implementation is not a transition.
        return Err("transition has no implementation".to_string());
    };
    let inputs = str_list(s.fields.get("inputs"));
    let outputs = str_list(s.fields.get("outputs"));
    let mut settings: Vec<(Value, Value)> = Vec::new();
    for i in &inputs {
        settings.push((Value::String(norm_key(i)), setting_value(cfg, i)));
    }
    let settings_dict = Value::Dict(std::sync::Arc::new(std::sync::Mutex::new(
        zel_starlark::StarlarkDict::from_entries(settings).map_err(|e| e.0)?,
    )));
    let mut ctx = EvalContext::default();
    let mut args = vec![settings_dict];
    if let Some(a) = attrs {
        args.push(a.clone());
    } else {
        args.push(Value::Struct(std::sync::Arc::new(zel_starlark::StructValue::default())));
    }
    let r = ctx.call_value(imp.clone(), args, BTreeMap::new()).map_err(|e| e.0)?;
    let Value::Dict(d) = r else {
        return Err(format!(
            "transition function returned {}, want dict of option names to values",
            r.type_name()
        ));
    };
    let items = d.lock().unwrap().items();
    let is_split = !items.is_empty() && items.iter().all(|(_, v)| matches!(v, Value::Dict(_)));
    let apply_one = |entries: Vec<(Value, Value)>| -> Result<BuildConfigurationKey, String> {
        let returned: Vec<String> =
            entries.iter().filter_map(|(k, _)| k.as_str().map(str::to_string)).collect();
        // The implementation must return exactly the declared outputs.
        for o in &outputs {
            if !returned.contains(o) {
                return Err(format!(
                    "transition function returned dictionary with missing key '{o}'; declared outputs are {outputs:?}"
                ));
            }
        }
        for r in &returned {
            if !outputs.contains(r) {
                return Err(format!(
                    "transition function returned undeclared output '{r}'; declared outputs are {outputs:?}"
                ));
            }
        }
        let mut out = cfg.clone();
        for (k, v) in entries {
            let k = k.as_str().unwrap().to_string();
            set_setting(&mut out, &k, &v)?;
        }
        Ok(out)
    };
    if is_split {
        let mut res = Vec::new();
        for (k, v) in items {
            let Some(key) = k.as_str().map(str::to_string) else {
                return Err(format!("transition split key must be a string, got {}", k.type_name()));
            };
            let Value::Dict(inner) = v else { unreachable!() };
            let entries = inner.lock().unwrap().items();
            res.push((Some(key), apply_one(entries)?));
        }
        Ok(res)
    } else {
        Ok(vec![(None, apply_one(items)?)])
    }
}

#[allow(dead_code)]
fn _int(i: i64) -> Value {
    Value::Int(BigInt::from_i64(i))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_read_settings() {
        let mut c = BuildConfigurationKey::target("k8", "fastbuild");
        set_setting(&mut c, "//command_line_option:compilation_mode", &Value::String("opt".into())).unwrap();
        assert_eq!(c.compilation_mode, "opt");
        // compilation_mode is explicit in the directory name: no ST-hash.
        assert_eq!(c.mnemonic(), "k8-opt");
        // No change → no ST suffix.
        let mut d = BuildConfigurationKey::target("k8", "fastbuild");
        set_setting(&mut d, "//command_line_option:compilation_mode", &Value::String("fastbuild".into())).unwrap();
        assert_eq!(d.mnemonic(), "k8-fastbuild");
        set_setting(&mut d, "//a:flag", &Value::Bool(true)).unwrap();
        assert_eq!(d.flag_values["//a:flag"], "true");
        assert!(d.mnemonic().contains("-ST-"));
    }

    #[test]
    fn exec_config_has_bazels_real_st_hash() {
        // Bazel 9 (`SkyframeExecutor.setBaselineConfiguration`): the exec baseline is the exec
        // transition on the host platform, so a default exec configuration has no ST suffix
        // (`bazel-out/<cpu>-opt-exec`), while a different exec platform does.
        let host_cpu = crate::build_options::auto_host_cpu();
        let t = BuildConfigurationKey::target(host_cpu, "fastbuild");
        assert_eq!(t.to_exec().mnemonic(), format!("{host_cpu}-opt-exec"));
        assert!(t.to_exec_for("//p:other").mnemonic().contains("-ST-"));
        // Transition back to the baseline value removes the change.
        let mut c = BuildConfigurationKey::target("k8", "fastbuild");
        set_setting(&mut c, "//a:flag", &Value::String("x".into())).unwrap();
        assert!(c.mnemonic().contains("-ST-"));
        let st = c.mnemonic();
        set_setting(&mut c, "//b:flag", &Value::Bool(true)).unwrap();
        assert_ne!(c.mnemonic(), st);
        let mut d = BuildConfigurationKey::target("k8", "fastbuild");
        set_setting(&mut d, "//a:flag", &Value::String("x".into())).unwrap();
        set_setting(&mut d, "//b:flag", &Value::Bool(true)).unwrap();
        assert_eq!(c.mnemonic(), d.mnemonic());
        set_setting(&mut d, "//b:flag", &Value::None).ok();
    }
}
