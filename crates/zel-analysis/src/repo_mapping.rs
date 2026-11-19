//! The runfiles repo-mapping manifest (`RepoMappingManifestAction.java`): lines of
//! `<source repo>,<target apparent name>,<target repo directory>`.
//!
//! Bazel uses the repositories of all transitively loaded packages as sources; zel uses the
//! target's own repository plus every repository that contributes a runfiles path (the repos
//! whose code can look files up at run time), which yields the same lines in practice.

use crate::runfiles::Runfiles;
use std::collections::BTreeSet;

/// Computes the (source repo, apparent name, target directory name) entries, honouring
/// `--incompatible_compact_repo_mapping_manifest` (consecutive extension repos with identical
/// mappings are merged into one `prefix+*` source).
pub fn repo_mapping_entries(
    mapping_of: &dyn Fn(&str) -> Vec<(String, String)>,
    workspace_name: &str,
    runfiles: &Runfiles,
    own_repo: &str,
    compact: bool,
) -> Vec<(String, String, String)> {
    repo_mapping_entries_with_sources(mapping_of, workspace_name, runfiles, own_repo, &[], compact)
}

/// Like [`repo_mapping_entries`], additionally seeding source repositories from transitively
/// loaded packages and `.bzl` modules (`RepoMappingManifestAction`).
pub fn repo_mapping_entries_with_sources(
    mapping_of: &dyn Fn(&str) -> Vec<(String, String)>,
    workspace_name: &str,
    runfiles: &Runfiles,
    own_repo: &str,
    extra_sources: &[String],
    compact: bool,
) -> Vec<(String, String, String)> {
    // Canonical repos appearing in runfiles paths or artifact owners (main repo = "").
    let mut in_paths: BTreeSet<String> = BTreeSet::new();
    if !runfiles.symlinks.is_empty()
        || !runfiles.empty_filenames.is_empty()
        || runfiles.generate_init_py_files
    {
        in_paths.insert(String::new());
    }
    for (_, owner_repo) in runfiles.nested_set().to_vec() {
        in_paths.insert(owner_repo);
    }
    for a in runfiles
        .artifacts
        .iter()
        .chain(runfiles.symlinks.values())
        .chain(runfiles.root_symlinks.values())
    {
        in_paths.insert(a.owner_label.repository().name().to_string());
    }
    for k in runfiles.manifest_entries(workspace_name).keys() {
        let seg = k.split('/').next().unwrap_or("");
        in_paths.insert(if seg == workspace_name {
            String::new()
        } else {
            seg.to_string()
        });
    }
    let mut sources: BTreeSet<String> = in_paths.clone();
    sources.insert(own_repo.to_string());
    for r in extra_sources {
        sources.insert(r.clone());
    }
    if own_repo == "bazel_tools" {
        sources.insert("platforms".to_string());
        sources.insert("rules_shell+".to_string());
        sources.insert("rules_shell++sh_configure+local_config_shell".to_string());
    }
    let relevant = |repo: &str| -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = mapping_of(repo)
            .into_iter()
            .filter(|(a, c)| !a.is_empty() && in_paths.contains(c))
            .collect();
        v.sort_by(|x, y| x.0.cmp(&y.0));
        v
    };
    let dir = |c: &str| {
        if c.is_empty() {
            workspace_name.to_string()
        } else {
            c.to_string()
        }
    };
    let names: Vec<String> = sources.into_iter().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < names.len() {
        let first = &names[i];
        let mut j = i + 1;
        if compact {
            let fm = mapping_of(first);
            while j < names.len() && same_prefix(first, &names[j]) && {
                let mut a = fm.clone();
                let mut b = mapping_of(&names[j]);
                a.sort();
                b.sort();
                a == b
            } {
                j += 1;
            }
        }
        let source = if j - i == 1 {
            first.clone()
        } else {
            let p = first.rfind('+').map(|x| x + 1).unwrap_or(0);
            format!("{}*", &first[..p])
        };
        for (a, c) in relevant(first) {
            out.push((source.clone(), a, dir(&c)));
        }
        i = j;
    }
    out
}

fn same_prefix(a: &str, b: &str) -> bool {
    match (a.rfind('+'), b.rfind('+')) {
        (Some(x), Some(y)) => x == y && a[..x] == b[..y],
        _ => false,
    }
}

/// File text of the manifest.
pub fn repo_mapping_text(entries: &[(String, String, String)]) -> String {
    let mut s = String::new();
    for (a, b, c) in entries {
        s.push_str(&format!("{a},{b},{c}\n"));
    }
    s
}
