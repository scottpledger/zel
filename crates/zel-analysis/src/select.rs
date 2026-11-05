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

//! `select()` resolution at analysis time (`ConfiguredAttributeMapper.resolveSelector`).
//!
//! Each condition label is analysed (a `config_setting`) and must provide a
//! `ConfigMatchingProvider` with `matches` and a `settings` list used for the
//! "most specialised match wins" rule.

use crate::starlark_rule::DepResolver;
use crate::BuildConfigurationKey;
use std::collections::BTreeMap;
use zel_cmdline::Label;
use zel_packages::attr::{AttrValue, SelectPart};
use zel_packages::RuleInstance;
use zel_starlark::Value;

/// Reads a `ConfigMatchingProvider` into `(matches, settings)`.
pub fn matching_info(v: &Value) -> Option<(bool, Vec<String>)> {
    let Value::ProviderInstance { fields, .. } = v else { return None };
    let matches = matches!(fields.get("matches"), Some(Value::Bool(true)));
    let settings = match fields.get("settings") {
        Some(l) => zel_starlark::eval::builtins::value_to_sequence(l)
            .ok()?
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
        None => Vec::new(),
    };
    Some((matches, settings))
}

fn refines(a: &[String], b: &[String]) -> bool {
    a.len() > b.len() && b.iter().all(|x| a.contains(x))
}

fn is_default(l: &Label) -> bool {
    l.canonical_form() == "//conditions:default"
}

fn concat(a: AttrValue, b: AttrValue, attr: &str) -> Result<AttrValue, String> {
    use AttrValue::*;
    Ok(match (a, b) {
        (StrList(mut x), StrList(y)) => {
            x.extend(y);
            StrList(x)
        }
        (LabelList(mut x), LabelList(y)) => {
            x.extend(y);
            LabelList(x)
        }
        (OutputList(mut x), OutputList(y)) => {
            x.extend(y);
            OutputList(x)
        }
        (Str(x), Str(y)) => Str(x + &y),
        (StrDict(mut x), StrDict(y)) => {
            for (k, v) in y {
                if x.insert(k.clone(), v).is_some() {
                    return Err(format!("Duplicate key '{k}' when concatenating dicts in attribute '{attr}'"));
                }
            }
            StrDict(x)
        }
        (StrListDict(mut x), StrListDict(y)) => {
            for (k, v) in y {
                if x.insert(k.clone(), v).is_some() {
                    return Err(format!("Duplicate key '{k}' when concatenating dicts in attribute '{attr}'"));
                }
            }
            StrListDict(x)
        }
        (LabelKeyedStrDict(mut x), LabelKeyedStrDict(y)) => {
            x.extend(y);
            LabelKeyedStrDict(x)
        }
        (a, b) => return Err(format!("Cannot concatenate values of attribute '{attr}': {a:?} and {b:?}")),
    })
}

/// Resolves all `select()`s in `av` under `cfg`; non-configurable values are returned as is.
pub fn resolve_select(
    av: &AttrValue,
    attr: &str,
    rule: &RuleInstance,
    cfg: &BuildConfigurationKey,
    resolver: &dyn DepResolver,
) -> Result<AttrValue, String> {
    let AttrValue::Select(parts) = av else { return Ok(av.clone()) };
    let mut acc: Option<AttrValue> = None;
    for part in parts {
        let v = match part {
            SelectPart::Plain(v) => resolve_select(v, attr, rule, cfg, resolver)?,
            SelectPart::Select { entries, no_match_error } => {
                resolve_selector(entries, no_match_error, attr, rule, cfg, resolver)?
            }
        };
        acc = Some(match acc {
            None => v,
            Some(a) => concat(a, v, attr)?,
        });
    }
    acc.ok_or_else(|| format!("empty select in attribute '{attr}'"))
}

fn resolve_selector(
    entries: &[(Label, AttrValue)],
    no_match_error: &str,
    attr: &str,
    rule: &RuleInstance,
    cfg: &BuildConfigurationKey,
    resolver: &dyn DepResolver,
) -> Result<AttrValue, String> {
    let label = &rule.label;
    let mut matching: Vec<(Label, AttrValue, Vec<String>)> = Vec::new();
    let mut condition_labels: Vec<&Label> = Vec::new();
    let mut default: Option<&AttrValue> = None;
    let mut errors: Vec<String> = Vec::new();
    for (cond, value) in entries {
        if is_default(cond) {
            default = Some(value);
            continue;
        }
        condition_labels.push(cond);
        let ct = resolver.configured_target(cond, cfg)?;
        let _ = resolver.check_dependency(rule, attr, &ct);
        // A constraint_value is a valid select() key: it matches when the target platform
        // has it (evaluated lazily to avoid cycles through the platform's own constraints).
        let synthesized;
        let info = if let Some(i) = ct.providers.get("ConfigMatchingProvider") {
            Some(i)
        } else if ct.providers.contains_key("ConstraintValueInfo") {
            let m = resolver.target_platform_has_constraint(cfg, cond)?;
            synthesized = crate::starlark_rule::provider_instance(
                "ConfigMatchingProvider",
                vec![
                    ("matches", Value::Bool(m)),
                    ("settings", crate::starlark_rule::mk_list(vec![Value::String(format!("constraint:{}", cond.canonical_form()))])),
                    ("label", Value::Label(cond.clone())),
                ],
            );
            Some(&synthesized)
        } else {
            None
        };
        let Some(info) = info else {
            return Err(format!(
                "{} is not a valid select() condition for {}.\nNote: Currently, only config_setting targets are allowed as select() conditions.",
                cond.canonical_form(),
                label.canonical_form()
            ));
        };
        if let Value::ProviderInstance { fields, .. } = info {
            if let Some(Value::List(l)) = fields.get("errors") {
                let msgs: Vec<String> = l.lock().unwrap().elements.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
                if !msgs.is_empty() {
                    errors.push(format!(
                        "config_setting {} is unresolvable because: {}",
                        cond.canonical_form(),
                        msgs.join(", ")
                    ));
                    continue;
                }
            }
        }
        let (m, settings) = matching_info(info).unwrap();
        if m {
            let mut suppressed = false;
            let mut i = 0;
            while i < matching.len() {
                if refines(&settings, &matching[i].2) {
                    matching.remove(i);
                    continue;
                } else if refines(&matching[i].2, &settings) {
                    suppressed = true;
                    break;
                }
                i += 1;
            }
            if !suppressed {
                matching.push((cond.clone(), value.clone(), settings));
            }
        }
    }
    if !errors.is_empty() {
        return Err(format!(
            "Unresolvable config_settings for configurable attribute \"{attr}\" in {}:\n{}",
            label.canonical_form(),
            errors.join("\n")
        ));
    }
    let mut distinct: Vec<&AttrValue> = Vec::new();
    for (_, v, _) in &matching {
        if !distinct.contains(&v) {
            distinct.push(v);
        }
    }
    if distinct.len() > 1 {
        return Err(format!(
            "Illegal ambiguous match on configurable attribute \"{attr}\" in {}:\n{}\nMultiple matches are not allowed unless one is unambiguously more specialized or they resolve to the same value. See https://bazel.build/reference/be/functions#select.",
            label.canonical_form(),
            matching.iter().map(|(l, _, _)| l.canonical_form()).collect::<Vec<_>>().join("\n")
        ));
    }
    if let Some((_, v, _)) = matching.into_iter().next() {
        return resolve_select(&v, attr, rule, cfg, resolver);
    }
    if let Some(d) = default {
        return resolve_select(d, attr, rule, cfg, resolver);
    }
    let mut error = format!(
        "configurable attribute \"{attr}\" in {} doesn't match this configuration",
        label.canonical_form()
    );
    if !no_match_error.is_empty() {
        error.push_str(&format!(": {no_match_error}\n"));
    } else {
        error.push_str(&format!(
            ". Would a default condition help?\n\nConditions checked:\n {}\n\nTo see a condition's definition, run: bazel query --output=build <condition label>.\n",
            condition_labels.iter().map(|l| l.canonical_form()).collect::<Vec<_>>().join("\n ")
        ));
    }
    let short = cfg.short_hash();
    error.push_str(&format!(
        "\nThis instance of {} has configuration identifier {short}. To inspect its configuration, run: bazel config {short}.\n",
        label.canonical_form()
    ));
    error.push_str("\nFor more help, see https://bazel.build/docs/configurable-attributes#faq-select-choose-condition.\n\n");
    Err(error)
}

#[allow(dead_code)]
fn _unused(_: BTreeMap<String, String>) {}
