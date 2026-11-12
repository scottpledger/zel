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

//! Platforms and toolchain resolution (`ToolchainResolutionFunction`,
//! `SingleToolchainResolutionFunction`, `PlatformLookupUtil`).

use std::collections::BTreeMap;
use std::sync::Arc;
use zel_analysis::starlark_rule::{mk_dict, provider_instance, DepResolver};
use zel_analysis::{BuildConfigurationKey, ConfiguredTarget, DefaultInfo};
use zel_cmdline::Label;
use zel_starlark::Value;

/// Host operating-system constraint value label (`@platforms//os:*`).
fn host_os_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "osx"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

fn host_cpu_name() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    }
}

fn lab(s: &str) -> Label {
    Label::parse_canonical(s).expect("valid built-in label")
}

/// The auto-detected host platform (`@platforms//host`): the host OS and CPU constraints.
pub fn host_platform(platforms_repo: &str) -> Arc<ConfiguredTarget> {
    let r = platforms_repo.trim_start_matches('@');
    let label = lab(&format!("@{r}//host:host"));
    let mut ct = ConfiguredTarget::new(label.clone(), BuildConfigurationKey::target("host", "fastbuild"), "platform".into());
    ct.default_info = DefaultInfo::default();
    ct.providers.insert(
        "PlatformInfo".into(),
        provider_instance(
            "PlatformInfo",
            vec![
                ("label", Value::Label(label)),
                (
                    "constraints",
                    mk_dict(vec![
                        (Value::Label(lab(&format!("@{r}//os:os"))), Value::Label(lab(&format!("@{r}//os:{}", host_os_name())))),
                        (Value::Label(lab(&format!("@{r}//cpu:cpu"))), Value::Label(lab(&format!("@{r}//cpu:{}", host_cpu_name())))),
                    ]),
                ),
                ("exec_properties", mk_dict(vec![])),
            ],
        ),
    );
    Arc::new(ct)
}

/// The host platform: `--host_platform` when set, else the auto-detected one.
pub fn configured_host_platform(
    cfg: &BuildConfigurationKey,
    resolver: &Arc<dyn DepResolver>,
    acfg: &crate::AnalysisConfig,
) -> Result<Arc<ConfiguredTarget>, String> {
    let core_cfg = BuildConfigurationKey::target(&cfg.cpu, "fastbuild").to_core_only();
    if let Some(l) = &acfg.host_platform {
        let ct = resolver.configured_target(l, &core_cfg)?;
        if !ct.providers.contains_key("PlatformInfo") {
            return Err(format!(
                "Target {} was referenced as a platform, but does not provide PlatformInfo",
                l.canonical_form()
            ));
        }
        return Ok(ct);
    }
    let default_lbl = Label::parse_canonical("@bazel_tools//tools:host_platform").ok();
    if let Some(ref l) = default_lbl {
        if let Ok(ct) = resolver.configured_target(l, &core_cfg) {
            if ct.providers.contains_key("PlatformInfo") {
                return Ok(ct);
            }
        }
    }
    Ok(host_platform(&acfg.platforms_repo))
}

/// The target platform of `cfg`: the first `--platforms` entry, else the host platform.
pub fn target_platform(
    cfg: &BuildConfigurationKey,
    resolver: &Arc<dyn DepResolver>,
    acfg: &crate::AnalysisConfig,
) -> Result<Option<Arc<ConfiguredTarget>>, String> {
    let core_cfg = BuildConfigurationKey::target(&cfg.cpu, "fastbuild").to_core_only();
    match cfg.platforms.first() {
        Some(p) => {
            let l = Label::parse_canonical(p).map_err(|e| e.to_string())?;
            let ct = resolver.configured_target(&l, &core_cfg)?;
            if !ct.providers.contains_key("PlatformInfo") {
                return Err(format!(
                    "Target {} was referenced as a platform, but does not provide PlatformInfo",
                    l.canonical_form()
                ));
            }
            Ok(Some(ct))
        }
        None => Ok(Some(configured_host_platform(cfg, resolver, acfg)?)),
    }
}

/// Toolchain resolution (`SkyFunctions.TOOLCHAIN_RESOLUTION`) loads the auto-detected host platform
/// `@platforms//host:host` as an execution platform candidate; `PostAnalysisQueryEnvironment.targetifyValues`
/// flattens the resolution's direct deps into the rule's, so cquery shows it as a direct dep.
pub fn record_exec_platform_edge(
    rule: &zel_packages::RuleInstance,
    cfg: &BuildConfigurationKey,
    resolver: &Arc<dyn DepResolver>,
    acfg: &crate::AnalysisConfig,
) {
    let core_cfg = BuildConfigurationKey::target(&cfg.cpu, "fastbuild").to_core_only();
    let r = acfg.platforms_repo.trim_start_matches('@');
    if let Ok(l) = Label::parse_canonical(&format!("@{r}//host:host")) {
        if let Ok(ct) = resolver.configured_target(&l, &core_cfg) {
            let _ = resolver.check_dependency(rule, "$exec_platform", &ct);
        }
    }
    if acfg.host_platform.is_none() {
        if let Ok(l) = Label::parse_canonical("@bazel_tools//tools:host_platform") {
            if let Ok(ct) = resolver.configured_target(&l, &core_cfg) {
                let _ = resolver.check_dependency(rule, "$exec_platform", &ct);
            }
        }
    }
}

fn platform_constraints(p: &ConfiguredTarget) -> BTreeMap<String, Label> {
    let mut m = BTreeMap::new();
    if let Some(Value::ProviderInstance { fields, .. }) = p.providers.get("PlatformInfo") {
        if let Some(Value::Dict(d)) = fields.get("constraints") {
            for (k, v) in d.lock().unwrap().items() {
                if let (Value::Label(k), Value::Label(v)) = (k, v) {
                    m.insert(k.canonical_form(), v);
                }
            }
        }
    }
    m
}

/// Whether platform `p` has constraint value `cv` (`PlatformInfo.constraints.has`):
/// an explicit value for the setting, or the setting's default.
pub fn platform_has_value(p: &ConfiguredTarget, cv: &ConfiguredTarget) -> bool {
    let Some(Value::ProviderInstance { fields, .. }) = cv.providers.get("ConstraintValueInfo") else {
        return false;
    };
    // The value's own label (an alias to a constraint_value reports the aliased target's).
    let cv_label = match fields.get("label") {
        Some(Value::Label(l)) => l.clone(),
        _ => cv.label.clone(),
    };
    let (setting, default) = match fields.get("constraint") {
        Some(Value::ProviderInstance { fields, .. }) => (
            match fields.get("label") {
                Some(Value::Label(l)) => l.canonical_form(),
                _ => return false,
            },
            match fields.get("default_constraint_value") {
                Some(Value::Label(l)) => Some(l.clone()),
                _ => None,
            },
        ),
        _ => return false,
    };
    match platform_constraints(p).get(&setting) {
        Some(v) => *v == cv_label,
        None => default.as_ref() == Some(&cv_label),
    }
}

/// A requested toolchain type.
#[derive(Debug, Clone)]
pub struct ToolchainTypeRequest {
    pub toolchain_type: Label,
    pub mandatory: bool,
}

/// Outcome of toolchain resolution.
#[derive(Debug, Clone)]
pub struct ResolvedToolchains {
    pub execution_platform: Arc<ConfiguredTarget>,
    /// toolchain type → label of the selected `toolchain()`'s implementation target (`None` for
    /// optional types with no match).
    pub toolchains: BTreeMap<String, Option<Label>>,
}

fn list_labels(v: Option<&Value>) -> Vec<Label> {
    match v {
        Some(l) => zel_starlark::eval::builtins::value_to_sequence(l)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|x| if let Value::Label(l) = x { Some(l) } else { None })
            .collect(),
        None => vec![],
    }
}

/// Resolves toolchains for a target: picks the first execution platform (extra execution
/// platforms in order, then the host platform) that satisfies `exec_compatible_with` and has a
/// matching registered toolchain for every mandatory type.
pub fn resolve_toolchains(
    owner: &Label,
    types: &[ToolchainTypeRequest],
    exec_compatible_with: &[Label],
    cfg: &BuildConfigurationKey,
    registered_toolchains: &[Label],
    extra_exec_platforms: &[Label],
    resolver: &Arc<dyn DepResolver>,
    acfg: &crate::AnalysisConfig,
) -> Result<ResolvedToolchains, String> {
    let target = target_platform(cfg, resolver, acfg)?.expect("platform");
    let mut exec_platforms: Vec<Arc<ConfiguredTarget>> = Vec::new();
    for l in extra_exec_platforms.iter() {
        let ep = resolver.configured_target(l, cfg)?;
        if !ep.providers.contains_key("PlatformInfo") {
            return Err(format!(
                "Target {} was referenced as a platform, but does not provide PlatformInfo",
                l.canonical_form()
            ));
        }
        exec_platforms.push(ep);
    }
    exec_platforms.push(configured_host_platform(cfg, resolver, acfg)?);

    // Load registered toolchains once.
    let mut declared: Vec<(Arc<ConfiguredTarget>, BTreeMap<String, Value>)> = Vec::new();
    for l in registered_toolchains {
        let ct = resolver.configured_target(l, cfg)?;
        if let Some(Value::ProviderInstance { fields, .. }) = ct.providers.get("DeclaredToolchainInfo") {
            let f = fields.clone();
            declared.push((ct.clone(), f));
        }
    }
    let ecw: Vec<Arc<ConfiguredTarget>> = exec_compatible_with
        .iter()
        .map(|l| resolver.configured_target(l, cfg))
        .collect::<Result<_, _>>()?;

    let satisfies = |p: &ConfiguredTarget, cvs: &[Arc<ConfiguredTarget>]| cvs.iter().all(|cv| platform_has_value(p, cv));
    let mut missing_for_all: Vec<Label> = Vec::new();
    'platform: for ep in &exec_platforms {
        if !satisfies(ep, &ecw) {
            continue;
        }
        let mut chosen: BTreeMap<String, Option<Label>> = BTreeMap::new();
        let mut missing: Vec<Label> = Vec::new();
        for t in types {
            let mut found: Option<Label> = None;
            for (_, f) in &declared {
                let ty = match f.get("toolchain_type") {
                    Some(Value::Label(l)) => l.clone(),
                    _ => continue,
                };
                if ty != t.toolchain_type {
                    continue;
                }
                let exec_cv: Vec<Arc<ConfiguredTarget>> = list_labels(f.get("exec_compatible_with"))
                    .iter()
                    .map(|l| resolver.configured_target(l, cfg))
                    .collect::<Result<_, _>>()?;
                let target_cv: Vec<Arc<ConfiguredTarget>> = list_labels(f.get("target_compatible_with"))
                    .iter()
                    .map(|l| resolver.configured_target(l, cfg))
                    .collect::<Result<_, _>>()?;
                if !satisfies(ep, &exec_cv) || !satisfies(&target, &target_cv) {
                    continue;
                }
                let mut settings_ok = true;
                for s in list_labels(f.get("target_settings")) {
                    let st = resolver.configured_target(&s, cfg)?;
                    match st.providers.get("ConfigMatchingProvider") {
                        Some(p) => {
                            if !zel_analysis::select::matching_info(p).map(|(m, _)| m).unwrap_or(false) {
                                settings_ok = false;
                            }
                        }
                        None => settings_ok = false,
                    }
                }
                if !settings_ok {
                    continue;
                }
                if let Some(Value::Label(imp)) = f.get("toolchain") {
                    found = Some(imp.clone());
                    break;
                }
            }
            if found.is_none() && t.mandatory {
                missing.push(t.toolchain_type.clone());
            }
            chosen.insert(t.toolchain_type.canonical_form(), found);
        }
        if missing.is_empty() {
            return Ok(ResolvedToolchains { execution_platform: ep.clone(), toolchains: chosen });
        }
        if missing_for_all.is_empty() {
            missing_for_all = missing;
        }
        continue 'platform;
    }
    if missing_for_all.is_empty() {
        return Err(format!(
            "While resolving toolchains for target {}: No execution platforms are compatible with the target's exec_compatible_with constraints",
            owner.canonical_form()
        ));
    }
    let tp_label = match target.providers.get("PlatformInfo") {
        Some(Value::ProviderInstance { fields, .. }) => match fields.get("label") {
            Some(Value::Label(l)) => l.canonical_form(),
            _ => String::new(),
        },
        _ => String::new(),
    };
    Err(format!(
        "While resolving toolchains for target {}: No matching toolchains found for target platform {}:\n{}\nTo debug, rerun with --toolchain_resolution_debug='{}'\n",
        owner.canonical_form(),
        tp_label,
        missing_for_all.iter().map(|t| format!("  {}", t.canonical_form())).collect::<Vec<_>>().join("\n"),
        missing_for_all.iter().map(|t| t.canonical_form()).collect::<Vec<_>>().join("|")
    ))
}
