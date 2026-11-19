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

//! `config_setting` and the platform/constraint/toolchain declaration rules
//! (`constraint_setting`, `constraint_value`, `platform`, `toolchain_type`, `toolchain`).

use crate::native::Nctx;
use std::collections::BTreeMap;
use zel_analysis::starlark_rule::{mk_dict, mk_list, provider_instance};
use zel_analysis::{ConfiguredTarget, DefaultInfo};
use zel_cmdline::Label;
use zel_packages::attr::AttrValue;
use zel_starlark::Value;

fn base(n: &Nctx) -> ConfiguredTarget {
    let mut ct = ConfiguredTarget::new(
        n.rule.label.clone(),
        n.cfg.clone(),
        n.rule.rule_class.clone(),
    );
    ct.default_info = DefaultInfo::default();
    ct.providers.insert(
        "DefaultInfo".into(),
        zel_analysis::starlark_rule::default_info_provider(
            &[],
            Default::default(),
            Default::default(),
            None,
        ),
    );
    ct.testonly = n.rule.testonly;
    ct
}

fn lbl(l: &Label) -> Value {
    Value::Label(l.clone())
}

// ------------------------------------------------------------------------ config_setting

fn command_option_def(name: &str) -> Option<&'static zel_options::OptionDefinition> {
    static MAP: std::sync::OnceLock<
        std::collections::HashMap<&'static str, &'static zel_options::OptionDefinition>,
    > = std::sync::OnceLock::new();
    MAP.get_or_init(zel_options::flag_tables::get_command_options_map)
        .get(name)
        .copied()
}

fn option_value_to_string(v: &zel_options::types::OptionValue) -> String {
    use zel_options::types::OptionValue;
    match v {
        OptionValue::Label(l) => Label::parse_canonical(l)
            .map(|p| p.canonical_form())
            .unwrap_or_else(|_| l.clone()),
        _ => v.to_string_repr(),
    }
}

fn scalar_option_matches(
    def: &zel_options::OptionDefinition,
    actual_raw: &str,
    expected: &zel_options::types::OptionValue,
) -> bool {
    use zel_options::types::{OptionType, OptionValue};
    match zel_options::converters::convert_option(actual_raw, def) {
        Ok(act) => match (&act, expected) {
            (OptionValue::String(a), OptionValue::String(b))
                if matches!(def.option_type, OptionType::Enum(_)) =>
            {
                a.eq_ignore_ascii_case(b)
            }
            (OptionValue::Label(a), OptionValue::Label(b)) => {
                option_value_to_string(&act) == option_value_to_string(expected) || a == b
            }
            _ => act == *expected,
        },
        Err(_) => actual_raw == option_value_to_string(expected),
    }
}

fn actual_option_list(
    cfg: &zel_analysis::BuildConfigurationKey,
    bo: &zel_analysis::build_options::BuildOptions,
    canonical: &str,
) -> Vec<String> {
    if canonical == "platforms" {
        return cfg.platforms.clone();
    }
    if let Some(zel_analysis::build_options::ConfigValue::List(l)) = bo.get(canonical) {
        if !l.is_empty() || !cfg.options.contains_key(canonical) {
            return l.clone();
        }
    }
    match cfg.options.get(canonical) {
        Some(raw) if !raw.is_empty() => raw.split(',').map(str::to_string).collect(),
        _ => Vec::new(),
    }
}

fn actual_option_scalar(
    cfg: &zel_analysis::BuildConfigurationKey,
    bo: &zel_analysis::build_options::BuildOptions,
    canonical: &str,
    def: Option<&zel_options::OptionDefinition>,
) -> Option<String> {
    match canonical {
        "compilation_mode" => return Some(cfg.compilation_mode.clone()),
        "cpu" => return Some(cfg.cpu.clone()),
        _ => {}
    }
    if let Some(raw) = cfg.options.get(canonical) {
        return Some(raw.clone());
    }
    if let Some(zel_analysis::build_options::ConfigValue::Scalar(s)) = bo.get(canonical) {
        if s == "null" {
            return Some(String::new());
        }
        return Some(s.clone());
    }
    if let Some(d) = def {
        if let Some(v) = d.default_value {
            if v == "null" {
                return Some(String::new());
            }
            return Some(v.to_string());
        }
        if matches!(d.option_type, zel_options::types::OptionType::String) {
            return Some(String::new());
        }
    }
    zel_analysis::build_options::find_option(canonical).map(|(_, opt)| {
        if opt.default == "null" {
            String::new()
        } else {
            opt.default.to_string()
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn match_starlark_flag_entry(
    n: &Nctx,
    attr_name: &str,
    raw_label: &Label,
    expected: &str,
    seen_flags: &mut std::collections::HashSet<String>,
    settings: &mut Vec<String>,
    errors: &mut Vec<String>,
    matches: &mut bool,
) -> Result<(), String> {
    let ct = n.resolver.configured_target(raw_label, n.cfg)?;
    n.resolver.check_dependency(n.rule, attr_name, &ct)?;
    let resolved_label = ct.actual_target_label().clone();
    if !seen_flags.insert(resolved_label.canonical_form()) {
        errors.push(format!(
            "flag_values cannot reference the same build setting '{}' more than once",
            resolved_label.canonical_form()
        ));
        *matches = false;
        return Ok(());
    }
    if let Some(Value::ProviderInstance { fields, .. }) = ct.providers.get("FeatureFlagInfo") {
        if let Some(Value::String(err_msg)) = fields.get("error") {
            if !err_msg.is_empty() {
                errors.push(err_msg.clone());
                *matches = false;
                return Ok(());
            }
        }
        if let Some(valid @ (Value::List(_) | Value::Tuple(_) | Value::Set(_))) =
            fields.get("valid_values")
        {
            let allowed: Vec<String> = match valid {
                Value::Set(s) => s
                    .lock()
                    .unwrap()
                    .elements
                    .iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect(),
                _ => zel_starlark::eval::builtins::value_to_sequence(valid)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect(),
            };
            if !allowed.is_empty() && !allowed.iter().any(|a| a == expected) {
                errors.push(format!(
                    "Error while validating feature flag {}: '{}' is not a valid value (allowed: {:?})",
                    raw_label.canonical_form(),
                    expected,
                    allowed
                ));
                *matches = false;
                return Ok(());
            }
        }
        let actual = fields.get("value").cloned().unwrap_or(Value::None);
        settings.push(format!(
            "flag:{}={expected}",
            resolved_label.canonical_form()
        ));
        if actual.as_str() != Some(expected) {
            *matches = false;
        }
        return Ok(());
    }

    let actual = match ct.providers.get("BuildSettingProvider") {
        Some(Value::ProviderInstance { fields, .. }) => fields.get("value").cloned(),
        _ => None,
    };
    let Some(actual) = actual else {
        errors.push(format!(
            "{} does not have mandatory providers: 'BuildSettingProvider'",
            raw_label.canonical_form()
        ));
        *matches = false;
        return Ok(());
    };

    let (norm_expected, matched) = match &actual {
        Value::Bool(act_b) => match zel_options::convert_boolean(expected) {
            Ok(exp_b) => (exp_b.to_string(), *act_b == exp_b),
            Err(e) => {
                errors.push(format!(
                    "Error while parsing user-defined flag {}={expected}: {}",
                    raw_label.canonical_form(),
                    e.message
                ));
                (expected.to_string(), false)
            }
        },
        Value::Int(act_i) => match zel_options::convert_long(expected) {
            Ok(exp_i) => (exp_i.to_string(), act_i.to_string() == exp_i.to_string()),
            Err(e) => {
                errors.push(format!(
                    "Error while parsing user-defined flag {}={expected}: {}",
                    raw_label.canonical_form(),
                    e.message
                ));
                (expected.to_string(), false)
            }
        },
        Value::Label(act_lbl) => {
            let pkg = n.rule.label.package_identifier();
            let entries = n
                .resolver
                .repo_mapping_entries(pkg.repository().name())
                .into_iter()
                .map(|(a, c)| (a, zel_cmdline::RepositoryName::create_unvalidated(c)))
                .collect();
            let mapping = zel_cmdline::RepositoryMapping::new(entries, pkg.repository().clone());
            match Label::parse_with_package_context(expected, pkg, &mapping) {
                Ok(exp_lbl) => {
                    let cf = exp_lbl.canonical_form();
                    (cf, act_lbl == &exp_lbl)
                }
                Err(e) => {
                    errors.push(format!(
                        "Error while parsing user-defined flag {}={expected}: {e}",
                        raw_label.canonical_form()
                    ));
                    (expected.to_string(), false)
                }
            }
        }
        Value::List(_) | Value::Tuple(_) | Value::Set(_) => {
            let elems: Vec<String> = match &actual {
                Value::Set(s) => s
                    .lock()
                    .unwrap()
                    .elements
                    .iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect(),
                _ => zel_starlark::eval::builtins::value_to_sequence(&actual)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|x| match x {
                        Value::String(s) => Some(s),
                        Value::Int(i) => Some(i.to_string()),
                        Value::Bool(b) => Some(b.to_string()),
                        Value::Label(l) => Some(l.canonical_form()),
                        _ => None,
                    })
                    .collect(),
            };
            let is_repeatable_scalar = n
                .resolver
                .rule_instance(&resolved_label)
                .ok()
                .flatten()
                .and_then(|r| {
                    r.extra
                        .rule_class
                        .as_ref()
                        .and_then(|rc| rc.starlark.as_ref())
                        .and_then(|sp| sp.kwargs.get("build_setting"))
                        .and_then(|bs| match bs {
                            Value::Struct(s) => Some(matches!(
                                s.fields.get("allow_multiple"),
                                Some(Value::Bool(true))
                            )),
                            _ => None,
                        })
                })
                .unwrap_or(false);
            if is_repeatable_scalar {
                (expected.to_string(), elems.iter().any(|x| x == expected))
            } else {
                let parts: Vec<&str> = if expected.is_empty() {
                    Vec::new()
                } else {
                    expected.split(',').collect()
                };
                if parts.len() > 1 {
                    errors.push(format!(
                        "\"{expected}\" not a valid value for flag {}: cannot match multi-element list value in config_setting",
                        raw_label.canonical_form()
                    ));
                    (expected.to_string(), false)
                } else if parts.is_empty() {
                    (String::new(), elems.is_empty())
                } else {
                    (expected.to_string(), elems.iter().any(|x| x == parts[0]))
                }
            }
        }
        Value::String(s) => (expected.to_string(), s == expected),
        _ => (expected.to_string(), false),
    };
    settings.push(format!(
        "flag:{}={norm_expected}",
        resolved_label.canonical_form()
    ));
    if !matched {
        *matches = false;
    }
    Ok(())
}

pub(crate) fn config_setting(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let mut settings: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut matches = true;
    let mut seen_flags: std::collections::HashSet<String> = std::collections::HashSet::new();
    let bo = n.cfg.build_options();

    let mut flag_aliases: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    if let Some(zel_analysis::build_options::ConfigValue::List(aliases)) = bo.get("flag_alias") {
        for entry in aliases {
            if let Some((alias, target)) = entry.split_once('=') {
                flag_aliases.insert(alias.to_string(), target.to_string());
            }
        }
    }

    if let Some(AttrValue::StrDict(values)) = n.value("values")? {
        for (k, v) in values {
            let key = k.trim_start_matches("--").to_string();
            if let Some(alias_target) = n
                .cfg
                .flag_alias_target(&key)
                .or_else(|| flag_aliases.get(&key).cloned())
            {
                let pkg = n.rule.label.package_identifier();
                let entries = n
                    .resolver
                    .repo_mapping_entries(pkg.repository().name())
                    .into_iter()
                    .map(|(a, c)| (a, zel_cmdline::RepositoryName::create_unvalidated(c)))
                    .collect();
                let mapping =
                    zel_cmdline::RepositoryMapping::new(entries, pkg.repository().clone());
                match Label::parse_with_package_context(&alias_target, pkg, &mapping) {
                    Ok(lbl_target) => {
                        match_starlark_flag_entry(
                            n,
                            ":flag_alias_settings",
                            &lbl_target,
                            &v,
                            &mut seen_flags,
                            &mut settings,
                            &mut errors,
                            &mut matches,
                        )?;
                    }
                    Err(e) => {
                        errors.push(format!("Error loading flag_alias '{key}': {e}"));
                        matches = false;
                    }
                }
                continue;
            }
            if key == "define" {
                let Some((dk, dv)) = v.split_once('=') else {
                    errors.push(
                        "Variable definitions must be in the form of a 'name=value' assignment"
                            .to_string(),
                    );
                    matches = false;
                    continue;
                };
                settings.push(format!("values:define={v}"));
                if n.cfg.user_defines.get(dk).map(|x| x.as_str()) != Some(dv) {
                    matches = false;
                }
                continue;
            }
            let canonical = if key == "c" {
                "compilation_mode"
            } else {
                key.as_str()
            };
            let def = command_option_def(canonical);
            let known = def.is_some()
                || KNOWN_OPTIONS.contains(&canonical)
                || zel_analysis::build_options::find_option(canonical).is_some();
            if !known {
                errors.push(format!("unknown option: '{key}'"));
                matches = false;
                continue;
            }
            if let Some(d) = def {
                let converted = match zel_options::converters::convert_option(&v, d) {
                    Ok(cv) => cv,
                    Err(e) => {
                        errors.push(format!(
                            "Error while parsing option {key}='{v}': {}",
                            e.message
                        ));
                        matches = false;
                        continue;
                    }
                };
                settings.push(format!(
                    "values:{canonical}={}",
                    option_value_to_string(&converted)
                ));
                if d.allow_multiple || matches!(converted, zel_options::types::OptionValue::List(_))
                {
                    let act_list = actual_option_list(n.cfg, &bo, canonical);
                    match &converted {
                        zel_options::types::OptionValue::List(exp_items) => {
                            if exp_items.is_empty() {
                                if !act_list.is_empty() {
                                    matches = false;
                                }
                            } else {
                                let norm_act: Vec<String> = act_list
                                    .iter()
                                    .map(|a| {
                                        Label::parse_canonical(a)
                                            .map(|l| l.canonical_form())
                                            .unwrap_or_else(|_| a.clone())
                                    })
                                    .collect();
                                for exp_item in exp_items {
                                    let want = option_value_to_string(exp_item);
                                    if !act_list.contains(&want) && !norm_act.contains(&want) {
                                        matches = false;
                                        break;
                                    }
                                }
                            }
                        }
                        _ => {
                            if !act_list
                                .iter()
                                .any(|act_elem| scalar_option_matches(d, act_elem, &converted))
                            {
                                matches = false;
                            }
                        }
                    }
                } else {
                    match actual_option_scalar(n.cfg, &bo, canonical, Some(d)) {
                        Some(act_raw) => {
                            if !scalar_option_matches(d, &act_raw, &converted) {
                                matches = false;
                            }
                        }
                        None => {
                            if !option_value_to_string(&converted).is_empty() {
                                matches = false;
                            }
                        }
                    }
                }
            } else {
                settings.push(format!("values:{canonical}={v}"));
                match actual_option_scalar(n.cfg, &bo, canonical, None) {
                    Some(act_raw) => {
                        if act_raw != v {
                            matches = false;
                        }
                    }
                    None => matches = false,
                }
            }
        }
    }
    if let Some(AttrValue::StrDict(defs)) = n.value("define_values")? {
        for (k, v) in defs {
            settings.push(format!("define:{k}={v}"));
            if n.cfg.user_defines.get(&k) != Some(&v) {
                matches = false;
            }
        }
    }
    if let Some(AttrValue::LabelKeyedStrDict(flags)) = n.value("flag_values")? {
        for (l, expected) in flags {
            match_starlark_flag_entry(
                n,
                "flag_values",
                &l,
                &expected,
                &mut seen_flags,
                &mut settings,
                &mut errors,
                &mut matches,
            )?;
        }
    }
    let constraint_values = n.labels("constraint_values")?;
    if !constraint_values.is_empty() {
        let platform = crate::toolchain::target_platform(n.cfg, n.resolver, n.acfg)?;
        let mut seen_settings: BTreeMap<String, Label> = BTreeMap::new();
        for cv in &constraint_values {
            let cv_ct = n.resolver.configured_target(cv, n.cfg)?;
            n.resolver
                .check_dependency(n.rule, "constraint_values", &cv_ct)?;
            let Some(Value::ProviderInstance {
                fields: cv_fields, ..
            }) = cv_ct.providers.get("ConstraintValueInfo")
            else {
                settings.push(format!("constraint:{}", cv.canonical_form()));
                errors.push(format!("{} is not a constraint_value", cv.canonical_form()));
                matches = false;
                continue;
            };
            let real_cv = match cv_fields.get("label") {
                Some(Value::Label(l)) => l.clone(),
                _ => cv.clone(),
            };
            if let Some(Value::ProviderInstance {
                fields: cs_fields, ..
            }) = cv_fields.get("constraint")
            {
                if let Some(Value::Label(cs_lbl)) = cs_fields.get("label") {
                    let cs_key = cs_lbl.canonical_form();
                    if let Some(prev) = seen_settings.get(&cs_key) {
                        if prev != &real_cv {
                            errors.push(format!(
                                "Duplicate constraint values detected: constraint_setting {} has [{}, {}]",
                                cs_key,
                                prev.canonical_form(),
                                real_cv.canonical_form()
                            ));
                            matches = false;
                            continue;
                        }
                    } else {
                        seen_settings.insert(cs_key, real_cv.clone());
                    }
                }
            }
            settings.push(format!("constraint:{}", real_cv.canonical_form()));
            if let Some(Value::List(refs)) = cv_fields.get("refines") {
                for r in &refs.lock().unwrap().elements {
                    if let Value::Label(rl) = r {
                        let s = format!("constraint:{}", rl.canonical_form());
                        if !settings.contains(&s) {
                            settings.push(s);
                        }
                    }
                }
            }
            match &platform {
                Some(p) => {
                    if !crate::toolchain::platform_has_value(p, &cv_ct) {
                        matches = false;
                    }
                }
                None => matches = false,
            }
        }
    }
    if settings.is_empty() && errors.is_empty() {
        errors.push(
            "Either values, define_values, flag_values or constraint_values must be specified"
                .to_string(),
        );
    }
    if !errors.is_empty() {
        return Err(format!(
            "in config_setting rule {}: {}",
            n.rule.label.canonical_form(),
            errors.join("; ")
        ));
    }
    let mut ct = base(n);
    ct.providers.insert(
        "ConfigMatchingProvider".into(),
        provider_instance(
            "ConfigMatchingProvider",
            vec![
                ("matches", Value::Bool(matches)),
                (
                    "settings",
                    mk_list(settings.into_iter().map(Value::String).collect()),
                ),
                ("label", lbl(&n.rule.label)),
            ],
        ),
    );
    Ok(ct)
}

const KNOWN_OPTIONS: &[&str] = &[
    "copt",
    "cxxopt",
    "conlyopt",
    "linkopt",
    "strip",
    "stamp",
    "javacopt",
    "host_cpu",
    "crosstool_top",
    "apple_platform_type",
    "ios_multi_cpus",
    "macos_cpus",
    "fission",
    "features",
    "compiler",
    "java_language_version",
    "java_runtime_version",
    "test_env",
    "action_env",
    "host_platform",
    "extra_execution_platforms",
    "extra_toolchains",
    "dynamic_mode",
    "collect_code_coverage",
];

// ------------------------------------------------------------------------ platform rules

pub(crate) fn constraint_setting(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let default = match n.value("default_constraint_value")? {
        Some(AttrValue::Label(Some(l))) => lbl(&l),
        _ => Value::None,
    };
    let refines = match n.value("refines_constraint_value")? {
        Some(AttrValue::Label(Some(l))) => {
            let core_cfg =
                zel_analysis::BuildConfigurationKey::target(&n.cfg.cpu, "fastbuild").to_core_only();
            let cv_ct = n.resolver.configured_target(&l, &core_cfg)?;
            let _ = n
                .resolver
                .check_dependency(n.rule, "refines_constraint_value", &cv_ct);
            let mut chain = Vec::new();
            if let Some(Value::ProviderInstance { fields, .. }) =
                cv_ct.providers.get("ConstraintValueInfo")
            {
                if let Some(Value::Label(rl)) = fields.get("label") {
                    chain.push(lbl(rl));
                } else {
                    chain.push(lbl(&l));
                }
                if let Some(Value::List(r)) = fields.get("refines") {
                    chain.extend(r.lock().unwrap().elements.iter().cloned());
                }
            } else {
                chain.push(lbl(&l));
            }
            mk_list(chain)
        }
        _ => mk_list(vec![]),
    };
    let mut ct = base(n);
    ct.providers.insert(
        "ConstraintSettingInfo".into(),
        provider_instance(
            "ConstraintSettingInfo",
            vec![
                ("label", lbl(&n.rule.label)),
                ("default_constraint_value", default),
                ("refines_constraint_values", refines),
            ],
        ),
    );
    Ok(ct)
}

pub(crate) fn constraint_value(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let setting = match n.value("constraint_setting")? {
        Some(AttrValue::Label(Some(l))) => l,
        _ => return Err(format!("{}: missing value for mandatory attribute 'constraint_setting' in 'constraint_value' rule", n.rule.label)),
    };
    let core_cfg =
        zel_analysis::BuildConfigurationKey::target(&n.cfg.cpu, "fastbuild").to_core_only();
    let s_ct = n.resolver.configured_target(&setting, &core_cfg)?;
    let _ = n
        .resolver
        .check_dependency(n.rule, "constraint_setting", &s_ct);
    let Some(setting_info) = s_ct.providers.get("ConstraintSettingInfo") else {
        return Err(format!(
            "{}: attribute 'constraint_setting': '{}' does not have mandatory providers: 'ConstraintSettingInfo'",
            n.rule.label,
            setting.canonical_form()
        ));
    };
    let refines = match setting_info {
        Value::ProviderInstance { fields, .. } => fields
            .get("refines_constraint_values")
            .cloned()
            .unwrap_or_else(|| mk_list(vec![])),
        _ => mk_list(vec![]),
    };
    let mut ct = base(n);
    ct.providers.insert(
        "ConstraintValueInfo".into(),
        provider_instance(
            "ConstraintValueInfo",
            vec![
                ("constraint", setting_info.clone()),
                ("label", lbl(&n.rule.label)),
                ("refines", refines),
            ],
        ),
    );
    Ok(ct)
}

pub(crate) fn platform(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let mut own_entries: Vec<(Value, Value)> = Vec::new();
    let mut parent_info_val: Option<Value> = None;
    let mut parent_constraints_val: Option<Value> = None;
    let mut exec_properties: BTreeMap<String, String> = BTreeMap::new();
    let mut parent_remote_props: Option<String> = None;
    let mut flags: Vec<String> = Vec::new();
    let core_cfg =
        zel_analysis::BuildConfigurationKey::target(&n.cfg.cpu, "fastbuild").to_core_only();
    let parents = n.labels("parents")?;
    if parents.len() > 1 {
        return Err(format!(
            "{}: attribute 'parents' in 'platform' rule {} may have at most one parent",
            n.rule.label, n.rule.label
        ));
    }
    for parent in parents {
        let p = n.resolver.configured_target(&parent, &core_cfg)?;
        n.resolver.check_dependency(n.rule, "parents", &p)?;
        let Some(p_info @ Value::ProviderInstance { fields, .. }) = p.providers.get("PlatformInfo")
        else {
            return Err(format!(
                "{}: attribute 'parents': '{}' does not have mandatory providers: 'PlatformInfo'",
                n.rule.label,
                parent.canonical_form()
            ));
        };
        parent_info_val = Some(p_info.clone());
        parent_constraints_val = fields.get("constraints").cloned();
        if let Some(Value::Dict(d)) = fields.get("exec_properties") {
            for (k, v) in d.lock().unwrap().items() {
                if let (Value::String(k), Value::String(v)) = (k, v) {
                    exec_properties.insert(k, v);
                }
            }
        }
        if let Some(Value::String(rp)) = fields.get("remote_execution_properties") {
            if !rp.is_empty() {
                parent_remote_props = Some(rp.clone());
            }
        }
        if let Some(Value::List(fl)) = fields.get("flags") {
            for item in &fl.lock().unwrap().elements {
                if let Some(s) = item.as_str() {
                    flags.push(s.to_string());
                }
            }
        }
    }
    let mut seen_settings: BTreeMap<String, Label> = BTreeMap::new();
    for raw_cv in n.labels("constraint_values")? {
        let ct = n.resolver.configured_target(&raw_cv, &core_cfg)?;
        n.resolver
            .check_dependency(n.rule, "constraint_values", &ct)?;
        let Some(cv_info @ Value::ProviderInstance { fields, .. }) =
            ct.providers.get("ConstraintValueInfo")
        else {
            return Err(format!("{}: attribute 'constraint_values': '{}' does not have mandatory providers: 'ConstraintValueInfo'", n.rule.label, raw_cv.canonical_form()));
        };
        let Some(setting_info @ Value::ProviderInstance { fields: sf, .. }) =
            fields.get("constraint")
        else {
            continue;
        };
        let Some(Value::Label(setting)) = sf.get("label") else {
            continue;
        };
        let setting = setting.clone();
        // An alias to a constraint_value stands for the aliased value.
        let cv = match fields.get("label") {
            Some(Value::Label(l)) => l.clone(),
            _ => raw_cv,
        };
        if let Some(prev) = seen_settings.get(&setting.canonical_form()) {
            if prev != &cv {
                return Err(format!(
                    "{}: Duplicate constraint values detected: constraint_setting {} has [{}, {}]",
                    n.rule.label,
                    setting.canonical_form(),
                    prev.canonical_form(),
                    cv.canonical_form()
                ));
            }
            continue;
        }
        seen_settings.insert(setting.canonical_form(), cv);
        own_entries.push((setting_info.clone(), cv_info.clone()));
    }
    if let Some(AttrValue::StrList(own_flags)) = n.value("flags")? {
        flags.extend(own_flags);
    }
    let own_remote_props = n.string("remote_execution_properties")?;
    let has_own_exec_props =
        matches!(n.value("exec_properties")?, Some(AttrValue::StrDict(ref d)) if !d.is_empty());
    if !own_remote_props.is_empty() && (has_own_exec_props || !exec_properties.is_empty()) {
        return Err(format!(
            "{}: Platform specifies both remote_execution_properties and exec_properties",
            n.rule.label
        ));
    }
    if has_own_exec_props && parent_remote_props.is_some() {
        return Err(format!(
            "{}: Platform specifies exec_properties while parent specifies remote_execution_properties",
            n.rule.label
        ));
    }
    let remote_props = if own_remote_props.is_empty() {
        parent_remote_props.unwrap_or_default()
    } else {
        own_remote_props.replace(
            "{PARENT_REMOTE_EXECUTION_PROPERTIES}",
            parent_remote_props.as_deref().unwrap_or(""),
        )
    };
    if let Some(AttrValue::StrDict(d)) = n.value("exec_properties")? {
        for (k, v) in d {
            if v.is_empty() {
                exec_properties.remove(&k);
            } else {
                exec_properties.insert(k, v);
            }
        }
    }
    let constraints_val =
        zel_analysis::register_constraint_collection(own_entries, parent_constraints_val.as_ref());
    let mut ct = base(n);
    ct.providers.insert(
        "PlatformInfo".into(),
        provider_instance(
            "PlatformInfo",
            vec![
                ("label", lbl(&n.rule.label)),
                ("parent", parent_info_val.unwrap_or(Value::None)),
                ("constraints", constraints_val),
                (
                    "flags",
                    mk_list(flags.into_iter().map(Value::String).collect()),
                ),
                (
                    "exec_properties",
                    mk_dict(
                        exec_properties
                            .into_iter()
                            .map(|(k, v)| (Value::String(k), Value::String(v)))
                            .collect(),
                    ),
                ),
                ("remote_execution_properties", Value::String(remote_props)),
            ],
        ),
    );
    Ok(ct)
}

pub(crate) fn config_feature_flag(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let allowed = n.strings("allowed_values")?;
    if allowed.is_empty() {
        return Err(format!(
            "in allowed_values attribute of config_feature_flag rule {}: attribute must be non empty",
            n.rule.label.canonical_form()
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for v in &allowed {
        if !seen.insert(v.clone()) {
            return Err(format!(
                "in allowed_values attribute of config_feature_flag rule {}: cannot contain duplicates, but contained multiple of \"{v}\"",
                n.rule.label.canonical_form()
            ));
        }
    }
    let has_default = n.explicit("default_value");
    let default_value = n.string("default_value")?;
    if has_default && !allowed.contains(&default_value) {
        return Err(format!(
            "in default_value attribute of config_feature_flag rule {}: cannot be default value of feature flag, which only allows {:?}, got \"{default_value}\"",
            n.rule.label.canonical_form(),
            allowed
        ));
    }
    let key = n.rule.label.canonical_form();
    let value = match n.cfg.flag_values.get(&key) {
        Some(configured) => {
            if !allowed.contains(configured) {
                return Err(format!(
                    "in config_feature_flag rule {}: value must be one of {:?}, but was \"{configured}\"",
                    key, allowed
                ));
            }
            configured.clone()
        }
        None if has_default => default_value,
        None => {
            return Err(format!(
                "in config_feature_flag rule {key}: feature flag {key} was accessed in a configuration without a value present"
            ));
        }
    };
    let mut ct = base(n);
    ct.providers.insert(
        "FeatureFlagInfo".into(),
        provider_instance("FeatureFlagInfo", vec![("value", Value::String(value))]),
    );
    Ok(ct)
}

pub(crate) fn toolchain_type(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let mut ct = base(n);
    ct.providers.insert(
        "ToolchainTypeInfo".into(),
        provider_instance(
            "ToolchainTypeInfo",
            vec![("type_label", lbl(&n.rule.label))],
        ),
    );
    Ok(ct)
}

pub(crate) fn toolchain(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let ty = match n.value("toolchain_type")? {
        Some(AttrValue::Label(Some(l))) => l,
        _ => {
            return Err(format!(
                "{}: missing value for mandatory attribute 'toolchain_type' in 'toolchain' rule",
                n.rule.label
            ))
        }
    };
    let target = match n.value("toolchain")? {
        Some(AttrValue::Label(Some(l))) => l,
        // `toolchain` is a NODEP_LABEL: stored as the unresolved label string.
        Some(AttrValue::Str(s)) => {
            if let Ok(l) = Label::parse_canonical(&s) {
                l
            } else if s.starts_with("//") {
                let pkg_part = s.trim_start_matches('/').split(':').next().unwrap_or("");
                let name_part = s.split(':').nth(1).unwrap_or(pkg_part);
                Label::create(n.rule.label.package_identifier().clone(), name_part)
                    .unwrap_or_else(|_| n.rule.label.clone())
            } else {
                Label::create(
                    n.rule.label.package_identifier().clone(),
                    s.trim_start_matches(':'),
                )
                .map_err(|e| e.to_string())?
            }
        }
        _ => {
            return Err(format!(
                "{}: missing value for mandatory attribute 'toolchain' in 'toolchain' rule",
                n.rule.label
            ))
        }
    };
    let use_target_exec = n.boolean("use_target_platform_constraints")?;
    let exec_labels = n.labels("exec_compatible_with")?;
    let target_labels = n.labels("target_compatible_with")?;
    if use_target_exec && (!exec_labels.is_empty() || !target_labels.is_empty()) {
        return Err(format!(
            "{}: It is not allowed to specify exec_compatible_with or target_compatible_with when use_target_platform_constraints is True",
            n.rule.label
        ));
    }
    let core_cfg =
        zel_analysis::BuildConfigurationKey::target(&n.cfg.cpu, "fastbuild").to_core_only();
    let tt_ct = n.resolver.configured_target(&ty, &core_cfg)?;
    n.resolver
        .check_dependency(n.rule, "toolchain_type", &tt_ct)?;
    let Some(
        tt_info @ Value::ProviderInstance {
            fields: tt_fields, ..
        },
    ) = tt_ct.providers.get("ToolchainTypeInfo")
    else {
        return Err(format!(
            "{}: attribute 'toolchain_type': '{}' does not have mandatory providers: 'ToolchainTypeInfo'",
            n.rule.label,
            ty.canonical_form()
        ));
    };
    let resolved_ty = match tt_fields.get("type_label") {
        Some(Value::Label(l)) => l.clone(),
        _ => ty.clone(),
    };
    let check_constraints = |attr_key: &str,
                             desc: &str,
                             labels: &[Label]|
     -> Result<Vec<Value>, String> {
        let mut seen: BTreeMap<String, Label> = BTreeMap::new();
        let mut resolved_out = Vec::with_capacity(labels.len());
        for raw in labels {
            let ct = n.resolver.configured_target(raw, &core_cfg)?;
            n.resolver.check_dependency(n.rule, attr_key, &ct)?;
            let Some(Value::ProviderInstance { fields, .. }) =
                ct.providers.get("ConstraintValueInfo")
            else {
                return Err(format!(
                    "{}: attribute '{attr_key}': '{}' does not have mandatory providers: 'ConstraintValueInfo'",
                    n.rule.label,
                    raw.canonical_form()
                ));
            };
            let setting = match fields.get("constraint") {
                Some(Value::ProviderInstance { fields, .. }) => match fields.get("label") {
                    Some(Value::Label(l)) => l.canonical_form(),
                    _ => continue,
                },
                _ => continue,
            };
            let cv = match fields.get("label") {
                Some(Value::Label(l)) => l.clone(),
                _ => raw.clone(),
            };
            if let Some(prev) = seen.get(&setting) {
                if prev != &cv {
                    return Err(format!(
                        "{}: Duplicate constraints detected in {}: constraint_setting {} has [{}, {}]",
                        n.rule.label,
                        desc,
                        setting,
                        prev.canonical_form(),
                        cv.canonical_form()
                    ));
                }
            } else {
                seen.insert(setting, cv.clone());
                resolved_out.push(lbl(&cv));
            }
        }
        Ok(resolved_out)
    };
    let exec_cv_list = check_constraints(
        "exec_compatible_with",
        "execution constraints",
        &exec_labels,
    )?;
    let target_cv_list = check_constraints(
        "target_compatible_with",
        "target constraints",
        &target_labels,
    )?;
    let target_setting_labels = n.labels("target_settings")?;
    for ts in &target_setting_labels {
        let ts_ct = n.resolver.configured_target(ts, n.cfg)?;
        n.resolver
            .check_dependency(n.rule, "target_settings", &ts_ct)?;
        if !ts_ct.providers.contains_key("ConfigMatchingProvider") {
            return Err(format!(
                "{}: attribute 'target_settings': '{}' does not have mandatory providers: 'ConfigMatchingProvider'",
                n.rule.label,
                ts.canonical_form()
            ));
        }
    }
    let mut ct = base(n);
    ct.providers.insert(
        "DeclaredToolchainInfo".into(),
        provider_instance(
            "DeclaredToolchainInfo",
            vec![
                ("toolchain_type", lbl(&resolved_ty)),
                ("toolchain_type_info", tt_info.clone()),
                ("exec_compatible_with", mk_list(exec_cv_list)),
                ("target_compatible_with", mk_list(target_cv_list)),
                (
                    "target_settings",
                    mk_list(target_setting_labels.iter().map(lbl).collect()),
                ),
                (
                    "use_target_platform_constraints",
                    Value::Bool(use_target_exec),
                ),
                ("toolchain", lbl(&target)),
                ("label", lbl(&n.rule.label)),
            ],
        ),
    );
    Ok(ct)
}
