//! `VENDOR.bazel` parsing, repository pinning/ignoring, and symlink replanting.
//!
//! Mirrors `com.google.devtools.build.lib.bazel.bzlmod.VendorManager`,
//! `VendorFileFunction`, and `RepositoryUtils.replantSymlinks`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub const VENDOR_FILE_NAME: &str = "VENDOR.bazel";
pub const EXTERNAL_ROOT_SYMLINK_NAME: &str = "bazel-external";
pub const WORKSPACE_SYMLINK_NAME: &str = "_main";

pub const VENDOR_FILE_HEADER: &str = r#"###############################################################################
# This file is used to configure how external repositories are handled in vendor mode.
# ONLY the two following functions can be used:
#
# ignore('@@<canonical repo name>', ...) is used to completely ignore this repo from vendoring.
# Bazel will use the normal external cache and fetch process for this repo.
#
# pin('@@<canonical repo name>', ...) is used to pin the contents of this repo under the vendor
# directory as if there is a --override_repository flag for this repo.
# Note that Bazel will NOT update the vendored source for this repo while running vendor command
# unless it's unpinned. The user can modify and maintain the vendored source for this repo manually.
###############################################################################
"#;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VendorFile {
    pub ignored_repos: BTreeSet<String>,
    pub pinned_repos: BTreeSet<String>,
}

impl VendorFile {
    /// Loads `VENDOR.bazel` from `vendor_dir`.
    pub fn load(vendor_dir: &Path) -> Result<Self, String> {
        let path = vendor_dir.join(VENDOR_FILE_NAME);
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(&path).map_err(|e| format!("ERROR: error reading VENDOR.bazel file: {e}"))?;
        Self::parse_at(&content, &path.display().to_string())
    }

    /// Loads `VENDOR.bazel`, or initializes it with default header if missing.
    pub fn load_or_init(vendor_dir: &Path) -> Result<Self, String> {
        let path = vendor_dir.join(VENDOR_FILE_NAME);
        if !path.exists() {
            let _ = fs::create_dir_all(vendor_dir);
            let _ = fs::write(&path, VENDOR_FILE_HEADER);
            return Ok(Self::default());
        }
        Self::load(vendor_dir)
    }

    /// Parses the Starlark content of a `VENDOR.bazel` file (error paths shown as `VENDOR.bazel`).
    pub fn parse(content: &str) -> Result<Self, String> {
        Self::parse_at(content, VENDOR_FILE_NAME)
    }

    /// Evaluates a `VENDOR.bazel` file with the `ignore()` / `pin()` builtins (Bazel's `VendorFileFunction`).
    /// On error returns the complete stderr text, i.e. `ERROR: ...` lines.
    pub fn parse_at(content: &str, display_path: &str) -> Result<Self, String> {
        use std::sync::{Arc, Mutex};
        use zel_starlark::{EvalContext, StarlarkError, Value};
        let state: Arc<Mutex<(BTreeSet<String>, BTreeSet<String>)>> = Arc::new(Mutex::new((BTreeSet::new(), BTreeSet::new())));
        let st = state.clone();
        let mut ctx = EvalContext::default();
        ctx.file_name = display_path.to_string();
        ctx.strict_names = true;
        ctx.host = Some(Arc::new(move |_c, name, pos, kw| {
            if !kw.is_empty() {
                let k = kw.keys().next().cloned().unwrap_or_default();
                return Err(StarlarkError(format!("{name}() got unexpected keyword argument '{k}'")));
            }
            let mut repos = Vec::new();
            for (i, v) in pos.iter().enumerate() {
                match v {
                    Value::String(s) => {
                        let Some(rest) = s.strip_prefix("@@") else {
                            return Err(StarlarkError("the canonical repository name must start with `@@`".to_string()));
                        };
                        repos.push(rest.to_string());
                    }
                    other => {
                        return Err(StarlarkError(format!("at index {i} of args, got element of type {}, want string", other.type_name())));
                    }
                }
            }
            let mut g = st.lock().unwrap();
            for r in repos {
                if name == "ignore" {
                    g.0.insert(r);
                } else {
                    g.1.insert(r);
                }
            }
            Ok(Value::None)
        }));
        ctx.globals.insert("ignore".into(), Value::BuiltinFunction("host:ignore".into()));
        ctx.globals.insert("pin".into(), Value::BuiltinFunction("host:pin".into()));
        if let Err(errors) = ctx.eval_file_checked(content, Some(("VENDOR.bazel files", false))) {
            let syntax = errors.iter().any(|e| e.contains("syntax error at"));
            let mut out = String::new();
            if syntax {
                for e in &errors {
                    let line = e.trim_matches(|c| c == '\n' || c == '\r');
                    out.push_str(&if line.starts_with("ERROR: ") { format!("{line}\n") } else { format!("ERROR: {line}\n") });
                }
                out.push_str("ERROR: error parsing VENDOR.bazel file");
            } else {
                let e = errors.first().cloned().unwrap_or_default();
                let msg = strip_location(&e);
                out.push_str(&format!("ERROR: error parsing VENDOR.bazel file: {msg}"));
            }
            return Err(out);
        }
        let g = state.lock().unwrap();
        Ok(Self { ignored_repos: g.0.clone(), pinned_repos: g.1.clone() })
    }
}

/// Drops a leading `file:line:col: ` location (and `ERROR: `) from a Starlark error.
fn strip_location(e: &str) -> String {
    let e = e.trim().trim_start_matches("ERROR: ");
    let mut rest = e;
    // location prefix: <path>:<line>:<col>: 
    if let Some(idx) = e.find(": ") {
        let head = &e[..idx];
        let mut parts = head.rsplitn(3, ':');
        let col = parts.next();
        let line = parts.next();
        if col.map(|c| c.chars().all(|x| x.is_ascii_digit())).unwrap_or(false) && line.map(|c| c.chars().all(|x| x.is_ascii_digit())).unwrap_or(false) {
            rest = &e[idx + 2..];
        }
    }
    rest.to_string()
}

/// Re-plants symlinks under `repo_under_vendor` to relative paths as in Bazel's `RepositoryUtils.replantSymlinks`.
pub fn replant_symlinks(
    repo_under_vendor: &Path,
    workspace: &Path,
    external_repo_root: &Path,
) -> Result<(), String> {
    let vendor_dir = repo_under_vendor.parent().ok_or("Cannot get vendor directory")?;

    // 1. Ensure <vendor_dir>/bazel-external symlinks to external_repo_root
    let external_symlink = vendor_dir.join(EXTERNAL_ROOT_SYMLINK_NAME);
    ensure_symlink(external_repo_root, &external_symlink)?;

    // 2. Ensure <external_repo_root>/_main symlinks to workspace
    let workspace_symlink = external_repo_root.join(WORKSPACE_SYMLINK_NAME);
    ensure_symlink(workspace, &workspace_symlink)?;

    // 3. Traverse repo_under_vendor and rewrite symlinks
    let mut symlinks = Vec::new();
    collect_symlinks(repo_under_vendor, &mut symlinks)?;

    for symlink_path in symlinks {
        let target = match fs::read_link(&symlink_path) {
            Ok(t) => t,
            Err(_) => continue,
        };

        let parent = symlink_path.parent().unwrap_or(repo_under_vendor);

        let new_target = if target.starts_with(workspace) {
            // Symlink pointing into main repo: route through bazel-external/_main
            let rel_in_ws = target.strip_prefix(workspace).unwrap();
            let depth_to_vendor = rel_depth(parent, vendor_dir);
            let mut rel = PathBuf::new();
            for _ in 0..depth_to_vendor {
                rel.push("..");
            }
            rel.push(EXTERNAL_ROOT_SYMLINK_NAME);
            rel.push(WORKSPACE_SYMLINK_NAME);
            rel.push(rel_in_ws);
            Some(rel)
        } else if target.is_absolute() && target.starts_with(repo_under_vendor) {
            // Same-repo symlink: replant relative within the repo
            let rel_in_repo = target.strip_prefix(repo_under_vendor).unwrap();
            let depth_to_repo = rel_depth(parent, repo_under_vendor);
            let mut rel = PathBuf::new();
            for _ in 0..depth_to_repo {
                rel.push("..");
            }
            rel.push(rel_in_repo);
            Some(rel)
        } else if target.is_absolute() && target.starts_with(external_repo_root) {
            // Cross-repo symlink pointing into external root: route through bazel-external
            let rel_in_ext = target.strip_prefix(external_repo_root).unwrap();
            let depth_to_vendor = rel_depth(parent, vendor_dir);
            let mut rel = PathBuf::new();
            for _ in 0..depth_to_vendor {
                rel.push("..");
            }
            rel.push(EXTERNAL_ROOT_SYMLINK_NAME);
            rel.push(rel_in_ext);
            Some(rel)
        } else {
            None
        };

        if let Some(new_target) = new_target {
            let _ = fs::remove_file(&symlink_path);
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(&new_target, &symlink_path)
                    .map_err(|e| format!("Failed to replant symlink {}: {e}", symlink_path.display()))?;
            }
        }
    }

    Ok(())
}

fn rel_depth(from: &Path, to: &Path) -> usize {
    if let Ok(rel) = from.strip_prefix(to) {
        rel.components().count()
    } else {
        1
    }
}

fn collect_symlinks(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return Ok(()),
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if let Ok(ft) = entry.file_type() {
            if ft.is_symlink() {
                out.push(p);
            } else if ft.is_dir() {
                collect_symlinks(&p, out)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn ensure_symlink(target: &Path, link: &Path) -> Result<(), String> {
    if link.is_symlink() {
        if let Ok(existing) = fs::read_link(link) {
            if existing == target {
                return Ok(());
            }
        }
        let _ = fs::remove_file(link);
    } else if link.exists() {
        let _ = fs::remove_dir_all(link);
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
            .map_err(|e| format!("Failed to create symlink {} -> {}: {e}", link.display(), target.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_vendor_file() {
        let content = r#"
# VENDOR.bazel
ignore("@@a+", "@@b+")
pin("@@c+")
"#;
        let vf = VendorFile::parse(content).unwrap();
        assert!(vf.ignored_repos.contains("a+"));
        assert!(vf.ignored_repos.contains("b+"));
        assert!(vf.pinned_repos.contains("c+"));
        assert!(!vf.ignored_repos.contains("c+"));
    }

    #[test]
    fn vendor_file_requires_double_at() {
        let content = r#"ignore("a+")"#;
        assert!(VendorFile::parse(content).is_err());
    }
}
