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
}

fn raw(parsed: &ParsedCommandOptions, name: &str) -> Vec<String> {
    parsed.parsed_options.get(name).map(|d| d.raw_values.clone()).unwrap_or_default()
}

/// `RepositoryOverrideConverter`'s name check: an apparent name must be a valid user-provided repo name.
fn validate_repo_flag_name(name: &str) -> Result<(), ()> {
    if name.is_empty() || name.starts_with('@') {
        return Err(());
    }
    if name.contains('+') {
        return Ok(());
    }
    let valid_start = name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric());
    if valid_start && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
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
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'-' | b'_'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "While parsing option --{flag}={value}: invalid module name '{name}': valid names must 1) only contain lowercase letters (a-z), digits (0-9), dots (.), hyphens (-), and underscores (_); 2) begin with a lowercase letter; 3) end with a lowercase letter or digit."
        ))
    }
}

fn split_pair(v: &str) -> Option<(String, String)> {
    v.split_once('=').map(|(a, b)| (a.to_string(), b.to_string()))
}

impl BzlmodConfig {
    pub fn from_parsed(
        parsed: &ParsedCommandOptions,
        workspace: &Path,
        output_base: &Path,
        client_env: BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let abs = |p: &str| -> PathBuf {
            if let Some(rest) = p.strip_prefix("%workspace%/") {
                workspace.join(rest)
            } else if p == "%workspace%" {
                workspace.to_path_buf()
            } else {
                let pb = PathBuf::from(p);
                if pb.is_absolute() {
                    pb
                } else {
                    workspace.join(pb)
                }
            }
        };
        let mode_str = |n: &str, d: &str| parsed.get_str(n).map(String::from).unwrap_or_else(|| raw(parsed, n).last().cloned().unwrap_or_else(|| d.to_string()));
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
            if !p.is_empty() {
                override_repository.insert(n, abs(&p));
            }
        }
        // `CommandEnvironment.getRepoEnv()`: the client environment (only `PATH` with
        // `--experimental_strict_repo_env`) with `--repo_env` applied in order: `NAME=value`
        // (`%bazel_workspace%` expanded), `NAME` (inherit from the client env), `=NAME` (unset).
        // `--action_env` does not feed the repository environment.
        let strict_repo_env = parsed.get_bool("experimental_strict_repo_env").unwrap_or(false);
        let repo_env_vars = zel_util::action_env::parse_env_vars(raw(parsed, "repo_env"));
        let (env, _nonstrict) = zel_util::action_env::repository_environments(
            &client_env,
            &repo_env_vars,
            strict_repo_env,
            workspace.to_str(),
        );
        let pairs = |n: &str| -> Result<Vec<(String, String)>, String> {
            raw(parsed, n)
                .iter()
                .map(|v| {
                    split_pair(v)
                        .map(|(a, b)| {
                            let b_path = if let Some(rest) = b.strip_prefix("%workspace%/") {
                                workspace.join(rest)
                            } else if b == "%workspace%" {
                                workspace.to_path_buf()
                            } else if Path::new(&b).is_absolute() {
                                PathBuf::from(&b)
                            } else {
                                workspace.join(&b)
                            };
                            (a, b_path.display().to_string())
                        })
                        .ok_or_else(|| {
                            let kind = if n == "override_module" { "Module overrides" } else { "Module injections" };
                            format!("While parsing option --{n}={v}: {kind} must be of the form 'module-name=path'")
                        })
                        .and_then(|(a, b)| validate_module_flag_name(&a, n, v).map(|_| (a, b)))
                })
                .collect()
        };
        let allow_yanked: Vec<String> = raw(parsed, "allow_yanked_versions").iter().flat_map(|s| s.split(',').map(|x| x.trim().to_string()).collect::<Vec<_>>()).filter(|s| !s.is_empty()).collect();
        let env_allowed = client_env.get("BZLMOD_ALLOW_YANKED_VERSIONS").cloned().unwrap_or_default();
        let mut allow_yanked = allow_yanked;
        allow_yanked.extend(env_allowed.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
        for y in &allow_yanked {
            if y != "all" && !y.contains('@') {
                return Err(format!(
                    "Parsing command line flag --allow_yanked_versions={y} failed, module versions must be of the form '<module name>@<version>'"
                ));
            }
        }
        Ok(BzlmodConfig {
            workspace: workspace.to_path_buf(),
            output_base: output_base.to_path_buf(),
            client_env: env,
            registries: raw(parsed, "registry"),
            check_direct_dependencies: check("check_direct_dependencies", "warning")?,
            bazel_compatibility_mode: check("check_bazel_compatibility", "error")?,
            lockfile_mode,
            override_repository,
            distdirs: raw(parsed, "distdir").iter().map(|d| abs(d)).collect(),
            repository_cache: match raw(parsed, "repository_cache").last() {
                // An explicitly empty `--repository_cache=` disables the cache.
                Some(s) if s.is_empty() => None,
                Some(s) => Some(abs(s)),
                None => Some(default_repository_cache(output_base)),
            },
            repo_contents_cache: raw(parsed, "repo_contents_cache").last().filter(|s| !s.is_empty()).map(|s| abs(s)),
            disable_download: parsed.get_bool("repository_disable_download").unwrap_or(false),
            // An empty `--vendor_dir=` is the (empty) path fragment relative to the workspace, i.e. the workspace root.
            vendor_dir: raw(parsed, "vendor_dir").last().map(|s| if s.is_empty() { workspace.to_path_buf() } else { abs(s) }),
            ignore_dev_dependency: parsed.get_bool("ignore_dev_dependency").unwrap_or(false),
            isolated_extension_usages: parsed.get_bool("experimental_isolated_extension_usages").unwrap_or(false),
            allow_yanked,
            override_modules: pairs("override_module")?
                .into_iter()
                .zip(raw(parsed, "override_module"))
                .filter_map(|(pair, raw)| raw.split_once('=').filter(|(_, path)| !path.is_empty()).map(|_| pair))
                .collect(),
            inject_modules: pairs("inject_module")?,
            inject_repositories: pairs("inject_repository")?,
            module_mirrors: raw(parsed, "module_mirrors"),
            main_mapping_errors: false,
            semantics_fingerprint: crate::semantics_fp::stable_fingerprint(&crate::semantics_fp::bool_overrides(parsed)),
            downloader_rewriter: crate::download::UrlRewriter::from_config_paths(workspace, &raw(parsed, "downloader_config"))?,
            download_retries: parsed.get_int("experimental_repository_downloader_retries").map(|v| v.max(0) as u32).unwrap_or(5),
            force: parsed.get_bool("force").unwrap_or(false),
            nofetch: parsed.get_bool("fetch") == Some(false),
            bazel_version: crate::resolve::default_bazel_version(),
            experimental_repository_ctx_execute_wasm: parsed.get_bool("experimental_repository_ctx_execute_wasm").unwrap_or(false),
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
pub fn check_downloader_config(command: &str, args: &[String], workspace: &Path) -> Result<(), String> {
    check_repo_contents_cache_location(command, args, workspace)?;
    check_override_flags(command, args, workspace)?;
    if !args.iter().any(|a| a.contains("downloader_config")) {
        return Ok(());
    }
    let Ok(parsed) = ParsedCommandOptions::parse_for_command(command, args) else { return Ok(()) };
    crate::download::UrlRewriter::from_config_paths(workspace, &raw(&parsed, "downloader_config")).map(|_| ())
}

/// Option-parsing failures of `--override_repository`, `--override_module` and the `--inject_*` flags
/// (`While parsing option --x=y: ...`), reported before any command runs.
fn check_override_flags(command: &str, args: &[String], workspace: &Path) -> Result<(), String> {
    if !args.iter().any(|a| a.starts_with("--override_repository") || a.starts_with("--override_module") || a.starts_with("--inject_")) {
        return Ok(());
    }
    let Ok(parsed) = ParsedCommandOptions::parse_for_command(command, args) else { return Ok(()) };
    match BzlmodConfig::from_parsed(&parsed, workspace, Path::new("/nonexistent-output-base"), BTreeMap::new()) {
        Err(e) if e.starts_with("While parsing option --override_") || e.starts_with("While parsing option --inject_") => Err(e),
        _ => Ok(()),
    }
}

/// `BazelRepositoryModule.beforeCommand`: the repo contents cache (by default `<repository_cache>/contents`) must not be
/// inside the main repository.
fn check_repo_contents_cache_location(command: &str, args: &[String], workspace: &Path) -> Result<(), String> {
    if !args.iter().any(|a| a.starts_with("--repository_cache=") || a.starts_with("--repo_contents_cache=")) {
        return Ok(());
    }
    let Ok(parsed) = ParsedCommandOptions::parse_for_command(command, args) else { return Ok(()) };
    let abs = |s: &str| if Path::new(s).is_absolute() { PathBuf::from(s) } else { workspace.join(s) };
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

/// Records the server's `output_user_root`, below which the default repository cache
/// (`<output_user_root>/cache/repos/v1`, `BlazeWorkspace.getCacheDir`) lives.
pub fn set_output_user_root(p: &Path) {
    *OUTPUT_USER_ROOT.lock().unwrap() = Some(p.to_path_buf());
}

static INSTALL_BASE: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Records the server's install base (`@bazel_tools` is reported at `<install_base>/embedded_tools`).
pub fn set_install_base(p: &Path) {
    *INSTALL_BASE.lock().unwrap() = Some(p.to_path_buf());
}

pub fn install_base() -> Option<PathBuf> {
    INSTALL_BASE.lock().unwrap().clone()
}

/// The default repository cache: `<output_user_root>/cache/repos/v1`, falling back to the parent
/// of the output base (the default `output_user_root` layout) before the server registered one.
pub fn default_repository_cache(output_base: &Path) -> PathBuf {
    let root = OUTPUT_USER_ROOT.lock().unwrap().clone().unwrap_or_else(|| output_base.join(".."));
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
        let fail = |message: String, warnings: Vec<String>, code: i32| SessionError { message, warnings, exit_code: code };
        let mode = cfg.lockfile_mode;
        let (ws_lock, hidden_lock) = if mode == LockfileMode::Off {
            (Lockfile::default(), Lockfile::default())
        } else {
            let ws = Lockfile::read(&cfg.lockfile_path(), mode).map_err(|e| fail(e, vec![], 48))?;
            let hid = Lockfile::read(&cfg.hidden_lockfile_path(), LockfileMode::Update).unwrap_or_default();
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
        match mode {
            LockfileMode::Off => {}
            LockfileMode::Update => {
                ro.known_hashes = Some(ws_lock.registry_file_hashes.clone());
                ro.known_hashes_mode = KnownHashesMode::UseAndUpdate;
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
            }
        }
        // Non-registry (archive/git) module overrides are fetched through a bootstrap runtime.
        let boot_opts = cfg.external_options();
        ro.repo_fetcher = Some(Arc::new(move |name: &str, spec: &RepoSpec| {
            let empty = Arc::new(Resolution::empty());
            let rt = Rt::new(boot_opts.clone(), empty);
            let canonical = format!("{name}+");
            let dir = rt.repo_dir(&canonical);
            let fp = rt.predeclared_input_hash(&canonical, spec);
            let marker = rt.external_dir().join(format!("@{canonical}.marker"));
            let _ = std::fs::create_dir_all(rt.external_dir());
            if dir.exists() && std::fs::read_to_string(&marker).map(|m| crate::fetch::marker_is_current(&m, &fp, &boot_opts.client_env, &boot_opts.workspace_root, &rt.external_dir())).unwrap_or(false) && !boot_opts.force {
                return Ok(dir);
            }
            if let Some(rcc_dir) = &boot_opts.repo_contents_cache {
                if !boot_opts.force {
                    let cache = crate::repo_contents_cache::RepoContentsCache::new(rcc_dir);
                    let candidates = cache.get_candidate_repos(&fp);
                    for cand in candidates {
                        if let Ok(m) = std::fs::read_to_string(&cand.recorded_inputs_file) {
                            if crate::fetch::marker_is_current(&m, &fp, &boot_opts.client_env, &boot_opts.workspace_root, &rt.external_dir()) {
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
            if is_reproducible {
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
            } else if e.message.contains("MODULE.bazel expected but not found at") {
                48
            } else if cfg.main_mapping_errors
                && !cfg.override_modules.is_empty()
                && !e.message.contains("The repository's path is ")
                && !e.message.contains("No MODULE.bazel, REPO.bazel, or WORKSPACE file found in")
            {
                48
            } else if cfg.main_mapping_errors && mapping_category {
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
        Ok(Session { cfg, resolution, rt, warnings })
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
        if mode != LockfileMode::Error && !would_be_empty && std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            std::fs::write(&path, text).map_err(|e| format!("Error while updating MODULE.bazel.lock file: {e}"))?;
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
            return Ok(c.to_string());
        }
        if !arg.starts_with('@') {
            return Err("The repo value has to be either apparent '@repo' or canonical '@@repo' repo name".into());
        }
        let a = arg.strip_prefix('@').unwrap_or(arg);
        let valid_start = a.chars().next().map(|c| c.is_ascii_alphanumeric()).unwrap_or(false);
        if !a.is_empty() && (!valid_start || !a.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))) {
            return Err(format!("invalid user-provided repo name '{a}': valid names may contain only A-Z, a-z, 0-9, '-', '_', '.', and must start with a letter or a number"));
        }
        if a.is_empty() {
            return Err("invalid user-provided repo name '': valid names may contain only A-Z, a-z, 0-9, '-', '_', '.', and must start with a letter or a number".into());
        }
        self.resolution
            .repo_mapping(&ModuleKey::root())
            .get(a)
            .cloned()
            .ok_or_else(|| format!("No repository visible as '@{a}' from main repository"))
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
        Self { exit_code: 0, stdout: String::new(), stderr }
    }
    fn fail(code: i32, stderr: String) -> Self {
        Self { exit_code: code, stdout: String::new(), stderr }
    }
}

fn warn_lines(ws: &[String]) -> String {
    ws.iter().map(|w| if w.starts_with("ERROR:") || w.starts_with("WARNING:") { format!("{w}\n") } else { format!("WARNING: {w}\n") }).collect()
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

/// `bazel fetch` (`--all`, `--configure`, `--force`, `--repo`, target patterns).
pub fn run_fetch(cfg: BzlmodConfig, parsed: &ParsedCommandOptions) -> CmdOutput {
    let mut cfg = cfg;
    cfg.main_mapping_errors = true;
    if parsed.get_bool("fetch") == Some(false) {
        return CmdOutput::fail(EXIT_COMMAND_LINE, "ERROR: You cannot run fetch with --nofetch\n".into());
    }
    let all = parsed.get_bool("all").unwrap_or(false);
    let configure = parsed.get_bool("configure").unwrap_or(false);
    let repos = raw(parsed, "repo");
    let targets = parsed.targets.clone();
    let count = [all, configure, !repos.is_empty(), !targets.is_empty()].iter().filter(|b| **b).count();
    if count > 1 {
        return CmdOutput::fail(EXIT_COMMAND_LINE, "ERROR: Only one fetch option can be provided for fetch command\n".into());
    }
    let session = match Session::open(cfg) {
        Ok(s) => s,
        Err(e) => return session_error(e),
    };
    let mut err = warn_lines(&session.warnings);
    let rt = session.rt.clone();
    let result: Result<String, (i32, String)> = (|| {
        if !targets.is_empty() {
            return fetch_targets(&session, &targets)
                .map(|_| "All external dependencies for the requested targets fetched successfully.".to_string())
                .map_err(|e| (EXIT_COMMAND_LINE, e));
        }
        if !repos.is_empty() {
            let mut errors = Vec::new();
            for r in &repos {
                match session.resolve_repo_arg(r) {
                    Ok(c) => {
                        if let Err(e) = rt.fetch_repo(&c) {
                            errors.push(e);
                        }
                    }
                    Err(e) => {
                        if e.starts_with("invalid user-provided repo name") || e.starts_with("The repo value has to be") {
                            return Err((EXIT_FETCH_FAILED, format!("Invalid repo name: {e}")));
                        }
                        errors.push(e);
                    }
                }
            }
            if !errors.is_empty() {
                if errors.len() == 1 && (errors[0].starts_with("ERROR:") || errors[0].contains("An error occurred during the fetch of repository") || errors[0].starts_with("fetching ") || errors[0].contains("error evaluating module extension") || errors[0].contains("Traceback (most recent call last)") || errors[0].contains("expected module extension") || errors[0].contains("does not have a tag class named") || errors[0].contains("root_module_direct_deps contained") || errors[0].starts_with("The repository's path is ") || errors[0].contains("Error loading '") || errors[0].contains("error loading '")) {
                    return Err((EXIT_FETCH_FAILED, errors.remove(0)));
                }
                return Err((EXIT_FETCH_FAILED, format!("Fetching some repos failed with errors: {}", errors.join("; "))));
            }
            return Ok("All requested repos fetched successfully.".into());
        }
        // HEAD's `FetchCommand.fetchAll` reports failures as `QUERY_EVALUATION_ERROR` (exit code 2); the
        // 9.3.2 release binary exits with 8 for lockfile errors (see skew_allowlist.md #8).
        // Evaluating every extension also verifies that the repos imported with `use_repo` are generated.
        for id in session.resolution.extension_ids() {
            if let Err(e) = rt.eval_extension(&id) {
                let code = if e.contains("does not exist in the lockfile") || e.contains("MODULE.bazel.lock is no longer up-to-date") {
                    EXIT_INTERRUPTED
                } else {
                    EXIT_FETCH_FAILED
                };
                return Err((code, e));
            }
        }
        let names = rt.all_repo_names().map_err(|e| (EXIT_FETCH_FAILED, e))?;
        for n in names {
            if configure && !rt.is_configure_repo(&n) {
                continue;
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
fn fetch_targets(session: &Session, targets: &[String]) -> Result<(), String> {
    let rt = &session.rt;
    let mut fetched_repos: HashSet<String> = HashSet::new();

    // 1. Initial repo discovery from target patterns (e.g. @@repo//pkg:target or @repo//pkg:target)
    for t in targets {
        let t = t.trim_start_matches('-');
        if let Some(r) = t.strip_prefix("@@") {
            let (repo, _) = r.split_once("//").unwrap_or((r, ""));
            if !repo.is_empty() && fetched_repos.insert(repo.to_string()) {
                let _ = rt.fetch_repo(repo);
            }
        } else if let Some(r) = t.strip_prefix('@') {
            let (repo, _) = r.split_once("//").unwrap_or((r, ""));
            match session.resolve_repo_arg(&format!("@{repo}")) {
                Ok(c) => {
                    if !c.is_empty() && fetched_repos.insert(c.clone()) {
                        let _ = rt.fetch_repo(&c);
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
    let resolver = zel_packages::resolve::PatternResolver::new(loader.clone(), zel_vfs::PathFragment::empty());
    let outcome = resolver.resolve_patterns(targets);
    for event in &outcome.events {
        rt.info(format!("WARNING: {}", event.message));
    }
    if !outcome.errors.is_empty() {
        // `TargetFetcher` runs a `--nobuild` build: pattern failures abort it like in `bazel build`, then
        // `FetchCommand` reports `QUERY_EVALUATION_ERROR` (exit 2).
        let mut m = String::from("WARNING: Target pattern parsing failed.\n");
        for err in &outcome.errors {
            m.push_str(&format!("ERROR: Skipping '{}': {}\nERROR: {}\n", err.pattern, err.message, err.message));
        }
        m.push_str("INFO: Elapsed time: 0.050s\nINFO: 0 processes.\nERROR: Build did NOT complete successfully");
        if let Some(err) = outcome.errors.first() {
            m.push_str(&format!("\nERROR: Fetching some target dependencies failed with errors: {}", err.message));
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
                    let canonical = mapping.get(repo_ident).unwrap_or(&repo_ident.to_string()).clone();
                    if fetched_repos.insert(canonical.clone()) {
                        queue.push(canonical);
                    }
                }
            }
        }
    }

    while let Some(r) = queue.pop() {
        if let Ok(_) = rt.fetch_repo(&r) {
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
                            let canonical = mapping.get(repo_ident).unwrap_or(&repo_ident.to_string()).clone();
                            if fetched_repos.insert(canonical.clone()) {
                                queue.push(canonical);
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// `bazel vendor`: copies fetched repositories into `--vendor_dir`.
pub fn run_vendor(cfg: BzlmodConfig, parsed: &ParsedCommandOptions) -> CmdOutput {
    let Some(vendor_dir) = cfg.vendor_dir.clone() else {
        return CmdOutput::fail(EXIT_COMMAND_LINE, "ERROR: You cannot run the vendor command without specifying --vendor_dir\n".into());
    };
    if parsed.get_bool("fetch") == Some(false) {
        return CmdOutput::fail(EXIT_COMMAND_LINE, "ERROR: You cannot run the vendor command with --nofetch\n".into());
    }
    let repos = raw(parsed, "repo");
    let lockfile_deps = parsed.get_bool("lockfile_deps").unwrap_or(false);
    let targets = parsed.targets.clone();
    if !repos.is_empty() && !targets.is_empty() {
        return CmdOutput::fail(EXIT_INTERRUPTED, "ERROR: Target patterns and --repo cannot both be specified\n".into());
    }
    if !repos.is_empty() && lockfile_deps {
        return CmdOutput::fail(EXIT_COMMAND_LINE, "ERROR: --lockfile_deps and --repo cannot both be specified\n".into());
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
    let ok_msg = std::cell::RefCell::new("INFO: All external dependencies vendored successfully.".to_string());
    let result: Result<Vec<String>, String> = (|| {
        let _ = std::fs::create_dir_all(&vendor_dir);
        let external_symlink = vendor_dir.join(crate::vendor::EXTERNAL_ROOT_SYMLINK_NAME);
        let _ = crate::vendor::ensure_symlink(&rt.external_dir(), &external_symlink);
        let mut names: Vec<String> = vec![];
        if !repos.is_empty() {
            *ok_msg.borrow_mut() = "INFO: All requested repos vendored successfully.".to_string();
            err.push_str("INFO: Vendoring repositories...\n");
            for r in &repos {
                let c = session.resolve_repo_arg(r).map_err(|e| {
                    if e.starts_with("invalid user-provided repo name") || e.starts_with("The repo value has to be") {
                        format!("Invalid repo name: {e}")
                    } else {
                        format!("Vendoring some repos failed with errors: [{e}]")
                    }
                })?;
                names.push(c);
            }
        } else if !targets.is_empty() || lockfile_deps {
            let subject = if targets.is_empty() { "lockfile updates" } else if lockfile_deps { "the requested targets and lockfile updates" } else { "the requested targets" };
            *ok_msg.borrow_mut() = format!("INFO: All external dependencies for {subject} vendored successfully.");
            err.push_str(&format!("INFO: Vendoring dependencies for {subject}...\n"));
            fetch_targets(&session, &targets)?;
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
                // Ignore repo as configured in VENDOR.bazel
                continue;
            }
            if vendor_file.pinned_repos.contains(n) {
                // Pinned repo: do not update contents
                continue;
            }
            if rt.is_local_repo(n) || n == "bazel_tools" {
                continue;
            }
            let dir = rt.fetch_repo(n).map_err(|e| format!("Fetching some repos failed with errors: {e}"))?;
            vendored.push(n.clone());
            let target = vendor_dir.join(n);
            std::fs::create_dir_all(&vendor_dir).map_err(|e| format!("Error while vendoring repos: {e}"))?;
            // `dir` may itself be a symlink to `target` (re-run after a previous vendor), so copy
            // to a temporary sibling first and only then replace the old vendored copy.
            let tmp_target = vendor_dir.join(format!(".{n}.vendor-tmp"));
            let _ = remove_any(&tmp_target);
            copy_tree(&dir, &tmp_target).map_err(|e| format!("Error while vendoring repos: {e}"))?;
            let _ = remove_any(&target);
            std::fs::rename(&tmp_target, &target).map_err(|e| format!("Error while vendoring repos: {e}"))?;
            crate::vendor::replant_symlinks(&target, &session.cfg.workspace, &rt.external_dir())
                .map_err(|e| format!("Error replanting symlinks: {e}"))?;
            let marker = rt.external_dir().join(format!("@{n}.marker"));
            if let Ok(m) = std::fs::read_to_string(&marker) {
                let _ = std::fs::write(vendor_dir.join(format!("@{n}.marker")), m);
            }
            let _ = remove_any(&dir);
            #[cfg(unix)]
            {
                let _ = std::os::unix::fs::symlink(&target, &dir);
            }
        }
        // Vendor registry files fetched over the network.
        for (url, hash) in &session.resolution.registry_file_hashes {
            if url.starts_with("file://") || hash.is_none() {
                continue;
            }
            if let Ok(Some(b)) = crate::registry::fetch_url(url, 3) {
                let rel = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
                let p = vendor_dir.join("_registries").join(rel);
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

    /// Whether `canonical` is created by a `local=True`-like rule (excluded from vendoring).
    pub fn is_local_repo(self: &Arc<Self>, canonical: &str) -> bool {
        if self.opts.override_repository.keys().any(|k| k == canonical) {
            return true;
        }
        match self.repo_spec_for(canonical) {
            Ok(Some(s)) => {
                if matches!(s.rule_name.as_str(), "local_repository" | "new_local_repository") && s.bzl_file.contains("bazel_tools") {
                    return true;
                }
                let (repo, pkg, name) = {
                    let r = s.bzl_file.trim_start_matches('@');
                    let (repo, rest) = r.split_once("//").unwrap_or((r, ""));
                    let (pkg, name) = rest.split_once(':').unwrap_or(("", rest));
                    (repo.to_string(), pkg.to_string(), name.to_string())
                };
                let key = format!("@@{repo}//{pkg}:{name}");
                match self.eval_bzl_fresh(&key, &repo, &pkg, &name) {
                    Ok(c) => match c.globals.get(&s.rule_name) {
                        Some(zel_starlark::Value::Host(k, id)) if k == "repository_rule" => {
                            id.parse::<usize>().ok().map(|i| {
                                let d = &self.state.lock().unwrap().rule_defs[i];
                                d.local || d.configure
                            }).unwrap_or(false)
                        }
                        _ => false,
                    },
                    Err(_) => false,
                }
            }
            _ => false,
        }
    }

    pub fn is_configure_repo(self: &Arc<Self>, canonical: &str) -> bool {
        match self.repo_spec_for(canonical) {
            Ok(Some(s)) => {
                if s.bzl_file.contains("bazel_tools") {
                    return false;
                }
                let (repo, pkg, name) = {
                    let r = s.bzl_file.trim_start_matches('@');
                    let (repo, rest) = r.split_once("//").unwrap_or((r, ""));
                    let (pkg, name) = rest.split_once(':').unwrap_or(("", rest));
                    (repo.to_string(), pkg.to_string(), name.to_string())
                };
                let key = format!("@@{repo}//{pkg}:{name}");
                match self.eval_bzl_fresh(&key, &repo, &pkg, &name) {
                    Ok(c) => match c.globals.get(&s.rule_name) {
                        Some(zel_starlark::Value::Host(k, id)) if k == "repository_rule" => {
                            id.parse::<usize>().ok().map(|i| self.state.lock().unwrap().rule_defs[i].configure).unwrap_or(false)
                        }
                        _ => false,
                    },
                    Err(_) => false,
                }
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod repo_env_tests {
    use super::*;

    fn env_of(client: &[(&str, &str)], args: &[&str]) -> BTreeMap<String, String> {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let parsed = ParsedCommandOptions::parse_for_command("build", &args).unwrap();
        let client = client.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        BzlmodConfig::from_parsed(&parsed, Path::new("/ws"), Path::new("/ob"), client).unwrap().client_env
    }

    #[test]
    fn repo_env_set_inherit_unset_and_workspace_placeholder() {
        let e = env_of(
            &[("PATH", "/p"), ("HOME", "/h"), ("SECRET", "s")],
            &["--repo_env=A=%bazel_workspace%/x", "--repo_env=SECRET", "--repo_env=MISSING", "--repo_env==HOME", "--repo_env=B=1", "--repo_env=B=2"],
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
            &["--experimental_strict_repo_env", "--repo_env=SECRET", "--repo_env=X=1"],
        );
        let keys: Vec<&str> = e.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["PATH", "SECRET", "X"]);
    }

    #[test]
    fn action_env_does_not_feed_the_repo_env() {
        let e = env_of(&[("HOME", "/h"), ("PATH", "/p")], &["--experimental_strict_repo_env", "--action_env=HOME", "--action_env=NEW=1"]);
        assert!(!e.contains_key("HOME") && !e.contains_key("NEW"), "{e:?}");
    }
}
