//! Materializes `<binary>.runfiles` trees with `SymlinkTreeHelper` and `RunfilesTreeUpdater`
//! semantics: symlinks for every manifest entry, empty files for empty-valued entries, stale
//! entries removed (with chmod retry), directory-after-symlink pass, `MANIFEST` symlinked to
//! `<binary>.runfiles_manifest`, ISO-8859-1 manifest escaping, and `_repo_mapping`.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
pub use zel_sandbox::{
    delete_tree, delete_trees_below, link_manifest, sync_symlink_tree, RunfilesSymlinksMode,
    RunfilesTarget, RunfilesTreeData, RunfilesTreeUpdater,
};
use zel_vfs::PathFragment;

/// Options mirroring `--enable_runfiles` and `--legacy_external_runfiles`.
#[derive(Debug, Clone)]
pub struct RunfilesTreeOptions {
    pub enable_runfiles: bool,
    pub legacy_external_runfiles: bool,
    /// `(source_repo, apparent_name, canonical_name)` entries for `_repo_mapping`.
    pub repo_mapping: Vec<(String, String, String)>,
    /// Exec paths of unresolved symlink artifacts (`ctx.actions.declare_symlink` /
    /// `Artifact.is_unresolved_symlink`), whose raw `read_link` target must be replicated verbatim
    /// into the runfiles tree and manifest rather than absolutized to the execroot path.
    pub unresolved_symlink_paths: BTreeSet<String>,
}

impl Default for RunfilesTreeOptions {
    fn default() -> Self {
        Self {
            enable_runfiles: true,
            legacy_external_runfiles: false,
            repo_mapping: Vec::new(),
            unresolved_symlink_paths: BTreeSet::new(),
        }
    }
}

/// Port of `com.google.devtools.build.lib.exec.SymlinkTreeHelper`.
#[derive(Debug, Clone)]
pub struct SymlinkTreeHelper {
    pub input_manifest: PathBuf,
    pub output_manifest: PathBuf,
    pub symlink_tree_root: PathBuf,
    pub workspace_name: String,
}

impl SymlinkTreeHelper {
    pub fn new(
        input_manifest: PathBuf,
        output_manifest: PathBuf,
        symlink_tree_root: PathBuf,
        workspace_name: impl Into<String>,
    ) -> Self {
        Self {
            input_manifest,
            output_manifest,
            symlink_tree_root,
            workspace_name: workspace_name.into(),
        }
    }

    pub fn create_fileset_symlinks(
        &self,
        symlink_map: &BTreeMap<PathFragment, PathFragment>,
    ) -> io::Result<()> {
        let converted: BTreeMap<PathFragment, Option<PathFragment>> = symlink_map
            .iter()
            .map(|(k, v)| (k.clone(), Some(v.clone())))
            .collect();
        sync_symlink_tree(&self.symlink_tree_root, &converted)?;
        self.create_workspace_subdirectory()
    }

    pub fn create_runfiles_symlinks(
        &self,
        symlinks: &BTreeMap<PathFragment, Option<PathFragment>>,
    ) -> io::Result<()> {
        sync_symlink_tree(&self.symlink_tree_root, symlinks)?;
        self.create_workspace_subdirectory()
    }

    pub fn create_minimal_runfiles_directory(&self) -> io::Result<()> {
        delete_trees_below(&self.symlink_tree_root)?;
        self.link_manifest()?;
        self.create_workspace_subdirectory()
    }

    pub fn link_manifest(&self) -> io::Result<()> {
        link_manifest(&self.input_manifest, &self.output_manifest)
    }

    pub fn create_workspace_subdirectory(&self) -> io::Result<()> {
        std::fs::create_dir_all(self.symlink_tree_root.join(&self.workspace_name))
    }
}

fn escape_root_relative_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            ' ' => out.push_str("\\s"),
            '\n' => out.push_str("\\n"),
            '\\' => out.push_str("\\b"),
            c => out.push(c),
        }
    }
    out
}

fn escape_target_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\\' => out.push_str("\\b"),
            c => out.push(c),
        }
    }
    out
}

/// Formats `SOURCE_SYMLINKS` manifest text matching `SourceManifestAction.java`, including
/// space/newline/backslash escaping and ISO-8859-1 byte encoding.
pub fn encode_source_manifest_iso_8859_1(entries: &BTreeMap<String, String>) -> Vec<u8> {
    let mut out = Vec::new();
    for (k, v) in entries {
        let needs_escaping = k.contains(' ') || k.contains('\n') || v.contains('\n');
        let line = if needs_escaping {
            format!(
                " {} {}\n",
                escape_root_relative_path(k),
                escape_target_path(v)
            )
        } else {
            format!("{k} {v}\n")
        };
        for ch in line.chars() {
            let code = ch as u32;
            out.push(if code <= 0xFF { code as u8 } else { b'?' });
        }
    }
    out
}

/// Resolves runfiles manifest entries `(runfiles-relative path -> source exec path or "")`
/// using a caller-provided callback `read_unresolved_target(abs_path, is_declared_unresolved)`
/// that returns `Some(raw_symlink_target)` when the entry should preserve its raw symlink target
/// (declared unresolved symlinks or dangling symlinks) rather than absolutizing to `exec_root`.
pub fn resolve_manifest_entries_with<F>(
    exec_root: &Path,
    entries: &BTreeMap<String, String>,
    unresolved_symlink_paths: &BTreeSet<String>,
    workspace_name: &str,
    legacy_external_runfiles: bool,
    mut read_unresolved_target: F,
) -> BTreeMap<String, String>
where
    F: FnMut(&Path, bool) -> Option<String>,
{
    let mut all: BTreeMap<String, String> = entries.clone();
    // Legacy external runfiles: also expose external repos under `<workspace>/external/<repo>`.
    if legacy_external_runfiles && !workspace_name.is_empty() {
        for (k, v) in entries {
            if !k.starts_with(&format!("{workspace_name}/")) && !k.starts_with('_') {
                if let Some((repo, rest)) = k.split_once('/') {
                    all.entry(format!("{workspace_name}/external/{repo}/{rest}"))
                        .or_insert_with(|| v.clone());
                }
            }
        }
    }
    all.into_iter()
        .map(|(k, v)| {
            let resolved = if v.is_empty() {
                String::new()
            } else {
                let is_abs = Path::new(&v).is_absolute() || v.starts_with('/');
                let abs_path = if is_abs {
                    PathBuf::from(&v)
                } else {
                    exec_root.join(&v)
                };
                if let Some(raw_target) =
                    read_unresolved_target(&abs_path, unresolved_symlink_paths.contains(&v))
                {
                    raw_target
                } else if is_abs {
                    v
                } else {
                    abs_path.to_string_lossy().into_owned()
                }
            };
            (k, resolved)
        })
        .collect()
}

/// Resolves runfiles manifest entries against the host filesystem at `exec_root`.
pub fn resolve_manifest_entries(
    exec_root: &Path,
    entries: &BTreeMap<String, String>,
    unresolved_symlink_paths: &BTreeSet<String>,
    workspace_name: &str,
    legacy_external_runfiles: bool,
) -> BTreeMap<String, String> {
    resolve_manifest_entries_with(
        exec_root,
        entries,
        unresolved_symlink_paths,
        workspace_name,
        legacy_external_runfiles,
        |abs_path, force_unresolved| {
            // Unresolved symlinks (`ctx.actions.declare_symlink` + `target_path`) replicate their
            // raw `read_link` target directly into the runfiles tree and manifest (matching Bazel's
            // `SymlinkTreeHelper` / `SourceManifestAction`), because relative targets like
            // `rules_python`'s venv `bin/python3 -> ../../../../../...` only resolve inside
            // `.runfiles`. Also replicate dangling symlinks when `unresolved_symlink_paths` was
            // not populated.
            let is_unresolved = force_unresolved
                || (std::fs::symlink_metadata(abs_path)
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(false)
                    && std::fs::metadata(abs_path).is_err());
            if is_unresolved {
                std::fs::read_link(abs_path)
                    .ok()
                    .map(|t| t.to_string_lossy().into_owned())
            } else {
                None
            }
        },
    )
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
    let input_manifest = PathBuf::from(&manifest_path);

    let resolved_entries = resolve_manifest_entries(
        exec_root,
        entries,
        &opts.unresolved_symlink_paths,
        workspace_name,
        opts.legacy_external_runfiles,
    );
    let manifest_bytes = encode_source_manifest_iso_8859_1(&resolved_entries);
    let manifest = String::from_utf8_lossy(&manifest_bytes).into_owned();

    let needs_write = match std::fs::read(&input_manifest) {
        Ok(existing) => existing != manifest_bytes,
        Err(_) => true,
    };
    if needs_write {
        if let Some(p) = input_manifest.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&input_manifest, &manifest_bytes)?;
    }
    let out_manifest = dir.join("MANIFEST");
    let helper = SymlinkTreeHelper::new(
        input_manifest.clone(),
        out_manifest,
        dir.clone(),
        workspace_name,
    );
    if !opts.enable_runfiles {
        // Minimal runfiles directory (`SymlinkTreeHelper.createMinimalRunfilesDirectory`):
        // keep the `.runfiles` root, delete trees below it, symlink `MANIFEST` -> input_manifest,
        // and create the workspace subdirectory.
        helper.create_minimal_runfiles_directory()?;
        return Ok(manifest);
    }

    let mut symlink_map: BTreeMap<PathFragment, Option<PathFragment>> = BTreeMap::new();
    for (k, v) in &resolved_entries {
        if k == "MANIFEST" {
            continue;
        }
        let target = if v.is_empty() {
            None
        } else {
            Some(PathFragment::create(v))
        };
        symlink_map.insert(PathFragment::create(k), target);
    }

    helper.create_runfiles_symlinks(&symlink_map)?;
    helper.link_manifest()?;
    if !symlink_map.contains_key(&PathFragment::create("_repo_mapping"))
        && !opts.repo_mapping.is_empty()
    {
        let mut rm = String::new();
        for (src, app, canon) in &opts.repo_mapping {
            rm.push_str(&format!("{src},{app},{canon}\n"));
        }
        std::fs::write(dir.join("_repo_mapping"), rm)?;
    }
    Ok(manifest)
}

/// Entry kind passed to [`compute_runfiles_artifact_map`] (`SymlinkTreeStrategy.java`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunfilesEntryKind {
    /// Regular artifact or unresolved symlink target.
    Artifact(PathFragment),
    /// Nested `.runfiles` tree artifact (`Artifact.isRunfilesTree()`): dropped by `SymlinkTreeStrategy`.
    NestedRunfilesTree(PathFragment),
    /// Empty placeholder file (`empty_filenames`).
    EmptyFile,
}

/// Computes the filtered runfiles symlink map per `SymlinkTreeStrategy.computeRunfilesArtifactMap`:
/// - rejects empty and absolute runfiles keys,
/// - drops nested `.runfiles` tree artifacts,
/// - drops entries obscured by a non-null ancestor directory/symlink entry,
/// - lets `empty_filenames` overwrite an existing entry,
/// - adds `<workspace_name>/.runfile -> None` when no `<workspace_name>/...` entry was seen,
/// - includes `_repo_mapping` only when `repo_mapping_manifest` is `Some`.
pub fn compute_runfiles_artifact_map(
    workspace_name: &str,
    entries: &[(PathFragment, RunfilesEntryKind)],
    empty_filenames: &[PathFragment],
    repo_mapping_manifest: Option<&PathFragment>,
) -> io::Result<BTreeMap<PathFragment, Option<PathFragment>>> {
    let mut non_empty: BTreeMap<PathFragment, PathFragment> = BTreeMap::new();
    for (key, kind) in entries {
        if key.is_empty() || key.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Invalid runfiles path: '{key}'"),
            ));
        }
        match kind {
            RunfilesEntryKind::NestedRunfilesTree(_) => continue,
            RunfilesEntryKind::Artifact(target) => {
                non_empty.insert(key.clone(), target.clone());
            }
            RunfilesEntryKind::EmptyFile => {}
        }
    }

    // Drop entries obscured by a strict non-null ancestor (`a` obscures `a/b`).
    let mut map: BTreeMap<PathFragment, Option<PathFragment>> = BTreeMap::new();
    for (key, target) in &non_empty {
        let mut obscured = false;
        let mut cur = key.parent_directory();
        while let Some(anc) = cur {
            if anc.is_empty() {
                break;
            }
            if non_empty.contains_key(&anc) {
                obscured = true;
                break;
            }
            cur = anc.parent_directory();
        }
        if !obscured {
            map.insert(key.clone(), Some(target.clone()));
        }
    }

    for (_, kind) in entries {
        if matches!(kind, RunfilesEntryKind::EmptyFile) {
            // Handled below alongside `empty_filenames` in entry order.
        }
    }
    for (key, kind) in entries {
        if matches!(kind, RunfilesEntryKind::EmptyFile) {
            map.insert(key.clone(), None);
        }
    }
    for empty in empty_filenames {
        if empty.is_empty() || empty.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Invalid empty runfiles path: '{empty}'"),
            ));
        }
        let ws_prefixed = PathFragment::create(workspace_name).get_relative(empty);
        map.insert(ws_prefixed, None);
    }

    let ws_prefix = format!("{workspace_name}/");
    let has_ws_entry = map
        .keys()
        .any(|k| k.path_string() == workspace_name || k.path_string().starts_with(&ws_prefix));
    if !has_ws_entry && !workspace_name.is_empty() {
        map.insert(
            PathFragment::create(&format!("{workspace_name}/.runfile")),
            None,
        );
    }

    if let Some(rm) = repo_mapping_manifest {
        map.insert(PathFragment::create("_repo_mapping"), Some(rm.clone()));
    }

    Ok(map)
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
        let m = build_runfiles_tree(
            &root,
            &exe,
            "_main",
            &entries,
            &RunfilesTreeOptions::default(),
        )
        .unwrap();
        let rf = PathBuf::from(format!("{}.runfiles", exe.display()));
        assert_eq!(
            std::fs::read_to_string(rf.join("_main/pkg/data.txt")).unwrap(),
            "d"
        );
        #[cfg(unix)]
        assert!(std::fs::symlink_metadata(rf.join("_main/pkg/data.txt"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read(rf.join("_main/pkg/__init__.py")).unwrap(),
            b""
        );
        assert!(!rf.join("_repo_mapping").exists());
        assert_eq!(std::fs::read_to_string(rf.join("MANIFEST")).unwrap(), m);
        // Output MANIFEST is a symlink to `<exe>.runfiles_manifest`.
        #[cfg(unix)]
        {
            assert!(std::fs::symlink_metadata(rf.join("MANIFEST"))
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(
                std::fs::read_link(rf.join("MANIFEST")).unwrap(),
                PathBuf::from(format!("{}.runfiles_manifest", exe.display()))
            );
        }
        assert!(m.contains(&format!(
            "_main/pkg/data.txt {}",
            root.join("pkg/data.txt").display()
        )));
        assert!(std::path::Path::new(&format!("{}.runfiles_manifest", exe.display())).exists());
        // Stale entry removed on rebuild, and MANIFEST remains a symlink even when input_manifest exists.
        std::fs::write(rf.join("_main/stale"), "x").unwrap();
        entries.remove("ext+/lib/x.txt");
        build_runfiles_tree(
            &root,
            &exe,
            "_main",
            &entries,
            &RunfilesTreeOptions::default(),
        )
        .unwrap();
        assert!(!rf.join("_main/stale").exists());
        assert!(!rf.join("ext+/lib/x.txt").exists());
        #[cfg(unix)]
        assert!(std::fs::symlink_metadata(rf.join("MANIFEST"))
            .unwrap()
            .file_type()
            .is_symlink());
        // Manifest-only mode creates minimal runfiles directory (workspace subdir + MANIFEST symlink).
        let o = RunfilesTreeOptions {
            enable_runfiles: false,
            ..Default::default()
        };
        build_runfiles_tree(&root, &exe, "_main", &entries, &o).unwrap();
        assert!(rf.exists());
        assert!(rf.join("_main").is_dir());
        assert!(rf.join("MANIFEST").exists());
        assert!(!rf.join("_main/pkg/data.txt").exists());
    }

    #[test]
    fn manifest_escaping_and_symlink_tree_helper_repairs() {
        let mut entries = BTreeMap::new();
        entries.insert("a b\\c".into(), "/src/x y".into());
        entries.insert("nl".into(), "/src/line\nbreak".into());
        let encoded = encode_source_manifest_iso_8859_1(&entries);
        let text = String::from_utf8(encoded).unwrap();
        assert!(text.contains(" a\\sb\\bc /src/x y\n"), "{text:?}");
        assert!(text.contains(" nl /src/line\\nbreak\n"), "{text:?}");

        let root = std::env::temp_dir().join(format!("zel-sth-{}", std::process::id()));
        let _ = delete_tree(&root);
        let rf = root.join("rf");
        std::fs::create_dir_all(&rf).unwrap();
        let helper = SymlinkTreeHelper::new(
            root.join("rf_manifest"),
            rf.join("MANIFEST"),
            rf.clone(),
            "_main",
        );

        // Rejects empty and absolute keys.
        let mut bad = BTreeMap::new();
        bad.insert(PathFragment::create("/abs"), None);
        assert!(helper.create_runfiles_symlinks(&bad).is_err());

        // Directory replaces symlink when both are present at the same prefix, and existing
        // regular file for empty placeholder is left untruncated.
        std::fs::write(rf.join("keep_empty"), b"existing-bytes").unwrap();
        std::fs::create_dir_all(rf.join("was_dir")).unwrap();
        std::fs::write(rf.join("was_dir/inner"), b"1").unwrap();
        let mut map = BTreeMap::new();
        map.insert(PathFragment::create("keep_empty"), None);
        map.insert(
            PathFragment::create("was_dir"),
            Some(PathFragment::create("/t")),
        );
        map.insert(
            PathFragment::create("pref"),
            Some(PathFragment::create("/link_target")),
        );
        map.insert(
            PathFragment::create("pref/sub"),
            Some(PathFragment::create("/sub_target")),
        );
        helper.create_runfiles_symlinks(&map).unwrap();
        assert_eq!(
            std::fs::read(rf.join("keep_empty")).unwrap(),
            b"existing-bytes"
        );
        #[cfg(unix)]
        {
            assert!(std::fs::symlink_metadata(rf.join("was_dir"))
                .unwrap()
                .file_type()
                .is_symlink());
            assert!(std::fs::symlink_metadata(rf.join("pref/sub"))
                .unwrap()
                .file_type()
                .is_symlink());
        }
        assert!(std::fs::symlink_metadata(rf.join("pref")).unwrap().is_dir());
    }

    #[test]
    fn runfiles_tree_updater_deduplicates_permanently_and_honors_build_runfile_links() {
        let root = std::env::temp_dir().join(format!("zel-rtu-{}", std::process::id()));
        let _ = delete_tree(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("src.txt"), "hi").unwrap();

        let updater = RunfilesTreeUpdater::new();
        let mut mapping = BTreeMap::new();
        mapping.insert(
            PathFragment::create("_main/src.txt"),
            Some(RunfilesTarget::File {
                exec_path: PathFragment::create("src.txt"),
                is_symlink: false,
            }),
        );

        // build_runfile_links = true returns immediately without creating anything.
        let skip_tree = RunfilesTreeData {
            exec_path: PathFragment::create("skip.runfiles"),
            workspace_name: "_main".into(),
            mapping: mapping.clone(),
            build_runfile_links: true,
            symlinks_mode: RunfilesSymlinksMode::Create,
        };
        updater.update_runfiles(&root, &[skip_tree]).unwrap();
        assert!(!root.join("skip.runfiles").exists());

        // Missing input manifest is a permanent no-op for the updater.
        let missing_manifest_tree = RunfilesTreeData {
            exec_path: PathFragment::create("t.runfiles"),
            workspace_name: "_main".into(),
            mapping: mapping.clone(),
            build_runfile_links: false,
            symlinks_mode: RunfilesSymlinksMode::Create,
        };
        updater
            .update_runfiles(&root, std::slice::from_ref(&missing_manifest_tree))
            .unwrap();
        assert!(!root.join("t.runfiles/MANIFEST").exists());
        // Even if input manifest is created later, the same updater does not retry.
        std::fs::write(root.join("t.runfiles_manifest"), "_main/src.txt src.txt\n").unwrap();
        updater
            .update_runfiles(&root, &[missing_manifest_tree])
            .unwrap();
        assert!(!root.join("t.runfiles/MANIFEST").exists());

        // A fresh updater materializes the tree and symlinks MANIFEST -> t.runfiles_manifest.
        let updater2 = RunfilesTreeUpdater::new();
        let tree2 = RunfilesTreeData {
            exec_path: PathFragment::create("t.runfiles"),
            workspace_name: "_main".into(),
            mapping,
            build_runfile_links: false,
            symlinks_mode: RunfilesSymlinksMode::Create,
        };
        updater2.update_runfiles(&root, &[tree2]).unwrap();
        #[cfg(unix)]
        assert!(std::fs::symlink_metadata(root.join("t.runfiles/MANIFEST"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_to_string(root.join("t.runfiles/_main/src.txt")).unwrap(),
            "hi"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unresolved_relative_symlinks_preserved_in_runfiles_tree_and_manifest() {
        let root = std::env::temp_dir().join(format!("zel-rf-unres-{}", std::process::id()));
        let _ = delete_tree(&root);
        let exec_root = root.join("execroot/_main");
        std::fs::create_dir_all(exec_root.join("external/ext_py/bin")).unwrap();
        std::fs::write(
            exec_root.join("external/ext_py/bin/python3"),
            "#!/bin/sh\necho VENV_OK\n",
        )
        .unwrap();
        std::fs::create_dir_all(exec_root.join("pkg")).unwrap();
        std::fs::write(exec_root.join("pkg/real.txt"), "real").unwrap();
        // Normal source symlink (resolves on disk, NOT an unresolved symlink artifact).
        std::os::unix::fs::symlink("real.txt", exec_root.join("pkg/src_sym.txt")).unwrap();

        // Derived unresolved relative symlink whose target is dangling in `bazel-out` and only
        // resolves when placed inside `bundler.runfiles/rules_pylyglot+/pylyglot/private/_bundler.venv/bin/`.
        let venv_bin = exec_root.join(
            "bazel-out/k8-opt-exec/bin/external/rules_pylyglot+/pylyglot/private/_bundler.venv/bin",
        );
        std::fs::create_dir_all(&venv_bin).unwrap();
        let rel_target = "../../../../../ext_py/bin/python3";
        std::os::unix::fs::symlink(rel_target, venv_bin.join("python3")).unwrap();
        assert!(
            std::fs::metadata(venv_bin.join("python3")).is_err(),
            "symlink should be dangling from bazel-out"
        );

        // Unresolved relative symlink whose target also happens to resolve locally in bazel-out.
        let out_pkg = exec_root.join("bazel-out/k8-opt-exec/bin/pkg");
        std::fs::create_dir_all(out_pkg.join("sub")).unwrap();
        std::fs::write(out_pkg.join("sibling.txt"), "local-sib").unwrap();
        std::os::unix::fs::symlink("../sibling.txt", out_pkg.join("sub/unres_existing")).unwrap();

        // Unresolved absolute symlink.
        std::os::unix::fs::symlink("/etc/hosts", out_pkg.join("sub/unres_abs")).unwrap();

        let exe = exec_root
            .join("bazel-out/k8-opt-exec/bin/external/rules_pylyglot+/pylyglot/private/bundler");
        let mut entries = BTreeMap::new();
        let venv_exec = "bazel-out/k8-opt-exec/bin/external/rules_pylyglot+/pylyglot/private/_bundler.venv/bin/python3";
        entries.insert(
            "rules_pylyglot+/pylyglot/private/_bundler.venv/bin/python3".to_string(),
            venv_exec.to_string(),
        );
        entries.insert(
            "ext_py/bin/python3".to_string(),
            "external/ext_py/bin/python3".to_string(),
        );
        entries.insert("_main/pkg/real.txt".to_string(), "pkg/real.txt".to_string());
        entries.insert(
            "_main/pkg/src_sym.txt".to_string(),
            "pkg/src_sym.txt".to_string(),
        );
        entries.insert(
            "_main/pkg/sibling.txt".to_string(),
            "bazel-out/k8-opt-exec/bin/pkg/sibling.txt".to_string(),
        );
        entries.insert(
            "_main/pkg/sub/unres_existing".to_string(),
            "bazel-out/k8-opt-exec/bin/pkg/sub/unres_existing".to_string(),
        );
        entries.insert(
            "_main/pkg/sub/unres_abs".to_string(),
            "bazel-out/k8-opt-exec/bin/pkg/sub/unres_abs".to_string(),
        );

        let mut opts = RunfilesTreeOptions::default();
        opts.unresolved_symlink_paths.insert(venv_exec.to_string());
        opts.unresolved_symlink_paths
            .insert("bazel-out/k8-opt-exec/bin/pkg/sub/unres_existing".to_string());
        opts.unresolved_symlink_paths
            .insert("bazel-out/k8-opt-exec/bin/pkg/sub/unres_abs".to_string());

        let manifest = build_runfiles_tree(&exec_root, &exe, "_main", &entries, &opts).unwrap();
        let rf = PathBuf::from(format!("{}.runfiles", exe.display()));

        // 1. Dangling relative symlink is replicated verbatim and now resolves inside `.runfiles`.
        let rf_py = rf.join("rules_pylyglot+/pylyglot/private/_bundler.venv/bin/python3");
        assert_eq!(
            std::fs::read_link(&rf_py).unwrap(),
            PathBuf::from(rel_target)
        );
        assert_eq!(
            std::fs::read_to_string(&rf_py).unwrap(),
            "#!/bin/sh\necho VENV_OK\n"
        );
        assert!(manifest.contains(&format!(
            "rules_pylyglot+/pylyglot/private/_bundler.venv/bin/python3 {rel_target}\n"
        )));

        // 2. Non-dangling source symlink points to its execroot path (not read_link'd).
        assert_eq!(
            std::fs::read_link(rf.join("_main/pkg/src_sym.txt")).unwrap(),
            exec_root.join("pkg/src_sym.txt")
        );

        // 3. Declared unresolved symlinks (existing relative & absolute) replicate raw targets.
        assert_eq!(
            std::fs::read_link(rf.join("_main/pkg/sub/unres_existing")).unwrap(),
            PathBuf::from("../sibling.txt")
        );
        assert_eq!(
            std::fs::read_link(rf.join("_main/pkg/sub/unres_abs")).unwrap(),
            PathBuf::from("/etc/hosts")
        );

        // 4. Even without `unresolved_symlink_paths`, a dangling relative symlink is still preserved.
        let exe2 = exec_root
            .join("bazel-out/k8-opt-exec/bin/external/rules_pylyglot+/pylyglot/private/bundler2");
        build_runfiles_tree(
            &exec_root,
            &exe2,
            "_main",
            &entries,
            &RunfilesTreeOptions::default(),
        )
        .unwrap();
        let rf2_py = PathBuf::from(format!("{}.runfiles", exe2.display()))
            .join("rules_pylyglot+/pylyglot/private/_bundler.venv/bin/python3");
        assert_eq!(
            std::fs::read_link(&rf2_py).unwrap(),
            PathBuf::from(rel_target)
        );
        assert_eq!(
            std::fs::read_to_string(&rf2_py).unwrap(),
            "#!/bin/sh\necho VENV_OK\n"
        );

        // 5. Existing `.runfiles_manifest` containing stale `bazel-out` paths is rewritten with
        //    the resolved raw targets for both `enable_runfiles: true` and `enable_runfiles: false`.
        let exe3 = exec_root
            .join("bazel-out/k8-opt-exec/bin/external/rules_pylyglot+/pylyglot/private/bundler3");
        let stale_manifest_path = PathBuf::from(format!("{}.runfiles_manifest", exe3.display()));
        std::fs::write(
            &stale_manifest_path,
            format!("rules_pylyglot+/pylyglot/private/_bundler.venv/bin/python3 {venv_exec}\n"),
        )
        .unwrap();
        build_runfiles_tree(
            &exec_root,
            &exe3,
            "_main",
            &entries,
            &RunfilesTreeOptions {
                enable_runfiles: false,
                unresolved_symlink_paths: opts.unresolved_symlink_paths.clone(),
                ..Default::default()
            },
        )
        .unwrap();
        let on_disk_mf = std::fs::read_to_string(&stale_manifest_path).unwrap();
        let in_tree_mf = std::fs::read_to_string(PathBuf::from(format!(
            "{}.runfiles/MANIFEST",
            exe3.display()
        )))
        .unwrap();
        assert_eq!(on_disk_mf, in_tree_mf);
        assert!(
            on_disk_mf.contains(&format!(
                "rules_pylyglot+/pylyglot/private/_bundler.venv/bin/python3 {rel_target}\n"
            )),
            "expected raw symlink target in manifest:\n{on_disk_mf}"
        );
        assert!(
            !on_disk_mf.contains(venv_exec),
            "stale bazel-out path should not remain in manifest:\n{on_disk_mf}"
        );

        let _ = delete_tree(&root);
    }
}
