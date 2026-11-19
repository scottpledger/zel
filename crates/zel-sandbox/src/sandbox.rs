// Copyright 2026 The Bazel Authors. All rights reserved.
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

//! Sandboxed spawn runners: `ProcessWrapperSandboxedSpawnRunner` and
//! `DarwinSandboxedSpawnRunner` (both on top of `SymlinkedSandboxedSpawn`), plus the sandbox
//! base directory handling of `SandboxModule` (`<output_base>/sandbox`, `--sandbox_base`,
//! cleanup after the build unless `--sandbox_debug`).

use crate::process_wrapper::{
    self, CancelFlag, CommandLineBuilder as PwCommandLineBuilder, ProcessWrapperOptions,
    Termination,
};
use crate::{
    cancelled_error, delete_tree, ExecHooks, ExpandedInput, HierarchicalPath, RunfilesTreeUpdater,
    Spawn, SpawnResult, SpawnStatus, DEFAULT_KILL_DELAY_MS,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use zel_vfs::PathFragment;

/// `SandboxOptions` (the subset that affects Unix runners).
#[derive(Debug, Clone)]
pub struct SandboxOptions {
    pub debug: bool,
    /// `--sandbox_base` (empty = `<output_base>/sandbox`).
    pub base: String,
    pub writable_paths: Vec<String>,
    pub block_paths: Vec<String>,
    pub tmpfs_paths: Vec<String>,
    /// (source, target) from `--sandbox_add_mount_pair`.
    pub mount_pairs: Vec<(String, String)>,
    pub default_allow_network: bool,
    pub fake_hostname: bool,
    pub fake_username: bool,
    pub explicit_pseudoterminal: bool,
    pub use_hermetic_linux_sandbox: bool,
    pub process_wrapper_graceful_sigterm: bool,
    /// Optional path to `process-wrapper` binary (`BinTools.getEmbeddedPath("process-wrapper")`).
    pub process_wrapper_path: Option<String>,
    /// Optional path to `linux-sandbox` binary (`LinuxSandboxUtil.getLinuxSandbox`).
    pub linux_sandbox_path: Option<String>,
}

impl Default for SandboxOptions {
    fn default() -> Self {
        Self {
            debug: false,
            base: String::new(),
            writable_paths: Vec::new(),
            block_paths: Vec::new(),
            tmpfs_paths: Vec::new(),
            mount_pairs: Vec::new(),
            // `--sandbox_default_allow_network` defaults to true.
            default_allow_network: true,
            fake_hostname: false,
            fake_username: false,
            explicit_pseudoterminal: false,
            use_hermetic_linux_sandbox: false,
            process_wrapper_graceful_sigterm: false,
            process_wrapper_path: None,
            linux_sandbox_path: None,
        }
    }
}

/// `SandboxOptions.MountPairConverter`.
pub fn parse_mount_pair(input: &str) -> Result<(String, String), String> {
    // Split on ':' but not on '\:'.
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = input.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == ':' && !(i > 0 && chars[i - 1] == '\\') {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    parts.push(cur);
    // Java's split drops trailing empty strings.
    while parts.len() > 1 && parts.last().map(|p| p.is_empty()).unwrap_or(false) {
        parts.pop();
    }
    let mut paths = Vec::new();
    for p in parts {
        if p.trim().is_empty() {
            return Err(format!(
                "Input {input} contains one or more empty paths. Input must be a single path to mount inside the sandbox or a mounting pair in the form of 'source:target'"
            ));
        }
        paths.push(p.replace("\\:", ":"));
    }
    if paths.is_empty() || paths.len() > 2 {
        return Err("Input must be a single path to mount inside the sandbox or a mounting pair in the form of 'source:target'".into());
    }
    Ok(if paths.len() == 1 {
        (paths[0].clone(), paths[0].clone())
    } else {
        (paths[0].clone(), paths[1].clone())
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxKind {
    ProcessWrapper,
    Darwin,
    Linux,
}

impl SandboxKind {
    pub fn name(self) -> &'static str {
        match self {
            SandboxKind::ProcessWrapper => "processwrapper-sandbox",
            SandboxKind::Darwin => "darwin-sandbox",
            SandboxKind::Linux => "linux-sandbox",
        }
    }
}

/// `LinuxSandboxedSpawnRunner.isSupported`: live-probes `linux-sandbox` when present on Linux.
pub fn linux_sandbox_supported() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let Some(bin) = find_linux_sandbox_binary_default() else {
        return false;
    };
    crate::linux::probe_linux_sandbox(&bin, None, 15).is_ok()
}

/// Live-probed support check for a specific exec root and `local_sigkill_grace_seconds`.
pub fn linux_sandbox_supported_at(
    exec_root: &Path,
    kill_grace_secs: u64,
    explicit_bin: Option<&str>,
) -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let bin = explicit_bin
        .map(str::to_string)
        .or_else(|| find_linux_sandbox_binary(exec_root));
    let Some(bin) = bin else {
        return false;
    };
    crate::linux::probe_linux_sandbox(&bin, Some(exec_root), kill_grace_secs).is_ok()
}

fn find_linux_sandbox_binary_default() -> Option<String> {
    if let Ok(p) = std::env::var("ZEL_LINUX_SANDBOX") {
        if !p.is_empty() && Path::new(&p).exists() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("linux-sandbox");
            if cand.exists() {
                return Some(cand.to_string_lossy().into_owned());
            }
        }
    }
    None
}

fn find_linux_sandbox_binary(exec_root: &Path) -> Option<String> {
    if let Some(output_base) = exec_root.parent().and_then(|p| p.parent()) {
        let p = output_base.join("install/linux-sandbox");
        if p.exists() {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    if let Some(p) = find_linux_sandbox_binary_default() {
        return Some(p);
    }
    if let Ok(home) = std::env::var("HOME") {
        let user = std::env::var("USER").unwrap_or_else(|_| "ubuntu".into());
        let install_root = PathBuf::from(&home).join(format!(".cache/bazel/_bazel_{user}/install"));
        if let Ok(rd) = std::fs::read_dir(&install_root) {
            for e in rd.flatten() {
                let cand = e.path().join("linux-sandbox");
                if cand.exists() {
                    return Some(cand.to_string_lossy().into_owned());
                }
            }
        }
    }
    None
}

fn shell_escape_arg(s: &str) -> String {
    if !s.is_empty()
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'_' | b'@' | b'%' | b'+' | b'=' | b':' | b',' | b'.' | b'/' | b'-'
                )
        })
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn format_interactive_debug_command(
    argv: &[String],
    env: &BTreeMap<String, String>,
    cwd: &str,
) -> String {
    let mut s = format!("(cd {} && \\\n  exec env - \\\n", shell_escape_arg(cwd));
    for (k, v) in env {
        s.push_str(&format!(
            "    {}={} \\\n",
            shell_escape_arg(k),
            shell_escape_arg(v)
        ));
    }
    let cmd = argv
        .iter()
        .map(|a| shell_escape_arg(a))
        .collect::<Vec<_>>()
        .join(" ");
    s.push_str(&format!("  {cmd})"));
    s
}

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// `DarwinSandboxedSpawnRunner.isSupported`: runs a trivial command inside `sandbox-exec`.
pub fn darwin_sandbox_supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        if !cfg!(target_os = "macos") {
            return false;
        }
        std::process::Command::new(SANDBOX_EXEC)
            .args(["-p", "(version 1) (allow default)", "/usr/bin/true"])
            .current_dir("/usr/bin")
            .env_clear()
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// `ProcessWrapperSandboxedSpawnRunner.isSupported`: any POSIX system with the wrapper.
pub fn process_wrapper_supported() -> bool {
    cfg!(unix)
}

/// Escapes quotes and backslashes for Apple SBPL Scheme string literals.
pub fn escape_scheme_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// `DarwinSandboxedSpawnRunner.writeConfig`.
pub fn darwin_sandbox_config(
    writable_dirs: &BTreeSet<String>,
    inaccessible_paths: &BTreeSet<String>,
    allow_network: bool,
    statistics_path: Option<&str>,
) -> String {
    let mut o = String::new();
    // Note: In Apple's sandbox configuration language, the *last* matching rule wins.
    o.push_str("(version 1)\n");
    o.push_str("(debug deny)\n");
    o.push_str("(allow default)\n");
    o.push_str("(allow process-exec (with no-sandbox) (literal \"/bin/ps\"))\n");
    if !allow_network {
        o.push_str("(deny network*)\n");
        o.push_str("(allow network-inbound (local ip \"localhost:*\"))\n");
        o.push_str("(allow network* (remote ip \"localhost:*\"))\n");
        o.push_str("(allow network* (remote unix-socket))\n");
    }
    // By default, everything is read-only.
    o.push_str("(deny file-write*)\n");
    o.push_str("(allow file-write*\n");
    for p in writable_dirs {
        o.push_str(&format!("    (subpath \"{}\")\n", escape_scheme_string(p)));
    }
    if let Some(s) = statistics_path {
        o.push_str(&format!("    (literal \"{}\")\n", escape_scheme_string(s)));
    }
    o.push_str(")\n");
    if !inaccessible_paths.is_empty() {
        o.push_str("(deny file-read*\n");
        for p in inaccessible_paths {
            o.push_str(&format!("    (subpath \"{}\")\n", escape_scheme_string(p)));
        }
        o.push_str(")\n");
    }
    o
}

fn resolve(p: &str) -> Option<String> {
    std::fs::canonicalize(p)
        .ok()
        .map(|x| x.to_string_lossy().into_owned())
}

/// Resolves symlinks in `path` as far as possible: if `path` exists, returns its canonical path;
/// otherwise canonicalizes the longest existing ancestor and appends the remaining normal
/// components. macOS Seatbelt (`sandbox-exec`) evaluates `(subpath ...)` / `(literal ...)` rules
/// against canonical paths (e.g. `/private/var/...` rather than `/var/...`).
fn resolve_best_effort(path: &Path) -> String {
    if let Ok(c) = std::fs::canonicalize(path) {
        return c.to_string_lossy().into_owned();
    }
    let mut tail = Vec::new();
    let mut cur = path;
    while let Some(parent) = cur.parent() {
        match cur.components().next_back() {
            Some(std::path::Component::Normal(name)) => tail.push(name.to_os_string()),
            Some(std::path::Component::CurDir) => {}
            _ => return path.to_string_lossy().into_owned(),
        }
        if parent.as_os_str().is_empty() {
            break;
        }
        if let Ok(mut c) = std::fs::canonicalize(parent) {
            for comp in tail.into_iter().rev() {
                c.push(comp);
            }
            return c.to_string_lossy().into_owned();
        }
        cur = parent;
    }
    path.to_string_lossy().into_owned()
}

static DEVELOPER_DIR_CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static SDK_ROOT_CACHE: OnceLock<Mutex<HashMap<(String, String), String>>> = OnceLock::new();

fn trim_ascii_le_space(s: &str) -> &str {
    s.trim_matches(|c: char| (c as u32) <= 0x20)
}

/// Validates a dotted version (`DottedVersion.fromString`): one or more `.`-separated components,
/// each starting with a non-negative 32-bit integer followed by optional ASCII `[a-zA-Z0-9]*`.
pub fn validate_dotted_version(version: &str) -> Result<(), String> {
    if version.is_empty() {
        return Err("Invalid Xcode version: empty string".to_string());
    }
    for comp in version.split('.') {
        if comp.is_empty() {
            return Err(format!(
                "Invalid Xcode version '{version}': empty component"
            ));
        }
        let digit_len = comp.bytes().take_while(|b| b.is_ascii_digit()).count();
        if digit_len == 0 {
            return Err(format!(
                "Invalid Xcode version '{version}': component '{comp}' must start with digits"
            ));
        }
        comp[..digit_len].parse::<i32>().map_err(|_| {
            format!("Invalid Xcode version '{version}': numeric overflow in '{comp}'")
        })?;
        if !comp[digit_len..].bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(format!(
                "Invalid Xcode version '{version}': invalid suffix in '{comp}'"
            ));
        }
    }
    Ok(())
}

fn query_developer_dir(version: &str, client_env: &BTreeMap<String, String>) -> io::Result<String> {
    let cache = DEVELOPER_DIR_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = cache.lock().unwrap().get(version) {
        return Ok(v.clone());
    }
    let mut cmd = std::process::Command::new("/usr/bin/xcode-select");
    cmd.arg("-p").env_clear().envs(client_env);
    let out = cmd.output().map_err(|e| {
        io::Error::new(
            io::ErrorKind::Other,
            format!("Failed to locate Xcode '{version}': {e}"),
        )
    })?;
    if !out.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!(
                "xcode-locator failed for Xcode version '{version}': {}",
                String::from_utf8_lossy(&out.stderr)
            ),
        ));
    }
    let val = trim_ascii_le_space(&String::from_utf8_lossy(&out.stdout)).to_string();
    cache
        .lock()
        .unwrap()
        .insert(version.to_string(), val.clone());
    Ok(val)
}

fn query_sdk_root(developer_dir: &str, apple_sdk_platform: &str) -> io::Result<String> {
    let plat = apple_sdk_platform.to_ascii_lowercase();
    let key = (developer_dir.to_string(), plat.clone());
    let cache = SDK_ROOT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = cache.lock().unwrap().get(&key) {
        return Ok(v.clone());
    }
    let mut cmd = std::process::Command::new("/usr/bin/xcrun");
    cmd.env_clear();
    if !developer_dir.is_empty() {
        cmd.env("DEVELOPER_DIR", developer_dir);
    }
    let out = cmd
        .args(["--sdk", &plat, "--show-sdk-path"])
        .output()
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("xcrun failed for SDK '{plat}': {e}"),
            )
        })?;
    if !out.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!(
                "xcrun failed for SDK '{plat}' (status {}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            ),
        ));
    }
    let val = trim_ascii_le_space(&String::from_utf8_lossy(&out.stdout)).to_string();
    cache.lock().unwrap().insert(key, val.clone());
    Ok(val)
}

/// Testable core of `XcodeLocalEnvProvider.rewriteLocalEnv`.
pub fn rewrite_xcode_local_env_with<FLoc, FSdk>(
    env: &BTreeMap<String, String>,
    client_env: &BTreeMap<String, String>,
    mut locate_xcode: FLoc,
    mut query_sdk: FSdk,
) -> io::Result<BTreeMap<String, String>>
where
    FLoc: FnMut(&str, &BTreeMap<String, String>) -> io::Result<String>,
    FSdk: FnMut(&str, &str) -> io::Result<String>,
{
    let mut r = rewrite_local_env(env, client_env);
    let contains_xcode_version = r.contains_key("XCODE_VERSION_OVERRIDE");
    let contains_apple_sdk_platform = r.contains_key("APPLE_SDK_PLATFORM");
    if !contains_xcode_version && !contains_apple_sdk_platform {
        return Ok(r);
    }
    let mut developer_dir = String::new();
    if contains_xcode_version && !r.contains_key("DEVELOPER_DIR") {
        let version = r["XCODE_VERSION_OVERRIDE"].clone();
        if version.starts_with('/') {
            developer_dir = version;
        } else {
            validate_dotted_version(&version)
                .map_err(|m| io::Error::new(io::ErrorKind::InvalidInput, m))?;
            developer_dir = locate_xcode(&version, client_env)?;
        }
        r.insert("DEVELOPER_DIR".to_string(), developer_dir.clone());
    }
    if contains_apple_sdk_platform {
        if r.contains_key("SDKROOT") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Cannot resolve APPLE_SDK_PLATFORM when SDKROOT is already set",
            ));
        }
        let sdk_platform = r["APPLE_SDK_PLATFORM"].clone();
        let sdk_root = query_sdk(&developer_dir, &sdk_platform)?;
        r.insert("SDKROOT".to_string(), sdk_root);
    }
    Ok(r)
}

/// `PosixLocalEnvProvider.rewriteLocalEnv`: TMPDIR from the client environment (if absolute) or `/tmp`.
pub fn rewrite_local_env(
    env: &BTreeMap<String, String>,
    client_env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut r: BTreeMap<String, String> = env
        .iter()
        .filter(|(k, _)| *k != "TMPDIR")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let p = client_env
        .get("TMPDIR")
        .filter(|s| !s.is_empty() && Path::new(s.as_str()).is_absolute())
        .cloned()
        .unwrap_or_else(|| "/tmp".to_string());
    r.insert("TMPDIR".to_string(), p);
    r
}

/// `LocalEnvProvider.forCurrentOs(clientEnv).rewriteLocalEnv(...)`:
/// On Darwin, applies `XcodeLocalEnvProvider` (`TMPDIR` + `DEVELOPER_DIR`/`SDKROOT` when
/// `XCODE_VERSION_OVERRIDE` or `APPLE_SDK_PLATFORM` is present, keeping those override keys).
/// On other Unix platforms, applies `PosixLocalEnvProvider`.
pub fn rewrite_local_env_for_current_os(
    env: &BTreeMap<String, String>,
    client_env: &BTreeMap<String, String>,
    _execution_info: &BTreeMap<String, String>,
) -> io::Result<BTreeMap<String, String>> {
    if cfg!(target_os = "macos") {
        rewrite_xcode_local_env_with(env, client_env, query_developer_dir, query_sdk_root)
    } else {
        Ok(rewrite_local_env(env, client_env))
    }
}

// ---------------------------------------------------------------------------------------------
// SandboxHelpers
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct SandboxInputs {
    /// `None` means an empty file (`EmptyActionInput`).
    pub files: BTreeMap<PathFragment, Option<PathBuf>>,
    pub virtual_inputs: BTreeMap<PathFragment, Vec<u8>>,
    pub symlinks: BTreeMap<PathFragment, PathFragment>,
}

impl SandboxInputs {
    pub fn limited_copy(&self, allowed: &BTreeSet<PathFragment>) -> Self {
        Self {
            files: self
                .files
                .iter()
                .filter(|(k, _)| allowed.contains(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            virtual_inputs: BTreeMap::new(),
            symlinks: self
                .symlinks
                .iter()
                .filter(|(k, _)| allowed.contains(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SandboxOutputs {
    /// exec_path -> sandbox_path
    pub files: BTreeMap<PathFragment, PathFragment>,
    /// exec_path -> sandbox_path
    pub dirs: BTreeMap<PathFragment, PathFragment>,
}

/// In-memory representation of a worker/sandbox root (`SandboxHelpers.SandboxContents`).
#[derive(Debug, Clone, Default)]
pub struct SandboxContents {
    /// `None` = empty file, `Some(target)` = symlink target.
    pub file_map: BTreeMap<String, Option<PathFragment>>,
    pub dir_map: BTreeMap<String, SandboxContents>,
}

pub struct SandboxHelpers;

impl SandboxHelpers {
    /// `SandboxHelpers.getTmpDirPath`: `<work_dir>/../../<basename(work_dir.parent)>-tmp`.
    pub fn get_tmp_dir_path(work_dir: &Path) -> PathBuf {
        let parent = work_dir.parent().unwrap_or(work_dir);
        let grandparent = parent.parent().unwrap_or(parent);
        let base = parent
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("worker");
        grandparent.join(format!("{base}-tmp"))
    }

    /// `SandboxHelpers.getOutputs`: separates regular file outputs and directory outputs,
    /// applying `spawn.path_mapper` to compute the sandbox-side path.
    pub fn get_outputs(spawn: &Spawn) -> SandboxOutputs {
        let mut files = BTreeMap::new();
        let mut dirs = BTreeMap::new();
        let is_dir_out = |p: &PathFragment| -> bool {
            spawn.output_dirs.contains(p)
                || (spawn.mnemonic == "TestRunner"
                    && (p.path_string().ends_with("/test.outputs")
                        || p.path_string().ends_with("/_coverage")))
        };
        for out in &spawn.outputs {
            let mapped = spawn.path_mapper.map(out);
            if is_dir_out(out) {
                dirs.insert(out.clone(), mapped);
            } else {
                files.insert(out.clone(), mapped);
            }
        }
        for dir in &spawn.output_dirs {
            let mapped = spawn.path_mapper.map(dir);
            dirs.entry(dir.clone()).or_insert(mapped);
        }
        SandboxOutputs { files, dirs }
    }

    /// `SandboxHelpers.processInputFiles`: requires `HIERARCHICAL_COMPARATOR` map, drops entries
    /// nested under non-symlinks, keeps entries nested under symlinks, writes virtual inputs
    /// atomically to `exec_root`, stores `Empty` as `None`, and reads symlink targets.
    pub fn process_input_files(
        input_map: &BTreeMap<HierarchicalPath, ExpandedInput>,
        exec_root: &Path,
        cancel: Option<&CancelFlag>,
    ) -> io::Result<(SandboxInputs, BTreeSet<PathFragment>)> {
        let mut files = BTreeMap::new();
        let mut symlinks = BTreeMap::new();
        let mut virtual_inputs = BTreeMap::new();
        let mut empty_dirs = BTreeSet::new();
        let mut last_file: Option<PathFragment> = None;

        for (HierarchicalPath(path), entry) in input_map {
            if cancel.is_some_and(|c| c.is_cancelled()) {
                return Err(cancelled_error());
            }
            if let Some(prev) = &last_file {
                if path.starts_with(prev) {
                    continue;
                }
            }
            match entry {
                ExpandedInput::EmptyDirectory(_orig) => {
                    last_file = Some(path.clone());
                    empty_dirs.insert(path.clone());
                }
                ExpandedInput::Virtual(v) => {
                    let digest = v.atomically_write_relative_to(exec_root)?;
                    virtual_inputs.insert(v.exec_path().clone(), digest);
                    last_file = Some(path.clone());
                    files.insert(
                        path.clone(),
                        Some(exec_root.join(v.exec_path().path_string())),
                    );
                }
                ExpandedInput::Empty => {
                    last_file = Some(path.clone());
                    files.insert(path.clone(), None);
                }
                ExpandedInput::File {
                    exec_path,
                    is_symlink,
                } => {
                    let src = exec_root.join(exec_path.path_string());
                    let actual_symlink = *is_symlink
                        || std::fs::symlink_metadata(&src)
                            .map(|m| m.file_type().is_symlink())
                            .unwrap_or(false);
                    if *is_symlink {
                        last_file = None;
                        let target = std::fs::read_link(&src)?;
                        symlinks.insert(
                            path.clone(),
                            PathFragment::create(&target.to_string_lossy()),
                        );
                    } else if actual_symlink {
                        // A non-symlink ActionInput that happens to point to a symlink on disk:
                        // in Bazel `actionInput.isSymlink()` is false so it goes into `files`,
                        // while `last_file` stays None if nested under symlink? No, in Bazel
                        // `actionInput.isSymlink()` determines both! Keep `last_file = Some` for
                        // non-symlink ActionInput, matching `SandboxHelpers.java:707`.
                        last_file = Some(path.clone());
                        files.insert(path.clone(), Some(src));
                    } else {
                        last_file = Some(path.clone());
                        files.insert(path.clone(), Some(src));
                    }
                }
            }
        }
        Ok((
            SandboxInputs {
                files,
                virtual_inputs,
                symlinks,
            },
            empty_dirs,
        ))
    }

    /// `SandboxHelpers.populateInputsAndDirsToCreate`.
    pub fn populate_inputs_and_dirs_to_create<'a>(
        writable_sandbox_dirs: &BTreeSet<PathFragment>,
        inputs_to_create: &mut BTreeSet<PathFragment>,
        dirs_to_create: &mut BTreeSet<PathFragment>,
        input_files: impl Iterator<Item = &'a PathFragment>,
        outputs: &SandboxOutputs,
    ) {
        for inp in input_files {
            inputs_to_create.insert(inp.clone());
            if let Some(parent) = inp.parent_directory() {
                if !parent.is_empty() {
                    dirs_to_create.insert(parent);
                }
            }
        }
        for file in outputs.files.values() {
            if let Some(parent) = file.parent_directory() {
                if !parent.is_empty() {
                    dirs_to_create.insert(parent);
                }
            }
        }
        for dir in outputs.dirs.values() {
            if !dir.is_empty() {
                dirs_to_create.insert(dir.clone());
            }
        }
        for w in writable_sandbox_dirs {
            if !w.is_empty() {
                dirs_to_create.insert(w.clone());
            }
        }
    }

    /// `SandboxHelpers.createDirectories`.
    pub fn create_directories(
        dirs_to_create: &BTreeSet<PathFragment>,
        dir: &Path,
        strict: bool,
        cancel: Option<&CancelFlag>,
    ) -> io::Result<()> {
        let norm = |p: &Path| PathFragment::create(&p.to_string_lossy());
        let mut known: BTreeSet<PathFragment> = BTreeSet::new();
        known.insert(norm(dir));
        if let Some(p) = dir.parent() {
            known.insert(norm(p));
        }
        known.insert(norm(&Self::get_tmp_dir_path(dir)));

        fn create_dir_in_root(
            path: &Path,
            known: &mut BTreeSet<PathFragment>,
            sandbox_exec_root: &Path,
        ) -> io::Result<()> {
            let key = PathFragment::create(&path.to_string_lossy());
            if known.contains(&key) {
                return Ok(());
            }
            let parent = path.parent().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "Path {} is not under/siblings of sandboxExecRoot: {}",
                        path.display(),
                        sandbox_exec_root.display()
                    ),
                )
            })?;
            create_dir_in_root(parent, known, sandbox_exec_root)?;
            match std::fs::create_dir(path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
            known.insert(key);
            Ok(())
        }

        for path in dirs_to_create {
            if cancel.is_some_and(|c| c.is_cancelled()) {
                return Err(cancelled_error());
            }
            if path.is_empty() {
                continue;
            }
            if strict {
                if path.is_absolute() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("absolute path not allowed in strict sandbox: {path}"),
                    ));
                }
                if path.contains_uplevel_references() && path.is_multi_segment() {
                    if let Some(seg1) = path.get_segment(1) {
                        if seg1 == ".." || seg1.contains("..") {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidInput,
                                format!("{path} escapes the sandbox exec root."),
                            ));
                        }
                    }
                }
            }
            // Join segment by segment (using normalized PathFragment) so `..` stays relative to `dir`.
            let target = if path.is_absolute() {
                PathBuf::from(path.path_string())
            } else {
                let mut cur = dir.to_path_buf();
                for seg in path.segments() {
                    if seg == ".." {
                        cur = cur.parent().unwrap_or(&cur).to_path_buf();
                    } else if seg != "." && !seg.is_empty() {
                        cur.push(seg);
                    }
                }
                cur
            };
            create_dir_in_root(&target, &mut known, dir)?;
        }
        Ok(())
    }

    fn path_relative_to_work_dir(
        abs_path: &Path,
        work_dir: &Path,
        execroot: &Path,
    ) -> PathFragment {
        if let Ok(rel) = abs_path.strip_prefix(work_dir) {
            PathFragment::create(&rel.to_string_lossy())
        } else if let Ok(rel) = abs_path.strip_prefix(execroot) {
            PathFragment::create("../").get_relative(&PathFragment::create(&rel.to_string_lossy()))
        } else {
            PathFragment::create(&abs_path.to_string_lossy())
        }
    }

    fn expected_target(path: &PathFragment, inputs: &SandboxInputs) -> Option<PathFragment> {
        if let Some(Some(file)) = inputs.files.get(path) {
            return Some(PathFragment::create(&file.to_string_lossy()));
        }
        inputs.symlinks.get(path).cloned()
    }

    /// `SandboxHelpers.cleanExisting`: cleans `root` (`work_dir.parent()`) to match `inputs` &
    /// `dirs_to_create`, removing already-valid entries from `inputs_to_create` & `dirs_to_create`.
    pub fn clean_existing(
        root: &Path,
        inputs: &SandboxInputs,
        inputs_to_create: &mut BTreeSet<PathFragment>,
        dirs_to_create: &mut BTreeSet<PathFragment>,
        work_dir: &Path,
        sandbox_contents: Option<&SandboxContents>,
        cancel: Option<&CancelFlag>,
    ) -> io::Result<()> {
        let inaccessible_helper_dir = work_dir.join("inaccessibleHelperDir");
        if inaccessible_helper_dir.exists() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(
                    &inaccessible_helper_dir,
                    std::fs::Permissions::from_mode(0o700),
                );
            }
        }
        let mut prefix_dirs: BTreeSet<PathFragment> = BTreeSet::new();
        for dir in dirs_to_create.iter() {
            if cancel.is_some_and(|c| c.is_cancelled()) {
                return Err(cancelled_error());
            }
            let mut parent = dir.parent_directory();
            while let Some(p) = parent {
                if p.is_empty() || !prefix_dirs.insert(p.clone()) {
                    break;
                }
                parent = p.parent_directory();
            }
        }
        if let Some(contents) = sandbox_contents {
            Self::clean_recursively_in_memory(
                root,
                inputs,
                inputs_to_create,
                dirs_to_create,
                work_dir,
                &prefix_dirs,
                contents,
                cancel,
            )
        } else {
            Self::clean_recursively(
                root,
                inputs,
                inputs_to_create,
                dirs_to_create,
                work_dir,
                &prefix_dirs,
                cancel,
            )
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn clean_recursively_in_memory(
        root: &Path,
        inputs: &SandboxInputs,
        inputs_to_create: &mut BTreeSet<PathFragment>,
        dirs_to_create: &mut BTreeSet<PathFragment>,
        work_dir: &Path,
        prefix_dirs: &BTreeSet<PathFragment>,
        stash: &SandboxContents,
        cancel: Option<&CancelFlag>,
    ) -> io::Result<()> {
        let execroot = work_dir.parent().unwrap_or(work_dir);
        for (name, target) in &stash.file_map {
            if cancel.is_some_and(|c| c.is_cancelled()) {
                return Err(cancelled_error());
            }
            let abs_path = root.join(name);
            let rel = Self::path_relative_to_work_dir(&abs_path, work_dir, execroot);
            let expected = Self::expected_target(&rel, inputs);
            let should_keep = match target {
                None => matches!(inputs.files.get(&rel), Some(None)),
                Some(t) => expected.as_ref() == Some(t),
            };
            if should_keep {
                inputs_to_create.remove(&rel);
            } else {
                delete_tree(&abs_path)?;
            }
        }
        for (name, sub) in &stash.dir_map {
            if cancel.is_some_and(|c| c.is_cancelled()) {
                return Err(cancelled_error());
            }
            let abs_path = root.join(name);
            let rel = Self::path_relative_to_work_dir(&abs_path, work_dir, execroot);
            if dirs_to_create.contains(&rel) || prefix_dirs.contains(&rel) {
                Self::clean_recursively_in_memory(
                    &abs_path,
                    inputs,
                    inputs_to_create,
                    dirs_to_create,
                    work_dir,
                    prefix_dirs,
                    sub,
                    cancel,
                )?;
                dirs_to_create.remove(&rel);
            } else {
                delete_tree(&abs_path)?;
            }
        }
        Ok(())
    }

    fn clean_recursively(
        root: &Path,
        inputs: &SandboxInputs,
        inputs_to_create: &mut BTreeSet<PathFragment>,
        dirs_to_create: &mut BTreeSet<PathFragment>,
        work_dir: &Path,
        prefix_dirs: &BTreeSet<PathFragment>,
        cancel: Option<&CancelFlag>,
    ) -> io::Result<()> {
        let execroot = work_dir.parent().unwrap_or(work_dir);
        let rd = match std::fs::read_dir(root) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        for entry in rd {
            if cancel.is_some_and(|c| c.is_cancelled()) {
                return Err(cancelled_error());
            }
            let entry = entry?;
            let abs_path = entry.path();
            let ft = entry.file_type()?;
            if abs_path == work_dir {
                if ft.is_dir() && !ft.is_symlink() {
                    Self::clean_recursively(
                        &abs_path,
                        inputs,
                        inputs_to_create,
                        dirs_to_create,
                        work_dir,
                        prefix_dirs,
                        cancel,
                    )?;
                } else {
                    delete_tree(&abs_path)?;
                    std::fs::create_dir_all(work_dir)?;
                }
                continue;
            }
            let rel = Self::path_relative_to_work_dir(&abs_path, work_dir, execroot);
            let expected = Self::expected_target(&rel, inputs);
            if let Some(exp) = expected {
                if ft.is_symlink() {
                    if let Ok(actual) = std::fs::read_link(&abs_path) {
                        if PathFragment::create(&actual.to_string_lossy()) == exp {
                            inputs_to_create.remove(&rel);
                            continue;
                        }
                    }
                }
                delete_tree(&abs_path)?;
            } else if ft.is_dir() && !ft.is_symlink() {
                if dirs_to_create.contains(&rel) || prefix_dirs.contains(&rel) {
                    Self::clean_recursively(
                        &abs_path,
                        inputs,
                        inputs_to_create,
                        dirs_to_create,
                        work_dir,
                        prefix_dirs,
                        cancel,
                    )?;
                    dirs_to_create.remove(&rel);
                } else {
                    delete_tree(&abs_path)?;
                }
            } else if matches!(inputs.files.get(&rel), Some(None)) && ft.is_file() {
                inputs_to_create.remove(&rel);
            } else if !inputs_to_create.contains(&rel) {
                delete_tree(&abs_path)?;
            }
        }
        Ok(())
    }

    pub fn create_content_map(
        work_dir: &Path,
        inputs: &SandboxInputs,
        outputs: &SandboxOutputs,
    ) -> SandboxContents {
        fn insert_file(root: &mut SandboxContents, path: &PathFragment, val: Option<PathFragment>) {
            let mut cur = root;
            if let Some(parent) = path.parent_directory() {
                for seg in parent.segments() {
                    cur = cur.dir_map.entry(seg.to_string()).or_default();
                }
            }
            cur.file_map.insert(path.base_name().to_string(), val);
        }
        fn insert_dir(root: &mut SandboxContents, path: &PathFragment) {
            let mut cur = root;
            for seg in path.segments() {
                cur = cur.dir_map.entry(seg.to_string()).or_default();
            }
        }
        let mut ws = SandboxContents::default();
        for (k, v) in &inputs.files {
            if !k.contains_uplevel_references() {
                insert_file(
                    &mut ws,
                    k,
                    v.as_ref()
                        .map(|p| PathFragment::create(&p.to_string_lossy())),
                );
            }
        }
        for (k, v) in &inputs.symlinks {
            if !k.contains_uplevel_references() {
                insert_file(&mut ws, k, Some(v.clone()));
            }
        }
        for f in outputs.files.values() {
            if let Some(p) = f.parent_directory() {
                if !p.is_empty() && !p.contains_uplevel_references() {
                    insert_dir(&mut ws, &p);
                }
            }
        }
        for d in outputs.dirs.values() {
            if !d.is_empty() && !d.contains_uplevel_references() {
                insert_dir(&mut ws, d);
            }
        }
        let mut root = SandboxContents::default();
        let base = work_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("_main")
            .to_string();
        root.dir_map.insert(base, ws);
        root
    }

    /// `SandboxHelpers.moveOutputs`: moves `files` then `dirs` from `source_root/<sandbox_path>`
    /// to `target_root/<exec_path>`, deleting existing targets (`delete_tree`, aborting on error),
    /// and falling back to copy with `0644`/`0755` retry on unreadable files/directories.
    pub fn move_outputs(
        outputs: &SandboxOutputs,
        source_root: &Path,
        target_root: &Path,
    ) -> io::Result<()> {
        for (exec_path, sandbox_path) in outputs.files.iter().chain(outputs.dirs.iter()) {
            let source = source_root.join(sandbox_path.path_string());
            let target = target_root.join(exec_path.path_string());
            let md = match std::fs::symlink_metadata(&source) {
                Ok(m) => m,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            delete_tree(&target)?;
            if let Some(p) = target.parent() {
                std::fs::create_dir_all(p)?;
            }
            if std::fs::rename(&source, &target).is_err() {
                if md.file_type().is_symlink() {
                    #[cfg(unix)]
                    {
                        let t = std::fs::read_link(&source)?;
                        std::os::unix::fs::symlink(t, &target)?;
                    }
                    #[cfg(not(unix))]
                    std::fs::copy(&source, &target)?;
                } else if md.is_file() {
                    copy_file_with_chmod_retry(&source, &target)?;
                } else if md.is_dir() {
                    copy_dir_with_chmod_retry(&source, &target)?;
                } else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "Don't know how to copy {} into {} because it has an unsupported type",
                            source.display(),
                            target.display()
                        ),
                    ));
                }
            }
        }
        Ok(())
    }
}

fn copy_file_with_chmod_retry(source: &Path, target: &Path) -> io::Result<()> {
    match std::fs::copy(source, target) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(source, std::fs::Permissions::from_mode(0o644));
            }
            std::fs::copy(source, target)?;
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn copy_dir_with_chmod_retry(source: &Path, target: &Path) -> io::Result<()> {
    let rd = match std::fs::read_dir(source) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(source, std::fs::Permissions::from_mode(0o755));
            }
            std::fs::read_dir(source)?
        }
        Err(e) => return Err(e),
    };
    std::fs::create_dir(target)?;
    for e in rd {
        let e = e?;
        let src_child = e.path();
        let dst_child = target.join(e.file_name());
        let md = std::fs::symlink_metadata(&src_child)?;
        if md.file_type().is_symlink() {
            #[cfg(unix)]
            {
                let t = std::fs::read_link(&src_child)?;
                std::os::unix::fs::symlink(t, &dst_child)?;
            }
            #[cfg(not(unix))]
            std::fs::copy(&src_child, &dst_child)?;
        } else if md.is_dir() {
            copy_dir_with_chmod_retry(&src_child, &dst_child)?;
        } else if md.is_file() {
            copy_file_with_chmod_retry(&src_child, &dst_child)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Don't know how to copy {} to {}",
                    src_child.display(),
                    dst_child.display()
                ),
            ));
        }
    }
    Ok(())
}

/// `Spawns.requiresNetwork(spawn, defaultSandboxAllowNetwork)`.
pub fn requires_network(spawn: &Spawn, default_allow: bool) -> bool {
    if spawn.execution_info.contains_key("block-network") {
        return false;
    }
    if spawn.execution_info.contains_key("requires-network") {
        return true;
    }
    default_allow
}

/// `Spawns.mayBeSandboxed`.
pub fn may_be_sandboxed(spawn: &Spawn) -> bool {
    !(spawn.execution_info.contains_key("nosandbox")
        || spawn.execution_info.contains_key("no-sandbox")
        || spawn.execution_info.contains_key("local"))
}

/// Snapshot of a real regular-file input before a hermetic hardlinked sandbox run.
#[derive(Debug, Clone)]
pub struct HermeticInputSnapshot {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
}

/// Verifies that no real regular-file input captured in `snapshots` was mutated via its hardlink
/// during hermetic sandbox execution.
pub fn verify_hermetic_inputs_unchanged(snapshots: &[HermeticInputSnapshot]) -> io::Result<()> {
    for snap in snapshots {
        if let Ok(md) = std::fs::metadata(&snap.path) {
            if md.is_file() && (md.len() != snap.size || md.modified().ok() != snap.modified) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Input file '{}' was modified during hermetic sandbox execution.",
                        snap.path.display()
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Result of preparing a sandbox, kept for diagnostics (`--sandbox_debug`).
#[derive(Debug, Clone)]
pub struct SandboxLayout {
    pub sandbox_path: PathBuf,
    pub sandbox_exec_root: PathBuf,
    pub writable_dirs: BTreeSet<String>,
    pub outputs: SandboxOutputs,
    pub hermetic_file_snapshots: Vec<HermeticInputSnapshot>,
}

static INITIALIZED_SANDBOX_BASES: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> =
    OnceLock::new();
static STALE_TRASH_IDS: AtomicU64 = AtomicU64::new(1);

/// `SandboxModule.beforeCommand`: on the first build for a given `sandbox_base`, wipes any
/// leftover stale sandbox directories (`stale-trash-N` rename + helper chmod + `delete_tree`),
/// propagating setup I/O errors so callers report `Sandbox.INITIALIZATION_FAILURE` (exit 36).
pub fn initialize_sandbox_base(sandbox_base: &Path) -> io::Result<()> {
    let seen =
        INITIALIZED_SANDBOX_BASES.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    let first_time = seen.lock().unwrap().insert(sandbox_base.to_path_buf());
    if first_time && sandbox_base.exists() {
        chmod_inaccessible_helpers_writable(sandbox_base);
        let entries: Vec<PathBuf> = std::fs::read_dir(sandbox_base)?
            .collect::<io::Result<Vec<_>>>()?
            .into_iter()
            .map(|e| e.path())
            .collect();
        for entry in entries {
            let trash = sandbox_base.join(format!(
                "stale-trash-{}",
                STALE_TRASH_IDS.fetch_add(1, Ordering::SeqCst)
            ));
            let to_delete = if std::fs::rename(&entry, &trash).is_ok() {
                trash
            } else {
                entry
            };
            if delete_tree(&to_delete).is_err() {
                chmod_inaccessible_helpers_writable(sandbox_base);
                delete_tree(&to_delete)?;
            }
        }
    }
    std::fs::create_dir_all(sandbox_base)?;
    let helper_file = sandbox_base.join("inaccessibleHelperFile");
    if std::fs::symlink_metadata(&helper_file).is_err() {
        std::fs::File::create(&helper_file)?;
    }
    let helper_dir = sandbox_base.join("inaccessibleHelperDir");
    if std::fs::symlink_metadata(&helper_dir).is_err() {
        std::fs::create_dir_all(&helper_dir)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&helper_file, std::fs::Permissions::from_mode(0o000))?;
        std::fs::set_permissions(&helper_dir, std::fs::Permissions::from_mode(0o000))?;
    }
    Ok(())
}

fn chmod_inaccessible_helpers_writable(sandbox_base: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir = sandbox_base.join("inaccessibleHelperDir");
        if std::fs::symlink_metadata(&dir).is_ok() {
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        let file = sandbox_base.join("inaccessibleHelperFile");
        if std::fs::symlink_metadata(&file).is_ok() {
            let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
        }
    }
    #[cfg(not(unix))]
    let _ = sandbox_base;
}

/// `SandboxHelpers.mountAdditionalPaths`: resolves `(source, target)` pairs from
/// `--sandbox_add_mount_pair` (relative sources under `sandbox_exec_root`, later source wins for
/// the same target) into `(target, source)` bind-mount entries.
pub fn resolve_additional_mounts(
    mount_pairs: &[(String, String)],
    sandbox_exec_root: &Path,
) -> Result<Vec<(String, String)>, String> {
    let mut ordered_targets: Vec<String> = Vec::new();
    let mut target_to_source: HashMap<String, String> = HashMap::new();
    for (src_raw, dst_raw) in mount_pairs {
        if src_raw.is_empty() || dst_raw.is_empty() {
            return Err("Mount path must not be empty".to_string());
        }
        let dst_pf = PathFragment::create(dst_raw);
        if !dst_pf.is_absolute() {
            return Err(format!(
                "Mount target '{dst_raw}' must be an absolute path."
            ));
        }
        let src_path = if Path::new(src_raw).is_absolute() {
            PathBuf::from(PathFragment::create(src_raw).path_string())
        } else {
            let rel = PathFragment::create(src_raw);
            sandbox_exec_root.join(rel.path_string())
        };
        let dst_norm = dst_pf.path_string().to_string();
        if !target_to_source.contains_key(&dst_norm) {
            ordered_targets.push(dst_norm.clone());
        }
        target_to_source.insert(dst_norm, src_path.to_string_lossy().into_owned());
    }
    Ok(ordered_targets
        .into_iter()
        .filter_map(|t| target_to_source.remove(&t).map(|s| (t, s)))
        .collect())
}

/// `HardlinkedSandboxedSpawn.copyFile`: resolves symlinks, hardlinks regular files (falling back
/// to a full file copy when hardlinking fails or `force_copy_for_debug` is true), and recursively
/// walks directories hardlinking each child.
fn materialize_hardlinked_input(
    src: &Path,
    dest: &Path,
    force_copy_for_debug: bool,
) -> io::Result<()> {
    let resolved = std::fs::canonicalize(src)?;
    let md = std::fs::metadata(&resolved)?;
    if md.is_dir() {
        std::fs::create_dir_all(dest)?;
        for entry in std::fs::read_dir(&resolved)? {
            let entry = entry?;
            let child_src = entry.path();
            let child_dst = dest.join(entry.file_name());
            materialize_hardlinked_input(&child_src, &child_dst, force_copy_for_debug)?;
        }
        Ok(())
    } else if md.is_file() {
        if force_copy_for_debug || std::fs::hard_link(&resolved, dest).is_err() {
            std::fs::copy(&resolved, dest)?;
        }
        Ok(())
    } else {
        Ok(())
    }
}

/// `SpawnExecutionContext.getId()`: a counter that lives as long as the server (bazel's sandbox
/// directories are `<base>/<runner>/<id>`), not per command.
static SPAWN_EXECUTION_IDS: AtomicU64 = AtomicU64::new(1);

pub struct SandboxRunner {
    pub kind: SandboxKind,
    pub exec_root: PathBuf,
    pub sandbox_base: PathBuf,
    pub options: SandboxOptions,
    pub client_env: BTreeMap<String, String>,
    pub kill_delay: Duration,
    always_writable: BTreeSet<String>,
    always_writable_error: Option<String>,
    inaccessible_helper_file: Option<PathBuf>,
    inaccessible_helper_dir: Option<PathBuf>,
    pub init_warning: Option<String>,
    #[allow(dead_code)]
    next_id: AtomicU64,
}

impl SandboxRunner {
    pub fn new(
        kind: SandboxKind,
        exec_root: &Path,
        sandbox_base: &Path,
        options: SandboxOptions,
        client_env: BTreeMap<String, String>,
        kill_delay: Option<Duration>,
    ) -> Self {
        let (always_writable, always_writable_error) = if kind == SandboxKind::Darwin {
            match Self::always_writable_dirs(&client_env) {
                Ok(set) => (set, None),
                Err(e) => (BTreeSet::new(), Some(e.to_string())),
            }
        } else {
            (BTreeSet::new(), None)
        };
        let (inaccessible_helper_file, inaccessible_helper_dir, init_warning) =
            if kind == SandboxKind::Linux && !options.block_paths.is_empty() {
                crate::linux::create_inaccessible_helpers(sandbox_base, &options.block_paths)
            } else {
                (None, None, None)
            };
        Self {
            kind,
            exec_root: exec_root.to_path_buf(),
            sandbox_base: sandbox_base.to_path_buf(),
            options,
            client_env,
            kill_delay: kill_delay.unwrap_or(Duration::from_millis(DEFAULT_KILL_DELAY_MS)),
            always_writable,
            always_writable_error,
            inaccessible_helper_file,
            inaccessible_helper_dir,
            init_warning,
            next_id: AtomicU64::new(1),
        }
    }

    /// `DarwinSandboxedSpawnRunner.getAlwaysWritableDirs`.
    fn always_writable_dirs(client_env: &BTreeMap<String, String>) -> io::Result<BTreeSet<String>> {
        let mut w = BTreeSet::new();
        let mut add = |p: &Path| -> io::Result<()> {
            if std::fs::symlink_metadata(p).is_ok() {
                let c = std::fs::canonicalize(p)?;
                w.insert(c.to_string_lossy().into_owned());
            }
            Ok(())
        };
        for p in ["/dev", "/tmp", "/private/tmp", "/private/var/tmp"] {
            add(Path::new(p))?;
        }
        for k in ["DARWIN_USER_TEMP_DIR", "DARWIN_USER_CACHE_DIR"] {
            if let Some(v) = client_env.get(k) {
                add(Path::new(v))?;
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            for sub in ["Library/Caches", "Library/Logs", "Library/Developer"] {
                add(&home.join(sub))?;
            }
        }
        Ok(w)
    }

    pub fn name(&self) -> &'static str {
        self.kind.name()
    }

    /// `AbstractSandboxSpawnRunner.getWritableDirs`.
    fn writable_dirs(
        &self,
        sandbox_exec_root: &Path,
        env: &BTreeMap<String, String>,
    ) -> io::Result<BTreeSet<String>> {
        let mut w = BTreeSet::new();
        let sbx_norm = PathFragment::create(&sandbox_exec_root.to_string_lossy());
        w.insert(sandbox_exec_root.to_string_lossy().into_owned());
        if let Some(r) = resolve(&sandbox_exec_root.to_string_lossy()) {
            w.insert(r);
        }
        let add = |w: &mut BTreeSet<String>, var: &str, value: &str| -> io::Result<()> {
            let raw_pf = if Path::new(value).is_absolute() {
                PathFragment::create(value)
            } else {
                sbx_norm.get_relative(&PathFragment::create(value))
            };
            let p = PathBuf::from(raw_pf.path_string());
            if raw_pf.starts_with(&sbx_norm) {
                // Below the sandbox exec root: already writable, created during sandbox setup.
                w.insert(p.to_string_lossy().into_owned());
            } else if std::fs::symlink_metadata(&p).is_ok() {
                let canon = std::fs::canonicalize(&p)?;
                w.insert(canon.to_string_lossy().into_owned());
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "Cannot resolve symlinks in {var} because it doesn't exist: \"{}\"",
                        p.display()
                    ),
                ));
            }
            Ok(())
        };
        if let Some(t) = env.get("TEST_TMPDIR") {
            add(&mut w, "TEST_TMPDIR", t)?;
        }
        let Some(tmpdir) = env.get("TMPDIR") else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TMPDIR environment variable is not set",
            ));
        };
        add(&mut w, "TMPDIR", tmpdir)?;
        for p in &self.options.writable_paths {
            let path = Path::new(p);
            if !path.is_absolute() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Sandbox writable path is not absolute: {p}"),
                ));
            }
            let canon = std::fs::canonicalize(path)?;
            w.insert(p.clone());
            w.insert(canon.to_string_lossy().into_owned());
        }
        Ok(w)
    }

    /// `SandboxOptions.getInaccessiblePaths` (resolved symlinks where possible).
    fn inaccessible_paths(&self) -> BTreeSet<String> {
        self.options
            .block_paths
            .iter()
            .map(|p| resolve_best_effort(Path::new(p)))
            .collect()
    }

    /// Prepares the sandbox directory (`prepareSpawn` + `createFileSystem`): execroot named like
    /// the real one, expanded/symlinked inputs, created output/writable directories.
    pub fn prepare(
        &self,
        spawn: &Spawn,
        id: u64,
    ) -> io::Result<(SandboxLayout, BTreeMap<String, String>)> {
        self.prepare_with_hooks(spawn, id, &ExecHooks::default())
    }

    fn prepare_with_hooks(
        &self,
        spawn: &Spawn,
        id: u64,
        hooks: &ExecHooks,
    ) -> io::Result<(SandboxLayout, BTreeMap<String, String>)> {
        if let Some(err) = &self.always_writable_error {
            return Err(io::Error::new(io::ErrorKind::Other, err.clone()));
        }
        let sandbox_path = self.sandbox_base.join(self.name()).join(id.to_string());
        std::fs::create_dir_all(&sandbox_path)?;
        // The execroot of the sandboxed process must end with the workspace name.
        let ws_name = self
            .exec_root
            .file_name()
            .map(|s| s.to_os_string())
            .unwrap_or_else(|| "_main".into());
        let sandbox_exec_root = sandbox_path.join("execroot").join(ws_name);
        std::fs::create_dir_all(&sandbox_exec_root)?;
        let env =
            rewrite_local_env_for_current_os(&spawn.env, &self.client_env, &spawn.execution_info)?;

        let mut writable = self.always_writable.clone();
        let extra = self.writable_dirs(&sandbox_exec_root, &env)?;
        writable.extend(extra.iter().cloned());

        let input_map = spawn
            .build_input_mapping(&PathFragment::empty())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let (inputs, empty_dirs) = SandboxHelpers::process_input_files(
            &input_map,
            &self.exec_root,
            hooks.cancel.as_ref(),
        )?;
        let outputs = SandboxHelpers::get_outputs(spawn);

        let mut writable_sandbox_dirs: BTreeSet<PathFragment> = BTreeSet::new();
        for w in &writable {
            if let Ok(rel) = Path::new(w).strip_prefix(&sandbox_exec_root) {
                let pf = PathFragment::create(&rel.to_string_lossy());
                if !pf.is_empty() {
                    writable_sandbox_dirs.insert(pf);
                }
            }
        }
        let mut inputs_to_create: BTreeSet<PathFragment> = BTreeSet::new();
        let mut dirs_to_create: BTreeSet<PathFragment> = BTreeSet::new();
        SandboxHelpers::populate_inputs_and_dirs_to_create(
            &writable_sandbox_dirs,
            &mut inputs_to_create,
            &mut dirs_to_create,
            inputs.files.keys().chain(inputs.symlinks.keys()),
            &outputs,
        );
        for d in empty_dirs {
            if !d.is_empty() {
                dirs_to_create.insert(d);
            }
        }
        SandboxHelpers::create_directories(
            &dirs_to_create,
            &sandbox_exec_root,
            true,
            hooks.cancel.as_ref(),
        )?;
        let use_hardlinks =
            self.kind == SandboxKind::Linux && self.options.use_hermetic_linux_sandbox;
        let mut hermetic_file_snapshots = Vec::new();
        for fragment in &inputs_to_create {
            if hooks.is_cancelled() {
                return Err(cancelled_error());
            }
            let dest = sandbox_exec_root.join(fragment.path_string());
            if let Some(file_dest) = inputs.files.get(fragment) {
                match file_dest {
                    Some(src) => {
                        if use_hardlinks {
                            if !inputs.virtual_inputs.contains_key(fragment) {
                                if let Ok(canon) = std::fs::canonicalize(src) {
                                    if let Ok(md) = std::fs::metadata(&canon) {
                                        if md.is_file() {
                                            hermetic_file_snapshots.push(HermeticInputSnapshot {
                                                path: canon,
                                                size: md.len(),
                                                modified: md.modified().ok(),
                                            });
                                        }
                                    }
                                }
                            }
                            materialize_hardlinked_input(src, &dest, self.options.debug)?;
                        } else {
                            #[cfg(unix)]
                            std::os::unix::fs::symlink(src, &dest)?;
                            #[cfg(windows)]
                            {
                                let linked = if src.is_dir() {
                                    std::os::windows::fs::symlink_dir(src, &dest).is_ok()
                                } else {
                                    std::os::windows::fs::symlink_file(src, &dest).is_ok()
                                };
                                if !linked {
                                    std::fs::copy(src, &dest)?;
                                }
                            }
                            #[cfg(not(any(unix, windows)))]
                            std::fs::copy(src, &dest)?;
                        }
                    }
                    None => {
                        std::fs::write(&dest, b"")?;
                    }
                }
            } else if let Some(symlink_dest) = inputs.symlinks.get(fragment) {
                #[cfg(unix)]
                std::os::unix::fs::symlink(symlink_dest.path_string(), &dest)?;
                #[cfg(windows)]
                {
                    let target = Path::new(symlink_dest.path_string());
                    let linked = if target.is_dir() {
                        std::os::windows::fs::symlink_dir(target, &dest).is_ok()
                    } else {
                        std::os::windows::fs::symlink_file(target, &dest).is_ok()
                    };
                    if !linked {
                        if target.is_file() {
                            std::fs::copy(target, &dest)?;
                        } else {
                            std::fs::write(&dest, b"")?;
                        }
                    }
                }
                #[cfg(not(any(unix, windows)))]
                let _ = symlink_dest;
            }
        }
        // Stage `<tool>.runfiles` trees (and `.runfiles_manifest`) for tool executables into the sandbox.
        for t in &spawn.tool_inputs {
            let ps = t.path_string();
            let rf_rel = format!("{ps}.runfiles");
            let rf_src = self.exec_root.join(&rf_rel);
            if rf_src.is_dir() {
                let rf_dest = sandbox_exec_root.join(&rf_rel);
                stage_runfiles_dir(&rf_src, &rf_dest)?;
            }
            let mf_rel = format!("{ps}.runfiles_manifest");
            let mf_src = self.exec_root.join(&mf_rel);
            if mf_src.is_file() {
                let mf_dest = sandbox_exec_root.join(&mf_rel);
                if let Some(p) = mf_dest.parent() {
                    std::fs::create_dir_all(p)?;
                }
                if std::fs::symlink_metadata(&mf_dest).is_err() {
                    #[cfg(unix)]
                    let _ = std::os::unix::fs::symlink(&mf_src, &mf_dest);
                }
            }
        }
        Ok((
            SandboxLayout {
                sandbox_path,
                sandbox_exec_root,
                writable_dirs: writable,
                outputs,
                hermetic_file_snapshots,
            },
            env,
        ))
    }

    /// `SandboxHelpers.moveOutputs`.
    #[allow(dead_code)]
    fn move_outputs(&self, spawn: &Spawn, sandbox_exec_root: &Path) -> io::Result<()> {
        let outputs = SandboxHelpers::get_outputs(spawn);
        SandboxHelpers::move_outputs(&outputs, sandbox_exec_root, &self.exec_root)
    }

    /// Runs `spawn` in a fresh sandbox.
    pub fn exec(&self, spawn: &Spawn) -> io::Result<SpawnResult> {
        self.exec_with(spawn, &crate::ExecHooks::default())
    }

    /// [`Self::exec`] with dynamic-execution hooks: the process is killed when `hooks.cancel`
    /// is raised and `hooks.lock_outputs` is consulted after the process exited and before its
    /// outputs are moved out of the sandbox (`stopConcurrentSpawns` in `AbstractSandboxSpawnRunner`).
    pub fn exec_with(&self, spawn: &Spawn, hooks: &crate::ExecHooks) -> io::Result<SpawnResult> {
        let id = SPAWN_EXECUTION_IDS.fetch_add(1, Ordering::SeqCst);
        let mk = |status, code, stdout: Vec<u8>, stderr: Vec<u8>| SpawnResult {
            status,
            exit_code: code,
            stdout,
            stderr,
            runner_name: self.name().to_string(),
            ..Default::default()
        };
        let parsed_timeout = match spawn.parse_timeout() {
            Ok(t) => t,
            Err(msg) => {
                let mut r = mk(
                    SpawnStatus::InvalidTimeout,
                    1,
                    Vec::new(),
                    format!("{msg}\n").into_bytes(),
                );
                r.failure_message = Some(msg);
                return Ok(r);
            }
        };
        if spawn.argv.is_empty() {
            return Ok(mk(
                SpawnStatus::ExecutionFailed,
                1,
                Vec::new(),
                b"empty command line\n".to_vec(),
            ));
        }

        if !spawn.runfiles_trees.is_empty() {
            if let Some(updater) = &hooks.runfiles_updater {
                updater.update_runfiles(&self.exec_root, &spawn.runfiles_trees)?;
            } else {
                RunfilesTreeUpdater::new()
                    .update_runfiles(&self.exec_root, &spawn.runfiles_trees)?;
            }
        }

        let (layout, env) = self.prepare_with_hooks(spawn, id, hooks)?;
        if matches!(parsed_timeout, Some(s) if s < 0) {
            if !self.options.debug {
                delete_tree(&layout.sandbox_path)?;
            }
            return Ok(mk(SpawnStatus::Timeout, 142, Vec::new(), Vec::new()));
        }
        let timeout_dur = parsed_timeout
            .filter(|&s| s > 0)
            .map(|s| Duration::from_secs(s as u64));
        let graceful = spawn
            .execution_info
            .contains_key("supports-graceful-termination")
            || self.options.process_wrapper_graceful_sigterm;
        let stats_path = layout.sandbox_path.join("stats.out");
        let mut argv = spawn.argv.clone();
        let mut debug_output: Option<String> = None;
        let mut linux_sandbox_used = false;
        let pw_bin = self
            .options
            .process_wrapper_path
            .as_deref()
            .or(spawn.process_wrapper_path.as_deref());
        let mut external_pw_used = false;
        if self.kind == SandboxKind::ProcessWrapper {
            if let Some(pw) = pw_bin {
                let b = PwCommandLineBuilder {
                    wrapper: pw.to_string(),
                    args: argv,
                    timeout: timeout_dur,
                    kill_delay: Some(spawn.kill_delay.unwrap_or(self.kill_delay)),
                    stats_path: Some(stats_path.to_string_lossy().into_owned()),
                    graceful_sigterm: graceful,
                    ..Default::default()
                };
                argv = b.build();
                external_pw_used = true;
            }
        } else if self.kind == SandboxKind::Darwin {
            let config = layout.sandbox_path.join("sandbox.sb");
            let allow_net = requires_network(spawn, self.options.default_allow_network);
            let darwin_writable: BTreeSet<String> = layout
                .writable_dirs
                .iter()
                .map(|p| resolve_best_effort(Path::new(p)))
                .collect();
            let darwin_stats_path = resolve_best_effort(&stats_path);
            let text = darwin_sandbox_config(
                &darwin_writable,
                &self.inaccessible_paths(),
                allow_net,
                Some(&darwin_stats_path),
            );
            std::fs::write(&config, text)?;
            if let Some(pw) = pw_bin {
                let b = PwCommandLineBuilder {
                    wrapper: pw.to_string(),
                    args: argv,
                    timeout: timeout_dur,
                    kill_delay: Some(spawn.kill_delay.unwrap_or(self.kill_delay)),
                    stats_path: Some(stats_path.to_string_lossy().into_owned()),
                    graceful_sigterm: graceful,
                    ..Default::default()
                };
                argv = b.build();
                external_pw_used = true;
            }
            let mut full = vec![
                SANDBOX_EXEC.to_string(),
                "-f".to_string(),
                config.to_string_lossy().into_owned(),
            ];
            full.extend(argv);
            argv = full;
        } else if self.kind == SandboxKind::Linux {
            let user_mounts = match resolve_additional_mounts(
                &self.options.mount_pairs,
                &layout.sandbox_exec_root,
            ) {
                Ok(m) => m,
                Err(msg) => {
                    if !self.options.debug {
                        let _ = delete_tree(&layout.sandbox_path);
                    }
                    let mut r = mk(
                        SpawnStatus::ExecutionFailed,
                        1,
                        Vec::new(),
                        format!("{msg}\n").into_bytes(),
                    );
                    r.failure_message = Some(msg);
                    return Ok(r);
                }
            };
            if let Err(msg) = crate::linux::validate_bind_mounts(&user_mounts) {
                if !self.options.debug {
                    let _ = delete_tree(&layout.sandbox_path);
                }
                let mut r = mk(
                    SpawnStatus::ExecutionFailed,
                    1,
                    Vec::new(),
                    format!("{msg}\n").into_bytes(),
                );
                r.failure_message = Some(msg);
                return Ok(r);
            }
            let ls_bin = self
                .options
                .linux_sandbox_path
                .clone()
                .or_else(|| find_linux_sandbox_binary(&self.exec_root));
            if let Some(ls_bin) = ls_bin {
                let mut writable = layout.writable_dirs.clone();
                if let Some(shm) = resolve("/dev/shm") {
                    writable.insert(shm);
                } else {
                    writable.insert("/dev/shm".to_string());
                }
                writable.insert("/tmp".to_string());

                let has_explicit_tmp_mount = user_mounts.iter().any(|(t, _)| t == "/tmp");
                let has_tmp_tmpfs = self.options.tmpfs_paths.iter().any(|p| p == "/tmp");
                let use_hermetic_tmp = !self.options.use_hermetic_linux_sandbox
                    && !has_explicit_tmp_mount
                    && !has_tmp_tmpfs;

                let mut bind_mounts = Vec::new();
                if use_hermetic_tmp {
                    let hermetic_tmp = layout.sandbox_path.join("_hermetic_tmp");
                    std::fs::create_dir_all(&hermetic_tmp)?;
                    let mut seen_mounts = BTreeSet::new();
                    let mut tmp_roots = vec![self.sandbox_base.clone(), self.exec_root.clone()];
                    for w in &writable {
                        tmp_roots.push(PathBuf::from(w));
                    }
                    for p in &tmp_roots {
                        if let Ok(rel) = p.strip_prefix("/tmp") {
                            if let Some(first) = rel.components().next() {
                                let first_s = first.as_os_str().to_string_lossy();
                                if !first_s.is_empty() && seen_mounts.insert(first_s.to_string()) {
                                    let src = format!("/tmp/{first_s}");
                                    let dst = hermetic_tmp.join(&*first_s);
                                    std::fs::create_dir_all(&dst)?;
                                    bind_mounts.push((dst.to_string_lossy().into_owned(), src));
                                }
                            }
                        }
                    }
                    for (target, source) in &user_mounts {
                        if let Ok(rel) = Path::new(target).strip_prefix("/tmp") {
                            let rewritten = hermetic_tmp.join(rel);
                            if let Some(p) = rewritten.parent() {
                                std::fs::create_dir_all(p)?;
                            }
                            if std::fs::metadata(source).is_ok_and(|m| m.is_dir()) {
                                std::fs::create_dir_all(&rewritten)?;
                            } else if !rewritten.exists() {
                                let _ = std::fs::File::create(&rewritten);
                            }
                            bind_mounts
                                .push((rewritten.to_string_lossy().into_owned(), source.clone()));
                        } else {
                            bind_mounts.push((target.clone(), source.clone()));
                        }
                    }
                    for tp in &self.options.tmpfs_paths {
                        if let Ok(rel) = Path::new(tp).strip_prefix("/tmp") {
                            std::fs::create_dir_all(hermetic_tmp.join(rel))?;
                        }
                    }
                    bind_mounts.push((
                        "/tmp".to_string(),
                        hermetic_tmp.to_string_lossy().into_owned(),
                    ));
                } else {
                    bind_mounts.extend(user_mounts);
                }

                if let (Some(helper_file), Some(helper_dir)) = (
                    &self.inaccessible_helper_file,
                    &self.inaccessible_helper_dir,
                ) {
                    for blocked in self.inaccessible_paths() {
                        if let Ok(md) = std::fs::metadata(&blocked) {
                            let src = if md.is_dir() {
                                helper_dir.to_string_lossy().into_owned()
                            } else {
                                helper_file.to_string_lossy().into_owned()
                            };
                            bind_mounts.push((blocked, src));
                        }
                    }
                }

                let mut b = crate::linux::LinuxSandboxCommandLineBuilder::new(&ls_bin);
                b.add_execution_info(&spawn.execution_info);
                if graceful {
                    b.sigint_sends_sigterm = true;
                }
                b.writable_files_and_directories = writable.into_iter().collect();
                b.tmpfs_directories = self.options.tmpfs_paths.clone();
                b.bind_mounts = bind_mounts;
                b.use_fake_hostname = self.options.fake_hostname;
                b.enable_pseudoterminal = self.options.explicit_pseudoterminal;
                if !requires_network(spawn, self.options.default_allow_network) {
                    b.create_network_namespace = crate::linux::NetworkNamespace::NetnsWithLoopback;
                }
                b.kill_delay = Some(spawn.kill_delay.unwrap_or(self.kill_delay));
                if self.options.debug {
                    b.sandbox_debug_path = Some(
                        layout
                            .sandbox_path
                            .join("debug.out")
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
                b.timeout = timeout_dur;
                if spawn.execution_info.contains_key("requires-fakeroot") {
                    b.use_fake_root = true;
                } else if self.options.fake_username {
                    b.use_fake_username = true;
                }
                if self.options.use_hermetic_linux_sandbox {
                    b.hermetic_sandbox_path =
                        Some(layout.sandbox_path.to_string_lossy().into_owned());
                    b.working_directory =
                        Some(layout.sandbox_exec_root.to_string_lossy().into_owned());
                }
                b.statistics_path = Some(stats_path.to_string_lossy().into_owned());
                if let Ok(built) = b.build_for_command(&argv) {
                    argv = built;
                    linux_sandbox_used = true;
                    if self.options.debug {
                        if let Ok(interactive) =
                            b.build_for_command(&["/bin/sh".to_string(), "-i".to_string()])
                        {
                            debug_output = Some(interactive.join("\0"));
                        }
                    }
                }
            }
        }
        let use_external_timeout = linux_sandbox_used || external_pw_used;
        let opt = ProcessWrapperOptions {
            graceful_sigterm: graceful,
            timeout_secs: if use_external_timeout {
                0.0
            } else {
                timeout_dur.map(|d| d.as_secs_f64()).unwrap_or(0.0)
            },
            kill_delay_secs: spawn.kill_delay.unwrap_or(self.kill_delay).as_secs_f64(),
            stats_path: if use_external_timeout {
                None
            } else {
                Some(stats_path.to_string_lossy().into_owned())
            },
            args: argv.clone(),
            cancel: hooks.cancel.clone(),
            close_stdin: true,
            ..Default::default()
        };
        let cwd = layout.sandbox_exec_root.to_string_lossy().into_owned();
        let outcome = process_wrapper::run(&opt, Some(&env), Some(&cwd));
        if matches!(&outcome, Ok(o) if o.cancelled) {
            if !self.options.debug {
                delete_tree(&layout.sandbox_path)?;
            }
            return Err(crate::cancelled_error());
        }
        let mut result = match outcome {
            Ok(o) => {
                let is_ext_timeout = use_external_timeout
                    && timeout_dur
                        .map(|t| {
                            o.wall.as_secs_f64() + 0.1 >= t.as_secs_f64()
                                && matches!(
                                    o.termination,
                                    Termination::Exited(142 | 137 | 143) | Termination::Signaled(_)
                                )
                        })
                        .unwrap_or(false);
                let post_wall_timeout = timeout_dur.is_some_and(|t| o.wall > t);
                let timed_out = o.timed_out || is_ext_timeout || post_wall_timeout;
                let (status, mut code) = match o.termination {
                    _ if timed_out => (SpawnStatus::Timeout, 142),
                    Termination::Exited(0) => (SpawnStatus::Success, 0),
                    t => (SpawnStatus::NonZeroExit, t.exit_code()),
                };
                let mut stderr = o.stderr;
                if code == 71 {
                    let text = String::from_utf8_lossy(&stderr).to_string();
                    if let Some(rest) = text.strip_prefix("sandbox-exec: execvp() of '") {
                        if let Some((prog, err)) = rest.split_once("' failed: ") {
                            stderr = format!(
                                "src/main/tools/process-wrapper-legacy.cc:80: \"execvp({prog}, ...)\": {err}"
                            )
                            .into_bytes();
                            code = 1;
                        }
                    }
                }
                let mut r = mk(status, code, o.stdout, stderr);
                r.wall_time_ms = o.wall.as_millis() as u64;
                r.usage = o.usage;
                r
            }
            Err(e) => {
                let prog = spawn.argv.first().map(String::as_str).unwrap_or("");
                let msg = e.to_string();
                let msg = msg.split(" (os error").next().unwrap_or(&msg);
                mk(
                    SpawnStatus::NonZeroExit,
                    1,
                    Vec::new(),
                    format!("src/main/tools/process-wrapper-legacy.cc:80: \"execvp({prog}, ...)\": {msg}\n").into_bytes(),
                )
            }
        };
        if let Err(e) = verify_hermetic_inputs_unchanged(&layout.hermetic_file_snapshots) {
            if !self.options.debug {
                let _ = delete_tree(&layout.sandbox_path);
            }
            return Err(e);
        }
        // `stopConcurrentSpawns`: the dynamic-execution branch that gets here first wins.
        if hooks.is_cancelled() || !hooks.lock(result.exit_code) {
            if !self.options.debug {
                delete_tree(&layout.sandbox_path)?;
            }
            return Err(crate::cancelled_error());
        }
        // Outputs are copied back even when the command failed; IO errors propagate as execution exceptions.
        let move_res = SandboxHelpers::move_outputs(
            &layout.outputs,
            &layout.sandbox_exec_root,
            &self.exec_root,
        );
        if let Err(e) = move_res {
            if !self.options.debug {
                let _ = delete_tree(&layout.sandbox_path);
            }
            return Err(e);
        }
        if result.status == SpawnStatus::Success {
            let missing: Vec<String> = spawn
                .mandatory_outputs()
                .filter(|o| {
                    std::fs::symlink_metadata(self.exec_root.join(o.path_string())).is_err()
                })
                .map(|o| o.path_string().to_string())
                .collect();
            if !missing.is_empty() {
                result.status = SpawnStatus::MissingOutputs;
                result.missing = missing;
            }
        }
        let mut display_argv = spawn.argv.clone();
        if self.options.debug {
            display_argv = argv;
            if linux_sandbox_used {
                let dbg_file = layout.sandbox_path.join("debug.out");
                let dbg_content = std::fs::read_to_string(&dbg_file).unwrap_or_default();
                let interactive_args: Vec<String> = debug_output
                    .take()
                    .map(|s| s.split('\0').map(str::to_string).collect())
                    .unwrap_or_default();
                let mut combined = String::new();
                if !dbg_content.is_empty() {
                    combined.push_str(&dbg_content);
                    combined.push('\n');
                }
                if !interactive_args.is_empty() {
                    combined.push_str("Run this command to start an interactive shell in an identical sandboxed environment:\n");
                    combined.push_str(&format_interactive_debug_command(
                        &interactive_args,
                        &env,
                        &cwd,
                    ));
                }
                if !combined.is_empty() {
                    debug_output = Some(combined);
                }
            }
        }
        result.executed_command = Some(crate::ExecutedCommand {
            argv: display_argv,
            env,
            cwd,
            debug_output,
        });
        if !self.options.debug {
            delete_tree(&layout.sandbox_path)?;
        }
        Ok(result)
    }

    /// `cleanupSandboxBase`: removes the per-runner directory and inaccessible helpers.
    pub fn cleanup_sandbox_base(&self) -> io::Result<()> {
        let root = self.sandbox_base.join(self.name());
        if root.exists() {
            for e in std::fs::read_dir(&root)? {
                let p = e?.path();
                delete_tree(&p)?;
            }
            let _ = std::fs::remove_dir(&root);
        }
        chmod_inaccessible_helpers_writable(&self.sandbox_base);
        let _ = delete_tree(&self.sandbox_base.join("inaccessibleHelperDir"));
        let _ = delete_tree(&self.sandbox_base.join("inaccessibleHelperFile"));
        let _ = std::fs::remove_dir(&self.sandbox_base);
        Ok(())
    }
}

fn stage_runfiles_dir(src_dir: &Path, dest_dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dest_dir)?;
    for entry in std::fs::read_dir(src_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let src = entry.path();
        let dest = dest_dir.join(&name);
        let md = std::fs::symlink_metadata(&src)?;
        if md.is_dir() {
            stage_runfiles_dir(&src, &dest)?;
        } else if std::fs::symlink_metadata(&dest).is_err() {
            #[cfg(unix)]
            {
                let target = if md.file_type().is_symlink() {
                    std::fs::read_link(&src).unwrap_or_else(|_| src.clone())
                } else {
                    src.clone()
                };
                std::os::unix::fs::symlink(&target, &dest)?;
            }
        }
    }
    Ok(())
}

/// `SandboxModule.computeSandboxBase`: `<output_base>/sandbox`, or
/// `<--sandbox_base>/<product>-sandbox.<md5(output_base)>`.
pub fn compute_sandbox_base(output_base: &str, sandbox_base_opt: &str) -> PathBuf {
    if sandbox_base_opt.is_empty() {
        PathBuf::from(output_base).join("sandbox")
    } else {
        let dir = format!(
            "bazel-sandbox.{}",
            zel_util::md5::hex_digest(output_base.as_bytes())
        );
        PathBuf::from(sandbox_base_opt).join(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zel-sbx2-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::canonicalize(&d).unwrap()
    }

    fn spawn(script: &str, inputs: &[&str], outputs: &[&str]) -> Spawn {
        Spawn {
            mnemonic: "Genrule".into(),
            owner_label: "//:x".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
            env: BTreeMap::new(),
            inputs: inputs.iter().map(|s| PathFragment::create(s)).collect(),
            outputs: outputs.iter().map(|s| PathFragment::create(s)).collect(),
            execution_info: BTreeMap::new(),
            ..Default::default()
        }
    }

    #[test]
    fn mount_pair_converter() {
        assert_eq!(parse_mount_pair("/a").unwrap(), ("/a".into(), "/a".into()));
        assert_eq!(
            parse_mount_pair("/a:/b").unwrap(),
            ("/a".into(), "/b".into())
        );
        assert_eq!(
            parse_mount_pair("/a\\:x:/b").unwrap(),
            ("/a:x".into(), "/b".into())
        );
        assert!(parse_mount_pair("/a::/b")
            .unwrap_err()
            .contains("contains one or more empty paths"));
        assert!(parse_mount_pair("/a:/b:/c")
            .unwrap_err()
            .starts_with("Input must be a single path"));
        assert!(parse_mount_pair("").unwrap_err().contains("empty paths"));
    }

    #[test]
    fn darwin_config_matches_bazel_format() {
        let w: BTreeSet<String> = ["/w/a".to_string(), "/w/b\"q".to_string()].into();
        let b: BTreeSet<String> = ["/blocked".to_string()].into();
        let cfg = darwin_sandbox_config(&w, &b, false, Some("/s/stats.out"));
        let want = "(version 1)\n(debug deny)\n(allow default)\n(allow process-exec (with no-sandbox) (literal \"/bin/ps\"))\n\
(deny network*)\n(allow network-inbound (local ip \"localhost:*\"))\n\
(allow network* (remote ip \"localhost:*\"))\n(allow network* (remote unix-socket))\n\
(deny file-write*)\n(allow file-write*\n    (subpath \"/w/a\")\n    (subpath \"/w/b\\\"q\")\n    (literal \"/s/stats.out\")\n)\n\
(deny file-read*\n    (subpath \"/blocked\")\n)\n";
        assert_eq!(cfg, want);
        let open = darwin_sandbox_config(&w, &BTreeSet::new(), true, None);
        assert!(!open.contains("deny network"));
        assert!(!open.contains("deny file-read"));
        assert!(!open.contains("literal \"/s"));
    }

    #[test]
    fn local_env_tmpdir_rewrite() {
        let mut env = BTreeMap::new();
        env.insert("TMPDIR".to_string(), "/spawn".to_string());
        env.insert("A".to_string(), "b".to_string());
        let r = rewrite_local_env(&env, &BTreeMap::new());
        assert_eq!(r["TMPDIR"], "/tmp");
        assert_eq!(r["A"], "b");
        let mut c = BTreeMap::new();
        let abs = if cfg!(windows) {
            r"C:\var\x\T\"
        } else {
            "/var/x/T/"
        };
        c.insert("TMPDIR".to_string(), abs.to_string());
        assert_eq!(rewrite_local_env(&env, &c)["TMPDIR"], abs);
    }

    #[test]
    fn sandbox_base_computation() {
        assert_eq!(
            compute_sandbox_base("/ob", ""),
            PathBuf::from("/ob/sandbox")
        );
        let p = compute_sandbox_base("/ob", "/dev/shm");
        assert_eq!(p.parent(), Some(Path::new("/dev/shm")));
        let name = p.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("bazel-sandbox."), "{name}");
        assert_eq!(name.len(), "bazel-sandbox.".len() + 32);
    }

    #[cfg(unix)]
    #[test]
    fn sandboxed_run_only_sees_declared_inputs_and_moves_outputs() {
        let root = tmp("run");
        let er = root.join("execroot/ws");
        std::fs::create_dir_all(er.join("pkg")).unwrap();
        std::fs::write(er.join("pkg/in.txt"), "data").unwrap();
        std::fs::write(er.join("pkg/undeclared.txt"), "secret").unwrap();
        let base = root.join("sandbox");
        let r = SandboxRunner::new(
            SandboxKind::ProcessWrapper,
            &er,
            &base,
            SandboxOptions::default(),
            BTreeMap::new(),
            None,
        );
        let s = spawn(
            "cat pkg/in.txt > out/o.txt; test ! -e pkg/undeclared.txt && echo hidden >> out/o.txt; pwd >> out/o.txt",
            &["pkg/in.txt"],
            &["out/o.txt"],
        );
        let res = r.exec(&s).unwrap();
        assert_eq!(
            res.status,
            SpawnStatus::Success,
            "{:?}",
            String::from_utf8_lossy(&res.stderr)
        );
        assert_eq!(res.runner_name, "processwrapper-sandbox");
        let out = std::fs::read_to_string(er.join("out/o.txt")).unwrap();
        assert!(out.starts_with("datahidden\n"), "{out}");
        assert!(
            out.trim_end().ends_with("/ws"),
            "execroot must end with the workspace name: {out}"
        );
        // The sandbox dir is deleted after the action.
        assert!(!base.join("processwrapper-sandbox/1").exists());
        // The input symlink was not followed to modify the original.
        assert_eq!(
            std::fs::read_to_string(er.join("pkg/in.txt")).unwrap(),
            "data"
        );
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_debug_keeps_sandbox_dir_and_cleanup_removes_it() {
        let root = tmp("dbg");
        let er = root.join("execroot/ws");
        std::fs::create_dir_all(&er).unwrap();
        let base = root.join("sandbox");
        let opts = SandboxOptions {
            debug: true,
            ..Default::default()
        };
        let r = SandboxRunner::new(
            SandboxKind::ProcessWrapper,
            &er,
            &base,
            opts,
            BTreeMap::new(),
            None,
        );
        let res = r.exec(&spawn("echo hi > o", &[], &["o"])).unwrap();
        assert!(res.is_ok());
        // The sandbox directory is named by the process-wide spawn execution id (not necessarily 1
        // when other tests ran first).
        let cmd = res.executed_command.unwrap();
        let cwd = cmd.cwd.to_string();
        assert!(
            cwd.contains("processwrapper-sandbox/") && cwd.ends_with("/execroot/ws"),
            "{cwd}"
        );
        assert!(std::path::Path::new(&cwd).exists());
        assert!(cwd.starts_with(base.to_str().unwrap()), "{cwd}");
        r.cleanup_sandbox_base().unwrap();
        assert!(!base.join("processwrapper-sandbox").exists());
    }

    #[test]
    fn writable_dirs_include_tmpdir_test_tmpdir_and_flag_paths() {
        let root = tmp("wd");
        let er = root.join("execroot/ws");
        std::fs::create_dir_all(&er).unwrap();
        let tmpd = root.join("t");
        std::fs::create_dir_all(&tmpd).unwrap();
        let extra = root.join("extra");
        std::fs::create_dir_all(&extra).unwrap();
        let opts = SandboxOptions {
            writable_paths: vec![extra.to_string_lossy().into()],
            ..Default::default()
        };
        let mut ce = BTreeMap::new();
        ce.insert("TMPDIR".to_string(), tmpd.to_string_lossy().into_owned());
        let r = SandboxRunner::new(
            SandboxKind::ProcessWrapper,
            &er,
            &root.join("sb"),
            opts,
            ce.clone(),
            None,
        );
        let env = rewrite_local_env(&BTreeMap::new(), &ce);
        let w = r.writable_dirs(&er, &env).unwrap();
        assert!(w.contains(&er.to_string_lossy().to_string()));
        assert!(w.contains(&tmpd.to_string_lossy().to_string()));
        assert!(w.contains(&extra.to_string_lossy().to_string()));
        // A TEST_TMPDIR that does not exist outside the exec root is an error.
        let mut env2 = env.clone();
        env2.insert("TEST_TMPDIR".into(), "/definitely/not/here".into());
        let e = r.writable_dirs(&er, &env2).unwrap_err();
        assert!(e
            .to_string()
            .contains("Cannot resolve symlinks in TEST_TMPDIR because it doesn't exist"));
    }

    #[cfg(unix)]
    #[test]
    fn missing_input_and_missing_output_are_reported() {
        let root = tmp("mi");
        let er = root.join("execroot/ws");
        std::fs::create_dir_all(&er).unwrap();
        let r = SandboxRunner::new(
            SandboxKind::ProcessWrapper,
            &er,
            &root.join("sb"),
            SandboxOptions::default(),
            BTreeMap::new(),
            None,
        );
        // Missing regular input becomes a dangling symlink in Bazel, so reading it fails in-process.
        let res = r.exec(&spawn("cat nope", &["nope"], &[])).unwrap();
        assert_eq!(res.status, SpawnStatus::NonZeroExit);
        let res = r.exec(&spawn("true", &[], &["o"])).unwrap();
        assert_eq!(res.status, SpawnStatus::MissingOutputs);
        assert_eq!(res.missing, vec!["o"]);
    }

    #[test]
    fn relative_client_tmpdir_is_ignored_and_falls_back_to_tmp() {
        let mut c = BTreeMap::new();
        c.insert("TMPDIR".to_string(), "relative/tmp".to_string());
        let r = rewrite_local_env(&BTreeMap::new(), &c);
        assert_eq!(r["TMPDIR"], "/tmp");
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_helpers_input_nesting_virtual_empty_symlink_and_move_outputs() {
        let root = tmp("helpers");
        let er = root.join("execroot/ws");
        let sbx = root.join("sbx/execroot/ws");
        std::fs::create_dir_all(er.join("dir")).unwrap();
        std::fs::create_dir_all(&sbx).unwrap();
        std::fs::write(er.join("dir/sub.txt"), "s").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/some/target", er.join("sym")).unwrap();

        let mut map = BTreeMap::new();
        // Hierarchical order: dir precedes dir/sub.txt.
        map.insert(
            HierarchicalPath::from("dir"),
            ExpandedInput::File {
                exec_path: PathFragment::create("dir"),
                is_symlink: false,
            },
        );
        map.insert(
            HierarchicalPath::from("dir/sub.txt"),
            ExpandedInput::File {
                exec_path: PathFragment::create("dir/sub.txt"),
                is_symlink: false,
            },
        );
        map.insert(
            HierarchicalPath::from("sym"),
            ExpandedInput::File {
                exec_path: PathFragment::create("sym"),
                is_symlink: true,
            },
        );
        // Nested under a symlink IS kept (unlike non-symlink).
        map.insert(
            HierarchicalPath::from("sym/child"),
            ExpandedInput::File {
                exec_path: PathFragment::create("sym/child"),
                is_symlink: false,
            },
        );
        map.insert(HierarchicalPath::from("empty.txt"), ExpandedInput::Empty);
        map.insert(
            HierarchicalPath::from("emptydir"),
            ExpandedInput::EmptyDirectory(PathFragment::create("emptydir")),
        );
        map.insert(
            HierarchicalPath::from("_bin/tool"),
            ExpandedInput::Virtual(crate::VirtualInput::Bytes {
                exec_path: PathFragment::create("_bin/tool"),
                content: b"#!/bin/sh\n".to_vec(),
                executable: true,
            }),
        );

        let (si, empty_dirs) = SandboxHelpers::process_input_files(&map, &er, None).unwrap();
        assert!(si.files.contains_key(&PathFragment::create("dir")));
        assert!(
            !si.files.contains_key(&PathFragment::create("dir/sub.txt")),
            "nested under non-symlink dropped"
        );
        assert!(
            si.files.contains_key(&PathFragment::create("sym/child")),
            "nested under symlink kept"
        );
        assert_eq!(
            si.files.get(&PathFragment::create("empty.txt")),
            Some(&None)
        );
        assert_eq!(
            si.symlinks.get(&PathFragment::create("sym")),
            Some(&PathFragment::create("/some/target"))
        );
        assert!(empty_dirs.contains(&PathFragment::create("emptydir")));
        assert!(er.join("_bin/tool").exists());

        // Strict create_directories rejects absolute and multi-uplevel paths.
        let abs_set = BTreeSet::from([PathFragment::create("/etc")]);
        assert!(SandboxHelpers::create_directories(&abs_set, &sbx, true, None).is_err());
        let esc_set = BTreeSet::from([PathFragment::create("../../escape")]);
        assert!(SandboxHelpers::create_directories(&esc_set, &sbx, true, None).is_err());
        let ok_sibling = BTreeSet::from([PathFragment::create("../sibling/dir")]);
        assert!(SandboxHelpers::create_directories(&ok_sibling, &sbx, true, None).is_ok());
        assert!(root.join("sbx/execroot/sibling/dir").is_dir());

        // Unreadable output file copy retries after chmod 0644 when rename fails.
        let src_dir = root.join("src_out");
        let dst_dir = root.join("dst_out");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();
        std::fs::write(src_dir.join("ro.txt"), "hello").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                src_dir.join("ro.txt"),
                std::fs::Permissions::from_mode(0o000),
            )
            .unwrap();
            // Make src_dir read-only (0555) so rename() fails with EACCES and triggers copy fallback.
            std::fs::set_permissions(&src_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        }
        let mut outs = SandboxOutputs::default();
        outs.files.insert(
            PathFragment::create("out/ro.txt"),
            PathFragment::create("ro.txt"),
        );
        let res = SandboxHelpers::move_outputs(&outs, &src_dir, &dst_dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&src_dir, std::fs::Permissions::from_mode(0o755));
        }
        res.unwrap();
        assert_eq!(
            std::fs::read_to_string(dst_dir.join("out/ro.txt")).unwrap(),
            "hello"
        );
    }

    #[cfg(unix)]
    #[test]
    fn delete_tree_chmod_retries_mode_000_dir() {
        use std::os::unix::fs::PermissionsExt;
        let root = tmp("dt000");
        let locked = root.join("locked");
        std::fs::create_dir_all(locked.join("inner")).unwrap();
        std::fs::write(locked.join("inner/f"), "x").unwrap();
        std::fs::set_permissions(locked.join("inner"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        delete_tree(&locked).unwrap();
        assert!(!locked.exists());
    }

    #[test]
    fn darwin_sandbox_enforces_write_policy_when_supported() {
        if !darwin_sandbox_supported() {
            eprintln!("sandbox-exec unavailable here; skipping");
            return;
        }
        let root = tmp("dar");
        let er = root.join("execroot/ws");
        std::fs::create_dir_all(&er).unwrap();
        let forbidden = std::env::current_dir()
            .unwrap()
            .join("zel-darwin-forbidden-file");
        let r = SandboxRunner::new(
            SandboxKind::Darwin,
            &er,
            &root.join("sb"),
            SandboxOptions::default(),
            BTreeMap::new(),
            None,
        );
        let s = spawn(
            &format!(
                "echo ok > o; echo bad > {} 2>/dev/null || echo blocked > o2",
                forbidden.display()
            ),
            &[],
            &["o", "o2"],
        );
        let res = r.exec(&s).unwrap();
        assert_eq!(
            res.status,
            SpawnStatus::Success,
            "{}",
            String::from_utf8_lossy(&res.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(er.join("o2")).unwrap().trim(),
            "blocked"
        );
        assert!(!forbidden.exists());
    }

    #[cfg(unix)]
    #[test]
    fn darwin_sandbox_resolves_symlinked_sandbox_base_paths() {
        let root = tmp("dar-sym");
        let real_root = root.join("real");
        let link_root = root.join("link");
        std::fs::create_dir_all(real_root.join("execroot/ws")).unwrap();
        std::os::unix::fs::symlink(&real_root, &link_root).unwrap();

        let link_er = link_root.join("execroot/ws");
        let link_sb = link_root.join("sb");
        let opts = SandboxOptions {
            block_paths: vec![link_root.join("blocked_missing").to_string_lossy().into()],
            ..Default::default()
        };
        let r = SandboxRunner::new(
            SandboxKind::Darwin,
            &link_er,
            &link_sb,
            opts,
            BTreeMap::new(),
            None,
        );
        let s = spawn("echo ok > bazel-out/p/ok.txt", &[], &["bazel-out/p/ok.txt"]);
        let (layout, _env) = r.prepare(&s, 99999).unwrap();
        let canonical_exec_root = real_root
            .join("sb/darwin-sandbox/99999/execroot/ws")
            .to_string_lossy()
            .into_owned();
        assert!(
            layout.writable_dirs.contains(&canonical_exec_root),
            "canonical sandbox execroot {canonical_exec_root} missing from {:?}",
            layout.writable_dirs
        );
        let stats_resolved = resolve_best_effort(&layout.sandbox_path.join("stats.out"));
        assert_eq!(
            stats_resolved,
            real_root
                .join("sb/darwin-sandbox/99999/stats.out")
                .to_string_lossy()
        );
        assert!(r.inaccessible_paths().contains(
            &real_root
                .join("blocked_missing")
                .to_string_lossy()
                .into_owned()
        ));
        let _ = delete_tree(&layout.sandbox_path);

        if darwin_sandbox_supported() {
            let res = r.exec(&s).unwrap();
            assert_eq!(
                res.status,
                SpawnStatus::Success,
                "{}",
                String::from_utf8_lossy(&res.stderr)
            );
            assert_eq!(
                std::fs::read_to_string(real_root.join("execroot/ws/bazel-out/p/ok.txt"))
                    .unwrap()
                    .trim(),
                "ok"
            );
        }
    }
}
