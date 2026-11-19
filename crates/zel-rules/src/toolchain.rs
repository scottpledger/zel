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
    let mut ct = ConfiguredTarget::new(
        label.clone(),
        BuildConfigurationKey::target("host", "fastbuild"),
        "platform".into(),
    );
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
                        (
                            Value::Label(lab(&format!("@{r}//os:os"))),
                            Value::Label(lab(&format!("@{r}//os:{}", host_os_name()))),
                        ),
                        (
                            Value::Label(lab(&format!("@{r}//cpu:cpu"))),
                            Value::Label(lab(&format!("@{r}//cpu:{}", host_cpu_name()))),
                        ),
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
    let Some(Value::ProviderInstance { fields, .. }) = cv.providers.get("ConstraintValueInfo")
    else {
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
            .filter_map(|x| {
                if let Value::Label(l) = x {
                    Some(l)
                } else {
                    None
                }
            })
            .collect(),
        None => vec![],
    }
}

/// Resolves a `toolchain_type` label through static `alias` chains to its canonical
/// `ToolchainTypeInfo.type_label`, memoized on `acfg`.
fn resolve_toolchain_type_label(
    raw_ty: &Label,
    core_cfg: &BuildConfigurationKey,
    resolver: &Arc<dyn DepResolver>,
    acfg: &crate::AnalysisConfig,
) -> Label {
    if let Some(cached) = acfg.cached_toolchain_type(raw_ty) {
        return cached;
    }
    let mut cur = raw_ty.clone();
    let mut visited = std::collections::HashSet::new();
    let mut static_resolved = None;
    for _ in 0..16 {
        if !visited.insert(cur.clone()) {
            break;
        }
        match resolver.rule_instance(&cur) {
            Ok(Some(r)) => match r.rule_class.as_str() {
                "toolchain_type" => {
                    static_resolved = Some(cur);
                    break;
                }
                "alias" => match r.attr_or_default("actual") {
                    Some(zel_packages::AttrValue::Label(Some(next))) => {
                        cur = next;
                        continue;
                    }
                    _ => break,
                },
                _ => break,
            },
            _ => break,
        }
    }
    if let Some(resolved) = static_resolved {
        acfg.cache_toolchain_type(raw_ty.clone(), resolved.clone());
        return resolved;
    }
    resolver
        .configured_target(raw_ty, core_cfg)
        .ok()
        .and_then(|tt| match tt.providers.get("ToolchainTypeInfo") {
            Some(Value::ProviderInstance { fields: tf, .. }) => match tf.get("type_label") {
                Some(Value::Label(rl)) => Some(rl.clone()),
                _ => None,
            },
            _ => None,
        })
        .unwrap_or_else(|| raw_ty.clone())
}

fn raw_toolchain_type_of(label: &Label, resolver: &Arc<dyn DepResolver>) -> Option<Label> {
    let r = resolver.rule_instance(label).ok()??;
    if r.rule_class != "toolchain" {
        return None;
    }
    match r.attr_or_default("toolchain_type") {
        Some(zel_packages::AttrValue::Label(Some(tl))) => Some(tl),
        _ => None,
    }
}

fn satisfies(p: &ConfiguredTarget, cvs: &[Arc<ConfiguredTarget>]) -> bool {
    cvs.iter().all(|cv| platform_has_value(p, cv))
}

#[allow(clippy::too_many_arguments)]
fn check_toolchain_candidate(
    tc: &crate::RegisteredToolchain,
    wanted_ty: &Label,
    ep: &ConfiguredTarget,
    target: &ConfiguredTarget,
    cfg: &BuildConfigurationKey,
    core_cfg: &BuildConfigurationKey,
    resolver: &Arc<dyn DepResolver>,
    acfg: &crate::AnalysisConfig,
) -> Result<Option<Label>, String> {
    if let Some(raw_ty) = &tc.raw_toolchain_type {
        let resolved_ty = resolve_toolchain_type_label(raw_ty, core_cfg, resolver, acfg);
        if resolved_ty != *wanted_ty {
            return Ok(None);
        }
    }
    let ct = resolver.configured_target(&tc.label, cfg)?;
    let Some(Value::ProviderInstance { fields: f, .. }) = ct.providers.get("DeclaredToolchainInfo")
    else {
        return Ok(None);
    };
    let raw_ty = match f.get("toolchain_type") {
        Some(Value::Label(tl)) => tl,
        _ => return Ok(None),
    };
    let resolved_ty = resolve_toolchain_type_label(raw_ty, core_cfg, resolver, acfg);
    if resolved_ty != *wanted_ty {
        return Ok(None);
    }
    let use_target_exec = matches!(
        f.get("use_target_platform_constraints"),
        Some(Value::Bool(true))
    );
    let exec_ok = if use_target_exec {
        let target_c = platform_constraints(target);
        let ep_c = platform_constraints(ep);
        target_c.iter().all(|(k, v)| ep_c.get(k) == Some(v))
    } else {
        let exec_cv: Vec<Arc<ConfiguredTarget>> = list_labels(f.get("exec_compatible_with"))
            .iter()
            .map(|l| resolver.configured_target(l, cfg))
            .collect::<Result<_, _>>()?;
        satisfies(ep, &exec_cv)
    };
    if !exec_ok {
        return Ok(None);
    }
    let target_cv: Vec<Arc<ConfiguredTarget>> = list_labels(f.get("target_compatible_with"))
        .iter()
        .map(|l| resolver.configured_target(l, cfg))
        .collect::<Result<_, _>>()?;
    if !satisfies(target, &target_cv) {
        return Ok(None);
    }
    for s in list_labels(f.get("target_settings")) {
        let st = resolver.configured_target(&s, cfg)?;
        let matched = st
            .providers
            .get("ConfigMatchingProvider")
            .and_then(zel_analysis::select::matching_info)
            .map(|(m, _)| m)
            .unwrap_or(false);
        if !matched {
            return Ok(None);
        }
    }
    Ok(match f.get("toolchain") {
        Some(Value::Label(imp)) => Some(imp.clone()),
        _ => None,
    })
}

/// Resolves toolchains for a target using incremental, memoized pattern expansion and
/// `toolchain_type` pre-filtering so unrelated registered toolchains (and their external repos)
/// are not analyzed or fetched when earlier registrations already satisfy the requested types.
pub(crate) fn resolve_toolchains_lazy(
    owner: &Label,
    types: &[ToolchainTypeRequest],
    exec_compatible_with: &[Label],
    cfg: &BuildConfigurationKey,
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

    let ecw: Vec<Arc<ConfiguredTarget>> = exec_compatible_with
        .iter()
        .map(|l| resolver.configured_target(l, cfg))
        .collect::<Result<_, _>>()?;

    if types.is_empty() {
        for ep in &exec_platforms {
            if satisfies(ep, &ecw) {
                return Ok(ResolvedToolchains {
                    execution_platform: ep.clone(),
                    toolchains: BTreeMap::new(),
                });
            }
        }
        return Err(format!(
            "While resolving toolchains for target {}: No execution platforms are compatible with the target's exec_compatible_with constraints",
            owner.canonical_form()
        ));
    }

    // Validate CLI `--extra_toolchains` and main-repository `register_toolchains` patterns
    // upfront so malformed registrations fail immediately regardless of match order.
    for (idx, pat) in acfg.toolchain_patterns.iter().enumerate() {
        if acfg.is_eager_toolchain_pattern(idx, pat) {
            acfg.expand_toolchain_pattern(pat)?;
        }
    }

    let core_cfg = cfg.to_core_only();
    let explicit_candidates: Vec<crate::RegisteredToolchain> = acfg
        .registered_toolchains
        .iter()
        .map(|l| crate::RegisteredToolchain {
            label: l.clone(),
            raw_toolchain_type: raw_toolchain_type_of(l, resolver),
        })
        .collect();

    let mut missing_for_all: Vec<Label> = Vec::new();
    for ep in &exec_platforms {
        if !satisfies(ep, &ecw) {
            continue;
        }
        let mut chosen: BTreeMap<String, Option<Label>> = BTreeMap::new();
        let mut missing: Vec<Label> = Vec::new();
        for t in types {
            let mut found: Option<Label> = None;
            let mut seen = std::collections::HashSet::new();
            for tc in &explicit_candidates {
                if !seen.insert(tc.label.clone()) {
                    continue;
                }
                if let Some(imp) = check_toolchain_candidate(
                    tc,
                    &t.toolchain_type,
                    ep,
                    &target,
                    cfg,
                    &core_cfg,
                    resolver,
                    acfg,
                )? {
                    found = Some(imp);
                    break;
                }
            }
            if found.is_none() {
                for pat in &acfg.toolchain_patterns {
                    let expanded = acfg.expand_toolchain_pattern(pat)?;
                    for tc in &expanded {
                        if !seen.insert(tc.label.clone()) {
                            continue;
                        }
                        if let Some(imp) = check_toolchain_candidate(
                            tc,
                            &t.toolchain_type,
                            ep,
                            &target,
                            cfg,
                            &core_cfg,
                            resolver,
                            acfg,
                        )? {
                            found = Some(imp);
                            break;
                        }
                    }
                    if found.is_some() {
                        break;
                    }
                }
            }
            if found.is_none() && t.mandatory {
                missing.push(t.toolchain_type.clone());
            }
            chosen.insert(t.toolchain_type.canonical_form(), found);
        }
        if missing.is_empty() {
            return Ok(ResolvedToolchains {
                execution_platform: ep.clone(),
                toolchains: chosen,
            });
        }
        if missing_for_all.is_empty() {
            missing_for_all = missing;
        }
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

/// Resolves toolchains for a target: picks the first execution platform (extra execution
/// platforms in order, then the host platform) that satisfies `exec_compatible_with` and has a
/// matching registered toolchain for every mandatory type.
#[allow(clippy::too_many_arguments)]
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

    let core_cfg = cfg.to_core_only();
    let candidates: Vec<crate::RegisteredToolchain> = registered_toolchains
        .iter()
        .map(|l| crate::RegisteredToolchain {
            label: l.clone(),
            raw_toolchain_type: raw_toolchain_type_of(l, resolver),
        })
        .collect();
    let ecw: Vec<Arc<ConfiguredTarget>> = exec_compatible_with
        .iter()
        .map(|l| resolver.configured_target(l, cfg))
        .collect::<Result<_, _>>()?;

    let mut missing_for_all: Vec<Label> = Vec::new();
    'platform: for ep in &exec_platforms {
        if !satisfies(ep, &ecw) {
            continue;
        }
        let mut chosen: BTreeMap<String, Option<Label>> = BTreeMap::new();
        let mut missing: Vec<Label> = Vec::new();
        for t in types {
            let mut found: Option<Label> = None;
            for tc in &candidates {
                if let Some(imp) = check_toolchain_candidate(
                    tc,
                    &t.toolchain_type,
                    ep,
                    &target,
                    cfg,
                    &core_cfg,
                    resolver,
                    acfg,
                )? {
                    found = Some(imp);
                    break;
                }
            }
            if found.is_none() && t.mandatory {
                missing.push(t.toolchain_type.clone());
            }
            chosen.insert(t.toolchain_type.canonical_form(), found);
        }
        if missing.is_empty() {
            return Ok(ResolvedToolchains {
                execution_platform: ep.clone(),
                toolchains: chosen,
            });
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
