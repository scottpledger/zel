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

//! Native rule implementations that mirror Bazel's Java rules: `filegroup`, `alias`,
//! `genrule`, `config_setting`, `test_suite`, `sh_binary`/`sh_test`/`sh_library`,
//! `label_flag`/`label_setting`.

use std::collections::BTreeMap;
use std::sync::Arc;
use zel_analysis::location::{expand_make_variables, LocationMode};
use zel_analysis::starlark_rule::{
    default_info_provider, mk_list, provider_instance, target_runfiles, DepResolver,
};
use zel_analysis::{
    Action, ActionKind, Artifact, ArtifactRoot, BuildConfigurationKey, CommandLineArgs,
    ConfiguredTarget, DefaultInfo, FileContent, OutputGroupInfo, Runfiles,
};
use zel_cmdline::Label;
use zel_packages::attr::AttrValue;
use zel_packages::RuleInstance;
use zel_starlark::Value;
use zel_vfs::PathFragment;

pub(crate) struct Nctx<'a> {
    pub rule: &'a RuleInstance,
    pub cfg: &'a BuildConfigurationKey,
    pub resolver: &'a Arc<dyn DepResolver>,
    pub acfg: &'a crate::AnalysisConfig,
}

impl Nctx<'_> {
    /// The select-resolved value of an attribute (explicit or default).
    pub fn value(&self, name: &str) -> Result<Option<AttrValue>, String> {
        match self.rule.attr_or_default(name) {
            None => Ok(None),
            Some(v) => zel_analysis::select::resolve_select(
                &v,
                name,
                self.rule,
                self.cfg,
                self.resolver.as_ref(),
            )
            .map(Some),
        }
    }

    pub fn explicit(&self, name: &str) -> bool {
        self.rule.extra.attr_values.contains_key(name)
    }

    pub fn string(&self, name: &str) -> Result<String, String> {
        Ok(match self.value(name)? {
            Some(AttrValue::Str(s)) => s,
            _ => String::new(),
        })
    }

    pub fn boolean(&self, name: &str) -> Result<bool, String> {
        Ok(matches!(self.value(name)?, Some(AttrValue::Bool(true))))
    }

    pub fn strings(&self, name: &str) -> Result<Vec<String>, String> {
        Ok(match self.value(name)? {
            Some(AttrValue::StrList(l)) => l,
            _ => Vec::new(),
        })
    }

    pub fn labels(&self, name: &str) -> Result<Vec<Label>, String> {
        Ok(match self.value(name)? {
            Some(AttrValue::LabelList(l)) | Some(AttrValue::OutputList(l)) => l,
            Some(AttrValue::Label(Some(l))) | Some(AttrValue::Output(Some(l))) => vec![l],
            _ => Vec::new(),
        })
    }

    pub fn deps(&self, name: &str, exec: bool) -> Result<Vec<Arc<ConfiguredTarget>>, String> {
        let cfg = if exec {
            self.cfg.to_exec_for(&self.acfg.default_exec_platform())
        } else {
            self.cfg.clone()
        };
        self.labels(name)?
            .iter()
            .map(|l| {
                let ct = self.resolver.configured_target(l, &cfg)?;
                self.resolver.check_dependency(self.rule, name, &ct)?;
                Ok(ct)
            })
            .collect()
    }

    pub fn sibling_layout(&self) -> bool {
        self.acfg.sibling_repository_layout
            || self
                .resolver
                .package_loader()
                .is_some_and(|l| l.config().sibling_repository_layout)
    }

    pub fn out_artifact(&self, l: &Label) -> Artifact {
        Artifact::derived_in_root_with_layout(
            &self.cfg.mnemonic(),
            ArtifactRoot::Bin,
            l.package_identifier()
                .package_fragment()
                .get_relative_str(l.name()),
            self.rule.label.clone(),
            self.sibling_layout(),
        )
    }

    fn err(&self, attr: &str, msg: &str) -> String {
        let loc = self.rule.extra.location.clone().unwrap_or_default();
        let prefix = if loc.is_empty() {
            String::new()
        } else {
            format!("{loc}: ")
        };
        let _ = attr;
        format!(
            "{prefix}in {} rule {}: {msg}",
            self.rule.rule_class,
            self.rule.label.canonical_form()
        )
    }

    /// `RuleContext.attributeError`: `in <attr> attribute of <rule class> rule <label>: <msg>`.
    fn attr_err(&self, attr: &str, msg: &str) -> String {
        let loc = self.rule.extra.location.clone().unwrap_or_default();
        let prefix = if loc.is_empty() {
            String::new()
        } else {
            format!("{loc}: ")
        };
        format!(
            "{prefix}in {attr} attribute of {} rule {}: {msg}",
            self.rule.rule_class,
            self.rule.label.canonical_form()
        )
    }
}

fn dedup(files: Vec<Artifact>) -> Vec<Artifact> {
    let mut seen = std::collections::BTreeSet::new();
    files
        .into_iter()
        .filter(|a| seen.insert(a.exec_path.path_string().to_string()))
        .collect()
}

fn finish(
    n: &Nctx,
    files: Vec<Artifact>,
    default_rf: Runfiles,
    data_rf: Runfiles,
    executable: Option<Artifact>,
    actions: Vec<Action>,
) -> Result<ConfiguredTarget, String> {
    let mut ct = ConfiguredTarget::new(
        n.rule.label.clone(),
        n.cfg.clone(),
        n.rule.rule_class.clone(),
    );
    ct.default_info = DefaultInfo {
        files: files.clone(),
        data_runfiles: data_rf.artifacts.clone(),
        default_runfiles: default_rf.artifacts.clone(),
        executable: executable.clone(),
    };
    ct.providers.insert(
        "DefaultInfo".to_string(),
        default_info_provider(&files, default_rf, data_rf, executable.clone()),
    );
    ct.providers.insert(
        "OutputGroupInfo".to_string(),
        provider_instance("OutputGroupInfo", vec![]),
    );
    let mut groups = BTreeMap::new();
    groups.insert("default".to_string(), files.clone());
    if executable.is_some() || n.rule.is_binary_rule() || n.rule.is_test_rule() {
        groups.insert("compilation_outputs".to_string(), files);
    }
    ct.output_groups = OutputGroupInfo { groups };
    // `SpawnAction.createActionEnvironment` against the owner's configuration.
    let allow_tags = n
        .cfg
        .bool_option("incompatible_allow_tags_propagation", true);
    let mut actions = actions;
    for a in &mut actions {
        if allow_tags && matches!(a.kind, ActionKind::Spawn { .. }) {
            zel_analysis::action::propagate_rule_tags(n.rule, &mut a.execution_requirements);
        }
        a.apply_configuration_env(n.cfg);
    }
    for a in &actions {
        ct.actions.extend(a.lower());
    }
    ct.typed_actions = actions;
    ct.testonly = n.rule.testonly;
    ct.is_test = n.rule.is_test_rule();
    Ok(ct)
}

pub(crate) fn filegroup(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let srcs = n.deps("srcs", false)?;
    let data = n.deps("data", false)?;
    let group = n.string("output_group")?;
    if group.ends_with("_INTERNAL_") {
        return Err(n.attr_err(
            "output_group",
            &format!("Output group {group} is not permitted for reference in filegroups."),
        ));
    }
    let mut files = Vec::new();
    for s in &srcs {
        if group.is_empty() {
            files.extend(s.default_info.files.iter().cloned());
        } else if let Some(g) = s.output_groups.groups.get(&group) {
            files.extend(g.iter().cloned());
        }
    }
    let files = dedup(files);
    // `Filegroup.java`: default runfiles = `RunfilesSupport.addRunfiles(DEFAULT_RUNFILES)`; data
    // runfiles = `filesToBuild` + `addRunfiles(DATA_RUNFILES)` (`incompatible_filegroup_runfiles_for_data`).
    // `data` deps contribute their data runfiles *and* their files (`alwaysIncludeFilesToBuildInData`).
    let mut default_rf = Runfiles::default();
    let mut data_rf = Runfiles {
        artifacts: files.clone(),
        ..Default::default()
    };
    for d in &data {
        let r = target_runfiles(d, true).merge(&Runfiles {
            artifacts: d.default_info.files.clone(),
            ..Default::default()
        });
        default_rf = default_rf.merge(&r);
        data_rf = data_rf.merge(&r);
    }
    for s in &srcs {
        default_rf = default_rf.merge(&target_runfiles(s, false));
        data_rf = data_rf.merge(&target_runfiles(s, true));
    }
    // `Filegroup.getExecutable`: the single file of the group, if there is exactly one.
    let exe = if files.len() == 1 {
        Some(files[0].clone())
    } else {
        None
    };
    finish(n, files, default_rf, data_rf, exe, vec![])
}

/// The `genquery` rule (`GenQuery.java`): the query is evaluated at analysis time and its output is the
/// content of the rule's single output file (`<name>`), written by a `FileWrite` action.
pub(crate) fn genquery(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let expression = n.string("expression")?;
    let opts = n.strings("opts")?;
    let strict = n.boolean("strict")?;
    let scope: Vec<String> = n
        .labels("scope")?
        .iter()
        .map(|l| l.canonical_form())
        .collect();
    let out_label = n.rule.label.clone();
    let out = n.out_artifact(&out_label);
    let Some(loader) = n.resolver.package_loader() else {
        return Err(n.err("expression", "genquery requires a package loader"));
    };
    let Some(hook) = crate::genquery_hook() else {
        return Err(n.err("expression", "genquery: no query engine is available"));
    };
    let compressed = n.boolean("compressed_output")?;
    let bytes =
        hook(&loader, &expression, &scope, strict, &opts).map_err(|e| n.err("expression", &e))?;
    let payload = if compressed {
        zel_util::deflate::gzip(&bytes)
    } else {
        bytes
    };
    let mut a = Action::new(
        n.rule.label.clone(),
        "FileWrite",
        ActionKind::FileWrite {
            content: FileContent::Bytes(payload),
            is_executable: false,
        },
    );
    a.outputs.push(out.clone());
    finish(
        n,
        vec![out],
        Runfiles::default(),
        Runfiles::default(),
        None,
        vec![a],
    )
}

pub(crate) fn alias(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let actual = match n.value("actual")? {
        Some(AttrValue::Label(Some(l))) => l,
        _ => {
            return Err(n.attr_err(
                "actual",
                "missing value for mandatory attribute 'actual' in 'alias' rule",
            ))
        }
    };
    let is_host_plat = n.rule.label.canonical_form() == "@bazel_tools//tools:host_platform"
        || n.rule.label.canonical_form() == "@@bazel_tools//tools:host_platform";
    let cfg = if is_host_plat {
        zel_analysis::BuildConfigurationKey::target(&n.cfg.cpu, "fastbuild").to_core_only()
    } else {
        n.cfg.clone()
    };
    let ct = n.resolver.configured_target(&actual, &cfg)?;
    n.resolver.check_dependency(n.rule, "actual", &ct)?;
    let mut alias_chain = vec![Value::Label(n.rule.label.clone())];
    let actual_label =
        if let Some(Value::ProviderInstance { fields, .. }) = ct.providers.get("AliasProvider") {
            if let Some(Value::List(l)) = fields.get("alias_chain") {
                alias_chain.extend(l.lock().unwrap().elements.iter().cloned());
            }
            match fields.get("actual_label") {
                Some(Value::Label(l)) => l.clone(),
                _ => ct.label.clone(),
            }
        } else {
            ct.label.clone()
        };
    let mut targets_chain = vec![actual];
    targets_chain.extend(ct.alias_targets.iter().cloned());
    let mut out = (*ct).clone();
    let mut chain = vec![n.rule.label.clone()];
    chain.extend(ct.alias_chain.iter().cloned());
    out.alias_chain = chain;
    out.alias_targets = targets_chain;
    out.actual_label = Some(ct.actual_target_label().clone());
    out.actual_rule_class = Some(ct.actual_rule_class_name().to_string());
    out.actual_testonly = Some(ct.actual_testonly.unwrap_or(ct.testonly));
    out.label = n.rule.label.clone();
    out.configuration = cfg;
    out.actions.clear();
    out.typed_actions.clear();
    out.is_test = false;
    out.rule_class = "alias".to_string();
    out.providers.insert(
        "AliasProvider".to_string(),
        provider_instance(
            "AliasProvider",
            vec![
                ("alias_chain", mk_list(alias_chain)),
                ("actual_label", Value::Label(actual_label)),
                ("original_label", Value::Label(n.rule.label.clone())),
            ],
        ),
    );
    Ok(out)
}

pub(crate) fn test_suite(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let mut vis_errors: Vec<String> = Vec::new();
    let mut data_rf = Runfiles::default();
    for t in n.labels("tests")? {
        let ct = n.resolver.configured_target(&t, n.cfg)?;
        // Every invisible test is reported (`RuleContext.reportError` per prerequisite).
        if let Err(e) = n.resolver.check_dependency(n.rule, "tests", &ct) {
            vis_errors.push(if vis_errors.is_empty() {
                e
            } else {
                format!("ERROR: {e}")
            });
            continue;
        }
        let eff_rc = ct
            .actual_rule_class
            .as_deref()
            .unwrap_or(ct.rule_class.as_str());
        if !ct.is_test && !eff_rc.ends_with("_test") && eff_rc != "test_suite" {
            return Err(format!(
                "in test_suite rule '{}': expecting a test or a test_suite rule but '{}' is not one.",
                n.rule.label.canonical_form(),
                t.canonical_form()
            ));
        }
        data_rf = data_rf.merge(&target_runfiles(&ct, true));
    }
    if !vis_errors.is_empty() {
        return Err(vis_errors.join("\n"));
    }
    finish(n, vec![], Runfiles::default(), data_rf, None, vec![])
}

pub(crate) fn label_flag(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let key = n.rule.label.canonical_form();
    let actual = match n.cfg.flag_values.get(&key) {
        Some(v) => {
            Label::parse_canonical(v).map_err(|e| n.err("build_setting_default", &e.to_string()))?
        }
        None => match n.value("build_setting_default")? {
            Some(AttrValue::Label(Some(l))) => l,
            _ => {
                let mut empty = finish(
                    n,
                    vec![],
                    Runfiles::default(),
                    Runfiles::default(),
                    None,
                    vec![],
                )?;
                empty.is_late_bound_alias = true;
                empty.providers.insert(
                    "BuildSettingProvider".into(),
                    provider_instance("BuildSettingProvider", vec![("value", Value::None)]),
                );
                return Ok(empty);
            }
        },
    };
    let ct = n.resolver.configured_target(&actual, n.cfg)?;
    n.resolver.check_dependency(n.rule, "actual", &ct)?;
    let mut targets_chain = vec![actual.clone()];
    targets_chain.extend(ct.alias_targets.iter().cloned());
    let mut out = (*ct).clone();
    let mut chain = vec![n.rule.label.clone()];
    chain.extend(ct.alias_chain.iter().cloned());
    out.alias_chain = chain;
    out.alias_targets = targets_chain;
    out.actual_label = Some(ct.actual_target_label().clone());
    out.actual_rule_class = Some(ct.actual_rule_class_name().to_string());
    out.actual_testonly = Some(ct.actual_testonly.unwrap_or(ct.testonly));
    out.is_late_bound_alias = true;
    out.label = n.rule.label.clone();
    out.actions.clear();
    out.typed_actions.clear();
    out.rule_class = "alias".to_string();
    out.providers.insert(
        "BuildSettingProvider".into(),
        provider_instance(
            "BuildSettingProvider",
            vec![("value", Value::Label(actual))],
        ),
    );
    Ok(out)
}

/// `JavaPluginsFlagAliasRule.JavaPluginsFlagAlias`: provides the plugins named by `--plugin`
/// as a (rules_java) `JavaPluginInfo` merged without Java outputs.
pub(crate) fn java_plugins_flag_alias(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let l = n.rule.label.canonical_form();
    if l != "@@bazel_tools//tools/jdk:java_plugins_flag_alias"
        && l != "//tools/jdk:java_plugins_flag_alias"
    {
        return Err(format!("Rule {l} cannot use private rule"));
    }
    let flag = n.cfg.options.get("plugin").cloned().unwrap_or_default();
    // `:java_plugins` (exec configuration): the labels of `--plugin`.
    let exec_cfg = n.cfg.to_exec_for(&n.acfg.default_exec_platform());
    const PLUGIN_INFO: &str = "@@rules_java+//java/private:java_info.bzl%JavaPluginInfo";
    let mut infos: Vec<zel_starlark::Value> = Vec::new();
    for raw in flag.split(',').filter(|s| !s.is_empty()) {
        let l = Label::parse_canonical(raw)
            .map_err(|e| format!("--plugin: invalid label '{raw}': {e}"))?;
        let ct = n.resolver.configured_target(&l, &exec_cfg)?;
        match ct.providers.get(PLUGIN_INFO) {
            Some(i) => infos.push(i.clone()),
            None => {
                return Err(format!(
                    "--plugin: '{raw}' does not have mandatory providers: 'JavaPluginInfo'"
                ))
            }
        }
    }
    // JavaPluginInfo.mergeWithoutJavaOutputs: union of the plugin data of all plugins.
    use zel_starlark::{Depset, DepsetOrder};
    let merged = |which: &str, field: &str| -> Value {
        let mut children: Vec<Arc<Depset>> = Vec::new();
        for i in &infos {
            if let Value::ProviderInstance { fields, .. } = i {
                if let Some(Value::ProviderInstance { fields: d, .. }) = fields.get(which) {
                    if let Some(Value::Depset(ds)) = d.get(field) {
                        children.push(ds.clone());
                    }
                }
            }
        }
        Value::Depset(Arc::new(
            Depset::new(DepsetOrder::Default, vec![], children).unwrap(),
        ))
    };
    let data_key = "@@rules_java+//java/private:java_info.bzl%JavaPluginDataInfo";
    let mk = |which: &str| {
        provider_instance(
            data_key,
            vec![
                ("processor_classes", merged(which, "processor_classes")),
                ("processor_jars", merged(which, "processor_jars")),
                ("processor_data", merged(which, "processor_data")),
            ],
        )
    };
    let info = provider_instance(
        PLUGIN_INFO,
        vec![
            ("plugins", mk("plugins")),
            ("api_generating_plugins", mk("api_generating_plugins")),
            ("java_outputs", zel_analysis::starlark_rule::mk_list(vec![])),
        ],
    );
    let mut ct = finish(
        n,
        vec![],
        Runfiles::default(),
        Runfiles::default(),
        None,
        vec![],
    )?;
    ct.providers.insert(
        "@@rules_java+//java/private:java_info.bzl%JavaPluginInfo".to_string(),
        info,
    );
    Ok(ct)
}

/// `CcLibcTopAlias`: an alias of `CppConfiguration.getLibcTopLabel()` (`--grte_top`); without a
/// label the target provides nothing.
pub(crate) fn cc_libc_top_alias(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let top = n
        .cfg
        .options
        .get("grte_top")
        .filter(|v| !v.is_empty())
        .cloned();
    match top {
        Some(l) => {
            let actual = Label::parse_canonical(&l).map_err(|e| e.to_string())?;
            let ct = n.resolver.configured_target(&actual, n.cfg)?;
            let mut out = (*ct).clone();
            let mut chain = vec![n.rule.label.clone()];
            chain.extend(ct.alias_chain.iter().cloned());
            out.alias_chain = chain;
            out.actual_label = Some(ct.actual_target_label().clone());
            out.actual_rule_class = Some(ct.actual_rule_class_name().to_string());
            out.actual_testonly = Some(ct.actual_testonly.unwrap_or(ct.testonly));
            out.is_late_bound_alias = true;
            out.label = n.rule.label.clone();
            Ok(out)
        }
        None => {
            let mut empty = finish(
                n,
                vec![],
                Runfiles::default(),
                Runfiles::default(),
                None,
                vec![],
            )?;
            empty.is_late_bound_alias = true;
            Ok(empty)
        }
    }
}

/// `ExtraAction`/`ActionListener`/`Environment`/`starlark_doc_extract`: analysed for their
/// dependencies (`tools`, `extra_actions`) only; they provide no files.
pub(crate) fn extra_action_like(n: &Nctx) -> Result<ConfiguredTarget, String> {
    match n.rule.rule_class.as_str() {
        "extra_action" => {
            n.deps("tools", true)?;
        }
        "action_listener" => {
            n.deps("extra_actions", false)?;
        }
        _ => {}
    }
    finish(
        n,
        vec![],
        Runfiles::default(),
        Runfiles::default(),
        None,
        vec![],
    )
}

pub(crate) fn label_setting(n: &Nctx) -> Result<ConfiguredTarget, String> {
    label_flag(n)
}

// ---------------------------------------------------------------------------------- genrule

fn singleton(arts: &[Artifact], var: &str, what: &str) -> Result<String, String> {
    match arts.len() {
        0 => Err(format!("variable '{var}' : no {what}")),
        1 => Ok(arts[0].exec_path.path_string().to_string()),
        _ => Err(format!("variable '{var}' : more than one {what}")),
    }
}

pub(crate) fn genrule(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let label = &n.rule.label;
    let out_labels = n.labels("outs")?;
    let outs: Vec<Artifact> = out_labels.iter().map(|l| n.out_artifact(l)).collect();
    if outs.is_empty() {
        return Err(n.attr_err("outs", "Genrules without outputs don't make sense"));
    }
    if n.boolean("executable")? && outs.len() != 1 {
        return Err(n.attr_err("executable", "if genrules produce executables, they are allowed only one output. If you need the executable=1 argument, then you should split this genrule into genrules producing single outputs"));
    }
    let cmd_attr = if n.explicit("cmd_bash") {
        "cmd_bash"
    } else if n.explicit("cmd") {
        "cmd"
    } else {
        return Err(n.attr_err(
            "cmd",
            "missing value for `cmd` attribute, you can also set `cmd_ps` or `cmd_bat` on Windows and `cmd_bash` on other platforms.",
        ));
    };
    let base_cmd = n.string(cmd_attr)?;

    // srcs (target configuration), tools (exec configuration), toolchains (target configuration).
    let srcs = n.deps("srcs", false)?;
    let mut tc_types: Vec<crate::toolchain::ToolchainTypeRequest> = Vec::new();
    let mut tc_make_vars: BTreeMap<String, String> = BTreeMap::new();
    let mut toolchain_tools: Vec<Arc<ConfiguredTarget>> = Vec::new();
    for l in n.labels("toolchains")? {
        let ct = n.resolver.configured_target(&l, n.cfg)?;
        n.resolver.check_dependency(n.rule, "toolchains", &ct)?;
        let has_tt = ct.providers.contains_key("ToolchainTypeInfo");
        let has_tv = ct.providers.contains_key("TemplateVariableInfo");
        if !has_tt && !has_tv {
            return Err(n.attr_err(
                "toolchains",
                &format!(
                    "'{}' does not have mandatory providers: 'TemplateVariableInfo' or 'ToolchainTypeInfo'",
                    l.canonical_form()
                ),
            ));
        }
        if has_tt {
            tc_types.push(crate::toolchain::ToolchainTypeRequest {
                toolchain_type: l.clone(),
                mandatory: true,
            });
        }
        if let Some(Value::ProviderInstance { fields, .. }) =
            ct.providers.get("TemplateVariableInfo")
        {
            if let Some(Value::Dict(d)) = fields.get("variables").or_else(|| fields.get("vars")) {
                for (k, v) in d.lock().unwrap().items() {
                    if let (Value::String(k), Value::String(v)) = (k, v) {
                        tc_make_vars.insert(k, v);
                    }
                }
            }
            toolchain_tools.push(ct.clone());
        }
    }
    let ecw = n.labels("exec_compatible_with")?;
    let mut selected_exec_platform = n.acfg.default_exec_platform();
    if !tc_types.is_empty() || !ecw.is_empty() {
        let tc = crate::resolve_ctx_toolchains(
            &n.rule.label,
            &tc_types,
            &ecw,
            n.cfg,
            n.acfg,
            n.resolver,
        )?;
        for d in &tc.deps {
            n.resolver.check_dependency(n.rule, "$toolchains", d)?;
        }
        selected_exec_platform = tc.exec_platform;
        tc_make_vars.extend(tc.make_variables);
        toolchain_tools.extend(tc.resolved_targets);
    }

    let exec_cfg = n.cfg.to_exec_for(&selected_exec_platform);
    let deps_in_exec = |attr_name: &str| -> Result<Vec<Arc<ConfiguredTarget>>, String> {
        n.labels(attr_name)?
            .iter()
            .map(|l| {
                let ct = n.resolver.configured_target(l, &exec_cfg)?;
                n.resolver.check_dependency(n.rule, attr_name, &ct)?;
                Ok(ct)
            })
            .collect()
    };
    let mut tools = deps_in_exec("tools")?;
    tools.extend(deps_in_exec("exec_tools")?);
    tools.extend(toolchain_tools);
    let mut label_map: BTreeMap<Label, Vec<Artifact>> = BTreeMap::new();
    for (l, out_art) in out_labels.iter().zip(&outs) {
        label_map
            .entry(l.clone())
            .or_default()
            .push(out_art.clone());
    }
    let mut resolved_srcs: Vec<Artifact> = Vec::new();
    for s in &srcs {
        label_map
            .entry(s.label.clone())
            .or_default()
            .extend(s.default_info.files.iter().cloned());
        for al in &s.alias_targets {
            label_map
                .entry(al.clone())
                .or_default()
                .extend(s.default_info.files.iter().cloned());
        }
        resolved_srcs.extend(s.default_info.files.iter().cloned());
    }
    let resolved_srcs = dedup(resolved_srcs);
    let mut tool_inputs: Vec<Artifact> = Vec::new();
    for t in &tools {
        tool_inputs.extend(t.default_info.files.iter().cloned());
        if let Some(e) = &t.default_info.executable {
            label_map
                .entry(t.label.clone())
                .or_default()
                .push(e.clone());
            for al in &t.alias_targets {
                label_map.entry(al.clone()).or_default().push(e.clone());
            }
            tool_inputs.push(e.clone());
            if let Some(rs) = &t.runfiles_support {
                tool_inputs.push(rs.manifest.clone());
                tool_inputs.push(rs.executable.clone());
                if let Some(rm) = &rs.repo_mapping_manifest {
                    tool_inputs.push(rm.clone());
                }
            } else {
                let rf = target_runfiles(t, false);
                tool_inputs.extend(rf.artifacts);
                tool_inputs.extend(rf.symlinks.values().cloned());
                tool_inputs.extend(rf.root_symlinks.values().cloned());
            }
        } else {
            label_map
                .entry(t.label.clone())
                .or_default()
                .extend(t.default_info.files.iter().cloned());
            for al in &t.alias_targets {
                label_map
                    .entry(al.clone())
                    .or_default()
                    .extend(t.default_info.files.iter().cloned());
            }
            let rf = target_runfiles(t, false);
            tool_inputs.extend(rf.artifacts);
            tool_inputs.extend(rf.symlinks.values().cloned());
            tool_inputs.extend(rf.root_symlinks.values().cloned());
        }
    }

    // 1. $(location ...) and friends.
    let owner_repo = label.repository().clone();
    let rm_entries = n
        .resolver
        .repo_mapping_entries(owner_repo.name())
        .into_iter()
        .map(|(a, c)| (a, zel_cmdline::RepositoryName::create_unvalidated(c)))
        .collect();
    let repo_mapping = zel_cmdline::RepositoryMapping::new(rm_entries, owner_repo);
    let located = zel_analysis::location::expand_locations_with_mapping(
        &base_cmd,
        &label_map,
        label,
        LocationMode::Exec,
        "_main",
        Some(&repo_mapping),
    )
    .map_err(|e| n.attr_err(cmd_attr, &e))?;

    // 2. Make variables.
    let sibling_layout = n.sibling_layout();
    let bin_dir = format!("bazel-out/{}/bin", n.cfg.mnemonic());
    let pkg = label.package_identifier().package_fragment().clone();
    // Targets of external repositories output below `<bin>/external/<repo>/` unless
    // `--experimental_sibling_repository_layout` places them under `bazel-out/<repo>/<cfg>/bin/`.
    let repo_bin_dir = if sibling_layout {
        Artifact::bin_dir_for_repo(
            &n.cfg.mnemonic(),
            label.package_identifier().repository(),
            true,
        )
    } else if label.package_identifier().repository().is_main() {
        bin_dir.clone()
    } else {
        format!(
            "{bin_dir}/external/{}",
            label.package_identifier().repository().name()
        )
    };
    let rule_dir = if pkg.path_string().is_empty() {
        repo_bin_dir.clone()
    } else {
        format!("{repo_bin_dir}/{}", pkg.path_string())
    };
    let lookup = |name: &str| -> Option<String> {
        match name {
            "SRCS" => Some(
                resolved_srcs
                    .iter()
                    .map(|a| a.exec_path.path_string().to_string())
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            "OUTS" => Some(
                outs.iter()
                    .map(|a| a.exec_path.path_string().to_string())
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            "<" => singleton(&resolved_srcs, "$<", "input file")
                .ok()
                .or(Some(String::new())),
            "@" => singleton(&outs, "$@", "output file")
                .ok()
                .or(Some(String::new())),
            "RULEDIR" => Some(rule_dir.clone()),
            "@D" => Some(if outs.len() == 1 {
                outs[0]
                    .exec_path
                    .parent_directory()
                    .map(|p| p.path_string().to_string())
                    .unwrap_or_default()
            } else {
                rule_dir.clone()
            }),
            other => n
                .cfg
                .user_defines
                .get(other)
                .cloned()
                .or_else(|| tc_make_vars.get(other).cloned())
                .or_else(|| match other {
                    "BINDIR" | "GENDIR" => Some(bin_dir.clone()),
                    "COMPILATION_MODE" => Some(n.cfg.compilation_mode.clone()),
                    "TARGET_CPU" => Some(n.cfg.cpu.clone()),
                    _ => None,
                }),
        }
    };
    // `$<` / `$@` must report Bazel's singleton errors when used.
    for (var, arts, what) in [
        ("$<", &resolved_srcs, "input file"),
        ("$@", &outs, "output file"),
    ] {
        let marker = var;
        let used = located.replace("$$", "").contains(marker)
            || (var == "$<" && located.replace("$$", "").contains("$(<)"))
            || (var == "$@" && located.replace("$$", "").contains("$(@)"));
        if used {
            singleton(arts, var, what).map_err(|e| n.attr_err(cmd_attr, &e))?;
        }
    }
    let command = expand_make_variables(&located, &lookup).map_err(|e| n.attr_err(cmd_attr, &e))?;
    let setup_label = Label::parse_canonical("@bazel_tools//tools/genrule:genrule-setup.sh")
        .unwrap_or_else(|_| {
            Label::parse_canonical("@@bazel_tools//tools/genrule:genrule-setup.sh").unwrap()
        });
    let setup_art = Artifact::source_with_layout(
        zel_vfs::PathFragment::create("tools/genrule/genrule-setup.sh"),
        setup_label.clone(),
        sibling_layout,
    );
    // Genrule environment setup (`GenRuleBase.java`): `source <$genrule_setup execPath>; ...`
    let command = format!("source {}; {command}", setup_art.exec_path);

    let message = n.string("message")?;
    let mut exec_info = BTreeMap::new();
    if n.boolean("local")? {
        exec_info.insert("local".to_string(), String::new());
    }
    let allow_tags = n
        .cfg
        .options
        .get("incompatible_allow_tags_propagation")
        .map(|v| v != "false")
        .unwrap_or(true);
    if allow_tags {
        for t in n.strings("tags")? {
            if t.starts_with("requires-")
                || t.starts_with("cpu:")
                || t.starts_with("resources:")
                || matches!(
                    t.as_str(),
                    "no-sandbox"
                        | "no-cache"
                        | "no-remote"
                        | "no-remote-cache"
                        | "no-remote-exec"
                        | "no-local"
                        | "local"
                        | "block-network"
                        | "supports-workers"
                        | "supports-multiplex-workers"
                        | "supports-graceful-termination"
                        | "supports-path-mapping"
                )
            {
                exec_info.entry(t).or_insert_with(String::new);
            }
        }
    }
    let mut action = Action::new(
        label.clone(),
        "Genrule",
        ActionKind::Spawn {
            executable: n
                .acfg
                .shell_executable
                .clone()
                .unwrap_or_else(|| "/bin/bash".to_string()),
            arguments: vec![CommandLineArgs {
                args: vec!["-c".to_string(), command.clone()],
                param_file: None,
            }],
            shell_command: Some(command),
        },
    );
    if !message.is_empty() {
        action.progress_message = Some(format!("{message} {label}"));
    }
    action.execution_requirements = exec_info;
    if let Ok(setup_ct) = n.resolver.configured_target(&setup_label, n.cfg) {
        let _ = n
            .resolver
            .check_dependency(n.rule, "$genrule_setup", &setup_ct);
    }
    let mut inputs = resolved_srcs.clone();
    inputs.push(setup_art);
    inputs.extend(dedup(tool_inputs.clone()));
    let should_stamp = match n.value("stamp")? {
        Some(AttrValue::Bool(b)) => b,
        Some(AttrValue::Tristate(1)) | Some(AttrValue::Int(1)) => true,
        Some(AttrValue::Tristate(0)) | Some(AttrValue::Int(0)) => false,
        Some(AttrValue::Tristate(-1)) | Some(AttrValue::Int(-1)) => {
            !n.cfg.is_exec && n.cfg.bool_option("stamp", false)
        }
        _ => false,
    };
    if should_stamp {
        for status_file in ["stable-status.txt", "volatile-status.txt"] {
            inputs.push(Artifact {
                is_source: false,
                is_tree_artifact: false,
                root: ArtifactRoot::Bin,
                is_unresolved_symlink: false,
                is_constant_metadata: status_file == "volatile-status.txt",
                exec_path: PathFragment::create(&format!("bazel-out/{status_file}")),
                root_relative_path: PathFragment::create(status_file),
                owner_label: label.clone(),
            });
        }
    }
    action.inputs = dedup(inputs);
    action.tools = dedup(tool_inputs);
    action.outputs = outs.clone();
    action.owner_kind = "genrule".to_string();
    action.exec_platform = Some(selected_exec_platform);
    action.use_default_shell_env = true;
    action.owner_config = n.cfg.short_hash();

    let executable = if n.boolean("executable")? && outs.len() == 1 {
        Some(outs[0].clone())
    } else {
        None
    };
    let data_rf = Runfiles {
        artifacts: outs.clone(),
        ..Default::default()
    };
    finish(
        n,
        outs,
        Runfiles::default(),
        data_rf,
        executable,
        vec![action],
    )
}

// ---------------------------------------------------------------------------------- misc

/// A `FileWrite` helper for rules that synthesize a text file.
#[allow(dead_code)]
pub(crate) fn write_action(n: &Nctx, out: Artifact, text: &str) -> Action {
    let mut a = Action::new(
        n.rule.label.clone(),
        "FileWrite",
        ActionKind::FileWrite {
            content: FileContent::Literal(text.to_string()),
            is_executable: false,
        },
    );
    a.outputs.push(out);
    a
}

#[allow(dead_code)]
fn _unused(_: PathFragment, _: Value) {}
