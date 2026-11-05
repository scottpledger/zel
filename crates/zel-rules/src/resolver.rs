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

//! A package-loader-backed, memoizing configured-target analyzer ([`TargetResolver`]) that
//! implements [`DepResolver`], including cycle detection and visibility/testonly checks
//! (`CommonPrerequisiteValidator`).

use crate::analyze_target_with;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, Weak};
use zel_analysis::starlark_rule::DepResolver;
use zel_analysis::{Artifact, BuildConfigurationKey, ConfiguredTarget, DefaultInfo};
use zel_cmdline::{Label, PackageIdentifier};
use zel_packages::{AttrValue, Package, PackageLoader, RuleInstance, Target};

/// Analysis-wide configuration (registered toolchains, platforms, ...).
#[derive(Debug, Clone, Default)]
pub struct AnalysisConfig {
    /// Registered `toolchain()` targets in priority order (`register_toolchains`,
    /// `--extra_toolchains`).
    pub registered_toolchains: Vec<Label>,
    /// `--extra_execution_platforms`.
    pub extra_execution_platforms: Vec<Label>,
    /// Toolchain target patterns (`--extra_toolchains` first, then `register_toolchains` of the
    /// module graph) expanded lazily by `expander` on the first toolchain resolution.
    pub toolchain_patterns: Vec<String>,
    /// Execution platform patterns (`--extra_execution_platforms`, then
    /// `register_execution_platforms`), expanded lazily like `toolchain_patterns`.
    pub execution_platform_patterns: Vec<String>,
    pub expander: Option<Arc<dyn PatternExpander>>,
    pub allow_unresolved_symlinks: bool,
    /// `--noincompatible_compact_repo_mapping_manifest`.
    pub no_compact_repo_mapping: bool,
    /// `--check_visibility` (default true).
    pub check_visibility: bool,
    /// `--host_platform` (default: the auto-detected host platform).
    pub host_platform: Option<Label>,
    /// Canonical name of the `platforms` repository as resolved by Bzlmod (default `platforms`).
    pub platforms_repo: String,
    /// `--shell_executable` (`ShellConfiguration`): the shell genrules run under.
    pub shell_executable: Option<String>,
}

/// Expands registered toolchain / execution-platform target patterns into rule labels.
pub trait PatternExpander: Send + Sync + std::fmt::Debug {
    /// Targets of rule class `rule_class` matched by `patterns`, in order.
    fn expand(&self, patterns: &[String], rule_class: &str) -> Result<Vec<Label>, String>;
}

impl AnalysisConfig {
    /// All registered toolchains in priority order: explicit labels, then expanded patterns.
    pub fn all_registered_toolchains(&self) -> Result<Vec<Label>, String> {
        let mut out = self.registered_toolchains.clone();
        if let (Some(e), false) = (&self.expander, self.toolchain_patterns.is_empty()) {
            for l in e.expand(&self.toolchain_patterns, "toolchain")? {
                if !out.contains(&l) {
                    out.push(l);
                }
            }
        }
        Ok(out)
    }

    /// Label of the execution platform used for `cfg = "exec"` dependencies of rules that do not
    /// resolve toolchains: the first registered execution platform, else the host platform.
    pub fn default_exec_platform(&self) -> String {
        if let Ok(v) = self.all_execution_platforms() {
            if let Some(f) = v.first() {
                return f.canonical_form();
            }
        }
        match &self.host_platform {
            Some(h) => h.canonical_form(),
            None => format!("@@{}//host:host", self.platforms_repo.trim_start_matches('@')),
        }
    }

    /// All registered execution platforms in priority order (excluding the host fallback).
    pub fn all_execution_platforms(&self) -> Result<Vec<Label>, String> {
        let mut out = self.extra_execution_platforms.clone();
        if let (Some(e), false) = (&self.expander, self.execution_platform_patterns.is_empty()) {
            for l in e.expand(&self.execution_platform_patterns, "platform")? {
                if !out.contains(&l) {
                    out.push(l);
                }
            }
        }
        Ok(out)
    }

    pub fn new() -> Self {
        Self { check_visibility: true, platforms_repo: "platforms".to_string(), ..Default::default() }
    }
}

pub struct TargetResolver {
    loader: Arc<PackageLoader>,
    pub config: AnalysisConfig,
    packages: Mutex<HashMap<PackageIdentifier, Arc<Package>>>,
    cache: Mutex<HashMap<(String, String), Arc<ConfiguredTarget>>>,
    stack: Mutex<Vec<(String, String)>>,
    /// The configurations of `stack` (for the full checksums printed in cycle reports).
    stack_cfg: Mutex<Vec<BuildConfigurationKey>>,
    pkg_sets: Mutex<HashMap<(String, String), zel_analysis::nested_set::NSet<zel_analysis::PkgMeta>>>,
    /// `CyclesReporter`: cycles (as sorted `label (config)` sets) already printed in full.
    reported_cycles: Mutex<std::collections::HashSet<Vec<String>>>,
    /// Post-analysis queries print cycles in key-hash order themselves: report every cycle in full.
    defer_cycle_dedup: std::sync::atomic::AtomicBool,
    /// Analysis completion order (dependencies before dependents).
    order: Mutex<Vec<Arc<ConfiguredTarget>>>,
    /// Memoized aspect applications (see `DepResolver::aspect_cache_get`).
    aspects: Mutex<HashMap<String, Arc<ConfiguredTarget>>>,
    /// Direct dependency edges of configured targets as seen by `check_dependency` (consumer
    /// key -> deps in attribute order); used by `cquery`.
    edges: Mutex<HashMap<(String, String), Vec<(Label, BuildConfigurationKey)>>>,
    /// Analysis-time rule warnings (`ruleContext.ruleWarning`), in the order they were reported.
    warnings: Mutex<Vec<String>>,
    me: Weak<TargetResolver>,
}

fn key(label: &Label, cfg: &BuildConfigurationKey) -> (String, String) {
    (label.canonical_form(), format!("{}#{}", cfg.mnemonic(), cfg.short_hash()))
}

/// [`PatternExpander`] backed by a [`PackageLoader`] (`PatternResolver` semantics: `:all`,
/// `/...`, single labels; canonical `@@repo//...` patterns as produced by Bzlmod).
struct LoaderExpander {
    loader: Arc<PackageLoader>,
}

impl std::fmt::Debug for LoaderExpander {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LoaderExpander")
    }
}

impl PatternExpander for LoaderExpander {
    fn expand(&self, patterns: &[String], rule_class: &str) -> Result<Vec<Label>, String> {
        let pr = zel_packages::PatternResolver::new(self.loader.clone(), zel_vfs::PathFragment::empty());
        let mut out = Vec::new();
        for p in patterns {
            let explicit = !(p.ends_with(":all") || p.ends_with("/...") || p.ends_with(":*") || p.ends_with(":all-targets"));
            let outcome = pr.resolve_patterns(std::slice::from_ref(p));
            if let Some(e) = outcome.errors.first() {
                return Err(format!(
                    "invalid registered {} '{}': {}",
                    if rule_class == "toolchain" { "toolchain" } else { "execution platform" },
                    p,
                    e.message
                ));
            }
            if explicit && outcome.targets.is_empty() {
                if let Ok(l) = Label::parse_canonical(p).or_else(|_| Label::parse_canonical(&format!("//{}", p.trim_start_matches("//")))) {
                    if let Ok(pkg) = self.loader.load_package(l.package_identifier()) {
                        if let Err(msg) = pkg.get_target(l.name(), &*self.loader.config().lookup.fs) {
                            return Err(format!(
                                "invalid registered {} '{}': {}",
                                if rule_class == "toolchain" { "toolchain" } else { "execution platform" },
                                p,
                                msg
                            ));
                        }
                    }
                }
            }
            for t in outcome.targets {
                match &t {
                    Target::Rule(r) if r.rule_class == rule_class => {
                        if !out.contains(&r.label) {
                            out.push(r.label.clone());
                        }
                    }
                    other if explicit => {
                        return Err(format!(
                            "invalid registered {} '{}': target '{}' is not a {}",
                            if rule_class == "toolchain" { "toolchain" } else { "execution platform" },
                            p,
                            other.label().canonical_form(),
                            rule_class
                        ));
                    }
                    _ => {}
                }
            }
        }
        Ok(out)
    }
}

impl TargetResolver {
    pub fn new(loader: Arc<PackageLoader>, mut config: AnalysisConfig) -> Arc<Self> {
        if config.expander.is_none() {
            config.expander = Some(Arc::new(LoaderExpander { loader: loader.clone() }));
        }
        Arc::new_cyclic(|me| Self {
            loader,
            config,
            packages: Mutex::new(HashMap::new()),
            cache: Mutex::new(HashMap::new()),
            stack: Mutex::new(Vec::new()),
            stack_cfg: Mutex::new(Vec::new()),
            pkg_sets: Mutex::new(HashMap::new()),
            reported_cycles: Mutex::new(Default::default()),
            defer_cycle_dedup: std::sync::atomic::AtomicBool::new(false),
            order: Mutex::new(Vec::new()),
            aspects: Mutex::new(HashMap::new()),
            edges: Mutex::new(HashMap::new()),
            warnings: Mutex::new(Vec::new()),
            me: me.clone(),
        })
    }

    pub fn package(&self, id: &PackageIdentifier) -> Result<Arc<Package>, String> {
        if let Some(p) = self.packages.lock().unwrap().get(id) {
            return Ok(p.clone());
        }
        let pkg = self.loader.load_package(id).map_err(|e| {
            // The events (with locations and Starlark tracebacks) precede the summary message.
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
                .filter(|e| e.kind == zel_packages::lookup::EventKind::Error)
                .map(|e| e.render())
                .collect();
            return Err(format!(
                "error loading package '{}': {}",
                id.package_fragment(),
                msgs.join("\n")
            ));
        }
        let pkg = Arc::new(pkg);
        self.packages.lock().unwrap().insert(id.clone(), pkg.clone());
        Ok(pkg)
    }

    fn me(&self) -> Arc<TargetResolver> {
        self.me.upgrade().expect("resolver alive")
    }

    /// Analyses `label` in `cfg` (memoized).
    pub fn analyze(
        &self,
        label: &Label,
        cfg: &BuildConfigurationKey,
    ) -> Result<Arc<ConfiguredTarget>, String> {
        let k = key(label, cfg);
        if let Some(c) = self.cache.lock().unwrap().get(&k) {
            return Ok(c.clone());
        }
        {
            let mut st = self.stack.lock().unwrap();
            if let Some(pos) = st.iter().position(|x| *x == k) {
                let cfgs = self.stack_cfg.lock().unwrap().clone();
                let cycle_labels: Vec<String> = st[pos..].iter().map(|x| x.0.clone()).collect();
                let hash = |i: usize| cfgs.get(i).map(|c| c.checksum()).unwrap_or_default();
                // `CycleInfo` printing (`SkyframeErrorProcessor`/`CycleUtils`): the path from the
                // top-level key (whose configuration is still the pre-trimming one), then the cycle.
                let mut path: Vec<String> = Vec::new();
                if let Some(root_cfg) = cfgs.first() {
                    let pre = root_cfg.untrimmed_build_options().checksum();
                    let trimmed = root_cfg.checksum();
                    if pre != trimmed {
                        path.push(format!("    {} ({pre})", st[0].0));
                    }
                }
                for i in 0..pos {
                    path.push(format!("    {} ({})", st[i].0, hash(i)));
                }
                let cyc: Vec<String> = (pos..st.len()).map(|i| format!("{} ({})", st[i].0, hash(i))).collect();
                let first = cyc.first().cloned().unwrap_or_default();
                let first_label = cycle_labels.first().cloned().unwrap_or_default();
                let top_label = st[0].0.clone();
                drop(st);
                let mut identity = cyc.clone();
                identity.sort();
                let first_report = self.reported_cycles.lock().unwrap().insert(identity);
                let already = !first_report && !self.defer_cycle_dedup.load(std::sync::atomic::Ordering::Relaxed);
                let rule_head = |lbl: &str| {
                    Label::parse_canonical(lbl).ok().and_then(|l| {
                        let pkg = self.package(l.package_identifier()).ok()?;
                        match pkg.targets.get(l.name())? {
                            Target::Rule(r) => Some(format!(
                                "{}in {} rule {}: ",
                                r.extra.location.clone().map(|x| format!("{x}: ")).unwrap_or_default(),
                                r.rule_class,
                                lbl
                            )),
                            _ => None,
                        }
                    })
                };
                if already {
                    let head = rule_head(&top_label).unwrap_or_else(|| "in cycle: ".to_string());
                    return Err(format!("{head}cycle in dependency graph: target depends on an already-reported cycle"));
                }
                // `in <kind> rule <label>:` of the first target of the cycle.
                let head = Label::parse_canonical(&first_label)
                    .ok()
                    .and_then(|l| {
                        let pkg = self.package(l.package_identifier()).ok()?;
                        match pkg.targets.get(l.name())? {
                            Target::Rule(r) => Some(format!(
                                "{}in {} rule {}: ",
                                r.extra.location.clone().map(|x| format!("{x}: ")).unwrap_or_default(),
                                r.rule_class,
                                first_label
                            )),
                            _ => None,
                        }
                    })
                    .unwrap_or_else(|| "in cycle: ".to_string());
                let mut lines = vec![format!("{head}cycle in dependency graph:")];
                lines.extend(path);
                lines.push(format!(".-> {first}"));
                for c in cyc.iter().skip(1) {
                    lines.push(format!("|   {c}"));
                }
                lines.push(format!("`-- {first}"));
                return Err(lines.join("\n"));
            }
            st.push(k.clone());
            self.stack_cfg.lock().unwrap().push(cfg.clone());
        }
        let r = self.analyze_uncached(label, cfg);
        self.stack.lock().unwrap().pop();
        self.stack_cfg.lock().unwrap().pop();
        let ct = Arc::new(r?);
        self.cache.lock().unwrap().insert(k, ct.clone());
        self.order.lock().unwrap().push(ct.clone());
        Ok(ct)
    }

    /// Every configured target analysed so far, dependencies before dependents (the transitive
    /// closure of everything that was requested, including exec-configuration dependencies).
    /// Drains the analysis-time rule warnings reported so far.
    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut *self.warnings.lock().unwrap())
    }

    pub fn analyzed_targets(&self) -> Vec<Arc<ConfiguredTarget>> {
        let mut v = self.order.lock().unwrap().clone();
        // Targets decorated by aspects applied along dependency edges carry the aspects' actions
        // (appended last: producers are found first in the undecorated targets, then here).
        v.extend(self.aspects.lock().unwrap().values().cloned());
        v
    }

    /// Analyses `label` in `cfg` and applies the top-level `aspects` (`--aspects`, values as
    /// returned by [`PackageLoader::load_bzl_export`]) in order; `params` are the raw
    /// `--aspects_parameters` values.
    pub fn analyze_with_aspects(
        &self,
        label: &Label,
        cfg: &BuildConfigurationKey,
        aspects: &[zel_starlark::Value],
        params: &BTreeMap<String, String>,
    ) -> Result<Arc<ConfiguredTarget>, String> {
        let mut ct = self.analyze(label, cfg)?;
        if aspects.is_empty() {
            return Ok(ct);
        }
        let me: Arc<dyn DepResolver> = self.me();
        let mut inputs = zel_analysis::starlark_rule::AnalysisInputs::new(me);
        inputs.exec_platform = Some(self.config.default_exec_platform());
        for a in aspects {
            ct = zel_analysis::starlark_rule::apply_aspect_cli(&inputs, ct, a, params)
                .map_err(|e| e.to_string())?;
        }
        Ok(ct)
    }

    /// The global action graph of everything analysed so far plus `extra` (e.g. targets decorated
    /// by top-level aspects): registers every typed action, reporting conflicting generating
    /// actions with Bazel's `file '...' is generated by these conflicting actions` text.
    pub fn action_graph(
        &self,
        extra: &[Arc<ConfiguredTarget>],
    ) -> Result<zel_analysis::ActionGraph, zel_analysis::ActionConflict> {
        let mut g = zel_analysis::ActionGraph::new();
        for ct in self.order.lock().unwrap().iter().chain(extra.iter()) {
            for a in &ct.typed_actions {
                g.register(a)?;
            }
        }
        // Actions registered by aspects that were applied along dependency edges (their targets
        // carry the base target's actions again; identical registrations are idempotent).
        for ct in self.aspects.lock().unwrap().values() {
            for a in &ct.typed_actions {
                g.register(a)?;
            }
        }
        Ok(g)
    }

    /// Direct dependencies (label and configuration) of the configured target `label` in `cfg`,
    /// in attribute order, as recorded during its analysis.
    /// `Package.Metadata` of the package of `label` (repository name + the repository mapping).
    fn pkg_meta(&self, label: &Label) -> zel_analysis::PkgMeta {
        let repo = label.repository().name().to_string();
        zel_analysis::PkgMeta {
            package: format!("{}//{}", repo, label.package_name()),
            mapping: <Self as DepResolver>::repo_mapping_entries(self, &repo),
            repo,
        }
    }

    /// `ConfiguredObjectValue.getTransitivePackages` (`TransitiveDependencyState.PackageCollector`):
    /// the target's own package, then the transitive packages of its direct dependencies ordered by
    /// `ConfiguredTargetKey` (label, then configuration checksum).
    pub fn transitive_packages(&self, label: &Label, cfg: &BuildConfigurationKey) -> zel_analysis::nested_set::NSet<zel_analysis::PkgMeta> {
        let mut visiting = Vec::new();
        self.transitive_packages_rec(label, cfg, &mut visiting)
    }

    fn transitive_packages_rec(
        &self,
        label: &Label,
        cfg: &BuildConfigurationKey,
        visiting: &mut Vec<(String, String)>,
    ) -> zel_analysis::nested_set::NSet<zel_analysis::PkgMeta> {
        use zel_analysis::nested_set::{NSet, STABLE_ORDER};
        let k = key(label, cfg);
        if let Some(s) = self.pkg_sets.lock().unwrap().get(&k) {
            return s.clone();
        }
        let own = self.pkg_meta(label);
        if visiting.contains(&k) {
            return NSet::build(STABLE_ORDER, vec![own], vec![]);
        }
        visiting.push(k.clone());
        let mut deps = self.direct_deps(label, cfg);
        // Platform, constraint and toolchain-type targets are looked up by the platform/toolchain
        // resolution functions, not configured as prerequisites: they add no packages.
        deps.retain(|(l, _)| !self.is_platform_like(l, 0));
        let ckey = |l: &Label, c: &BuildConfigurationKey| (l.repository().name().to_string(), l.package_name().to_string(), l.name().to_string(), c.checksum());
        deps.sort_by(|a, b| ckey(&a.0, &a.1).cmp(&ckey(&b.0, &b.1)));
        deps.dedup_by(|a, b| ckey(&a.0, &a.1) == ckey(&b.0, &b.1));
        let children: Vec<_> = deps.iter().map(|(l, c)| self.transitive_packages_rec(l, c, visiting)).collect();
        visiting.pop();
        let set = NSet::build(STABLE_ORDER, vec![own], children);
        self.pkg_sets.lock().unwrap().insert(k, set.clone());
        set
    }

    /// Platform / toolchain-type targets (looking through aliases): resolved by the platform and
    /// toolchain resolution functions, not configured as prerequisites.
    fn is_platform_like(&self, l: &Label, depth: usize) -> bool {
        let Ok(pkg) = self.package(l.package_identifier()) else { return false };
        match pkg.targets.get(l.name()) {
            Some(Target::Rule(r)) => match r.rule_class.as_str() {
                "platform" | "toolchain_type" | "toolchain" => true,
                "alias" if depth < 8 => match r.attr_or_default("actual") {
                    Some(AttrValue::Label(Some(a))) => self.is_platform_like(&a, depth + 1),
                    _ => false,
                },
                _ => false,
            },
            _ => false,
        }
    }

    pub fn set_defer_cycle_dedup(&self, v: bool) {
        self.defer_cycle_dedup.store(v, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn direct_deps(&self, label: &Label, cfg: &BuildConfigurationKey) -> Vec<(Label, BuildConfigurationKey)> {
        self.edges.lock().unwrap().get(&key(label, cfg)).cloned().unwrap_or_default()
    }

    /// Aspects applied to `label` recorded in the aspect cache.
    pub fn aspects_for(&self, label: &Label) -> Vec<Arc<ConfiguredTarget>> {
        self.aspects
            .lock()
            .unwrap()
            .values()
            .filter(|ct| ct.label == *label)
            .cloned()
            .collect()
    }

    /// The loader used for package lookups.
    pub fn loader(&self) -> &Arc<PackageLoader> {
        &self.loader
    }

    fn analyze_uncached(
        &self,
        label: &Label,
        cfg: &BuildConfigurationKey,
    ) -> Result<ConfiguredTarget, String> {
        let pkg = match self.package(label.package_identifier()) {
            Ok(p) => p,
            Err(e) => {
                // `NoSuchPackageException` for a dependency: reported plain and once more at the
                // referencing rule (`... and referenced by '//x:y'`).
                let st = self.stack.lock().unwrap().clone();
                if e.starts_with("no such package") && st.len() >= 2 {
                    let parent = st[st.len() - 2].0.clone();
                    let loc = Label::parse_canonical(&parent).ok().and_then(|l| {
                        let p = self.package(l.package_identifier()).ok()?;
                        match p.targets.get(l.name())? {
                            Target::Rule(r) => r.extra.location.clone(),
                            _ => None,
                        }
                    });
                    if let Some(loc) = loc {
                        return Err(format!("{e}\nERROR: {loc}: {e} and referenced by '{parent}'"));
                    }
                }
                return Err(e);
            }
        };
        let target = match pkg.targets.get(label.name()) {
            Some(t) => t.clone(),
            None => {
                // A source file that is not declared by a rule.
                let root = self
                    .loader
                    .config()
                    .lookup
                    .resolver
                    .repo_root(label.repository())
                    .ok_or_else(|| format!("no such package '{}': repository is not defined", label.package_identifier().package_fragment()))?;
                let path = root
                    .get_relative(label.package_identifier().package_fragment())
                    .get_relative_str(label.name());
                let is_prereq = self.stack.lock().unwrap().len() > 1;
                if self.loader.config().lookup.fs.exists(&path, true) || is_prereq {
                    Target::InputFile { label: label.clone() }
                } else {
                    return Err(format!(
                        "no such target '{}': target '{}' not declared in package '{}' defined by {}",
                        label.canonical_form(),
                        label.name(),
                        label.package_name(),
                        pkg.build_file
                    ));
                }
            }
        };
        let mut ct = analyze_target_with(&target, cfg, self.me() as Arc<dyn DepResolver>, &self.config)?;
        if let Target::Rule(r) = &target {
            if r.rule_class == "config_setting" && label.repository().is_main() {
                // `ConfigSetting.checkOptionValue`: `DEPRECATED_PRE_PLATFORMS_FLAGS` (main repo only).
                if let Some(AttrValue::StrDict(values)) = r.attr_or_default("values") {
                    for (k, _) in values {
                        let opt = k.trim_start_matches("--");
                        if matches!(opt, "cpu" | "host_cpu" | "crosstool_top") {
                            let loc = r.extra.location.clone().unwrap_or_default();
                            self.warnings.lock().unwrap().push(format!(
                                "WARNING: {loc}: in config_setting rule {}: select() on {opt} is deprecated. Use platform constraints instead: https://bazel.build/docs/configurable-attributes#platforms.",
                                label.canonical_form().trim_start_matches("@@")
                            ));
                        }
                    }
                }
            }
        }
        if let Target::Rule(r) = &target {
            if r.rule_class == "config_setting" {
                // `config_setting` configures its `constraint_values` (`NoConfigTransition`
                // prerequisites): they are dependency edges of the configured target.
                if let Some(AttrValue::LabelList(cvs)) = r.attr_or_default("constraint_values") {
                    if let Some(owner) = self.stack.lock().unwrap().last().cloned() {
                        let mut e = self.edges.lock().unwrap();
                        let v = e.entry(owner).or_default();
                        for l in cvs {
                            let dcfg = cfg.to_core_only();
                            if !v.iter().any(|(x, c)| *x == l && *c == dcfg) {
                                v.push((l, dcfg));
                            }
                        }
                    }
                }
            }
        }
        ct.visibility = match &target {
            Target::Rule(r) => r.extra.effective_visibility.clone(),
            Target::InputFile { label } => {
                let src: Vec<String> = pkg
                    .exported_files
                    .get(label.name())
                    .map(|v| v.iter().map(|l| l.canonical_form()).collect())
                    .unwrap_or_default();
                if pkg.exported_files.contains_key(label.name()) {
                    pkg.exported_files[label.name()].clone()
                } else {
                    let _ = src;
                    pkg.default_visibility
                        .iter()
                        .filter_map(|s| Label::parse_canonical(s).ok())
                        .collect()
                }
            }
            _ => Vec::new(),
        };
        Ok(ct)
    }

    /// Expands a `test_suite` label into its member test targets (`TestSuiteExpansionFunction`):
    /// explicit `tests` (nested suites expand recursively), or, if empty, every non-`manual`
    /// test rule of the package. Suite `tags` and `--test_tag_filters` select members; `manual`
    /// tests are only included when listed explicitly or when the filters name `manual`.
    /// Non-suite labels are returned unchanged.
    pub fn expand_test_suites(
        &self,
        label: &Label,
        tag_filters: &[String],
    ) -> Result<Vec<Label>, String> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        self.expand_suite_into(label, tag_filters, &mut out, &mut seen, true)?;
        Ok(out)
    }

    fn expand_suite_into(
        &self,
        label: &Label,
        filters: &[String],
        out: &mut Vec<Label>,
        seen: &mut std::collections::HashSet<String>,
        top: bool,
    ) -> Result<(), String> {
        let pkg = self.package(label.package_identifier())?;
        let rule = match pkg.targets.get(label.name()) {
            Some(Target::Rule(r)) if r.rule_class == "test_suite" => r.clone(),
            _ => {
                if seen.insert(label.canonical_form()) {
                    out.push(label.clone());
                }
                return Ok(());
            }
        };
        let strs = |r: &RuleInstance, n: &str| match r.extra.attr_values.get(n) {
            Some(zel_packages::AttrValue::StrList(l)) => l.clone(),
            _ => Vec::new(),
        };
        let suite_tags = strs(&rule, "tags");
        let tag_ok = |tags: &[String], f: &[String]| {
            let pos: Vec<&str> = f.iter().filter(|x| !x.starts_with('-')).map(|x| x.as_str()).collect();
            let neg: Vec<&str> = f.iter().filter_map(|x| x.strip_prefix('-')).collect();
            (pos.is_empty() || tags.iter().any(|t| pos.contains(&t.as_str())))
                && !tags.iter().any(|t| neg.contains(&t.as_str()))
        };
        let explicit = match rule.extra.attr_values.get("tests") {
            Some(zel_packages::AttrValue::LabelList(l)) if !l.is_empty() => Some(l.clone()),
            _ => None,
        };
        // The suite's own tags are filters (excluding the pseudo-tag `manual`, which only
        // keeps the suite itself out of wildcards).
        let suite_filters: Vec<String> = suite_tags.into_iter().filter(|t| t != "manual").collect();
        let manual_requested = filters.iter().any(|f| f == "manual");
        let members: Vec<(Label, bool)> = match explicit {
            Some(l) => l.into_iter().map(|l| (l, true)).collect(),
            None => pkg
                .targets
                .values()
                .filter_map(|t| match t {
                    Target::Rule(r) if r.is_test_rule() && r.rule_class != "test_suite" => {
                        Some((r.label.clone(), false))
                    }
                    _ => None,
                })
                .collect(),
        };
        for (m, is_explicit) in members {
            let mpkg = self.package(m.package_identifier())?;
            if let Some(Target::Rule(r)) = mpkg.targets.get(m.name()) {
                if r.rule_class == "test_suite" {
                    self.expand_suite_into(&m, filters, out, seen, false)?;
                    continue;
                }
                let tags = strs(r, "tags");
                if !is_explicit && tags.iter().any(|t| t == "manual") && !manual_requested {
                    continue;
                }
                if !tag_ok(&tags, &suite_filters) || !tag_ok(&tags, filters) {
                    continue;
                }
                if seen.insert(m.canonical_form()) {
                    out.push(m);
                }
            } else {
                return Err(format!(
                    "in test_suite rule {}: '{}' is not a test",
                    label.canonical_form(),
                    m.canonical_form()
                ));
            }
        }
        let _ = top;
        Ok(())
    }

    fn is_visible(&self, spec: &Label, from: &PackageIdentifier) -> Result<bool, String> {
        let name = spec.name();
        let spkg = spec.package_identifier();
        Ok(match name {
            "public" if spec.package_name() == "visibility" => true,
            "private" if spec.package_name() == "visibility" => false,
            "__pkg__" => spkg == from,
            "__subpackages__" => {
                spkg.repository() == from.repository()
                    && (spkg.package_fragment().path_string().is_empty()
                        || from.package_fragment().path_string() == spkg.package_fragment().path_string()
                        || from
                            .package_fragment()
                            .path_string()
                            .starts_with(&format!("{}/", spkg.package_fragment().path_string())))
            }
            _ => {
                // A package_group.
                let pkg = self.package(spkg)?;
                match pkg.targets.get(name) {
                    Some(Target::PackageGroup { packages, .. }) => {
                        let mut ok = false;
                        for p in packages {
                            let (neg, p) = match p.strip_prefix('-') {
                                Some(r) => (true, r),
                                None => (false, p.as_str()),
                            };
                            let m = package_spec_matches(p, from, spkg.repository());
                            if m {
                                ok = !neg;
                            }
                        }
                        if !ok {
                            if let Some(incs) = pkg.group_includes.get(name) {
                                for i in incs {
                                    if self.is_visible(i, from)? {
                                        ok = true;
                                        break;
                                    }
                                }
                            }
                        }
                        ok
                    }
                    _ => false,
                }
            }
        })
    }
}

/// Whether `from` matches a package_group `packages` entry; unqualified specs name packages of
/// the package_group's own repository (`group_repo`).
fn package_spec_matches(spec: &str, from: &PackageIdentifier, group_repo: &zel_cmdline::RepositoryName) -> bool {
    let (repo_part, rest) = match spec.strip_prefix('@') {
        Some(r) => match r.split_once("//") {
            Some((repo, rest)) => (Some(repo.trim_start_matches('@')), rest),
            None => (None, spec),
        },
        None => (None, spec.strip_prefix("//").unwrap_or(spec)),
    };
    if let Some(r) = repo_part {
        if from.repository().name() != r {
            return false;
        }
    } else if from.repository() != group_repo {
        return false;
    }
    let pkg = from.package_fragment().path_string();
    if rest == "..." {
        return true;
    }
    if let Some(prefix) = rest.strip_suffix("/...") {
        return pkg == prefix || pkg.starts_with(&format!("{prefix}/"));
    }
    pkg == rest
}

impl DepResolver for TargetResolver {
    fn package_loader(&self) -> Option<Arc<PackageLoader>> {
        Some(self.loader.clone())
    }

    fn repo_mapping_entries(&self, repo: &str) -> Vec<(String, String)> {
        let owner = if repo.is_empty() {
            zel_cmdline::RepositoryName::main()
        } else {
            zel_cmdline::RepositoryName::create_unvalidated(repo)
        };
        let m = self.loader.config().lookup.resolver.mapping(&owner);
        m.entries().map(|(a, c)| (a.clone(), c.name().to_string())).collect()
    }

    fn rule_instance(&self, label: &Label) -> Result<Option<RuleInstance>, String> {
        let pkg = self.package(label.package_identifier())?;
        Ok(match pkg.targets.get(label.name()) {
            Some(Target::Rule(r)) => Some(r.clone()),
            // Aspects applied to a generated file apply to its generating rule's target only
            // with `apply_to_generating_rules`; plain files have no rule.
            _ => None,
        })
    }

    fn target_platform_has_constraint(&self, cfg: &BuildConfigurationKey, value: &Label) -> Result<bool, String> {
        let me: Arc<dyn DepResolver> = self.me();
        let platform = crate::toolchain::target_platform(cfg, &me, &self.config)?.expect("platform");
        let cv = self.analyze(value, cfg)?;
        Ok(crate::toolchain::platform_has_value(&platform, &cv))
    }

    fn aspect_cache_get(&self, key: &str) -> Option<Arc<ConfiguredTarget>> {
        self.aspects.lock().unwrap().get(key).cloned()
    }

    fn aspect_cache_put(&self, key: String, value: Arc<ConfiguredTarget>) {
        self.aspects.lock().unwrap().insert(key, value);
    }

    fn aspect_toolchains(
        &self,
        owner: &Label,
        toolchains: &[zel_starlark::Value],
        exec_compatible_with: &[String],
        cfg: &BuildConfigurationKey,
    ) -> Result<(BTreeMap<String, zel_starlark::Value>, BTreeMap<String, String>), String> {
        let list = zel_starlark::Value::List(Arc::new(Mutex::new(zel_starlark::StarlarkList::new(toolchains.to_vec()))));
        let types = crate::toolchain_requests(Some(&list));
        let ecw: Vec<Label> = exec_compatible_with.iter().filter_map(|s| Label::parse_canonical(s).ok()).collect();
        let me: Arc<dyn DepResolver> = self.me();
        let tc = crate::resolve_ctx_toolchains(owner, &types, &ecw, cfg, &self.config, &me)?;
        Ok((tc.toolchains, tc.make_variables))
    }

    fn configured_target(
        &self,
        label: &Label,
        cfg: &BuildConfigurationKey,
    ) -> Result<Arc<ConfiguredTarget>, String> {
        self.analyze(label, cfg)
    }

    fn check_dependency(
        &self,
        consumer: &RuleInstance,
        attr: &str,
        dep: &ConfiguredTarget,
    ) -> Result<(), String> {
        if let Some(owner) = self.stack.lock().unwrap().last().cloned() {
            let mut e = self.edges.lock().unwrap();
            let v = e.entry(owner).or_default();
            if !v.iter().any(|(l, c)| *l == dep.label && *c == dep.configuration) {
                v.push((dep.label.clone(), dep.configuration.clone()));
            }
        }
        let prefix = || {
            let loc = consumer.extra.location.clone().map(|l| format!("{l}: ")).unwrap_or_default();
            format!("{loc}in {attr} attribute of {} rule {}: ", consumer.rule_class, consumer.label.canonical_form())
        };
        // `RuleContext.validateDirectPrerequisiteType`: a label that is not a rule, names no
        // existing file and is not allowed as a file -> a misspelled rule.
        if dep.rule_class == "source file" {
            let def = consumer.extra.rule_class.as_ref().and_then(|rc| rc.attr_or_implicit(attr));
            let none_allowed = def
                .map(|a| {
                    matches!(a.ty, zel_packages::attr::AttrType::Label | zel_packages::attr::AttrType::LabelList)
                        && (a.files_unspecified || matches!(a.allowed_files, zel_packages::attr::FileTypes::NoFiles))
                })
                .unwrap_or(false);
            if none_allowed {
                let missing = self
                    .loader
                    .config()
                    .lookup
                    .resolver
                    .repo_root(dep.label.repository())
                    .map(|root| {
                        let path = root
                            .get_relative(dep.label.package_identifier().package_fragment())
                            .get_relative_str(dep.label.name());
                        !self.loader.config().lookup.fs.exists(&path, true)
                    })
                    .unwrap_or(false);
                if missing {
                    let kind = if dep.label.name().contains('.') { "target" } else { "rule" };
                    return Err(format!("{}{kind} '{}' does not exist", prefix(), dep.label.canonical_form()));
                }
            }
        }
        // Visibility (`target_compatible_with` constraint values are not visibility checked).
        if self.config.check_visibility && attr != "target_compatible_with" {
            // Visibility of a private (underscore) attribute's default is checked from the
            // package of the rule's definition (--incompatible_visibility_private_attributes_at_definition).
            let def_label = if attr.starts_with('_') {
                consumer
                    .extra
                    .rule_class
                    .as_ref()
                    .and_then(|rc| rc.starlark.as_ref())
                    .and_then(|sp| Label::parse_canonical(&sp.defined_in).ok())
            } else {
                None
            };
            let from = match &def_label {
                Some(l) => l.package_identifier(),
                None => consumer.label.package_identifier(),
            };
            let dep_pkg = dep.label.package_identifier();
            // `package_group` targets are always visible (their visibility is implicitly public).
            if dep_pkg != from && dep.rule_class != "package_group" {
                let mut visible = false;
                for spec in &dep.visibility {
                    if self.is_visible(spec, from)? {
                        visible = true;
                        break;
                    }
                }
                if !visible {
                    let mut msg = format!(
                        "Visibility error:\ntarget '{}' is not visible from\ntarget '{}'\nRecommendation: modify the visibility declaration if you think the dependency is legitimate. For more info see https://bazel.build/concepts/visibility",
                        dep.label.canonical_form(),
                        consumer.label.canonical_form()
                    );
                    if dep.rule_class == "source file" {
                        msg.push_str(". To depend on that source file target, either add it to a filegroup() and depend on that filegroup, or else use the exports_files() function to change the source file's visibility");
                    }
                    // `RuleContext.reportError` without the attribute: `in <kind> rule <label>: `.
                    let loc = consumer.extra.location.clone().map(|l| format!("{l}: ")).unwrap_or_default();
                    return Err(format!(
                        "{loc}in {} rule {}: {msg}",
                        consumer.rule_class,
                        consumer.label.canonical_form()
                    ));
                }
            }
        }
        // testonly.
        if dep.testonly && !consumer.testonly {
            return Err(format!(
                "{}non-test target '{}' depends on testonly target '{}' and doesn't have testonly attribute set",
                prefix(),
                consumer.label.canonical_form(),
                dep.label.canonical_form()
            ));
        }
        Ok(())
    }
}

/// Source-file configured target.
pub(crate) fn source_file_target(label: &Label, cfg: &BuildConfigurationKey) -> ConfiguredTarget {
    let rel = label.package_identifier().package_fragment().get_relative_str(label.name());
    let art = Artifact::source(rel, label.clone());
    let mut ct = ConfiguredTarget::new(label.clone(), cfg.clone(), "source file".to_string());
    ct.default_info = DefaultInfo { files: vec![art.clone()], ..Default::default() };
    ct.providers.insert(
        "DefaultInfo".into(),
        zel_analysis::starlark_rule::// FilesToRunProvider.fromSingleExecutableArtifact: a source file is its own executable.
        default_info_provider(&[art.clone()], Default::default(), Default::default(), Some(art.clone())),
    );
    let mut groups = BTreeMap::new();
    groups.insert("default".to_string(), vec![art]);
    ct.output_groups = zel_analysis::OutputGroupInfo { groups };
    ct
}
