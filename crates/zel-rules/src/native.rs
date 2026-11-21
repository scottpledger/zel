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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use zel_analysis::location::{expand_make_variables, LocationMode};
use zel_analysis::starlark_rule::{
    default_info_provider, files_depset, mk_list, provider_instance, target_runfiles, DepResolver,
};
use zel_analysis::{
    Action, ActionKind, Artifact, ArtifactRoot, BuildConfigurationKey, CommandLineArgs,
    ConfiguredTarget, DefaultInfo, FileContent, OutputGroupInfo, Runfiles,
};
use zel_cmdline::{Label, RepositoryName};
use zel_packages::attr::AttrValue;
use zel_packages::{RepoResolver, RuleInstance};
use zel_proto::dynamic::Msg;
use zel_starlark::syntax::{
    AssignOp, AssignTarget, BinaryOpKind, Expr, Lexer, ParamDef, Parser, Pattern, Stmt,
    UnaryOpKind, UserFunctionDef,
};
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
    if executable.is_some()
        || n.rule.is_binary_rule()
        || (n.rule.is_test_rule() && n.rule.rule_class != "test_suite")
    {
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
    ct.is_test = n.rule.is_test_rule() && n.rule.rule_class != "test_suite";
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
        } else if let Some(g) = s
            .output_groups
            .groups
            .get(&group)
            .or_else(|| s.get_actual().output_groups.groups.get(&group))
        {
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
    let mut ct = finish(n, files.clone(), default_rf, data_rf, exe, vec![])?;
    let mut inst_files = Vec::new();
    let mut meta_files = Vec::new();
    let mut cov_env = BTreeMap::new();
    let mut sup_files = Vec::new();
    if n.cfg.bool_option("collect_code_coverage", false) {
        let include_gen = n.cfg.bool_option(
            "experimental_collect_code_coverage_for_generated_files",
            false,
        );
        for f in &files {
            if f.is_source || include_gen {
                inst_files.push(f.clone());
            }
        }
        for d in srcs.iter().chain(data.iter()) {
            if let Some(i) = &d.instrumented_files {
                inst_files.extend(i.instrumented_files.iter().cloned());
                meta_files.extend(i.metadata_files.iter().cloned());
                cov_env.extend(i.environment.iter().map(|(k, v)| (k.clone(), v.clone())));
                sup_files.extend(i.support_files.iter().cloned());
            }
        }
    }
    let inst_files = dedup(inst_files);
    let meta_files = dedup(meta_files);
    let sup_files = dedup(sup_files);
    ct.instrumented_files = Some(zel_analysis::InstrumentedFiles {
        instrumented_files: inst_files,
        metadata_files: meta_files,
        environment: cov_env,
        support_files: sup_files,
    });
    Ok(ct)
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
    out.actual = Some(ct.clone());
    out.actual_label = Some(ct.actual_target_label().clone());
    out.actual_rule_class = Some(ct.actual_rule_class_name().to_string());
    out.actual_testonly = Some(ct.actual_testonly.unwrap_or(ct.testonly));
    out.label = n.rule.label.clone();
    // `AliasConfiguredTarget.getConfigurationKey` keeps the alias lookup key's configuration,
    // even when `host_platform` resolves `actual` in the core-only configuration.
    out.configuration = n.cfg.clone();
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
    out.providers.insert(
        "VisibilityProvider".to_string(),
        provider_instance(
            "VisibilityProvider",
            vec![
                (
                    "visibility",
                    mk_list(
                        n.rule
                            .extra
                            .effective_visibility
                            .iter()
                            .cloned()
                            .map(Value::Label)
                            .collect(),
                    ),
                ),
                (
                    "is_created_in_symbolic_macro",
                    Value::Bool(n.rule.extra.macro_instance.is_some()),
                ),
            ],
        ),
    );
    Ok(out)
}

pub(crate) fn test_suite(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let mut vis_errors: Vec<String> = Vec::new();
    let mut data_rf = Runfiles::default();
    let explicit_tests = n.labels("tests")?;
    let test_labels = if explicit_tests.is_empty() {
        n.labels("$implicit_tests").unwrap_or_default()
    } else {
        explicit_tests
    };
    let suite_tags = n.strings("tags")?;
    let matches_suite_tags = |test_tags: &[String]| -> bool {
        suite_tags
            .iter()
            .filter(|t| t.as_str() != "manual")
            .all(|t| match t.strip_prefix('-') {
                Some(ex) => !test_tags.iter().any(|x| x == ex),
                None => {
                    let req = t.strip_prefix('+').unwrap_or(t);
                    test_tags.iter().any(|x| x == req)
                }
            })
    };
    let mut transitive_tests: Vec<Value> = Vec::new();
    for t in test_labels {
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
        let actual = ct.get_actual();
        let eff_rc = actual
            .actual_rule_class
            .as_deref()
            .unwrap_or(actual.rule_class.as_str());
        let is_nested_suite =
            eff_rc == "test_suite" || actual.providers.contains_key("TransitiveTestsInfo");
        let is_test = actual.is_test || eff_rc.ends_with("_test");
        if !is_test && !is_nested_suite {
            return Err(format!(
                "in test_suite rule '{}': expecting a test or a test_suite rule but '{}' is not one.",
                n.rule.label.canonical_form(),
                t.canonical_form()
            ));
        }
        if !is_nested_suite {
            let mut dep_tags: Vec<String> = Vec::new();
            if let Ok(Some(ri)) = n.resolver.rule_instance(&actual.label) {
                dep_tags = ri.rule_tags();
                if let Some(AttrValue::Str(sz)) = ri.attr_or_default("size") {
                    if !sz.is_empty() && !dep_tags.contains(&sz) {
                        dep_tags.push(sz);
                    }
                }
            }
            if !dep_tags.is_empty() && !matches_suite_tags(&dep_tags) {
                continue;
            }
            transitive_tests.push(Value::Label(actual.label.clone()));
        } else if let Some(Value::ProviderInstance { fields, .. }) =
            actual.providers.get("TransitiveTestsInfo")
        {
            if let Some(Value::List(l)) = fields.get("tests") {
                transitive_tests.extend(l.lock().unwrap().elements.iter().cloned());
            }
        }
        let dep_rf = target_runfiles(&ct, true).merge(&Runfiles {
            artifacts: ct.default_info.files.clone(),
            ..Default::default()
        });
        data_rf = data_rf.merge(&dep_rf);
    }
    if !vis_errors.is_empty() {
        return Err(vis_errors.join("\n"));
    }
    let mut out = finish(n, vec![], Runfiles::default(), data_rf, None, vec![])?;
    out.providers.insert(
        "TransitiveTestsInfo".to_string(),
        provider_instance(
            "TransitiveTestsInfo",
            vec![("tests", mk_list(transitive_tests))],
        ),
    );
    Ok(out)
}

pub(crate) fn label_flag(n: &Nctx) -> Result<ConfiguredTarget, String> {
    label_build_setting(n, true)
}

fn parse_libc_top_label(raw: &str) -> Result<Option<Label>, String> {
    if raw.is_empty() || raw == "default" {
        return Ok(None);
    }
    if !raw.starts_with("//") && !raw.starts_with('@') {
        return Err(format!(
            "Invalid --grte_top value '{raw}': expected a label starting with '//' or '@'"
        ));
    }
    let parsed = Label::parse_canonical(raw)
        .map_err(|e| format!("Invalid --grte_top value '{raw}': {e}"))?;
    let everything = Label::create(parsed.package_identifier().clone(), "everything")
        .map_err(|e| e.to_string())?;
    Ok(Some(everything))
}

fn label_build_setting(n: &Nctx, is_flag: bool) -> Result<ConfiguredTarget, String> {
    let default_lbl = match n.value("build_setting_default")? {
        Some(AttrValue::Label(Some(l))) => l,
        _ => {
            return Err(n.err(
                "build_setting_default",
                &format!(
                    "missing value for mandatory attribute 'build_setting_default' in '{}' rule",
                    n.rule.rule_class
                ),
            ))
        }
    };
    let key = n.rule.label.canonical_form();
    let actual = match n.cfg.flag_values.get(&key) {
        Some(v) => {
            let pkg = n.rule.label.package_identifier();
            let entries = n
                .resolver
                .repo_mapping_entries(pkg.repository().name())
                .into_iter()
                .map(|(a, c)| (a, zel_cmdline::RepositoryName::create_unvalidated(c)))
                .collect();
            let mapping = zel_cmdline::RepositoryMapping::new(entries, pkg.repository().clone());
            Label::parse_with_package_context(v, pkg, &mapping)
                .map_err(|e| n.err("build_setting_default", &e.to_string()))?
        }
        None => default_lbl.clone(),
    };
    let ct = n.resolver.configured_target(&actual, n.cfg)?;
    n.resolver.check_dependency(n.rule, ":alias", &ct)?;
    let mut targets_chain = vec![actual.clone()];
    targets_chain.extend(ct.alias_targets.iter().cloned());
    let mut out = (*ct).clone();
    let mut chain = vec![n.rule.label.clone()];
    chain.extend(ct.alias_chain.iter().cloned());
    out.alias_chain = chain;
    out.alias_targets = targets_chain;
    out.actual = Some(ct.clone());
    out.actual_label = Some(ct.actual_target_label().clone());
    out.actual_rule_class = Some(ct.actual_rule_class_name().to_string());
    out.actual_testonly = Some(ct.actual_testonly.unwrap_or(ct.testonly));
    out.is_late_bound_alias = true;
    out.label = n.rule.label.clone();
    out.configuration = n.cfg.clone();
    out.actions.clear();
    out.typed_actions.clear();
    out.is_test = false;
    out.rule_class = "alias".to_string();
    out.providers.insert(
        "LateBoundAliasProvider".into(),
        provider_instance("LateBoundAliasProvider", vec![]),
    );
    out.providers.insert(
        "BuildSettingProvider".into(),
        provider_instance(
            "BuildSettingProvider",
            vec![
                ("value", Value::Label(actual)),
                ("default", Value::Label(default_lbl)),
                ("label", Value::Label(n.rule.label.clone())),
                ("is_flag", Value::Bool(is_flag)),
            ],
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
    let bo = n.cfg.build_options();
    let raw_plugins: Vec<String> = if let Some(s) = n.cfg.options.get("plugin") {
        s.split([',', zel_analysis::build_options::MULTI_OPT_SEP])
            .filter(|x| !x.is_empty())
            .map(str::to_string)
            .collect()
    } else {
        match bo.get("plugin") {
            Some(zel_analysis::build_options::ConfigValue::List(vs)) => vs
                .iter()
                .flat_map(|x| x.split([',', zel_analysis::build_options::MULTI_OPT_SEP]))
                .filter(|x| !x.is_empty())
                .map(str::to_string)
                .collect(),
            Some(zel_analysis::build_options::ConfigValue::Scalar(s)) => s
                .split([',', zel_analysis::build_options::MULTI_OPT_SEP])
                .filter(|x| !x.is_empty())
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        }
    };
    let root_pkg =
        zel_cmdline::PackageIdentifier::create_in_main_repo("").map_err(|e| e.to_string())?;
    let main_entries: Vec<(String, zel_cmdline::RepositoryName)> = n
        .resolver
        .repo_mapping_entries("")
        .into_iter()
        .map(|(a, c)| {
            let r = if c.is_empty() {
                zel_cmdline::RepositoryName::main()
            } else {
                zel_cmdline::RepositoryName::create_unvalidated(&c)
            };
            (a, r)
        })
        .collect();
    let main_repo_map = zel_cmdline::RepositoryMapping::new_ordered(
        main_entries,
        zel_cmdline::RepositoryName::main(),
    );
    // `:java_plugins` (exec configuration): the labels of `--plugin`.
    let exec_cfg = n.cfg.to_exec_for(&n.acfg.default_exec_platform());
    const PLUGIN_INFO: &str = "@@rules_java+//java/private:java_info.bzl%JavaPluginInfo";
    let mut infos: Vec<zel_starlark::Value> = Vec::new();
    for raw in &raw_plugins {
        let norm_raw = if raw.starts_with("//") || raw.starts_with('@') {
            raw.clone()
        } else {
            format!("//{raw}")
        };
        let l = Label::parse_with_package_context(&norm_raw, &root_pkg, &main_repo_map)
            .or_else(|_| Label::parse_canonical(&norm_raw))
            .map_err(|e| format!("--plugin: invalid label '{raw}': {e}"))?;
        let ct = n.resolver.configured_target(&l, &exec_cfg)?;
        n.resolver.check_dependency(n.rule, ":java_plugins", &ct)?;
        let actual = ct.get_actual();
        match actual.providers.get(PLUGIN_INFO) {
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
    let raw_top = n
        .cfg
        .build_options()
        .get("grte_top")
        .map(|v| v.display())
        .filter(|s| !s.is_empty() && s != "null")
        .or_else(|| n.cfg.options.get("grte_top").cloned())
        .unwrap_or_default();
    match parse_libc_top_label(&raw_top)? {
        Some(actual) => {
            let ct = n.resolver.configured_target(&actual, n.cfg)?;
            n.resolver.check_dependency(n.rule, ":alias", &ct)?;
            let mut targets_chain = vec![actual];
            targets_chain.extend(ct.alias_targets.iter().cloned());
            let mut out = (*ct).clone();
            let mut chain = vec![n.rule.label.clone()];
            chain.extend(ct.alias_chain.iter().cloned());
            out.alias_chain = chain;
            out.alias_targets = targets_chain;
            out.actual = Some(ct.clone());
            out.actual_label = Some(ct.actual_target_label().clone());
            out.actual_rule_class = Some(ct.actual_rule_class_name().to_string());
            out.actual_testonly = Some(ct.actual_testonly.unwrap_or(ct.testonly));
            out.is_late_bound_alias = true;
            out.label = n.rule.label.clone();
            out.configuration = n.cfg.clone();
            out.actions.clear();
            out.typed_actions.clear();
            out.is_test = false;
            out.rule_class = "alias".to_string();
            out.providers.insert(
                "LateBoundAliasProvider".into(),
                provider_instance("LateBoundAliasProvider", vec![]),
            );
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
            empty.providers.insert(
                "LateBoundAliasProvider".into(),
                provider_instance("LateBoundAliasProvider", vec![]),
            );
            Ok(empty)
        }
    }
}

/// `ExtraAction`/`ActionListener`/`Environment`: analysed for their dependencies
/// (`tools`, `extra_actions`) only; they provide no files.
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
    label_build_setting(n, false)
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
    let ecw = n.labels("exec_compatible_with")?;

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
    let mut selected_exec_platform = n.acfg.default_exec_platform();
    let mut resolved_exec_plat: Option<String> = None;
    if !tc_types.is_empty() || !ecw.is_empty() {
        if let Ok(tc) =
            crate::resolve_ctx_toolchains(&n.rule.label, &tc_types, &ecw, n.cfg, n.acfg, n.resolver)
        {
            for d in &tc.deps {
                n.resolver.check_dependency(n.rule, "$toolchains", d)?;
            }
            tc_make_vars.extend(tc.make_variables);
            toolchain_tools.extend(tc.resolved_targets);
            selected_exec_platform = tc.exec_platform.clone();
            resolved_exec_plat = Some(tc.exec_platform);
        } else if !tc_types.is_empty() {
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
            tc_make_vars.extend(tc.make_variables);
            toolchain_tools.extend(tc.resolved_targets);
            selected_exec_platform = tc.exec_platform.clone();
            resolved_exec_plat = Some(tc.exec_platform);
        }
    }
    let exec_is_windows = {
        let mut is_win = ecw.iter().any(|l| l.name() == "windows");
        if !is_win {
            let plat_lbl = resolved_exec_plat
                .as_deref()
                .and_then(|s| Label::parse_canonical(s).ok())
                .or_else(|| {
                    n.acfg
                        .all_execution_platforms()
                        .ok()
                        .and_then(|v| v.into_iter().next())
                });
            if let Some(pl) = plat_lbl {
                if pl.name().to_ascii_lowercase().contains("windows") {
                    is_win = true;
                } else if let Ok(pct) = n.resolver.configured_target(&pl, &n.cfg.to_core_only()) {
                    if let Ok(win_cv) = Label::parse_canonical("@platforms//os:windows")
                        .or_else(|_| Label::parse_canonical("@@platforms+//os:windows"))
                    {
                        if let Ok(cv_ct) =
                            n.resolver.configured_target(&win_cv, &n.cfg.to_core_only())
                        {
                            if crate::toolchain::platform_has_value(&pct, &cv_ct) {
                                is_win = true;
                            }
                        }
                    }
                }
            } else {
                is_win = cfg!(windows);
            }
        }
        is_win
    };
    let non_empty_explicit = |attr: &str| -> Result<bool, String> {
        Ok(n.explicit(attr) && !n.string(attr)?.is_empty())
    };
    let cmd_attr = if exec_is_windows {
        if non_empty_explicit("cmd_ps")? {
            "cmd_ps"
        } else if non_empty_explicit("cmd_bat")? {
            "cmd_bat"
        } else if non_empty_explicit("cmd_bash")? {
            "cmd_bash"
        } else if n.explicit("cmd") {
            "cmd"
        } else {
            return Err(n.attr_err(
                "cmd",
                "missing value for `cmd` attribute, you can also set `cmd_ps` or `cmd_bat` on Windows and `cmd_bash` on other platforms.",
            ));
        }
    } else if non_empty_explicit("cmd_bash")? {
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
    let is_cmd_bat = cmd_attr == "cmd_bat";
    let norm_make_path = |s: String| -> String {
        if is_cmd_bat {
            s.replace('/', "\\")
        } else {
            s
        }
    };
    let singleton_err: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
    let lookup = |name: &str| -> Option<String> {
        match name {
            "SRCS" => Some(norm_make_path(
                resolved_srcs
                    .iter()
                    .map(|a| a.exec_path.path_string().to_string())
                    .collect::<Vec<_>>()
                    .join(" "),
            )),
            "OUTS" => Some(norm_make_path(
                outs.iter()
                    .map(|a| a.exec_path.path_string().to_string())
                    .collect::<Vec<_>>()
                    .join(" "),
            )),
            "<" => match singleton(&resolved_srcs, "$<", "input file") {
                Ok(p) => Some(norm_make_path(p)),
                Err(e) => {
                    if singleton_err.borrow().is_none() {
                        *singleton_err.borrow_mut() = Some(e);
                    }
                    Some(String::new())
                }
            },
            "@" => match singleton(&outs, "$@", "output file") {
                Ok(p) => Some(norm_make_path(p)),
                Err(e) => {
                    if singleton_err.borrow().is_none() {
                        *singleton_err.borrow_mut() = Some(e);
                    }
                    Some(String::new())
                }
            },
            "RULEDIR" => Some(norm_make_path(rule_dir.clone())),
            "@D" => Some(norm_make_path(if outs.len() == 1 {
                outs[0]
                    .exec_path
                    .parent_directory()
                    .map(|p| p.path_string().to_string())
                    .unwrap_or_default()
            } else {
                rule_dir.clone()
            })),
            other => n
                .cfg
                .user_defines
                .get(other)
                .cloned()
                .or_else(|| tc_make_vars.get(other).cloned())
                .or_else(|| match other {
                    "BINDIR" | "GENDIR" => Some(norm_make_path(bin_dir.clone())),
                    "COMPILATION_MODE" => Some(n.cfg.compilation_mode.clone()),
                    "TARGET_CPU" => Some(n.cfg.cpu.clone()),
                    _ => None,
                }),
        }
    };
    let expanded_cmd =
        expand_make_variables(&located, &lookup).map_err(|e| n.attr_err(cmd_attr, &e))?;
    if let Some(e) = singleton_err.into_inner() {
        return Err(n.attr_err(cmd_attr, &e));
    }
    let setup_label = Label::parse_canonical("@bazel_tools//tools/genrule:genrule-setup.sh")
        .unwrap_or_else(|_| {
            Label::parse_canonical("@@bazel_tools//tools/genrule:genrule-setup.sh").unwrap()
        });
    let setup_art = Artifact::source_with_layout(
        zel_vfs::PathFragment::create("tools/genrule/genrule-setup.sh"),
        setup_label.clone(),
        sibling_layout,
    );

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
        for t in n.rule.rule_tags() {
            if let Some(cpu) = t.strip_prefix("cpu:") {
                exec_info.insert("cpu".to_string(), cpu.to_string());
            } else if t.starts_with("block-")
                || t.starts_with("requires-")
                || t.starts_with("no-")
                || t.starts_with("supports-")
                || t.starts_with("disable-")
                || t.starts_with("resources:")
                || t == "local"
            {
                exec_info.entry(t).or_insert_with(String::new);
            }
        }
    }
    zel_analysis::action::apply_modify_execution_info_to_map(n.cfg, "Genrule", &mut exec_info);

    let mut extra_actions: Vec<Action> = Vec::new();
    let mut inputs = resolved_srcs.clone();
    let pkg_prefix = if pkg.path_string().is_empty() {
        String::new()
    } else {
        format!("{}/", pkg.path_string())
    };
    let make_script_artifact = |ext: &str,
                                content: String,
                                is_executable: bool,
                                extra_actions: &mut Vec<Action>|
     -> Artifact {
        let script_rel = PathFragment::create(&format!(
            "{}{}.genrule_script.{ext}",
            pkg_prefix,
            label.name()
        ));
        let art = Artifact::derived_in_root_with_layout(
            &n.cfg.mnemonic(),
            ArtifactRoot::Bin,
            script_rel,
            label.clone(),
            sibling_layout,
        );
        let mut wa = Action::new(
            label.clone(),
            "FileWrite",
            ActionKind::FileWrite {
                content: FileContent::Literal(content),
                is_executable,
            },
        );
        wa.outputs.push(art.clone());
        wa.owner_kind = "genrule".to_string();
        wa.owner_config = n.cfg.short_hash();
        extra_actions.push(wa);
        art
    };

    let (spawn_exe, spawn_args, spawn_shell_cmd, needs_bash_setup) = match cmd_attr {
        "cmd_ps" => {
            if expanded_cmd.len() > 32767 {
                let script = make_script_artifact(
                    "ps1",
                    format!("{expanded_cmd}\r\n"),
                    false,
                    &mut extra_actions,
                );
                let sp = script.exec_path.path_string().to_string();
                inputs.push(script);
                (
                    "powershell.exe".to_string(),
                    vec!["-File".to_string(), sp],
                    None,
                    false,
                )
            } else {
                (
                    "powershell.exe".to_string(),
                    vec!["-Command".to_string(), expanded_cmd.clone()],
                    None,
                    false,
                )
            }
        }
        "cmd_bat" => {
            if expanded_cmd.len() > 8191 {
                let script = make_script_artifact(
                    "bat",
                    format!("{expanded_cmd}\r\n"),
                    false,
                    &mut extra_actions,
                );
                let sp = script.exec_path.path_string().to_string();
                inputs.push(script);
                (
                    "cmd.exe".to_string(),
                    vec![
                        "/S".to_string(),
                        "/E:ON".to_string(),
                        "/V:ON".to_string(),
                        "/D".to_string(),
                        "/C".to_string(),
                        sp,
                    ],
                    None,
                    false,
                )
            } else {
                (
                    "cmd.exe".to_string(),
                    vec![
                        "/S".to_string(),
                        "/E:ON".to_string(),
                        "/V:ON".to_string(),
                        "/D".to_string(),
                        "/C".to_string(),
                        expanded_cmd.clone(),
                    ],
                    None,
                    false,
                )
            }
        }
        _ => {
            let shell = if exec_info.contains_key("requires-darwin") {
                "/bin/bash".to_string()
            } else {
                n.acfg
                    .shell_executable
                    .clone()
                    .unwrap_or_else(|| "/bin/bash".to_string())
            };
            let command = format!("source {}; {expanded_cmd}", setup_art.exec_path);
            if command.len() > 65536 {
                let script = make_script_artifact(
                    "sh",
                    format!("#!{shell}\n{command}\n"),
                    true,
                    &mut extra_actions,
                );
                let sp = script.exec_path.path_string().to_string();
                inputs.push(script);
                (shell, vec![sp], None, true)
            } else {
                (
                    shell,
                    vec!["-c".to_string(), command.clone()],
                    Some(command),
                    true,
                )
            }
        }
    };

    let mut action = Action::new(
        label.clone(),
        "Genrule",
        ActionKind::Spawn {
            executable: spawn_exe,
            arguments: vec![CommandLineArgs {
                args: spawn_args,
                param_file: None,
            }],
            shell_command: spawn_shell_cmd,
        },
    );
    if !message.is_empty() {
        action.progress_message = Some(format!("{message} {label}"));
    }
    action.execution_requirements = exec_info;
    if needs_bash_setup {
        if let Ok(setup_ct) = n.resolver.configured_target(&setup_label, n.cfg) {
            let _ = n
                .resolver
                .check_dependency(n.rule, "$genrule_setup", &setup_ct);
        }
        inputs.push(setup_art);
    }
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
    if let Some(ep) = resolved_exec_plat {
        action.exec_platform = Some(ep);
    }
    extra_actions.push(action);

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
        extra_actions,
    )
}

// ---------------------------------------------------------------------------------- starlark_doc_extract

/// Native `starlark_doc_extract` rule (`StarlarkDocExtract.java`).
///
/// Parses the Starlark `.bzl` module referenced by `src`, extracts documentation into a
/// `stardoc_output.ModuleInfo` protobuf message, and registers `FileWrite` actions for both
/// implicit outputs (`%{name}.binaryproto` as the default output and `%{name}.textproto`).
pub(crate) fn starlark_doc_extract(n: &Nctx) -> Result<ConfiguredTarget, String> {
    let src_labels = n.labels("src")?;
    let src_label = src_labels.first().ok_or_else(|| {
        n.attr_err(
            "src",
            "missing value for mandatory attribute 'src' in 'starlark_doc_extract' rule",
        )
    })?;
    let src_deps = n.deps("src", false)?;
    let _deps = n.deps("deps", false)?;
    let _data = n.deps("data", false)?;

    let symbol_names = n.strings("symbol_names")?;
    let render_main_repo_name = n.boolean("render_main_repo_name")?;

    let effective_src_label = src_deps
        .first()
        .map(|ct| ct.actual_target_label().clone())
        .unwrap_or_else(|| src_label.clone());
    let src_is_generated = src_deps
        .first()
        .and_then(|ct| ct.default_info.files.first())
        .is_some_and(|a| !a.is_source);

    let rule_repo = n.rule.label.package_identifier().repository();
    let loader = n.resolver.package_loader();
    let repo_resolver = loader.as_ref().map(|l| l.config().lookup.resolver.as_ref());
    let file_display = render_doc_label(
        &effective_src_label,
        rule_repo,
        render_main_repo_name,
        repo_resolver,
    );

    let mut module_info = Msg::new("stardoc_output.ModuleInfo");
    module_info.set_str("file", &file_display);

    if !src_is_generated {
        if let Some(loader) = loader.as_ref() {
            if let Some(source) = read_bzl_source(loader, &effective_src_label) {
                let mut ctx = DocExtractCtx {
                    loader,
                    rule_repo,
                    render_main_repo_name,
                    visited: BTreeSet::new(),
                };
                ctx.extract_module(
                    &effective_src_label,
                    &file_display,
                    &source,
                    &symbol_names,
                    &mut module_info,
                )
                .map_err(|e| n.err("src", &e))?;
            }
        }
    }

    let pkg_id = n.rule.label.package_identifier();
    let name = n.rule.label.name();
    let bin_label = Label::create(pkg_id.clone(), &format!("{name}.binaryproto"))
        .map_err(|e| n.err("name", &format!("invalid binaryproto output name: {e}")))?;
    let txt_label = Label::create(pkg_id.clone(), &format!("{name}.textproto"))
        .map_err(|e| n.err("name", &format!("invalid textproto output name: {e}")))?;

    let bin_art = n.out_artifact(&bin_label);
    let txt_art = n.out_artifact(&txt_label);

    let mut bin_action = Action::new(
        n.rule.label.clone(),
        "FileWrite",
        ActionKind::FileWrite {
            content: FileContent::Bytes(module_info.encode_to_vec()),
            is_executable: false,
        },
    );
    bin_action.outputs.push(bin_art.clone());

    let mut txt_action = Action::new(
        n.rule.label.clone(),
        "FileWrite",
        ActionKind::FileWrite {
            content: FileContent::Literal(module_info.to_text()),
            is_executable: false,
        },
    );
    txt_action.outputs.push(txt_art.clone());

    let mut ct = finish(
        n,
        vec![bin_art.clone()],
        Runfiles::default(),
        Runfiles::default(),
        None,
        vec![bin_action, txt_action],
    )?;
    ct.output_groups
        .groups
        .insert("binaryproto".to_string(), vec![bin_art.clone()]);
    ct.output_groups
        .groups
        .insert("textproto".to_string(), vec![txt_art.clone()]);
    ct.providers.insert(
        "OutputGroupInfo".to_string(),
        provider_instance(
            "OutputGroupInfo",
            vec![
                ("binaryproto", files_depset(&[bin_art])),
                ("textproto", files_depset(&[txt_art])),
            ],
        ),
    );
    Ok(ct)
}

fn render_doc_label(
    label: &Label,
    rule_repo: &RepositoryName,
    render_main_repo_name: bool,
    resolver: Option<&dyn RepoResolver>,
) -> String {
    let pkg = label.package_identifier().package_fragment().path_string();
    let target = label.name();
    let label_repo = label.package_identifier().repository();
    if label_repo.is_main() {
        if render_main_repo_name {
            let main_name = resolver.and_then(|r| {
                r.mapping(rule_repo)
                    .entries()
                    .find(|(apparent, canonical)| !apparent.is_empty() && canonical.is_main())
                    .map(|(apparent, _)| apparent.clone())
                    .or_else(|| {
                        r.module_name_and_version(&RepositoryName::main())
                            .map(|(name, _)| name)
                            .filter(|name| !name.is_empty())
                    })
            });
            if let Some(repo_name) = main_name {
                return format!("@{repo_name}//{pkg}:{target}");
            }
        }
        format!("//{pkg}:{target}")
    } else {
        if let Some(r) = resolver {
            if let Some((apparent, _)) = r
                .mapping(rule_repo)
                .entries()
                .find(|(apparent, canonical)| !apparent.is_empty() && *canonical == label_repo)
            {
                return format!("@{apparent}//{pkg}:{target}");
            }
        }
        format!("{}//{pkg}:{target}", label_repo.canonical_form())
    }
}

fn read_bzl_source(loader: &zel_packages::PackageLoader, label: &Label) -> Option<String> {
    let repo_root = loader
        .config()
        .lookup
        .resolver
        .repo_root(label.package_identifier().repository())?;
    let rel = label
        .package_identifier()
        .package_fragment()
        .get_relative_str(label.name());
    let abs = repo_root.get_relative(&rel);
    let bytes = loader.config().lookup.fs.read_file(&abs).ok()?;
    Some(zel_starlark::source_from_bytes(&bytes))
}

struct DocExtractCtx<'a> {
    loader: &'a zel_packages::PackageLoader,
    rule_repo: &'a RepositoryName,
    render_main_repo_name: bool,
    visited: BTreeSet<String>,
}

#[derive(Clone)]
enum ExtractedDef {
    Func(Arc<UserFunctionDef>),
    Rule(Expr),
    Provider(Expr),
    Aspect(Expr),
    Macro(Expr),
    RepositoryRule(Expr),
    ModuleExtension(Expr),
    Struct(Vec<(String, Expr)>),
    Alias(String),
    Other(Expr),
}

struct ParsedBzlModule {
    label: Label,
    file_display: String,
    module_docstring: Option<String>,
    loads: BTreeMap<String, (String, String)>,
    defs: BTreeMap<String, ExtractedDef>,
    string_consts: BTreeMap<String, String>,
    string_list_consts: BTreeMap<String, Vec<String>>,
    dict_consts: BTreeMap<String, Vec<(String, Expr)>>,
    doc_comments: BTreeMap<String, String>,
}

impl DocExtractCtx<'_> {
    fn parse_bzl(
        &self,
        label: &Label,
        file_display: &str,
        source: &str,
    ) -> Result<ParsedBzlModule, String> {
        let mut lex = Lexer::new(file_display, source);
        let tokens = lex.lex_all().map_err(|e| e.to_string())?;
        let comments = std::mem::take(&mut lex.comments);
        let standalone_lines = std::mem::take(&mut lex.standalone_comment_lines);
        let mut parser = Parser::new(tokens);
        let stmts = parser.parse_module().map_err(|e| e.0)?;
        let (raw_doc_comments, _unused) =
            zel_starlark::syntax::extract_doc_comments(&stmts, &comments, &standalone_lines);
        let mut doc_comments = BTreeMap::new();
        for (name, dc) in raw_doc_comments {
            doc_comments.insert(name, dc.text());
        }

        let module_docstring = match stmts.first() {
            Some(Stmt::Expr(Expr::Literal(Value::String(s)))) => Some(normalize_docstring(s)),
            _ => None,
        };

        let mut loads = BTreeMap::new();
        let mut defs = BTreeMap::new();
        let mut string_consts = BTreeMap::new();
        let mut string_list_consts = BTreeMap::new();
        let mut dict_consts = BTreeMap::new();

        for stmt in &stmts {
            match stmt {
                Stmt::Load(load_stmt, _) => {
                    for (local, exported) in &load_stmt.symbols {
                        loads.insert(
                            local.clone(),
                            (load_stmt.module_label.clone(), exported.clone()),
                        );
                    }
                }
                Stmt::Def(func_def) => {
                    defs.insert(func_def.name.clone(), ExtractedDef::Func(func_def.clone()));
                }
                Stmt::Assign {
                    target,
                    op: AssignOp::Assign,
                    value,
                    ..
                } => match target {
                    AssignTarget::Ident(name) => {
                        if let Some(s) = eval_string_expr(value, &string_consts) {
                            string_consts.insert(name.clone(), s);
                        }
                        if let Some(sl) =
                            eval_string_list_expr(value, &string_consts, &string_list_consts)
                        {
                            string_list_consts.insert(name.clone(), sl);
                        }
                        if let Some(entries) = eval_dict_expr(value, &string_consts, &dict_consts) {
                            dict_consts.insert(name.clone(), entries);
                        }
                        defs.insert(name.clone(), classify_assigned_expr(value));
                    }
                    AssignTarget::Tuple(patterns) => {
                        if let Some((callee_name, _)) = top_call_name(value) {
                            if callee_name == "provider" {
                                if let Some(Pattern::Name(name)) = patterns.first() {
                                    defs.insert(
                                        name.clone(),
                                        ExtractedDef::Provider(value.clone()),
                                    );
                                }
                            }
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        Ok(ParsedBzlModule {
            label: label.clone(),
            file_display: file_display.to_string(),
            module_docstring,
            loads,
            defs,
            string_consts,
            string_list_consts,
            dict_consts,
            doc_comments,
        })
    }

    fn extract_module(
        &mut self,
        label: &Label,
        file_display: &str,
        source: &str,
        symbol_names: &[String],
        out: &mut Msg,
    ) -> Result<(), String> {
        let parsed = self.parse_bzl(label, file_display, source)?;
        if let Some(ref doc) = parsed.module_docstring {
            out.set_str("module_docstring", doc);
        }

        let filter: Option<BTreeSet<&str>> = if symbol_names.is_empty() {
            None
        } else {
            Some(symbol_names.iter().map(String::as_str).collect())
        };

        let mut collector = CollectedEntries::default();
        self.visited.insert(label.canonical_form());

        for (name, def) in &parsed.defs {
            let should_consider = match &filter {
                None => !name.starts_with('_'),
                Some(set) => {
                    set.contains(name.as_str())
                        || set
                            .iter()
                            .any(|wanted| wanted.starts_with(&format!("{name}.")))
                }
            };
            if !should_consider {
                continue;
            }
            self.collect_symbol(
                &parsed,
                name,
                name,
                Some(name),
                def,
                filter.as_ref(),
                0,
                &mut collector,
            );
        }

        // Also handle explicitly requested symbols that come straight from a `load(...)` binding.
        if let Some(ref set) = filter {
            let mut handled_load_tops: BTreeSet<&str> = BTreeSet::new();
            for wanted in set {
                let top = wanted.split('.').next().unwrap_or(wanted);
                if parsed.defs.contains_key(top) || !handled_load_tops.insert(top) {
                    continue;
                }
                if let Some((mod_str, orig_name)) = parsed.loads.get(top) {
                    self.collect_from_load(
                        &parsed,
                        mod_str,
                        orig_name,
                        top,
                        filter.as_ref(),
                        0,
                        &mut collector,
                    );
                }
            }
        }

        collector.write_into(out);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_symbol(
        &mut self,
        module: &ParsedBzlModule,
        export_name: &str,
        origin_name: &str,
        doc_binding: Option<&str>,
        def: &ExtractedDef,
        filter: Option<&BTreeSet<&str>>,
        depth: usize,
        out: &mut CollectedEntries,
    ) {
        if depth > 8 {
            return;
        }
        let matches_exact = match filter {
            None => true,
            Some(set) => {
                set.contains(export_name)
                    || set
                        .iter()
                        .any(|w| export_name.starts_with(&format!("{w}.")))
            }
        };

        match def {
            ExtractedDef::Func(func_def) => {
                if matches_exact {
                    out.funcs.push((
                        export_name.to_string(),
                        build_function_info(
                            export_name,
                            origin_name,
                            &module.file_display,
                            func_def,
                        ),
                    ));
                }
            }
            ExtractedDef::Rule(expr) => {
                if matches_exact {
                    out.rules.push((
                        export_name.to_string(),
                        build_rule_info(
                            export_name,
                            origin_name,
                            &module.file_display,
                            expr,
                            module,
                        ),
                    ));
                }
            }
            ExtractedDef::Provider(expr) => {
                if matches_exact {
                    out.providers.push((
                        export_name.to_string(),
                        build_provider_info(
                            export_name,
                            origin_name,
                            &module.file_display,
                            expr,
                            module,
                        ),
                    ));
                }
            }
            ExtractedDef::Aspect(expr) => {
                if matches_exact {
                    out.aspects.push((
                        export_name.to_string(),
                        build_aspect_info(
                            export_name,
                            origin_name,
                            &module.file_display,
                            expr,
                            module,
                        ),
                    ));
                }
            }
            ExtractedDef::Macro(expr) => {
                if matches_exact {
                    out.macros.push((
                        export_name.to_string(),
                        build_macro_info(
                            export_name,
                            origin_name,
                            &module.file_display,
                            expr,
                            module,
                        ),
                    ));
                }
            }
            ExtractedDef::RepositoryRule(expr) => {
                if matches_exact {
                    out.repo_rules.push((
                        export_name.to_string(),
                        build_repository_rule_info(
                            export_name,
                            origin_name,
                            &module.file_display,
                            expr,
                            module,
                        ),
                    ));
                }
            }
            ExtractedDef::ModuleExtension(expr) => {
                if matches_exact {
                    out.module_extensions.push((
                        export_name.to_string(),
                        build_module_extension_info(
                            export_name,
                            &module.file_display,
                            expr,
                            module,
                        ),
                    ));
                }
            }
            ExtractedDef::Struct(fields) => {
                for (field_name, field_expr) in fields {
                    let qualified = format!("{export_name}.{field_name}");
                    let consider = match filter {
                        None => !field_name.starts_with('_'),
                        Some(set) => {
                            set.contains(export_name)
                                || set.contains(qualified.as_str())
                                || set.iter().any(|w| w.starts_with(&format!("{qualified}.")))
                        }
                    };
                    if !consider {
                        continue;
                    }
                    let field_def = classify_assigned_expr(field_expr);
                    self.collect_symbol(
                        module,
                        &qualified,
                        field_name,
                        None,
                        &field_def,
                        filter,
                        depth + 1,
                        out,
                    );
                }
            }
            ExtractedDef::Alias(rhs) => {
                if let Some(local_def) = module.defs.get(rhs).cloned() {
                    let next_origin = if rhs.starts_with('_') {
                        origin_name
                    } else {
                        rhs.as_str()
                    };
                    let next_doc_binding = if module.doc_comments.contains_key(rhs) {
                        Some(rhs.as_str())
                    } else {
                        doc_binding
                    };
                    self.collect_symbol(
                        module,
                        export_name,
                        next_origin,
                        next_doc_binding,
                        &local_def,
                        filter,
                        depth + 1,
                        out,
                    );
                } else if let Some((mod_str, orig_name)) = module.loads.get(rhs).cloned() {
                    self.collect_from_load(
                        module,
                        &mod_str,
                        &orig_name,
                        export_name,
                        filter,
                        depth + 1,
                        out,
                    );
                } else if matches_exact {
                    if let Some(doc) = doc_binding.and_then(|b| module.doc_comments.get(b)) {
                        out.other_symbols.push((
                            export_name.to_string(),
                            build_other_symbol_info(export_name, doc, "unknown"),
                        ));
                    }
                }
            }
            ExtractedDef::Other(expr) => {
                if matches_exact {
                    if let Some(doc) = doc_binding.and_then(|b| module.doc_comments.get(b)) {
                        out.other_symbols.push((
                            export_name.to_string(),
                            build_other_symbol_info(export_name, doc, infer_expr_type_name(expr)),
                        ));
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_from_load(
        &mut self,
        current: &ParsedBzlModule,
        mod_str: &str,
        orig_name: &str,
        export_name: &str,
        filter: Option<&BTreeSet<&str>>,
        depth: usize,
        out: &mut CollectedEntries,
    ) {
        if depth > 8 {
            return;
        }
        let repo = current.label.package_identifier().repository();
        let mapping = self.loader.config().lookup.resolver.mapping(repo);
        let Ok(target_label) = Label::parse_with_package_context(
            mod_str,
            current.label.package_identifier(),
            &mapping,
        ) else {
            return;
        };
        let key = target_label.canonical_form();
        if !self.visited.insert(key.clone()) {
            return;
        }
        let target_display = render_doc_label(
            &target_label,
            self.rule_repo,
            self.render_main_repo_name,
            Some(self.loader.config().lookup.resolver.as_ref()),
        );
        if let Some(src) = read_bzl_source(self.loader, &target_label) {
            if let Ok(loaded_mod) = self.parse_bzl(&target_label, &target_display, &src) {
                if let Some(def) = loaded_mod.defs.get(orig_name).cloned() {
                    self.collect_symbol(
                        &loaded_mod,
                        export_name,
                        orig_name,
                        Some(orig_name),
                        &def,
                        filter,
                        depth + 1,
                        out,
                    );
                }
            }
        }
        self.visited.remove(&key);
    }
}

#[derive(Default)]
struct CollectedEntries {
    rules: Vec<(String, Msg)>,
    providers: Vec<(String, Msg)>,
    funcs: Vec<(String, Msg)>,
    aspects: Vec<(String, Msg)>,
    module_extensions: Vec<(String, Msg)>,
    repo_rules: Vec<(String, Msg)>,
    macros: Vec<(String, Msg)>,
    other_symbols: Vec<(String, Msg)>,
}

impl CollectedEntries {
    fn write_into(mut self, out: &mut Msg) {
        self.rules.sort_by(|a, b| a.0.cmp(&b.0));
        self.providers.sort_by(|a, b| a.0.cmp(&b.0));
        self.funcs.sort_by(|a, b| a.0.cmp(&b.0));
        self.aspects.sort_by(|a, b| a.0.cmp(&b.0));
        self.module_extensions.sort_by(|a, b| a.0.cmp(&b.0));
        self.repo_rules.sort_by(|a, b| a.0.cmp(&b.0));
        self.macros.sort_by(|a, b| a.0.cmp(&b.0));
        self.other_symbols.sort_by(|a, b| a.0.cmp(&b.0));

        for (_, m) in self.rules {
            out.set_msg("rule_info", m);
        }
        for (_, m) in self.providers {
            out.set_msg("provider_info", m);
        }
        for (_, m) in self.funcs {
            out.set_msg("func_info", m);
        }
        for (_, m) in self.aspects {
            out.set_msg("aspect_info", m);
        }
        for (_, m) in self.module_extensions {
            out.set_msg("module_extension_info", m);
        }
        for (_, m) in self.repo_rules {
            out.set_msg("repository_rule_info", m);
        }
        for (_, m) in self.macros {
            out.set_msg("macro_info", m);
        }
        for (_, m) in self.other_symbols {
            out.set_msg("starlark_other_symbol_info", m);
        }
    }
}

fn top_call_name(expr: &Expr) -> Option<(&str, &Expr)> {
    if let Expr::Call { callee, .. } = expr {
        if let Expr::Ident(name, _) = callee.as_ref() {
            return Some((name.as_str(), expr));
        }
    }
    None
}

fn classify_assigned_expr(expr: &Expr) -> ExtractedDef {
    if let Expr::Call {
        callee, kw_args, ..
    } = expr
    {
        if let Expr::Ident(name, _) = callee.as_ref() {
            match name.as_str() {
                "rule" => return ExtractedDef::Rule(expr.clone()),
                "provider" => return ExtractedDef::Provider(expr.clone()),
                "aspect" => return ExtractedDef::Aspect(expr.clone()),
                "macro" => return ExtractedDef::Macro(expr.clone()),
                "repository_rule" => return ExtractedDef::RepositoryRule(expr.clone()),
                "module_extension" => return ExtractedDef::ModuleExtension(expr.clone()),
                "struct" => return ExtractedDef::Struct(kw_args.clone()),
                _ => {}
            }
        }
    }
    if let Expr::Ident(rhs, _) = expr {
        if !matches!(rhs.as_str(), "True" | "False" | "None") {
            return ExtractedDef::Alias(rhs.clone());
        }
    }
    ExtractedDef::Other(expr.clone())
}

fn call_kwarg<'a>(expr: &'a Expr, key: &str) -> Option<&'a Expr> {
    if let Expr::Call { kw_args, .. } = expr {
        kw_args.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    } else {
        None
    }
}

fn make_origin_key(name: &str, file: &str) -> Msg {
    let mut origin = Msg::new("stardoc_output.OriginKey");
    if !name.is_empty() && !name.starts_with('_') {
        origin.set_str("name", name);
    }
    if !file.is_empty() {
        origin.set_str("file", file);
    }
    origin
}

fn make_implicit_name_attr() -> Msg {
    let mut attr = Msg::new("stardoc_output.AttributeInfo");
    attr.set_str("name", "name");
    attr.set_str("doc_string", "A unique name for this target.");
    attr.set_enum("type", "NAME");
    attr.set_bool("mandatory", true);
    attr.set_bool("nonconfigurable", true);
    attr.set_bool("natively_defined", true);
    attr
}

fn build_rule_info(
    export_name: &str,
    origin_name: &str,
    file_display: &str,
    expr: &Expr,
    module: &ParsedBzlModule,
) -> Msg {
    let mut info = Msg::new("stardoc_output.RuleInfo");
    info.set_str("rule_name", export_name);
    if let Some(doc_expr) = call_kwarg(expr, "doc") {
        if let Some(doc) = eval_string_expr(doc_expr, &module.string_consts) {
            info.set_str("doc_string", normalize_docstring(&doc));
        }
    }
    info.set_msg("attribute", make_implicit_name_attr());
    if let Some(attrs_expr) = call_kwarg(expr, "attrs") {
        for attr in extract_attrs(attrs_expr, module) {
            info.set_msg("attribute", attr);
        }
    }
    info.set_msg("origin_key", make_origin_key(origin_name, file_display));
    if let Some(provides_expr) = call_kwarg(expr, "provides") {
        if let Some(group) = extract_provider_name_group(provides_expr, file_display) {
            info.set_msg("advertised_providers", group);
        }
    }
    let is_test = call_kwarg(expr, "test")
        .and_then(eval_bool_expr)
        .unwrap_or(false);
    let is_exec = is_test
        || call_kwarg(expr, "executable")
            .and_then(eval_bool_expr)
            .unwrap_or(false);
    info.set_bool("test", is_test);
    info.set_bool("executable", is_exec);
    info
}

fn build_macro_info(
    export_name: &str,
    origin_name: &str,
    file_display: &str,
    expr: &Expr,
    module: &ParsedBzlModule,
) -> Msg {
    let mut info = Msg::new("stardoc_output.MacroInfo");
    info.set_str("macro_name", export_name);
    if let Some(doc_expr) = call_kwarg(expr, "doc") {
        if let Some(doc) = eval_string_expr(doc_expr, &module.string_consts) {
            info.set_str("doc_string", normalize_docstring(&doc));
        }
    }
    info.set_msg("attribute", make_implicit_name_attr());
    if let Some(attrs_expr) = call_kwarg(expr, "attrs") {
        for attr in extract_attrs(attrs_expr, module) {
            info.set_msg("attribute", attr);
        }
    }
    info.set_msg("origin_key", make_origin_key(origin_name, file_display));
    let finalizer = call_kwarg(expr, "finalizer")
        .and_then(eval_bool_expr)
        .unwrap_or(false);
    info.set_bool("finalizer", finalizer);
    info
}

fn build_provider_info(
    export_name: &str,
    origin_name: &str,
    file_display: &str,
    expr: &Expr,
    module: &ParsedBzlModule,
) -> Msg {
    let mut info = Msg::new("stardoc_output.ProviderInfo");
    info.set_str("provider_name", export_name);
    if let Some(doc_expr) = call_kwarg(expr, "doc") {
        if let Some(doc) = eval_string_expr(doc_expr, &module.string_consts) {
            info.set_str("doc_string", normalize_docstring(&doc));
        }
    }
    if let Some(fields_expr) = call_kwarg(expr, "fields") {
        if let Some(entries) =
            eval_dict_expr(fields_expr, &module.string_consts, &module.dict_consts)
        {
            for (field_name, field_doc_expr) in entries {
                let mut f = Msg::new("stardoc_output.ProviderFieldInfo");
                f.set_str("name", &field_name);
                if let Some(doc) = eval_string_expr(&field_doc_expr, &module.string_consts) {
                    f.set_str("doc_string", normalize_docstring(&doc));
                }
                info.set_msg("field_info", f);
            }
        } else if let Some(names) = eval_string_list_expr(
            fields_expr,
            &module.string_consts,
            &module.string_list_consts,
        ) {
            for field_name in names {
                let mut f = Msg::new("stardoc_output.ProviderFieldInfo");
                f.set_str("name", &field_name);
                info.set_msg("field_info", f);
            }
        }
    }
    info.set_msg("origin_key", make_origin_key(origin_name, file_display));
    info
}

fn build_aspect_info(
    export_name: &str,
    origin_name: &str,
    file_display: &str,
    expr: &Expr,
    module: &ParsedBzlModule,
) -> Msg {
    let mut info = Msg::new("stardoc_output.AspectInfo");
    info.set_str("aspect_name", export_name);
    if let Some(doc_expr) = call_kwarg(expr, "doc") {
        if let Some(doc) = eval_string_expr(doc_expr, &module.string_consts) {
            info.set_str("doc_string", normalize_docstring(&doc));
        }
    }
    if let Some(aa_expr) = call_kwarg(expr, "attr_aspects") {
        if let Some(items) =
            eval_string_list_expr(aa_expr, &module.string_consts, &module.string_list_consts)
        {
            for item in items {
                info.set_str("aspect_attribute", item);
            }
        }
    }
    info.set_msg("attribute", make_implicit_name_attr());
    if let Some(attrs_expr) = call_kwarg(expr, "attrs") {
        for attr in extract_attrs(attrs_expr, module) {
            info.set_msg("attribute", attr);
        }
    }
    info.set_msg("origin_key", make_origin_key(origin_name, file_display));
    info
}

fn build_repository_rule_info(
    export_name: &str,
    origin_name: &str,
    file_display: &str,
    expr: &Expr,
    module: &ParsedBzlModule,
) -> Msg {
    let mut info = Msg::new("stardoc_output.RepositoryRuleInfo");
    info.set_str("rule_name", export_name);
    if let Some(doc_expr) = call_kwarg(expr, "doc") {
        if let Some(doc) = eval_string_expr(doc_expr, &module.string_consts) {
            info.set_str("doc_string", normalize_docstring(&doc));
        }
    }
    info.set_msg("attribute", make_implicit_name_attr());
    if let Some(attrs_expr) = call_kwarg(expr, "attrs") {
        for attr in extract_attrs(attrs_expr, module) {
            info.set_msg("attribute", attr);
        }
    }
    if let Some(env_expr) = call_kwarg(expr, "environ") {
        if let Some(items) =
            eval_string_list_expr(env_expr, &module.string_consts, &module.string_list_consts)
        {
            for item in items {
                info.set_str("environ", item);
            }
        }
    }
    info.set_msg("origin_key", make_origin_key(origin_name, file_display));
    info
}

fn build_module_extension_info(
    export_name: &str,
    file_display: &str,
    expr: &Expr,
    module: &ParsedBzlModule,
) -> Msg {
    let mut info = Msg::new("stardoc_output.ModuleExtensionInfo");
    info.set_str("extension_name", export_name);
    if let Some(doc_expr) = call_kwarg(expr, "doc") {
        if let Some(doc) = eval_string_expr(doc_expr, &module.string_consts) {
            info.set_str("doc_string", normalize_docstring(&doc));
        }
    }
    if let Some(tc_expr) = call_kwarg(expr, "tag_classes") {
        if let Some(entries) = eval_dict_expr(tc_expr, &module.string_consts, &module.dict_consts) {
            for (tag_name, tag_expr) in entries {
                let mut tc = Msg::new("stardoc_output.ModuleExtensionTagClassInfo");
                tc.set_str("tag_name", &tag_name);
                if let Some(doc_expr) = call_kwarg(&tag_expr, "doc") {
                    if let Some(doc) = eval_string_expr(doc_expr, &module.string_consts) {
                        tc.set_str("doc_string", normalize_docstring(&doc));
                    }
                }
                if let Some(attrs_expr) = call_kwarg(&tag_expr, "attrs") {
                    for attr in extract_attrs(attrs_expr, module) {
                        tc.set_msg("attribute", attr);
                    }
                }
                info.set_msg("tag_class", tc);
            }
        }
    }
    let mut origin = Msg::new("stardoc_output.OriginKey");
    if !file_display.is_empty() {
        origin.set_str("file", file_display);
    }
    info.set_msg("origin_key", origin);
    info
}

fn build_other_symbol_info(name: &str, doc: &str, type_name: &str) -> Msg {
    let mut info = Msg::new("stardoc_output.StarlarkOtherSymbolInfo");
    info.set_str("name", name);
    info.set_str("doc", doc);
    info.set_str("type_name", type_name);
    info
}

fn build_function_info(
    export_name: &str,
    origin_name: &str,
    file_display: &str,
    func_def: &UserFunctionDef,
) -> Msg {
    let mut info = Msg::new("stardoc_output.StarlarkFunctionInfo");
    info.set_str("function_name", export_name);

    let raw_doc = match func_def.body.first() {
        Some(Stmt::Expr(Expr::Literal(Value::String(s)))) => Some(s.as_str()),
        _ => None,
    };
    let parsed_doc = raw_doc.map(parse_function_docstring).unwrap_or_default();

    // Proto specifies ordering: ordinary -> keyword-only -> *args -> **kwargs.
    let mut ordinary: Vec<&ParamDef> = Vec::new();
    let mut kw_only: Vec<&ParamDef> = Vec::new();
    let mut varargs: Option<&ParamDef> = None;
    let mut kwargs: Option<&ParamDef> = None;
    for p in &func_def.params {
        if p.is_args {
            varargs = Some(p);
        } else if p.is_kwargs {
            kwargs = Some(p);
        } else if p.is_kw_only {
            kw_only.push(p);
        } else {
            ordinary.push(p);
        }
    }

    let ordered = ordinary
        .into_iter()
        .map(|p| (p, "PARAM_ROLE_ORDINARY"))
        .chain(kw_only.into_iter().map(|p| (p, "PARAM_ROLE_KEYWORD_ONLY")))
        .chain(varargs.into_iter().map(|p| (p, "PARAM_ROLE_VARARGS")))
        .chain(kwargs.into_iter().map(|p| (p, "PARAM_ROLE_KWARGS")));

    for (p, role) in ordered {
        let mut pm = Msg::new("stardoc_output.FunctionParamInfo");
        pm.set_str("name", &p.name);
        if let Some(pdoc) = parsed_doc.param_docs.get(&p.name) {
            pm.set_str("doc_string", pdoc);
        }
        if let Some(ref def_expr) = p.default {
            pm.set_str("default_value", format_starlark_expr(def_expr));
        }
        let mandatory = !p.is_args && !p.is_kwargs && p.default.is_none();
        pm.set_bool("mandatory", mandatory);
        pm.set_enum("role", role);
        info.set_msg("parameter", pm);
    }

    if !parsed_doc.summary.is_empty() {
        info.set_str("doc_string", &parsed_doc.summary);
    }
    if let Some(ref ret) = parsed_doc.returns {
        let mut r = Msg::new("stardoc_output.FunctionReturnInfo");
        r.set_str("doc_string", ret);
        info.set_msg("return", r);
    }
    if let Some(ref dep) = parsed_doc.deprecated {
        let mut d = Msg::new("stardoc_output.FunctionDeprecationInfo");
        d.set_str("doc_string", dep);
        info.set_msg("deprecated", d);
    }
    info.set_msg("origin_key", make_origin_key(origin_name, file_display));
    info
}

fn extract_attrs(attrs_expr: &Expr, module: &ParsedBzlModule) -> Vec<Msg> {
    let Some(entries) = eval_dict_expr(attrs_expr, &module.string_consts, &module.dict_consts)
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (attr_name, attr_expr) in entries {
        if attr_name.starts_with('_') {
            continue;
        }
        out.push(build_attribute_info(&attr_name, &attr_expr, module));
    }
    out
}

fn build_attribute_info(attr_name: &str, expr: &Expr, module: &ParsedBzlModule) -> Msg {
    let mut info = Msg::new("stardoc_output.AttributeInfo");
    info.set_str("name", attr_name);

    let (attr_type_enum, default_fallback, implicit_nonconfigurable) = match expr {
        Expr::Call { callee, .. } => match callee.as_ref() {
            Expr::Attr { attr, .. } => match attr.as_str() {
                "int" => ("INT", "0", false),
                "label" => ("LABEL", "None", false),
                "string" => ("STRING", "\"\"", false),
                "string_list" => ("STRING_LIST", "[]", false),
                "int_list" => ("INT_LIST", "[]", false),
                "label_list" => ("LABEL_LIST", "[]", false),
                "bool" => ("BOOLEAN", "False", false),
                "label_keyed_string_dict" => ("LABEL_STRING_DICT", "{}", false),
                "string_dict" => ("STRING_DICT", "{}", false),
                "string_list_dict" => ("STRING_LIST_DICT", "{}", false),
                "output" => ("OUTPUT", "None", true),
                "output_list" => ("OUTPUT_LIST", "[]", true),
                "label_dict" => ("LABEL_DICT_UNARY", "{}", false),
                "label_list_dict" => ("LABEL_LIST_DICT", "{}", false),
                _ => ("UNKNOWN", "", false),
            },
            _ => ("UNKNOWN", "", false),
        },
        _ => ("UNKNOWN", "", false),
    };

    if let Some(doc_expr) = call_kwarg(expr, "doc") {
        if let Some(doc) = eval_string_expr(doc_expr, &module.string_consts) {
            info.set_str("doc_string", normalize_docstring(&doc));
        }
    }
    info.set_enum("type", attr_type_enum);

    let mandatory = call_kwarg(expr, "mandatory")
        .and_then(eval_bool_expr)
        .unwrap_or(false);
    info.set_bool("mandatory", mandatory);

    if !mandatory {
        if let Some(def_expr) = call_kwarg(expr, "default") {
            let formatted = format_starlark_expr(def_expr);
            if !formatted.is_empty() {
                info.set_str("default_value", formatted);
            }
        } else if !default_fallback.is_empty() {
            info.set_str("default_value", default_fallback);
        }
    }

    let nonconfigurable = implicit_nonconfigurable
        || call_kwarg(expr, "configurable")
            .and_then(eval_bool_expr)
            .is_some_and(|c| !c);
    info.set_bool("nonconfigurable", nonconfigurable);

    if let Some(values_expr) = call_kwarg(expr, "values") {
        if let Some(vals) = eval_string_list_expr(
            values_expr,
            &module.string_consts,
            &module.string_list_consts,
        ) {
            for v in vals {
                info.set_str("values", format!("\"{}\"", v.replace('"', "\\\"")));
            }
        }
    }

    if let Some(provs_expr) = call_kwarg(expr, "providers") {
        if let Expr::List(items, _) | Expr::Tuple(items, _) = provs_expr {
            let is_nested = items
                .iter()
                .any(|e| matches!(e, Expr::List(_, _) | Expr::Tuple(_, _)));
            if is_nested {
                for group_expr in items {
                    if let Some(g) = extract_provider_name_group(group_expr, &module.file_display) {
                        info.set_msg("provider_name_group", g);
                    }
                }
            } else if let Some(g) = extract_provider_name_group(provs_expr, &module.file_display) {
                info.set_msg("provider_name_group", g);
            }
        }
    }

    info
}

fn extract_provider_name_group(expr: &Expr, file_display: &str) -> Option<Msg> {
    let items = match expr {
        Expr::List(v, _) | Expr::Tuple(v, _) => v,
        _ => return None,
    };
    if items.is_empty() {
        return None;
    }
    let mut group = Msg::new("stardoc_output.ProviderNameGroup");
    let mut added = false;
    for item in items {
        let pname = match item {
            Expr::Ident(n, _) => Some(n.clone()),
            Expr::Literal(Value::String(s)) => Some(s.clone()),
            Expr::Attr { attr, .. } => Some(attr.clone()),
            _ => None,
        };
        if let Some(name) = pname {
            group.set_str("provider_name", &name);
            group.set_msg("origin_key", make_origin_key(&name, file_display));
            added = true;
        }
    }
    added.then_some(group)
}

fn eval_string_expr(expr: &Expr, consts: &BTreeMap<String, String>) -> Option<String> {
    match expr {
        Expr::Literal(Value::String(s)) => Some(s.clone()),
        Expr::Ident(name, _) => consts.get(name).cloned(),
        Expr::BinaryOp {
            op: BinaryOpKind::Add,
            lhs,
            rhs,
            ..
        } => {
            let l = eval_string_expr(lhs, consts)?;
            let r = eval_string_expr(rhs, consts)?;
            Some(format!("{l}{r}"))
        }
        _ => None,
    }
}

fn eval_bool_expr(expr: &Expr) -> Option<bool> {
    match expr {
        Expr::Literal(Value::Bool(b)) => Some(*b),
        Expr::Ident(name, _) if name == "True" => Some(true),
        Expr::Ident(name, _) if name == "False" => Some(false),
        Expr::UnaryOp {
            op: UnaryOpKind::Not,
            expr,
            ..
        } => eval_bool_expr(expr).map(|b| !b),
        _ => None,
    }
}

fn eval_string_list_expr(
    expr: &Expr,
    str_consts: &BTreeMap<String, String>,
    list_consts: &BTreeMap<String, Vec<String>>,
) -> Option<Vec<String>> {
    match expr {
        Expr::List(items, _) | Expr::Tuple(items, _) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(eval_string_expr(item, str_consts)?);
            }
            Some(out)
        }
        Expr::Ident(name, _) => list_consts.get(name).cloned(),
        Expr::BinaryOp {
            op: BinaryOpKind::Add,
            lhs,
            rhs,
            ..
        } => {
            let mut l = eval_string_list_expr(lhs, str_consts, list_consts)?;
            let r = eval_string_list_expr(rhs, str_consts, list_consts)?;
            l.extend(r);
            Some(l)
        }
        _ => None,
    }
}

fn eval_dict_expr(
    expr: &Expr,
    str_consts: &BTreeMap<String, String>,
    dict_consts: &BTreeMap<String, Vec<(String, Expr)>>,
) -> Option<Vec<(String, Expr)>> {
    match expr {
        Expr::Dict(entries, _, _) => {
            let mut out = Vec::with_capacity(entries.len());
            for (k, v) in entries {
                let key = eval_string_expr(k, str_consts)?;
                out.push((key, v.clone()));
            }
            Some(out)
        }
        Expr::Ident(name, _) => dict_consts.get(name).cloned(),
        Expr::BinaryOp {
            op: BinaryOpKind::BitOr,
            lhs,
            rhs,
            ..
        } => {
            let mut merged = eval_dict_expr(lhs, str_consts, dict_consts)?;
            let right = eval_dict_expr(rhs, str_consts, dict_consts)?;
            merge_dict_entries(&mut merged, right);
            Some(merged)
        }
        Expr::Call {
            callee,
            pos_args,
            kw_args,
            ..
        } => {
            let is_dict_merge = match callee.as_ref() {
                Expr::Ident(n, _) => n == "dict",
                Expr::Attr { attr, .. } => attr == "add",
                _ => false,
            };
            if !is_dict_merge {
                return None;
            }
            let mut merged = Vec::new();
            for arg in pos_args {
                if let Some(entries) = eval_dict_expr(arg, str_consts, dict_consts) {
                    merge_dict_entries(&mut merged, entries);
                }
            }
            for (k, v) in kw_args {
                merge_dict_entries(&mut merged, vec![(k.clone(), v.clone())]);
            }
            Some(merged)
        }
        _ => None,
    }
}

fn merge_dict_entries(base: &mut Vec<(String, Expr)>, extra: Vec<(String, Expr)>) {
    for (k, v) in extra {
        if let Some(existing) = base.iter_mut().find(|(ek, _)| ek == &k) {
            existing.1 = v;
        } else {
            base.push((k, v));
        }
    }
}

fn format_starlark_expr(expr: &Expr) -> String {
    match expr {
        Expr::Literal(v) => v.repr(),
        Expr::Ident(name, _) => name.clone(),
        Expr::List(elems, _) => {
            let inner: Vec<String> = elems.iter().map(format_starlark_expr).collect();
            format!("[{}]", inner.join(", "))
        }
        Expr::Tuple(elems, _) => {
            let inner: Vec<String> = elems.iter().map(format_starlark_expr).collect();
            if elems.len() == 1 {
                format!("({},)", inner[0])
            } else {
                format!("({})", inner.join(", "))
            }
        }
        Expr::Dict(entries, _, _) => {
            let inner: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("{}: {}", format_starlark_expr(k), format_starlark_expr(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        Expr::UnaryOp { op, expr, .. } => {
            let inner = format_starlark_expr(expr);
            match op {
                UnaryOpKind::Neg => format!("-{inner}"),
                UnaryOpKind::Pos => format!("+{inner}"),
                UnaryOpKind::Not => format!("not {inner}"),
                UnaryOpKind::Invert => format!("~{inner}"),
            }
        }
        Expr::BinaryOp { op, lhs, rhs, .. } => {
            let op_s = match op {
                BinaryOpKind::Add => "+",
                BinaryOpKind::Sub => "-",
                BinaryOpKind::Mul => "*",
                BinaryOpKind::Div => "/",
                BinaryOpKind::FloorDiv => "//",
                BinaryOpKind::Mod => "%",
                BinaryOpKind::BitAnd => "&",
                BinaryOpKind::BitOr => "|",
                BinaryOpKind::BitXor => "^",
                BinaryOpKind::Shl => "<<",
                BinaryOpKind::Shr => ">>",
                BinaryOpKind::And => "and",
                BinaryOpKind::Or => "or",
            };
            format!(
                "{} {op_s} {}",
                format_starlark_expr(lhs),
                format_starlark_expr(rhs)
            )
        }
        Expr::Attr { target, attr, .. } => format!("{}.{attr}", format_starlark_expr(target)),
        Expr::Call {
            callee,
            pos_args,
            kw_args,
            ..
        } => {
            let mut parts: Vec<String> = pos_args.iter().map(format_starlark_expr).collect();
            for (k, v) in kw_args {
                parts.push(format!("{k} = {}", format_starlark_expr(v)));
            }
            format!("{}({})", format_starlark_expr(callee), parts.join(", "))
        }
        _ => String::new(),
    }
}

fn infer_expr_type_name(expr: &Expr) -> &'static str {
    match expr {
        Expr::Literal(v) => v.type_name(),
        Expr::Ident(n, _) if n == "True" || n == "False" => "bool",
        Expr::Ident(n, _) if n == "None" => "NoneType",
        Expr::List(_, _) | Expr::ListComp { .. } => "list",
        Expr::Tuple(_, _) => "tuple",
        Expr::Dict(_, _, _) | Expr::DictComp { .. } => "dict",
        Expr::Lambda { .. } => "function",
        _ => "unknown",
    }
}

fn normalize_docstring(raw: &str) -> String {
    let lines: Vec<&str> = raw.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    let first = lines[0].trim();
    let min_indent = lines[1..]
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.chars().take_while(|c| c.is_whitespace()).count())
        .min()
        .unwrap_or(0);
    let mut normalized = Vec::with_capacity(lines.len());
    normalized.push(first.to_string());
    for line in &lines[1..] {
        let trimmed_end = line.trim_end();
        if trimmed_end.is_empty() {
            normalized.push(String::new());
        } else {
            normalized.push(trimmed_end.chars().skip(min_indent).collect());
        }
    }
    while normalized.first().is_some_and(|l| l.is_empty()) {
        normalized.remove(0);
    }
    while normalized.last().is_some_and(|l| l.is_empty()) {
        normalized.pop();
    }
    normalized.join("\n")
}

#[derive(Default)]
struct ParsedFunctionDoc {
    summary: String,
    param_docs: BTreeMap<String, String>,
    returns: Option<String>,
    deprecated: Option<String>,
}

fn parse_function_docstring(raw: &str) -> ParsedFunctionDoc {
    let norm = normalize_docstring(raw);
    if norm.is_empty() {
        return ParsedFunctionDoc::default();
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Section {
        Summary,
        Args,
        Returns,
        Deprecated,
    }

    let mut section = Section::Summary;
    let mut summary_lines: Vec<&str> = Vec::new();
    let mut args_lines: Vec<&str> = Vec::new();
    let mut returns_lines: Vec<&str> = Vec::new();
    let mut deprecated_lines: Vec<&str> = Vec::new();

    for line in norm.lines() {
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start().len();
        if indent == 0 && trimmed == "Args:" {
            section = Section::Args;
            continue;
        } else if indent == 0 && trimmed == "Returns:" {
            section = Section::Returns;
            continue;
        } else if indent == 0 && trimmed == "Deprecated:" {
            section = Section::Deprecated;
            continue;
        }
        match section {
            Section::Summary => summary_lines.push(line),
            Section::Args => args_lines.push(line),
            Section::Returns => returns_lines.push(line),
            Section::Deprecated => deprecated_lines.push(line),
        }
    }

    let trim_lines = |lines: Vec<&str>| -> String {
        let joined = lines.join("\n");
        normalize_docstring(&joined)
    };

    let mut param_docs = BTreeMap::new();
    if !args_lines.is_empty() {
        let base_indent = args_lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        let mut cur_param: Option<String> = None;
        let mut cur_lines: Vec<String> = Vec::new();
        let flush = |cur_param: &mut Option<String>,
                     cur_lines: &mut Vec<String>,
                     out: &mut BTreeMap<String, String>| {
            if let Some(p) = cur_param.take() {
                let text = normalize_docstring(&cur_lines.join("\n"));
                out.insert(p, text);
            }
            cur_lines.clear();
        };
        for line in args_lines {
            let trimmed = line.trim();
            let indent = line.len() - line.trim_start().len();
            if !trimmed.is_empty() && indent == base_indent {
                if let Some((head, rest)) = trimmed.split_once(':') {
                    let pname = head
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .trim_start_matches('*');
                    if !pname.is_empty()
                        && pname.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        flush(&mut cur_param, &mut cur_lines, &mut param_docs);
                        cur_param = Some(pname.to_string());
                        cur_lines.push(rest.trim().to_string());
                        continue;
                    }
                }
            }
            if cur_param.is_some() {
                cur_lines.push(line.to_string());
            }
        }
        flush(&mut cur_param, &mut cur_lines, &mut param_docs);
    }

    let returns = {
        let s = trim_lines(returns_lines);
        (!s.is_empty()).then_some(s)
    };
    let deprecated = {
        let s = trim_lines(deprecated_lines);
        (!s.is_empty()).then_some(s)
    };

    ParsedFunctionDoc {
        summary: trim_lines(summary_lines),
        param_docs,
        returns,
        deprecated,
    }
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
