//! Materializes `<binary>.runfiles` trees with `SymlinkTreeHelper` and `RunfilesTreeUpdater`
//! semantics: symlinks for every manifest entry, empty files for empty-valued entries, stale
//! entries removed (with chmod retry), directory-after-symlink pass, `MANIFEST` symlinked to
//! `<binary>.runfiles_manifest`, ISO-8859-1 manifest escaping, and `_repo_mapping`.

use std::collections::BTreeMap;
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

    let abs = |v: &str| -> String {
        if v.is_empty() || Path::new(v).is_absolute() || v.starts_with('/') {
            v.to_string()
        } else {
            exec_root.join(v).to_string_lossy().into_owned()
        }
    };
    let mut all: BTreeMap<String, String> = entries.clone();
    // Legacy external runfiles: also expose external repos under `<workspace>/external/<repo>`.
    if opts.legacy_external_runfiles {
        for (k, v) in entries {
            if !k.starts_with(&format!("{workspace_name}/")) && !k.starts_with('_') {
                if let Some((repo, rest)) = k.split_once('/') {
                    all.entry(format!("{workspace_name}/external/{repo}/{rest}"))
                        .or_insert_with(|| v.clone());
                }
            }
        }
    }
    let resolved_entries: BTreeMap<String, String> =
        all.iter().map(|(k, v)| (k.clone(), abs(v))).collect();
    let manifest_bytes = encode_source_manifest_iso_8859_1(&resolved_entries);
    let manifest = String::from_utf8_lossy(&manifest_bytes).into_owned();

    let input_existed = std::fs::symlink_metadata(&input_manifest).is_ok();
    if !input_existed {
        if let Some(p) = input_manifest.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&input_manifest, &manifest_bytes)?;
    }
    if !opts.enable_runfiles {
        // Manifest-only mode: no symlink tree.
        let _ = delete_tree(&dir);
        return Ok(manifest);
    }

    let mut symlink_map: BTreeMap<PathFragment, Option<PathFragment>> = BTreeMap::new();
    for (k, v) in &resolved_entries {
        if k == "MANIFEST" || k == "_repo_mapping" {
            continue;
        }
        let target = if v.is_empty() {
            None
        } else {
            Some(PathFragment::create(v))
        };
        symlink_map.insert(PathFragment::create(k), target);
    }

    // Preserve `_repo_mapping` and `MANIFEST` across sync by updating them right after sync
    // (or let sync clean unknown files, then write `_repo_mapping` and symlink/write `MANIFEST`).
    let out_manifest = dir.join("MANIFEST");
    let helper = SymlinkTreeHelper::new(
        input_manifest,
        out_manifest.clone(),
        dir.clone(),
        workspace_name,
    );
    helper.create_runfiles_symlinks(&symlink_map)?;
    if input_existed {
        if std::fs::symlink_metadata(&out_manifest)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            let _ = std::fs::remove_file(&out_manifest);
        }
        std::fs::write(&out_manifest, &manifest_bytes)?;
    } else {
        helper.link_manifest()?;
    }
    let mut rm = String::new();
    for (src, app, canon) in &opts.repo_mapping {
        rm.push_str(&format!("{src},{app},{canon}\n"));
    }
    std::fs::write(dir.join("_repo_mapping"), rm)?;
    Ok(manifest)
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
        assert!(rf.join("_repo_mapping").exists());
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
        // Stale entry removed on rebuild.
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
        // Manifest-only mode removes the tree.
        let o = RunfilesTreeOptions {
            enable_runfiles: false,
            ..Default::default()
        };
        build_runfiles_tree(&root, &exe, "_main", &entries, &o).unwrap();
        assert!(!rf.exists());
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
}
