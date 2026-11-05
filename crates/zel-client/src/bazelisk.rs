// Copyright 2026 The Zel Authors. All rights reserved.
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

//! Built-in Bazelisk-compatible version resolver, downloader, cache manager, and wrapper launcher.
//!
//! Supports:
//! - `USE_ZEL_VERSION` and `USE_BAZEL_VERSION` environment variables
//! - `.zelversion` and `.bazelversion` workspace files
//! - `USE_ZEL_FALLBACK_VERSION` and `USE_BAZEL_FALLBACK_VERSION` (`error`, `warn:<ver>`, `silent:<ver>`, `<ver>`)
//! - `ZEL_SKIP_WRAPPER` and `BAZELISK_SKIP_WRAPPER` recursion guards
//! - Local sibling binary discovery (`zel-<ver>-<os>-<arch>`, `bazel-<ver>-<os>-<arch>`, `zel-real`, `bazel-real`)
//! - Local cache lookup and automatic release downloads (`ZEL_HOME` / `BAZELISK_HOME`, `ZEL_BASE_URL` / `BAZELISK_BASE_URL`)
//! - SHA-256 integrity verification (`<artifact>.sha256` and `ZEL_VERIFY_SHA256` / `BAZELISK_VERIFY_SHA256`)
//! - Workspace wrapper script delegation (`ZEL_WRAPPER` / `BAZEL_WRAPPER`, `tools/zel`, `tools/bazel` with `ZEL_REAL` and `BAZEL_REAL`)

use std::cmp::Ordering;
use std::fmt;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use zel_util::sha256;
use zel_vfs::{FileSystem, PathFragment};

pub const DEFAULT_ZEL_BASE_URL: &str = "https://github.com/scottpledger/zel/releases/download";

/// Describes where the required Zel/Bazel version specification originated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionSource {
    UseZelVersionEnv,
    UseBazelVersionEnv,
    ZelVersionFile(PathFragment),
    BazelVersionFile(PathFragment),
    FallbackEnv(String),
    DefaultCurrent,
}

impl fmt::Display for VersionSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VersionSource::UseZelVersionEnv => write!(f, "$USE_ZEL_VERSION"),
            VersionSource::UseBazelVersionEnv => write!(f, "$USE_BAZEL_VERSION"),
            VersionSource::ZelVersionFile(p) => write!(f, "{p}"),
            VersionSource::BazelVersionFile(p) => write!(f, "{p}"),
            VersionSource::FallbackEnv(var) => write!(f, "${var}"),
            VersionSource::DefaultCurrent => write!(f, "running binary default"),
        }
    }
}

/// Parsed and normalized version requirement for a workspace invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedVersionSpec {
    pub raw_spec: String,
    pub version: String,
    pub fork: Option<String>,
    pub is_path: bool,
    pub source: VersionSource,
    pub warn_message: Option<String>,
}

/// Environment and platform configuration used by the built-in Bazelisk launcher.
#[derive(Debug, Clone)]
pub struct BazeliskEnv {
    pub use_zel_version: Option<String>,
    pub use_bazel_version: Option<String>,
    pub fallback_version: Option<(String, String)>,
    pub skip_wrapper: bool,
    pub bazelisk_skip_wrapper: bool,
    pub base_url: Option<String>,
    pub home_dir: Option<PathFragment>,
    pub no_download: bool,
    pub verify_sha256: Option<String>,
    pub wrapper_rel_path: Option<String>,
    pub current_exe: Option<PathFragment>,
    pub current_version: String,
    pub invoked_as_bazel: bool,
    pub os_name: String,
    pub arch_name: String,
    pub raw_arch_name: String,
    pub user_home: Option<PathFragment>,
    pub xdg_cache_home: Option<PathFragment>,
    pub inherit_stdio: bool,
}

impl Default for BazeliskEnv {
    fn default() -> Self {
        let (os_name, arch_name, raw_arch_name) = host_platform_names();
        Self {
            use_zel_version: None,
            use_bazel_version: None,
            fallback_version: None,
            skip_wrapper: false,
            bazelisk_skip_wrapper: false,
            base_url: None,
            home_dir: None,
            no_download: false,
            verify_sha256: None,
            wrapper_rel_path: None,
            current_exe: None,
            current_version: zel_server::build_label(),
            invoked_as_bazel: false,
            os_name,
            arch_name,
            raw_arch_name,
            user_home: None,
            xdg_cache_home: None,
            inherit_stdio: false,
        }
    }
}

impl BazeliskEnv {
    /// Captures the current process environment and platform configuration.
    pub fn from_process_env() -> Self {
        let (os_name, arch_name, raw_arch_name) = host_platform_names();
        let non_empty_var = |name: &str| -> Option<String> {
            std::env::var(name)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let is_truthy_var = |name: &str| -> bool {
            non_empty_var(name)
                .map(|v| !matches!(v.to_ascii_lowercase().as_str(), "0" | "false" | "no"))
                .unwrap_or(false)
        };

        let fallback_version = non_empty_var("USE_ZEL_FALLBACK_VERSION")
            .map(|v| ("USE_ZEL_FALLBACK_VERSION".to_string(), v))
            .or_else(|| {
                non_empty_var("USE_BAZEL_FALLBACK_VERSION")
                    .map(|v| ("USE_BAZEL_FALLBACK_VERSION".to_string(), v))
            });

        let zel_skip_wrapper = is_truthy_var("ZEL_SKIP_WRAPPER");
        let bazelisk_skip_wrapper = is_truthy_var("BAZELISK_SKIP_WRAPPER");
        let base_url = non_empty_var("ZEL_BASE_URL").or_else(|| non_empty_var("BAZELISK_BASE_URL"));
        let home_dir = non_empty_var("ZEL_HOME")
            .or_else(|| non_empty_var("BAZELISK_HOME"))
            .map(|p| PathFragment::create(&p));
        let no_download = is_truthy_var("ZEL_NO_DOWNLOAD") || is_truthy_var("BAZELISK_NO_DOWNLOAD");
        let verify_sha256 =
            non_empty_var("ZEL_VERIFY_SHA256").or_else(|| non_empty_var("BAZELISK_VERIFY_SHA256"));
        let wrapper_rel_path =
            non_empty_var("ZEL_WRAPPER").or_else(|| non_empty_var("BAZEL_WRAPPER"));

        let current_exe = std::env::current_exe()
            .ok()
            .and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)))
            .map(|p| PathFragment::create(&p.to_string_lossy()));

        let user_home = non_empty_var("HOME")
            .or_else(|| non_empty_var("USERPROFILE"))
            .map(|p| PathFragment::create(&p));
        let xdg_cache_home = non_empty_var("XDG_CACHE_HOME").map(|p| PathFragment::create(&p));

        Self {
            use_zel_version: non_empty_var("USE_ZEL_VERSION"),
            use_bazel_version: non_empty_var("USE_BAZEL_VERSION"),
            fallback_version,
            skip_wrapper: zel_skip_wrapper,
            bazelisk_skip_wrapper,
            base_url,
            home_dir,
            no_download,
            verify_sha256,
            wrapper_rel_path,
            current_exe,
            current_version: zel_server::build_label(),
            invoked_as_bazel: zel_server::version_product_prefix() == "bazel",
            os_name,
            arch_name,
            raw_arch_name,
            user_home,
            xdg_cache_home,
            inherit_stdio: true,
        }
    }

    /// Merges `.zeliskrc` and `.bazeliskrc` files from `workspace_dir` and `user_home`
    /// (process environment variables take precedence over workspace rc files, which take
    /// precedence over user home rc files).
    pub fn with_rc_files(&self, fs: &dyn FileSystem, workspace_dir: &PathFragment) -> Self {
        let mut merged = self.clone();
        let mut rc_paths = vec![
            workspace_dir.get_relative_str(".zeliskrc"),
            workspace_dir.get_relative_str(".bazeliskrc"),
        ];
        if let Some(ref home) = self.user_home {
            rc_paths.push(home.get_relative_str(".zeliskrc"));
            rc_paths.push(home.get_relative_str(".bazeliskrc"));
        }

        for rc_path in rc_paths {
            let Ok(bytes) = fs.read_file(&rc_path) else {
                continue;
            };
            // Track whether a higher-precedence source (process env or earlier rc file) already
            // specified a version override so a lower-precedence file (e.g. ~/.zeliskrc) cannot
            // override a higher-precedence file (e.g. workspace .bazeliskrc).
            let version_locked_by_higher_source =
                merged.use_zel_version.is_some() || merged.use_bazel_version.is_some();
            let content = String::from_utf8_lossy(&bytes);
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                let Some((raw_k, raw_v)) = trimmed.split_once('=') else {
                    continue;
                };
                let k = raw_k.trim();
                let v = raw_v
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .trim()
                    .to_string();
                if v.is_empty() {
                    continue;
                }
                let is_truthy = !matches!(v.to_ascii_lowercase().as_str(), "0" | "false" | "no");
                match k {
                    "USE_ZEL_VERSION"
                        if !version_locked_by_higher_source && merged.use_zel_version.is_none() =>
                    {
                        merged.use_zel_version = Some(v);
                    }
                    "USE_BAZEL_VERSION"
                        if !version_locked_by_higher_source
                            && merged.use_zel_version.is_none()
                            && merged.use_bazel_version.is_none() =>
                    {
                        merged.use_bazel_version = Some(v);
                    }
                    "USE_ZEL_FALLBACK_VERSION" | "USE_BAZEL_FALLBACK_VERSION"
                        if merged.fallback_version.is_none() =>
                    {
                        merged.fallback_version = Some((k.to_string(), v));
                    }
                    "ZEL_BASE_URL" | "BAZELISK_BASE_URL" if merged.base_url.is_none() => {
                        merged.base_url = Some(v);
                    }
                    "ZEL_HOME" | "BAZELISK_HOME" if merged.home_dir.is_none() => {
                        merged.home_dir = Some(PathFragment::create(&v));
                    }
                    "ZEL_NO_DOWNLOAD" | "BAZELISK_NO_DOWNLOAD" if !merged.no_download => {
                        merged.no_download = is_truthy;
                    }
                    "ZEL_VERIFY_SHA256" | "BAZELISK_VERIFY_SHA256"
                        if merged.verify_sha256.is_none() =>
                    {
                        merged.verify_sha256 = Some(v);
                    }
                    "ZEL_WRAPPER" | "BAZEL_WRAPPER" if merged.wrapper_rel_path.is_none() => {
                        merged.wrapper_rel_path = Some(v);
                    }
                    "ZEL_SKIP_WRAPPER" if !merged.skip_wrapper => {
                        merged.skip_wrapper = is_truthy;
                    }
                    "BAZELISK_SKIP_WRAPPER" if !merged.bazelisk_skip_wrapper => {
                        merged.bazelisk_skip_wrapper = is_truthy;
                    }
                    _ => {}
                }
            }
        }
        merged
    }

    pub fn exe_extension(&self) -> &'static str {
        if self.os_name == "windows" {
            ".exe"
        } else {
            ""
        }
    }

    /// Returns the primary cache directory (`$ZEL_HOME`, `$BAZELISK_HOME`, or OS cache dir).
    pub fn primary_cache_dir(&self) -> PathFragment {
        if let Some(ref home) = self.home_dir {
            return home.clone();
        }
        if let Some(ref xdg) = self.xdg_cache_home {
            return xdg.get_relative_str("zel");
        }
        if let Some(ref home) = self.user_home {
            if self.os_name == "darwin" {
                return home.get_relative_str("Library/Caches/zel");
            }
            return home.get_relative_str(".cache/zel");
        }
        PathFragment::create("/tmp/zel-cache")
    }

    /// Returns candidate cache directories to search for already-downloaded binaries.
    pub fn candidate_cache_dirs(&self) -> Vec<PathFragment> {
        let mut dirs = vec![self.primary_cache_dir()];
        if self.home_dir.is_none() {
            if let Some(ref xdg) = self.xdg_cache_home {
                dirs.push(xdg.get_relative_str("bazelisk"));
            } else if let Some(ref home) = self.user_home {
                if self.os_name == "darwin" {
                    dirs.push(home.get_relative_str("Library/Caches/bazelisk"));
                } else {
                    dirs.push(home.get_relative_str(".cache/bazelisk"));
                }
            }
        }
        dirs
    }
}

/// Action determined by the built-in Bazelisk launcher before running a CLI command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LauncherAction {
    /// The currently running `zel` process matches the required version and no workspace
    /// wrapper script needs to intercept the invocation.
    RunInProcess { spec: ResolvedVersionSpec },
    /// Delegate execution to another `zel`/`bazel` binary or a workspace wrapper script.
    ExecExternal {
        program: PathFragment,
        args: Vec<String>,
        env_vars: Vec<(String, String)>,
        spec: ResolvedVersionSpec,
    },
}

pub fn host_platform_names() -> (String, String, String) {
    let os_name = match std::env::consts::OS {
        "macos" | "darwin" => "darwin",
        "linux" => "linux",
        "windows" => "windows",
        other => other,
    }
    .to_string();

    let raw_arch = std::env::consts::ARCH.to_string();
    let arch_name = match raw_arch.as_str() {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "arm64",
        other => other,
    }
    .to_string();

    (os_name, arch_name, raw_arch)
}

/// Strips an optional leading `'v'` or `'V'` prefix when followed by an ASCII digit.
pub fn normalize_version(version: &str) -> &str {
    let trimmed = version.trim();
    if let Some(rest) = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
    {
        if rest.as_bytes().first().is_some_and(|b| b.is_ascii_digit()) {
            return rest;
        }
    }
    trimmed
}

/// Compares two version strings using natural/semantic version ordering.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let na = normalize_version(a);
    let nb = normalize_version(b);
    let (core_a, pre_a) = na.split_once('-').unwrap_or((na, ""));
    let (core_b, pre_b) = nb.split_once('-').unwrap_or((nb, ""));

    let segs_a: Vec<&str> = core_a.split('.').collect();
    let segs_b: Vec<&str> = core_b.split('.').collect();
    let max_len = segs_a.len().max(segs_b.len());

    for i in 0..max_len {
        let sa = segs_a.get(i).copied().unwrap_or("0");
        let sb = segs_b.get(i).copied().unwrap_or("0");
        let ord = match (sa.parse::<u64>(), sb.parse::<u64>()) {
            (Ok(va), Ok(vb)) => va.cmp(&vb),
            _ => sa.cmp(sb),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }

    match (pre_a.is_empty(), pre_b.is_empty()) {
        (true, true) => Ordering::Equal,
        // Stable release sorts higher than prerelease of same core version
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => pre_a.cmp(pre_b),
    }
}

/// Reads the first non-empty, non-comment line from a `.zelversion` or `.bazelversion` file.
fn read_version_file(fs: &dyn FileSystem, path: &PathFragment) -> Option<String> {
    let bytes = fs.read_file(path).ok()?;
    let content = String::from_utf8_lossy(&bytes);
    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            return Some(trimmed.to_string());
        }
    }
    // An existing but empty .zelversion / .bazelversion file is ignored (matching Bazelisk core.go)
    None
}

fn looks_like_file_path(
    fs: &dyn FileSystem,
    cwd: &PathFragment,
    raw: &str,
) -> Option<PathFragment> {
    let pf = PathFragment::create(raw);
    let full = if pf.is_absolute() {
        pf.clone()
    } else {
        cwd.get_relative(&pf)
    };
    if fs.exists(&full, true) {
        return Some(full);
    }
    if raw.starts_with('/')
        || raw.starts_with("./")
        || raw.starts_with("../")
        || raw.contains('\\')
        || (raw.len() > 2 && raw.as_bytes()[1] == b':')
    {
        return Some(full);
    }
    None
}

fn parse_raw_version_spec(
    fs: &dyn FileSystem,
    cwd: &PathFragment,
    raw_spec: String,
    source: VersionSource,
    warn_message: Option<String>,
) -> ResolvedVersionSpec {
    if let Some(full_path) = looks_like_file_path(fs, cwd, &raw_spec) {
        return ResolvedVersionSpec {
            raw_spec,
            version: full_path.path_string().to_string(),
            fork: None,
            is_path: true,
            source,
            warn_message,
        };
    }

    // Check for `<owner>/<repo>/<version>` or `<fork>/<version>` syntax
    let parts: Vec<&str> = raw_spec.split('/').collect();
    let (fork, version_part) = match parts.as_slice() {
        [owner, repo, ver] if !owner.is_empty() && !repo.is_empty() && !ver.is_empty() => {
            (Some(format!("{owner}/{repo}")), (*ver).to_string())
        }
        [fork_name, ver] if !fork_name.is_empty() && !ver.is_empty() => {
            (Some((*fork_name).to_string()), (*ver).to_string())
        }
        _ => (None, raw_spec.clone()),
    };

    ResolvedVersionSpec {
        raw_spec,
        version: version_part,
        fork,
        is_path: false,
        source,
        warn_message,
    }
}

/// Resolves the desired Zel/Bazel version specification from environment variables or workspace files.
pub fn resolve_version_spec(
    fs: &dyn FileSystem,
    cwd: &PathFragment,
    workspace_dir: &PathFragment,
    env: &BazeliskEnv,
) -> Result<ResolvedVersionSpec, String> {
    if let Some(ref val) = env.use_zel_version {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return Ok(parse_raw_version_spec(
                fs,
                cwd,
                trimmed.to_string(),
                VersionSource::UseZelVersionEnv,
                None,
            ));
        }
    }

    if let Some(ref val) = env.use_bazel_version {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return Ok(parse_raw_version_spec(
                fs,
                cwd,
                trimmed.to_string(),
                VersionSource::UseBazelVersionEnv,
                None,
            ));
        }
    }

    let zelversion_path = workspace_dir.get_relative_str(".zelversion");
    if fs.exists(&zelversion_path, true) {
        if let Some(ver) = read_version_file(fs, &zelversion_path) {
            return Ok(parse_raw_version_spec(
                fs,
                cwd,
                ver,
                VersionSource::ZelVersionFile(zelversion_path),
                None,
            ));
        }
    }

    let bazelversion_path = workspace_dir.get_relative_str(".bazelversion");
    if fs.exists(&bazelversion_path, true) {
        if let Some(ver) = read_version_file(fs, &bazelversion_path) {
            return Ok(parse_raw_version_spec(
                fs,
                cwd,
                ver,
                VersionSource::BazelVersionFile(bazelversion_path),
                None,
            ));
        }
    }

    if let Some((ref var_name, ref fb_raw)) = env.fallback_version {
        let trimmed = fb_raw.trim();
        if trimmed == "error" {
            return Err(format!(
                "No Zel/Bazel version was specified in $USE_ZEL_VERSION, $USE_BAZEL_VERSION, .zelversion, or .bazelversion, and ${var_name} is set to 'error'."
            ));
        } else if let Some(ver) = trimmed.strip_prefix("warn:") {
            let ver = ver.trim().to_string();
            let warn = format!(
                "Warning: No Zel/Bazel version specified; falling back to '{ver}' (from ${var_name})."
            );
            return Ok(parse_raw_version_spec(
                fs,
                cwd,
                ver,
                VersionSource::FallbackEnv(var_name.clone()),
                Some(warn),
            ));
        } else if let Some(ver) = trimmed.strip_prefix("silent:") {
            return Ok(parse_raw_version_spec(
                fs,
                cwd,
                ver.trim().to_string(),
                VersionSource::FallbackEnv(var_name.clone()),
                None,
            ));
        } else if !trimmed.is_empty() {
            return Ok(parse_raw_version_spec(
                fs,
                cwd,
                trimmed.to_string(),
                VersionSource::FallbackEnv(var_name.clone()),
                None,
            ));
        }
    }

    Ok(ResolvedVersionSpec {
        raw_spec: env.current_version.clone(),
        version: env.current_version.clone(),
        fork: None,
        is_path: false,
        source: VersionSource::DefaultCurrent,
        warn_message: None,
    })
}

/// Checks whether a workspace wrapper script (`$ZEL_WRAPPER`, `$BAZEL_WRAPPER`, `tools/zel`, or `tools/bazel`)
/// exists in `workspace_dir`.
pub fn find_workspace_wrapper(
    fs: &dyn FileSystem,
    workspace_dir: &PathFragment,
    env: &BazeliskEnv,
) -> Option<PathFragment> {
    let candidates: Vec<String> = if let Some(ref custom) = env.wrapper_rel_path {
        vec![custom.clone()]
    } else {
        vec!["tools/zel".to_string(), "tools/bazel".to_string()]
    };

    for rel in candidates {
        let rel_pf = PathFragment::create(&rel);
        let full = if rel_pf.is_absolute() {
            rel_pf
        } else {
            workspace_dir.get_relative(&rel_pf)
        };
        if let Ok(st) = fs.stat(&full, true) {
            if st.is_file && is_executable_file(&full) {
                return Some(full);
            }
        }
    }
    None
}

fn is_executable_file(path: &PathFragment) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let std_path = std::path::Path::new(path.path_string());
        if let Ok(meta) = std::fs::metadata(std_path) {
            return meta.permissions().mode() & 0o111 != 0;
        }
        // For InMemoryFileSystem paths that do not exist on the host OS, treat existing VFS files as executable
        true
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        true
    }
}

fn mark_executable(path: &PathFragment) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let std_path = std::path::Path::new(path.path_string());
        if let Ok(meta) = std::fs::metadata(std_path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(std_path, perms);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// Extracts a version string from a binary filename such as `zel-9.3.1-darwin-arm64` or `bazel-9.3.1`.
fn extract_version_from_binary_name<'a>(name: &'a str, env: &BazeliskEnv) -> Option<&'a str> {
    let stem = name.strip_suffix(".exe").unwrap_or(name);
    if stem.ends_with(".sha256") || stem == "zel-real" || stem == "bazel-real" {
        return None;
    }
    let rest = stem
        .strip_prefix("zel-")
        .or_else(|| stem.strip_prefix("bazel_nojdk-"))
        .or_else(|| stem.strip_prefix("bazel-"))?;

    let suffix1 = format!("-{}-{}", env.os_name, env.arch_name);
    let suffix2 = format!("-{}-{}", env.os_name, env.raw_arch_name);
    let ver = rest
        .strip_suffix(&suffix1)
        .or_else(|| rest.strip_suffix(&suffix2))
        .unwrap_or(rest);

    if ver
        .as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_digit() || *b == b'v')
    {
        Some(ver)
    } else {
        None
    }
}

/// Resolves `"latest"` or `"latest-N"` against available local/cached/base_url versions,
/// falling back to `env.current_version`.
fn resolve_latest_version(
    fs: &dyn FileSystem,
    spec_version: &str,
    env: &BazeliskEnv,
) -> Result<String, String> {
    let offset: usize = if spec_version == "latest" {
        0
    } else if let Some(off_str) = spec_version.strip_prefix("latest-") {
        off_str
            .parse::<usize>()
            .map_err(|_| format!("Invalid latest version specifier '{spec_version}'"))?
    } else {
        return Ok(spec_version.to_string());
    };

    let mut discovered: Vec<String> = Vec::new();
    let add_ver = |discovered: &mut Vec<String>, v: &str| {
        let norm = normalize_version(v).to_string();
        if !norm.is_empty() && !discovered.contains(&norm) {
            discovered.push(norm);
        }
    };

    // 1. Check local directory base_url if configured
    if let Some(ref base) = env.base_url {
        let local_base = base.strip_prefix("file://").unwrap_or(base);
        let base_pf = PathFragment::create(local_base);
        if base_pf.is_absolute() && fs.exists(&base_pf, true) {
            if let Ok(entries) = fs.readdir(&base_pf) {
                for entry in entries {
                    if let Some(v) = extract_version_from_binary_name(&entry.name, env) {
                        add_ver(&mut discovered, v);
                    } else if entry
                        .name
                        .as_bytes()
                        .first()
                        .is_some_and(|b| b.is_ascii_digit() || *b == b'v')
                    {
                        add_ver(&mut discovered, &entry.name);
                    }
                }
            }
        }
    }

    // 2. Check sibling binaries in wrapper_dir
    if let Some(ref exe) = env.current_exe {
        if let Some(wrapper_dir) = exe.parent_directory() {
            if let Ok(entries) = fs.readdir(&wrapper_dir) {
                for entry in entries {
                    if let Some(v) = extract_version_from_binary_name(&entry.name, env) {
                        add_ver(&mut discovered, v);
                    }
                }
            }
        }
    }

    // 3. Check cache directories
    for cache_dir in env.candidate_cache_dirs() {
        for sub in ["downloads/zel", "downloads/bazelbuild", "bin"] {
            let dir = cache_dir.get_relative_str(sub);
            if let Ok(entries) = fs.readdir(&dir) {
                for entry in entries {
                    if let Some(v) = extract_version_from_binary_name(&entry.name, env) {
                        add_ver(&mut discovered, v);
                    }
                }
            }
        }
    }

    if discovered.is_empty() {
        add_ver(&mut discovered, &env.current_version);
    }

    discovered.sort_by(|a, b| compare_versions(a, b));
    if offset >= discovered.len() {
        return Err(format!(
            "Cannot resolve '{spec_version}': only {} version(s) available ({})",
            discovered.len(),
            discovered.join(", ")
        ));
    }
    Ok(discovered[discovered.len() - 1 - offset].clone())
}

/// Candidate filenames for a given version in a directory.
fn candidate_binary_filenames(version: &str, env: &BazeliskEnv) -> Vec<String> {
    let ext = env.exe_extension();
    let norm = normalize_version(version);
    let mut versions = vec![norm.to_string()];
    let v_prefixed = format!("v{norm}");
    if version != norm && !versions.contains(&version.to_string()) {
        versions.insert(0, version.to_string());
    }
    if !versions.contains(&v_prefixed) {
        versions.push(v_prefixed);
    }

    let mut names = Vec::new();
    let mut push_unique = |s: String| {
        if !names.contains(&s) {
            names.push(s);
        }
    };

    let prefixes = if env.invoked_as_bazel {
        ["bazel", "zel", "bazel_nojdk"]
    } else {
        ["zel", "bazel", "bazel_nojdk"]
    };

    for ver in &versions {
        for prefix in prefixes {
            push_unique(format!(
                "{prefix}-{ver}-{}-{}{ext}",
                env.os_name, env.arch_name
            ));
            push_unique(format!(
                "{prefix}-{ver}-{}-{}{ext}",
                env.os_name, env.raw_arch_name
            ));
            push_unique(format!("{prefix}-{ver}{ext}"));
        }
    }
    names
}

fn probe_real_binary_version(bin_path: &PathFragment) -> Option<String> {
    let std_path = std::path::Path::new(bin_path.path_string());
    if !std_path.exists() {
        return None;
    }
    let output = Command::new(std_path)
        .arg("--version")
        .env("ZEL_SKIP_WRAPPER", "true")
        .env("BAZELISK_SKIP_WRAPPER", "true")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(ver) = trimmed
            .strip_prefix("zel ")
            .or_else(|| trimmed.strip_prefix("bazel "))
        {
            return Some(ver.trim().to_string());
        }
    }
    None
}

fn verify_binary_sha256(
    bytes: &[u8],
    expected_sha256: &str,
    source_desc: &str,
) -> Result<(), String> {
    let expected = expected_sha256
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if expected.is_empty() {
        return Ok(());
    }
    let actual = sha256::hex_digest(bytes).to_ascii_lowercase();
    if actual != expected {
        return Err(format!(
            "SHA-256 checksum mismatch for {source_desc}: expected {expected}, got {actual}"
        ));
    }
    Ok(())
}

fn fetch_url_bytes(fs: &dyn FileSystem, url: &str) -> Result<Vec<u8>, String> {
    if let Some(file_path) = url.strip_prefix("file://") {
        let pf = PathFragment::create(file_path);
        return fs
            .read_file(&pf)
            .map_err(|e| format!("Failed to read {url}: {e}"));
    }
    let as_pf = PathFragment::create(url);
    if as_pf.is_absolute() && !url.contains("://") {
        return fs
            .read_file(&as_pf)
            .map_err(|e| format!("Failed to read {url}: {e}"));
    }

    // Use curl (or wget fallback) for http:// / https:// URLs
    if let Ok(output) = Command::new("curl")
        .args(["-fsSL", "--connect-timeout", "10", url])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        if output.status.success() {
            return Ok(output.stdout);
        }
    }

    if let Ok(output) = Command::new("wget")
        .args(["-qO-", "--timeout=10", "--tries=1", url])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        if output.status.success() {
            return Ok(output.stdout);
        }
    }

    Err(format!("Failed to download {url}"))
}

/// Escapes a `BAZELISK_BASE_URL` or fork identifier using Bazelisk's metadata directory naming
/// rule (`[^a-zA-Z0-9]` -> `'-'`).
pub fn escape_bazelisk_source_name(source: &str) -> String {
    source
        .trim_end_matches('/')
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Populates a Bazelisk-compatible content-addressable cache entry (`downloads/metadata/...` and
/// `downloads/sha256/<sha256>/bin/bazel`) inside `cache_root` so both `bazelisk` and `zel` can
/// resolve the binary offline without network access.
pub fn populate_bazelisk_cache_entry(
    fs: &dyn FileSystem,
    cache_root: &PathFragment,
    version: &str,
    base_url_or_fork: &str,
    bytes: &[u8],
    env: &BazeliskEnv,
) -> Result<PathFragment, String> {
    let sha_hex = sha256::hex_digest(bytes).to_ascii_lowercase();
    let ext = env.exe_extension();
    let sha_bin_dir = cache_root.get_relative_str(&format!("downloads/sha256/{sha_hex}/bin"));
    fs.create_directory_and_parents(&sha_bin_dir)
        .map_err(|e| format!("Failed to create {sha_bin_dir}: {e}"))?;

    let sha_bazel_bin = sha_bin_dir.get_relative_str(&format!("bazel{ext}"));
    if !fs.exists(&sha_bazel_bin, true) {
        let tmp_bin =
            sha_bin_dir.get_relative_str(&format!("bazel{ext}.tmp.{}", std::process::id()));
        fs.write_file(&tmp_bin, bytes)
            .map_err(|e| format!("Failed to write {tmp_bin}: {e}"))?;
        mark_executable(&tmp_bin);
        fs.rename(&tmp_bin, &sha_bazel_bin)
            .map_err(|e| format!("Failed to finalize {sha_bazel_bin}: {e}"))?;
        mark_executable(&sha_bazel_bin);
    }

    let norm_ver = normalize_version(version);
    let escaped_src = escape_bazelisk_source_name(base_url_or_fork);
    for meta_subdir in [
        escaped_src.as_str(),
        "bazelbuild",
        &escape_bazelisk_source_name(DEFAULT_ZEL_BASE_URL),
    ] {
        let meta_dir = cache_root.get_relative_str(&format!("downloads/metadata/{meta_subdir}"));
        let _ = fs.create_directory_and_parents(&meta_dir);
        let mut meta_filenames = vec![
            format!("bazel-{norm_ver}-{}-{}{ext}", env.os_name, env.arch_name),
            format!("zel-{norm_ver}-{}-{}{ext}", env.os_name, env.arch_name),
        ];
        // Bazelisk < 4.1.0 on darwin-arm64 falls back to requesting `darwin-x86_64`
        if env.os_name == "darwin" && env.arch_name == "arm64" {
            meta_filenames.push(format!("bazel-{norm_ver}-darwin-x86_64{ext}"));
        }
        for mf in meta_filenames {
            let _ = fs.write_file(&meta_dir.get_relative_str(&mf), sha_hex.as_bytes());
        }
    }

    // Also populate legacy Bazelisk (< 1.21.0) directory layout (`downloads/bazelbuild/bazel-<ver>-<os>-<arch>/bin/bazel`)
    let mut legacy_platforms = vec![format!("{}-{}", env.os_name, env.arch_name)];
    if env.os_name == "darwin" && env.arch_name == "arm64" {
        legacy_platforms.push("darwin-x86_64".to_string());
    }
    for plat in legacy_platforms {
        for legacy_root in ["bazelbuild", "_custom"] {
            let legacy_bin_dir = cache_root.get_relative_str(&format!(
                "downloads/{legacy_root}/bazel-{norm_ver}-{plat}/bin"
            ));
            if fs.create_directory_and_parents(&legacy_bin_dir).is_ok() {
                let legacy_bin = legacy_bin_dir.get_relative_str(&format!("bazel{ext}"));
                if !fs.exists(&legacy_bin, true) && fs.write_file(&legacy_bin, bytes).is_ok() {
                    mark_executable(&legacy_bin);
                }
            }
        }
    }

    Ok(sha_bazel_bin)
}

/// Locates or downloads the binary satisfying `version`.
pub fn resolve_version_binary(
    fs: &dyn FileSystem,
    version: &str,
    fork: Option<&str>,
    source: &VersionSource,
    env: &BazeliskEnv,
) -> Result<PathFragment, String> {
    let candidates = candidate_binary_filenames(version, env);

    // 1. Search sibling binaries next to `current_exe` (`wrapper_dir`)
    if fork.is_none() {
        if let Some(ref exe) = env.current_exe {
            if let Some(wrapper_dir) = exe.parent_directory() {
                for name in &candidates {
                    let cand = wrapper_dir.get_relative_str(name);
                    if fs.exists(&cand, true) && is_executable_file(&cand) {
                        return Ok(cand);
                    }
                }
                // Check `zel-real` / `bazel-real`
                let ext = env.exe_extension();
                for real_name in [format!("zel-real{ext}"), format!("bazel-real{ext}")] {
                    let real_cand = wrapper_dir.get_relative_str(&real_name);
                    if fs.exists(&real_cand, true) && is_executable_file(&real_cand) {
                        if let Some(probed) = probe_real_binary_version(&real_cand) {
                            if normalize_version(&probed) == normalize_version(version) {
                                return Ok(real_cand);
                            }
                        }
                    }
                }
            }
        }
    }

    let base_url = if let Some(ref custom_url) = env.base_url {
        custom_url.trim_end_matches('/').to_string()
    } else if let Some(fork_spec) = fork {
        if fork_spec.contains('/') {
            format!("https://github.com/{fork_spec}/releases/download")
        } else {
            DEFAULT_ZEL_BASE_URL.to_string()
        }
    } else {
        DEFAULT_ZEL_BASE_URL.to_string()
    };

    // 2. Search local cache directories (`$ZEL_HOME`, `$BAZELISK_HOME`, `~/.cache/zel`, etc.)
    let fork_dir = fork.unwrap_or("zel");
    let escaped_base = escape_bazelisk_source_name(&base_url);
    for cache_dir in env.candidate_cache_dirs() {
        // 2a. Check Bazelisk content-addressable cache (`downloads/metadata/<src>/<name>` -> `downloads/sha256/<sha>/bin/bazel`)
        for meta_src in [
            escaped_base.as_str(),
            fork_dir,
            "zel",
            "bazelbuild",
            &escape_bazelisk_source_name(DEFAULT_ZEL_BASE_URL),
        ] {
            let meta_dir = cache_dir.get_relative_str(&format!("downloads/metadata/{meta_src}"));
            for name in &candidates {
                let meta_file = meta_dir.get_relative_str(name);
                if let Ok(sha_bytes) = fs.read_file(&meta_file) {
                    let sha_hex = String::from_utf8_lossy(&sha_bytes)
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_ascii_lowercase();
                    if !sha_hex.is_empty() {
                        if let Some(ref expected_sha) = env.verify_sha256 {
                            let exp = expected_sha.trim().to_ascii_lowercase();
                            if !exp.is_empty() && exp != sha_hex {
                                return Err(format!(
                                    "SHA-256 checksum mismatch for cached {meta_file}: expected {exp}, got {sha_hex}"
                                ));
                            }
                        }
                        for bin_name in [
                            format!("bazel{}", env.exe_extension()),
                            format!("zel{}", env.exe_extension()),
                        ] {
                            let sha_bin = cache_dir.get_relative_str(&format!(
                                "downloads/sha256/{sha_hex}/bin/{bin_name}"
                            ));
                            if fs.exists(&sha_bin, true) && is_executable_file(&sha_bin) {
                                return Ok(sha_bin);
                            }
                        }
                    }
                }
            }
        }

        // 2b. Check direct download and bin directories
        for sub in [
            format!("downloads/{fork_dir}"),
            "downloads/zel".to_string(),
            "downloads/bazelbuild".to_string(),
            "bin".to_string(),
        ] {
            let dir = cache_dir.get_relative_str(&sub);
            for name in &candidates {
                let cand = dir.get_relative_str(name);
                if fs.exists(&cand, true) && is_executable_file(&cand) {
                    if let Some(ref expected_sha) = env.verify_sha256 {
                        let bytes = fs
                            .read_file(&cand)
                            .map_err(|e| format!("Failed to read cached binary {cand}: {e}"))?;
                        verify_binary_sha256(&bytes, expected_sha, cand.path_string())?;
                    }
                    return Ok(cand);
                }
                // Also support Bazelisk directory layout: `downloads/.../<name>/bin/bazel`
                for inner_bin in [
                    format!("bin/zel{}", env.exe_extension()),
                    format!("bin/bazel{}", env.exe_extension()),
                ] {
                    let nested = cand.get_relative_str(&inner_bin);
                    if fs.exists(&nested, true) && is_executable_file(&nested) {
                        if let Some(ref expected_sha) = env.verify_sha256 {
                            let bytes = fs.read_file(&nested).map_err(|e| {
                                format!("Failed to read cached binary {nested}: {e}")
                            })?;
                            verify_binary_sha256(&bytes, expected_sha, nested.path_string())?;
                        }
                        return Ok(nested);
                    }
                }
            }
        }
    }

    // 3. Download from release URL if allowed
    if env.no_download {
        return Err(format!(
            "The project requires Zel version '{}' (specified in {}), but the running binary is version '{}' and downloads are disabled ($ZEL_NO_DOWNLOAD / $BAZELISK_NO_DOWNLOAD).",
            version, source, env.current_version
        ));
    }

    let norm_ver = normalize_version(version);
    let ext = env.exe_extension();
    let artifact_names = [
        format!("zel-{norm_ver}-{}-{}{ext}", env.os_name, env.arch_name),
        format!("bazel-{norm_ver}-{}-{}{ext}", env.os_name, env.arch_name),
    ];
    let tags = [norm_ver.to_string(), format!("v{norm_ver}")];

    let mut candidate_urls = Vec::new();
    for tag in &tags {
        for art in &artifact_names {
            let u = format!("{base_url}/{tag}/{art}");
            if !candidate_urls.contains(&u) {
                candidate_urls.push(u);
            }
        }
    }
    // Also support flat directory / file:// base URLs (e.g. `dist/release`)
    for art in &artifact_names {
        let u = format!("{base_url}/{art}");
        if !candidate_urls.contains(&u) {
            candidate_urls.push(u);
        }
    }

    for url in &candidate_urls {
        if let Ok(bytes) = fetch_url_bytes(fs, url) {
            // Verify against explicit env SHA-256 or companion `<url>.sha256` if available
            if let Some(ref expected_sha) = env.verify_sha256 {
                verify_binary_sha256(&bytes, expected_sha, url)?;
            } else if let Ok(sha_bytes) = fetch_url_bytes(fs, &format!("{url}.sha256")) {
                let sha_text = String::from_utf8_lossy(&sha_bytes);
                verify_binary_sha256(&bytes, &sha_text, url)?;
            }

            let primary_cache = env.primary_cache_dir();
            let cache_dir = primary_cache.get_relative_str(&format!("downloads/{fork_dir}"));
            fs.create_directory_and_parents(&cache_dir)
                .map_err(|e| format!("Failed to create cache directory {cache_dir}: {e}"))?;

            let dest_name = &artifact_names[0];
            let dest_path = cache_dir.get_relative_str(dest_name);
            let tmp_path =
                cache_dir.get_relative_str(&format!("{dest_name}.tmp.{}", std::process::id()));

            fs.write_file(&tmp_path, &bytes)
                .map_err(|e| format!("Failed to write downloaded binary {tmp_path}: {e}"))?;
            mark_executable(&tmp_path);
            fs.rename(&tmp_path, &dest_path)
                .map_err(|e| format!("Failed to finalize downloaded binary {dest_path}: {e}"))?;
            mark_executable(&dest_path);

            // Also populate Bazelisk's `downloads/metadata/...` and `downloads/sha256/...` layout
            let _ =
                populate_bazelisk_cache_entry(fs, &primary_cache, norm_ver, &base_url, &bytes, env);

            return Ok(dest_path);
        }
    }

    Err(format!(
        "The project requires Zel version '{}' (specified in {}), but the running binary is version '{}' and version '{}' could not be found locally or downloaded from {}.",
        version, source, env.current_version, version, base_url
    ))
}

/// Plans whether the current CLI invocation should run in-process or delegate to another binary
/// or workspace wrapper script (`tools/zel` / `tools/bazel`).
pub fn plan_invocation(
    fs: &dyn FileSystem,
    cwd: &PathFragment,
    workspace_dir: &PathFragment,
    raw_args: &[String],
    raw_env: &BazeliskEnv,
) -> Result<LauncherAction, String> {
    let mut env = raw_env.with_rc_files(fs, workspace_dir);

    // If invoked by external `bazelisk` (`BAZELISK_SKIP_WRAPPER=true`, `ZEL_SKIP_WRAPPER` unset),
    // `bazelisk` has already processed `$USE_BAZEL_VERSION`, `.bazelversion`, and `tools/bazel`.
    // Ignore `$USE_BAZEL_VERSION` if it points to `current_exe` (or if `.zelversion` / `$USE_ZEL_VERSION`
    // is used to specify the Zel version underneath `bazelisk`).
    if env.bazelisk_skip_wrapper && !env.skip_wrapper {
        env.use_bazel_version = None;
    }

    let mut spec = resolve_version_spec(fs, cwd, workspace_dir, &env)?;

    if env.skip_wrapper {
        return Ok(LauncherAction::RunInProcess { spec });
    }

    // When invoked via external `bazelisk`, if the version requirement came from `.bazelversion`
    // (not `.zelversion` or `$USE_ZEL_VERSION`), `bazelisk` already resolved and launched this binary.
    if env.bazelisk_skip_wrapper
        && !matches!(
            spec.source,
            VersionSource::UseZelVersionEnv | VersionSource::ZelVersionFile(_)
        )
    {
        return Ok(LauncherAction::RunInProcess { spec });
    }

    // Resolve `latest` or `latest-N` if requested
    if !spec.is_path && (spec.version == "latest" || spec.version.starts_with("latest-")) {
        spec.version = resolve_latest_version(fs, &spec.version, &env)?;
    }

    // When invoking a local build output directly inside its own workspace (e.g. `./target/debug/zel`,
    // `./target/release/zel`, or `./bazel-bin/crates/zel`) without an explicit `$USE_ZEL_VERSION` or
    // `$USE_BAZEL_VERSION` override, treat the local build artifact as satisfying the workspace version file.
    let is_workspace_build_exe = matches!(
        spec.source,
        VersionSource::ZelVersionFile(_) | VersionSource::BazelVersionFile(_)
    ) && env.current_exe.as_ref().is_some_and(|exe| {
        exe.starts_with(&workspace_dir.get_relative_str("target"))
            || exe.starts_with(&workspace_dir.get_relative_str("bazel-bin"))
            || exe.starts_with(&workspace_dir.get_relative_str("bazel-out"))
    });

    let version_matches_current = !spec.is_path
        && spec.fork.is_none()
        && (normalize_version(&spec.version) == normalize_version(&env.current_version)
            || is_workspace_build_exe);

    // Determine the target Zel/Bazel binary path if needed
    let resolved_binary: Option<PathFragment> = if spec.is_path {
        let bin_path = PathFragment::create(&spec.version);
        if !fs.exists(&bin_path, true) {
            return Err(format!(
                "The Zel/Bazel binary '{}' (specified in {}) does not exist.",
                bin_path, spec.source
            ));
        }
        Some(bin_path)
    } else if version_matches_current {
        env.current_exe.clone()
    } else {
        Some(resolve_version_binary(
            fs,
            &spec.version,
            spec.fork.as_deref(),
            &spec.source,
            &env,
        )?)
    };

    // Check for workspace wrapper script (`$ZEL_WRAPPER`, `$BAZEL_WRAPPER`, `tools/zel`, `tools/bazel`)
    if !env.bazelisk_skip_wrapper {
        if let Some(wrapper_path) = find_workspace_wrapper(fs, workspace_dir, &env) {
            let real_bin = resolved_binary
                .clone()
                .unwrap_or_else(|| PathFragment::create("zel"));
            let mut env_vars = vec![
                ("ZEL_REAL".to_string(), real_bin.path_string().to_string()),
                ("BAZEL_REAL".to_string(), real_bin.path_string().to_string()),
                ("ZEL_SKIP_WRAPPER".to_string(), "true".to_string()),
                ("BAZELISK_SKIP_WRAPPER".to_string(), "true".to_string()),
            ];
            if env.invoked_as_bazel {
                env_vars.push(("ZEL_PRODUCT_PREFIX".to_string(), "bazel".to_string()));
            } else {
                env_vars.push(("ZEL_PRODUCT_PREFIX".to_string(), "zel".to_string()));
            }
            return Ok(LauncherAction::ExecExternal {
                program: wrapper_path,
                args: raw_args.to_vec(),
                env_vars,
                spec,
            });
        }
    }

    // If the resolved version matches the running binary (or resolved_binary is current_exe), run in-process!
    if version_matches_current {
        return Ok(LauncherAction::RunInProcess { spec });
    }
    if let (Some(ref target), Some(ref cur)) = (&resolved_binary, &env.current_exe) {
        if target == cur {
            return Ok(LauncherAction::RunInProcess { spec });
        }
    }

    let target_bin = resolved_binary.expect("resolved_binary must be Some when version differs");
    let mut env_vars = vec![
        ("ZEL_SKIP_WRAPPER".to_string(), "true".to_string()),
        ("BAZELISK_SKIP_WRAPPER".to_string(), "true".to_string()),
    ];
    if env.invoked_as_bazel {
        env_vars.push(("ZEL_PRODUCT_PREFIX".to_string(), "bazel".to_string()));
    } else {
        env_vars.push(("ZEL_PRODUCT_PREFIX".to_string(), "zel".to_string()));
    }

    Ok(LauncherAction::ExecExternal {
        program: target_bin,
        args: raw_args.to_vec(),
        env_vars,
        spec,
    })
}

/// Executes an external binary or wrapper script and streams its stdout/stderr and exit code.
pub fn execute_external_action<O: Write + Send, E: Write + Send>(
    program: &PathFragment,
    args: &[String],
    env_vars: &[(String, String)],
    cwd: &PathFragment,
    inherit_stdio: bool,
    stdout_writer: &mut O,
    stderr_writer: &mut E,
) -> Result<i32, String> {
    let mut cmd = Command::new(program.path_string());
    cmd.args(args)
        .current_dir(cwd.path_string())
        .stdin(Stdio::inherit());

    for (k, v) in env_vars {
        cmd.env(k, v);
    }

    if inherit_stdio {
        let _ = stdout_writer.flush();
        let _ = stderr_writer.flush();
        let status = cmd
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|e| format!("Failed to execute '{}': {e}", program))?;
        return Ok(status.code().unwrap_or(1));
    }

    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to execute '{}': {e}", program))?;

    let mut child_out = child.stdout.take();
    let mut child_err = child.stderr.take();

    std::thread::scope(|s| {
        let out_handle = s.spawn(|| {
            if let Some(ref mut r) = child_out {
                let mut buf = [0u8; 4096];
                while let Ok(n) = r.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let _ = stdout_writer.write_all(&buf[..n]);
                    let _ = stdout_writer.flush();
                }
            }
        });
        let err_handle = s.spawn(|| {
            if let Some(ref mut r) = child_err {
                let mut buf = [0u8; 4096];
                while let Ok(n) = r.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let _ = stderr_writer.write_all(&buf[..n]);
                    let _ = stderr_writer.flush();
                }
            }
        });
        let _ = out_handle.join();
        let _ = err_handle.join();
    });

    let status = child
        .wait()
        .map_err(|e| format!("Failed to wait for '{}': {e}", program))?;

    Ok(status.code().unwrap_or(1))
}
