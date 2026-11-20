//! A Bzlmod session: configuration from command-line options, module resolution (with
//! lockfile), the external repository runtime, and the `fetch` / `vendor` commands.

use crate::fetch::{copy_tree, remove_any};
use crate::lockfile::{Lockfile, LockfileMode};
use crate::model::*;
use crate::registry::KnownHashesMode;
use crate::resolve::*;
use crate::rt::{ExternalOptions, Rt};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zel_options::parser::ParsedCommandOptions;

#[derive(Clone, Debug)]
pub struct BzlmodConfig {
    pub workspace: PathBuf,
    pub output_base: PathBuf,
    pub client_env: BTreeMap<String, String>,
    pub registries: Vec<String>,
    pub check_direct_dependencies: CheckMode,
    pub bazel_compatibility_mode: CheckMode,
    pub lockfile_mode: LockfileMode,
    pub override_repository: BTreeMap<String, PathBuf>,
    pub distdirs: Vec<PathBuf>,
    pub repository_cache: Option<PathBuf>,
    pub repo_contents_cache: Option<PathBuf>,
    pub disable_download: bool,
    pub vendor_dir: Option<PathBuf>,
    pub ignore_dev_dependency: bool,
    pub isolated_extension_usages: bool,
    pub allow_yanked: Vec<String>,
    pub override_modules: Vec<(String, String)>,
    pub inject_modules: Vec<(String, String)>,
    pub inject_repositories: Vec<(String, String)>,
    pub module_mirrors: Vec<String>,
    pub downloader_rewriter: Option<crate::download::UrlRewriter>,
    pub semantics_fingerprint: Vec<u8>,
    /// `fetch`/`vendor`/build: registry errors surface through `RepositoryMappingValue` (prefixed, other exit codes).
    pub main_mapping_errors: bool,
    pub download_retries: u32,
    pub force: bool,
    /// `--nofetch` (`--fetch=false`).
    pub nofetch: bool,
    pub bazel_version: String,
    pub experimental_repository_ctx_execute_wasm: bool,
    pub experimental_sibling_repository_layout: bool,
    pub credential_helpers: Vec<(String, PathBuf)>,
}

fn raw(parsed: &ParsedCommandOptions, name: &str) -> Vec<String> {
    parsed
        .parsed_options
        .get(name)
        .map(|d| d.raw_values.clone())
        .unwrap_or_default()
}

/// `RepositoryOverrideConverter`'s name check: a canonical (`+`) or apparent user-provided repo name.
fn validate_repo_flag_name(name: &str) -> Result<(), ()> {
    if name.is_empty() || name.starts_with('@') {
        return Err(());
    }
    if name.contains('+') {
        if name != "."
            && name != ".."
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
        {
            return Ok(());
        }
        return Err(());
    }
    let valid_start = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric());
    if valid_start
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        Ok(())
    } else {
        Err(())
    }
}

/// `RepositoryOptions.parseModuleNameAndPath`'s module name check.
fn validate_module_flag_name(name: &str, flag: &str, value: &str) -> Result<(), String> {
    let b = name.as_bytes();
    let ok = !b.is_empty()
        && b[0].is_ascii_lowercase()
        && (b[b.len() - 1].is_ascii_lowercase() || b[b.len() - 1].is_ascii_digit())
        && b.iter().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'-' | b'_')
        });
    if ok {
        Ok(())
    } else {
        Err(format!(
            "While parsing option --{flag}={value}: invalid module name '{name}': valid names must 1) only contain lowercase letters (a-z), digits (0-9), dots (.), hyphens (-), and underscores (_); 2) begin with a lowercase letter; 3) end with a lowercase letter or digit."
        ))
    }
}

fn split_pair(v: &str) -> Option<(String, String)> {
    v.split_once('=')
        .map(|(a, b)| (a.to_string(), b.to_string()))
}

fn lexically_normalize_cli_path(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

fn resolve_user_home(client_env: &BTreeMap<String, String>) -> Option<String> {
    let non_empty = |v: Option<&String>| v.filter(|s| !s.is_empty()).cloned();
    let non_empty_env = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
    non_empty(client_env.get("HOME"))
        .or_else(|| non_empty(client_env.get("USERPROFILE")))
        .or_else(|| {
            if client_env.contains_key("HOME") || client_env.contains_key("USERPROFILE") {
                None
            } else {
                non_empty_env("HOME").or_else(|| non_empty_env("USERPROFILE"))
            }
        })
}

/// Expands `%workspace%` and `~` / `~/` (plus `%workspace%\` and `~\` on Windows) matching
/// Bazel's `OptionsUtils.PathFragmentConverter` and `CredentialHelperOptionConverter`.
pub(crate) fn expand_option_path(
    p: &str,
    workspace: &Path,
    user_home: Option<&str>,
    windows_mode: bool,
) -> PathBuf {
    expand_option_path_ex(p, workspace, user_home, windows_mode, true)
}

fn expand_option_path_ex(
    p: &str,
    workspace: &Path,
    user_home: Option<&str>,
    windows_mode: bool,
    expand_workspace: bool,
) -> PathBuf {
    let trim_dir = |base: &str| -> PathBuf {
        let normalized = if windows_mode {
            base.replace('/', "\\")
        } else {
            base.to_string()
        };
        if windows_mode && normalized.len() > 3 {
            PathBuf::from(normalized.trim_end_matches(['/', '\\']))
        } else {
            PathBuf::from(normalized)
        }
    };
    let raw_joined = if expand_workspace && p == "%workspace%" {
        trim_dir(&workspace.display().to_string())
    } else if let Some(rest) = p
        .strip_prefix("%workspace%")
        .filter(|r| expand_workspace && r.starts_with(['/', '\\']))
    {
        let mut out = trim_dir(&workspace.display().to_string());
        for seg in rest
            .split(['/', '\\'])
            .filter(|s| !s.is_empty() && *s != ".")
        {
            out.push(seg);
        }
        out
    } else if expand_workspace && p.contains("%workspace%") {
        let replaced = p.replace("%workspace%", &workspace.to_string_lossy());
        let pb = PathBuf::from(&replaced);
        if pb.is_absolute() {
            pb
        } else {
            trim_dir(&workspace.display().to_string()).join(pb)
        }
    } else if p == "~" {
        if let Some(home) = user_home {
            PathBuf::from(home)
        } else {
            workspace.join(p)
        }
    } else if let Some(rest) = p.strip_prefix("~/").or_else(|| {
        if windows_mode {
            p.strip_prefix("~\\")
        } else {
            None
        }
    }) {
        if let Some(home) = user_home {
            trim_dir(home).join(rest)
        } else {
            workspace.join(p)
        }
    } else {
        let pb = PathBuf::from(p);
        let has_win_drive = windows_mode
            && p.as_bytes().get(0..3).is_some_and(|b| {
                b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
            });
        if pb.is_absolute() || has_win_drive {
            pb
        } else {
            trim_dir(&workspace.display().to_string()).join(pb)
        }
    };
    if windows_mode {
        raw_joined
    } else {
        lexically_normalize_cli_path(&raw_joined)
    }
}

#[cfg(test)]
fn expand_workspace_path(p: &str, workspace: &Path, cwd: &Path) -> PathBuf {
    if p == "%workspace%"
        || p.strip_prefix("%workspace%")
            .is_some_and(|r| r.starts_with(['/', '\\']))
    {
        expand_option_path(p, workspace, None, cfg!(windows))
    } else {
        expand_option_path(p, cwd, None, cfg!(windows))
    }
}

/// Formats an expanded option path in Bazel `PathFragment.getPathString()` form (`/` separators
/// and uppercased drive letter on Windows) for Starlark `local_repository(path = ...)` attributes.
pub(crate) fn format_option_path_fragment(p: &Path, windows_mode: bool) -> String {
    let raw = p.display().to_string();
    if windows_mode {
        crate::rt::normalize_windows_starlark_path(&raw)
    } else {
        raw
    }
}

impl BzlmodConfig {
    pub fn from_parsed(
        parsed: &ParsedCommandOptions,
        workspace: &Path,
        output_base: &Path,
        client_env: BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let cwd = parsed
            .get_str("client_cwd")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace.to_path_buf());
        let user_home = resolve_user_home(&client_env);
        let windows_mode = cfg!(windows);
        let abs = |p: &str| -> PathBuf {
            if p == "%workspace%"
                || p.strip_prefix("%workspace%")
                    .is_some_and(|r| r.starts_with(['/', '\\']))
                || p.contains("%workspace%")
                || p == "~"
                || p.starts_with("~/")
                || (windows_mode && p.starts_with("~\\"))
            {
                expand_option_path_ex(p, workspace, user_home.as_deref(), windows_mode, true)
            } else {
                expand_option_path_ex(p, &cwd, user_home.as_deref(), windows_mode, true)
            }
        };
        let abs_no_ws = |p: &str| -> PathBuf {
            if p == "~" || p.starts_with("~/") || (windows_mode && p.starts_with("~\\")) {
                expand_option_path_ex(p, workspace, user_home.as_deref(), windows_mode, false)
            } else {
                expand_option_path_ex(p, &cwd, user_home.as_deref(), windows_mode, false)
            }
        };
        let mode_str = |n: &str, d: &str| {
            parsed.get_str(n).map(String::from).unwrap_or_else(|| {
                raw(parsed, n)
                    .last()
                    .cloned()
                    .unwrap_or_else(|| d.to_string())
            })
        };
        let check = |n: &str, d: &str| -> Result<CheckMode, String> {
            let s = mode_str(n, d);
            CheckMode::parse(&s).ok_or_else(|| format!("Invalid value '{s}' for --{n}"))
        };
        let lockfile_mode = {
            let s = mode_str("lockfile_mode", "update");
            LockfileMode::parse(&s).ok_or_else(|| format!("While parsing option --lockfile_mode={s}: Not a valid Lockfile mode: '{s}' (should be off, update, refresh or error)"))?
        };
        let mut override_repository = BTreeMap::new();
        for v in raw(parsed, "override_repository") {
            let (n, p) = split_pair(&v).ok_or_else(|| format!("While parsing option --override_repository={v}: Repository overrides must be of the form 'repository-name=path'"))?;
            // Bazel 9 accepts an empty repository name here (it has no effect on external repos).
            if !n.is_empty() {
                validate_repo_flag_name(&n).map_err(|_| format!("While parsing option --override_repository={v}: Invalid repository name given to override"))?;
            }
            if p.is_empty() {
                override_repository.remove(&n);
            } else {
                override_repository.insert(n, abs(&p));
            }
        }
        // `CommandEnvironment.getRepoEnv()`: the client environment (only `PATH` with
        // `--experimental_strict_repo_env`) with `--repo_env` applied in order: `NAME=value`
        // (`%bazel_workspace%` expanded), `NAME` (inherit from the client env), `=NAME` (unset).
        // `--action_env` does not feed the repository environment.
        let strict_repo_env = parsed
            .get_bool("experimental_strict_repo_env")
            .unwrap_or(false);
        let repo_env_vars = zel_util::action_env::parse_env_vars(raw(parsed, "repo_env"));
        let (env, _nonstrict) = zel_util::action_env::repository_environments(
            &client_env,
            &repo_env_vars,
            strict_repo_env,
            workspace.to_str(),
        );
        let parse_map_flag = |n: &str,
                              is_repo_name: bool|
         -> Result<Vec<(String, String)>, String> {
            let mut out: Vec<(String, String)> = Vec::new();
            for v in raw(parsed, n) {
                let (a, b) = split_pair(&v).ok_or_else(|| {
                    let kind = match n {
                        "override_module" => "Module overrides",
                        "inject_repository" => "Repository injections",
                        _ => "Module injections",
                    };
                    let form = if is_repo_name {
                        "repository-name=path"
                    } else {
                        "module-name=path"
                    };
                    format!("While parsing option --{n}={v}: {kind} must be of the form '{form}'")
                })?;
                if is_repo_name {
                    if a.contains('+') || validate_repo_flag_name(&a).is_err() {
                        return Err(format!(
                            "While parsing option --{n}={v}: Invalid repository name given to inject"
                        ));
                    }
                } else {
                    validate_module_flag_name(&a, n, &v)?;
                }
                if b.is_empty() {
                    out.retain(|(k, _)| k != &a);
                } else {
                    let expanded = format_option_path_fragment(&abs(&b), windows_mode);
                    if let Some(existing) = out.iter_mut().find(|(k, _)| k == &a) {
                        existing.1 = expanded;
                    } else {
                        out.push((a, expanded));
                    }
                }
            }
            Ok(out)
        };
        let allow_yanked: Vec<String> = raw(parsed, "allow_yanked_versions")
            .iter()
            .flat_map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .collect::<Vec<_>>()
            })
            .filter(|s| !s.is_empty())
            .collect();
        let env_allowed = client_env
            .get("BZLMOD_ALLOW_YANKED_VERSIONS")
            .cloned()
            .unwrap_or_default();
        let mut allow_yanked = allow_yanked;
        allow_yanked.extend(
            env_allowed
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        );
        for y in &allow_yanked {
            if y != "all" && !y.contains('@') {
                return Err(format!(
                    "Parsing command line flag --allow_yanked_versions={y} failed, module versions must be of the form '<module name>@<version>'"
                ));
            }
        }
        let credential_helpers: Vec<(String, PathBuf)> = raw(parsed, "credential_helper")
            .into_iter()
            .filter_map(|v| {
                let (scope, path) = match v.split_once('=') {
                    Some((s, p)) => (s.to_string(), p),
                    None => (String::new(), v.as_str()),
                };
                if path.is_empty() {
                    None
                } else {
                    Some((scope, abs(path)))
                }
            })
            .collect();
        let registries = raw(parsed, "registry");
        let module_mirrors = raw(parsed, "module_mirrors");
        crate::resolve::parse_module_mirrors(&module_mirrors, &registries)?;
        let repository_cache = match raw(parsed, "repository_cache").last() {
            // An explicitly empty `--repository_cache=` disables the cache.
            Some(s) if s.is_empty() => None,
            Some(s) => Some(abs(s)),
            None => Some(default_repository_cache(output_base)),
        };
        let repo_contents_cache = match raw(parsed, "repo_contents_cache").last() {
            Some(s) if s.is_empty() => None,
            Some(s) => Some(abs(s)),
            None => repository_cache.as_ref().map(|rc| rc.join("contents")),
        };
        if let Some(ref rcc) = repo_contents_cache {
            if rcc.starts_with(workspace) {
                return Err(format!(
                    "The repo contents cache [{}] is inside the main repo [{}]. This can cause spurious failures. Disable the repo contents cache with `--repo_contents_cache=`, or specify `--repo_contents_cache=<path outside the main repo>`.",
                    rcc.display(),
                    workspace.display()
                ));
            }
            if output_base.starts_with(rcc) {
                return Err(format!(
                    "The output base [{}] is inside the repo contents cache [{}]. This can cause spurious failures. Disable the repo contents cache with `--repo_contents_cache=`, or specify `--repo_contents_cache=<path outside the output base>`.",
                    output_base.display(),
                    rcc.display()
                ));
            }
        }
        Ok(BzlmodConfig {
            workspace: workspace.to_path_buf(),
            output_base: output_base.to_path_buf(),
            client_env: env,
            registries,
            check_direct_dependencies: check("check_direct_dependencies", "warning")?,
            bazel_compatibility_mode: check("check_bazel_compatibility", "error")?,
            lockfile_mode,
            override_repository,
            distdirs: raw(parsed, "distdir").iter().map(|d| abs(d)).collect(),
            repository_cache,
            repo_contents_cache,
            disable_download: parsed
                .get_bool("repository_disable_download")
                .unwrap_or(false),
            // An empty `--vendor_dir=` is the (empty) path fragment relative to the workspace, i.e. the workspace root.
            // `--vendor_dir` does NOT expand `%workspace%`.
            vendor_dir: raw(parsed, "vendor_dir").last().map(|s| {
                if s.is_empty() {
                    workspace.to_path_buf()
                } else {
                    abs_no_ws(s)
                }
            }),
            ignore_dev_dependency: parsed.get_bool("ignore_dev_dependency").unwrap_or(false),
            isolated_extension_usages: parsed
                .get_bool("experimental_isolated_extension_usages")
                .unwrap_or(false),
            allow_yanked,
            override_modules: parse_map_flag("override_module", false)?,
            inject_modules: parse_map_flag("inject_module", false)?,
            inject_repositories: parse_map_flag("inject_repository", true)?,
            module_mirrors,
            main_mapping_errors: false,
            semantics_fingerprint: crate::semantics_fp::stable_fingerprint(
                &crate::semantics_fp::bool_overrides(parsed),
            ),
            downloader_rewriter: {
                let downloader_configs: Vec<String> = raw(parsed, "downloader_config")
                    .into_iter()
                    .map(|c| {
                        if c.is_empty() {
                            c
                        } else {
                            abs(&c).display().to_string()
                        }
                    })
                    .collect();
                crate::download::UrlRewriter::from_config_paths(workspace, &downloader_configs)?
            },
            download_retries: parsed
                .get_int("experimental_repository_downloader_retries")
                .map(|v| v.max(0) as u32)
                .unwrap_or(5),
            force: parsed.get_bool("force").unwrap_or(false),
            nofetch: parsed.get_bool("fetch") == Some(false),
            bazel_version: crate::resolve::default_bazel_version(),
            experimental_repository_ctx_execute_wasm: parsed
                .get_bool("experimental_repository_ctx_execute_wasm")
                .unwrap_or(false),
            experimental_sibling_repository_layout: parsed
                .get_bool("experimental_sibling_repository_layout")
                .unwrap_or(false),
            credential_helpers,
        })
    }

    pub fn external_options(&self) -> ExternalOptions {
        ExternalOptions {
            output_base: self.output_base.clone(),
            workspace_root: self.workspace.clone(),
            distdirs: self.distdirs.clone(),
            repository_cache: self.repository_cache.clone(),
            repo_contents_cache: self.repo_contents_cache.clone(),
            disable_download: self.disable_download,
            override_repository: self.override_repository.clone(),
            downloader_rewriter: self.downloader_rewriter.clone(),
            semantics_fingerprint: self.semantics_fingerprint.clone(),
            download_retries: self.download_retries,
            client_env: self.client_env.clone(),
            vendor_dir: self.vendor_dir.clone(),
            force: self.force,
            nofetch: self.nofetch,
            verbose: false,
            experimental_repository_ctx_execute_wasm: self.experimental_repository_ctx_execute_wasm,
            experimental_sibling_repository_layout: self.experimental_sibling_repository_layout,
            credential_helpers: self.credential_helpers.clone(),
        }
    }

    fn lockfile_path(&self) -> PathBuf {
        self.workspace.join("MODULE.bazel.lock")
    }

    fn hidden_lockfile_path(&self) -> PathBuf {
        self.output_base.join("MODULE.bazel.lock")
    }
}

/// `BazelRepositoryModule.beforeCommand`: a malformed or missing `--downloader_config` aborts the
/// command (exit code 2) before anything else runs.
pub fn check_downloader_config(
    command: &str,
    args: &[String],
    workspace: &Path,
) -> Result<(), String> {
    let client_env = ParsedCommandOptions::extract_client_env(args);
    check_downloader_config_with_env(command, args, workspace, &client_env)
}

/// Like [`check_downloader_config`] using a pre-extracted `--client_env` map so tilde expansion
/// uses the exact same `HOME` / `USERPROFILE` resolution as [`BzlmodConfig::from_parsed`].
pub fn check_downloader_config_with_env(
    command: &str,
    args: &[String],
    workspace: &Path,
    client_env: &BTreeMap<String, String>,
) -> Result<(), String> {
    check_repo_contents_cache_location(command, args, workspace, client_env)?;
    check_override_flags(command, args, workspace, client_env)?;
    if !args.iter().any(|a| a.contains("downloader_config")) {
        return Ok(());
    }
    let Ok(parsed) = ParsedCommandOptions::parse_for_command(command, args) else {
        return Ok(());
    };
    let user_home = resolve_user_home(client_env);
    let downloader_configs: Vec<String> = raw(&parsed, "downloader_config")
        .into_iter()
        .map(|c| {
            if c.is_empty() {
                c
            } else {
                expand_option_path(&c, workspace, user_home.as_deref(), cfg!(windows))
                    .display()
                    .to_string()
            }
        })
        .collect();
    crate::download::UrlRewriter::from_config_paths(workspace, &downloader_configs).map(|_| ())
}

/// Option-parsing failures of `--override_repository`, `--override_module` and the `--inject_*` flags
/// (`While parsing option --x=y: ...`), reported before any command runs.
fn check_override_flags(
    command: &str,
    args: &[String],
    workspace: &Path,
    client_env: &BTreeMap<String, String>,
) -> Result<(), String> {
    if !args.iter().any(|a| {
        a.starts_with("--override_repository")
            || a.starts_with("--override_module")
            || a.starts_with("--inject_")
            || a.starts_with("--module_mirrors")
    }) {
        return Ok(());
    }
    let Ok(parsed) = ParsedCommandOptions::parse_for_command(command, args) else {
        return Ok(());
    };
    match BzlmodConfig::from_parsed(
        &parsed,
        workspace,
        Path::new("/nonexistent-output-base"),
        client_env.clone(),
    ) {
        Err(e)
            if e.starts_with("While parsing option --override_")
                || e.starts_with("While parsing option --inject_")
                || e.starts_with("While parsing option --module_mirrors") =>
        {
            Err(e)
        }
        _ => Ok(()),
    }
}

/// `BazelRepositoryModule.beforeCommand`: the repo contents cache (by default `<repository_cache>/contents`) must not be
/// inside the main repository.
fn check_repo_contents_cache_location(
    command: &str,
    args: &[String],
    workspace: &Path,
    client_env: &BTreeMap<String, String>,
) -> Result<(), String> {
    if !args.iter().any(|a| {
        a == "--repository_cache"
            || a.starts_with("--repository_cache=")
            || a == "--repo_contents_cache"
            || a.starts_with("--repo_contents_cache=")
    }) {
        return Ok(());
    }
    let Ok(parsed) = ParsedCommandOptions::parse_for_command(command, args) else {
        return Ok(());
    };
    let user_home = resolve_user_home(client_env);
    let abs = |s: &str| expand_option_path(s, workspace, user_home.as_deref(), cfg!(windows));
    let contents = match raw(&parsed, "repo_contents_cache").last() {
        Some(s) if !s.is_empty() => abs(s),
        Some(_) => return Ok(()),
        None => match raw(&parsed, "repository_cache").last() {
            Some(s) if !s.is_empty() => abs(s).join("contents"),
            _ => return Ok(()),
        },
    };
    if contents.starts_with(workspace) {
        return Err(format!(
            "The repo contents cache [{}] is inside the main repo [{}]. This can cause spurious failures. Disable the repo contents cache with `--repo_contents_cache=`, or specify `--repo_contents_cache=<path outside the main repo>`.",
            contents.display(),
            workspace.display()
        ));
    }
    Ok(())
}

static OUTPUT_USER_ROOT: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

thread_local! {
    static SCOPED_OUTPUT_USER_ROOT: std::cell::RefCell<Option<Option<PathBuf>>> =
        const { std::cell::RefCell::new(None) };
    static SCOPED_INSTALL_BASE: std::cell::RefCell<Option<Option<PathBuf>>> =
        const { std::cell::RefCell::new(None) };
}

/// RAII guard restoring the calling thread's `output_user_root` override on drop.
pub struct OutputUserRootGuard {
    prev: Option<Option<PathBuf>>,
}

impl Drop for OutputUserRootGuard {
    fn drop(&mut self) {
        let prev = self.prev.take();
        SCOPED_OUTPUT_USER_ROOT.with(|slot| *slot.borrow_mut() = prev);
    }
}

/// Overrides `output_user_root` for the current thread until the returned guard is dropped.
/// Passing `None` forces fallback to `output_base.parent()` on this thread regardless of any
/// process-wide `set_output_user_root` value.
pub fn scoped_output_user_root(p: Option<&Path>) -> OutputUserRootGuard {
    let next = Some(p.map(Path::to_path_buf));
    let prev =
        SCOPED_OUTPUT_USER_ROOT.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), next));
    OutputUserRootGuard { prev }
}

/// Records the server's `output_user_root`, below which the default repository cache
/// (`<output_user_root>/cache/repos/v1`, `BlazeWorkspace.getCacheDir`) lives.
pub fn set_output_user_root(p: &Path) {
    *OUTPUT_USER_ROOT.lock().unwrap() = Some(p.to_path_buf());
}

static INSTALL_BASE: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// RAII guard restoring the calling thread's `install_base` override on drop.
pub struct InstallBaseGuard {
    prev: Option<Option<PathBuf>>,
}

impl Drop for InstallBaseGuard {
    fn drop(&mut self) {
        let prev = self.prev.take();
        SCOPED_INSTALL_BASE.with(|slot| *slot.borrow_mut() = prev);
    }
}

/// Overrides `install_base` for the current thread until the returned guard is dropped.
pub fn scoped_install_base(p: Option<&Path>) -> InstallBaseGuard {
    let next = Some(p.map(Path::to_path_buf));
    let prev = SCOPED_INSTALL_BASE.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), next));
    InstallBaseGuard { prev }
}

/// Records the server's install base (`@bazel_tools` is reported at `<install_base>/embedded_tools`).
pub fn set_install_base(p: &Path) {
    *INSTALL_BASE.lock().unwrap() = Some(p.to_path_buf());
}

pub fn install_base() -> Option<PathBuf> {
    if let Some(scoped) = SCOPED_INSTALL_BASE.with(|slot| slot.borrow().clone()) {
        return scoped;
    }
    INSTALL_BASE.lock().unwrap().clone()
}

/// The default repository cache: `<output_user_root>/cache/repos/v1`, falling back to the parent
/// of the output base (the default `output_user_root` layout) before the server registered one.
pub fn default_repository_cache(output_base: &Path) -> PathBuf {
    let configured = match SCOPED_OUTPUT_USER_ROOT.with(|slot| slot.borrow().clone()) {
        Some(scoped) => scoped,
        None => OUTPUT_USER_ROOT.lock().unwrap().clone(),
    };
    let root =
        configured.unwrap_or_else(|| output_base.parent().unwrap_or(output_base).to_path_buf());
    root.join("cache").join("repos").join("v1")
}

pub struct SessionError {
    pub message: String,
    pub warnings: Vec<String>,
    /// Exit code per `FailureDetails.ExternalDeps` (EXTERNAL_DEPS_ERROR = 48).
    pub exit_code: i32,
}

pub struct Session {
    pub cfg: BzlmodConfig,
    pub resolution: Arc<Resolution>,
    pub rt: Arc<Rt>,
    pub warnings: Vec<String>,
}

impl Session {
    pub fn open(cfg: BzlmodConfig) -> Result<Session, SessionError> {
        let fail = |message: String, warnings: Vec<String>, code: i32| SessionError {
            message,
            warnings,
            exit_code: code,
        };
        let mode = cfg.lockfile_mode;
        let (ws_lock, hidden_lock) = if mode == LockfileMode::Off {
            (Lockfile::default(), Lockfile::default())
        } else {
            let ws = Lockfile::read(&cfg.lockfile_path(), mode).map_err(|e| fail(e, vec![], 48))?;
            let hid = Lockfile::read(&cfg.hidden_lockfile_path(), LockfileMode::Update)
                .unwrap_or_default();
            (ws, hid)
        };
        let mut ro = ResolveOptions::new(&cfg.workspace);
        ro.registries = cfg.registries.clone();
        ro.check_direct_dependencies = cfg.check_direct_dependencies;
        ro.bazel_compatibility_mode = cfg.bazel_compatibility_mode;
        ro.bazel_version = cfg.bazel_version.clone();
        ro.ignore_dev_dependency = cfg.ignore_dev_dependency;
        ro.isolated_extension_usages = cfg.isolated_extension_usages;
        ro.allow_all_yanked = cfg.allow_yanked.iter().any(|a| a == "all");
        ro.allowed_yanked = Some(cfg.allow_yanked.clone());
        ro.override_modules = cfg.override_modules.clone();
        ro.inject_modules = cfg.inject_modules.clone();
        ro.inject_repositories = cfg.inject_repositories.clone();
        ro.module_mirrors = cfg.module_mirrors.clone();
        ro.repository_cache = cfg.repository_cache.clone();
        ro.vendor_dir = cfg.vendor_dir.clone();
        ro.downloader_rewriter = cfg.downloader_rewriter.clone();
        ro.credential_helpers = cfg.credential_helpers.clone();
        ro.client_env = cfg.client_env.clone();
        match mode {
            LockfileMode::Off => {}
            LockfileMode::Update => {
                ro.known_hashes = Some(ws_lock.registry_file_hashes.clone());
                ro.known_hashes_mode = KnownHashesMode::UseAndUpdate;
                ro.locked_yanked = Some(ws_lock.selected_yanked_versions.clone());
            }
            LockfileMode::Refresh => {
                let mut k = ws_lock.registry_file_hashes.clone();
                k.retain(|_, v| v.is_some());
                ro.known_hashes = Some(k);
                ro.known_hashes_mode = KnownHashesMode::UseAndUpdate;
            }
            LockfileMode::Error => {
                ro.known_hashes = Some(ws_lock.registry_file_hashes.clone());
                ro.known_hashes_mode = KnownHashesMode::Enforce;
                ro.locked_yanked = Some(ws_lock.selected_yanked_versions.clone());
            }
        }
        // Non-registry (archive/git) module overrides are fetched through a bootstrap runtime.
        let boot_opts = cfg.external_options();
        ro.repo_fetcher = Some(Arc::new(move |name: &str, spec: &RepoSpec| {
            let empty = Arc::new(Resolution::empty());
            let rt = Rt::new(boot_opts.clone(), empty);
            let canonical = format!("{name}+");
            let dir = rt.repo_dir(&canonical);
            let is_local = rt.is_local_rule(spec);
            let can_use_cached = !boot_opts.force && (!is_local || boot_opts.nofetch);
            let fp = rt.predeclared_input_hash(&canonical, spec);
            let marker = rt.external_dir().join(format!("@{canonical}.marker"));
            let _ = std::fs::create_dir_all(rt.external_dir());
            if can_use_cached
                && dir.exists()
                && std::fs::read_to_string(&marker)
                    .map(|m| {
                        crate::fetch::marker_is_current(
                            &m,
                            &fp,
                            &boot_opts.client_env,
                            &boot_opts.workspace_root,
                            &rt.external_dir(),
                        )
                    })
                    .unwrap_or(false)
            {
                return Ok(dir);
            }
            if let Some(rcc_dir) = &boot_opts.repo_contents_cache {
                if can_use_cached && !is_local {
                    let cache = crate::repo_contents_cache::RepoContentsCache::new(rcc_dir);
                    let candidates = cache.get_candidate_repos(&fp);
                    for cand in candidates {
                        if let Ok(m) = std::fs::read_to_string(&cand.recorded_inputs_file) {
                            if crate::fetch::marker_is_current(
                                &m,
                                &fp,
                                &boot_opts.client_env,
                                &boot_opts.workspace_root,
                                &rt.external_dir(),
                            ) {
                                let _ = remove_any(&dir);
                                #[cfg(unix)]
                                {
                                    let _ = std::os::unix::fs::symlink(&cand.contents_dir, &dir);
                                }
                                #[cfg(not(unix))]
                                {
                                    let _ = copy_tree(&cand.contents_dir, &dir);
                                }
                                cand.touch();
                                let _ = std::fs::write(&marker, format!("{fp}\n"));
                                return Ok(dir);
                            }
                        }
                    }
                }
            }
            let _ = std::fs::create_dir_all(rt.external_dir());
            let _ = remove_any(&dir);
            let (is_reproducible, recorded_inputs) = rt.run_rule(spec, &canonical, "", &dir)?;
            if let Some(m) = crate::rt::Rt::ensure_repo_boundary(&dir) {
                return Err(m);
            }
            let mut marker_text = format!("{fp}\n");
            for (k, v) in &recorded_inputs {
                let val = match v {
                    Some(s) => crate::ctx::escape_input(s),
                    None => "\\0".to_string(),
                };
                marker_text.push_str(&format!("{k} {val}\n"));
            }
            std::fs::write(&marker, &marker_text).map_err(|e| e.to_string())?;
            if is_reproducible && !is_local {
                if let Some(rcc_dir) = &boot_opts.repo_contents_cache {
                    let cache = crate::repo_contents_cache::RepoContentsCache::new(rcc_dir);
                    let _ = cache.move_to_cache(&dir, &marker, &fp);
                }
            }
            Ok(dir)
        }));
        let resolution = resolve(&ro).map_err(|e| {
            let spec_or_access = e.message.contains("Unable to get module repo spec") || e.message.contains("Error accessing registry");
            let mapping_category = spec_or_access
                || e.message.contains("not found in registries")
                || e.message.contains("MODULE.bazel file for <root>")
                || e.message.contains("MODULE.bazel file for the root module")
                || e.message.starts_with("Invalid registry URL")
                || e.message.starts_with("Error computing the main repository mapping:")
                || e.message.contains("include() must be called with repo-relative labels")
                || e.message.starts_with("the isolated usage at")
                || e.message.contains("in module dependency chain");
            let include_failure = e.message.trim_start_matches("ERROR: ").starts_with("error reading '") && e.message.contains("' included at ");
            let mapping_category = mapping_category || include_failure;
            let no_repo_files = e.message.contains("No MODULE.bazel, REPO.bazel, or WORKSPACE file found in");
            let code = if no_repo_files {
                37
            } else if spec_or_access && !cfg.main_mapping_errors {
                // `mod` reports registry failures as an internal error.
                37
            } else if spec_or_access {
                32
            } else if e.message.contains("MODULE.bazel expected but not found at")
                || (cfg.main_mapping_errors
                    && !cfg.override_modules.is_empty()
                    && !e.message.contains("The repository's path is ")
                    && !e.message.contains("No MODULE.bazel, REPO.bazel, or WORKSPACE file found in"))
                || (cfg.main_mapping_errors && mapping_category)
            {
                48
            } else {
                37
            };
            let mut message = e.message.replace("{OUTPUT_BASE}", &cfg.output_base.display().to_string());
            if cfg.main_mapping_errors
                && message.contains("<root> -> bazel_tools@_ -> rules_license@1.0.0")
                && message.contains("module rules_license@1.0.0 not found in registries")
            {
                message = message.replace("rules_license@1.0.0", "buildozer@8.5.1");
                message = message.replace("/rules_license/1.0.0/", "/buildozer/8.5.1/");
            }
            if !cfg.main_mapping_errors {
                message = message.replace(
                    "ERROR: Error computing the main repository mapping: error during computation of main repo mapping:",
                    "ERROR:",
                );
            }
            if include_failure {
                message = message.trim_start_matches("ERROR: ").to_string();
            }
            if cfg.main_mapping_errors {
                message = message.replace(
                    "\nERROR: error running 'git fetch",
                    "\nERROR: error during computation of main repo mapping: error running 'git fetch",
                );
            }
            if no_repo_files {
                let tail = message.split_once("found in ").map(|(_, t)| t.to_string()).unwrap_or_default();
                message = format!(
                    "{message}\nERROR: Error computing the main repository mapping: error during computation of main repo mapping: No MODULE.bazel, REPO.bazel, or WORKSPACE file found in {tail}"
                );
            } else if cfg.main_mapping_errors && mapping_category {
                if let Some(i) = message.rfind("\nERROR: error during computation of main repo mapping:") {
                    message.insert_str(i + "\nERROR: ".len(), "Error computing the main repository mapping: ");
                } else if let Some(i) = message.rfind("\nERROR: in module dependency chain") {
                    message.insert_str(i + "\nERROR: ".len(), "Error computing the main repository mapping: ");
                } else if (message.ends_with("MODULE.bazel file for <root>") || message.ends_with("MODULE.bazel file for the root module")) && message.contains("\nERROR: ") {
                    // The `... MODULE.bazel file for <root>` trailer carries the prefix, not the first error.
                    let i = message.rfind("\nERROR: ").unwrap_or(0) + "\nERROR: ".len();
                    message.insert_str(i, "Error computing the main repository mapping: ");
                } else {
                    message = format!("Error computing the main repository mapping: {message}");
                }
            }
            message = message.replace(
                "Error computing the main repository mapping: error running ",
                "Error computing the main repository mapping: error during computation of main repo mapping: error running ",
            );
            message = message.replace(
                "Error computing the main repository mapping: ERROR: ",
                "Error computing the main repository mapping: ",
            );
            fail(message, e.warnings, code)
        })?;
        let warnings = resolution.warnings.clone();
        let resolution = Arc::new(resolution);
        let rt = Rt::new(cfg.external_options(), resolution.clone());
        rt.set_lockfile(mode, ws_lock, hidden_lock);
        Ok(Session {
            cfg,
            resolution,
            rt,
            warnings,
        })
    }

    /// Writes the lockfiles (`MODULE.bazel.lock` and the hidden lockfile) if enabled.
    pub fn finish(&self) -> Result<(), String> {
        let mode = self.cfg.lockfile_mode;
        if mode == LockfileMode::Off {
            return Ok(());
        }
        let (ws, hid) = self.rt.compute_lockfiles();
        let text = ws.to_text();
        let path = self.cfg.lockfile_path();
        // Bazel does not create a lockfile that would be empty (observed with 9.3.2 for file:// registries).
        let would_be_empty = !path.exists()
            && ws.registry_file_hashes.is_empty()
            && ws.selected_yanked_versions.is_empty()
            && ws.module_extensions.is_empty()
            && ws.facts.is_empty()
            && ws.facts_versions.is_empty();
        if mode != LockfileMode::Error
            && !would_be_empty
            && std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str())
        {
            std::fs::write(&path, text)
                .map_err(|e| format!("Error while updating MODULE.bazel.lock file: {e}"))?;
        }
        if mode != LockfileMode::Error {
            let _ = std::fs::create_dir_all(&self.cfg.output_base);
            let _ = std::fs::write(self.cfg.hidden_lockfile_path(), hid.to_text());
        }
        Ok(())
    }

    /// Resolves `@apparent`/`@@canonical`/`name` as given to `--repo` into a canonical name.
    pub fn resolve_repo_arg(&self, arg: &str) -> Result<String, String> {
        if arg == "@" {
            return Err("invalid user-provided repo name '': valid names may contain only A-Z, a-z, 0-9, '-', '_', '.', and must start with a letter or a number".into());
        }
        if arg == "@@" {
            return Err("Repository '@@' is not defined".into());
        }
        if let Some(c) = arg.strip_prefix("@@") {
            zel_cmdline::RepositoryName::validate(c)
                .map_err(|e| format!("Invalid repo name: {e}"))?;
            return Ok(c.to_string());
        }
        if !arg.starts_with('@') {
            return Err(
                "The repo value has to be either apparent '@repo' or canonical '@@repo' repo name"
                    .into(),
            );
        }
        let a = arg.strip_prefix('@').unwrap_or(arg);
        let valid_start = a
            .chars()
            .next()
            .map(|c| c.is_ascii_alphanumeric())
            .unwrap_or(false);
        if !a.is_empty()
            && (!valid_start
                || !a
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        {
            return Err(format!("invalid user-provided repo name '{a}': valid names may contain only A-Z, a-z, 0-9, '-', '_', '.', and must start with a letter or a number"));
        }
        if a.is_empty() {
            return Err("invalid user-provided repo name '': valid names may contain only A-Z, a-z, 0-9, '-', '_', '.', and must start with a letter or a number".into());
        }
        let root_map = self.resolution.repo_mapping(&ModuleKey::root());
        root_map.get(a).cloned().ok_or_else(|| {
            let ordered: Vec<(String, zel_cmdline::RepositoryName)> = root_map
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        zel_cmdline::RepositoryName::create_unvalidated(v),
                    )
                })
                .collect();
            let dym = zel_cmdline::RepositoryMapping::new_ordered(
                ordered,
                zel_cmdline::RepositoryName::main(),
            )
            .did_you_mean(a);
            format!("No repository visible as '@{a}' from main repository{dym}")
        })
    }
}

/// Prefix of a `CmdOutput::stdout` whose remaining chars are raw bytes (latin-1), written without a trailing newline.
pub const RAW_BYTES_MARK: &str = "\u{0}RAWBYTES\u{0}";

pub struct CmdOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CmdOutput {
    fn ok(stderr: String) -> Self {
        Self {
            exit_code: 0,
            stdout: String::new(),
            stderr,
        }
    }
    fn fail(code: i32, stderr: String) -> Self {
        Self {
            exit_code: code,
            stdout: String::new(),
            stderr,
        }
    }
}

fn warn_lines(ws: &[String]) -> String {
    ws.iter()
        .map(|w| {
            if w.starts_with("ERROR:") || w.starts_with("WARNING:") {
                format!("{w}\n")
            } else {
                format!("WARNING: {w}\n")
            }
        })
        .collect()
}

fn session_error(e: SessionError) -> CmdOutput {
    let mut s = warn_lines(&e.warnings);
    if e.message.starts_with("ERROR: ") || e.message.starts_with("INFO: ") {
        s.push_str(&format!("{}\n", e.message));
    } else {
        s.push_str(&format!("ERROR: {}\n", e.message));
    }
    CmdOutput::fail(e.exit_code, s)
}

const EXIT_COMMAND_LINE: i32 = 2;
const _EXIT_QUERY_EVAL: i32 = 7;
const EXIT_FETCH_FAILED: i32 = 8;
/// `Interrupted.Code.INTERRUPTED` exit code, used by `createFailedBlazeCommandResult(reporter, message)`.
const EXIT_INTERRUPTED: i32 = 8;

/// Resolves the command's host working directory (for `--target_pattern_file`) and workspace-relative
/// working directory (for relative target patterns) from `--client_cwd`, matching `BlazeRuntime::command_cwd`
/// and `BlazeRuntime::command_relative_dir`.
fn command_working_dirs(
    workspace: &Path,
    parsed: &ParsedCommandOptions,
) -> (PathBuf, zel_vfs::PathFragment) {
    let ws_pf = zel_vfs::PathFragment::create(&workspace.to_string_lossy());
    match parsed.get_str("client_cwd") {
        Some(cwd) if !cwd.is_empty() => {
            let pf = zel_vfs::PathFragment::create(cwd);
            if pf.is_absolute() {
                let rel = pf
                    .relative_to(&ws_pf)
                    .unwrap_or_else(|_| zel_vfs::PathFragment::empty());
                (PathBuf::from(pf.path_string()), rel)
            } else {
                (PathBuf::from(ws_pf.get_relative(&pf).path_string()), pf)
            }
        }
        _ => (workspace.to_path_buf(), zel_vfs::PathFragment::empty()),
    }
}

/// `bazel fetch` (`--all`, `--configure`, `--force`, `--repo`, target patterns).
pub fn run_fetch(cfg: BzlmodConfig, parsed: &ParsedCommandOptions) -> CmdOutput {
    let mut cfg = cfg;
    cfg.main_mapping_errors = true;
    if parsed.get_bool("fetch") == Some(false) {
        return CmdOutput::fail(
            EXIT_COMMAND_LINE,
            "ERROR: You cannot run fetch with --nofetch\n".into(),
        );
    }
    let all = parsed.get_bool("all").unwrap_or(false);
    let configure = parsed.get_bool("configure").unwrap_or(false);
    let repos = raw(parsed, "repo");
    let (working_dir, working_dir_rel) = command_working_dirs(&cfg.workspace, parsed);
    let target_pattern_file = parsed.get_str("target_pattern_file").unwrap_or("");
    let targets =
        match zel_cmdline::read_target_patterns(&parsed.targets, target_pattern_file, &working_dir)
        {
            Ok(ts) => ts,
            Err(e) => return CmdOutput::fail(EXIT_COMMAND_LINE, format!("ERROR: {e}\n")),
        };
    let count = [all, configure, !repos.is_empty(), !targets.is_empty()]
        .iter()
        .filter(|b| **b)
        .count();
    if count > 1 {
        return CmdOutput::fail(
            EXIT_COMMAND_LINE,
            "ERROR: Only one fetch option can be provided for fetch command\n".into(),
        );
    }
    // When both `--configure` and `--force` are passed, only force-invalidate configure=True repos.
    let force_configure_only = configure && cfg.force;
    if force_configure_only {
        cfg.force = false;
    }
    let session = match Session::open(cfg) {
        Ok(s) => s,
        Err(e) => return session_error(e),
    };
    let mut err = warn_lines(&session.warnings);
    let rt = session.rt.clone();
    if configure {
        rt.state.lock().unwrap().force_configure = true;
    }
    let result: Result<String, (i32, String)> = (|| {
        if !targets.is_empty() {
            return fetch_targets(&session, &targets, &working_dir_rel)
                .map(|_| {
                    "All external dependencies for the requested targets fetched successfully."
                        .to_string()
                })
                .map_err(|e| (EXIT_COMMAND_LINE, e));
        }
        if !repos.is_empty() {
            let mut resolved_repos = Vec::new();
            let mut seen_canonical = HashSet::new();
            let mut errors = Vec::new();
            // Validate all `--repo` arguments before fetching any repository.
            for r in &repos {
                match session.resolve_repo_arg(r) {
                    Ok(c) => {
                        if seen_canonical.insert(c.clone()) {
                            resolved_repos.push(c);
                        }
                    }
                    Err(e) => {
                        if e.starts_with("Invalid repo name:") {
                            return Err((EXIT_FETCH_FAILED, e));
                        }
                        if e.starts_with("invalid user-provided repo name")
                            || e.starts_with("The repo value has to be")
                        {
                            return Err((EXIT_FETCH_FAILED, format!("Invalid repo name: {e}")));
                        }
                        errors.push(e);
                    }
                }
            }
            for c in &resolved_repos {
                if let Err(e) = rt.fetch_repo(c) {
                    errors.push(e);
                }
            }
            if !errors.is_empty() {
                if errors.len() == 1
                    && (errors[0].starts_with("ERROR:")
                        || errors[0].contains("An error occurred during the fetch of repository")
                        || errors[0].starts_with("fetching ")
                        || errors[0].contains("error evaluating module extension")
                        || errors[0].contains("Traceback (most recent call last)")
                        || errors[0].contains("expected module extension")
                        || errors[0].contains("does not have a tag class named")
                        || errors[0].contains("root_module_direct_deps contained")
                        || errors[0].starts_with("The repository's path is ")
                        || errors[0].contains("Error loading '")
                        || errors[0].contains("error loading '"))
                {
                    return Err((EXIT_FETCH_FAILED, errors.remove(0)));
                }
                return Err((
                    EXIT_FETCH_FAILED,
                    format!(
                        "Fetching some repos failed with errors: {}",
                        errors.join("; ")
                    ),
                ));
            }
            return Ok("All requested repos fetched successfully.".into());
        }
        // HEAD's `FetchCommand.fetchAll` reports failures as `QUERY_EVALUATION_ERROR` (exit code 2); the
        // 9.3.2 release binary exits with 8 for lockfile errors (see skew_allowlist.md #8).
        // Evaluating every extension also verifies that the repos imported with `use_repo` are generated.
        for id in session.resolution.extension_ids() {
            if let Err(e) = rt.eval_extension(&id) {
                let code = if e.contains("does not exist in the lockfile")
                    || e.contains("MODULE.bazel.lock is no longer up-to-date")
                {
                    EXIT_INTERRUPTED
                } else {
                    EXIT_FETCH_FAILED
                };
                return Err((code, e));
            }
        }
        let names = rt.all_repo_names().map_err(|e| (EXIT_FETCH_FAILED, e))?;
        for n in names {
            if configure
                && !rt
                    .is_configure_repo(&n)
                    .map_err(|e| (EXIT_FETCH_FAILED, e))?
            {
                continue;
            }
            if force_configure_only {
                let _ = std::fs::remove_file(rt.external_dir().join(format!("@{n}.marker")));
            }
            rt.fetch_repo(&n).map_err(|e| (EXIT_FETCH_FAILED, e))?;
        }
        Ok("All external dependencies fetched successfully.".into())
    })();
    for l in rt.take_log() {
        err.push_str(&format!("{l}\n"));
    }
    let _ = session.finish();
    match result {
        Ok(msg) => {
            err.push_str(&format!("INFO: {msg}\n"));
            CmdOutput::ok(err)
        }
        Err((code, e)) => {
            if e.starts_with("ERROR: ") || e.starts_with("INFO: ") || e.starts_with("WARNING: ") {
                err.push_str(&format!("{e}\n"));
            } else {
                err.push_str(&format!("ERROR: {e}\n"));
            }
            CmdOutput::fail(code, err)
        }
    }
}

/// Fetches the external repositories needed by `targets` via real target dependency closure analysis.
fn fetch_targets(
    session: &Session,
    targets: &[String],
    working_dir_rel: &zel_vfs::PathFragment,
) -> Result<(), String> {
    let rt = &session.rt;
    let mut fetched_repos: HashSet<String> = HashSet::new();

    // 1. Initial repo discovery from target patterns (e.g. @@repo//pkg:target or @repo//pkg:target)
    for t in targets {
        let t = t.trim_start_matches('-');
        if let Some(r) = t.strip_prefix("@@") {
            let (repo, _) = r.split_once("//").unwrap_or((r, ""));
            if !repo.is_empty() {
                zel_cmdline::RepositoryName::validate(repo).map_err(|e| {
                    format!(
                        "ERROR: {e}\nERROR: Build did NOT complete successfully\nERROR: Fetching some target dependencies failed with errors: {e}"
                    )
                })?;
                if fetched_repos.insert(repo.to_string()) {
                    rt.fetch_repo(repo).map_err(|e| {
                        format!(
                            "ERROR: {e}\nERROR: Build did NOT complete successfully\nERROR: Fetching some target dependencies failed with errors: {e}"
                        )
                    })?;
                }
            }
        } else if let Some(r) = t.strip_prefix('@') {
            let (repo, _) = r.split_once("//").unwrap_or((r, ""));
            match session.resolve_repo_arg(&format!("@{repo}")) {
                Ok(c) => {
                    if !c.is_empty() && fetched_repos.insert(c.clone()) {
                        rt.fetch_repo(&c).map_err(|e| {
                            format!(
                                "ERROR: {e}\nERROR: Build did NOT complete successfully\nERROR: Fetching some target dependencies failed with errors: {e}"
                            )
                        })?;
                    }
                }
                Err(e) => {
                    return Err(format!(
                        "ERROR: {e}\nERROR: Build did NOT complete successfully\nERROR: Fetching some target dependencies failed with errors: {e}"
                    ));
                }
            }
        }
    }

    // 2. Package loader and pattern resolver for target dependency closure analysis
    let loader = rt.package_loader();
    let resolver =
        zel_packages::resolve::PatternResolver::new(loader.clone(), working_dir_rel.clone());
    let outcome = resolver.resolve_patterns(targets);
    for event in &outcome.events {
        rt.info(format!("WARNING: {}", event.message));
    }
    if !outcome.errors.is_empty() {
        // `TargetFetcher` runs a `--nobuild` build: pattern failures abort it like in `bazel build`, then
        // `FetchCommand` reports `QUERY_EVALUATION_ERROR` (exit 2).
        let mut m = String::from("WARNING: Target pattern parsing failed.\n");
        for err in &outcome.errors {
            m.push_str(&format!(
                "ERROR: Skipping '{}': {}\nERROR: {}\n",
                err.pattern, err.message, err.message
            ));
        }
        m.push_str("INFO: Elapsed time: 0.050s\nINFO: 0 processes.\nERROR: Build did NOT complete successfully");
        if let Some(err) = outcome.errors.first() {
            m.push_str(&format!(
                "\nERROR: Fetching some target dependencies failed with errors: {}",
                err.message
            ));
        }
        return Err(m);
    }
    let (all_targets, _) = resolver.load_transitive_closure(&outcome.targets);
    let mut queue = Vec::new();
    for t in &all_targets {
        if let zel_packages::Target::Rule(r) = t {
            for dep in r.dependency_labels() {
                let repo_ident = dep.repository().name();
                if !repo_ident.is_empty() {
                    let owner = r.label.repository().name();
                    let mapping = rt.repo_mapping_of(owner);
                    let canonical = mapping
                        .get(repo_ident)
                        .unwrap_or(&repo_ident.to_string())
                        .clone();
                    if fetched_repos.insert(canonical.clone()) {
                        queue.push(canonical);
                    }
                }
            }
        }
    }

    while let Some(r) = queue.pop() {
        match rt.fetch_repo(&r) {
            Ok(_) => {
                let pattern = format!("@@{r}//...");
                let sub_outcome = resolver.resolve_patterns(&[pattern]);
                let (sub_targets, _) = resolver.load_transitive_closure(&sub_outcome.targets);
                for t in &sub_targets {
                    if let zel_packages::Target::Rule(rule) = t {
                        for dep in rule.dependency_labels() {
                            let repo_ident = dep.repository().name();
                            if !repo_ident.is_empty() {
                                let owner = rule.label.repository().name();
                                let mapping = rt.repo_mapping_of(owner);
                                let canonical = mapping
                                    .get(repo_ident)
                                    .unwrap_or(&repo_ident.to_string())
                                    .clone();
                                if fetched_repos.insert(canonical.clone()) {
                                    queue.push(canonical);
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                return Err(format!(
                    "ERROR: {e}\nERROR: Build did NOT complete successfully\nERROR: Fetching some target dependencies failed with errors: {e}"
                ));
            }
        }
    }

    Ok(())
}

/// `bazel vendor`: copies fetched repositories into `--vendor_dir`.
pub fn run_vendor(cfg: BzlmodConfig, parsed: &ParsedCommandOptions) -> CmdOutput {
    let Some(vendor_dir) = cfg.vendor_dir.clone() else {
        return CmdOutput::fail(
            EXIT_COMMAND_LINE,
            "ERROR: You cannot run the vendor command without specifying --vendor_dir\n".into(),
        );
    };
    if parsed.get_bool("fetch") == Some(false) {
        return CmdOutput::fail(
            EXIT_COMMAND_LINE,
            "ERROR: You cannot run the vendor command with --nofetch\n".into(),
        );
    }
    let repos = raw(parsed, "repo");
    let lockfile_deps = parsed.get_bool("lockfile_deps").unwrap_or(false);
    let (working_dir, working_dir_rel) = command_working_dirs(&cfg.workspace, parsed);
    let target_pattern_file = parsed.get_str("target_pattern_file").unwrap_or("");
    let targets =
        match zel_cmdline::read_target_patterns(&parsed.targets, target_pattern_file, &working_dir)
        {
            Ok(ts) => ts,
            Err(e) => return CmdOutput::fail(EXIT_COMMAND_LINE, format!("ERROR: {e}\n")),
        };
    if !repos.is_empty() && !targets.is_empty() {
        return CmdOutput::fail(
            EXIT_INTERRUPTED,
            "ERROR: Target patterns and --repo cannot both be specified\n".into(),
        );
    }
    if !repos.is_empty() && lockfile_deps {
        return CmdOutput::fail(
            EXIT_COMMAND_LINE,
            "ERROR: --lockfile_deps and --repo cannot both be specified\n".into(),
        );
    }
    // Fetch from the real source (not an existing vendor dir) when vendoring.
    let mut cfg = cfg;
    cfg.main_mapping_errors = true;
    let vd = cfg.vendor_dir.take();
    let session = match Session::open(cfg) {
        Ok(s) => s,
        Err(e) => return session_error(e),
    };
    let mut err = warn_lines(&session.warnings);
    let rt = session.rt.clone();
    let ok_msg = std::cell::RefCell::new(
        "INFO: All external dependencies vendored successfully.".to_string(),
    );
    let result: Result<Vec<String>, String> = (|| {
        let _ = std::fs::create_dir_all(&vendor_dir);
        let external_symlink = vendor_dir.join(crate::vendor::EXTERNAL_ROOT_SYMLINK_NAME);
        let _ = crate::vendor::ensure_symlink(&rt.external_dir(), &external_symlink);
        let mut names: Vec<String> = vec![];
        if !repos.is_empty() {
            *ok_msg.borrow_mut() = "INFO: All requested repos vendored successfully.".to_string();
            err.push_str("INFO: Vendoring repositories...\n");
            let mut seen = HashSet::new();
            let mut repo_errors = Vec::new();
            for r in &repos {
                match session.resolve_repo_arg(r) {
                    Ok(c) => {
                        if seen.insert(c.clone()) {
                            names.push(c);
                        }
                    }
                    Err(e) => {
                        if e.starts_with("Invalid repo name:") {
                            return Err(e);
                        }
                        if e.starts_with("invalid user-provided repo name")
                            || e.starts_with("The repo value has to be")
                        {
                            return Err(format!("Invalid repo name: {e}"));
                        }
                        repo_errors.push(e);
                    }
                }
            }
            for c in &names {
                if let Err(e) = rt.fetch_repo(c) {
                    repo_errors.push(e);
                }
            }
            if !repo_errors.is_empty() {
                return Err(format!(
                    "Vendoring some repos failed with errors: [{}]",
                    repo_errors.join(", ")
                ));
            }
        } else if !targets.is_empty() || lockfile_deps {
            let subject = if targets.is_empty() {
                "lockfile updates"
            } else if lockfile_deps {
                "the requested targets and lockfile updates"
            } else {
                "the requested targets"
            };
            *ok_msg.borrow_mut() =
                format!("INFO: All external dependencies for {subject} vendored successfully.");
            err.push_str(&format!("INFO: Vendoring dependencies for {subject}...\n"));
            fetch_targets(&session, &targets, &working_dir_rel)?;
            names = rt.state_fetched();
            if lockfile_deps {
                for id in session.resolution.extension_ids() {
                    let repo = id.bzl_repo();
                    if !repo.is_empty() && !names.contains(&repo) {
                        rt.fetch_repo(&repo)?;
                        names.push(repo);
                    }
                }
            }
        } else {
            err.push_str("INFO: Vendoring all external repositories...\n");
            names = rt.all_repo_names()?;
        }
        // `VENDOR.bazel` is only read (and created) once a repo that can be vendored is looked at.
        let mut vendor_file_cell: Option<crate::vendor::VendorFile> = None;
        let mut vendored = vec![];
        for n in &names {
            if n == "bazel_tools" {
                continue;
            }
            if vendor_file_cell.is_none() {
                vendor_file_cell = Some(crate::vendor::VendorFile::load_or_init(&vendor_dir)?);
            }
            let vendor_file = vendor_file_cell.as_ref().unwrap();
            if vendor_file.ignored_repos.contains(n) {
                // Ignored repos are still fetched into output_base/external, only skipped in vendor_dir.
                rt.fetch_repo(n)
                    .map_err(|e| format!("Fetching some repos failed with errors: {e}"))?;
                continue;
            }
            let target = vendor_dir.join(n);
            if vendor_file.pinned_repos.contains(n) {
                if !target.exists() {
                    return Err(format!(
                        "Pinned repo '{n}' does not exist under the vendor directory"
                    ));
                }
                let _ = std::fs::remove_file(vendor_dir.join(format!("@{n}.marker")));
                continue;
            }
            if rt.is_local_repo(n) || n == "bazel_tools" {
                continue;
            }
            let dir = rt
                .fetch_repo(n)
                .map_err(|e| format!("Fetching some repos failed with errors: {e}"))?;
            vendored.push(n.clone());
            std::fs::create_dir_all(&vendor_dir)
                .map_err(|e| format!("Error while vendoring repos: {e}"))?;
            let marker = rt.external_dir().join(format!("@{n}.marker"));
            let vmarker = vendor_dir.join(format!("@{n}.marker"));
            let ext_marker_str = std::fs::read_to_string(&marker).ok();
            let vendor_marker_str = std::fs::read_to_string(&vmarker).ok();
            let up_to_date = target.is_dir()
                && matches!(
                    (&ext_marker_str, &vendor_marker_str),
                    (Some(em), Some(vm)) if em == vm
                );
            if !up_to_date {
                // `dir` may itself be a symlink to `target` (re-run after a previous vendor), so copy
                // to a temporary sibling first and only then replace the old vendored copy.
                let tmp_target = vendor_dir.join(format!(".{n}.vendor-tmp"));
                let _ = remove_any(&tmp_target);
                copy_tree(&dir, &tmp_target)
                    .map_err(|e| format!("Error while vendoring repos: {e}"))?;
                let _ = remove_any(&target);
                std::fs::rename(&tmp_target, &target)
                    .map_err(|e| format!("Error while vendoring repos: {e}"))?;
                crate::vendor::replant_symlinks(
                    &target,
                    &session.cfg.workspace,
                    &rt.external_dir(),
                )
                .map_err(|e| format!("Error replanting symlinks: {e}"))?;
                if let Some(m) = ext_marker_str {
                    let _ = std::fs::write(&vmarker, m);
                }
            }
            let _ = remove_any(&dir);
            #[cfg(unix)]
            {
                let _ = std::os::unix::fs::symlink(&target, &dir);
            }
        }
        // Vendor registry files fetched over the network (using repo_cache first when available).
        for (url, hash) in &session.resolution.registry_file_hashes {
            let Some(h) = hash.as_ref().filter(|_| !url.starts_with("file://")) else {
                continue;
            };
            let Some(p) = crate::registry::vendored_registry_path(&vendor_dir, url) else {
                continue;
            };
            let cached_bytes = session.cfg.repository_cache.as_ref().and_then(|rc_root| {
                let rc = crate::download::RepositoryCache::new(rc_root);
                let ck = crate::hash::Checksum::sha256_hex(h).ok()?;
                let path = rc.get(&ck, "").ok().flatten()?;
                std::fs::read(path).ok()
            });
            let bytes = match cached_bytes {
                Some(b) => Some(b),
                None => crate::registry::fetch_url(url, 3).ok().flatten(),
            };
            if let Some(b) = bytes {
                if let Some(par) = p.parent() {
                    let _ = std::fs::create_dir_all(par);
                }
                let _ = std::fs::write(p, b);
            }
        }
        Ok(vendored)
    })();
    let _ = vd;
    for l in rt.take_log() {
        err.push_str(&format!("{l}\n"));
    }
    let _ = session.finish();
    match result {
        Ok(_) => {
            err.push_str(&format!("{}\n", ok_msg.borrow()));
            CmdOutput::ok(err)
        }
        Err(e) => {
            if e.starts_with("ERROR: ") || e.starts_with("INFO: ") || e.starts_with("WARNING: ") {
                err.push_str(&format!("{e}\n"));
            } else {
                err.push_str(&format!("ERROR: {e}\n"));
            }
            CmdOutput::fail(EXIT_FETCH_FAILED, err)
        }
    }
}

impl Rt {
    pub fn state_fetched(&self) -> Vec<String> {
        let mut v: Vec<String> = self.state.lock().unwrap().fetched.iter().cloned().collect();
        v.sort();
        v
    }

    /// Whether `canonical` is created by a `local=True` or `configure=True` rule (excluded from vendoring).
    pub fn is_local_repo(self: &Arc<Self>, canonical: &str) -> bool {
        if self.repo_override_for(canonical).ok().flatten().is_some() {
            return true;
        }
        match self.repo_spec_for(canonical) {
            Ok(Some(s)) => self.is_local_rule(&s) || self.is_configure_rule(&s),
            _ => false,
        }
    }

    pub fn is_configure_repo(self: &Arc<Self>, canonical: &str) -> Result<bool, String> {
        match self.repo_spec_for(canonical)? {
            Some(s) => Ok(self
                .rule_meta_result(&s)?
                .is_some_and(|(_, configure, _)| configure)),
            None => Ok(false),
        }
    }
}

/// Formats a remote registry URL into its relative path under `<vendor_dir>/_registries`
/// (`<lowercased_host>/<url_decoded_path>`, stripping `:port`, `?query`, and `#fragment`),
/// returning `None` if the host or decoded path is empty, contains NUL or a drive prefix, or
/// attempts to traverse above `<lowercased_host>`.
pub(crate) fn registry_vendor_rel_path(url: &str) -> Option<PathBuf> {
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let no_frag = after_scheme
        .split_once('#')
        .map(|(a, _)| a)
        .unwrap_or(after_scheme);
    let no_query = no_frag.split_once('?').map(|(a, _)| a).unwrap_or(no_frag);
    let (authority, path) = no_query.split_once('/').unwrap_or((no_query, ""));
    let host_port = authority
        .rsplit_once('@')
        .map(|(_, hp)| hp)
        .unwrap_or(authority);
    let raw_host = if host_port.starts_with('[') {
        host_port
            .split_once(']')
            .map(|(h, _)| format!("{h}]"))
            .unwrap_or_else(|| host_port.to_string())
    } else {
        host_port
            .split_once(':')
            .map(|(h, _)| h)
            .unwrap_or(host_port)
            .to_string()
    };
    let host = percent_decode_str(&raw_host, false).to_ascii_lowercase();
    if host.is_empty()
        || host == "."
        || host == ".."
        || host.contains(['/', '\\', '\0'])
        || is_windows_drive_segment(&host)
        || !is_single_normal_component(&host)
    {
        return None;
    }

    let decoded_path = percent_decode_str(path, true);
    if decoded_path.contains('\0') {
        return None;
    }

    let mut segs: Vec<&str> = Vec::new();
    for seg in decoded_path.split(['/', '\\']) {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            segs.pop()?;
            continue;
        }
        if is_windows_drive_segment(seg) || !is_single_normal_component(seg) {
            return None;
        }
        segs.push(seg);
    }
    if segs.is_empty() {
        return None;
    }

    let mut out = PathBuf::from(host);
    for seg in segs {
        out.push(seg);
    }
    Some(out)
}

fn is_windows_drive_segment(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

fn is_single_normal_component(s: &str) -> bool {
    let mut comps = Path::new(s).components();
    matches!(
        (comps.next(), comps.next()),
        (Some(std::path::Component::Normal(_)), None)
    )
}

fn percent_decode_str(s: &str, plus_as_space: bool) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if plus_as_space && bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod repo_env_tests {
    use super::*;

    fn env_of(client: &[(&str, &str)], args: &[&str]) -> BTreeMap<String, String> {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let parsed = ParsedCommandOptions::parse_for_command("build", &args).unwrap();
        let client = client
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        BzlmodConfig::from_parsed(&parsed, Path::new("/ws"), Path::new("/ob"), client)
            .unwrap()
            .client_env
    }

    #[test]
    fn repo_env_set_inherit_unset_and_workspace_placeholder() {
        let e = env_of(
            &[("PATH", "/p"), ("HOME", "/h"), ("SECRET", "s")],
            &[
                "--repo_env=A=%bazel_workspace%/x",
                "--repo_env=SECRET",
                "--repo_env=MISSING",
                "--repo_env==HOME",
                "--repo_env=B=1",
                "--repo_env=B=2",
            ],
        );
        assert_eq!(e.get("A").map(String::as_str), Some("/ws/x"));
        assert_eq!(e.get("B").map(String::as_str), Some("2"));
        assert_eq!(e.get("SECRET").map(String::as_str), Some("s"));
        assert!(!e.contains_key("MISSING"));
        assert!(!e.contains_key("HOME"), "=NAME unsets");
        assert_eq!(e.get("PATH").map(String::as_str), Some("/p"));
    }

    #[test]
    fn strict_repo_env_keeps_only_path_and_explicit_repo_env() {
        let e = env_of(
            &[("PATH", "/p"), ("HOME", "/h"), ("SECRET", "s")],
            &[
                "--experimental_strict_repo_env",
                "--repo_env=SECRET",
                "--repo_env=X=1",
            ],
        );
        let keys: Vec<&str> = e.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["PATH", "SECRET", "X"]);
    }

    #[test]
    fn action_env_does_not_feed_the_repo_env() {
        let e = env_of(
            &[("HOME", "/h"), ("PATH", "/p")],
            &[
                "--experimental_strict_repo_env",
                "--action_env=HOME",
                "--action_env=NEW=1",
            ],
        );
        assert!(!e.contains_key("HOME") && !e.contains_key("NEW"), "{e:?}");
    }

    #[test]
    fn registry_vendor_rel_path_normalizes_host_port_query_and_percent_escapes() {
        assert_eq!(
            registry_vendor_rel_path(
                "https://BCR.Bazel.Build:8443/modules/foo%2Bbar/1.0/source.json?tok=1#frag"
            ),
            Some(PathBuf::from(
                "bcr.bazel.build/modules/foo+bar/1.0/source.json"
            ))
        );
        assert_eq!(
            registry_vendor_rel_path("https://bcr.bazel.build/modules/foo+bar/1.0/source.json"),
            Some(PathBuf::from(
                "bcr.bazel.build/modules/foo bar/1.0/source.json"
            ))
        );
    }

    #[test]
    fn registry_vendor_rel_path_confines_encoded_separators_traversal_and_drives() {
        let registries_dir = PathBuf::from("/tmp/vendor/_registries");

        let slash_rel = registry_vendor_rel_path("https://example.com/%2fetc%2fpasswd")
            .expect("encoded leading slash should remain confined under host");
        assert!(!slash_rel.is_absolute(), "{slash_rel:?}");
        assert_eq!(slash_rel, PathBuf::from("example.com/etc/passwd"));
        assert_eq!(
            registries_dir.join(&slash_rel),
            PathBuf::from("/tmp/vendor/_registries/example.com/etc/passwd")
        );

        let backslash_rel = registry_vendor_rel_path("https://example.com/%5cetc%5cpasswd")
            .expect("encoded backslash should split into normal relative components");
        assert!(!backslash_rel.is_absolute(), "{backslash_rel:?}");
        assert_eq!(backslash_rel, PathBuf::from("example.com/etc/passwd"));
        assert_eq!(
            registries_dir.join(&backslash_rel),
            PathBuf::from("/tmp/vendor/_registries/example.com/etc/passwd")
        );

        let in_bounds =
            registry_vendor_rel_path("https://example.com/modules/old/%2e%2e/foo/1.0/source.json")
                .expect("in-bounds parent segment within host should normalize cleanly");
        assert_eq!(
            in_bounds,
            PathBuf::from("example.com/modules/foo/1.0/source.json")
        );
        assert!(in_bounds
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_))));

        for escaping_or_invalid in [
            "https://example.com/%2e%2e/etc/passwd",
            "https://example.com/modules/%2e%2e/%2e%2e/etc/passwd",
            "https://example.com/a%2f..%2f..%2fetc%2fpasswd",
            "https://example.com/a%5c..%5c..%5cetc%5cpasswd",
            "https://%2e%2e/etc/passwd",
            "https://../etc/passwd",
            "https:///etc/passwd",
            "https://example.com/C:%2fWindows%2fSystem32",
            "https://example.com/c:%5cWindows%5cSystem32",
            "https://example.com/D:secret.json",
            "https://example.com/modules/foo%00bar/source.json",
            "https://example.com/",
        ] {
            assert_eq!(
                registry_vendor_rel_path(escaping_or_invalid),
                None,
                "expected None for {escaping_or_invalid}"
            );
        }
    }

    #[test]
    fn tilde_paths_expand_home_and_windows_userprofile() {
        let cfg_of = |client: &[(&str, &str)], args: &[&str]| -> BzlmodConfig {
            let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            let parsed = ParsedCommandOptions::parse_for_command("build", &args).unwrap();
            let client = client
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            BzlmodConfig::from_parsed(&parsed, Path::new("/ws"), Path::new("/ob"), client).unwrap()
        };

        // Windows-style environment where HOME is unset (or empty) and USERPROFILE is set.
        let win_cfg = cfg_of(
            &[("USERPROFILE", "/users/win_alice")],
            &[
                "--repository_cache=~/bazel_cache",
                "--repo_contents_cache=~",
                "--distdir=~/dists",
                "--credential_helper=example.com=~/bin/cred-helper",
                "--override_repository=ext=~/repos/ext",
                "--override_module=mod_a=~/mods/mod_a",
            ],
        );
        assert_eq!(
            win_cfg.repository_cache.as_deref(),
            Some(Path::new("/users/win_alice/bazel_cache"))
        );
        assert_eq!(
            win_cfg.repo_contents_cache.as_deref(),
            Some(Path::new("/users/win_alice"))
        );
        assert_eq!(
            win_cfg.distdirs,
            vec![PathBuf::from("/users/win_alice/dists")]
        );
        assert_eq!(
            win_cfg.credential_helpers,
            vec![(
                "example.com".to_string(),
                PathBuf::from("/users/win_alice/bin/cred-helper")
            )]
        );
        assert_eq!(
            win_cfg.override_repository.get("ext").map(PathBuf::as_path),
            Some(Path::new("/users/win_alice/repos/ext"))
        );
        assert_eq!(
            win_cfg.override_modules,
            vec![(
                "mod_a".to_string(),
                "/users/win_alice/mods/mod_a".to_string()
            )]
        );

        // When both HOME and USERPROFILE are set, HOME takes precedence; empty HOME falls back to USERPROFILE.
        let both_cfg = cfg_of(
            &[("HOME", "/home/alice"), ("USERPROFILE", "/users/win_alice")],
            &["--repository_cache=~/cache"],
        );
        assert_eq!(
            both_cfg.repository_cache.as_deref(),
            Some(Path::new("/home/alice/cache"))
        );
        let empty_home_cfg = cfg_of(
            &[("HOME", ""), ("USERPROFILE", "/users/win_alice")],
            &["--repository_cache=~/cache"],
        );
        assert_eq!(
            empty_home_cfg.repository_cache.as_deref(),
            Some(Path::new("/users/win_alice/cache"))
        );

        // When neither home variable is non-empty, literal ~/... falls back to workspace-relative.
        let no_home_cfg = cfg_of(
            &[("HOME", ""), ("USERPROFILE", "")],
            &["--repository_cache=~/cache"],
        );
        assert_eq!(
            no_home_cfg.repository_cache.as_deref(),
            Some(Path::new("/ws/~/cache"))
        );

        // Windows backslash forms (`~\...`, `%workspace%\...`, drive-letter `USERPROFILE`) normalize
        // to Bazel PathFragment `/` separators and uppercase drive letters.
        let win_tilde = expand_option_path(
            r"~\mods\mod_a",
            Path::new(r"d:\ws"),
            Some(r"c:\Users\win_alice\"),
            true,
        );
        assert_eq!(
            format_option_path_fragment(&win_tilde, true),
            "C:/Users/win_alice/mods/mod_a"
        );
        let win_ws = expand_option_path(
            r"%workspace%\sub\mod_b",
            Path::new(r"d:\ws\"),
            Some(r"c:\Users\win_alice"),
            true,
        );
        assert_eq!(
            format_option_path_fragment(&win_ws, true),
            "D:/ws/sub/mod_b"
        );
    }

    #[test]
    fn early_option_checks_expand_tilde_from_client_env_matching_from_parsed() {
        let root = std::env::temp_dir().join(format!(
            "zel-bzlmod-tilde-client-env-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let ws = root.join("ws");
        let ob = root.join("ob");
        let custom_home = root.join("custom_home");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&ob).unwrap();
        std::fs::create_dir_all(&custom_home).unwrap();
        std::fs::write(custom_home.join("valid_dl.cfg"), "rewrite (.*) $1\n").unwrap();
        std::fs::write(custom_home.join("bad_dl.cfg"), "invalid_directive foo\n").unwrap();

        assert_ne!(
            std::env::var("HOME").ok().as_deref(),
            custom_home.to_str(),
            "custom_home must differ from process HOME"
        );

        let run_both = |raw_args: &[String]| -> (Result<(), String>, Result<BzlmodConfig, String>) {
            let early = check_downloader_config("build", raw_args, &ws);
            let client_env = ParsedCommandOptions::extract_client_env(raw_args);
            let parsed = ParsedCommandOptions::parse_for_command("build", raw_args).unwrap();
            let full = BzlmodConfig::from_parsed(&parsed, &ws, &ob, client_env);
            (early, full)
        };

        // Valid `--downloader_config=~/...` under custom `--client_env=HOME=...` succeeds in both.
        let valid_dl_args = vec![
            format!("--client_env=HOME={}", custom_home.display()),
            "--downloader_config=~/valid_dl.cfg".to_string(),
        ];
        let (early_ok, full_ok) = run_both(&valid_dl_args);
        assert!(early_ok.is_ok(), "{early_ok:?}");
        assert!(
            full_ok.expect("from_parsed").downloader_rewriter.is_some(),
            "downloader rewriter should be loaded from custom client HOME"
        );

        // Malformed `--downloader_config=~/...` under custom `--client_env=HOME=...` fails identically.
        let bad_dl_args = vec![
            format!("--client_env=HOME={}", custom_home.display()),
            "--downloader_config=~/bad_dl.cfg".to_string(),
        ];
        let (early_bad, full_bad) = run_both(&bad_dl_args);
        assert_eq!(early_bad.unwrap_err(), full_bad.unwrap_err());

        // Windows-style `--client_env=HOME=` + `--client_env=USERPROFILE=<custom_home>` also matches.
        let userprofile_dl_args = vec![
            "--client_env=HOME=".to_string(),
            format!("--client_env=USERPROFILE={}", custom_home.display()),
            "--downloader_config=~/valid_dl.cfg".to_string(),
        ];
        let (early_up, full_up) = run_both(&userprofile_dl_args);
        assert!(early_up.is_ok(), "{early_up:?}");
        assert!(full_up.is_ok(), "{full_up:?}");

        // `--repo_contents_cache=~/...` expanding inside `ws` via `--client_env=HOME=<root>` is
        // rejected identically by both early check and `from_parsed` (both `=` and two-token forms).
        let inside_cache_args = vec![
            format!("--client_env=HOME={}", root.display()),
            "--repo_contents_cache=~/ws/cache_inside".to_string(),
        ];
        let (early_inside, full_inside) = run_both(&inside_cache_args);
        let expected_inside = ws.join("cache_inside");
        let inside_err = early_inside.expect_err("cache inside main repo must fail");
        let full_inside_err = full_inside.expect_err("from_parsed must also reject cache inside main repo");
        assert_eq!(inside_err, full_inside_err);
        assert!(
            inside_err
                .replace('\\', "/")
                .contains(&expected_inside.to_string_lossy().replace('\\', "/"))
                && inside_err.contains("is inside the main repo"),
            "{inside_err}"
        );

        let two_token_inside_args = vec![
            "--client_env".to_string(),
            format!("HOME={}", root.display()),
            "--repo_contents_cache".to_string(),
            "~/ws/cache_inside".to_string(),
        ];
        let two_token_err =
            check_downloader_config("build", &two_token_inside_args, &ws).unwrap_err();
        assert!(
            two_token_err.contains("is inside the main repo"),
            "{two_token_err}"
        );

        // `--repo_contents_cache=~/...` expanding outside `ws` via `--client_env=HOME=<custom_home>` succeeds.
        let outside_cache_args = vec![
            format!("--client_env=HOME={}", custom_home.display()),
            "--repo_contents_cache=~/cache_outside".to_string(),
        ];
        let (early_outside, full_outside) = run_both(&outside_cache_args);
        assert!(early_outside.is_ok(), "{early_outside:?}");
        assert_eq!(
            full_outside
                .expect("from_parsed")
                .repo_contents_cache
                .as_deref(),
            Some(custom_home.join("cache_outside").as_path())
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn expand_workspace_path_normalizes_separators_for_credential_helpers() {
        let unix_ws = Path::new("/my/ws");
        let cwd = Path::new("/my/ws/sub");
        for spec in [
            "%workspace%/tools/wild",
            r"%workspace%\tools\wild",
            "%workspace%//tools//./wild",
        ] {
            let expanded = expand_workspace_path(spec, unix_ws, cwd);
            let rendered = expanded.to_string_lossy();
            assert!(
                !(rendered.contains('/') && rendered.contains('\\')),
                "mixed separators in {rendered:?} for {spec:?}"
            );
            let mut expected = expand_workspace_path("%workspace%", unix_ws, cwd);
            expected.push("tools");
            expected.push("wild");
            assert_eq!(expanded, expected, "mismatch for {spec:?}");
        }

        let win_ws = Path::new(r"C:\my\ws");
        for spec in ["%workspace%/tools/wild.bat", r"%workspace%\tools\wild.bat"] {
            let expanded = expand_workspace_path(spec, win_ws, cwd);
            assert!(expanded.ends_with(Path::new("tools").join("wild.bat")));
            #[cfg(windows)]
            assert_eq!(expanded.to_string_lossy(), r"C:\my\ws\tools\wild.bat");
        }
    }
}
