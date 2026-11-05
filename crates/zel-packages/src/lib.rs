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

//! Bazel Package model (`Package`, `Target`, `Rule`, `InputFile`, `OutputFile`) and Starlark
//! `BUILD` / `.bzl` evaluator.
//! Mirrors `src/main/java/com/google/devtools/build/lib/packages`.

pub mod aspect;
pub mod attr;
pub mod builtins;
pub mod glob;
pub mod loader;
pub mod lookup;
pub mod pkg_spec;
pub mod provider;
pub mod resolve;
pub mod rule_class;
pub mod rule_defs_gen;

use std::collections::BTreeMap;
use std::sync::Arc;
use zel_cmdline::{Label, PackageIdentifier};
use zel_starlark::{LoadStatement, Value};
use zel_vfs::{FileSystem, PathFragment};

pub use attr::{AttrType, AttrValue, Attribute};
pub use loader::{BzlOverride, LoaderConfig, PackageCache, PackageLoadError, PackageLoader};
pub use lookup::{PackageEvent, PackageLookupConfig, RepoResolver, SimpleRepoResolver};
pub use resolve::{PackageErrors, PatternError, PatternResolver, ResolveOutcome};
pub use rule_class::RuleClass;

/// Extra per-rule information of the typed model (`Rule.java`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuleExtra {
    /// The rule class (schema) this rule was instantiated from.
    pub rule_class: Option<Arc<RuleClass>>,
    /// Explicitly-set, type-converted attribute values (defaults are *not* stored; see
    /// [`RuleInstance::attr_or_default`]).
    pub attr_values: BTreeMap<String, AttrValue>,
    /// `/path/BUILD:line:col` of the call that created the rule.
    pub location: Option<String>,
    /// `generator_name` (the enclosing macro's `name`), if created by a macro.
    pub generator_name: Option<String>,
    /// `generator_function` (the macro function name).
    pub generator_function: Option<String>,
    /// `generator_location` (BUILD file location of the macro call).
    pub generator_location: Option<String>,
    /// Call stack frames from BUILD down to instantiation.
    pub instantiation_stack: Vec<(String, String)>,
    /// Name of the symbolic macro instance that declared this rule.
    pub macro_instance: Option<String>,
    /// Visibility after applying `package(default_visibility = ...)`.
    pub effective_visibility: Vec<Label>,
    /// `true` if the `visibility` attribute was set explicitly.
    pub visibility_explicit: bool,
    /// Attributes whose value comes from a `package()` default, not from the rule call (they are
    /// present in `attr_values` but not *explicitly specified*).
    pub pkg_default_attrs: Vec<String>,
    /// The `features` the rule call itself passed, when the package merged its own features
    /// into `attr_values["features"]` (query shows only the rule's own list).
    pub own_features: Option<AttrValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleInstance {
    pub label: Label,
    pub rule_class: String,
    pub attrs: BTreeMap<String, Value>,
    pub srcs: Vec<String>,
    pub deps: Vec<String>,
    pub outs: Vec<String>,
    pub tools: Vec<String>,
    pub data: Vec<String>,
    pub visibility: Vec<String>,
    pub cmd: Option<String>,
    pub executable: bool,
    pub testonly: bool,
    pub extra: RuleExtra,
}

impl RuleInstance {
    pub fn is_test_rule(&self) -> bool {
        self.rule_class.ends_with("_test") || self.rule_class == "test_suite"
    }

    pub fn is_binary_rule(&self) -> bool {
        self.rule_class.ends_with("_binary") || self.executable
    }

    /// Labels this rule depends on: the labels of all label-typed attributes (including every
    /// `select()` branch and key) except visibility/compatibility metadata and outputs.
    pub fn dependency_labels(&self) -> Vec<Label> {
        const NON_DEPS: &[&str] = &[
            "visibility",
            "compatible_with",
            "restricted_to",
            "package_metadata",
            "aspect_hints",
        ];
        let mut out: Vec<Label> = Vec::new();
        for (name, v) in &self.extra.attr_values {
            if NON_DEPS.contains(&name.as_str()) {
                continue;
            }
            let is_output = self
                .extra
                .rule_class
                .as_ref()
                .and_then(|rc| rc.attr(name))
                .map(|a| matches!(a.ty, AttrType::Output | AttrType::OutputList))
                .unwrap_or(false);
            if is_output {
                continue;
            }
            for l in v.labels() {
                if !out.contains(&l) {
                    out.push(l);
                }
            }
        }
        out
    }

    /// The value of attribute `name`: explicit value, else the rule class default.
    pub fn attr_or_default(&self, name: &str) -> Option<AttrValue> {
        if let Some(v) = self.extra.attr_values.get(name) {
            return Some(v.clone());
        }
        self.extra
            .rule_class
            .as_ref()
            .and_then(|rc| rc.attr(name))
            .map(|a| a.default_value())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Rule(RuleInstance),
    InputFile {
        label: Label,
    },
    OutputFile {
        label: Label,
        generating_rule: Label,
    },
    PackageGroup {
        label: Label,
        packages: Vec<String>,
    },
    /// `environment_group()` (a target of kind `environment group`).
    EnvironmentGroup {
        label: Label,
        environments: Vec<Label>,
        defaults: Vec<Label>,
    },
}

impl Target {
    pub fn label(&self) -> &Label {
        match self {
            Self::Rule(r) => &r.label,
            Self::InputFile { label } => label,
            Self::OutputFile { label, .. } => label,
            Self::PackageGroup { label, .. } => label,
            Self::EnvironmentGroup { label, .. } => label,
        }
    }

    pub fn target_kind(&self) -> String {
        match self {
            Self::Rule(r) => format!("{} rule", r.rule_class),
            Self::InputFile { .. } => "source file".to_string(),
            Self::OutputFile { .. } => "generated file".to_string(),
            Self::PackageGroup { .. } => "package group".to_string(),
            Self::EnvironmentGroup { .. } => "environment group".to_string(),
        }
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        match self {
            Self::Rule(r) => {
                if let Some(crate::attr::AttrValue::StrList(l)) = r.attr_or_default("tags") {
                    for s in l {
                        if s == tag {
                            return true;
                        }
                    }
                }
                false
            }
            _ => false,
        }
    }
}

/// Package-wide defaults set by `package()` / `repo()` (`PackageArgs.java`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PackageArgs {
    pub default_visibility: Option<Vec<Label>>,
    pub default_testonly: Option<bool>,
    pub default_deprecation: Option<String>,
    pub default_compatible_with: Vec<Label>,
    pub default_restricted_to: Vec<Label>,
    pub features: Vec<String>,
    pub licenses: Vec<String>,
    pub default_package_metadata: Vec<Label>,
    pub default_hdrs_check: Option<String>,
    pub transitive_visibility: Option<Label>,
}

/// An evaluated Bazel `BUILD` / `BUILD.bazel` package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub id: PackageIdentifier,
    pub build_file: PathFragment,
    pub targets: BTreeMap<String, Target>,
    pub loads: Vec<LoadStatement>,
    pub default_visibility: Vec<String>,
    pub args: PackageArgs,
    /// Visibility declared through `exports_files(..., visibility = ...)`, by file name.
    pub exported_files: BTreeMap<String, Vec<Label>>,
    /// `package_group` includes, by group name.
    pub group_includes: BTreeMap<String, Vec<Label>>,
    /// `/path/BUILD:line:col` of the call that declared each target, by target name (for
    /// package groups, files declared by `exports_files` and generated files).
    pub target_locations: BTreeMap<String, String>,
    /// `environment_group()` declarations: name → (environments, defaults).
    pub environment_groups: BTreeMap<String, (Vec<Label>, Vec<Label>)>,
    /// Targets declared by a symbolic macro outside that macro's namespace
    /// (`target name → macro instance name`); enforced at analysis time.
    pub macro_namespace_violating_targets: BTreeMap<String, String>,
    /// Messages produced while loading (errors, warnings, `print()` output).
    pub events: Vec<PackageEvent>,
    /// `true` if loading reported errors (the package may be partial).
    pub contains_errors: bool,
}

impl Package {
    /// Evaluates a `BUILD` file's Starlark source (and any `.bzl` files it loads) into a `Package`.
    ///
    /// Convenience wrapper over [`PackageLoader`] for a workspace rooted at `workspace_root`.
    /// Returns `Err` with the rendered error events if the package contains errors.
    pub fn evaluate_build_file(
        id: PackageIdentifier,
        workspace_root: &PathFragment,
        build_file: PathFragment,
        source: &str,
        fs: Arc<dyn FileSystem>,
    ) -> Result<Self, String> {
        let resolver: Arc<dyn lookup::RepoResolver> =
            Arc::new(SimpleRepoResolver::new(workspace_root.clone()));
        Self::evaluate_build_file_with(id, build_file, source, fs, resolver)
    }

    /// Like [`evaluate_build_file`](Self::evaluate_build_file) with an explicit external
    /// repository resolver.
    pub fn evaluate_build_file_with(
        id: PackageIdentifier,
        build_file: PathFragment,
        source: &str,
        fs: Arc<dyn FileSystem>,
        resolver: Arc<dyn lookup::RepoResolver>,
    ) -> Result<Self, String> {
        let loader = PackageLoader::new(LoaderConfig::new(fs, resolver));
        let pkg = loader.evaluate_source(&id, &build_file, source);
        if pkg.contains_errors {
            return Err(pkg
                .events
                .iter()
                .filter(|e| e.kind == lookup::EventKind::Error)
                .map(|e| e.render())
                .collect::<Vec<_>>()
                .join("\n"));
        }
        Ok(pkg)
    }

    pub fn rules(&self) -> impl Iterator<Item = &RuleInstance> {
        self.targets.values().filter_map(|t| match t {
            Target::Rule(r) => Some(r),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests;
