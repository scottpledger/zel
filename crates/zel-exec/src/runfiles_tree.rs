//! Materializes `<binary>.runfiles` trees with `build-runfiles` semantics: symlinks for every
//! manifest entry, empty files for empty-valued entries, stale entries removed, plus the
//! `MANIFEST`, `<binary>.runfiles_manifest` and `_repo_mapping` files.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

/// Options mirroring `--enable_runfiles` and `--legacy_external_runfiles`.
#[derive(Debug, Clone)]
pub struct RunfilesTreeOptions {
    pub enable_runfiles: bool,
    pub legacy_external_runfiles: bool,
    /// `(source_repo, apparent_name, canonical_name)` entries for `_repo_mapping`.
    pub repo_mapping: Vec<(String, String, String)>,
}

impl Default for RunfilesTreeOptions {
    fn default() -> Self {
        Self {
            enable_runfiles: true,
            legacy_external_runfiles: false,
            repo_mapping: Vec::new(),
        }
    }
}

fn walk(dir: &Path, base: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        out.push(p.strip_prefix(base).unwrap().to_path_buf());
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            walk(&p, base, out);
        }
    }
}

/// Builds the runfiles tree. `entries` maps runfiles-relative paths to execroot-relative (or
/// absolute) source paths; an empty value creates an empty file. Returns the manifest text written.
pub fn build_runfiles_tree(
    exec_root: &Path,
    executable: &Path,
    workspace_name: &str,
    entries: &BTreeMap<String, String>,
    opts: &RunfilesTreeOptions,
) -> io::Result<String> {
    let mut dir = executable.as_os_str().to_owned();
    dir.push(".runfiles");
    let dir = PathBuf::from(dir);
    let mut manifest_path = executable.as_os_str().to_owned();
    manifest_path.push(".runfiles_manifest");

    let abs = |v: &str| -> String {
        if v.is_empty() || v.starts_with('/') {
            v.to_string()
        } else {
            exec_root.join(v).to_string_lossy().into_owned()
        }
    };
    let mut manifest = String::new();
    let mut all: BTreeMap<String, String> = entries.clone();
    // Legacy external runfiles: also expose external repos under `<workspace>/external/<repo>`.
    if opts.legacy_external_runfiles {
        for (k, v) in entries {
            if !k.starts_with(&format!("{workspace_name}/")) && !k.starts_with("_") {
                if let Some((repo, rest)) = k.split_once('/') {
                    all.entry(format!("{workspace_name}/external/{repo}/{rest}"))
                        .or_insert_with(|| v.clone());
                }
            }
        }
    }
    for (k, v) in &all {
        manifest.push_str(k);
        manifest.push(' ');
        manifest.push_str(&abs(v));
        manifest.push('\n');
    }
    if let Some(p) = manifest_path_parent(&manifest_path) {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(&manifest_path, &manifest)?;
    if !opts.enable_runfiles {
        // Manifest-only mode: no symlink tree.
        let _ = std::fs::remove_dir_all(&dir);
        return Ok(manifest);
    }

    std::fs::create_dir_all(dir.join(workspace_name))?;
    // Desired set of paths (files and parent dirs).
    let mut want_files: BTreeSet<PathBuf> = BTreeSet::new();
    let mut want_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    want_dirs.insert(PathBuf::from(workspace_name));
    for k in all.keys() {
        let p = PathBuf::from(k);
        let mut anc = p.parent();
        while let Some(a) = anc {
            if !a.as_os_str().is_empty() {
                want_dirs.insert(a.to_path_buf());
            }
            anc = a.parent();
        }
        want_files.insert(p);
    }
    for k in ["MANIFEST", "_repo_mapping"] {
        want_files.insert(PathBuf::from(k));
    }
    // Remove stale entries.
    let mut existing = Vec::new();
    walk(&dir, &dir, &mut existing);
    existing.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for rel in existing {
        let full = dir.join(&rel);
        let md = std::fs::symlink_metadata(&full)?;
        if md.is_dir() {
            if !want_dirs.contains(&rel) {
                let _ = std::fs::remove_dir(&full);
            }
        } else if !want_files.contains(&rel) {
            let _ = std::fs::remove_file(&full);
        }
    }
    for (k, v) in &all {
        let link = dir.join(k);
        if let Some(p) = link.parent() {
            std::fs::create_dir_all(p)?;
        }
        let src = abs(v);
        if src.is_empty() {
            if std::fs::symlink_metadata(&link).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                let _ = std::fs::remove_file(&link);
            }
            if !link.exists() {
                std::fs::write(&link, b"")?;
            }
            continue;
        }
        match std::fs::read_link(&link) {
            Ok(t) if t == Path::new(&src) => continue,
            Ok(_) => {
                let _ = std::fs::remove_file(&link);
            }
            Err(_) => {
                if std::fs::symlink_metadata(&link).is_ok() {
                    let _ = std::fs::remove_file(&link);
                }
            }
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&src, &link)?;
    }
    std::fs::write(dir.join("MANIFEST"), &manifest)?;
    let mut rm = String::new();
    for (src, app, canon) in &opts.repo_mapping {
        rm.push_str(&format!("{src},{app},{canon}\n"));
    }
    std::fs::write(dir.join("_repo_mapping"), rm)?;
    Ok(manifest)
}

fn manifest_path_parent(p: &std::ffi::OsString) -> Option<&Path> {
    Path::new(p).parent()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_tree_prunes_stale_and_writes_manifest() {
        let root = std::env::temp_dir().join(format!("zel-rf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        std::fs::write(root.join("pkg/data.txt"), "d").unwrap();
        let exe = root.join("bazel-out/k8-fastbuild/bin/pkg/t");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        let mut entries = BTreeMap::new();
        entries.insert("_main/pkg/data.txt".to_string(), "pkg/data.txt".to_string());
        entries.insert("_main/pkg/__init__.py".to_string(), String::new());
        entries.insert("ext+/lib/x.txt".to_string(), "pkg/data.txt".to_string());
        let m = build_runfiles_tree(&root, &exe, "_main", &entries, &RunfilesTreeOptions::default()).unwrap();
        let rf = PathBuf::from(format!("{}.runfiles", exe.display()));
        assert_eq!(std::fs::read_to_string(rf.join("_main/pkg/data.txt")).unwrap(), "d");
        assert!(std::fs::symlink_metadata(rf.join("_main/pkg/data.txt")).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read(rf.join("_main/pkg/__init__.py")).unwrap(), b"");
        assert!(rf.join("_repo_mapping").exists());
        assert_eq!(std::fs::read_to_string(rf.join("MANIFEST")).unwrap(), m);
        assert!(m.contains(&format!("_main/pkg/data.txt {}/pkg/data.txt", root.display())));
        assert!(std::path::Path::new(&format!("{}.runfiles_manifest", exe.display())).exists());
        // Stale entry removed on rebuild.
        std::fs::write(rf.join("_main/stale"), "x").unwrap();
        entries.remove("ext+/lib/x.txt");
        build_runfiles_tree(&root, &exe, "_main", &entries, &RunfilesTreeOptions::default()).unwrap();
        assert!(!rf.join("_main/stale").exists());
        assert!(!rf.join("ext+/lib/x.txt").exists());
        // Manifest-only mode removes the tree.
        let o = RunfilesTreeOptions { enable_runfiles: false, ..Default::default() };
        build_runfiles_tree(&root, &exe, "_main", &entries, &o).unwrap();
        assert!(!rf.exists());
    }
}
