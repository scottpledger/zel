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
use zel_analysis::location::{expand_locations, expand_make_variables, LocationMode};
use zel_analysis::starlark_rule::{
    default_info_provider, provider_instance, target_runfiles, DepResolver,
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

impl<'a> Nctx<'a> {
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
        let cfg = if exec { self.cfg.to_exec_for(&self.acfg.default_exec_platform()) } else { self.cfg.clone() };
        self.labels(name)?
            .iter()
            .map(|l| {
                let ct = self.resolver.configured_target(l, &cfg)?;
                self.resolver.check_dependency(self.rule, name, &ct)?;
                Ok(ct)
            })
            .collect()
    }

    pub fn out_artifact(&self, l: &Label) -> Artifact {
        Artifact::derived_in_root(
            &self.cfg.mnemonic(),
            ArtifactRoot::Bin,
            l.package_identifier().package_fragment().get_relative_str(l.name()),
            self.rule.label.clone(),
        )
    }

    fn err(&self, attr: &str, msg: &str) -> String {
        let loc = self.rule.extra.location.clone().unwrap_or_default();
        let prefix = if loc.is_empty() { String::new() } else { format!("{loc}: ") };
        let _ = attr;
        format!("{prefix}in {} rule {}: {msg}", self.rule.rule_class, self.rule.label.canonical_form())
    }

    /// `RuleContext.attributeError`: `in <attr> attribute of <rule class> rule <label>: <msg>`.
    fn attr_err(&self, attr: &str, msg: &str) -> String {
        let loc = self.rule.extra.location.clone().unwrap_or_default();
        let prefix = if loc.is_empty() { String::new() } else { format!("{loc}: ") };
        format!("{prefix}in {attr} attribute of {} rule {}: {msg}", self.rule.rule_class, self.rule.label.canonical_form())
    }
}

fn dedup(files: Vec<Artifact>) -> Vec<Artifact> {
    let mut seen = std::collections::BTreeSet::new();
    files.into_iter().filter(|a| seen.insert(a.exec_path.path_string().to_string())).collect()
}

fn finish(
    n: &Nctx,
    files: Vec<Artifact>,
    default_rf: Runfiles,
    data_rf: Runfiles,
    executable: Option<Artifact>,
    actions: Vec<Action>,
) -> Result<ConfiguredTarget, String> {
    let mut ct = ConfiguredTarget::new(n.rule.label.clone(), n.cfg.clone(), n.rule.rule_class.clone());
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
    ct.providers
        .insert("OutputGroupInfo".to_string(), provider_instance("OutputGroupInfo", vec![]));
    let mut groups = BTreeMap::new();
    groups.insert("default".to_string(), files.clone());
    if executable.is_some() || n.rule.is_binary_rule() || n.rule.is_test_rule() {
        groups.insert("compilation_outputs".to_string(), files);
    }
    ct.output_groups = OutputGroupInfo { groups };
    // `SpawnAction.createActionEnvironment` against the owner's configuration.
    let mut actions = actions;
    for a in &mut actions {
        a.apply_configuration_env(&n.cfg);
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
    let mut files = Vec::new();
    for s in &srcs {
        if group.is_empty() {
            files.extend(s.default_info.files.iter().cloned());
        } else {
            match s.output_groups.groups.get(&group) {
                Some(g) => files.extend(g.iter().cloned()),
                None => {
                    return Err(n.err(
                        "output_group",
                        &format!(
                            "Output group '{group}' not found in dependency '{}'",
                            s.label.canonical_form()
                        ),
                    ))
                }
            }
        }
    }
    let files = dedup(files);
    // `Filegroup.java`: default runfiles = `RunfilesSupport.addRunfiles(DEFAULT_RUNFILES)`; data
    // runfiles = `filesToBuild` + `addRunfiles(DATA_RUNFILES)` (`incompatible_filegroup_runfiles_for_data`).
    // `data` deps contribute their data runfiles *and* their files (`alwaysIncludeFilesToBuildInData`).
    let mut default_rf = Runfiles::default();
    let mut data_rf = Runfiles { artifacts: files.clone(), ..Default::default() };
    for d in &data {
        let r = target_runfiles(d, true).merge(&Runfiles { artifacts: d.default_info.files.clone(), ..Default::default() });
        default_rf = default_rf.merge(&r);
        data_rf = data_rf.merge(&r);
    }
    for s in &srcs {
        default_rf = default_rf.merge(&target_runfiles(s, false));
        data_rf = data_rf.merge(&target_runfiles(s, true));
    }
    // `Filegroup.getExecutable`: the single file of the group, if there is exactly one.
    let exe = if files.len() == 1 { Some(files[0].clone()) } else { None };
    finish(n, files, default_rf, data_rf, exe, vec![])
}

/// The `genquery` rule (`GenQuery.java`): the query is evaluated at analysis time and its output is the
/// content of the rule's single output file (`<name>`), written by a `FileWrite` action.
pub(crate) fn genquery(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let expression = n.string("expression")?;
    let opts = n.strings("opts")?;
    let strict = n.boolean("strict")?;
    let scope: Vec<String> = n.labels("scope")?.iter().map(|l| l.canonical_form()).collect();
    let out_label = n.rule.label.clone();
    let out = n.out_artifact(&out_label);
    let Some(loader) = n.resolver.package_loader() else {
        return Err(n.err("expression", "genquery requires a package loader"));
    };
    let Some(hook) = crate::genquery_hook() else {
        return Err(n.err("expression", "genquery: no query engine is available"));
    };
    let bytes = hook(&loader, &expression, &scope, strict, &opts).map_err(|e| n.err("expression", &e))?;
    let mut a = Action::new(
        n.rule.label.clone(),
        "FileWrite",
        ActionKind::FileWrite {
            content: FileContent::Literal(String::from_utf8_lossy(&bytes).into_owned()),
            is_executable: false,
        },
    );
    a.outputs.push(out.clone());
    finish(n, vec![out], Runfiles::default(), Runfiles::default(), None, vec![a])
}

pub(crate) fn alias(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let actual = match n.value("actual")? {
        Some(AttrValue::Label(Some(l))) => l,
        _ => return Err(n.attr_err("actual", "missing value for mandatory attribute 'actual' in 'alias' rule")),
    };
    let is_host_plat = n.rule.label.canonical_form() == "@bazel_tools//tools:host_platform"
        || n.rule.label.canonical_form() == "@@bazel_tools//tools:host_platform";
    let cfg = if is_host_plat {
        zel_analysis::BuildConfigurationKey::target(&n.cfg.cpu, "fastbuild").to_core_only()
    } else {
        n.cfg.clone()
    };
    let ct = n.resolver.configured_target(&actual, &cfg)?;
    let _ = n.resolver.check_dependency(n.rule, "actual", &ct);
    let mut out = (*ct).clone();
    out.label = n.rule.label.clone();
    out.configuration = cfg;
    out.actions.clear();
    out.typed_actions.clear();
    out.rule_class = "alias".to_string();
    Ok(out)
}

pub(crate) fn test_suite(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let mut vis_errors: Vec<String> = Vec::new();
    for t in n.labels("tests")? {
        let ct = n.resolver.configured_target(&t, n.cfg)?;
        // Every invisible test is reported (`RuleContext.reportError` per prerequisite).
        if let Err(e) = n.resolver.check_dependency(n.rule, "tests", &ct) {
            vis_errors.push(if vis_errors.is_empty() { e } else { format!("ERROR: {e}") });
            continue;
        }
        if !ct.rule_class.ends_with("_test") && ct.rule_class != "test_suite" {
            return Err(format!(
                "in test_suite rule '{}': expecting a test or a test_suite rule but '{}' is not one.",
                n.rule.label.canonical_form(),
                t.canonical_form()
            ));
        }
    }
    if !vis_errors.is_empty() {
        return Err(vis_errors.join("\n"));
    }
    finish(n, vec![], Runfiles::default(), Runfiles::default(), None, vec![])
}

pub(crate) fn label_flag(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let key = n.rule.label.canonical_form();
    let actual = match n.cfg.flag_values.get(&key) {
        Some(v) => Label::parse_canonical(v).map_err(|e| n.err("build_setting_default", &e.to_string()))?,
        None => match n.value("build_setting_default")? {
            Some(AttrValue::Label(Some(l))) => l,
            _ => {
                return finish(n, vec![], Runfiles::default(), Runfiles::default(), None, vec![]);
            }
        },
    };
    let ct = n.resolver.configured_target(&actual, n.cfg)?;
    let _ = n.resolver.check_dependency(n.rule, "actual", &ct);
    let mut out = (*ct).clone();
    out.label = n.rule.label.clone();
    out.actions.clear();
    out.typed_actions.clear();
    out.rule_class = "alias".to_string();
    Ok(out)
}

/// `JavaPluginsFlagAliasRule.JavaPluginsFlagAlias`: provides the plugins named by `--plugin`
/// as a (rules_java) `JavaPluginInfo` merged without Java outputs.
pub(crate) fn java_plugins_flag_alias(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let l = n.rule.label.canonical_form();
    if l != "@@bazel_tools//tools/jdk:java_plugins_flag_alias" && l != "//tools/jdk:java_plugins_flag_alias" {
        return Err(format!("Rule {l} cannot use private rule"));
    }
    let flag = n.cfg.options.get("plugin").cloned().unwrap_or_default();
    // `:java_plugins` (exec configuration): the labels of `--plugin`.
    let exec_cfg = n.cfg.to_exec_for(&n.acfg.default_exec_platform());
    const PLUGIN_INFO: &str = "@@rules_java+//java/private:java_info.bzl%JavaPluginInfo";
    let mut infos: Vec<zel_starlark::Value> = Vec::new();
    for raw in flag.split(',').filter(|s| !s.is_empty()) {
        let l = Label::parse_canonical(raw).map_err(|e| format!("--plugin: invalid label '{raw}': {e}"))?;
        let ct = n.resolver.configured_target(&l, &exec_cfg)?;
        match ct.providers.get(PLUGIN_INFO) {
            Some(i) => infos.push(i.clone()),
            None => return Err(format!("--plugin: '{raw}' does not have mandatory providers: 'JavaPluginInfo'")),
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
        Value::Depset(Arc::new(Depset::new(DepsetOrder::Default, vec![], children).unwrap()))
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
    let mut ct = finish(n, vec![], Runfiles::default(), Runfiles::default(), None, vec![])?;
    ct.providers.insert("@@rules_java+//java/private:java_info.bzl%JavaPluginInfo".to_string(), info);
    Ok(ct)
}

/// `CcLibcTopAlias`: an alias of `CppConfiguration.getLibcTopLabel()` (`--grte_top`); without a
/// label the target provides nothing.
pub(crate) fn cc_libc_top_alias(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let top = n.cfg.options.get("grte_top").filter(|v| !v.is_empty()).cloned();
    match top {
        Some(l) => {
            let actual = Label::parse_canonical(&l).map_err(|e| e.to_string())?;
            let ct = n.resolver.configured_target(&actual, n.cfg)?;
            let mut out = (*ct).clone();
            out.label = n.rule.label.clone();
            Ok(out)
        }
        None => finish(n, vec![], Runfiles::default(), Runfiles::default(), None, vec![]),
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
    finish(n, vec![], Runfiles::default(), Runfiles::default(), None, vec![])
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

    // srcs (target configuration), tools (exec configuration).
    let srcs = n.deps("srcs", false)?;
    let mut tools = n.deps("tools", true)?;
    tools.extend(n.deps("exec_tools", true)?);
    let mut label_map: BTreeMap<Label, Vec<Artifact>> = BTreeMap::new();
    for (l, out_art) in out_labels.iter().zip(&outs) {
        label_map.entry(l.clone()).or_default().push(out_art.clone());
    }
    let mut resolved_srcs: Vec<Artifact> = Vec::new();
    for s in &srcs {
        label_map.entry(s.label.clone()).or_default().extend(s.default_info.files.iter().cloned());
        resolved_srcs.extend(s.default_info.files.iter().cloned());
    }
    let resolved_srcs = dedup(resolved_srcs);
    let mut tool_inputs: Vec<Artifact> = Vec::new();
    for t in &tools {
        let files = match &t.default_info.executable {
            Some(e) => vec![e.clone()],
            None => t.default_info.files.clone(),
        };
        label_map.entry(t.label.clone()).or_default().extend(files.iter().cloned());
        tool_inputs.extend(t.default_info.files.iter().cloned());
        tool_inputs.extend(target_runfiles(t, false).artifacts);
    }

    // 1. $(location ...) and friends.
    let located = expand_locations(&base_cmd, &label_map, label, LocationMode::Exec, "_main")
        .map_err(|e| n.attr_err(cmd_attr, &e))?;

    // 2. Make variables.
    let bin_dir = format!("bazel-out/{}/bin", n.cfg.mnemonic());
    let pkg = label.package_identifier().package_fragment().clone();
    // Targets of external repositories output below `<bin>/external/<repo>/`.
    let repo_bin_dir = if label.package_identifier().repository().is_main() {
        bin_dir.clone()
    } else {
        format!("{bin_dir}/external/{}", label.package_identifier().repository().name())
    };
    let rule_dir = if pkg.path_string().is_empty() {
        repo_bin_dir.clone()
    } else {
        format!("{repo_bin_dir}/{}", pkg.path_string())
    };
    let lookup = |name: &str| -> Option<String> {
        match name {
            "SRCS" => Some(
                resolved_srcs.iter().map(|a| a.exec_path.path_string().to_string()).collect::<Vec<_>>().join(" "),
            ),
            "OUTS" => Some(outs.iter().map(|a| a.exec_path.path_string().to_string()).collect::<Vec<_>>().join(" ")),
            "<" => singleton(&resolved_srcs, "$<", "input file").ok().or(Some(String::new())),
            "@" => singleton(&outs, "$@", "output file").ok().or(Some(String::new())),
            "RULEDIR" => Some(rule_dir.clone()),
            "@D" => Some(if outs.len() == 1 {
                outs[0].exec_path.parent_directory().map(|p| p.path_string().to_string()).unwrap_or_default()
            } else {
                rule_dir.clone()
            }),
            "BINDIR" | "GENDIR" => Some(bin_dir.clone()),
            "COMPILATION_MODE" => Some(n.cfg.compilation_mode.clone()),
            "TARGET_CPU" => Some(n.cfg.cpu.clone()),
            _ => None,
        }
    };
    // `$<` / `$@` must report Bazel's singleton errors when used.
    for (var, arts, what) in [("$<", &resolved_srcs, "input file"), ("$@", &outs, "output file")] {
        let marker = var;
        let used = located.replace("$$", "").contains(marker)
            || (var == "$<" && located.replace("$$", "").contains("$(<)"))
            || (var == "$@" && located.replace("$$", "").contains("$(@)"));
        if used {
            singleton(arts, var, what).map_err(|e| n.attr_err(cmd_attr, &e))?;
        }
    }
    let command = expand_make_variables(&located, &lookup).map_err(|e| n.attr_err(cmd_attr, &e))?;
    // Genrule environment setup (`tools/genrule/genrule-setup.sh`): `source external/bazel_tools/tools/genrule/genrule-setup.sh; ...`
    let command = format!("source external/bazel_tools/tools/genrule/genrule-setup.sh; {command}");

    let message = n.string("message")?;
    let mut exec_info = BTreeMap::new();
    if n.boolean("local")? {
        exec_info.insert("local".to_string(), String::new());
    }
    for t in n.strings("tags")? {
        if let Some(rest) = t.strip_prefix("requires-") {
            let _ = rest;
            exec_info.insert(t.clone(), String::new());
        } else if t == "no-sandbox" || t == "no-cache" || t == "no-remote" || t == "local" || t == "block-network" {
            exec_info.insert(t.clone(), String::new());
        }
    }
    let mut action = Action::new(
        label.clone(),
        "Genrule",
        ActionKind::Spawn {
            executable: n.acfg.shell_executable.clone().unwrap_or_else(|| "/bin/bash".to_string()),
            arguments: vec![CommandLineArgs { args: vec!["-c".to_string(), command.clone()], param_file: None }],
            shell_command: Some(command),
        },
    );
    if !message.is_empty() {
        action.progress_message = Some(format!("{message} {label}"));
    }
    action.execution_requirements = exec_info;
    let setup_label = Label::parse_canonical("@bazel_tools//tools/genrule:genrule-setup.sh")
        .unwrap_or_else(|_| Label::parse_canonical("@@bazel_tools//tools/genrule:genrule-setup.sh").unwrap());
    if let Ok(setup_ct) = n.resolver.configured_target(&setup_label, n.cfg) {
        let _ = n.resolver.check_dependency(n.rule, "$genrule_setup", &setup_ct);
    }
    let setup_art = Artifact::source(
        zel_vfs::PathFragment::create("external/bazel_tools/tools/genrule/genrule-setup.sh"),
        setup_label,
    );
    let mut inputs = resolved_srcs.clone();
    inputs.push(setup_art);
    inputs.extend(dedup(tool_inputs.clone()));
    action.inputs = dedup(inputs);
    action.tools = dedup(tool_inputs);
    action.outputs = outs.clone();
    action.owner_kind = "genrule".to_string();
    action.use_default_shell_env = true;
    action.owner_config = n.cfg.short_hash();

    let executable = if n.boolean("executable")? && outs.len() == 1 { Some(outs[0].clone()) } else { None };
    let data_rf = Runfiles { artifacts: outs.clone(), ..Default::default() };
    finish(n, outs, Runfiles::default(), data_rf, executable, vec![action])
}

// ---------------------------------------------------------------------------------- misc

/// A `FileWrite` helper for rules that synthesize a text file.
#[allow(dead_code)]
pub(crate) fn write_action(n: &Nctx, out: Artifact, text: &str) -> Action {
    let mut a = Action::new(
        n.rule.label.clone(),
        "FileWrite",
        ActionKind::FileWrite { content: FileContent::Literal(text.to_string()), is_executable: false },
    );
    a.outputs.push(out);
    a
}

#[allow(dead_code)]
fn _unused(_: PathFragment, _: Value) {}
