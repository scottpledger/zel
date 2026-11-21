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
    let sibling_layout = acfg.sibling_repository_layout
        || resolver
            .package_loader()
            .is_some_and(|l| l.config().sibling_repository_layout);
    match target {
        Target::InputFile { label } => Ok(resolver::source_file_target_with_layout(
            label,
            config,
            sibling_layout,
        )),
        Target::OutputFile {
            label,
            generating_rule,
        } => {
            let gen = resolver.configured_target(generating_rule, config)?;
            let mut rel = label
                .package_identifier()
                .package_fragment()
                .get_relative_str(label.name());
            // Generated files of external repositories live below `external/<repo>/` unless
            // `--experimental_sibling_repository_layout` is enabled.
            if !sibling_layout && !label.package_identifier().repository().is_main() {
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
            ct.visibility = gen.visibility.clone();
            Ok(ct)
        }
        Target::PackageGroup { label, packages } => {
            thread_local! {
                static ACTIVE_PKG_GROUPS: std::cell::RefCell<Vec<Label>> =
                    const { std::cell::RefCell::new(Vec::new()) };
            }
            let cycle_err = ACTIVE_PKG_GROUPS.with(|stack| {
                let mut st = stack.borrow_mut();
                if let Some(pos) = st.iter().position(|x| x == label) {
                    let mut cyc: Vec<String> =
                        st[pos..].iter().map(|l| l.canonical_form()).collect();
                    cyc.push(label.canonical_form());
                    return Some(format!("cycle in dependency graph: {}", cyc.join(" -> ")));
                }
                st.push(label.clone());
                None
            });
            if let Some(e) = cycle_err {
                return Err(e);
            }
            struct ActivePkgGroupGuard;
            impl Drop for ActivePkgGroupGuard {
                fn drop(&mut self) {
                    ACTIVE_PKG_GROUPS.with(|stack| {
                        stack.borrow_mut().pop();
                    });
                }
            }
            let _guard = ActivePkgGroupGuard;

            let null_cfg = config.to_core_only();
            let repo = label.package_identifier().repository();
            let entries = resolver.repo_mapping_entries(repo.name());
            let mut map = std::collections::HashMap::new();
            for (a, c) in entries {
                if let Ok(rn) = zel_cmdline::RepositoryName::create(&c) {
                    map.insert(a, rn);
                }
            }
            let mapping = zel_cmdline::RepositoryMapping::new(map, repo.clone());
            let mut direct_specs = Vec::with_capacity(packages.len());
            for raw in packages {
                let spec = zel_packages::pkg_spec::PackageSpec::parse(&mapping, repo, raw)
                    .map_err(|e| format!("package group '{}': {e}", label.canonical_form()))?;
                direct_specs.push(spec);
            }

            let mut group_blocks: Vec<String> = Vec::new();
            if let Some(loader) = resolver.package_loader() {
                let pkg = loader
                    .load_package(label.package_identifier())
                    .map_err(|e| {
                        let mut lines: Vec<String> = e
                            .events
                            .iter()
                            .filter(|ev| ev.kind == zel_packages::lookup::EventKind::Error)
                            .map(|ev| ev.render())
                            .collect();
                        lines.push(e.message.clone());
                        lines.join("\n")
                    })?;
                if pkg.contains_errors {
                    let msgs: Vec<String> = pkg
                        .events
                        .iter()
                        .filter(|ev| ev.kind == zel_packages::lookup::EventKind::Error)
                        .map(|ev| ev.render())
                        .collect();
                    return Err(format!(
                        "error loading package '{}': {}",
                        label.package_identifier().package_fragment(),
                        msgs.join("\n")
                    ));
                }
                if let Some(incs) = pkg.group_includes.get(label.name()) {
                    for inc in incs {
                        let inc_ct = resolver.configured_target(inc, &null_cfg)?;
                        let Some(Value::ProviderInstance { fields, .. }) =
                            inc_ct.providers.get("PackageSpecificationInfo")
                        else {
                            return Err(format!(
                                "Label '{}' does not refer to a package group",
                                inc.canonical_form()
                            ));
                        };
                        if let Some(Value::Host(_, inc_encoded)) = fields.get("contains") {
                            if !inc_encoded.is_empty() {
                                group_blocks.push(inc_encoded.clone());
                            }
                        }
                    }
                }
            }
            let own_encoded =
                zel_packages::pkg_spec::PackageGroupContents::from_specs(direct_specs)
                    .encode_canonical();
            if !own_encoded.is_empty() {
                group_blocks.push(own_encoded);
            }
            let encoded_specs = group_blocks.join("\n---\n");
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
                    vec![
                        ("label", Value::Label(label.clone())),
                        (
                            "contains",
                            Value::Host("pkg_spec_contains".into(), encoded_specs),
                        ),
                    ],
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
            Value::Struct(s)
                if s.fields.contains_key("toolchain_type")
                    || s.fields.contains_key("_toolchain_type") =>
            {
                let l = match s
                    .fields
                    .get("toolchain_type")
                    .or_else(|| s.fields.get("name"))
                {
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
                return Err(format!(
                    "Target {} was referenced as a toolchain type, but does not provide ToolchainTypeInfo",
                    t.toolchain_type.canonical_form()
                ));
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
    let is_analysis_test = rule
        .extra
        .rule_class
        .as_ref()
        .and_then(|rc| rc.starlark.as_ref())
        .and_then(|sp| sp.kwargs.get("analysis_test"))
        .is_some_and(|v| matches!(v, Value::Bool(true)));
    match analyze_rule_inner(rule, config, resolver, acfg) {
        Ok(ct) => Ok(ct),
        Err(e) if config.bool_option("allow_analysis_failures", false) && !is_analysis_test => {
            let is_test = rule.extra.rule_class.as_ref().is_some_and(|rc| rc.is_test);
            Ok(zel_analysis::starlark_rule::make_analysis_failure_target(
                &rule.label,
                config,
                &rule.rule_class,
                rule.testonly,
                is_test,
                Some(e),
                Vec::new(),
            ))
        }
        Err(e) => Err(e),
    }
}

fn analyze_rule_inner(
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
                && !ts.fields.contains_key("_composed_transitions")
            {
                cfg = config.to_core_only();
            } else if ts.fields.contains_key("implementation")
                || ts.fields.contains_key("_composed_transitions")
            {
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
        inputs.sibling_repository_layout = acfg.sibling_repository_layout
            || resolver
                .package_loader()
                .is_some_and(|l| l.config().sibling_repository_layout);
        inputs.allow_tags_propagation =
            cfg.bool_option("incompatible_allow_tags_propagation", true);
        inputs.exec_platform = Some(acfg.default_exec_platform());
        inputs.has_exec_groups_context = true;
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
        let mut per_eg_ecw: BTreeMap<String, Vec<Label>> = BTreeMap::new();
        if let Some(zel_packages::AttrValue::LabelListDict(entries)) =
            rule.extra.attr_values.get("exec_group_compatible_with")
        {
            for (gname, labels) in entries {
                per_eg_ecw
                    .entry(gname.clone())
                    .or_default()
                    .extend(labels.iter().cloned());
            }
        }
        if !types.is_empty() || !rule_ecw.is_empty() {
            let tc = resolve_ctx_toolchains(&rule.label, &types, &rule_ecw, &cfg, acfg, &resolver)?;
            for d in &tc.deps {
                resolver.check_dependency(rule, "$toolchains", d)?;
            }
            inputs.exec_platform = Some(tc.exec_platform.clone());
            inputs.extra_make_variables.extend(tc.make_variables);
            inputs.toolchains.extend(tc.toolchains.clone());
            // `--incompatible_auto_exec_groups`: automatically creates an exec group keyed by
            // each requested toolchain type's canonical label.
            let auto_eg = cfg.bool_option("incompatible_auto_exec_groups", false);
            if auto_eg && !types.is_empty() {
                for t in &types {
                    let gname = t.toolchain_type.canonical_form();
                    let mut auto_ecw = rule_ecw.clone();
                    if let Some(extra) = per_eg_ecw.get(&gname) {
                        auto_ecw.extend(extra.iter().cloned());
                    }
                    let gtc = resolve_ctx_toolchains(
                        &rule.label,
                        std::slice::from_ref(t),
                        &auto_ecw,
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
                let is_default_test_eg = matches!(
                    gs.fields.get("_default_test_exec_group"),
                    Some(Value::Bool(true))
                );
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
                if let Some(extra) = per_eg_ecw.get(&name) {
                    gecw.extend(extra.iter().cloned());
                }
                let tc =
                    resolve_ctx_toolchains(&rule.label, &gtypes, &gecw, &cfg, acfg, &resolver)?;
                if is_default_test_eg {
                    for d in &tc.resolved_targets {
                        resolver.check_dependency(rule, "[toolchain dependency: test]", d)?;
                    }
                } else {
                    for d in &tc.deps {
                        resolver.check_dependency(rule, "$toolchains", d)?;
                    }
                }
                inputs
                    .exec_group_platforms
                    .insert(name.clone(), tc.exec_platform);
                inputs.exec_group_toolchains.insert(name, tc.toolchains);
            }
        }
        if (rule.is_test_rule() || rule.extra.rule_class.as_ref().is_some_and(|rc| rc.is_test))
            && rule.rule_class != "test_suite"
            && !inputs.exec_group_platforms.contains_key("test")
        {
            let test_tt =
                Label::parse_canonical("@bazel_tools//tools/test:default_test_toolchain_type")
                    .expect("valid default_test_toolchain_type label");
            let test_reqs = [ToolchainTypeRequest {
                toolchain_type: test_tt,
                mandatory: false,
            }];
            let test_ecw = per_eg_ecw.get("test").cloned().unwrap_or_default();
            let tc =
                resolve_ctx_toolchains(&rule.label, &test_reqs, &test_ecw, &cfg, acfg, &resolver)?;
            for d in &tc.resolved_targets {
                resolver.check_dependency(rule, "[toolchain dependency: test]", d)?;
            }
            inputs
                .exec_group_platforms
                .insert("test".to_string(), tc.exec_platform);
            inputs
                .exec_group_toolchains
                .insert("test".to_string(), tc.toolchains);
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
        "extra_action" | "action_listener" | "environment" => native::extra_action_like(&n)?,
        "starlark_doc_extract" => native::starlark_doc_extract(&n)?,
        "label_flag" => native::label_flag(&n)?,
        "label_setting" => native::label_setting(&n)?,
        "config_setting" => config::config_setting(&n)?,
        "config_feature_flag" => config::config_feature_flag(&n)?,
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
    ct.created_in_symbolic_macro = rule.extra.macro_instance.is_some();
    if rule.rule_class != "alias" && !(ct.is_late_bound_alias && ct.actual.is_some()) {
        ct.testonly = rule.testonly;
    }
    if !is_plat && rule.rule_class != "alias" && rule.rule_class != "package_group" {
        if ct.exec_platform.is_none() {
            ct.exec_platform = Some(acfg.default_exec_platform());
        }
        if (ct.is_test || rule.is_test_rule())
            && rule.rule_class != "test_suite"
            && !ct.exec_group_platforms.contains_key("test")
        {
            let test_tt =
                Label::parse_canonical("@bazel_tools//tools/test:default_test_toolchain_type")
                    .expect("valid default_test_toolchain_type label");
            let test_reqs = [ToolchainTypeRequest {
                toolchain_type: test_tt,
                mandatory: false,
            }];
            let mut test_ecw: Vec<Label> = Vec::new();
            if let Some(zel_packages::AttrValue::LabelListDict(entries)) =
                rule.extra.attr_values.get("exec_group_compatible_with")
            {
                for (gname, labels) in entries {
                    if gname == "test" {
                        test_ecw.extend(labels.iter().cloned());
                    }
                }
            }
            if let Ok(tc) =
                resolve_ctx_toolchains(&rule.label, &test_reqs, &test_ecw, config, acfg, &resolver)
            {
                ct.exec_group_platforms
                    .insert("test".to_string(), tc.exec_platform);
                ct.exec_group_toolchains
                    .insert("test".to_string(), tc.toolchains);
            } else {
                ct.exec_group_platforms
                    .insert("test".to_string(), acfg.default_exec_platform());
                ct.exec_group_toolchains
                    .insert("test".to_string(), BTreeMap::new());
            }
        }
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
