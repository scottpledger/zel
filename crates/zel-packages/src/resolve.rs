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

//! Target pattern resolution (`TargetPatternPhaseFunction`, `TargetPatternFunction`,
//! `RecursivePackageProvider`) and `Package.getTarget` error reporting.

use crate::loader::{PackageLoadError, PackageLoader};
use crate::lookup::{EventKind, PackageEvent, PackageLookup};
use crate::{Package, Target};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use zel_cmdline::{
    Label, PackageIdentifier, PatternSign, RepositoryMapping, RepositoryName, SignedTargetPattern,
    TargetPattern,
};
use zel_vfs::{DirentType, PathFragment};

/// Edit-distance based target-name suggestions (`TargetSuggester`).
fn suggest_targets(input: &str, words: &[String]) -> String {
    let lower = input.to_lowercase();
    let max = std::cmp::min(5, (input.chars().count() + 1) / 2);
    if max == 0 {
        return String::new();
    }
    let mut by_dist: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for w in words {
        if let Some(d) = edit_distance(&lower, &w.to_lowercase(), max - 1) {
            by_dist.entry(d).or_default().push(w.clone());
        }
    }
    const MAX: usize = 10;
    let picked: Vec<String> = if by_dist.is_empty() {
        Vec::new()
    } else if let Some(z) = by_dist.get(&0) {
        z.iter().take(MAX).cloned().collect()
    } else if let Some(o) = by_dist.get(&1) {
        o.iter().take(MAX).cloned().collect()
    } else {
        let mut out = Vec::new();
        let mut d = 2;
        while d < max && out.len() < MAX {
            if let Some(v) = by_dist.get(&d) {
                out.extend(v.iter().take(MAX - out.len()).cloned());
            }
            d += 1;
        }
        out
    };
    pretty_print_targets(&picked)
}

fn pretty_print_targets(t: &[String]) -> String {
    match t.len() {
        0 => String::new(),
        1 => format!(" (did you mean {}?)", t[0]),
        n => format!(
            " (did you mean {}, or {}?)",
            t[..n - 1].join(", "),
            t[n - 1]
        ),
    }
}

pub(crate) fn edit_distance(a: &str, b: &str, max: usize) -> Option<usize> {
    if a == b {
        return Some(0);
    }
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > max {
        return None;
    }
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        row[0] = i;
        let mut best = row[0];
        let mut prev = i - 1;
        for j in 1..=b.len() {
            let old = row[j];
            row[j] = std::cmp::min(
                prev + usize::from(a[i - 1] != b[j - 1]),
                1 + std::cmp::min(row[j - 1], row[j]),
            );
            prev = old;
            best = best.min(row[j]);
        }
        if best > max {
            return None;
        }
    }
    (row[b.len()] <= max).then_some(row[b.len()])
}

impl Package {
    /// `Package.getTarget`: the target `name`, or Bazel's `no such target` message.
    pub fn get_target(&self, name: &str, fs: &dyn zel_vfs::FileSystem) -> Result<&Target, String> {
        if let Some(t) = self.targets.get(name) {
            return Ok(t);
        }
        let label = Label::create(self.id.clone(), name)
            .map(|l| l.canonical_form())
            .unwrap_or_else(|_| format!("//{}:{name}", self.id.package_fragment()));
        let suggestion = self.alternate_target_suggestion(name, fs);
        Err(format!(
            "no such target '{label}': target '{name}' not declared in package '{}' defined by {}{suggestion}",
            self.id.package_fragment(),
            self.build_file
        ))
    }

    fn repo_relative_build_file(&self) -> String {
        let base = self.build_file.base_name();
        if self.id.package_fragment().is_empty() {
            base.to_string()
        } else {
            format!("{}/{base}", self.id.package_fragment())
        }
    }

    fn alternate_target_suggestion(&self, name: &str, fs: &dyn zel_vfs::FileSystem) -> String {
        let pkg_dir = self
            .build_file
            .parent_directory()
            .unwrap_or_else(PathFragment::empty);
        let normalized = PathFragment::create(name).path_string() == name;
        if !normalized || name == "*" {
            return String::new();
        }
        let path = pkg_dir.get_relative_str(name);
        if let Ok(st) = fs.stat(&path, true) {
            if st.is_directory {
                return format!(
                    "; however, a source directory of this name exists.  (Perhaps add 'exports_files([\"{name}\"])' to {}, or define a filegroup?)",
                    self.repo_relative_build_file()
                );
            }
            return format!(
                "; however, a source file of this name exists.  (Perhaps add 'exports_files([\"{name}\"])' to {}?)",
                self.repo_relative_build_file()
            );
        }
        let mut matching_source_files: Vec<String> = Vec::new();
        for (tn, t) in &self.targets {
            if matches!(t, Target::InputFile { .. }) {
                let base = tn.rsplit('/').next().unwrap_or(tn);
                let stem = base.split('.').next().unwrap_or(base);
                if tn == name || base == name || (name.contains('.') && stem == name) {
                    matching_source_files.push(tn.clone());
                }
            }
        }
        if !matching_source_files.is_empty() {
            let mut consuming: Vec<String> = Vec::new();
            for (rn, t) in &self.targets {
                if let Target::Rule(r) = t {
                    for s in &r.srcs {
                        // `srcs` hold canonical labels (`//pkg:name`) or package-relative names.
                        let own_prefix = format!("//{}:", self.id.package_fragment());
                        let short = s
                            .strip_prefix(&own_prefix)
                            .or_else(|| s.strip_prefix(':'))
                            .unwrap_or(s);
                        if matching_source_files.iter().any(|f| f == short)
                            && !consuming.contains(rn)
                        {
                            consuming.push(rn.clone());
                        }
                    }
                }
            }
            if !consuming.is_empty() {
                consuming.truncate(10);
                return pretty_print_targets(&consuming);
            }
        }
        let mut all: Vec<String> = self
            .targets
            .iter()
            .filter_map(|(k, t)| {
                if matches!(t, Target::InputFile { .. }) {
                    Some(k.clone())
                } else {
                    None
                }
            })
            .collect();
        for (k, t) in &self.targets {
            if !matches!(t, Target::InputFile { .. }) {
                all.push(k.clone());
            }
        }
        suggest_targets(name, &all)
    }
}

/// An error resolving one pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternError {
    pub pattern: String,
    pub message: String,
    /// Set when the pattern failed only because its package loaded with errors (the package may
    /// still be partially usable; Bazel reports `package contains errors: <pkg>: <first error>`).
    pub package_errors: Option<PackageErrors>,
}

/// A package that loaded with errors (`Package.containsErrors()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageErrors {
    pub package: PackageIdentifier,
    /// `include_files` of the failing wildcard (`:*` / `:all-targets`), `false` otherwise.
    pub include_files: bool,
    /// Message of the first error event (`Package.getFailureDetail().getMessage()`).
    pub first_error: String,
}

/// Result of resolving a list of (signed) target patterns.
#[derive(Debug, Default)]
pub struct ResolveOutcome {
    /// Resolved targets in first-seen order, duplicates removed, negative patterns subtracted.
    pub targets: Vec<Target>,
    /// Targets excluded by `FilteringPolicy` (`ResolvedTargets.getFilteredTargets()`).
    pub filtered_targets: Vec<Target>,
    /// Patterns that failed (all of them are reported even without `--keep_going`; the caller
    /// decides whether to stop).
    pub errors: Vec<PatternError>,
    /// Events (errors/warnings/prints) of every package that was loaded.
    pub events: Vec<PackageEvent>,
}

/// Filtering policy matching `com.google.devtools.build.lib.pkgcache.FilteringPolicy` /
/// `FilteringPolicies`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilteringPolicy {
    NoFilter,
    FilterManual,
    FilterTests,
    RulesOnly,
    RuleTypeExplicit(String),
    And(Box<FilteringPolicy>, Box<FilteringPolicy>),
}

impl FilteringPolicy {
    pub fn should_retain(&self, target: &Target, explicit: bool, pkg: &Package) -> bool {
        match self {
            Self::NoFilter => true,
            Self::FilterManual => explicit || !target.has_tag("manual"),
            Self::FilterTests => {
                let is_test = matches!(target, Target::Rule(r) if r.is_test_rule());
                is_test && (explicit || !target.has_tag("manual"))
            }
            Self::RulesOnly => matches!(target, Target::Rule(_)),
            Self::RuleTypeExplicit(rule_name) => {
                if explicit {
                    return true;
                }
                match target {
                    Target::Rule(r) => r.rule_class == *rule_name,
                    Target::OutputFile {
                        generating_rule, ..
                    } => match pkg.targets.get(generating_rule.name()) {
                        Some(Target::Rule(r)) => r.rule_class == *rule_name,
                        _ => false,
                    },
                    _ => false,
                }
            }
            Self::And(a, b) => {
                a.should_retain(target, explicit, pkg) && b.should_retain(target, explicit, pkg)
            }
        }
    }
}

/// `TargetPatternResolverUtil.getParsingErrorMessage`.
pub fn get_parsing_error_message(message: &str, original_pattern: Option<&str>) -> String {
    match original_pattern {
        None => message.to_string(),
        Some(p) => format!("while parsing '{p}': {message}"),
    }
}

/// `TargetPatternResolverUtil.getPathFragment`.
pub fn get_path_fragment(path_prefix: &str) -> Result<PathFragment, String> {
    let directory = PathFragment::create(path_prefix);
    if directory.contains_uplevel_references() {
        return Err(format!(
            "up-level references are not permitted: '{}'",
            directory.path_string()
        ));
    }
    if !path_prefix.is_empty()
        && zel_cmdline::validator::validate_package_name(path_prefix).is_err()
    {
        return Err(format!("'{path_prefix}' is not a valid package name"));
    }
    Ok(directory)
}

/// `TargetPatternResolverUtil.resolvePackageTargets`.
pub fn resolve_package_targets(pkg: &Package, policy: &FilteringPolicy) -> Vec<Target> {
    if matches!(policy, FilteringPolicy::NoFilter) {
        return pkg.targets.values().cloned().collect();
    }
    pkg.targets
        .values()
        .filter(|t| policy.should_retain(t, false, pkg))
        .cloned()
        .collect()
}

/// Resolves target patterns against a [`PackageLoader`], caching loaded packages for the lifetime
/// of the resolver (one command invocation).
pub struct PatternResolver {
    loader: Arc<PackageLoader>,
    cache: Mutex<HashMap<PackageIdentifier, Result<Arc<Package>, PackageLoadError>>>,
    reported: Mutex<HashSet<PackageIdentifier>>,
    /// Client working directory relative to the workspace root.
    pub relative_dir: PathFragment,
    pub main_mapping: RepositoryMapping,
}

impl PatternResolver {
    pub fn new(loader: Arc<PackageLoader>, relative_dir: PathFragment) -> Self {
        let main_mapping = loader
            .config()
            .lookup
            .resolver
            .mapping(&RepositoryName::main());
        Self {
            loader,
            cache: Mutex::new(HashMap::new()),
            reported: Mutex::new(HashSet::new()),
            relative_dir,
            main_mapping,
        }
    }

    pub fn loader(&self) -> &Arc<PackageLoader> {
        &self.loader
    }

    /// Loads (once) the package `id`.
    pub fn package(&self, id: &PackageIdentifier) -> Result<Arc<Package>, PackageLoadError> {
        if let Some(r) = self.cache.lock().unwrap().get(id) {
            return r.clone();
        }
        let r = self.loader.load_package(id).map(Arc::new);
        self.cache.lock().unwrap().insert(id.clone(), r.clone());
        r
    }

    /// Takes the not-yet-reported events of `pkg` (each package's events are reported once).
    fn take_events(&self, pkg: &Package, out: &mut Vec<PackageEvent>) {
        if self.reported.lock().unwrap().insert(pkg.id.clone()) {
            out.extend(pkg.events.iter().cloned());
        }
    }

    /// All packages loaded so far.
    pub fn loaded_packages(&self) -> BTreeMap<PackageIdentifier, Arc<Package>> {
        self.cache
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(k, v)| v.as_ref().ok().map(|p| (k.clone(), Arc::clone(p))))
            .collect()
    }

    /// Resolves a label to its target (`Package.getTarget`), loading its package.
    pub fn get_target(&self, label: &Label) -> Result<Target, String> {
        let pkg = self
            .package(label.package_identifier())
            .map_err(|e| e.message)?;
        pkg.get_target(label.name(), self.loader.config().lookup.fs.as_ref())
            .cloned()
    }

    pub fn rules_or_all(pkg: &Package, include_files: bool) -> Vec<Target> {
        pkg.targets
            .values()
            .filter(|t| include_files || matches!(t, Target::Rule(_)))
            .cloned()
            .collect()
    }

    /// Expands one parsed pattern to targets.
    pub fn expand(
        &self,
        pattern: &TargetPattern,
        events: &mut Vec<PackageEvent>,
    ) -> Result<Vec<Target>, String> {
        match pattern {
            TargetPattern::SingleTarget(label) => {
                let pkg = self.package(label.package_identifier()).map_err(|e| {
                    events.extend(e.events.iter().cloned());
                    e.message
                })?;
                self.take_events(&pkg, events);
                // A target missing from a package that contains errors is a plain
                // `NoSuchTargetException` (`no such target ...: not declared in package ...`).
                let t = pkg
                    .get_target(label.name(), self.loader.config().lookup.fs.as_ref())?
                    .clone();
                Ok(vec![t])
            }
            TargetPattern::PathAsTarget { original, path } => {
                let label = self.interpret_path_as_target(original, path)?;
                self.expand(&TargetPattern::SingleTarget(label), events)
            }
            TargetPattern::TargetsInPackage {
                package,
                include_files,
            } => {
                let pkg = self.package(package).map_err(|e| {
                    events.extend(e.events.iter().cloned());
                    e.message
                })?;
                self.take_events(&pkg, events);
                if pkg.contains_errors {
                    return Err(format!(
                        "error loading package '{}': Package '{}' contains errors",
                        package.package_fragment(),
                        package.package_fragment()
                    ));
                }
                Ok(Self::rules_or_all(&pkg, *include_files))
            }
            TargetPattern::TargetsBelowDirectory {
                directory,
                include_files,
            } => self
                .expand_below_directory(directory, *include_files, &[], false, events)
                .map(|(ts, _)| ts),
        }
    }

    fn expand_below_directory(
        &self,
        directory: &PackageIdentifier,
        include_files: bool,
        excluded_subdirs: &[PathFragment],
        keep_going: bool,
        events: &mut Vec<PackageEvent>,
    ) -> Result<(Vec<Target>, Vec<String>), String> {
        let mut out = Vec::new();
        let mut pkgs = Vec::new();
        self.find_packages_below_excluding(directory, excluded_subdirs, &mut pkgs, events)?;
        let mut pkg_failures: Vec<String> = Vec::new();
        let mut pkg_err_events = Vec::new();
        for id in pkgs {
            match self.package(&id) {
                Ok(pkg) => {
                    self.take_events(&pkg, events);
                    if pkg.contains_errors {
                        pkg_err_events.push(PackageEvent::error(
                            None,
                            format!("package contains errors: {}", id.package_fragment()),
                        ));
                        pkg_failures.push(format!(
                            "error loading package '{}': Package '{}' contains errors",
                            id.package_fragment(),
                            id.package_fragment()
                        ));
                        continue;
                    }
                    out.extend(Self::rules_or_all(&pkg, include_files));
                }
                Err(e) => {
                    pkg_failures.push(e.message);
                }
            }
        }
        pkg_err_events.reverse();
        events.extend(pkg_err_events);
        if !keep_going {
            if let Some(first) = pkg_failures.into_iter().next() {
                return Err(first);
            }
            return Ok((out, Vec::new()));
        }
        Ok((out, pkg_failures))
    }

    /// `InterpretPathAsTarget`: finds the package containing `path` and builds the label.
    fn interpret_path_as_target(&self, original: &str, path: &str) -> Result<Label, String> {
        let segs: Vec<String> = if cfg!(windows) {
            let frag = PathFragment::create(path);
            frag.segments().iter().map(|s| s.to_string()).collect()
        } else {
            path.split('/')
                .filter(|s| !s.is_empty() && *s != ".")
                .map(|s| s.to_string())
                .collect()
        };
        let lookup = &self.loader.config().lookup;
        let repo = RepositoryName::main();
        for i in (0..=segs.len()).rev() {
            let pkg_frag = PathFragment::create(&segs[..i].join("/"));
            let Ok(id) = PackageIdentifier::create(repo.clone(), pkg_frag) else {
                continue;
            };
            if let PackageLookup::Found { .. } = lookup.lookup(&id) {
                let name = if i == segs.len() {
                    segs.last().cloned().unwrap_or_default()
                } else {
                    segs[i..].join("/")
                };
                if name.is_empty() {
                    break;
                }
                return Label::create(id, &name).map_err(|e| e.to_string());
            }
        }
        Err(format!(
            "couldn't determine target from filename '{original}'"
        ))
    }

    /// Lists packages (directories with BUILD files) at or below `dir`, honoring `.bazelignore`,
    /// `REPO.bazel` ignores and `--deleted_packages`.
    pub fn find_packages_below(
        &self,
        dir: &PackageIdentifier,
        out: &mut Vec<PackageIdentifier>,
    ) -> Result<(), String> {
        let mut ignored_events = Vec::new();
        self.find_packages_below_excluding(dir, &[], out, &mut ignored_events)
    }

    pub fn find_packages_below_excluding(
        &self,
        dir: &PackageIdentifier,
        excluded_subdirs: &[PathFragment],
        out: &mut Vec<PackageIdentifier>,
        events: &mut Vec<PackageEvent>,
    ) -> Result<(), String> {
        let lookup = &self.loader.config().lookup;
        let Some(root) = lookup.resolver.repo_root(dir.repository()) else {
            let main_map = lookup.resolver.mapping(&RepositoryName::main());
            let dym = main_map.did_you_mean(dir.repository().name());
            let repo_display = if dir.repository().is_main() {
                dir.repository().canonical_form()
            } else if !dir.repository().is_visible() {
                dir.repository().name_with_at()
            } else {
                format!(
                    "@@[unknown repo '{}' requested from @@{dym}]",
                    dir.repository().name()
                )
            };
            let not_vis = dir
                .repository()
                .not_visible_error_message()
                .unwrap_or_else(|| {
                    format!(
                        "No repository visible as '@{}' from main repository{dym}",
                        dir.repository().name()
                    )
                });
            return Err(format!(
                "no such package '{repo_display}//{}': The repository '{repo_display}' could not be resolved: {not_vis}",
                dir.package_fragment()
            ));
        };
        if let Err(e) = lookup.check_bazelignore(&root) {
            return Err(format!("Error reading .bazelignore: {e}"));
        }
        if let Some(entry) = lookup
            .ignored_subdirectories(&root)
            .matching_entry(dir.package_fragment())
        {
            let pat_display = if dir.repository().is_main() {
                if dir.package_fragment().is_empty() {
                    "//...".to_string()
                } else {
                    format!("//{}/...", dir.package_fragment())
                }
            } else if dir.package_fragment().is_empty() {
                format!("{}/...", dir.repository().canonical_form())
            } else {
                format!(
                    "{}//{}/...",
                    dir.repository().canonical_form(),
                    dir.package_fragment()
                )
            };
            events.push(PackageEvent {
                kind: EventKind::Warning,
                location: None,
                message: format!(
                    "Pattern '{pat_display}' was filtered out by ignored directory '{entry}'"
                ),
            });
            return Ok(());
        }
        let start = root.get_relative(dir.package_fragment());
        if !lookup
            .fs
            .stat(&start, true)
            .map(|s| s.is_directory)
            .unwrap_or(false)
        {
            return Err(format!(
                "no targets found beneath '{}'",
                dir.package_fragment()
            ));
        }
        let is_root = dir.package_fragment().is_empty();
        let mut ancestors = Vec::new();
        self.walk_dirs(
            dir,
            &root,
            is_root && dir.repository().is_main(),
            excluded_subdirs,
            &mut ancestors,
            out,
            events,
        );
        if out.is_empty() {
            if !excluded_subdirs.is_empty() {
                let mut unexcluded = Vec::new();
                let mut dummy_events = Vec::new();
                let mut dummy_anc = Vec::new();
                self.walk_dirs(
                    dir,
                    &root,
                    is_root && dir.repository().is_main(),
                    &[],
                    &mut dummy_anc,
                    &mut unexcluded,
                    &mut dummy_events,
                );
                if !unexcluded.is_empty() {
                    return Ok(());
                }
            }
            return Err(format!(
                "no targets found beneath '{}'",
                dir.package_fragment()
            ));
        }
        Ok(())
    }

    fn resolve_real_dir(
        fs: &dyn zel_vfs::FileSystem,
        start: &PathFragment,
    ) -> Option<PathFragment> {
        let mut cur = start.clone();
        for _ in 0..32 {
            let st = fs.stat(&cur, false).ok()?;
            if !st.is_symlink {
                return Some(cur);
            }
            let target = fs.read_symbolic_link(&cur).ok()?;
            cur = if target.is_absolute() {
                target
            } else if let Some(parent) = cur.parent_directory() {
                parent.get_relative(&target)
            } else {
                target
            };
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_dirs(
        &self,
        id: &PackageIdentifier,
        root: &PathFragment,
        skip_convenience: bool,
        excluded_subdirs: &[PathFragment],
        ancestors: &mut Vec<PathFragment>,
        out: &mut Vec<PackageIdentifier>,
        events: &mut Vec<PackageEvent>,
    ) {
        if excluded_subdirs
            .iter()
            .any(|ex| id.package_fragment().starts_with(ex))
        {
            return;
        }
        let lookup = &self.loader.config().lookup;
        let abs = root.get_relative(id.package_fragment());
        let Some(real_dir) = Self::resolve_real_dir(lookup.fs.as_ref(), &abs) else {
            return;
        };
        match lookup.lookup(id) {
            PackageLookup::Found { .. } => out.push(id.clone()),
            PackageLookup::Deleted(reason)
                if reason.contains(".bazelignore") || reason.contains("REPO.bazel") =>
            {
                // Ignored directories are not traversed at all.
                return;
            }
            _ => {}
        }
        let Ok(entries) = lookup.fs.readdir(&abs) else {
            return;
        };
        ancestors.push(real_dir);
        let mut names: Vec<_> = entries.into_iter().collect();
        names.sort_by(|a, b| a.name.cmp(&b.name));
        for e in names {
            if skip_convenience
                && e.name.starts_with("bazel-")
                && e.dirent_type == DirentType::Symlink
            {
                continue;
            }
            let child_abs = abs.get_relative_str(&e.name);
            let child_rel = id.package_fragment().get_relative_str(&e.name);
            let cst = match lookup.fs.stat(&child_abs, true) {
                Ok(s) => s,
                Err(_) => {
                    if e.dirent_type == DirentType::Symlink
                        && Self::resolve_real_dir(lookup.fs.as_ref(), &child_abs).is_none()
                        && lookup.fs.read_symbolic_link(&child_abs).is_ok()
                    {
                        events.push(PackageEvent::error(
                            None,
                            format!(
                                "Failed to get information about path, for {}, skipping: Symlink cycle",
                                child_rel.path_string()
                            ),
                        ));
                    }
                    continue;
                }
            };
            if !cst.is_directory {
                continue;
            }
            if e.dirent_type == DirentType::Symlink {
                match Self::resolve_real_dir(lookup.fs.as_ref(), &child_abs) {
                    Some(real_child) if ancestors.contains(&real_child) => {
                        events.push(PackageEvent::error(
                            None,
                            format!(
                                "Failed to get information about path, for {}, skipping: Symlink cycle",
                                child_rel.path_string()
                            ),
                        ));
                        continue;
                    }
                    None => {
                        events.push(PackageEvent::error(
                            None,
                            format!(
                                "Failed to get information about path, for {}, skipping: Symlink cycle",
                                child_rel.path_string()
                            ),
                        ));
                        continue;
                    }
                    _ => {}
                }
            }
            let Ok(child) = PackageIdentifier::create(id.repository().clone(), child_rel) else {
                continue; // directory name is not a valid package component
            };
            self.walk_dirs(
                &child,
                root,
                false,
                excluded_subdirs,
                ancestors,
                out,
                events,
            );
        }
        ancestors.pop();
    }

    /// Resolves patterns in order: positives add, negatives subtract from what was accumulated so
    /// far (`TargetPatternPhaseFunction`).
    pub fn resolve_patterns(&self, patterns: &[String]) -> ResolveOutcome {
        self.resolve_patterns_with_policy(patterns, &FilteringPolicy::NoFilter)
    }

    /// Resolves patterns in order, optionally filtering out targets tagged `manual` from wildcard expansions
    /// (`TargetPattern::TargetsInPackage` and `TargetPattern::TargetsBelowDirectory`).
    pub fn resolve_patterns_filtered(
        &self,
        patterns: &[String],
        filter_manual: bool,
    ) -> ResolveOutcome {
        self.resolve_patterns_with_options(patterns, filter_manual, false)
    }

    pub fn resolve_patterns_with_options(
        &self,
        patterns: &[String],
        filter_manual: bool,
        keep_going: bool,
    ) -> ResolveOutcome {
        let policy = if filter_manual {
            FilteringPolicy::FilterManual
        } else {
            FilteringPolicy::NoFilter
        };
        self.resolve_patterns_with_policy_and_options(patterns, &policy, keep_going)
    }

    /// Resolves patterns in order with an explicit [`FilteringPolicy`]. Negative patterns always
    /// use `FilteringPolicy::NoFilter`, matching `TargetPatternPhaseFunction`.
    pub fn resolve_patterns_with_policy(
        &self,
        patterns: &[String],
        policy: &FilteringPolicy,
    ) -> ResolveOutcome {
        self.resolve_patterns_with_policy_and_options(patterns, policy, false)
    }

    pub fn resolve_patterns_with_policy_and_options(
        &self,
        patterns: &[String],
        policy: &FilteringPolicy,
        keep_going: bool,
    ) -> ResolveOutcome {
        let mut outcome = ResolveOutcome::default();
        let mut parsed_list: Vec<Option<(String, SignedTargetPattern)>> =
            Vec::with_capacity(patterns.len());
        for raw in patterns {
            match SignedTargetPattern::parse_with(
                raw,
                &self.relative_dir,
                &RepositoryName::main(),
                &self.main_mapping,
            ) {
                Ok(s) => parsed_list.push(Some((raw.clone(), s))),
                Err(e) => {
                    outcome.errors.push(PatternError {
                        pattern: raw.clone(),
                        message: e.0,
                        package_errors: None,
                    });
                    parsed_list.push(None);
                }
            }
        }
        // Smart negation: fold negative `TargetsBelowDirectory` patterns into preceding positive
        // `TargetsBelowDirectory` patterns unless a later positive pattern overlaps them.
        let mut excluded_for: Vec<Vec<PathFragment>> = vec![Vec::new(); parsed_list.len()];
        let mut folded_negative: Vec<bool> = vec![false; parsed_list.len()];
        for k in 0..parsed_list.len() {
            let Some((
                _,
                SignedTargetPattern {
                    sign: PatternSign::Negative,
                    pattern:
                        TargetPattern::TargetsBelowDirectory {
                            directory: neg_dir, ..
                        },
                },
            )) = &parsed_list[k]
            else {
                continue;
            };
            let has_later_positive = (k + 1..parsed_list.len()).any(|m| {
                matches!(
                    &parsed_list[m],
                    Some((
                        _,
                        SignedTargetPattern {
                            sign: PatternSign::Positive,
                            ..
                        }
                    ))
                )
            });
            if has_later_positive {
                continue;
            }
            let mut covered_earlier = false;
            for j in 0..k {
                if let Some((
                    _,
                    SignedTargetPattern {
                        sign: PatternSign::Positive,
                        pattern:
                            TargetPattern::TargetsBelowDirectory {
                                directory: pos_dir, ..
                            },
                    },
                )) = &parsed_list[j]
                {
                    if pos_dir.repository() == neg_dir.repository()
                        && neg_dir
                            .package_fragment()
                            .starts_with(pos_dir.package_fragment())
                    {
                        excluded_for[j].push(neg_dir.package_fragment().clone());
                        covered_earlier = true;
                    }
                }
            }
            if covered_earlier {
                folded_negative[k] = true;
            }
        }

        let mut acc: Vec<Target> = Vec::new();
        let mut filtered: Vec<Target> = Vec::new();
        for (idx, item) in parsed_list.into_iter().enumerate() {
            let Some((raw, mut signed)) = item else {
                continue;
            };
            if folded_negative[idx] {
                continue;
            }
            let mut ambiguity_warnings = Vec::new();
            let mut replacement: Option<Label> = None;
            // `TargetsInPackage.getWildcardConflict`: `//pkg:all` naming an existing target uses that target.
            if let TargetPattern::TargetsInPackage { package, .. } = &signed.pattern {
                let unsigned = raw.strip_prefix('-').unwrap_or(&raw);
                if unsigned.starts_with("//") || unsigned.starts_with('@') {
                    if let Some((_, suffix)) = unsigned.rsplit_once(':') {
                        if let Ok(pkg) = self.package(package) {
                            if let Some(t) = pkg.targets.get(suffix) {
                                ambiguity_warnings.push(PackageEvent {
                                    kind: EventKind::Warning,
                                    location: None,
                                    message: format!(
                                        "The target pattern '{unsigned}' is ambiguous: ':{suffix}' is both a wildcard, and the name of an existing {}; using the latter interpretation",
                                        t.target_kind()
                                    ),
                                });
                                replacement = Label::create(package.clone(), suffix).ok();
                            }
                        }
                    }
                }
            }
            if let Some(l) = replacement {
                signed.pattern = TargetPattern::SingleTarget(l);
            }
            outcome.events.extend(ambiguity_warnings);
            let explicit = !matches!(
                signed.pattern,
                TargetPattern::TargetsInPackage { .. }
                    | TargetPattern::TargetsBelowDirectory { .. }
            );
            let mut events = Vec::new();
            let res = match &signed.pattern {
                TargetPattern::TargetsBelowDirectory {
                    directory,
                    include_files,
                } => {
                    match self.expand_below_directory(
                        directory,
                        *include_files,
                        &excluded_for[idx],
                        keep_going,
                        &mut events,
                    ) {
                        Ok((ts, pkg_errs)) => {
                            for msg in pkg_errs {
                                outcome.errors.push(PatternError {
                                    pattern: raw.clone(),
                                    message: msg,
                                    package_errors: None,
                                });
                            }
                            Ok(ts)
                        }
                        Err(e) => Err(e),
                    }
                }
                _ => self.expand(&signed.pattern, &mut events),
            };
            outcome.events.extend(events);
            match (signed.sign, res) {
                (_, Err(message)) => {
                    let package_errors = self.package_errors_for(&signed.pattern, &message);
                    outcome.errors.push(PatternError {
                        pattern: raw,
                        message,
                        package_errors,
                    })
                }
                (PatternSign::Positive, Ok(ts)) => {
                    for t in ts {
                        if !explicit && !matches!(policy, FilteringPolicy::NoFilter) {
                            if let Ok(pkg) = self.package(t.label().package_identifier()) {
                                if !policy.should_retain(&t, explicit, &pkg) {
                                    if !acc.iter().any(|x| x.label() == t.label())
                                        && !filtered.iter().any(|x| x.label() == t.label())
                                    {
                                        filtered.push(t);
                                    }
                                    continue;
                                }
                            }
                        }
                        filtered.retain(|x| x.label() != t.label());
                        if !acc.iter().any(|x| x.label() == t.label()) {
                            acc.push(t);
                        }
                    }
                }
                (PatternSign::Negative, Ok(ts)) => {
                    let remove: HashSet<Label> = ts.iter().map(|t| t.label().clone()).collect();
                    acc.retain(|t| !remove.contains(t.label()));
                    filtered.retain(|t| !remove.contains(t.label()));
                }
            }
        }
        outcome.targets = acc;
        outcome.filtered_targets = filtered;
        outcome
    }

    /// If `message` is the "package contains errors" failure of a pattern naming one package,
    /// returns the details Bazel's `package contains errors: ...` event needs.
    fn package_errors_for(&self, pattern: &TargetPattern, message: &str) -> Option<PackageErrors> {
        let (id, include_files) = match pattern {
            TargetPattern::TargetsInPackage {
                package,
                include_files,
            } => ((*package).clone(), *include_files),
            _ => return None,
        };
        let frag = id.package_fragment();
        if message != format!("error loading package '{frag}': Package '{frag}' contains errors") {
            return None;
        }
        let pkg = self.package(&id).ok()?;
        let first_error = pkg
            .events
            .iter()
            .find(|e| e.kind == EventKind::Error)
            .map(|e| e.message.clone())
            .unwrap_or_default();
        Some(PackageErrors {
            package: id,
            include_files,
            first_error,
        })
    }

    /// Renders error events (`ERROR: ...` lines) for display.
    pub fn render_error_events(events: &[PackageEvent]) -> Vec<String> {
        events
            .iter()
            .filter(|e| e.kind == EventKind::Error)
            .map(|e| e.render())
            .collect()
    }
}

impl PatternResolver {
    /// Events of loaded packages that have not been reported yet (each package once, ordered by
    /// package identifier).
    pub fn drain_new_events(&self) -> Vec<PackageEvent> {
        let cache = self.cache.lock().unwrap();
        let mut ids: Vec<&PackageIdentifier> = cache.keys().collect();
        ids.sort();
        let mut out = Vec::new();
        for id in ids {
            if let Some(Ok(p)) = cache.get(id) {
                if self.reported.lock().unwrap().insert(id.clone()) {
                    out.extend(p.events.iter().cloned());
                }
            }
        }
        out
    }

    /// Loads the transitive closure of packages referenced by the dependency labels of the rules
    /// in `roots`. Returns `(referencing rule label, error message)` for every dependency that
    /// cannot be resolved (missing package or target).
    pub fn load_transitive_closure(
        &self,
        roots: &[Target],
    ) -> (Vec<Target>, Vec<(Label, Label, String)>) {
        let mut order: Vec<Target> = Vec::new();
        let mut visited: HashSet<Label> = HashSet::new();
        let mut errors: Vec<(Label, Label, String)> = Vec::new();
        for r in roots {
            self.visit(r, &mut visited, &mut order, &mut errors);
        }
        (order, errors)
    }

    fn visit(
        &self,
        t: &Target,
        visited: &mut HashSet<Label>,
        order: &mut Vec<Target>,
        errors: &mut Vec<(Label, Label, String)>,
    ) {
        if !visited.insert(t.label().clone()) {
            return;
        }
        match t {
            Target::Rule(r) => {
                for dep in r.dependency_labels() {
                    match self.get_target(&dep) {
                        Ok(dt) => self.visit(&dt, visited, order, errors),
                        Err(msg) => {
                            errors.push((r.label.clone(), dep, msg));
                        }
                    }
                }
                order.push(t.clone());
            }
            Target::OutputFile {
                generating_rule, ..
            } => {
                if let Ok(rt) = self.get_target(generating_rule) {
                    self.visit(&rt, visited, order, errors);
                }
            }
            _ => {}
        }
    }
}
