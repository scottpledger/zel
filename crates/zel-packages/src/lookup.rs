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

//! Package lookup: which directories are packages (`PackageLookupFunction`), `.bazelignore`,
//! `REPO.bazel`, `--deleted_packages`.

use crate::glob;
use std::collections::BTreeSet;
use std::sync::Arc;
use zel_cmdline::{Label, PackageIdentifier, RepositoryMapping, RepositoryName};
use zel_vfs::{FileSystem, PathFragment};

/// Maps repositories to their source roots and apparent names to canonical names.
pub trait RepoResolver: Send + Sync {
    /// Root directory of `repo`, or `None` if the repository is not defined.
    fn repo_root(&self, repo: &RepositoryName) -> Option<PathFragment>;
    /// Repository mapping used when parsing labels that appear in files of `owner`.
    fn mapping(&self, owner: &RepositoryName) -> RepositoryMapping;
}

/// A per-runtime holder for the external repository resolver of the command currently running.
///
/// Cloning shares the slot. The server owns one slot per `BlazeRuntime` and installs the
/// command's resolver (Bzlmod: fetch-on-demand into `<output_base>/external`) before the command
/// runs; Skyframe functions registered for that runtime read it, so independent runtimes in one
/// process never observe each other's resolver.
#[derive(Clone, Default)]
pub struct RepoResolverSlot(Arc<std::sync::RwLock<Option<Arc<dyn RepoResolver>>>>);

impl RepoResolverSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Installs (or clears) the resolver.
    pub fn set(&self, resolver: Option<Arc<dyn RepoResolver>>) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = resolver;
    }

    /// The installed resolver, if any.
    pub fn get(&self) -> Option<Arc<dyn RepoResolver>> {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// A resolver backed by explicit tables (used by the server and tests).
#[derive(Debug, Clone)]
pub struct SimpleRepoResolver {
    pub main_root: PathFragment,
    /// Canonical repository name → root directory.
    pub external: std::collections::HashMap<String, PathFragment>,
    /// Apparent name → canonical repository (shared by all owners).
    pub apparent: std::collections::HashMap<String, RepositoryName>,
}

impl SimpleRepoResolver {
    pub fn new(main_root: PathFragment) -> Self {
        Self {
            main_root,
            external: Default::default(),
            apparent: Default::default(),
        }
    }
}

impl RepoResolver for SimpleRepoResolver {
    fn repo_root(&self, repo: &RepositoryName) -> Option<PathFragment> {
        if repo.is_main() {
            Some(self.main_root.clone())
        } else {
            self.external.get(repo.name()).cloned()
        }
    }

    fn mapping(&self, owner: &RepositoryName) -> RepositoryMapping {
        RepositoryMapping::new(self.apparent.clone(), owner.clone())
    }
}

/// Severity of a [`PackageEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Error,
    Warning,
    Info,
    Debug,
}

/// A message produced while loading a package (`Event` in Bazel).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageEvent {
    pub kind: EventKind,
    /// `file:line:col` when known.
    pub location: Option<String>,
    pub message: String,
}

impl PackageEvent {
    pub fn error(location: Option<String>, message: impl Into<String>) -> Self {
        Self {
            kind: EventKind::Error,
            location,
            message: message.into(),
        }
    }

    /// Renders as Bazel prints it on the console (`ERROR: loc: message`).
    pub fn render(&self) -> String {
        let prefix = match self.kind {
            EventKind::Error => "ERROR",
            EventKind::Warning => "WARNING",
            EventKind::Info => "INFO",
            EventKind::Debug => "DEBUG",
        };
        match &self.location {
            Some(l) => format!("{prefix}: {l}: {}", self.message),
            None => format!("{prefix}: {}", self.message),
        }
    }
}

/// Outcome of looking up a package (`PackageLookupValue`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageLookup {
    Found { build_file: PathFragment },
    NoBuildFile,
    Deleted(String),
    InvalidName(String),
    RepoNotFound(String),
}

/// Looks up `BUILD.bazel` then `BUILD` in `pkg_dir`; both must be regular files (a *directory*
/// named `BUILD` does not make a package).
pub fn find_build_file(fs: &dyn FileSystem, pkg_dir: &PathFragment) -> Option<PathFragment> {
    for name in ["BUILD.bazel", "BUILD"] {
        let p = pkg_dir.get_relative_str(name);
        if let Ok(st) = fs.stat(&p, true) {
            if st.is_file {
                return Some(p);
            }
        }
    }
    None
}

/// Parsed `REPO.bazel`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoFile {
    /// Glob patterns from `ignore_directories()`.
    pub ignored_directories: Vec<String>,
}

/// Parses `.bazelignore` contents like `PathFragmentLineProcessor` + `getIgnoredPrefixes`.
pub fn parse_bazelignore(contents: &str) -> Result<Vec<PathFragment>, String> {
    let mut out = Vec::new();
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let frag = PathFragment::create(line);
        if frag.is_absolute() {
            return Err(format!("'{frag}': cannot be an absolute path"));
        }
        out.push(frag);
    }
    Ok(out)
}

/// Everything needed to decide whether a directory is a package.
pub struct PackageLookupConfig {
    pub fs: Arc<dyn FileSystem>,
    pub resolver: Arc<dyn RepoResolver>,
    pub deleted_packages: BTreeSet<PackageIdentifier>,
}

impl PackageLookupConfig {
    /// Entries of `.bazelignore` in `repo_root` (main repository only in Bazel; applied to every
    /// repo here since external repos may also ship one).
    pub fn bazelignore(&self, repo_root: &PathFragment) -> Vec<PathFragment> {
        match self.fs.read_file(&repo_root.get_relative_str(".bazelignore")) {
            Ok(bytes) => parse_bazelignore(&String::from_utf8_lossy(&bytes)).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    /// `ignore_directories()` patterns from `REPO.bazel`, if any.
    pub fn repo_ignored_dirs(&self, repo_root: &PathFragment) -> Vec<String> {
        crate::loader::parse_repo_file(self.fs.as_ref(), repo_root)
            .map(|r| r.ignored_directories)
            .unwrap_or_default()
    }

    /// Is `pkg` ignored by `.bazelignore`, returning the reason message.
    pub fn ignored_reason(&self, pkg: &PackageIdentifier, repo_root: &PathFragment) -> Option<String> {
        let frag = pkg.package_fragment();
        for e in self.bazelignore(repo_root) {
            if !e.is_empty() && frag.starts_with(&e) {
                return Some("Package is considered deleted due to .bazelignore".to_string());
            }
        }
        let pats = self.repo_ignored_dirs(repo_root);
        if !pats.is_empty() {
            let segs: Vec<&str> = frag.segments();
            for p in &pats {
                let psegs: Vec<&str> = p.split('/').collect();
                // A directory is ignored if the pattern matches it or any of its ancestors.
                for k in 1..=segs.len() {
                    if glob::path_matches(&psegs, &segs[..k]) {
                        return Some(
                            "Package is considered deleted due to ignore_directories() in REPO.bazel"
                                .to_string(),
                        );
                    }
                }
            }
        }
        None
    }

    pub fn lookup(&self, pkg: &PackageIdentifier) -> PackageLookup {
        let Some(root) = self.resolver.repo_root(pkg.repository()) else {
            let repo_display = if pkg.repository().is_main() {
                pkg.repository().canonical_form()
            } else {
                format!("@@[unknown repo '{}' requested from @@]", pkg.repository().name())
            };
            return PackageLookup::RepoNotFound(format!(
                "The repository '{repo_display}' could not be resolved: No repository visible as '@{}' from main repository",
                pkg.repository().name()
            ));
        };
        if pkg.repository().is_main() && self.deleted_packages.contains(pkg) {
            return PackageLookup::Deleted(
                "Package is considered deleted due to --deleted_packages".to_string(),
            );
        }
        if let Some(reason) = self.ignored_reason(pkg, &root) {
            return PackageLookup::Deleted(reason);
        }
        let dir = root.get_relative(pkg.package_fragment());
        match find_build_file(self.fs.as_ref(), &dir) {
            Some(build_file) => PackageLookup::Found { build_file },
            None => PackageLookup::NoBuildFile,
        }
    }

    /// `PackageLookupFunction.explainNoBuildFileValue`.
    pub fn explain_no_build_file(&self, pkg: &PackageIdentifier) -> String {
        let educational = "Add a BUILD file to a directory to mark it as a package.";
        if pkg.repository().is_main() && pkg.package_fragment().path_string() == "external" {
            // `PackageFunction`: the `//external` package no longer exists.
            return "//external package is not available since the WORKSPACE file is deprecated, please migrate to Bzlmod. See https://bazel.build/external/migration#bind-targets.".to_string();
        }
        if pkg.repository().is_main() {
            format!(
                "BUILD file not found in any of the following directories. {educational}\n - {}",
                pkg.package_fragment()
            )
        } else {
            format!(
                "BUILD file not found in directory '{}' of external repository {}. {educational}",
                pkg.package_fragment(),
                pkg.repository().canonical_form()
            )
        }
    }

    /// The text after `no such package '<pkg>': ` for an unsuccessful lookup.
    pub fn error_message(&self, pkg: &PackageIdentifier, lookup: &PackageLookup) -> String {
        match lookup {
            PackageLookup::NoBuildFile => self.explain_no_build_file(pkg),
            PackageLookup::Deleted(r) | PackageLookup::InvalidName(r) | PackageLookup::RepoNotFound(r) => {
                r.clone()
            }
            PackageLookup::Found { .. } => String::new(),
        }
    }

    /// For a label `l`, finds the deepest enclosing sub-package that makes the label cross a
    /// package boundary, and renders Bazel's error (`ContainingPackageLookupValue`). Returns `None`
    /// if the label's package is the closest containing package of the label's path.
    pub fn label_crossing_subpackage_error(&self, label: &Label) -> Option<String> {
        let name_path = PathFragment::create(label.name());
        let segs: Vec<String> = name_path.segments().iter().map(|s| s.to_string()).collect();
        if segs.len() < 2 {
            return None;
        }
        let _ = self.resolver.repo_root(label.package_identifier().repository())?;
        let pkg_frag = label.package_identifier().package_fragment().clone();
        // Examine directories pkg/seg0, pkg/seg0/seg1, ... (all but the file itself); the deepest
        // existing sub-package wins.
        let mut found: Option<(usize, PathFragment)> = None;
        let mut cur = pkg_frag.clone();
        for (i, seg) in segs[..segs.len() - 1].iter().enumerate() {
            cur = cur.get_relative_str(seg);
            let id = PackageIdentifier::create(label.package_identifier().repository().clone(), cur.clone()).ok()?;
            if let PackageLookup::Found { .. } = self.lookup(&id) {
                found = Some((i + 1, cur.clone()));
            }
        }
        let (depth, sub) = found?;
        let rest = segs[depth..].join("/");
        let label_str = label.canonical_form();
        let sub_display = sub.path_string().to_string();
        let prefix = if label.package_identifier().repository().is_main() {
            "//"
        } else {
            ""
        };
        Some(format!(
            "Label '{label_str}' is invalid because '{sub_display}' is a subpackage; perhaps you meant to put the colon here: '{prefix}{sub_display}:{rest}'?"
        ))
    }
}
