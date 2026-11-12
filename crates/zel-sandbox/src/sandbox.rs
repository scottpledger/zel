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

use crate::process_wrapper::{self, ProcessWrapperOptions, Termination};
use crate::{Spawn, SpawnResult, SpawnStatus, DEFAULT_KILL_DELAY_MS};
use zel_vfs::PathFragment;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

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

/// `LinuxSandboxedSpawnRunner.isSupported`: supported on Linux hosts.
pub fn linux_sandbox_supported() -> bool {
    cfg!(target_os = "linux")
}

fn find_linux_sandbox_binary(exec_root: &Path) -> Option<String> {
    if let Some(output_base) = exec_root.parent().and_then(|p| p.parent()) {
        let p = output_base.join("install/linux-sandbox");
        if p.exists() {
            return Some(p.to_string_lossy().into_owned());
        }
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
                || matches!(b, b'_' | b'@' | b'%' | b'+' | b'=' | b':' | b',' | b'.' | b'/' | b'-')
        })
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn format_interactive_debug_command(argv: &[String], env: &BTreeMap<String, String>, cwd: &str) -> String {
    let mut s = format!("(cd {} && \\\n  exec env - \\\n", shell_escape_arg(cwd));
    for (k, v) in env {
        s.push_str(&format!("    {}={} \\\n", shell_escape_arg(k), shell_escape_arg(v)));
    }
    let cmd = argv.iter().map(|a| shell_escape_arg(a)).collect::<Vec<_>>().join(" ");
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
        o.push_str("(allow network-bind (local ip \"*:*\"))\n");
        o.push_str("(allow network-inbound (local ip \"localhost:*\"))\n");
        o.push_str("(allow network* (remote ip \"localhost:*\"))\n");
        o.push_str("(allow network* (local unix-socket))\n");
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
    std::fs::canonicalize(p).ok().map(|x| x.to_string_lossy().into_owned())
}

/// `PosixLocalEnvProvider.rewriteLocalEnv`: TMPDIR from the client environment or `/tmp`.
pub fn rewrite_local_env(
    env: &BTreeMap<String, String>,
    client_env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut r: BTreeMap<String, String> = env.iter().filter(|(k, _)| *k != "TMPDIR").map(|(k, v)| (k.clone(), v.clone())).collect();
    let p = client_env.get("TMPDIR").filter(|s| !s.is_empty()).cloned().unwrap_or_else(|| "/tmp".to_string());
    r.insert("TMPDIR".to_string(), p);
    r
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

/// Result of preparing a sandbox, kept for diagnostics (`--sandbox_debug`).
#[derive(Debug, Clone)]
pub struct SandboxLayout {
    pub sandbox_path: PathBuf,
    pub sandbox_exec_root: PathBuf,
    pub writable_dirs: BTreeSet<String>,
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
        let always_writable = if kind == SandboxKind::Darwin {
            Self::always_writable_dirs(&client_env)
        } else {
            BTreeSet::new()
        };
        Self {
            kind,
            exec_root: exec_root.to_path_buf(),
            sandbox_base: sandbox_base.to_path_buf(),
            options,
            client_env,
            kill_delay: kill_delay.unwrap_or(Duration::from_millis(DEFAULT_KILL_DELAY_MS)),
            always_writable,
            next_id: AtomicU64::new(1),
        }
    }

    /// `DarwinSandboxedSpawnRunner.getAlwaysWritableDirs`.
    fn always_writable_dirs(client_env: &BTreeMap<String, String>) -> BTreeSet<String> {
        let mut w = BTreeSet::new();
        let mut add = |p: &str| {
            if let Some(r) = resolve(p) {
                w.insert(r);
            }
        };
        for p in ["/dev", "/tmp", "/private/tmp", "/private/var/tmp"] {
            add(p);
        }
        for k in ["DARWIN_USER_TEMP_DIR", "DARWIN_USER_CACHE_DIR"] {
            if let Some(v) = client_env.get(k) {
                add(v);
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            for sub in ["Library/Caches", "Library/Logs", "Library/Developer"] {
                add(&home.join(sub).to_string_lossy());
            }
        }
        w
    }

    pub fn name(&self) -> &'static str {
        self.kind.name()
    }

    /// `AbstractSandboxSpawnRunner.getWritableDirs`.
    fn writable_dirs(&self, sandbox_exec_root: &Path, env: &BTreeMap<String, String>) -> io::Result<BTreeSet<String>> {
        let mut w = BTreeSet::new();
        w.insert(sandbox_exec_root.to_string_lossy().into_owned());
        let add = |w: &mut BTreeSet<String>, var: &str, value: &str| -> io::Result<()> {
            let p = if Path::new(value).is_absolute() { PathBuf::from(value) } else { sandbox_exec_root.join(value) };
            if p.starts_with(sandbox_exec_root) {
                // Below the sandbox exec root: already writable, but must exist.
                w.insert(p.to_string_lossy().into_owned());
            } else if p.exists() {
                w.insert(resolve(&p.to_string_lossy()).unwrap_or_else(|| p.to_string_lossy().into_owned()));
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("Cannot resolve symlinks in {var} because it doesn't exist: \"{}\"", p.display()),
                ));
            }
            Ok(())
        };
        if let Some(t) = env.get("TEST_TMPDIR") {
            add(&mut w, "TEST_TMPDIR", t)?;
        }
        add(&mut w, "TMPDIR", env.get("TMPDIR").map(String::as_str).unwrap_or("/tmp"))?;
        for p in &self.options.writable_paths {
            w.insert(p.clone());
            if let Some(r) = resolve(p) {
                w.insert(r);
            }
        }
        Ok(w)
    }

    /// `SandboxOptions.getInaccessiblePaths` (resolved symlinks where possible).
    fn inaccessible_paths(&self) -> BTreeSet<String> {
        self.options.block_paths.iter().map(|p| resolve(p).unwrap_or_else(|| p.clone())).collect()
    }

    /// Prepares the sandbox directory (`prepareSpawn` + `createFileSystem`): execroot named like
    /// the real one, symlinked inputs, created output/writable directories.
    pub fn prepare(&self, spawn: &Spawn, id: u64) -> io::Result<(SandboxLayout, BTreeMap<String, String>)> {
        let sandbox_path = self.sandbox_base.join(self.name()).join(id.to_string());
        std::fs::create_dir_all(&sandbox_path)?;
        // The execroot of the sandboxed process must end with the workspace name.
        let ws_name = self.exec_root.file_name().map(|s| s.to_os_string()).unwrap_or_else(|| "_main".into());
        let sandbox_exec_root = sandbox_path.join("execroot").join(ws_name);
        std::fs::create_dir_all(&sandbox_exec_root)?;
        let env = rewrite_local_env(&spawn.env, &self.client_env);

        let mut writable = self.always_writable.clone();
        let extra = self.writable_dirs(&sandbox_exec_root, &env)?;
        writable.extend(extra.iter().cloned());

        // dirsToCreate: parents of inputs, parents of outputs, writable dirs under the exec root.
        let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
        let mut inputs: Vec<&PathFragment> = Vec::new();
        for i in &spawn.inputs {
            inputs.push(i);
            if let Some(p) = Path::new(i.path_string()).parent() {
                dirs.insert(sandbox_exec_root.join(p));
            }
        }
        for o in &spawn.outputs {
            if let Some(p) = Path::new(o.path_string()).parent() {
                dirs.insert(sandbox_exec_root.join(p));
            }
        }
        for d in &spawn.output_dirs {
            dirs.insert(sandbox_exec_root.join(d.path_string()));
        }
        if spawn.mnemonic == "TestRunner" {
            for o in &spawn.outputs {
                if o.path_string().ends_with("/test.outputs") || o.path_string().ends_with("/_coverage") {
                    dirs.insert(sandbox_exec_root.join(o.path_string()));
                }
            }
        }
        for w in &writable {
            if Path::new(w).starts_with(&sandbox_exec_root) {
                dirs.insert(PathBuf::from(w));
            }
        }
        for d in &dirs {
            std::fs::create_dir_all(d)?;
        }
        let mut sorted_inputs: Vec<&PathFragment> = inputs;
        sorted_inputs.sort_by(|a, b| a.path_string().cmp(b.path_string()));
        let mut last_file: Option<&str> = None;
        for i in sorted_inputs {
            let ps = i.path_string();
            if let Some(prev) = last_file {
                if ps.starts_with(prev) && ps.as_bytes().get(prev.len()) == Some(&b'/') {
                    continue;
                }
            }
            let src = self.exec_root.join(ps);
            let is_symlink = std::fs::symlink_metadata(&src).map(|m| m.file_type().is_symlink()).unwrap_or(false);
            if is_symlink {
                last_file = None;
            } else {
                last_file = Some(ps);
            }
            let dest = sandbox_exec_root.join(ps);
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p)?;
            }
            if std::fs::symlink_metadata(&dest).is_ok() {
                continue;
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(&src, &dest)?;
        }
        Ok((SandboxLayout { sandbox_path, sandbox_exec_root, writable_dirs: writable }, env))
    }

    /// `SandboxHelpers.moveOutputs`.
    fn move_outputs(&self, spawn: &Spawn, sandbox_exec_root: &Path) -> io::Result<()> {
        for out in &spawn.outputs {
            let source = sandbox_exec_root.join(out.path_string());
            let target = self.exec_root.join(out.path_string());
            if std::fs::symlink_metadata(&source).is_err() {
                continue;
            }
            if let Ok(md) = std::fs::symlink_metadata(&target) {
                if md.is_dir() {
                    let _ = std::fs::remove_dir_all(&target);
                } else {
                    let _ = std::fs::remove_file(&target);
                }
            }
            if let Some(p) = target.parent() {
                std::fs::create_dir_all(p)?;
            }
            if std::fs::rename(&source, &target).is_err() {
                copy_tree(&source, &target)?;
            }
        }
        Ok(())
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
            wall_time_ms: 0,
            runner_name: self.name().to_string(),
            missing: Vec::new(),
            usage: None,
            executed_command: None,
        };
        if spawn.argv.is_empty() {
            return Ok(mk(SpawnStatus::ExecutionFailed, 1, Vec::new(), b"empty command line\n".to_vec()));
        }
        let missing_inputs: Vec<String> = spawn
            .inputs
            .iter()
            .filter(|i| std::fs::symlink_metadata(self.exec_root.join(i.path_string())).is_err())
            .map(|i| i.path_string().to_string())
            .collect();
        if !missing_inputs.is_empty() {
            let mut r = mk(SpawnStatus::MissingInputs, 1, Vec::new(), Vec::new());
            r.missing = missing_inputs;
            return Ok(r);
        }

        let (layout, env) = match self.prepare(spawn, id) {
            Ok(x) => x,
            Err(e) => {
                return Ok(mk(
                    SpawnStatus::ExecutionFailed,
                    1,
                    Vec::new(),
                    format!("Could not copy inputs into sandbox: {e}\n").into_bytes(),
                ))
            }
        };
        let stats_path = layout.sandbox_path.join("stats.out");
        let mut argv = spawn.argv.clone();
        let mut debug_output: Option<String> = None;
        let mut linux_sandbox_used = false;
        if self.kind == SandboxKind::Darwin {
            let config = layout.sandbox_path.join("sandbox.sb");
            let allow_net = requires_network(spawn, self.options.default_allow_network);
            let text = darwin_sandbox_config(
                &layout.writable_dirs,
                &self.inaccessible_paths(),
                allow_net,
                Some(&stats_path.to_string_lossy()),
            );
            std::fs::write(&config, text)?;
            let mut full = vec![SANDBOX_EXEC.to_string(), "-f".to_string(), config.to_string_lossy().into_owned()];
            full.extend(argv);
            argv = full;
        } else if self.kind == SandboxKind::Linux {
            if let Some(ls_bin) = find_linux_sandbox_binary(&self.exec_root) {
                let mut writable = layout.writable_dirs.clone();
                if let Some(shm) = resolve("/dev/shm") {
                    writable.insert(shm);
                } else {
                    writable.insert("/dev/shm".to_string());
                }
                writable.insert("/tmp".to_string());
                let hermetic_tmp = layout.sandbox_path.join("_hermetic_tmp");
                let _ = std::fs::create_dir_all(&hermetic_tmp);
                let mut bind_mounts = Vec::new();
                // Mount any /tmp/<seg1> parent of sandbox_base / exec_root under _hermetic_tmp.
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
                                let _ = std::fs::create_dir_all(&dst);
                                bind_mounts.push((dst.to_string_lossy().into_owned(), src));
                            }
                        }
                    }
                }
                bind_mounts.push(("/tmp".to_string(), hermetic_tmp.to_string_lossy().into_owned()));
                let mut b = crate::linux::LinuxSandboxCommandLineBuilder::new(&ls_bin);
                b.add_execution_info(&spawn.execution_info);
                b.writable_files_and_directories = writable.into_iter().collect();
                for tp in &self.options.tmpfs_paths {
                    if let Ok(rel) = Path::new(tp).strip_prefix("/tmp") {
                        let _ = std::fs::create_dir_all(hermetic_tmp.join(rel));
                    }
                }
                b.tmpfs_directories = self.options.tmpfs_paths.clone();
                b.bind_mounts = bind_mounts;
                b.use_fake_hostname = self.options.fake_hostname;
                b.enable_pseudoterminal = self.options.explicit_pseudoterminal;
                if !requires_network(spawn, self.options.default_allow_network) {
                    b.create_network_namespace = crate::linux::NetworkNamespace::Netns;
                }
                b.kill_delay = Some(spawn.kill_delay.unwrap_or(self.kill_delay));
                if self.options.debug {
                    b.sandbox_debug_path = Some(layout.sandbox_path.join("debug.out").to_string_lossy().into_owned());
                }
                if let Some(t) = spawn.timeout_secs() {
                    b.timeout = Some(Duration::from_secs_f64(t));
                }
                if spawn.execution_info.contains_key("requires-fakeroot") {
                    b.use_fake_root = true;
                } else if self.options.fake_username {
                    b.use_fake_username = true;
                }
                if self.options.use_hermetic_linux_sandbox {
                    b.hermetic_sandbox_path = Some(layout.sandbox_path.to_string_lossy().into_owned());
                    b.working_directory = Some(layout.sandbox_exec_root.to_string_lossy().into_owned());
                }
                b.statistics_path = Some(stats_path.to_string_lossy().into_owned());
                if let Ok(built) = b.build_for_command(&argv) {
                    argv = built;
                    linux_sandbox_used = true;
                    if self.options.debug {
                        if let Ok(interactive) = b.build_for_command(&["/bin/sh".to_string(), "-i".to_string()]) {
                            debug_output = Some(interactive.join("\0"));
                        }
                    }
                }
            }
        }
        let opt = ProcessWrapperOptions {
            graceful_sigterm: spawn.execution_info.contains_key("supports-graceful-termination"),
            timeout_secs: if linux_sandbox_used { 0.0 } else { spawn.timeout_secs().unwrap_or(0.0) },
            kill_delay_secs: spawn.kill_delay.unwrap_or(self.kill_delay).as_secs_f64(),
            stats_path: if linux_sandbox_used { None } else { Some(stats_path.to_string_lossy().into_owned()) },
            args: argv.clone(),
            cancel: hooks.cancel.clone(),
            ..Default::default()
        };
        let cwd = layout.sandbox_exec_root.to_string_lossy().into_owned();
        let outcome = process_wrapper::run(&opt, Some(&env), Some(&cwd));
        if matches!(&outcome, Ok(o) if o.cancelled) {
            if !self.options.debug {
                let _ = std::fs::remove_dir_all(&layout.sandbox_path);
            }
            return Err(crate::cancelled_error());
        }
        let mut result = match outcome {
            Ok(o) => {
                let is_ls_timeout = linux_sandbox_used
                    && spawn.timeout_secs().map(|t| t > 0.0 && o.wall.as_secs_f64() + 0.1 >= t && matches!(o.termination, Termination::Exited(142 | 137 | 143) | Termination::Signaled(_))).unwrap_or(false);
                let timed_out = o.timed_out || is_ls_timeout;
                let (status, mut code) = match o.termination {
                    _ if timed_out => (SpawnStatus::Timeout, 142),
                    Termination::Exited(0) => (SpawnStatus::Success, 0),
                    t => (SpawnStatus::NonZeroExit, t.exit_code()),
                };
                let mut stderr = o.stderr;
                // Bazel runs the command through `process-wrapper` inside `sandbox-exec`, so an
                // unexecutable program is reported by the wrapper with exit status 1.
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
                if timed_out && !linux_sandbox_used {
                    stderr.extend_from_slice(
                        format!("Action timed out after {:.0}s\n", spawn.timeout_secs().unwrap_or(0.0)).as_bytes(),
                    );
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
        // `stopConcurrentSpawns`: the dynamic-execution branch that gets here first wins.
        if hooks.is_cancelled() || !hooks.lock(result.exit_code) {
            if !self.options.debug {
                let _ = std::fs::remove_dir_all(&layout.sandbox_path);
            }
            return Err(crate::cancelled_error());
        }
        // Outputs are copied back even when the command failed.
        if let Err(e) = self.move_outputs(spawn, &layout.sandbox_exec_root) {
            result.status = SpawnStatus::ExecutionFailed;
            result.exit_code = 1;
            result.stderr.extend_from_slice(format!("Could not copy outputs from sandbox: {e}\n").as_bytes());
        } else if result.status == SpawnStatus::Success {
            let missing: Vec<String> = spawn
                .mandatory_outputs()
                .filter(|o| std::fs::symlink_metadata(self.exec_root.join(o.path_string())).is_err())
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
                    combined.push_str(&format_interactive_debug_command(&interactive_args, &env, &cwd));
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
            let _ = std::fs::remove_dir_all(&layout.sandbox_path);
        }
        Ok(result)
    }

    /// `cleanupSandboxBase`: removes the per-runner directory (and the base when empty).
    pub fn cleanup_sandbox_base(&self) -> io::Result<()> {
        let root = self.sandbox_base.join(self.name());
        if root.exists() {
            for e in std::fs::read_dir(&root)? {
                let p = e?.path();
                let _ = std::fs::remove_dir_all(&p);
            }
            let _ = std::fs::remove_dir(&root);
        }
        let _ = std::fs::remove_dir(&self.sandbox_base);
        Ok(())
    }
}

/// `SandboxModule.computeSandboxBase`: `<output_base>/sandbox`, or
/// `<--sandbox_base>/<product>-sandbox.<md5(output_base)>`.
pub fn compute_sandbox_base(output_base: &str, sandbox_base_opt: &str) -> PathBuf {
    if sandbox_base_opt.is_empty() {
        PathBuf::from(output_base).join("sandbox")
    } else {
        let dir = format!("bazel-sandbox.{}", zel_util::md5::hex_digest(output_base.as_bytes()));
        PathBuf::from(sandbox_base_opt).join(dir)
    }
}

fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    let md = std::fs::symlink_metadata(src)?;
    if md.file_type().is_symlink() {
        let t = std::fs::read_link(src)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(t, dst)?;
    } else if md.is_dir() {
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            copy_tree(&e.path(), &dst.join(e.file_name()))?;
        }
    } else {
        std::fs::copy(src, dst)?;
    }
    Ok(())
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
        assert_eq!(parse_mount_pair("/a:/b").unwrap(), ("/a".into(), "/b".into()));
        assert_eq!(parse_mount_pair("/a\\:x:/b").unwrap(), ("/a:x".into(), "/b".into()));
        assert!(parse_mount_pair("/a::/b").unwrap_err().contains("contains one or more empty paths"));
        assert!(parse_mount_pair("/a:/b:/c").unwrap_err().starts_with("Input must be a single path"));
        assert!(parse_mount_pair("").unwrap_err().contains("empty paths"));
    }

    #[test]
    fn darwin_config_matches_bazel_format() {
        let w: BTreeSet<String> = ["/w/a".to_string(), "/w/b\"q".to_string()].into();
        let b: BTreeSet<String> = ["/blocked".to_string()].into();
        let cfg = darwin_sandbox_config(&w, &b, false, Some("/s/stats.out"));
        let want = "(version 1)\n(debug deny)\n(allow default)\n(allow process-exec (with no-sandbox) (literal \"/bin/ps\"))\n\
(deny network*)\n(allow network-bind (local ip \"*:*\"))\n(allow network-inbound (local ip \"localhost:*\"))\n\
(allow network* (remote ip \"localhost:*\"))\n(allow network* (local unix-socket))\n(allow network* (remote unix-socket))\n\
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
        c.insert("TMPDIR".to_string(), "/var/x/T/".to_string());
        assert_eq!(rewrite_local_env(&env, &c)["TMPDIR"], "/var/x/T/");
    }

    #[test]
    fn sandbox_base_computation() {
        assert_eq!(compute_sandbox_base("/ob", ""), PathBuf::from("/ob/sandbox"));
        let p = compute_sandbox_base("/ob", "/dev/shm");
        assert!(p.to_string_lossy().starts_with("/dev/shm/bazel-sandbox."));
        assert_eq!(p.file_name().unwrap().to_string_lossy().len(), "bazel-sandbox.".len() + 32);
    }

    #[test]
    fn sandboxed_run_only_sees_declared_inputs_and_moves_outputs() {
        let root = tmp("run");
        let er = root.join("execroot/ws");
        std::fs::create_dir_all(er.join("pkg")).unwrap();
        std::fs::write(er.join("pkg/in.txt"), "data").unwrap();
        std::fs::write(er.join("pkg/undeclared.txt"), "secret").unwrap();
        let base = root.join("sandbox");
        let r = SandboxRunner::new(SandboxKind::ProcessWrapper, &er, &base, SandboxOptions::default(), BTreeMap::new(), None);
        let s = spawn(
            "cat pkg/in.txt > out/o.txt; test ! -e pkg/undeclared.txt && echo hidden >> out/o.txt; pwd >> out/o.txt",
            &["pkg/in.txt"],
            &["out/o.txt"],
        );
        let res = r.exec(&s).unwrap();
        assert_eq!(res.status, SpawnStatus::Success, "{:?}", String::from_utf8_lossy(&res.stderr));
        assert_eq!(res.runner_name, "processwrapper-sandbox");
        let out = std::fs::read_to_string(er.join("out/o.txt")).unwrap();
        assert!(out.starts_with("datahidden\n"), "{out}");
        assert!(out.trim_end().ends_with("/ws"), "execroot must end with the workspace name: {out}");
        // The sandbox dir is deleted after the action.
        assert!(!base.join("processwrapper-sandbox/1").exists());
        // The input symlink was not followed to modify the original.
        assert_eq!(std::fs::read_to_string(er.join("pkg/in.txt")).unwrap(), "data");
    }

    #[test]
    fn sandbox_debug_keeps_sandbox_dir_and_cleanup_removes_it() {
        let root = tmp("dbg");
        let er = root.join("execroot/ws");
        std::fs::create_dir_all(&er).unwrap();
        let base = root.join("sandbox");
        let opts = SandboxOptions { debug: true, ..Default::default() };
        let r = SandboxRunner::new(SandboxKind::ProcessWrapper, &er, &base, opts, BTreeMap::new(), None);
        let res = r.exec(&spawn("echo hi > o", &[], &["o"])).unwrap();
        assert!(res.is_ok());
        // The sandbox directory is named by the process-wide spawn execution id (not necessarily 1
        // when other tests ran first).
        let cmd = res.executed_command.unwrap();
        let cwd = cmd.cwd.to_string();
        assert!(cwd.contains("processwrapper-sandbox/") && cwd.ends_with("/execroot/ws"), "{cwd}");
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
        let opts = SandboxOptions { writable_paths: vec![extra.to_string_lossy().into()], ..Default::default() };
        let mut ce = BTreeMap::new();
        ce.insert("TMPDIR".to_string(), tmpd.to_string_lossy().into_owned());
        let r = SandboxRunner::new(SandboxKind::ProcessWrapper, &er, &root.join("sb"), opts, ce.clone(), None);
        let env = rewrite_local_env(&BTreeMap::new(), &ce);
        let w = r.writable_dirs(&er, &env).unwrap();
        assert!(w.contains(&er.to_string_lossy().to_string()));
        assert!(w.contains(&tmpd.to_string_lossy().to_string()));
        assert!(w.contains(&extra.to_string_lossy().to_string()));
        // A TEST_TMPDIR that does not exist outside the exec root is an error.
        let mut env2 = env.clone();
        env2.insert("TEST_TMPDIR".into(), "/definitely/not/here".into());
        let e = r.writable_dirs(&er, &env2).unwrap_err();
        assert!(e.to_string().contains("Cannot resolve symlinks in TEST_TMPDIR because it doesn't exist"));
    }

    #[test]
    fn missing_input_and_missing_output_are_reported() {
        let root = tmp("mi");
        let er = root.join("execroot/ws");
        std::fs::create_dir_all(&er).unwrap();
        let r = SandboxRunner::new(SandboxKind::ProcessWrapper, &er, &root.join("sb"), SandboxOptions::default(), BTreeMap::new(), None);
        let res = r.exec(&spawn("true", &["nope"], &[])).unwrap();
        assert_eq!(res.status, SpawnStatus::MissingInputs);
        let res = r.exec(&spawn("true", &[], &["o"])).unwrap();
        assert_eq!(res.status, SpawnStatus::MissingOutputs);
        assert_eq!(res.missing, vec!["o"]);
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
        let forbidden = std::env::current_dir().unwrap().join("zel-darwin-forbidden-file");
        let r = SandboxRunner::new(SandboxKind::Darwin, &er, &root.join("sb"), SandboxOptions::default(), BTreeMap::new(), None);
        let s = spawn(&format!("echo ok > o; echo bad > {} 2>/dev/null || echo blocked > o2", forbidden.display()), &[], &["o", "o2"]);
        let res = r.exec(&s).unwrap();
        assert_eq!(res.status, SpawnStatus::Success, "{}", String::from_utf8_lossy(&res.stderr));
        assert_eq!(std::fs::read_to_string(er.join("o2")).unwrap().trim(), "blocked");
        assert!(!forbidden.exists());
    }
}
