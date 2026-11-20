//! Convenience symlinks (`bazel-bin`, `bazel-out`, `bazel-testlogs`, `bazel-<workspace>`),
//! mirroring `buildtool/OutputDirectoryLinksUtils.java` and `--symlink_prefix` /
//! `--experimental_convenience_symlinks`.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConvenienceSymlinksMode {
    /// Create/refresh symlinks.
    #[default]
    Normal,
    /// Delete any symlinks previously created.
    Clean,
    /// Do nothing.
    Ignore,
    /// Do not touch the file system, only report which links would exist.
    LogOnly,
}

impl ConvenienceSymlinksMode {
    pub fn parse(v: &str) -> Option<Self> {
        Some(match v.to_ascii_lowercase().as_str() {
            "normal" | "true" | "1" | "yes" | "y" | "t" => Self::Normal,
            "clean" => Self::Clean,
            "ignore" | "false" | "0" | "no" | "n" | "f" => Self::Ignore,
            "log_only" => Self::LogOnly,
            _ => return None,
        })
    }
}

pub struct SymlinkRequest<'a> {
    pub workspace_dir: &'a str,
    pub execroot: &'a str,
    pub bin_dir: &'a str,
    pub testlogs_dir: &'a str,
    pub genfiles_dir: &'a str,
    /// `--symlink_prefix`; `None` means `bazel-`. A prefix of `/` disables all links.
    pub symlink_prefix: Option<&'a str>,
    pub mode: ConvenienceSymlinksMode,
    pub product_name: &'a str,
    pub skip_genfiles: bool,
}

/// Returns the (link path, target) pairs for a request, honoring `--symlink_prefix`.
pub fn convenience_links(req: &SymlinkRequest) -> Vec<(String, String)> {
    let prefix = req
        .symlink_prefix
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{}-", req.product_name));
    if prefix.starts_with('/') || prefix.split('/').any(|seg| seg == "..") {
        return Vec::new();
    }
    let base = |name: &str| -> String {
        let full = format!("{prefix}{name}");
        format!("{}/{}", req.workspace_dir, full)
    };
    let ws_name = Path::new(req.workspace_dir)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "workspace".to_string());
    let out_dir = Path::new(req.execroot)
        .join("bazel-out")
        .to_string_lossy()
        .into_owned();
    let mut links = vec![
        (base(&ws_name), req.execroot.to_string()),
        (base("out"), out_dir),
        (base("bin"), req.bin_dir.to_string()),
        (base("testlogs"), req.testlogs_dir.to_string()),
    ];
    if !req.skip_genfiles && !req.genfiles_dir.is_empty() {
        links.push((base("genfiles"), req.genfiles_dir.to_string()));
    }
    links
}

/// Returns `(workspace_relative_path, target, create)` entries for BEP
/// `ConvenienceSymlinksIdentified`.
pub fn bep_convenience_symlinks(req: &SymlinkRequest) -> Vec<(String, String, bool)> {
    if req.mode == ConvenienceSymlinksMode::Ignore {
        return Vec::new();
    }
    let create = req.mode != ConvenienceSymlinksMode::Clean;
    let ws_prefix = format!("{}/", req.workspace_dir.trim_end_matches('/'));
    convenience_links(req)
        .into_iter()
        .map(|(link, target)| {
            let rel = link
                .strip_prefix(&ws_prefix)
                .unwrap_or(link.as_str())
                .to_string();
            (rel, target, create)
        })
        .collect()
}

/// Creates, refreshes or removes the convenience symlinks. Returns console messages
/// (`WARNING: ...` / `INFO: ...`) in Bazel's wording.
pub fn create_convenience_symlinks(req: &SymlinkRequest) -> Vec<String> {
    let mut msgs = Vec::new();
    let links = convenience_links(req);
    match req.mode {
        ConvenienceSymlinksMode::Ignore => return msgs,
        ConvenienceSymlinksMode::LogOnly => {
            for (l, t) in &links {
                msgs.push(format!(
                    "INFO: Convenience symlink {l} -> {t} (not created)"
                ));
            }
            return msgs;
        }
        _ => {}
    }
    for (link, target) in &links {
        let lp = Path::new(link);
        if req.mode == ConvenienceSymlinksMode::Clean {
            if zel_vfs::is_junction_or_symlink(lp) {
                let _ = zel_vfs::remove_junction_or_symlink(lp);
            }
            continue;
        }
        let _ = std::fs::create_dir_all(target);
        if std::fs::symlink_metadata(lp).is_ok() {
            if zel_vfs::is_junction_or_symlink(lp) {
                if zel_vfs::junction_or_symlink_points_to(lp, Path::new(target)) {
                    continue;
                }
                let _ = zel_vfs::remove_junction_or_symlink(lp);
            } else {
                msgs.push(format!(
                    "WARNING: Failed to create convenience symlink '{link}': it exists and is not a symlink"
                ));
                continue;
            }
        }
        if let Some(parent) = lp.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = zel_vfs::create_directory_junction_or_symlink(Path::new(target), lp) {
            msgs.push(format!(
                "WARNING: Failed to create convenience symlink '{link}': {e}"
            ));
        }
    }
    msgs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_retargets_and_cleans_links_and_honors_prefix() {
        let root = std::env::temp_dir().join(format!("zel-symlinks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ws = root.join("ws");
        let execroot = root.join("ob/execroot/_main");
        let bin_fastbuild = execroot.join("bazel-out/k8-fastbuild/bin");
        let testlogs_fastbuild = execroot.join("bazel-out/k8-fastbuild/testlogs");
        let bin_opt = execroot.join("bazel-out/k8-opt/bin");
        let testlogs_opt = execroot.join("bazel-out/k8-opt/testlogs");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&bin_fastbuild).unwrap();
        std::fs::write(bin_fastbuild.join("artifact.txt"), b"fastbuild").unwrap();
        std::fs::create_dir_all(&bin_opt).unwrap();
        std::fs::write(bin_opt.join("artifact.txt"), b"opt").unwrap();

        let ws_s = ws.to_string_lossy().into_owned();
        let er_s = execroot.to_string_lossy().into_owned();
        let bin_fb_s = bin_fastbuild.to_string_lossy().into_owned();
        let tl_fb_s = testlogs_fastbuild.to_string_lossy().into_owned();
        let bin_opt_s = bin_opt.to_string_lossy().into_owned();
        let tl_opt_s = testlogs_opt.to_string_lossy().into_owned();

        fn make_req<'a>(
            ws: &'a str,
            er: &'a str,
            prefix: Option<&'a str>,
            mode: ConvenienceSymlinksMode,
            bin: &'a str,
            tl: &'a str,
        ) -> SymlinkRequest<'a> {
            SymlinkRequest {
                workspace_dir: ws,
                execroot: er,
                bin_dir: bin,
                testlogs_dir: tl,
                genfiles_dir: bin,
                symlink_prefix: prefix,
                mode,
                product_name: "bazel",
                skip_genfiles: true,
            }
        }

        let msgs = create_convenience_symlinks(&make_req(
            &ws_s,
            &er_s,
            None,
            ConvenienceSymlinksMode::Normal,
            &bin_fb_s,
            &tl_fb_s,
        ));
        assert!(msgs.is_empty(), "{msgs:?}");
        assert!(zel_vfs::is_junction_or_symlink(&ws.join("bazel-bin")));
        assert!(zel_vfs::junction_or_symlink_points_to(
            &ws.join("bazel-bin"),
            &bin_fastbuild
        ));
        assert!(zel_vfs::junction_or_symlink_points_to(
            &ws.join("bazel-out"),
            &execroot.join("bazel-out")
        ));
        assert!(zel_vfs::is_junction_or_symlink(&ws.join("bazel-ws")));

        // Repeat Normal with identical targets is a silent no-op.
        let msgs_repeat = create_convenience_symlinks(&make_req(
            &ws_s,
            &er_s,
            None,
            ConvenienceSymlinksMode::Normal,
            &bin_fb_s,
            &tl_fb_s,
        ));
        assert!(msgs_repeat.is_empty(), "{msgs_repeat:?}");

        // Retargeting `bazel-bin` / `bazel-testlogs` replaces existing links/junctions without
        // warning or touching the previous target directory's contents.
        let msgs_retarget = create_convenience_symlinks(&make_req(
            &ws_s,
            &er_s,
            None,
            ConvenienceSymlinksMode::Normal,
            &bin_opt_s,
            &tl_opt_s,
        ));
        assert!(msgs_retarget.is_empty(), "{msgs_retarget:?}");
        assert!(zel_vfs::junction_or_symlink_points_to(
            &ws.join("bazel-bin"),
            &bin_opt
        ));
        assert_eq!(
            std::fs::read(ws.join("bazel-bin/artifact.txt")).unwrap(),
            b"opt"
        );
        assert_eq!(
            std::fs::read(bin_fastbuild.join("artifact.txt")).unwrap(),
            b"fastbuild"
        );

        // Clean removes convenience links/junctions while preserving target contents.
        create_convenience_symlinks(&make_req(
            &ws_s,
            &er_s,
            None,
            ConvenienceSymlinksMode::Clean,
            &bin_opt_s,
            &tl_opt_s,
        ));
        assert!(std::fs::symlink_metadata(ws.join("bazel-bin")).is_err());
        assert!(std::fs::symlink_metadata(ws.join("bazel-out")).is_err());
        assert_eq!(std::fs::read(bin_opt.join("artifact.txt")).unwrap(), b"opt");

        create_convenience_symlinks(&make_req(
            &ws_s,
            &er_s,
            Some("zz-"),
            ConvenienceSymlinksMode::Normal,
            &bin_opt_s,
            &tl_opt_s,
        ));
        assert!(zel_vfs::is_junction_or_symlink(&ws.join("zz-testlogs")));
        assert!(convenience_links(&make_req(
            &ws_s,
            &er_s,
            Some("/"),
            ConvenienceSymlinksMode::Normal,
            &bin_opt_s,
            &tl_opt_s
        ))
        .is_empty());

        // Real directory in the way => warning, not clobbered.
        std::fs::create_dir(ws.join("bazel-bin")).unwrap();
        std::fs::write(ws.join("bazel-bin/real.txt"), b"keep").unwrap();
        let m = create_convenience_symlinks(&make_req(
            &ws_s,
            &er_s,
            None,
            ConvenienceSymlinksMode::Normal,
            &bin_opt_s,
            &tl_opt_s,
        ));
        assert!(m
            .iter()
            .any(|x| x.contains("bazel-bin") && x.starts_with("WARNING")));
        assert_eq!(
            std::fs::read(ws.join("bazel-bin/real.txt")).unwrap(),
            b"keep"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
