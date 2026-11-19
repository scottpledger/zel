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

/// Value of option `name` in `cfg`; `None` if the option is unknown/unset.
fn option_values(cfg: &zel_analysis::BuildConfigurationKey, name: &str) -> Option<Vec<String>> {
    let canonical = if name == "c" {
        "compilation_mode"
    } else {
        name
    };
    match canonical {
        "compilation_mode" => Some(vec![cfg.compilation_mode.clone()]),
        "cpu" => Some(vec![cfg.cpu.clone()]),
        "platforms" => Some(cfg.platforms.clone()),
        other => match cfg.options.get(other) {
            Some(v) => Some(v.split(',').map(str::to_string).collect()),
            // An option that was not set has its declared default value
            // (ConfigSetting compares against the fully-parsed BuildOptions.)
            None => option_default(other).or_else(|| {
                zel_analysis::build_options::find_option(other).and_then(|(_, opt)| {
                    if opt.default == "null" || opt.default.is_empty() {
                        None
                    } else {
                        Some(vec![opt.default.to_string()])
                    }
                })
            }),
        },
    }
}

fn option_default(name: &str) -> Option<Vec<String>> {
    static MAP: std::sync::OnceLock<
        std::collections::HashMap<&'static str, &'static zel_options::OptionDefinition>,
    > = std::sync::OnceLock::new();
    let d = MAP
        .get_or_init(zel_options::flag_tables::get_command_options_map)
        .get(name)?;
    if d.allow_multiple {
        return None;
    }
    // The tables record an empty-string default of a String option as "no default".
    match d.default_value {
        Some(v) => Some(vec![v.to_string()]),
        None if matches!(d.option_type, zel_options::types::OptionType::String) => {
            Some(vec![String::new()])
        }
        None => None,
    }
}

pub(crate) fn config_setting(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let mut settings: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut matches = true;

    if let Some(AttrValue::StrDict(values)) = n.value("values")? {
        for (k, v) in values {
            let key = k.trim_start_matches("--").to_string();
            settings.push(format!("values:{key}={v}"));
            if key == "define" {
                let (dk, dv) = v.split_once('=').unwrap_or((v.as_str(), ""));
                if n.cfg.user_defines.get(dk).map(|x| x.as_str()) != Some(dv) {
                    matches = false;
                }
                continue;
            }
            match option_values(n.cfg, &key) {
                Some(vals) => {
                    let norm_v = if vals.len() == 1 && matches!(vals[0].as_str(), "true" | "false")
                    {
                        parse_bazel_bool(&v)
                            .map(|b| b.to_string())
                            .unwrap_or_else(|| v.clone())
                    } else {
                        v.clone()
                    };
                    if !vals.contains(&norm_v) {
                        matches = false;
                    }
                }
                None => {
                    let known = KNOWN_OPTIONS.contains(&key.as_str())
                        || zel_analysis::build_options::find_option(if key == "c" {
                            "compilation_mode"
                        } else {
                            key.as_str()
                        })
                        .is_some();
                    if !known {
                        errors.push(format!("unknown option: '{key}'"));
                    }
                    matches = false;
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
            let ct = n.resolver.configured_target(&l, n.cfg)?;
            let resolved_label = ct.alias_targets.last().unwrap_or(&l);
            // Build settings, and rules providing `config_common.FeatureFlagInfo` (the value of
            // a feature flag), can be matched by `flag_values`.
            let actual = match ct
                .providers
                .get("BuildSettingProvider")
                .or_else(|| ct.providers.get("FeatureFlagInfo"))
            {
                Some(Value::ProviderInstance { fields, .. }) => fields.get("value").cloned(),
                _ => None,
            };
            let Some(actual) = actual else {
                errors.push(format!(
                    "{} does not have mandatory providers: 'BuildSettingProvider'",
                    l.canonical_form()
                ));
                matches = false;
                continue;
            };
            let norm_expected = match &actual {
                Value::Bool(_) => match parse_bazel_bool(&expected) {
                    Some(b) => b.to_string(),
                    None => {
                        errors.push(format!(
                            "Error while parsing user-defined configuration value: '{}' cannot be converted to {} type bool",
                            expected,
                            l.canonical_form()
                        ));
                        matches = false;
                        continue;
                    }
                },
                Value::Label(_) => {
                    let pkg = n.rule.label.package_identifier();
                    let entries = n
                        .resolver
                        .repo_mapping_entries(pkg.repository().name())
                        .into_iter()
                        .map(|(a, c)| (a, zel_cmdline::RepositoryName::create_unvalidated(c)))
                        .collect();
                    let mapping =
                        zel_cmdline::RepositoryMapping::new(entries, pkg.repository().clone());
                    Label::parse_with_package_context(&expected, pkg, &mapping)
                        .map(|x| x.canonical_form())
                        .unwrap_or_else(|_| expected.clone())
                }
                _ => expected.clone(),
            };
            settings.push(format!(
                "flag:{}={norm_expected}",
                resolved_label.canonical_form()
            ));
            if !flag_value_matches(&actual, &norm_expected) {
                matches = false;
            }
        }
    }
    let constraint_values = n.labels("constraint_values")?;
    if !constraint_values.is_empty() {
        let platform = crate::toolchain::target_platform(n.cfg, n.resolver, n.acfg)?;
        for cv in &constraint_values {
            let cv_ct = n.resolver.configured_target(cv, n.cfg)?;
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
        let mut ct = base(n);
        ct.providers.insert(
            "ConfigMatchingProvider".into(),
            provider_instance(
                "ConfigMatchingProvider",
                vec![
                    ("matches", Value::Bool(false)),
                    ("settings", mk_list(vec![])),
                    (
                        "errors",
                        mk_list(errors.into_iter().map(Value::String).collect()),
                    ),
                    ("label", lbl(&n.rule.label)),
                ],
            ),
        );
        return Ok(ct);
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

fn parse_bazel_bool(raw: &str) -> Option<bool> {
    match raw.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "t" | "y" => Some(true),
        "false" | "0" | "no" | "f" | "n" => Some(false),
        _ => None,
    }
}

fn flag_value_matches(actual: &Value, expected: &str) -> bool {
    match actual {
        Value::Bool(b) => parse_bazel_bool(expected) == Some(*b),
        Value::String(s) => s == expected,
        Value::Int(i) => i.to_string() == expected,
        Value::Label(l) => {
            l.canonical_form() == expected
                || format!("//{}:{}", l.package_name(), l.name()) == expected
        }
        Value::List(_) | Value::Tuple(_) => zel_starlark::eval::builtins::value_to_sequence(actual)
            .map(|v| {
                v.iter().any(|x| x.as_str() == Some(expected))
                    || v.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                        == expected
            })
            .unwrap_or(false),
        _ => false,
    }
}

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
    // setting label -> value label
    let mut constraints: BTreeMap<String, (Label, Label)> = BTreeMap::new();
    let mut exec_properties: BTreeMap<String, String> = BTreeMap::new();
    let mut parent_remote_props: Option<String> = None;
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
        let Some(Value::ProviderInstance { fields, .. }) = p.providers.get("PlatformInfo") else {
            return Err(format!(
                "{}: attribute 'parents': '{}' does not have mandatory providers: 'PlatformInfo'",
                n.rule.label,
                parent.canonical_form()
            ));
        };
        if let Some(Value::Dict(d)) = fields.get("constraints") {
            for (k, v) in d.lock().unwrap().items() {
                if let (Value::Label(k), Value::Label(v)) = (k, v) {
                    constraints.insert(k.canonical_form(), (k, v));
                }
            }
        }
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
    }
    let mut seen_settings: BTreeMap<String, Label> = BTreeMap::new();
    for raw_cv in n.labels("constraint_values")? {
        let ct = n.resolver.configured_target(&raw_cv, &core_cfg)?;
        n.resolver
            .check_dependency(n.rule, "constraint_values", &ct)?;
        let Some(Value::ProviderInstance { fields, .. }) = ct.providers.get("ConstraintValueInfo")
        else {
            return Err(format!("{}: attribute 'constraint_values': '{}' does not have mandatory providers: 'ConstraintValueInfo'", n.rule.label, raw_cv.canonical_form()));
        };
        let setting = match fields.get("constraint") {
            Some(Value::ProviderInstance { fields, .. }) => match fields.get("label") {
                Some(Value::Label(l)) => l.clone(),
                _ => continue,
            },
            _ => continue,
        };
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
        } else {
            seen_settings.insert(setting.canonical_form(), cv.clone());
        }
        constraints.insert(setting.canonical_form(), (setting, cv));
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
    let mut ct = base(n);
    ct.providers.insert(
        "PlatformInfo".into(),
        provider_instance(
            "PlatformInfo",
            vec![
                ("label", lbl(&n.rule.label)),
                (
                    "constraints",
                    mk_dict(
                        constraints
                            .values()
                            .map(|(s, v)| (lbl(s), lbl(v)))
                            .collect(),
                    ),
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
    let ll = |name: &str| -> Result<Value, String> {
        Ok(mk_list(n.labels(name)?.iter().map(lbl).collect()))
    };
    let use_target_exec = n.boolean("use_target_platform_constraints")?;
    let core_cfg =
        zel_analysis::BuildConfigurationKey::target(&n.cfg.cpu, "fastbuild").to_core_only();
    let check_dups = |attr_name: &str, labels: &[Label]| -> Result<(), String> {
        let mut seen: BTreeMap<String, Label> = BTreeMap::new();
        for raw in labels {
            let ct = n.resolver.configured_target(raw, &core_cfg)?;
            if let Some(Value::ProviderInstance { fields, .. }) =
                ct.providers.get("ConstraintValueInfo")
            {
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
                            attr_name,
                            setting,
                            prev.canonical_form(),
                            cv.canonical_form()
                        ));
                    }
                } else {
                    seen.insert(setting, cv);
                }
            }
        }
        Ok(())
    };
    let exec_labels = n.labels("exec_compatible_with")?;
    let target_labels = n.labels("target_compatible_with")?;
    check_dups("execution constraints", &exec_labels)?;
    check_dups("target constraints", &target_labels)?;
    let mut ct = base(n);
    ct.providers.insert(
        "DeclaredToolchainInfo".into(),
        provider_instance(
            "DeclaredToolchainInfo",
            vec![
                ("toolchain_type", lbl(&ty)),
                ("exec_compatible_with", ll("exec_compatible_with")?),
                ("target_compatible_with", ll("target_compatible_with")?),
                ("target_settings", ll("target_settings")?),
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
