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

//! Execution of Starlark rule implementations (`StarlarkRuleConfiguredTargetUtil`,
//! `StarlarkRuleContext`, `StarlarkActionFactory`).
//!
//! [`analyze_starlark_rule`] builds a real `ctx` (a host object whose attributes and methods
//! are served by this module), invokes the `implementation` function with it, and turns the
//! returned providers plus the registered actions into a [`ConfiguredTarget`].
//!
//! Layout (textually included): `starlark_rule_host.rs` (attribute access / indexing),
//! `starlark_rule_actions.rs` (ctx/actions/runfiles/template_dict methods),
//! `starlark_rule_driver.rs` (attribute conversion, implementation call, provider handling).

use crate::args::{args_method, ArgsBuilder};
use crate::location::{
    expand_locations_with_mapping, expand_make_variables, expand_with_locations, shell_tokenize,
    LocationMode,
};
use crate::objects::*;
use crate::{
    Action, ActionConflict, ActionGraph, ActionKind, Artifact, ArtifactRoot, BuildConfigurationKey,
    CommandLineArgs, ConfiguredTarget, DefaultInfo, FileContent, OutputGroupInfo, ParamFileFormat,
    ResourceSet, Runfiles,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use zel_cmdline::Label;
use zel_packages::attr::{AttrType, AttrValue, Attribute};
use zel_packages::rule_class::StarlarkRuleSpec;
use zel_packages::RuleInstance;
use zel_starlark::{
    BigInt, Depset, DepsetOrder, EvalContext, StarlarkDict, StarlarkError, StarlarkList,
    StructValue, Value,
};
use zel_vfs::PathFragment;

/// Resolves dependencies (and `config_setting`s) to analysed configured targets.
pub trait DepResolver: Send + Sync {
    /// The package loader behind this resolver (`genquery` evaluates its query against it).
    fn package_loader(&self) -> Option<Arc<zel_packages::PackageLoader>> {
        None
    }

    /// Returns the configured target for `label` analysed in `cfg`.
    fn configured_target(
        &self,
        label: &Label,
        cfg: &BuildConfigurationKey,
    ) -> Result<Arc<ConfiguredTarget>, String>;

    /// Validates a direct dependency edge (visibility, `testonly`); the default accepts all.
    fn check_dependency(
        &self,
        _consumer: &RuleInstance,
        _attr: &str,
        _dep: &ConfiguredTarget,
    ) -> Result<(), String> {
        Ok(())
    }

    /// `(apparent name, canonical name)` entries of the repository mapping of the repository
    /// with canonical name `repo` (empty string = main repository). Used for the runfiles
    /// `_repo_mapping` manifest (`RepoMappingManifestAction`).
    fn repo_mapping_entries(&self, _repo: &str) -> Vec<(String, String)> {
        Vec::new()
    }

    /// The loaded rule instance behind `label` (`None` for files, package groups, ...).
    /// Needed to apply aspects (`ctx.rule`).
    fn rule_instance(&self, _label: &Label) -> Result<Option<RuleInstance>, String> {
        Ok(None)
    }

    /// `ctx.target_platform_has_constraint(constraint_value)`: whether the target platform of
    /// `cfg` has the constraint value `value`.
    fn target_platform_has_constraint(
        &self,
        _cfg: &BuildConfigurationKey,
        _value: &Label,
    ) -> Result<bool, String> {
        Err("platform queries are not available".to_string())
    }

    /// Memoization of aspect applications, keyed by an opaque string.
    fn aspect_cache_get(&self, _key: &str) -> Option<Arc<ConfiguredTarget>> {
        None
    }

    fn aspect_cache_put(&self, _key: String, _value: Arc<ConfiguredTarget>) {}

    /// Resolves the toolchains `toolchains` (raw `aspect(toolchains = ...)` values) for an aspect
    /// applied to `owner` in `cfg`: toolchain type → `ToolchainInfo` plus extra Make variables.
    #[allow(clippy::type_complexity)]
    fn aspect_toolchains(
        &self,
        _owner: &Label,
        _toolchains: &[Value],
        _exec_compatible_with: &[String],
        _cfg: &BuildConfigurationKey,
    ) -> Result<(BTreeMap<String, Value>, BTreeMap<String, String>), String> {
        Err("toolchain resolution is not available for aspects".to_string())
    }
}

/// Inputs of rule analysis beyond the rule itself.
#[derive(Clone)]
pub struct AnalysisInputs {
    pub resolver: Arc<dyn DepResolver>,
    /// Resolved toolchains: toolchain-type label → `ToolchainInfo` instance (or `None` for
    /// optional toolchains that did not resolve). Keyed by canonical label string.
    pub toolchains: BTreeMap<String, Value>,
    /// Extra Make variables (from toolchains' `TemplateVariableInfo`).
    pub extra_make_variables: BTreeMap<String, String>,
    pub workspace_name: String,
    /// `--allow_unresolved_symlinks`.
    pub allow_unresolved_symlinks: bool,
    /// `--incompatible_compact_repo_mapping_manifest` (default true).
    pub compact_repo_mapping: bool,
    /// The execution platform label (canonical) used for `cfg = "exec"` dependencies
    /// (`ExecutionTransitionFactory`); defaults to the host platform.
    pub exec_platform: Option<String>,
    /// Exec group name → (toolchain type → `ToolchainInfo`).
    pub exec_group_toolchains: BTreeMap<String, BTreeMap<String, Value>>,
    /// Exec group name → execution platform label.
    pub exec_group_platforms: BTreeMap<String, String>,
    /// `--incompatible_allow_tags_propagation` (default true).
    pub allow_tags_propagation: bool,
    /// `--experimental_sibling_repository_layout` (default false).
    pub sibling_repository_layout: bool,
}

impl AnalysisInputs {
    pub fn new(resolver: Arc<dyn DepResolver>) -> Self {
        Self {
            resolver,
            toolchains: BTreeMap::new(),
            extra_make_variables: BTreeMap::new(),
            workspace_name: "_main".to_string(),
            allow_unresolved_symlinks: false,
            compact_repo_mapping: true,
            exec_platform: None,
            exec_group_toolchains: BTreeMap::new(),
            exec_group_platforms: BTreeMap::new(),
            allow_tags_propagation: true,
            sibling_repository_layout: false,
        }
    }
}

// ------------------------------------------------------------------------------------------
// Value helpers

pub fn mk_list(v: Vec<Value>) -> Value {
    let val = Value::List(Arc::new(Mutex::new(StarlarkList::new(v))));
    val.freeze();
    val
}

pub fn mk_dict(items: Vec<(Value, Value)>) -> Value {
    let val = Value::Dict(Arc::new(Mutex::new(
        StarlarkDict::from_entries(items).unwrap_or_default(),
    )));
    val.freeze();
    val
}

pub fn mk_struct(fields: Vec<(&str, Value)>) -> Value {
    Value::Struct(Arc::new(StructValue::new(
        fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    )))
}

pub fn mk_depset(
    order: DepsetOrder,
    direct: Vec<Value>,
    transitive: Vec<Arc<Depset>>,
) -> Result<Value, StarlarkError> {
    Ok(Value::Depset(Arc::new(Depset::new(
        order, direct, transitive,
    )?)))
}

/// Depset of files.
pub fn files_depset(files: &[Artifact]) -> Value {
    Value::Depset(Arc::new(
        Depset::new(
            DepsetOrder::Default,
            files.iter().map(file_value).collect(),
            Vec::new(),
        )
        .expect("files are hashable"),
    ))
}

pub fn provider_instance(name: &str, fields: Vec<(&str, Value)>) -> Value {
    Value::ProviderInstance {
        provider_name: name.to_string(),
        fields: fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    }
}

fn err<T>(m: impl Into<String>) -> Result<T, StarlarkError> {
    Err(StarlarkError(m.into()))
}

fn sequence(v: &Value) -> Option<Vec<Value>> {
    match v {
        Value::List(_) | Value::Tuple(_) => zel_starlark::eval::builtins::value_to_sequence(v).ok(),
        Value::Depset(d) => Some(d.to_list()),
        _ => None,
    }
}

fn type_name(v: &Value) -> &'static str {
    v.type_name()
}

fn files_in(v: &Value, what: &str) -> Result<Vec<Artifact>, StarlarkError> {
    let items = match sequence(v) {
        Some(i) => i,
        None => {
            return err(format!(
                "expected value of type 'sequence or depset' for {what} but got {} instead",
                type_name(v)
            ))
        }
    };
    items
        .iter()
        .map(|i| {
            artifact_of(i).ok_or_else(|| {
                StarlarkError(format!(
                    "expected value of type 'File' for a member of parameter '{what}' but got {} instead",
                    type_name(i)
                ))
            })
        })
        .collect()
}

fn dedup(files: Vec<Artifact>) -> Vec<Artifact> {
    let mut seen = BTreeSet::new();
    files
        .into_iter()
        .filter(|a| seen.insert(a.exec_path.path_string().to_string()))
        .collect()
}

/// Legacy-model files of a configured target (`FileProvider.getFilesToBuild`).
pub fn target_files(ct: &ConfiguredTarget) -> Vec<Artifact> {
    ct.default_info.files.clone()
}

// ------------------------------------------------------------------------------------------
// Context state

struct CtxState {
    rule: RuleInstance,
    label: Label,
    cfg: BuildConfigurationKey,
    inputs: AnalysisInputs,
    pkg_dir: PathFragment,
    output_to_genfiles: bool,
    exec_groups: BTreeSet<String>,
    /// attribute name → resolved dependency targets.
    deps: BTreeMap<String, Vec<Arc<ConfiguredTarget>>>,
    attr_struct: Value,
    file_struct: Value,
    files_struct: Value,
    executable_struct: Value,
    outputs_struct: Value,
    /// Artifacts predeclared by the rule class (`outputs`, `attr.output`).
    predeclared: Vec<Artifact>,
    default_executable: Option<Artifact>,
    default_executable_accessed: Mutex<bool>,
    actions: Mutex<Vec<Action>>,
    graph: Mutex<ActionGraph>,
    declared_trees: Mutex<BTreeSet<String>>,
    declared_files: Mutex<BTreeSet<String>>,
    declared_artifacts: Mutex<Vec<Artifact>>,
    rule_errors: Mutex<Vec<String>>,
    make_vars: BTreeMap<String, String>,
    frozen: Mutex<bool>,
    /// `ctx.rule` for aspects (`attr`, `file`, `files`, `executable`, `kind`).
    rule_struct: Option<Value>,
    /// `ctx.aspect_ids` for aspects.
    aspect_ids: Vec<String>,
    /// Fragments declared by the running aspect (`aspect(fragments = [...])`); empty for rules.
    aspect_fragments: Vec<String>,
    /// Toolchains declared by the running aspect (`aspect(toolchains = [...])`); empty for rules.
    aspect_toolchains_declared: Vec<Value>,
    /// Base target's toolchains / exec groups exposed on `ctx.rule.toolchains` / `ctx.rule.exec_groups`.
    base_toolchains: BTreeMap<String, Value>,
    base_exec_groups: BTreeSet<String>,
    base_exec_group_toolchains: BTreeMap<String, BTreeMap<String, Value>>,
    base_exec_group_platforms: BTreeMap<String, String>,
    /// `ctx.split_attr` (rules only; `None` in aspects).
    split_attr: Value,
    /// True while executing a `subrule(...)` implementation (`SubruleContext`: always `ArtifactRoot::Bin`).
    in_subrule: std::sync::atomic::AtomicBool,
}

impl CtxState {
    /// `RepositoryMapping` for the package of this target (used for label parsing in
    /// `expand_location`, `package_relative_label`, `toolchains[...]`, etc.).
    pub fn repo_mapping(&self) -> zel_cmdline::RepositoryMapping {
        let owner = self.label.package_identifier().repository();
        let entries = self.inputs.resolver.repo_mapping_entries(owner.name());
        let ordered: Vec<(String, zel_cmdline::RepositoryName)> = entries
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
        zel_cmdline::RepositoryMapping::new_ordered(ordered, owner.clone())
    }

    /// Repo-mapping manifest entries for `runfiles` of this target (`RepoMappingManifestAction`).
    pub fn repo_mapping_for(&self, rf: &Runfiles) -> Vec<(String, String, String)> {
        let resolver = self.inputs.resolver.clone();
        crate::repo_mapping::repo_mapping_entries(
            &move |r: &str| resolver.repo_mapping_entries(r),
            &self.inputs.workspace_name,
            rf,
            self.label.package_identifier().repository().name(),
            self.inputs.compact_repo_mapping,
        )
    }

    fn bin_dir(&self) -> String {
        format!("bazel-out/{}/bin", self.cfg.mnemonic())
    }
    fn genfiles_dir(&self) -> String {
        format!("bazel-out/{}/bin", self.cfg.mnemonic())
    }
    fn root(&self) -> ArtifactRoot {
        if self.output_to_genfiles && !self.in_subrule.load(std::sync::atomic::Ordering::Relaxed) {
            ArtifactRoot::Genfiles
        } else {
            ArtifactRoot::Bin
        }
    }
    fn derived(&self, rel: PathFragment) -> Artifact {
        Artifact::derived_in_root(&self.cfg.mnemonic(), self.root(), rel, self.label.clone())
    }
    fn check_mutable(&self, what: &str) -> Result<(), StarlarkError> {
        if *self.frozen.lock().unwrap() {
            return err(format!(
                "cannot call {what}() after the rule implementation function has returned (the context is frozen)"
            ));
        }
        Ok(())
    }
    fn register(&self, mut a: Action) -> Result<(), StarlarkError> {
        a.owner_kind = self.rule.rule_class.clone();
        a.owner_config = self.cfg.short_hash();
        if a.exec_platform.is_none() {
            a.exec_platform = a
                .exec_group
                .as_deref()
                .and_then(|g| self.inputs.exec_group_platforms.get(g))
                .or(self.inputs.exec_platform.as_ref())
                .cloned();
        }
        a.apply_configuration_env(&self.cfg);
        a.inputs = dedup(std::mem::take(&mut a.inputs));
        self.graph
            .lock()
            .unwrap()
            .register(&a)
            .map_err(|ActionConflict { message, .. }| StarlarkError(message))?;
        self.actions.lock().unwrap().push(a);
        Ok(())
    }

    /// `StarlarkProviderValidationUtil.validateArtifacts`: checks for tree/file conflicts and
    /// declared non-source artifacts with no generating action.
    fn validate_artifacts(&self) -> Result<(), String> {
        let trees = self.declared_trees.lock().unwrap();
        let files = self.declared_files.lock().unwrap();
        let conflicts: Vec<String> = self
            .declared_artifacts
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.is_tree_artifact && files.contains(a.exec_path.path_string()))
            .map(|a| a.root_relative_path.path_string().to_string())
            .collect();
        if !conflicts.is_empty() {
            return Err(format!(
                "The following directories were also declared as files:\n{}",
                conflicts.join("\n")
            ));
        }
        drop(trees);
        drop(files);

        let graph = self.graph.lock().unwrap();
        let mut orphans: Vec<String> = Vec::new();
        let mut seen = BTreeSet::new();
        let mut check_art = |a: &Artifact| {
            if !a.is_source
                && seen.insert(a.exec_path.path_string().to_string())
                && graph.generating_action(a.exec_path.path_string()).is_none()
            {
                orphans.push(a.root_relative_path.path_string().to_string());
            }
        };
        for a in self.declared_artifacts.lock().unwrap().iter() {
            check_art(a);
        }
        for a in &self.predeclared {
            check_art(a);
        }
        if *self.default_executable_accessed.lock().unwrap() {
            if let Some(exe) = &self.default_executable {
                check_art(exe);
            }
        }
        if !orphans.is_empty() {
            return Err(format!(
                "The following files have no generating action:\n{}",
                orphans.join("\n")
            ));
        }
        Ok(())
    }
}

fn arg<'a>(
    pos: &'a [Value],
    kw: &'a BTreeMap<String, Value>,
    i: usize,
    name: &str,
) -> Option<&'a Value> {
    kw.get(name).or_else(|| pos.get(i))
}

fn check_kwargs(
    func: &str,
    pos: &[Value],
    kw: &BTreeMap<String, Value>,
    names: &[&str],
) -> Result<(), StarlarkError> {
    for k in kw.keys() {
        if !names.contains(&k.as_str()) {
            return err(format!("{func}() got unexpected keyword argument '{k}'"));
        }
    }
    if pos.len() > names.len() {
        return err(format!(
            "{func}() accepts no more than {} positional argument{} but got {}",
            names.len(),
            if names.len() == 1 { "" } else { "s" },
            pos.len()
        ));
    }
    for (i, _) in pos.iter().enumerate() {
        if kw.contains_key(names[i]) {
            return err(format!(
                "{func}() got multiple values for parameter '{}'",
                names[i]
            ));
        }
    }
    Ok(())
}

fn none_or(v: Option<&Value>) -> Option<&Value> {
    match v {
        Some(Value::None) | None => None,
        Some(x) => Some(x),
    }
}

fn want_str(v: &Value, func: &str, param: &str) -> Result<String, StarlarkError> {
    match v {
        Value::String(s) => Ok(s.clone()),
        o => err(format!(
            "in call to {func}(), parameter '{param}' got value of type '{}', want 'string'",
            type_name(o)
        )),
    }
}

fn want_file(v: &Value, func: &str, param: &str) -> Result<Artifact, StarlarkError> {
    artifact_of(v).ok_or_else(|| {
        StarlarkError(format!(
            "in call to {func}(), parameter '{param}' got value of type '{}', want 'File'",
            type_name(v)
        ))
    })
}

fn str_dict(v: &Value, what: &str) -> Result<BTreeMap<String, String>, StarlarkError> {
    let Value::Dict(d) = v else {
        return err(format!(
            "expected value of type 'dict' for {what} but got {}",
            type_name(v)
        ));
    };
    let mut m = BTreeMap::new();
    for (k, v) in d.lock().unwrap().items() {
        match (k, v) {
            (Value::String(k), Value::String(v)) => {
                m.insert(k, v);
            }
            _ => return err(format!("expected dict of strings for {what}")),
        }
    }
    Ok(m)
}

/// Like [`str_dict`] but keeps the dict's insertion order.
fn str_dict_ordered(v: &Value, what: &str) -> Result<Vec<(String, String)>, StarlarkError> {
    let Value::Dict(d) = v else {
        return err(format!(
            "expected value of type 'dict' for {what} but got {}",
            type_name(v)
        ));
    };
    let mut m: Vec<(String, String)> = Vec::new();
    for (k, v) in d.lock().unwrap().items() {
        match (k, v) {
            (Value::String(k), Value::String(v)) => m.push((k, v)),
            _ => return err(format!("expected dict of strings for {what}")),
        }
    }
    Ok(m)
}

include!("starlark_rule_host.rs");
include!("cc_native.rs");
include!("java_native.rs");
include!("py_native.rs");
include!("starlark_rule_actions.rs");
include!("starlark_rule_driver.rs");
include!("starlark_rule_aspect.rs");

#[cfg(test)]
mod java_native_tests {
    use super::*;

    struct MapPlatformResolver {
        platforms: BTreeMap<String, Arc<ConfiguredTarget>>,
    }

    impl DepResolver for MapPlatformResolver {
        fn configured_target(
            &self,
            label: &Label,
            _cfg: &BuildConfigurationKey,
        ) -> Result<Arc<ConfiguredTarget>, String> {
            self.platforms
                .get(&label.canonical_form())
                .cloned()
                .ok_or_else(|| format!("missing platform {}", label.canonical_form()))
        }
    }

    fn make_platform_ct(
        label_str: &str,
        os_constraint: Option<(&str, &str)>,
    ) -> Arc<ConfiguredTarget> {
        let label = Label::parse_canonical(label_str).unwrap();
        let mut ct = ConfiguredTarget::new(
            label.clone(),
            BuildConfigurationKey::target("k8", "fastbuild").to_core_only(),
            "platform".into(),
        );
        let mut constraints = Vec::new();
        if let Some((setting, value)) = os_constraint {
            constraints.push((
                Value::Label(Label::parse_canonical(setting).unwrap()),
                Value::Label(Label::parse_canonical(value).unwrap()),
            ));
        }
        ct.providers.insert(
            "PlatformInfo".into(),
            provider_instance(
                "PlatformInfo",
                vec![
                    ("label", Value::Label(label)),
                    ("constraints", mk_dict(constraints)),
                ],
            ),
        );
        Arc::new(ct)
    }

    #[test]
    fn strict_deps_mode_normalizes_default_and_strict_and_rejects_unknown() {
        assert_eq!(normalize_strict_deps_mode(None).unwrap(), "ERROR");
        assert_eq!(
            normalize_strict_deps_mode(Some(&Value::String("default".into()))).unwrap(),
            "ERROR"
        );
        assert_eq!(
            normalize_strict_deps_mode(Some(&Value::String("strict".into()))).unwrap(),
            "ERROR"
        );
        assert_eq!(
            normalize_strict_deps_mode(Some(&Value::String("warn".into()))).unwrap(),
            "WARN"
        );
        assert_eq!(
            normalize_strict_deps_mode(Some(&Value::String("off".into()))).unwrap(),
            "OFF"
        );
        assert!(normalize_strict_deps_mode(Some(&Value::String("bogus".into()))).is_err());
    }

    #[test]
    fn utf8_env_follows_execution_or_host_os_not_target_platforms() {
        let mac_exec = make_platform_ct(
            "//pkg:macos_exec",
            Some(("@@platforms+//os:os", "@@platforms+//os:macos")),
        );
        let osx_exec = make_platform_ct(
            "//pkg:osx_exec",
            Some(("@@platforms//os:os", "@@platforms//os:osx")),
        );
        let linux_exec = make_platform_ct(
            "//pkg:linux_exec",
            Some(("@@platforms//os:os", "@@platforms//os:linux")),
        );
        let bare_exec = make_platform_ct("//pkg:bare_exec", None);

        let resolver: Arc<dyn DepResolver> = Arc::new(MapPlatformResolver {
            platforms: BTreeMap::from([
                (mac_exec.label.canonical_form(), mac_exec.clone()),
                (osx_exec.label.canonical_form(), osx_exec.clone()),
                (linux_exec.label.canonical_form(), linux_exec.clone()),
                (bare_exec.label.canonical_form(), bare_exec.clone()),
            ]),
        });

        // Cross-compiling to a Linux target platform on a macOS host (default/unresolved host exec
        // platform) must emit `en_US.UTF-8`, never `C.UTF-8`.
        let mut cfg_linux_target = BuildConfigurationKey::target("k8", "fastbuild");
        cfg_linux_target.platforms = vec!["@@_main//pkg:linux_target".into()];
        let mut inputs_default = AnalysisInputs::new(resolver.clone());
        inputs_default.exec_platform = Some("@@platforms//host:host".into());
        assert_eq!(
            utf8_env_for_inputs(&cfg_linux_target, &inputs_default, true)["LC_CTYPE"],
            "en_US.UTF-8"
        );
        assert_eq!(
            utf8_env_for_inputs(&cfg_linux_target, &inputs_default, false)["LC_CTYPE"],
            "C.UTF-8"
        );

        // Cross-compiling to a macOS target platform on a Linux host must emit `C.UTF-8`.
        let mut cfg_macos_target = BuildConfigurationKey::target("darwin_arm64", "fastbuild");
        cfg_macos_target.platforms = vec!["@@_main//pkg:macos_target".into()];
        assert_eq!(
            utf8_env_for_inputs(&cfg_macos_target, &inputs_default, false)["LC_CTYPE"],
            "C.UTF-8"
        );

        // Explicit execution platform OS constraints override host OS regardless of target platform.
        let mut inputs_mac_exec = AnalysisInputs::new(resolver.clone());
        inputs_mac_exec.exec_platform = Some(mac_exec.label.canonical_form());
        assert_eq!(
            utf8_env_for_inputs(&cfg_linux_target, &inputs_mac_exec, false)["LC_CTYPE"],
            "en_US.UTF-8"
        );
        let mut inputs_osx_exec = AnalysisInputs::new(resolver.clone());
        inputs_osx_exec.exec_platform = Some(osx_exec.label.canonical_form());
        assert_eq!(
            utf8_env_for_inputs(&cfg_linux_target, &inputs_osx_exec, false)["LC_CTYPE"],
            "en_US.UTF-8"
        );

        let mut inputs_linux_exec = AnalysisInputs::new(resolver.clone());
        inputs_linux_exec.exec_platform = Some(linux_exec.label.canonical_form());
        assert_eq!(
            utf8_env_for_inputs(&cfg_macos_target, &inputs_linux_exec, true)["LC_CTYPE"],
            "C.UTF-8"
        );

        // Execution platform with no `@platforms//os:os` constraint falls back to host OS
        // (`ConstraintConstants.getOsFromConstraintsOrHost`).
        let mut inputs_bare_exec = AnalysisInputs::new(resolver.clone());
        inputs_bare_exec.exec_platform = Some(bare_exec.label.canonical_form());
        assert_eq!(
            utf8_env_for_inputs(&cfg_linux_target, &inputs_bare_exec, true)["LC_CTYPE"],
            "en_US.UTF-8"
        );
        assert_eq!(
            utf8_env_for_inputs(&cfg_macos_target, &inputs_bare_exec, false)["LC_CTYPE"],
            "C.UTF-8"
        );

        // With `--incompatible_auto_exec_groups=true`, the Java toolchain exec group platform wins.
        let mut cfg_auto_eg = cfg_linux_target.clone();
        cfg_auto_eg
            .options
            .insert("incompatible_auto_exec_groups".into(), "true".into());
        let mut inputs_auto_eg = AnalysisInputs::new(resolver);
        inputs_auto_eg.exec_platform = Some(linux_exec.label.canonical_form());
        inputs_auto_eg.exec_group_platforms.insert(
            "@@bazel_tools//tools/jdk:toolchain_type".into(),
            mac_exec.label.canonical_form(),
        );
        assert_eq!(
            utf8_env_for_inputs(&cfg_auto_eg, &inputs_auto_eg, false)["LC_CTYPE"],
            "en_US.UTF-8"
        );
    }
}
