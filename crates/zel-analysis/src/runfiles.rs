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

//! `Runfiles` and `RunfilesSupport` (`Runfiles.java`, `RunfilesSupport.java`).

use crate::Artifact;
use std::collections::BTreeMap;

/// The set of files a binary needs at runtime (`ctx.runfiles(...)` result).
///
/// * `artifacts`: files mapped at `<workspace>/<short_path>` (ordered, de-duplicated).
/// * `symlinks`: explicit `path -> file` entries relative to `<workspace>/`.
/// * `root_symlinks`: explicit entries relative to the runfiles root.
/// * `empty_filenames`: empty files created in the tree (e.g. python `__init__.py`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Runfiles {
    pub artifacts: Vec<Artifact>,
    pub symlinks: BTreeMap<String, Artifact>,
    pub root_symlinks: BTreeMap<String, Artifact>,
    pub empty_filenames: Vec<String>,
    pub suppress_init: bool,
    /// `Runfiles.getArtifacts()` with its `NestedSet` structure (`(exec path, owner repository)`
    /// elements, compile order). Empty when only `artifacts` was filled: see [`Runfiles::nested_set`].
    pub nested: crate::nested_set::NSet<(String, String)>,
}

impl Runfiles {
    pub fn is_empty(&self) -> bool {
        self.artifacts.is_empty()
            && self.symlinks.is_empty()
            && self.root_symlinks.is_empty()
            && self.empty_filenames.is_empty()
    }

    /// The nested artifact set: the recorded structure, or (for runfiles that were built from a
    /// plain list) a set of direct elements (`Builder.addArtifacts`).
    pub fn nested_set(&self) -> crate::nested_set::NSet<(String, String)> {
        use crate::nested_set::{NSet, COMPILE_ORDER};
        if self.nested.is_empty() && !self.artifacts.is_empty() {
            let direct: Vec<(String, String)> = self.artifacts.iter().map(Runfiles::elem).collect();
            return NSet::build(COMPILE_ORDER, direct, vec![]);
        }
        self.nested.clone()
    }

    /// `(exec path, owner repository)` of an artifact as a set element.
    pub fn elem(a: &Artifact) -> (String, String) {
        (a.exec_path.path_string().to_string(), a.owner_label.repository().name().to_string())
    }

    /// `Runfiles.Builder`: `direct` artifacts plus the transitive artifact sets of `parts`.
    pub fn build_nested(
        direct: &[Artifact],
        transitive: Vec<crate::nested_set::NSet<(String, String)>>,
    ) -> crate::nested_set::NSet<(String, String)> {
        crate::nested_set::NSet::build(crate::nested_set::COMPILE_ORDER, direct.iter().map(Runfiles::elem).collect(), transitive)
    }

    /// `Runfiles.merge`: keeps first occurrence order, de-duplicating by exec path.
    pub fn merge(&self, other: &Runfiles) -> Runfiles {
        let mut out = self.clone();
        out.nested = if other.is_empty() {
            self.nested_set()
        } else {
            crate::nested_set::NSet::build(crate::nested_set::COMPILE_ORDER, vec![], vec![self.nested_set(), other.nested_set()])
        };
        for a in &other.artifacts {
            if !out.artifacts.iter().any(|x| x.exec_path == a.exec_path) {
                out.artifacts.push(a.clone());
            }
        }
        for (k, v) in &other.symlinks {
            out.symlinks.entry(k.clone()).or_insert_with(|| v.clone());
        }
        for (k, v) in &other.root_symlinks {
            out.root_symlinks.entry(k.clone()).or_insert_with(|| v.clone());
        }
        for e in &other.empty_filenames {
            if !out.empty_filenames.contains(e) {
                out.empty_filenames.push(e.clone());
            }
        }
        out.suppress_init |= other.suppress_init;
        out
    }

    pub fn merge_all<'a>(items: impl IntoIterator<Item = &'a Runfiles>) -> Runfiles {
        items
            .into_iter()
            .fold(Runfiles::default(), |acc, r| acc.merge(r))
    }

    /// Computes the manifest entries `(runfiles-relative path, source exec path or "")` the way
    /// `Runfiles.getRunfilesInputs` does: artifacts at `<workspace>/<short_path>` (or
    /// `<repo>/<path>` for external files), then `symlinks`, then `root_symlinks`, and, unless
    /// `suppress_init`, nothing implicit (Python init files are added by the Python rules).
    pub fn manifest_entries(&self, workspace_name: &str) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        for a in &self.artifacts {
            let sp = a.short_path();
            let key = match sp.strip_prefix("../") {
                Some(rest) => rest.to_string(),
                None => format!("{workspace_name}/{sp}"),
            };
            m.entry(key).or_insert_with(|| a.exec_path.to_string());
        }
        for (k, v) in &self.symlinks {
            m.insert(format!("{workspace_name}/{k}"), v.exec_path.to_string());
        }
        for (k, v) in &self.root_symlinks {
            m.insert(k.clone(), v.exec_path.to_string());
        }
        for e in &self.empty_filenames {
            // Workspace-relative paths; `../<repo>/...` names an external repo's directory.
            let key = match e.strip_prefix("../") {
                Some(root_rel) => root_rel.to_string(),
                None => format!("{workspace_name}/{e}"),
            };
            m.entry(key).or_insert_with(String::new);
        }
        m
    }

    /// Exact `MANIFEST` file text: one `"<runfiles path> <source path>"` line per entry.
    pub fn manifest_text(&self, workspace_name: &str) -> String {
        let mut out = String::new();
        for (k, v) in self.manifest_entries(workspace_name) {
            out.push_str(&k);
            out.push(' ');
            out.push_str(&v);
            out.push('\n');
        }
        out
    }
}

/// `RunfilesSupport`: the `<binary>.runfiles` tree, its manifest, and the runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunfilesSupport {
    pub runfiles: Runfiles,
    /// The executable the runfiles belong to.
    pub executable: Artifact,
    /// `<exec path of executable>.runfiles`
    pub runfiles_dir: String,
    /// `<executable>.runfiles_manifest` (the manifest file, written by a FileWrite action).
    pub manifest: Artifact,
    pub workspace_name: String,
    /// `(source repo, apparent name, target directory)` lines of `<executable>.repo_mapping`,
    /// also written to `<runfiles dir>/_repo_mapping`.
    pub repo_mapping: Vec<(String, String, String)>,
    /// The `<executable>.repo_mapping` file.
    pub repo_mapping_manifest: Option<Artifact>,
}

impl RunfilesSupport {
    /// All files that must exist for the runfiles tree: the runfiles artifacts, the manifest.
    pub fn inputs(&self) -> Vec<Artifact> {
        let mut v = self.runfiles.artifacts.clone();
        v.extend(self.runfiles.symlinks.values().cloned());
        v.extend(self.runfiles.root_symlinks.values().cloned());
        v.push(self.manifest.clone());
        v
    }
}
