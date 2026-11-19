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

//! Target analysis: dispatches a loaded `Target` under a configuration to a Starlark rule
//! implementation (via the typed engine in `zel-analysis`) or to a native rule implementation.
//! Mirrors `src/main/java/com/google/devtools/build/lib/rules` (native rules) and
//! `StarlarkRuleConfiguredTargetUtil` (Starlark rules).
//!
//! Rule classes without a toolchain-backed implementation (`cc_*`, `java_*`, `py_*`,
//! `proto_library`, ...) are reported as analysis errors; no placeholder outputs are produced.

mod config;
pub mod failure;
mod legacy;
mod native;
pub mod resolver;
pub mod toolchain;

pub use resolver::{AnalysisConfig, PatternExpander, RegisteredToolchain, TargetResolver};

use std::collections::BTreeMap;
use std::sync::Arc;
use toolchain::{resolve_toolchains_lazy, ToolchainTypeRequest};
use zel_analysis::starlark_rule::{analyze_starlark_rule, AnalysisInputs, DepResolver};
use zel_analysis::{BuildConfigurationKey, ConfiguredTarget, DefaultInfo, OutputGroupInfo};
use zel_cmdline::Label;
use zel_packages::{RuleInstance, Target};
use zel_starlark::Value;

/// A resolver that fails every dependency request (used when analysing without a package
/// loader, e.g. leaf rules in unit tests).
struct NoDeps;

impl DepResolver for NoDeps {
    fn configured_target(
        &self,
        label: &Label,
        _cfg: &BuildConfigurationKey,
    ) -> Result<Arc<ConfiguredTarget>, String> {
        Err(format!(
            "cannot resolve dependency {}: no package loader available",
            label.canonical_form()
        ))
    }
}

/// `GenQuery.executeQuery`: evaluates `expression` (opts as in the rule's `opts` attribute) against the
/// package loader and returns the formatted output. Registered by `zel-server` (the query engine
/// lives in `zel-query`, which depends on this crate).
pub type GenQueryFn = dyn Fn(
        &Arc<zel_packages::PackageLoader>,
        &str,
        &[String],
        bool,
        &[String],
    ) -> Result<Vec<u8>, String>
    + Send
    + Sync;

static GENQUERY_HOOK: std::sync::OnceLock<Box<GenQueryFn>> = std::sync::OnceLock::new();

/// Installs the query engine used by the `genquery` rule (first registration wins).
pub fn set_genquery_hook(f: Box<GenQueryFn>) {
    let _ = GENQUERY_HOOK.set(f);
}

pub(crate) fn genquery_hook() -> Option<&'static GenQueryFn> {
    GENQUERY_HOOK.get().map(|b| b.as_ref())
}

/// Analyzes a loaded `Target` under `config` into a `ConfiguredTarget` with concrete actions.
pub fn analyze_target(
    target: &Target,
    config: &BuildConfigurationKey,
) -> Result<ConfiguredTarget, String> {
    analyze_target_with(target, config, Arc::new(NoDeps), &AnalysisConfig::new())
}

/// Like [`analyze_target`], resolving dependencies through `resolver`.
pub fn analyze_target_with(
    target: &Target,
    config: &BuildConfigurationKey,
    resolver: Arc<dyn DepResolver>,
    acfg: &AnalysisConfig,
) -> Result<ConfiguredTarget, String> {
    match target {
        Target::InputFile { label } => Ok(resolver::source_file_target(label, config)),
        Target::OutputFile {
            label,
            generating_rule,
        } => {
            let gen = resolver.configured_target(generating_rule, config)?;
            let mut rel = label
                .package_identifier()
                .package_fragment()
                .get_relative_str(label.name());
            // Generated files of external repositories live below `external/<repo>/`.
            if !label.package_identifier().repository().is_main() {
                rel = zel_vfs::PathFragment::create(&format!(
                    "external/{}/{}",
                    label.package_identifier().repository().name(),
                    rel.path_string()
                ));
            }
            let art = gen
                .typed_actions
                .iter()
                .flat_map(|a| a.outputs.iter())
                .find(|a| {
                    a.root_relative_path == rel
                        || a.exec_path
                            .path_string()
                            .ends_with(&format!("/{}", rel.path_string()))
                })
                .cloned()
                .or_else(|| {
                    gen.actions.iter().flat_map(|a| a.outputs()).find(|a| {
                        a.root_relative_path == rel
                            || a.exec_path
                                .path_string()
                                .ends_with(&format!("/{}", rel.path_string()))
                    })
                })
                .or_else(|| {
                    gen.default_info
                        .files
                        .iter()
                        .find(|a| {
                            a.root_relative_path == rel
                                || a.exec_path
                                    .path_string()
                                    .ends_with(&format!("/{}", rel.path_string()))
                        })
                        .cloned()
                })
                .ok_or_else(|| {
                    format!(
                        "output file '{}' was not generated by its rule {}",
                        label.canonical_form(),
                        generating_rule.canonical_form()
                    )
                })?;
            let mut ct =
                ConfiguredTarget::new(label.clone(), config.clone(), "generated file".to_string());
            ct.default_info = DefaultInfo {
                files: vec![art.clone()],
                executable: Some(art.clone()),
                ..Default::default()
            };
            ct.providers.insert(
                "DefaultInfo".into(),
                zel_analysis::starlark_rule::default_info_provider(
                    &[art.clone()],
                    Default::default(),
                    Default::default(),
                    Some(art.clone()),
                ),
            );
            let mut groups = BTreeMap::new();
            groups.insert("default".to_string(), vec![art]);
            ct.output_groups = OutputGroupInfo { groups };
            ct.testonly = gen.testonly;
            Ok(ct)
        }
        Target::PackageGroup { label, .. } => {
            let null_cfg = config.to_core_only();
            if let Some(pkg) = resolver
                .package_loader()
                .and_then(|l| l.load_package(label.package_identifier()).ok())
            {
                if let Some(incs) = pkg.group_includes.get(label.name()) {
                    for inc in incs {
                        resolver.configured_target(inc, &null_cfg)?;
                    }
                }
            }
            let mut ct =
                ConfiguredTarget::new(label.clone(), config.clone(), "package_group".to_string());
            ct.providers.insert(
                "DefaultInfo".into(),
                zel_analysis::starlark_rule::default_info_provider(
                    &[],
                    Default::default(),
                    Default::default(),
                    None,
                ),
            );
            ct.providers.insert(
                "PackageSpecificationInfo".into(),
                zel_analysis::starlark_rule::provider_instance(
                    "PackageSpecificationInfo",
                    vec![("label", Value::Label(label.clone()))],
                ),
            );
            Ok(ct)
        }
        Target::EnvironmentGroup { label, .. } => {
            // Not configurable: environment groups are only used by the (legacy) constraint checks.
            Ok(ConfiguredTarget::new(
                label.clone(),
                config.clone(),
                "environment_group".to_string(),
            ))
        }
        Target::Rule(rule) => analyze_rule(rule, config, resolver, acfg),
    }
}

fn label_list(v: &Value) -> Vec<Label> {
    zel_starlark::eval::builtins::value_to_sequence(v)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|x| match x {
            Value::Label(l) => Some(l),
            Value::String(s) => Label::parse_canonical(&s).ok(),
            _ => None,
        })
        .collect()
}

fn toolchain_requests(v: Option<&Value>) -> Vec<ToolchainTypeRequest> {
    let Some(v) = v else { return Vec::new() };
    zel_starlark::eval::builtins::value_to_sequence(v)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|x| match &x {
            Value::Struct(s) if s.fields.contains_key("_toolchain_type") => {
                let l = match s.fields.get("name") {
                    Some(Value::Label(l)) => l.clone(),
                    Some(Value::String(n)) => Label::parse_canonical(n).ok()?,
                    _ => return None,
                };
                let mandatory = !matches!(s.fields.get("mandatory"), Some(Value::Bool(false)));
                Some(ToolchainTypeRequest {
                    toolchain_type: l,
                    mandatory,
                })
            }
            Value::Label(l) => Some(ToolchainTypeRequest {
                toolchain_type: l.clone(),
                mandatory: true,
            }),
            Value::String(s) => Label::parse_canonical(s)
                .ok()
                .map(|l| ToolchainTypeRequest {
                    toolchain_type: l,
                    mandatory: true,
                }),
            _ => None,
        })
        .collect()
}

/// The toolchain context of one rule / exec group: the selected execution platform, the
/// `ToolchainInfo` of each requested type (`None` for unresolved optional toolchains) and the
/// toolchains' Make variables.
pub(crate) struct ToolchainCtx {
    pub exec_platform: String,
    pub toolchains: BTreeMap<String, Value>,
    pub make_variables: BTreeMap<String, String>,
    /// Configured targets of the requested toolchain types and the resolved implementations
    /// (`PostAnalysisQueryEnvironment.targetifyValues` shows them as direct deps of the rule).
    pub deps: Vec<Arc<ConfiguredTarget>>,
    /// Resolved toolchain implementation targets (`ToolchainContext.prerequisiteTargets`).
    pub resolved_targets: Vec<Arc<ConfiguredTarget>>,
}

pub(crate) fn resolve_ctx_toolchains(
    owner: &Label,
    types: &[ToolchainTypeRequest],
    exec_compatible_with: &[Label],
    cfg: &BuildConfigurationKey,
    acfg: &AnalysisConfig,
    resolver: &Arc<dyn DepResolver>,
) -> Result<ToolchainCtx, String> {
    let exec_platforms = acfg.all_execution_platforms()?;
    let core_cfg = cfg.to_core_only();
    let mut resolved_requests: Vec<ToolchainTypeRequest> = Vec::new();
    let mut type_aliases: Vec<(Label, Label, Arc<ConfiguredTarget>)> = Vec::new();
    for t in types {
        let tt = match resolver.configured_target(&t.toolchain_type, &core_cfg) {
            Ok(ct) => ct,
            Err(e) => {
                if t.mandatory {
                    return Err(e);
                }
                continue;
            }
        };
        let real_label = match tt.providers.get("ToolchainTypeInfo") {
            Some(Value::ProviderInstance { fields, .. }) => match fields.get("type_label") {
                Some(Value::Label(l)) => l.clone(),
                _ => t.toolchain_type.clone(),
            },
            _ => {
                if t.mandatory {
                    return Err(format!(
                        "Target {} was referenced as a toolchain type, but does not provide ToolchainTypeInfo",
                        t.toolchain_type.canonical_form()
                    ));
                }
                continue;
            }
        };
        resolved_requests.push(ToolchainTypeRequest {
            toolchain_type: real_label.clone(),
            mandatory: t.mandatory,
        });
        type_aliases.push((t.toolchain_type.clone(), real_label, tt));
    }
    let resolved = resolve_toolchains_lazy(
        owner,
        &resolved_requests,
        exec_compatible_with,
        cfg,
        &exec_platforms,
        resolver,
        acfg,
    )?;
    // The platform's own label (an `alias` such as `@bazel_tools//tools:host_platform` reports
    // the aliased platform's, like `PlatformInfo.label`).
    let exec_label = match resolved.execution_platform.providers.get("PlatformInfo") {
        Some(Value::ProviderInstance { fields, .. }) => match fields.get("label") {
            Some(Value::Label(l)) => l.canonical_form(),
            _ => resolved.execution_platform.label.canonical_form(),
        },
        _ => resolved.execution_platform.label.canonical_form(),
    };
    let mut out = ToolchainCtx {
        exec_platform: exec_label,
        toolchains: BTreeMap::new(),
        make_variables: BTreeMap::new(),
        deps: Vec::new(),
        resolved_targets: Vec::new(),
    };
    for (req_label, real_label, tt) in &type_aliases {
        out.deps.push(tt.clone());
        let imp = resolved
            .toolchains
            .get(&real_label.canonical_form())
            .and_then(|o| o.as_ref());
        let v = match imp {
            None => Value::None,
            Some(l) => {
                // `DependencyProducer`: toolchain dependencies keep the configuration of the
                // requesting target (no exec transition).
                let ct = resolver.configured_target(l, cfg)?;
                out.deps.push(ct.clone());
                out.resolved_targets.push(ct.clone());
                if let Some(Value::ProviderInstance { fields, .. }) =
                    ct.providers.get("TemplateVariableInfo")
                {
                    if let Some(Value::Dict(d)) =
                        fields.get("variables").or_else(|| fields.get("vars"))
                    {
                        for (k, v) in d.lock().unwrap().items() {
                            if let (Value::String(k), Value::String(v)) = (k, v) {
                                out.make_variables.insert(k, v);
                            }
                        }
                    }
                }
                ct.providers.get("ToolchainInfo").cloned().ok_or_else(|| {
                    format!(
                        "{}: toolchain {} for type {} does not provide ToolchainInfo",
                        owner.canonical_form(),
                        l.canonical_form(),
                        real_label.canonical_form()
                    )
                })?
            }
        };
        out.toolchains.insert(req_label.canonical_form(), v.clone());
        out.toolchains
            .entry(real_label.canonical_form())
            .or_insert(v);
    }
    Ok(out)
}

fn check_target_compatible_with(
    rule: &RuleInstance,
    cfg: &BuildConfigurationKey,
    resolver: &Arc<dyn DepResolver>,
    acfg: &AnalysisConfig,
) -> Result<Option<zel_analysis::IncompatiblePlatform>, String> {
    let Some(raw) = rule.attr_or_default("target_compatible_with") else {
        return Ok(None);
    };
    let resolved = zel_analysis::select::resolve_select(
        &raw,
        "target_compatible_with",
        rule,
        cfg,
        resolver.as_ref(),
    )?;
    let labels = match resolved {
        zel_packages::AttrValue::LabelList(l) if !l.is_empty() => l,
        _ => return Ok(None),
    };
    let Some(plat) = toolchain::target_platform(cfg, resolver, acfg)? else {
        return Ok(None);
    };
    let tp_label = match plat.providers.get("PlatformInfo") {
        Some(Value::ProviderInstance { fields, .. }) => match fields.get("label") {
            Some(Value::Label(l)) => l.clone(),
            _ => plat.label.clone(),
        },
        _ => plat.label.clone(),
    };
    let mut unsatisfied: Vec<Label> = Vec::new();
    for cv in labels {
        let cv_ct = resolver.configured_target(&cv, cfg)?;
        if let Some(Value::ProviderInstance { fields, .. }) =
            cv_ct.providers.get("ConstraintValueInfo")
        {
            if !toolchain::platform_has_value(&plat, &cv_ct) {
                let real = match fields.get("label") {
                    Some(Value::Label(l)) => l.clone(),
                    _ => cv,
                };
                if !unsatisfied.contains(&real) {
                    unsatisfied.push(real);
                }
            }
        } else if let Some(cmp) = cv_ct.providers.get("ConfigMatchingProvider") {
            if !zel_analysis::select::matching_info(cmp)
                .map(|(m, _)| m)
                .unwrap_or(false)
                && !unsatisfied.contains(&cv)
            {
                unsatisfied.push(cv);
            }
        }
    }
    if unsatisfied.is_empty() {
        Ok(None)
    } else {
        Ok(Some(zel_analysis::IncompatiblePlatform {
            target_platform: tp_label,
            constraints: unsatisfied,
            dependency_chain: Vec::new(),
        }))
    }
}

fn check_visibility_groups(
    rule: &RuleInstance,
    config: &BuildConfigurationKey,
    resolver: &Arc<dyn DepResolver>,
) -> Result<(), String> {
    if rule.rule_class == "package_group" {
        return Ok(());
    }
    let null_cfg = config.to_core_only();
    for vis in &rule.extra.effective_visibility {
        if vis.package_name() != "visibility"
            && vis.name() != "__pkg__"
            && vis.name() != "__subpackages__"
        {
            resolver.configured_target(vis, &null_cfg)?;
        }
    }
    Ok(())
}

fn analyze_rule(
    rule: &RuleInstance,
    config: &BuildConfigurationKey,
    resolver: Arc<dyn DepResolver>,
    acfg: &AnalysisConfig,
) -> Result<ConfiguredTarget, String> {
    let is_plat = matches!(
        rule.rule_class.as_str(),
        "platform" | "constraint_value" | "constraint_setting"
    );
    let plat_cfg;
    let config = if is_plat {
        plat_cfg = config.to_core_only();
        &plat_cfg
    } else {
        config
    };
    if !is_plat {
        check_visibility_groups(rule, config, &resolver)?;
    }
    let starlark = rule
        .extra
        .rule_class
        .as_ref()
        .and_then(|rc| rc.starlark.clone());
    if let Some(spec) = starlark {
        // Incoming (rule-level) transition.
        let mut cfg = config.clone();
        if let Some(t @ Value::Struct(ts)) = spec.kwargs.get("cfg") {
            if ts.fields.get("_build_setting").and_then(|v| v.as_str()) == Some("none")
                && !ts.fields.contains_key("implementation")
            {
                cfg = config.to_core_only();
            } else if ts.fields.contains_key("implementation") {
                let attrs = zel_analysis::starlark_rule::build_transition_attr_struct(
                    rule,
                    config,
                    resolver.as_ref(),
                );
                cfg = zel_analysis::transition::apply_transition_with_resolver(
                    t,
                    config,
                    Some(&attrs),
                    Some(resolver.as_ref()),
                )
                .map_err(|e| {
                    format!(
                        "in {} rule {}: {e}",
                        rule.rule_class,
                        rule.label.canonical_form()
                    )
                })?;
            }
        }
        let mut inputs = AnalysisInputs::new(resolver.clone());
        inputs.allow_unresolved_symlinks = acfg.allow_unresolved_symlinks;
        inputs.compact_repo_mapping = !acfg.no_compact_repo_mapping;
        inputs.allow_tags_propagation = cfg
            .options
            .get("incompatible_allow_tags_propagation")
            .map(|v| v != "false")
            .unwrap_or(true);
        inputs.exec_platform = Some(acfg.default_exec_platform());
        if let Ok(Some(plat_ct)) = toolchain::target_platform(&cfg, &resolver, acfg) {
            let _ = resolver.check_dependency(rule, "$target_platform", &plat_ct);
            toolchain::record_exec_platform_edge(rule, &cfg, &resolver, acfg);
        }
        let types = toolchain_requests(spec.kwargs.get("toolchains"));
        let mut rule_ecw: Vec<Label> = spec
            .kwargs
            .get("exec_compatible_with")
            .map(label_list)
            .unwrap_or_default();
        if let Some(zel_packages::AttrValue::LabelList(l)) =
            rule.extra.attr_values.get("exec_compatible_with")
        {
            rule_ecw.extend(l.iter().cloned());
        }
        if !types.is_empty() {
            let tc = resolve_ctx_toolchains(&rule.label, &types, &rule_ecw, &cfg, acfg, &resolver)?;
            for d in &tc.deps {
                let _ = resolver.check_dependency(rule, "$toolchains", d);
            }
            inputs.exec_platform = Some(tc.exec_platform.clone());
            inputs.extra_make_variables.extend(tc.make_variables);
            inputs.toolchains.extend(tc.toolchains.clone());
            // `--incompatible_auto_exec_groups`: automatically creates an exec group keyed by
            // each requested toolchain type's canonical label.
            let auto_eg = cfg
                .options
                .get("incompatible_auto_exec_groups")
                .map(|v| v == "true")
                .unwrap_or(false);
            if auto_eg {
                for t in &types {
                    let gname = t.toolchain_type.canonical_form();
                    let gtc = resolve_ctx_toolchains(
                        &rule.label,
                        std::slice::from_ref(t),
                        &rule_ecw,
                        &cfg,
                        acfg,
                        &resolver,
                    )?;
                    inputs
                        .exec_group_platforms
                        .insert(gname.clone(), gtc.exec_platform);
                    inputs.exec_group_toolchains.insert(gname, gtc.toolchains);
                }
            }
        }
        // Exec groups: each group resolves its own toolchains / execution platform
        // (`copy_from_rule` inherits the rule's).
        if let Some(Value::Dict(d)) = spec.kwargs.get("exec_groups") {
            for (name, g) in d.lock().unwrap().items() {
                let Some(name) = name.as_str().map(str::to_string) else {
                    continue;
                };
                let Value::Struct(gs) = &g else { continue };
                let copy = matches!(gs.fields.get("copy_from_rule"), Some(Value::Bool(true)));
                let mut gtypes = toolchain_requests(gs.fields.get("toolchains"));
                let mut gecw: Vec<Label> = gs
                    .fields
                    .get("exec_compatible_with")
                    .map(label_list)
                    .unwrap_or_default();
                if copy {
                    gtypes.extend(types.clone());
                    gecw.extend(rule_ecw.clone());
                }
                let tc =
                    resolve_ctx_toolchains(&rule.label, &gtypes, &gecw, &cfg, acfg, &resolver)?;
                for d in &tc.deps {
                    let _ = resolver.check_dependency(rule, "$toolchains", d);
                }
                inputs
                    .exec_group_platforms
                    .insert(name.clone(), tc.exec_platform);
                inputs.exec_group_toolchains.insert(name, tc.toolchains);
            }
        }
        let mut ct = analyze_starlark_rule(rule, &cfg, inputs)?;
        ct.visibility = rule.extra.effective_visibility.clone();
        if ct.incompatible.is_none() {
            if let Some(inc) = check_target_compatible_with(rule, &cfg, &resolver, acfg)? {
                ct.incompatible = Some(inc);
                ct.actions.clear();
                ct.typed_actions.clear();
            }
        }
        return Ok(ct);
    }

    if rule.rule_class != "platform"
        && rule.rule_class != "constraint_value"
        && rule.rule_class != "constraint_setting"
        && rule.rule_class != "package_group"
        && rule.rule_class != "alias"
        && rule.rule_class != "toolchain_type"
        && (rule.rule_class != "config_setting"
            || rule
                .extra
                .attr_values
                .get("constraint_values")
                .is_some_and(|v| !v.labels().is_empty()))
        && rule.label.name() != "host_platform"
    {
        if let Ok(Some(plat_ct)) = toolchain::target_platform(config, &resolver, acfg) {
            let _ = resolver.check_dependency(rule, "$target_platform", &plat_ct);
            toolchain::record_exec_platform_edge(rule, config, &resolver, acfg);
        }
    }
    let n = native::Nctx {
        rule,
        cfg: config,
        resolver: &resolver,
        acfg,
    };
    let mut ct = match rule.rule_class.as_str() {
        "filegroup" => native::filegroup(&n)?,
        "alias" => native::alias(&n)?,
        "genrule" => native::genrule(&n)?,
        "test_suite" => native::test_suite(&n)?,
        "genquery" => native::genquery(&n)?,
        "java_plugins_flag_alias" => native::java_plugins_flag_alias(&n)?,
        "cc_libc_top_alias" => native::cc_libc_top_alias(&n)?,
        "extra_action" | "action_listener" | "environment" | "starlark_doc_extract" => native::extra_action_like(&n)?,
        "label_flag" => native::label_flag(&n)?,
        "label_setting" => native::label_setting(&n)?,
        "config_setting" => config::config_setting(&n)?,
        "constraint_setting" => config::constraint_setting(&n)?,
        "constraint_value" => config::constraint_value(&n)?,
        "platform" => config::platform(&n)?,
        "toolchain_type" => config::toolchain_type(&n)?,
        "toolchain" => config::toolchain(&n)?,
        "copy_file" | "rust_binary" | "rust_test" | "rust_library" | "rust_shared_library" => {
            let mut ct = legacy::analyze_legacy(rule, config)?;
            ct.providers.insert(
                "DefaultInfo".into(),
                zel_analysis::starlark_rule::default_info_provider(
                    &ct.default_info.files,
                    zel_analysis::Runfiles {
                        artifacts: ct.default_info.default_runfiles.clone(),
                        ..Default::default()
                    },
                    zel_analysis::Runfiles {
                        artifacts: ct.default_info.data_runfiles.clone(),
                        ..Default::default()
                    },
                    ct.default_info.executable.clone(),
                ),
            );
            ct
        }
        other => {
            return Err(format!(
                "{}in {other} rule {}: rule class '{other}' has no implementation in this build (it requires a toolchain-backed rule implementation that is not available)",
                rule.extra.location.as_ref().map(|l| format!("{l}: ")).unwrap_or_default(),
                rule.label.canonical_form()
            ))
        }
    };
    ct.visibility = rule.extra.effective_visibility.clone();
    if rule.rule_class != "alias" {
        ct.testonly = rule.testonly;
    }
    if ct.incompatible.is_none()
        && !is_plat
        && rule.rule_class != "alias"
        && rule.rule_class != "package_group"
    {
        if let Some(inc) = check_target_compatible_with(rule, config, &resolver, acfg)? {
            ct.incompatible = Some(inc);
            ct.actions.clear();
            ct.typed_actions.clear();
        }
    }
    Ok(ct)
}

#[cfg(test)]
mod tests;
