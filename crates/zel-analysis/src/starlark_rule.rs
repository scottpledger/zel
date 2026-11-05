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
use crate::location::{expand_locations, expand_make_variables, LocationMode};
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
        }
    }
}

// ------------------------------------------------------------------------------------------
// Value helpers

pub fn mk_list(v: Vec<Value>) -> Value {
    Value::List(Arc::new(Mutex::new(StarlarkList::new(v))))
}

pub fn mk_dict(items: Vec<(Value, Value)>) -> Value {
    Value::Dict(Arc::new(Mutex::new(StarlarkDict::from_entries(items).unwrap_or_default())))
}

pub fn mk_struct(fields: Vec<(&str, Value)>) -> Value {
    Value::Struct(Arc::new(StructValue::new(
        fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    )))
}

pub fn mk_depset(
    order: DepsetOrder,
    direct: Vec<Value>,
    transitive: Vec<Arc<Depset>>,
) -> Result<Value, StarlarkError> {
    Ok(Value::Depset(Arc::new(Depset::new(order, direct, transitive)?)))
}

/// Depset of files.
pub fn files_depset(files: &[Artifact]) -> Value {
    Value::Depset(Arc::new(
        Depset::new(DepsetOrder::Default, files.iter().map(file_value).collect(), Vec::new())
            .expect("files are hashable"),
    ))
}

pub fn provider_instance(name: &str, fields: Vec<(&str, Value)>) -> Value {
    Value::ProviderInstance {
        provider_name: name.to_string(),
        fields: fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
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
    make_vars: BTreeMap<String, String>,
    frozen: Mutex<bool>,
    /// `ctx.rule` for aspects (`attr`, `file`, `files`, `executable`, `kind`).
    rule_struct: Option<Value>,
    /// `ctx.aspect_ids` for aspects.
    aspect_ids: Vec<String>,
    /// Fragments declared by the running aspect (`aspect(fragments = [...])`); empty for rules.
    aspect_fragments: Vec<String>,
    /// `ctx.split_attr` (rules only; `None` in aspects).
    split_attr: Value,
}

impl CtxState {
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
        if self.output_to_genfiles {
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
            return err(format!("{func}() got multiple values for parameter '{}'", names[i]));
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
        return err(format!("expected value of type 'dict' for {what} but got {}", type_name(v)));
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
        return err(format!("expected value of type 'dict' for {what} but got {}", type_name(v)));
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
