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
        Some(match v {
            "normal" | "true" | "1" => Self::Normal,
            "clean" => Self::Clean,
            "ignore" | "false" | "0" => Self::Ignore,
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
    if prefix == "/" {
        return Vec::new();
    }
    // A prefix with a slash is relative to the workspace (or absolute).
    let base = |name: &str| -> String {
        let full = format!("{prefix}{name}");
        if full.starts_with('/') {
            full
        } else {
            format!("{}/{}", req.workspace_dir, full)
        }
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

/// Creates, refreshes or removes the convenience symlinks. Returns console messages
/// (`WARNING: ...` / `INFO: ...`) in Bazel's wording.
pub fn create_convenience_symlinks(req: &SymlinkRequest) -> Vec<String> {
    let mut msgs = Vec::new();
    let links = convenience_links(req);
    match req.mode {
        ConvenienceSymlinksMode::Ignore => return msgs,
        ConvenienceSymlinksMode::LogOnly => {
            for (l, t) in &links {
                msgs.push(format!("INFO: Convenience symlink {l} -> {t} (not created)"));
            }
            return msgs;
        }
        _ => {}
    }
    for (link, target) in &links {
        let lp = Path::new(link);
        let existing = std::fs::symlink_metadata(lp);
        if req.mode == ConvenienceSymlinksMode::Clean {
            if matches!(&existing, Ok(m) if m.file_type().is_symlink()) {
                let _ = std::fs::remove_file(lp);
            }
            continue;
        }
        if let Ok(m) = &existing {
            if m.file_type().is_symlink() {
                if std::fs::read_link(lp).map(|t| t == Path::new(target)).unwrap_or(false) {
                    continue;
                }
                let _ = std::fs::remove_file(lp);
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
        #[cfg(unix)]
        if let Err(e) = std::os::unix::fs::symlink(target, lp) {
            msgs.push(format!("WARNING: Failed to create convenience symlink '{link}': {e}"));
        }
    }
    msgs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req<'a>(ws: &'a str, prefix: Option<&'a str>, mode: ConvenienceSymlinksMode) -> SymlinkRequest<'a> {
        SymlinkRequest {
            workspace_dir: ws,
            execroot: "/tmp/ob/execroot/_main",
            bin_dir: "/tmp/ob/execroot/_main/bazel-out/k8-fastbuild/bin",
            testlogs_dir: "/tmp/ob/execroot/_main/bazel-out/k8-fastbuild/testlogs",
            genfiles_dir: "/tmp/ob/execroot/_main/bazel-out/k8-fastbuild/bin",
            symlink_prefix: prefix,
            mode,
            product_name: "bazel",
            skip_genfiles: true,
        }
    }

    #[test]
    fn creates_and_cleans_links_and_honors_prefix() {
        let ws = std::env::temp_dir().join(format!("zel-symlinks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(&ws).unwrap();
        let w = ws.to_str().unwrap();
        let msgs = create_convenience_symlinks(&req(w, None, ConvenienceSymlinksMode::Normal));
        assert!(msgs.is_empty(), "{msgs:?}");
        assert!(std::fs::read_link(ws.join("bazel-bin")).unwrap().ends_with("k8-fastbuild/bin"));
        assert!(std::fs::read_link(ws.join("bazel-out")).unwrap().ends_with("_main/bazel-out"));
        assert!(std::fs::symlink_metadata(ws.join(format!("bazel-{}", ws.file_name().unwrap().to_str().unwrap()))).is_ok());
        create_convenience_symlinks(&req(w, None, ConvenienceSymlinksMode::Clean));
        assert!(std::fs::symlink_metadata(ws.join("bazel-bin")).is_err());
        create_convenience_symlinks(&req(w, Some("zz-"), ConvenienceSymlinksMode::Normal));
        assert!(std::fs::symlink_metadata(ws.join("zz-testlogs")).is_ok());
        assert!(convenience_links(&req(w, Some("/"), ConvenienceSymlinksMode::Normal)).is_empty());
        // Real directory in the way => warning, not clobbered.
        std::fs::create_dir(ws.join("bazel-bin")).unwrap();
        let m = create_convenience_symlinks(&req(w, None, ConvenienceSymlinksMode::Normal));
        assert!(m.iter().any(|x| x.contains("bazel-bin") && x.starts_with("WARNING")));
        let _ = std::fs::remove_dir_all(&ws);
    }
}
