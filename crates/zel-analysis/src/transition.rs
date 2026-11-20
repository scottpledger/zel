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

fn to_list_value(items: Vec<String>) -> Value {
    Value::List(std::sync::Arc::new(std::sync::Mutex::new(
        zel_starlark::StarlarkList::new(items.into_iter().map(Value::String).collect()),
    )))
}

fn env_var_raw(var: &zel_util::action_env::EnvVar) -> String {
    match var {
        zel_util::action_env::EnvVar::Set(n, v) => format!("{n}={v}"),
        zel_util::action_env::EnvVar::Inherit(n) => n.clone(),
        zel_util::action_env::EnvVar::Unset(n) => format!("={n}"),
    }
}

/// Reads the current value of setting `key` from `cfg`.
pub fn setting_value(cfg: &BuildConfigurationKey, key: &str) -> Value {
    if let Some(name) = key.strip_prefix(CLO) {
        return match name {
            "compilation_mode" => Value::String(cfg.compilation_mode.clone()),
            "cpu" => Value::String(cfg.cpu.clone()),
            "host_cpu" => Value::String(
                cfg.options
                    .get("host_cpu")
                    .filter(|s| !s.is_empty())
                    .cloned()
                    .unwrap_or_else(|| crate::build_options::auto_host_cpu().to_string()),
            ),
            "platforms" => Value::List(std::sync::Arc::new(std::sync::Mutex::new(
                zel_starlark::StarlarkList::new(
                    cfg.platforms
                        .iter()
                        .map(|p| Value::String(p.clone()))
                        .collect(),
                ),
            ))),
            "define" => Value::List(std::sync::Arc::new(std::sync::Mutex::new(
                zel_starlark::StarlarkList::new(
                    cfg.user_defines
                        .iter()
                        .map(|(k, v)| Value::String(format!("{k}={v}")))
                        .collect(),
                ),
            ))),
            "action_env" | "host_action_env" | "test_env" if !cfg.has_options_override(name) => {
                let vars = match name {
                    "action_env" => &cfg.env.action_env,
                    "host_action_env" => &cfg.env.host_action_env,
                    _ => &cfg.env.test_env,
                };
                to_list_value(vars.iter().map(env_var_raw).collect())
            }
            other if is_list_option(other) => {
                if crate::build_options::find_option(other)
                    .is_some_and(|(_, opt)| opt.java_type.contains("PerLabelOptions"))
                {
                    if cfg.is_exec {
                        if other == "per_file_copt" {
                            if let Some(raw) = cfg.options.get("per_file_copt") {
                                return to_list_value(crate::build_options::decode_unsplit_list(
                                    raw,
                                ));
                            }
                            if cfg.has_options_override("host_per_file_copt") {
                                let raw = cfg
                                    .options
                                    .get("host_per_file_copt")
                                    .map(String::as_str)
                                    .unwrap_or("");
                                return to_list_value(crate::build_options::decode_unsplit_list(
                                    raw,
                                ));
                            }
                            if let Some(n) = cfg.native.get("host_per_file_copt") {
                                if !n.raw.is_empty() {
                                    return to_list_value(
                                        crate::build_options::decode_unsplit_list(&n.raw),
                                    );
                                }
                            }
                        } else if other == "per_file_ltobackendopt" {
                            if let Some(raw) = cfg.options.get("per_file_ltobackendopt") {
                                return to_list_value(crate::build_options::decode_unsplit_list(
                                    raw,
                                ));
                            }
                        } else if cfg.has_options_override(other) {
                            let raw = cfg.options.get(other).map(String::as_str).unwrap_or("");
                            return to_list_value(crate::build_options::decode_unsplit_list(raw));
                        } else if let Some(n) = cfg.native.get(other) {
                            if !n.raw.is_empty() {
                                return to_list_value(crate::build_options::decode_unsplit_list(
                                    &n.raw,
                                ));
                            }
                        }
                    } else if cfg.has_options_override(other) {
                        let raw = cfg.options.get(other).map(String::as_str).unwrap_or("");
                        return to_list_value(crate::build_options::decode_unsplit_list(raw));
                    } else if let Some(n) = cfg.native.get(other) {
                        if !n.raw.is_empty() {
                            return to_list_value(crate::build_options::decode_unsplit_list(
                                &n.raw,
                            ));
                        }
                    }
                    return match cfg.build_options().get(other) {
                        Some(crate::build_options::ConfigValue::List(l)) => to_list_value(
                            l.iter()
                                .map(|e| crate::starlark_rule::reverse_per_label_for_starlark(e))
                                .collect(),
                        ),
                        _ => to_list_value(Vec::new()),
                    };
                }
                if cfg.has_options_override(other) {
                    let raw = cfg.options.get(other).map(String::as_str).unwrap_or("");
                    let splits_commas =
                        crate::build_options::find_option(other).is_some_and(|(_, opt)| {
                            crate::build_options::splits_commas_in_options_map(other, opt)
                        });
                    let items = if splits_commas {
                        raw.split(',')
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect()
                    } else {
                        crate::build_options::decode_unsplit_list(raw)
                    };
                    to_list_value(items)
                } else {
                    match cfg.build_options().get(other) {
                        Some(crate::build_options::ConfigValue::List(l)) => {
                            to_list_value(l.clone())
                        }
                        _ => to_list_value(Vec::new()),
                    }
                }
            }
            other => match cfg.options.get(other) {
                Some(v) => Value::String(v.clone()),
                None => match cfg.build_options().get(other) {
                    Some(crate::build_options::ConfigValue::Scalar(s)) => Value::String(s.clone()),
                    _ => Value::String(String::new()),
                },
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
        Value::Set(s) => s
            .lock()
            .unwrap()
            .elements
            .iter()
            .map(stringify)
            .collect::<Result<Vec<_>, _>>()?
            .join(","),
        Value::List(_) | Value::Tuple(_) => zel_starlark::eval::builtins::value_to_sequence(v)
            .map_err(|e| e.0)?
            .iter()
            .map(stringify)
            .collect::<Result<Vec<_>, _>>()?
            .join(","),
        other => {
            return Err(format!(
                "cannot use value of type '{}' as a build setting value",
                other.type_name()
            ))
        }
    })
}

/// Keeps `cfg.env` (the structured environment options) in step with a transition writing one of
/// the environment-related native options.
fn sync_env_config(cfg: &mut BuildConfigurationKey, name: &str, value: &Value) {
    let list = |v: &Value| -> Vec<zel_util::action_env::EnvVar> {
        let items = match v {
            Value::List(_) | Value::Tuple(_) => {
                zel_starlark::eval::builtins::value_to_sequence(v).unwrap_or_default()
            }
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
    if let Some((_, opt)) = crate::build_options::find_option(name) {
        return crate::build_options::is_list_option(opt);
    }
    matches!(name, "ltoopt")
}

fn stringify_list_items(v: &Value) -> Result<Vec<String>, String> {
    match v {
        Value::None => Ok(Vec::new()),
        Value::Set(s) => s.lock().unwrap().elements.iter().map(stringify).collect(),
        Value::List(_) | Value::Tuple(_) => zel_starlark::eval::builtins::value_to_sequence(v)
            .map_err(|e| e.0)?
            .iter()
            .map(stringify)
            .collect(),
        other => {
            let item = stringify(other)?;
            Ok(if item.is_empty() {
                Vec::new()
            } else {
                vec![item]
            })
        }
    }
}

/// Java `toString` rendering of a build-setting value, as hashed by
/// `OutputPathMnemonicComputer.hashChosenOptions`.
pub fn render_java(v: &Value, native_list: bool) -> Result<String, String> {
    Ok(match v {
        Value::Set(s) => {
            let items = s
                .lock()
                .unwrap()
                .elements
                .iter()
                .map(|x| render_java(x, false))
                .collect::<Result<Vec<_>, _>>()?;
            crate::render_list(&items)
        }
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
pub fn set_setting(
    cfg: &mut BuildConfigurationKey,
    key: &str,
    value: &Value,
) -> Result<(), String> {
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
            "platforms" => {
                cfg.platforms = s
                    .split(',')
                    .filter(|x| !x.is_empty())
                    .map(str::to_string)
                    .collect()
            }
            "define" => {
                cfg.user_defines.clear();
                for d in s.split(',').filter(|x| !x.is_empty()) {
                    let (k, v) = d.split_once('=').unwrap_or((d, ""));
                    cfg.user_defines.insert(k.to_string(), v.to_string());
                }
            }
            other => {
                let stored = match crate::build_options::find_option(other) {
                    Some((_, opt))
                        if crate::build_options::is_list_option(opt)
                            && !crate::build_options::splits_commas_in_options_map(other, opt) =>
                    {
                        let items = stringify_list_items(value)?;
                        crate::build_options::encode_unsplit_list(&items)
                    }
                    _ => s.clone(),
                };
                cfg.options.insert(other.to_string(), stored);
                sync_env_config(cfg, other, value);
            }
        }
    } else {
        cfg.flag_values.insert(key.to_string(), s.clone());
    }
    if EXPLICIT_IN_OUTPUT_PATH.contains(&key) {
        return Ok(());
    }
    let baseline = cfg
        .transition_baseline
        .entry(key.to_string())
        .or_insert(before_java)
        .clone();
    if baseline.as_deref() == Some(new_java.as_str())
        || (baseline.is_none()
            && !native_list
            && matches!(new_java.as_str(), "" | "false")
            && name.is_some())
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

fn parse_transition_ctx(
    fields: &BTreeMap<String, Value>,
) -> Option<(
    zel_cmdline::PackageIdentifier,
    zel_cmdline::RepositoryMapping,
)> {
    let pkg_str = fields.get("_defining_pkg").and_then(|v| v.as_str())?;
    let pkg = zel_cmdline::Label::parse_canonical(&format!("{pkg_str}:_"))
        .ok()?
        .package_identifier()
        .clone();
    let mut entries = std::collections::HashMap::new();
    if let Some(Value::Dict(d)) = fields.get("_repo_map") {
        for (k, v) in d.lock().unwrap().items() {
            if let (Some(ks), Some(vs)) = (k.as_str(), v.as_str()) {
                entries.insert(
                    ks.to_string(),
                    zel_cmdline::RepositoryName::create_unvalidated(vs),
                );
            }
        }
    }
    let mapping = zel_cmdline::RepositoryMapping::new(entries, pkg.repository().clone());
    Some((pkg, mapping))
}

fn canonicalize_setting(
    raw: &str,
    ctx: Option<&(
        zel_cmdline::PackageIdentifier,
        zel_cmdline::RepositoryMapping,
    )>,
) -> Result<String, String> {
    if raw.starts_with(CLO) {
        return Ok(raw.to_string());
    }
    if let Some((pkg, mapping)) = ctx {
        let lbl = zel_cmdline::Label::parse_with_package_context(raw, pkg, mapping)
            .map_err(|e| format!("Malformed label in transition: '{raw}': {e}"))?;
        return Ok(lbl.canonical_form());
    }
    Ok(zel_cmdline::Label::parse_canonical(raw)
        .map(|l| l.canonical_form())
        .unwrap_or_else(|_| raw.to_string()))
}

#[derive(Clone, Debug)]
struct BuildSettingSpec {
    resolved_key: String,
    ty: String,
    allow_multiple: bool,
    default_attr: Option<zel_packages::attr::AttrValue>,
    setting_ctx: Option<(
        zel_cmdline::PackageIdentifier,
        zel_cmdline::RepositoryMapping,
    )>,
}

fn resolve_build_setting(
    key: &str,
    cfg: &BuildConfigurationKey,
    resolver: Option<&dyn crate::starlark_rule::DepResolver>,
) -> Result<Option<BuildSettingSpec>, String> {
    let effective_key: String = if let Some(alias_name) = key.strip_prefix(CLO) {
        if let Some(target_label) = cfg.flag_alias_target(alias_name) {
            target_label
        } else {
            return Ok(None);
        }
    } else {
        key.to_string()
    };
    let key = effective_key.as_str();
    let Some(r) = resolver else {
        return Ok(None);
    };
    let Some(loader) = r.package_loader() else {
        return Ok(None);
    };
    let mut cur = zel_cmdline::Label::parse_canonical(key).map_err(|e| e.to_string())?;
    let mut seen = Vec::new();
    let mk_setting_ctx = |lbl: &zel_cmdline::Label| {
        let pkg = lbl.package_identifier().clone();
        let entries = r
            .repo_mapping_entries(pkg.repository().name())
            .into_iter()
            .map(|(a, c)| (a, zel_cmdline::RepositoryName::create_unvalidated(c)))
            .collect();
        let mapping = zel_cmdline::RepositoryMapping::new(entries, pkg.repository().clone());
        Some((pkg, mapping))
    };
    for _ in 0..32 {
        if seen.contains(&cur) {
            return Err(format!("cycle in alias chain for build setting '{key}'"));
        }
        seen.push(cur.clone());
        let pkg = loader
            .load_package(cur.package_identifier())
            .map_err(|e| e.message)?;
        let target = pkg.get_target(cur.name(), &*loader.config().lookup.fs)?;
        let zel_packages::Target::Rule(rule) = target else {
            return Err(format!(
                "attempting to transition on '{key}' which is not a build setting"
            ));
        };
        if rule.rule_class == "alias" {
            match rule.attr_or_default("actual") {
                Some(zel_packages::attr::AttrValue::Label(Some(next))) => {
                    cur = next;
                    continue;
                }
                _ => {
                    return Err(format!(
                        "attempting to transition on '{key}' which is not a build setting"
                    ));
                }
            }
        }
        if rule.rule_class == "label_flag" || rule.rule_class == "label_setting" {
            return Ok(Some(BuildSettingSpec {
                resolved_key: cur.canonical_form(),
                ty: "label".to_string(),
                allow_multiple: false,
                default_attr: rule.attr_or_default("build_setting_default"),
                setting_ctx: mk_setting_ctx(&cur),
            }));
        }
        if rule.rule_class == "config_feature_flag" {
            return Ok(Some(BuildSettingSpec {
                resolved_key: cur.canonical_form(),
                ty: "string".to_string(),
                allow_multiple: false,
                default_attr: rule.attr_or_default("default_value"),
                setting_ctx: mk_setting_ctx(&cur),
            }));
        }
        let Some(rc) = rule.extra.rule_class.as_ref() else {
            return Err(format!(
                "attempting to transition on '{key}' which is not a build setting"
            ));
        };
        if !rc.build_setting {
            return Err(format!(
                "attempting to transition on '{key}' which is not a build setting"
            ));
        }
        let (ty, allow_multiple) = match rc
            .starlark
            .as_ref()
            .and_then(|s| s.kwargs.get("build_setting"))
        {
            Some(Value::Struct(s)) => (
                match s.fields.get("_build_setting") {
                    Some(Value::String(t)) => t.clone(),
                    _ => "string".to_string(),
                },
                matches!(s.fields.get("allow_multiple"), Some(Value::Bool(true))),
            ),
            _ => ("string".to_string(), false),
        };
        let default_attr = rule.attr_or_default("build_setting_default");
        return Ok(Some(BuildSettingSpec {
            resolved_key: cur.canonical_form(),
            ty,
            allow_multiple,
            default_attr,
            setting_ctx: mk_setting_ctx(&cur),
        }));
    }
    Err(format!("alias depth exceeded for build setting '{key}'"))
}

fn default_value_for_spec(spec: &BuildSettingSpec) -> Value {
    use zel_packages::attr::AttrValue;
    let scalar = match &spec.default_attr {
        Some(AttrValue::Str(s)) => Value::String(s.clone()),
        Some(AttrValue::Int(i)) => Value::Int(BigInt::from_i64(*i)),
        Some(AttrValue::Bool(b)) => Value::Bool(*b),
        Some(AttrValue::Label(Some(l))) => Value::Label(l.clone()),
        Some(AttrValue::StrList(l)) => {
            if spec.ty == "string_set" {
                let mut set = zel_starlark::StarlarkSet::new();
                for s in l {
                    let _ = set.add(Value::String(s.clone()));
                }
                let val = Value::Set(std::sync::Arc::new(std::sync::Mutex::new(set)));
                val.freeze();
                val
            } else {
                Value::List(std::sync::Arc::new(std::sync::Mutex::new(
                    zel_starlark::StarlarkList::new(
                        l.iter().map(|s| Value::String(s.clone())).collect(),
                    ),
                )))
            }
        }
        _ => match spec.ty.as_str() {
            "int" => Value::Int(BigInt::from_i64(0)),
            "bool" => Value::Bool(false),
            "string_list" => Value::List(std::sync::Arc::new(std::sync::Mutex::new(
                zel_starlark::StarlarkList::new(vec![]),
            ))),
            "string_set" => {
                let val = Value::Set(std::sync::Arc::new(std::sync::Mutex::new(
                    zel_starlark::StarlarkSet::new(),
                )));
                val.freeze();
                val
            }
            "label" => Value::None,
            _ => Value::String(String::new()),
        },
    };
    if spec.allow_multiple && !matches!(scalar, Value::List(_)) {
        Value::List(std::sync::Arc::new(std::sync::Mutex::new(
            zel_starlark::StarlarkList::new(vec![scalar]),
        )))
    } else {
        scalar
    }
}

fn setting_value_with_spec(
    cfg: &BuildConfigurationKey,
    key: &str,
    spec: Option<&BuildSettingSpec>,
) -> Value {
    let Some(spec) = spec else {
        return setting_value(cfg, key);
    };
    if let Some(raw) = cfg.flag_values.get(&spec.resolved_key) {
        if spec.ty == "string_set" {
            let mut set = zel_starlark::StarlarkSet::new();
            for part in raw.split(',').filter(|s| !s.is_empty()) {
                let _ = set.add(Value::String(part.to_string()));
            }
            let val = Value::Set(std::sync::Arc::new(std::sync::Mutex::new(set)));
            val.freeze();
            return val;
        }
        if spec.allow_multiple || spec.ty == "string_list" {
            let mut items = Vec::new();
            for part in raw.split(',').filter(|s| !s.is_empty()) {
                if let Ok(v) = crate::starlark_rule::convert_flag_value(&spec.ty, part) {
                    items.push(v);
                }
            }
            return Value::List(std::sync::Arc::new(std::sync::Mutex::new(
                zel_starlark::StarlarkList::new(items),
            )));
        }
        if let Ok(v) = crate::starlark_rule::convert_flag_value(&spec.ty, raw) {
            return v;
        }
    }
    default_value_for_spec(spec)
}

fn validate_scalar_for_spec(
    key: &str,
    ty: &str,
    v: &Value,
    tctx: Option<&(
        zel_cmdline::PackageIdentifier,
        zel_cmdline::RepositoryMapping,
    )>,
) -> Result<String, String> {
    match ty {
        "string" => match v {
            Value::String(s) => Ok(s.clone()),
            other => Err(format!(
                "expected value of type 'string' for {key}, but got {} ({})",
                other.repr(),
                other.type_name()
            )),
        },
        "int" => match v {
            Value::Int(i) => {
                let n = i
                    .to_i64()
                    .and_then(|v| i32::try_from(v).ok())
                    .ok_or_else(|| {
                        format!(
                            "signed 32-bit int expected for {key}, got out-of-range value {}",
                            i
                        )
                    })?;
                Ok(n.to_string())
            }
            other => Err(format!(
                "expected value of type 'int' for {key}, but got {} ({})",
                other.repr(),
                other.type_name()
            )),
        },
        "bool" => match v {
            Value::Bool(b) => Ok(if *b { "true" } else { "false" }.to_string()),
            Value::Int(i) if i.to_i64() == Some(0) => Ok("false".to_string()),
            Value::Int(i) if i.to_i64() == Some(1) => Ok("true".to_string()),
            other => Err(format!(
                "expected value of type 'bool' for {key}, but got {} ({})",
                other.repr(),
                other.type_name()
            )),
        },
        "label" => match v {
            Value::Label(l) => Ok(l.canonical_form()),
            Value::String(s) => canonicalize_setting(s, tctx),
            other => Err(format!(
                "expected value of type 'Label' for {key}, but got {} ({})",
                other.repr(),
                other.type_name()
            )),
        },
        _ => stringify(v),
    }
}

fn set_setting_with_spec(
    cfg: &mut BuildConfigurationKey,
    key: &str,
    value: &Value,
    spec: Option<&BuildSettingSpec>,
    tctx: Option<&(
        zel_cmdline::PackageIdentifier,
        zel_cmdline::RepositoryMapping,
    )>,
) -> Result<(), String> {
    let Some(spec) = spec else {
        return set_setting(cfg, key, value);
    };
    if matches!(value, Value::None) {
        return Ok(());
    }
    let resolved = &spec.resolved_key;
    let eff_ctx = spec.setting_ctx.as_ref().or(tctx);
    let raw_new = if spec.allow_multiple {
        if !matches!(value, Value::List(_) | Value::Tuple(_)) {
            return Err(format!(
                "'{resolved}' allows multiple values and must be set in transition using a starlark list instead of single value '{}'",
                value.repr()
            ));
        }
        let seq = zel_starlark::eval::builtins::value_to_sequence(value).map_err(|e| e.0)?;
        let mut parts = Vec::with_capacity(seq.len());
        for item in &seq {
            parts.push(validate_scalar_for_spec(key, &spec.ty, item, eff_ctx)?);
        }
        parts.join(",")
    } else if spec.ty == "string_list" {
        if !matches!(value, Value::List(_) | Value::Tuple(_)) {
            return Err(format!(
                "expected value of type 'list(string)' for {key}, but got {} ({})",
                value.repr(),
                value.type_name()
            ));
        }
        let seq = zel_starlark::eval::builtins::value_to_sequence(value).map_err(|e| e.0)?;
        let mut parts = Vec::with_capacity(seq.len());
        for item in &seq {
            parts.push(validate_scalar_for_spec(key, "string", item, eff_ctx)?);
        }
        parts.join(",")
    } else if spec.ty == "string_set" {
        let items: Vec<Value> = match value {
            Value::Set(s) => s.lock().unwrap().elements.clone(),
            Value::List(_) | Value::Tuple(_) => {
                zel_starlark::eval::builtins::value_to_sequence(value).map_err(|e| e.0)?
            }
            other => {
                return Err(format!(
                    "expected value of type 'set(string)' for {key}, but got {} ({})",
                    other.repr(),
                    other.type_name()
                ))
            }
        };
        let mut seen = Vec::new();
        for item in &items {
            let s = validate_scalar_for_spec(key, "string", item, tctx)?;
            if !seen.contains(&s) {
                seen.push(s);
            }
        }
        seen.join(",")
    } else {
        validate_scalar_for_spec(key, &spec.ty, value, eff_ctx)?
    };

    let def_val = default_value_for_spec(spec);
    let def_rendered = if matches!(def_val, Value::None) {
        String::new()
    } else if let Value::Set(s) = &def_val {
        let elems: Vec<String> = s
            .lock()
            .unwrap()
            .elements
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        format!("[{}]", elems.join(", "))
    } else {
        render_java(&def_val, false)?
    };
    let new_rendered = crate::starlark_rule::render_flag_value(&spec.ty, &raw_new);
    let before_val = setting_value_with_spec(cfg, key, Some(spec));
    let before_java = if cfg.flag_values.contains_key(resolved) {
        if let Value::Set(s) = &before_val {
            let elems: Vec<String> = s
                .lock()
                .unwrap()
                .elements
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            Some(format!("[{}]", elems.join(", ")))
        } else {
            Some(render_java(&before_val, false)?)
        }
    } else {
        None
    };

    if new_rendered == def_rendered {
        cfg.flag_values.remove(resolved);
        cfg.flag_types.remove(resolved);
    } else {
        cfg.flag_values.insert(resolved.clone(), raw_new);
        cfg.flag_types.insert(resolved.clone(), spec.ty.clone());
    }

    let baseline = cfg
        .transition_baseline
        .entry(resolved.clone())
        .or_insert(before_java)
        .clone();
    if baseline.as_deref() == Some(new_rendered.as_str())
        || (baseline.is_none() && new_rendered == def_rendered)
    {
        cfg.transition_changes.remove(resolved);
    } else {
        cfg.transition_changes
            .insert(resolved.clone(), new_rendered);
    }
    Ok(())
}

/// Applies the Starlark transition `trans` (the struct produced by `transition(...)`) to `cfg`.
/// `attrs` is the `attr` struct passed to rule-level/attribute transitions. A split transition
/// (implementation returning a dict of dicts) is an error here; see [`apply_transition_split`].
pub fn apply_transition(
    trans: &Value,
    cfg: &BuildConfigurationKey,
    attrs: Option<&Value>,
) -> Result<BuildConfigurationKey, String> {
    apply_transition_with_resolver(trans, cfg, attrs, None)
}

pub fn apply_transition_with_resolver(
    trans: &Value,
    cfg: &BuildConfigurationKey,
    attrs: Option<&Value>,
    resolver: Option<&dyn crate::starlark_rule::DepResolver>,
) -> Result<BuildConfigurationKey, String> {
    let mut v = apply_transition_split_with_resolver(trans, cfg, attrs, resolver)?;
    if v.len() == 1 {
        Ok(v.remove(0).1)
    } else {
        Err(
            "Rule transition only allowed to return a single transitioned configuration."
                .to_string(),
        )
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
    apply_transition_split_with_resolver(trans, cfg, attrs, None)
}

/// Execution-platform resolution context (`AttributeTransitionData` +
/// `DependencyResolutionHelpers.getExecutionPlatformLabel`) used when evaluating `config.exec(...)`
/// transitions (standalone or composed via `.and_then(...)`).
#[derive(Clone, Copy)]
pub struct ExecTransitionContext<'a> {
    pub attr_name: &'a str,
    pub default_exec_platform: Option<&'a str>,
    pub exec_group_platforms: Option<&'a BTreeMap<String, String>>,
}

impl<'a> ExecTransitionContext<'a> {
    pub fn resolve_exec_platform(
        &self,
        exec_group: Option<&str>,
    ) -> Result<Option<&'a str>, String> {
        match exec_group {
            None => Ok(self.default_exec_platform),
            Some(g) => match self.exec_group_platforms {
                Some(groups) => match groups.get(g) {
                    Some(p) => Ok(Some(p.as_str())),
                    None => Err(format!(
                        "Attr '{}' declares a transition for non-existent exec group '{g}'",
                        self.attr_name
                    )),
                },
                None => Ok(self.default_exec_platform),
            },
        }
    }
}

fn compose_split_keys(key1: Option<&str>, key2: Option<String>) -> Option<String> {
    match (key1, key2) {
        (None, k) => k,
        (Some(a), None) => Some(a.to_string()),
        (Some(""), Some(b)) => Some(b),
        (Some(a), Some(b)) if b.is_empty() => Some(a.to_string()),
        (Some(a), Some(b)) => Some(format!("{a},{b}")),
    }
}

fn is_exec_transition_step(v: &Value) -> bool {
    matches!(
        v,
        Value::Struct(s)
            if s.fields.get("_build_setting").and_then(|v| v.as_str()) == Some("exec")
    )
}

pub fn apply_transition_split_with_resolver(
    trans: &Value,
    cfg: &BuildConfigurationKey,
    attrs: Option<&Value>,
    resolver: Option<&dyn crate::starlark_rule::DepResolver>,
) -> Result<Vec<(Option<String>, BuildConfigurationKey)>, String> {
    apply_transition_split_with_exec_ctx(trans, cfg, attrs, resolver, None)
}

pub fn apply_transition_split_with_exec_ctx(
    trans: &Value,
    cfg: &BuildConfigurationKey,
    attrs: Option<&Value>,
    resolver: Option<&dyn crate::starlark_rule::DepResolver>,
    exec_ctx: Option<&ExecTransitionContext<'_>>,
) -> Result<Vec<(Option<String>, BuildConfigurationKey)>, String> {
    let Value::Struct(s) = trans else {
        return Err(format!(
            "unsupported transition value of type '{}'",
            trans.type_name()
        ));
    };
    if let Some(Value::List(chain)) = s.fields.get("_composed_transitions") {
        let steps = chain.lock().unwrap().elements.clone();
        if steps
            .iter()
            .filter(|step| is_exec_transition_step(step))
            .count()
            > 1
        {
            return Err("can't compose two exec transitions".to_string());
        }
        let mut current: Vec<(Option<String>, BuildConfigurationKey)> = vec![(None, cfg.clone())];
        for step in &steps {
            let mut next = Vec::new();
            for (k0, c0) in current {
                for (k1, c1) in
                    apply_transition_split_with_exec_ctx(step, &c0, attrs, resolver, exec_ctx)?
                {
                    next.push((compose_split_keys(k0.as_deref(), k1), c1));
                }
            }
            current = next;
        }
        return Ok(current);
    }
    match s.fields.get("_build_setting").and_then(|v| v.as_str()) {
        Some("none") => {
            let mut out = cfg.clone();
            out.flag_values.clear();
            out.flag_types.clear();
            out.transition_changes.clear();
            return Ok(vec![(None, out)]);
        }
        Some("target") => {
            return Ok(vec![(None, cfg.clone())]);
        }
        Some("exec") => {
            let exec_group = s.fields.get("exec_group").and_then(|v| v.as_str());
            let plat = match exec_ctx {
                Some(ec) => ec.resolve_exec_platform(exec_group)?,
                None => None,
            };
            let next_cfg = match plat {
                Some(p) => cfg.to_exec_for(p),
                None => cfg.to_exec(),
            };
            return Ok(vec![(None, next_cfg)]);
        }
        _ => {}
    }
    let tctx = parse_transition_ctx(&s.fields);
    if let Some(Value::String(attr_name)) = s.fields.get("_feature_flag_transition_attr") {
        let mut out = cfg.clone();
        if let Some(Value::Struct(attr_s)) = attrs {
            if let Some(Value::Dict(d)) = attr_s.fields.get(attr_name) {
                for (k, v) in d.lock().unwrap().items() {
                    let key_str = match &k {
                        Value::Label(l) => l.canonical_form(),
                        Value::String(st) => canonicalize_setting(st, tctx.as_ref())?,
                        _ => continue,
                    };
                    let spec = resolve_build_setting(&key_str, &out, resolver)?;
                    set_setting_with_spec(&mut out, &key_str, &v, spec.as_ref(), tctx.as_ref())?;
                }
            }
        }
        return Ok(vec![(None, out)]);
    }
    // `analysis_test_transition(settings = {...})`: sets the given build settings.
    if let Some(Value::Dict(d)) = s
        .fields
        .get("settings")
        .filter(|_| s.fields.contains_key("_analysis_test_transition"))
    {
        let mut out = cfg.clone();
        for (k, v) in d.lock().unwrap().items() {
            let Some(k) = k.as_str().map(str::to_string) else {
                return Err("analysis_test_transition settings keys must be strings".to_string());
            };
            let ck = canonicalize_setting(&k, tctx.as_ref())?;
            let spec = resolve_build_setting(&ck, &out, resolver)?;
            set_setting_with_spec(&mut out, &ck, &v, spec.as_ref(), tctx.as_ref())?;
        }
        // Like Bazel's `PATCH_TRANSITION_KEY`: a split with a single anonymous entry, so that
        // `ctx.attr.<name>` is a list (`target_under_test[0]`).
        return Ok(vec![(Some(String::new()), out)]);
    }
    if let Some(Value::String(attr_name)) = s
        .fields
        .get("attribute")
        .filter(|_| s.fields.contains_key("_config_feature_flag_transition"))
    {
        let mut out = cfg.clone();
        if let Some(Value::Struct(attr_st)) = attrs {
            if let Some(Value::Dict(d)) = attr_st.fields.get(attr_name) {
                for (k, v) in d.lock().unwrap().items() {
                    let ck = match &k {
                        Value::Label(l) => l.canonical_form(),
                        Value::String(ks) => canonicalize_setting(ks, tctx.as_ref())?,
                        _ => continue,
                    };
                    if let Value::String(vs) = v {
                        out.flag_values.insert(ck.clone(), vs.clone());
                        out.transition_changes.insert(ck, vs);
                    }
                }
            }
        }
        return Ok(vec![(None, out)]);
    }
    let Some(imp) = s.fields.get("implementation") else {
        // `config.exec()` / `config.target()` / `config.none()` are handled by the caller; a
        // struct without implementation is not a transition.
        return Err("transition has no implementation".to_string());
    };
    let inputs = str_list(s.fields.get("inputs"));
    let outputs = str_list(s.fields.get("outputs"));
    let mut canonical_inputs = Vec::with_capacity(inputs.len());
    for i in &inputs {
        canonical_inputs.push(canonicalize_setting(i, tctx.as_ref())?);
    }
    let mut canonical_outputs = Vec::with_capacity(outputs.len());
    for o in &outputs {
        canonical_outputs.push(canonicalize_setting(o, tctx.as_ref())?);
    }
    let mut input_specs: BTreeMap<String, Option<BuildSettingSpec>> = BTreeMap::new();
    let mut output_specs: BTreeMap<String, Option<BuildSettingSpec>> = BTreeMap::new();
    for ci in &canonical_inputs {
        input_specs.insert(ci.clone(), resolve_build_setting(ci, cfg, resolver)?);
    }
    for co in &canonical_outputs {
        output_specs.insert(co.clone(), resolve_build_setting(co, cfg, resolver)?);
    }
    let mut settings: Vec<(Value, Value)> = Vec::new();
    for (i, ci) in inputs.iter().zip(canonical_inputs.iter()) {
        let spec = input_specs.get(ci).and_then(|s| s.as_ref());
        settings.push((
            Value::String(norm_key(i)),
            setting_value_with_spec(cfg, ci, spec),
        ));
    }
    let settings_dict = Value::Dict(std::sync::Arc::new(std::sync::Mutex::new(
        zel_starlark::StarlarkDict::from_entries(settings).map_err(|e| e.0)?,
    )));
    let mut ctx = EvalContext::default();
    let sibling_layout = resolver
        .and_then(|r| r.package_loader())
        .map(|l| l.config().sibling_repository_layout)
        .unwrap_or(false);
    ctx.semantics.experimental_sibling_repository_layout = sibling_layout;
    ctx.thread.semantics.experimental_sibling_repository_layout = sibling_layout;
    let tctx_for_label = tctx.clone();
    ctx.host = Some(std::sync::Arc::new(move |ctx, host_name, pos, _kw| {
        if host_name == "Label" {
            let v = pos.first().ok_or_else(|| {
                zel_starlark::StarlarkError(
                    "Label() missing 1 required positional argument: input".into(),
                )
            })?;
            return match v {
                Value::Label(_) => Ok(v.clone()),
                Value::String(s) => {
                    if let Some((pkg, mapping)) =
                        ctx.current_label_ctx().or_else(|| tctx_for_label.clone())
                    {
                        zel_cmdline::Label::parse_with_package_context(s, &pkg, &mapping)
                            .map(Value::Label)
                            .map_err(|e| zel_starlark::StarlarkError(e.to_string()))
                    } else {
                        zel_cmdline::Label::parse_canonical(s)
                            .map(Value::Label)
                            .map_err(|e| zel_starlark::StarlarkError(e.to_string()))
                    }
                }
                other => Err(zel_starlark::StarlarkError(format!(
                    "Label() got {} for 'input', want string or Label",
                    other.type_name()
                ))),
            };
        }
        Err(zel_starlark::StarlarkError(format!(
            "unknown builtin 'host:{host_name}'"
        )))
    }));
    let mut args = vec![settings_dict];
    if let Some(a) = attrs {
        args.push(a.clone());
    } else {
        args.push(Value::Struct(std::sync::Arc::new(
            zel_starlark::StructValue::default(),
        )));
    }
    let r = ctx
        .call_value(imp.clone(), args, BTreeMap::new())
        .map_err(|e| e.0)?;
    if matches!(r, Value::None) {
        return Ok(vec![(None, cfg.clone())]);
    }
    let apply_one = |entries: Vec<(Value, Value)>| -> Result<BuildConfigurationKey, String> {
        if entries.is_empty() && canonical_outputs.is_empty() {
            return Ok(cfg.clone());
        }
        let mut canon_entries: Vec<(String, Value)> = Vec::with_capacity(entries.len());
        for (k, v) in entries {
            let Some(ks) = k.as_str() else {
                return Err(format!(
                    "transition output key must be a string, got {}",
                    k.type_name()
                ));
            };
            let ck = canonicalize_setting(ks, tctx.as_ref())?;
            canon_entries.push((ck, v));
        }
        let returned: Vec<&String> = canon_entries.iter().map(|(k, _)| k).collect();
        // The implementation must return exactly the declared outputs.
        for co in &canonical_outputs {
            if !returned.contains(&co) {
                return Err(format!(
                    "transition function returned dictionary with missing key '{co}'; declared outputs are {canonical_outputs:?}"
                ));
            }
        }
        for r in &returned {
            if !canonical_outputs.contains(*r) {
                return Err(format!(
                    "transition function returned undeclared output '{r}'; declared outputs are {canonical_outputs:?}"
                ));
            }
        }
        let mut out = cfg.clone();
        let old_cpu = out.cpu.clone();
        for (ck, v) in canon_entries {
            let spec = output_specs.get(&ck).and_then(|s| s.as_ref());
            set_setting_with_spec(&mut out, &ck, &v, spec, tctx.as_ref())?;
        }
        if out.cpu != old_cpu
            && !canonical_outputs
                .iter()
                .any(|o| o == "//command_line_option:platforms")
        {
            out.platforms.clear();
        }
        Ok(out)
    };
    match r {
        Value::List(l) => {
            let elems = l.lock().unwrap().elements.clone();
            if elems.is_empty() {
                return Ok(Vec::new());
            }
            let mut res = Vec::with_capacity(elems.len());
            for (idx, elem) in elems.into_iter().enumerate() {
                let Value::Dict(inner) = elem else {
                    return Err(format!(
                        "transition split list element must be a dict, got {}",
                        elem.type_name()
                    ));
                };
                let entries = inner.lock().unwrap().items();
                res.push((Some(idx.to_string()), apply_one(entries)?));
            }
            Ok(res)
        }
        Value::Dict(d) => {
            let items = d.lock().unwrap().items();
            if items.is_empty() {
                return Ok(vec![(None, cfg.clone())]);
            }
            let is_split =
                !items.is_empty() && items.iter().all(|(_, v)| matches!(v, Value::Dict(_)));
            if is_split {
                let mut res = Vec::new();
                for (k, v) in items {
                    let Some(key) = k.as_str().map(str::to_string) else {
                        return Err(format!(
                            "transition split key must be a string, got {}",
                            k.type_name()
                        ));
                    };
                    let Value::Dict(inner) = v else {
                        unreachable!()
                    };
                    let entries = inner.lock().unwrap().items();
                    res.push((Some(key), apply_one(entries)?));
                }
                Ok(res)
            } else {
                Ok(vec![(None, apply_one(items)?)])
            }
        }
        other => Err(format!(
            "transition function returned {}, want dict or list of dicts",
            other.type_name()
        )),
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
        set_setting(
            &mut c,
            "//command_line_option:compilation_mode",
            &Value::String("opt".into()),
        )
        .unwrap();
        assert_eq!(c.compilation_mode, "opt");
        // compilation_mode is explicit in the directory name: no ST-hash.
        assert_eq!(c.mnemonic(), "k8-opt");
        // No change → no ST suffix.
        let mut d = BuildConfigurationKey::target("k8", "fastbuild");
        set_setting(
            &mut d,
            "//command_line_option:compilation_mode",
            &Value::String("fastbuild".into()),
        )
        .unwrap();
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

    #[test]
    fn compose_split_keys_absorbs_patch_keys_and_joins_splits_with_comma() {
        assert_eq!(compose_split_keys(None, None), None);
        assert_eq!(compose_split_keys(None, Some("k".into())), Some("k".into()));
        assert_eq!(compose_split_keys(Some("k"), None), Some("k".into()));
        assert_eq!(
            compose_split_keys(Some(""), Some("k".into())),
            Some("k".into())
        );
        assert_eq!(
            compose_split_keys(Some("k"), Some(String::new())),
            Some("k".into())
        );
        assert_eq!(
            compose_split_keys(Some(""), Some(String::new())),
            Some(String::new())
        );
        assert_eq!(
            compose_split_keys(Some("stub_split0"), Some("stub_split1".into())),
            Some("stub_split0,stub_split1".into())
        );
    }
}
