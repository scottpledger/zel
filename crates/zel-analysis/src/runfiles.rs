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
use std::collections::{BTreeMap, BTreeSet};

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
    pub symlink_entries: Vec<(String, Artifact)>,
    pub root_symlink_entries: Vec<(String, Artifact)>,
    pub empty_filenames: Vec<String>,
    /// Legacy Python `__init__.py` empty-files supplier (`GetInitPyFiles`): propagated across
    /// `merge` and evaluated over the final manifest entries (after obscuring-symlink filtering).
    pub generate_init_py_files: bool,
    pub suppress_init: bool,
    /// `Runfiles.ConflictPolicy`: true = ERROR (when symlinks were explicitly supplied without
    /// `skip_conflict_checking`), false = WARN.
    pub conflict_policy_error: bool,
    /// `Runfiles.getArtifacts()` with its `NestedSet` structure (`(exec path, owner repository)`
    /// elements, compile order). Empty when only `artifacts` was filled: see [`Runfiles::nested_set`].
    pub nested: crate::nested_set::NSet<(String, String)>,
}

/// Deduplicates `artifacts` in compile-order (later element wins on equal runfiles path / exec path),
/// preserving postorder insertion semantics.
fn dedup_keep_last(items: Vec<Artifact>) -> Vec<Artifact> {
    let mut seen = BTreeSet::new();
    let mut rev = Vec::with_capacity(items.len());
    for a in items.into_iter().rev() {
        if seen.insert(a.exec_path.path_string().to_string()) {
            rev.push(a);
        }
    }
    rev.reverse();
    rev
}

/// `Runfiles.filterListForObscuringSymlinks`: drops any entry whose proper ancestor directory
/// path is also a symlink/file entry in `working`.
fn filter_obscuring_symlinks(working: &mut BTreeMap<String, String>) {
    let keys: Vec<String> = working.keys().cloned().collect();
    let mut to_remove = Vec::new();
    for k in &keys {
        let mut cur = k.as_str();
        while let Some((parent, _)) = cur.rsplit_once('/') {
            if working.contains_key(parent) {
                to_remove.push(k.clone());
                break;
            }
            cur = parent;
        }
    }
    for k in to_remove {
        working.remove(&k);
    }
}

impl Runfiles {
    /// `Runfiles.isEmpty`: ignores `empty_filenames` (matches `Runfiles.java`).
    pub fn is_empty(&self) -> bool {
        self.artifacts.is_empty()
            && self.symlinks.is_empty()
            && self.root_symlinks.is_empty()
            && self.symlink_entries.is_empty()
            && self.root_symlink_entries.is_empty()
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
        (
            a.exec_path.path_string().to_string(),
            a.owner_label.repository().name().to_string(),
        )
    }

    /// `Runfiles.Builder`: `direct` artifacts plus the transitive artifact sets of `parts`.
    pub fn build_nested(
        direct: &[Artifact],
        transitive: Vec<crate::nested_set::NSet<(String, String)>>,
    ) -> crate::nested_set::NSet<(String, String)> {
        crate::nested_set::NSet::build(
            crate::nested_set::COMPILE_ORDER,
            direct.iter().map(Runfiles::elem).collect(),
            transitive,
        )
    }

    /// Ordered `(path, target_file)` entries for Starlark `runfiles.symlinks`, falling back to
    /// `self.symlinks` when `symlink_entries` was not populated directly.
    pub fn ordered_symlinks(&self) -> Vec<(String, Artifact)> {
        if !self.symlink_entries.is_empty() || self.symlinks.is_empty() {
            self.symlink_entries.clone()
        } else {
            self.symlinks
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        }
    }

    /// Ordered `(path, target_file)` entries for Starlark `runfiles.root_symlinks`, falling back
    /// to `self.root_symlinks` when `root_symlink_entries` was not populated directly.
    pub fn ordered_root_symlinks(&self) -> Vec<(String, Artifact)> {
        if !self.root_symlink_entries.is_empty() || self.root_symlinks.is_empty() {
            self.root_symlink_entries.clone()
        } else {
            self.root_symlinks
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        }
    }

    /// `Runfiles.merge`: compile-order merge where `other` overwrites `self` on conflicting
    /// runfiles paths and adopts the stricter `conflict_policy_error`.
    pub fn merge(&self, other: &Runfiles) -> Runfiles {
        let mut out = self.clone();
        out.conflict_policy_error |= other.conflict_policy_error;
        out.generate_init_py_files |= other.generate_init_py_files;
        if other.is_empty() {
            out.nested = self.nested_set();
            out.symlink_entries = self.ordered_symlinks();
            out.root_symlink_entries = self.ordered_root_symlinks();
            for e in &other.empty_filenames {
                if !out.empty_filenames.contains(e) {
                    out.empty_filenames.push(e.clone());
                }
            }
            out.suppress_init |= other.suppress_init;
            return out;
        }
        out.nested = crate::nested_set::NSet::build(
            crate::nested_set::COMPILE_ORDER,
            vec![],
            vec![self.nested_set(), other.nested_set()],
        );
        out.artifacts.extend(other.artifacts.iter().cloned());
        out.artifacts = dedup_keep_last(std::mem::take(&mut out.artifacts));
        for (k, v) in other.ordered_symlinks() {
            out.symlinks.insert(k, v);
        }
        if !self.symlinks.is_empty()
            || !other.symlinks.is_empty()
            || !self.symlink_entries.is_empty()
            || !other.symlink_entries.is_empty()
        {
            let mut combined = self.ordered_symlinks();
            combined.extend(other.ordered_symlinks());
            out.symlink_entries = combined;
        }
        for (k, v) in other.ordered_root_symlinks() {
            out.root_symlinks.insert(k, v);
        }
        if !self.root_symlinks.is_empty()
            || !other.root_symlinks.is_empty()
            || !self.root_symlink_entries.is_empty()
            || !other.root_symlink_entries.is_empty()
        {
            let mut combined = self.ordered_root_symlinks();
            combined.extend(other.ordered_root_symlinks());
            out.root_symlink_entries = combined;
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

    /// Computes the manifest entries `(runfiles-relative path, source exec path or "")` matching
    /// `Runfiles.getRunfilesInputs`:
    /// `symlinks`, then `artifacts` (overwriting symlinks), then `filterListForObscuringSymlinks`,
    /// then `empty_filenames` (overwriting real files), then workspace-prefixing, then
    /// `root_symlinks`, then `<workspace>/.runfile` when no entry sits under `<workspace>`.
    pub fn manifest_entries(&self, workspace_name: &str) -> BTreeMap<String, String> {
        let mut working = BTreeMap::new();
        for (k, v) in self.ordered_symlinks() {
            working.insert(k, v.exec_path.to_string());
        }
        for a in &self.artifacts {
            working.insert(a.short_path(), a.exec_path.to_string());
        }
        filter_obscuring_symlinks(&mut working);
        for e in &self.empty_filenames {
            working.insert(e.clone(), String::new());
        }
        if self.generate_init_py_files && !self.suppress_init {
            let py_paths: Vec<String> = working
                .keys()
                .filter(|p| p.ends_with(".py") || p.ends_with(".so") || p.ends_with(".pyc"))
                .cloned()
                .collect();
            let mut extra = Vec::new();
            for p in &py_paths {
                let mut cur = p.as_str();
                while let Some((dir, _)) = cur.rsplit_once('/') {
                    cur = dir;
                    if cur.is_empty() || cur == ".." {
                        break;
                    }
                    let seg = cur.rsplit('/').next().unwrap_or(cur);
                    if seg == "__pycache__" {
                        continue;
                    }
                    let init_py = format!("{cur}/__init__.py");
                    let init_pyc = format!("{cur}/__init__.pyc");
                    if !working.contains_key(&init_py) && !working.contains_key(&init_pyc) {
                        extra.push(init_py);
                    }
                }
            }
            for init in extra {
                working.entry(init).or_default();
            }
        }

        let mut final_map = BTreeMap::new();
        let mut saw_workspace = false;
        let ws_prefix = format!("{workspace_name}/");
        for (path, target) in working {
            if let Some(ext) = path.strip_prefix("../") {
                if ext == workspace_name || ext.starts_with(&ws_prefix) {
                    saw_workspace = true;
                }
                final_map.insert(ext.to_string(), target);
            } else {
                saw_workspace = true;
                final_map.insert(format!("{workspace_name}/{path}"), target);
            }
        }
        for (k, v) in self.ordered_root_symlinks() {
            if k == workspace_name || k.starts_with(&ws_prefix) {
                saw_workspace = true;
            }
            final_map.insert(k, v.exec_path.to_string());
        }
        if !saw_workspace && !final_map.is_empty() {
            final_map.insert(format!("{workspace_name}/.runfile"), String::new());
        }
        final_map
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

#[cfg(test)]
mod tests {
    use super::*;
    use zel_cmdline::Label;
    use zel_vfs::PathFragment;

    fn art(p: &str) -> Artifact {
        Artifact::source(
            PathFragment::create(p),
            Label::parse_canonical("//a:t").unwrap(),
        )
    }

    #[test]
    fn merge_other_overwrites_and_artifacts_beat_symlinks() {
        let a1 = art("p/a");
        let a2 = art("p/b");
        let mut r1 = Runfiles::default();
        r1.symlinks.insert("link".into(), a1.clone());
        let mut r2 = Runfiles::default();
        r2.symlinks.insert("link".into(), a2.clone());
        let m = r1.merge(&r2);
        assert_eq!(m.symlinks["link"], a2);

        // Artifacts overwrite symlinks at the same runfiles path, and empty_filenames overwrite files.
        let mut r3 = Runfiles::default();
        r3.symlinks.insert("p/a".into(), a2.clone());
        r3.artifacts.push(a1.clone());
        let entries = r3.manifest_entries("_main");
        assert_eq!(entries["_main/p/a"], "p/a");

        r3.empty_filenames.push("p/a".into());
        let entries = r3.manifest_entries("_main");
        assert_eq!(entries["_main/p/a"], "");

        // is_empty() ignores empty_filenames.
        let r_empty_only = Runfiles {
            empty_filenames: vec!["a/__init__.py".into()],
            ..Default::default()
        };
        assert!(r_empty_only.is_empty());

        // External-only runfiles emit <workspace>/.runfile.
        let ext_art = Artifact::source(
            PathFragment::create("p/x"),
            Label::parse_canonical("@@other//p:x").unwrap(),
        );
        let r_ext = Runfiles {
            artifacts: vec![ext_art],
            ..Default::default()
        };
        let ext_entries = r_ext.manifest_entries("_main");
        assert_eq!(
            ext_entries.get("_main/.runfile").map(String::as_str),
            Some("")
        );
    }

    #[test]
    fn generated_init_py_supplier_propagates_across_merge_and_respects_pycache() {
        let base_art = Artifact::source(
            PathFragment::create("pkg/a.py"),
            Label::parse_canonical("//pkg:a.py").unwrap(),
        );
        let later_art = Artifact::source(
            PathFragment::create("later/sub/b.py"),
            Label::parse_canonical("//later/sub:b.py").unwrap(),
        );
        let cache_art = Artifact::source(
            PathFragment::create("cache_pkg/__pycache__/c.pyc"),
            Label::parse_canonical("//cache_pkg:c.pyc").unwrap(),
        );
        let not_cache_art = Artifact::source(
            PathFragment::create("my__pycache__/d.py"),
            Label::parse_canonical("//my__pycache__:d.py").unwrap(),
        );
        let r_base = Runfiles {
            artifacts: vec![base_art],
            generate_init_py_files: true,
            ..Default::default()
        };
        let r_later = Runfiles {
            artifacts: vec![later_art, cache_art, not_cache_art],
            ..Default::default()
        };
        let merged = r_base.merge(&r_later);
        assert!(merged.generate_init_py_files);
        let entries = merged.manifest_entries("_main");
        assert_eq!(
            entries.get("_main/pkg/__init__.py").map(String::as_str),
            Some("")
        );
        assert_eq!(
            entries.get("_main/later/__init__.py").map(String::as_str),
            Some("")
        );
        assert_eq!(
            entries
                .get("_main/later/sub/__init__.py")
                .map(String::as_str),
            Some("")
        );
        assert_eq!(
            entries
                .get("_main/cache_pkg/__init__.py")
                .map(String::as_str),
            Some("")
        );
        assert!(!entries.contains_key("_main/cache_pkg/__pycache__/__init__.py"));
        assert_eq!(
            entries
                .get("_main/my__pycache__/__init__.py")
                .map(String::as_str),
            Some("")
        );
    }

    #[test]
    fn merge_preserves_ordered_symlinks_and_root_symlinks_across_map_and_vec_representations() {
        let a1 = art("p/one");
        let a2 = art("p/two");
        let r1 = art("p/root_one");
        let r2 = art("p/root_two");

        // `map_only` populates `symlinks` / `root_symlinks` maps with empty `*_entries` vecs.
        let mut map_only = Runfiles::default();
        map_only.symlinks.insert("map_sym".into(), a1.clone());
        map_only
            .root_symlinks
            .insert("map_root_sym".into(), r1.clone());

        // `vec_backed` populates both maps and ordered `*_entries` vecs (`ctx.runfiles(symlinks=...)`).
        let mut vec_backed = Runfiles::default();
        vec_backed.symlinks.insert("vec_sym".into(), a2.clone());
        vec_backed
            .symlink_entries
            .push(("vec_sym".into(), a2.clone()));
        vec_backed
            .root_symlinks
            .insert("vec_root_sym".into(), r2.clone());
        vec_backed
            .root_symlink_entries
            .push(("vec_root_sym".into(), r2.clone()));

        // Direction 1: `vec_backed.merge(&map_only)` must retain `map_only`'s symlinks in `ordered_*`.
        let m1 = vec_backed.merge(&map_only);
        assert_eq!(
            m1.ordered_symlinks(),
            vec![
                ("vec_sym".to_string(), a2.clone()),
                ("map_sym".to_string(), a1.clone()),
            ]
        );
        assert_eq!(
            m1.ordered_root_symlinks(),
            vec![
                ("vec_root_sym".to_string(), r2.clone()),
                ("map_root_sym".to_string(), r1.clone()),
            ]
        );

        // Direction 2: `map_only.merge(&vec_backed)` must retain both in merge order.
        let m2 = map_only.merge(&vec_backed);
        assert_eq!(
            m2.ordered_symlinks(),
            vec![
                ("map_sym".to_string(), a1.clone()),
                ("vec_sym".to_string(), a2.clone()),
            ]
        );
        assert_eq!(
            m2.ordered_root_symlinks(),
            vec![
                ("map_root_sym".to_string(), r1.clone()),
                ("vec_root_sym".to_string(), r2.clone()),
            ]
        );

        // Direction 3: `map_only.merge(&map_only_other)` also populates ordered entries consistently.
        let mut map_only_other = Runfiles::default();
        map_only_other
            .symlinks
            .insert("map_sym_2".into(), a2.clone());
        let m3 = map_only.merge(&map_only_other);
        assert_eq!(
            m3.ordered_symlinks(),
            vec![("map_sym".to_string(), a1), ("map_sym_2".to_string(), a2),]
        );
    }
}
