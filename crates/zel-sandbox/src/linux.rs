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

//! Port of `LinuxSandboxCommandLineBuilder` and the `LinuxSandboxedSpawnRunner` support probe.
//!
//! The `linux-sandbox` helper itself (`src/main/tools/linux-sandbox*.cc`: user/mount/net/pid
//! namespaces, pivot_root, tmpfs) is a C++ program that is not part of this repository's Rust
//! build; this module builds its exact command line so that, where the helper is installed on a
//! Linux host, it can be invoked. On other platforms `get_unsupported_reason` reports why the
//! `linux-sandbox` strategy is unavailable and it is never registered.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetworkNamespace {
    #[default]
    NoNetns,
    Netns,
    NetnsWithLoopback,
}

#[derive(Debug, Clone, Default)]
pub struct LinuxSandboxCommandLineBuilder {
    pub linux_sandbox_path: String,
    pub hermetic_sandbox_path: Option<String>,
    pub working_directory: Option<String>,
    pub timeout: Option<Duration>,
    pub kill_delay: Option<Duration>,
    pub persistent_process: bool,
    pub stdout_path: Option<String>,
    pub stderr_path: Option<String>,
    pub writable_files_and_directories: Vec<String>,
    pub tmpfs_directories: Vec<String>,
    /// target -> source
    pub bind_mounts: Vec<(String, String)>,
    pub statistics_path: Option<String>,
    pub use_fake_hostname: bool,
    pub create_network_namespace: NetworkNamespace,
    pub use_fake_root: bool,
    pub use_fake_username: bool,
    pub enable_pseudoterminal: bool,
    pub sandbox_debug_path: Option<String>,
    pub sigint_sends_sigterm: bool,
    pub cgroups_dirs: Vec<String>,
}

impl LinuxSandboxCommandLineBuilder {
    pub fn new(path: &str) -> Self {
        Self {
            linux_sandbox_path: path.to_string(),
            ..Default::default()
        }
    }

    /// `addExecutionInfo`: `supports-graceful-termination` makes SIGINT send SIGTERM.
    pub fn add_execution_info(&mut self, info: &BTreeMap<String, String>) -> &mut Self {
        if info.contains_key("supports-graceful-termination") {
            self.sigint_sends_sigterm = true;
        }
        self
    }

    /// `buildForCommand`.
    pub fn build_for_command(&self, command: &[String]) -> Result<Vec<String>, String> {
        if self.use_fake_username && self.use_fake_root {
            return Err("useFakeUsername and useFakeRoot are exclusive".into());
        }
        let mut c = vec![self.linux_sandbox_path.clone()];
        let add2 = |c: &mut Vec<String>, f: &str, v: &str| {
            c.push(f.to_string());
            c.push(v.to_string());
        };
        if let Some(w) = &self.working_directory {
            add2(&mut c, "-W", w);
        }
        if let Some(t) = self.timeout {
            add2(&mut c, "-T", &t.as_secs().to_string());
        }
        if let Some(k) = self.kill_delay {
            add2(&mut c, "-t", &k.as_secs().to_string());
        }
        if let Some(p) = &self.stdout_path {
            add2(&mut c, "-l", p);
        }
        if let Some(p) = &self.stderr_path {
            add2(&mut c, "-L", p);
        }
        for p in &self.writable_files_and_directories {
            add2(&mut c, "-w", p);
        }
        for p in &self.tmpfs_directories {
            add2(&mut c, "-e", p);
        }
        for (target, source) in &self.bind_mounts {
            add2(&mut c, "-M", source);
            if source != target {
                add2(&mut c, "-m", target);
            }
        }
        if let Some(p) = &self.statistics_path {
            add2(&mut c, "-S", p);
        }
        if let Some(p) = &self.hermetic_sandbox_path {
            add2(&mut c, "-h", p);
        }
        if self.use_fake_hostname {
            c.push("-H".into());
        }
        match self.create_network_namespace {
            NetworkNamespace::NetnsWithLoopback => c.push("-N".into()),
            NetworkNamespace::Netns => c.push("-n".into()),
            NetworkNamespace::NoNetns => {}
        }
        if self.use_fake_root {
            c.push("-R".into());
        }
        if self.use_fake_username {
            c.push("-U".into());
        }
        if self.enable_pseudoterminal {
            c.push("-P".into());
        }
        if let Some(p) = &self.sandbox_debug_path {
            add2(&mut c, "-D", p);
        }
        if self.sigint_sends_sigterm {
            c.push("-i".into());
        }
        if self.persistent_process {
            c.push("-p".into());
        }
        for d in &self.cgroups_dirs {
            add2(&mut c, "-C", d);
        }
        c.push("--".into());
        c.extend(command.iter().cloned());
        Ok(c)
    }
}

static PROBE_CACHE: OnceLock<Mutex<HashMap<PathBuf, Result<(), String>>>> = OnceLock::new();

/// Live-probes `linux-sandbox` (`LinuxSandboxedSpawnRunner.isSupported`), caching the result per
/// binary path. Executes `<linux_sandbox> -T <kill_grace_secs> -- /bin/true` with an empty
/// environment, cwd = `exec_root` (or `/tmp`), and stdout/stderr discarded.
pub fn probe_linux_sandbox(
    linux_sandbox_binary: &str,
    exec_root: Option<&Path>,
    kill_grace_secs: u64,
) -> Result<(), String> {
    if !cfg!(target_os = "linux") {
        return Err("linux-sandbox is only supported on Linux".to_string());
    }
    let path = PathBuf::from(linux_sandbox_binary);
    if !path.exists() {
        return Err(format!(
            "linux-sandbox binary {linux_sandbox_binary} does not exist"
        ));
    }
    let cache = PROBE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let guard = cache.lock().unwrap();
        if let Some(cached) = guard.get(&path) {
            return cached.clone();
        }
    }
    let res = run_probe(linux_sandbox_binary, exec_root, kill_grace_secs);
    let mut guard = cache.lock().unwrap();
    guard.entry(path).or_insert_with(|| res.clone());
    res
}

fn run_probe(
    linux_sandbox_binary: &str,
    exec_root: Option<&Path>,
    kill_grace_secs: u64,
) -> Result<(), String> {
    let mut b = LinuxSandboxCommandLineBuilder::new(linux_sandbox_binary);
    b.timeout = Some(Duration::from_secs(kill_grace_secs));
    let argv = b.build_for_command(&["/bin/true".to_string()])?;
    let cwd = exec_root
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| Path::new("/tmp"));
    let status = {
        let mut attempt = 0u32;
        loop {
            let res = std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .current_dir(cwd)
                .env_clear()
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            // In multi-threaded test/daemon processes on Linux, another thread's fork()
            // before execve() can transiently hold a writable fd open, causing ETXTBSY (26).
            if let Err(ref e) = res {
                if e.raw_os_error() == Some(26) && attempt < 10 {
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(5 * attempt as u64));
                    continue;
                }
            }
            break res;
        }
    };
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("linux-sandbox probe failed with status {s}")),
        Err(e) => Err(format!("linux-sandbox probe failed: {e}")),
    }
}

/// `LinuxSandboxUtil.validateBindMounts`: validates each `(target, source)` bind mount entry.
pub fn validate_bind_mounts(bind_mounts: &[(String, String)]) -> Result<(), String> {
    for (target, source) in bind_mounts {
        let src_md = std::fs::metadata(source)
            .map_err(|_| format!("Mount source '{source}' does not exist."))?;
        let dst_md = std::fs::metadata(target)
            .map_err(|_| format!("Mount target '{target}' does not exist."))?;
        let src_is_dir = src_md.is_dir();
        let dst_is_dir = dst_md.is_dir();
        if src_is_dir != dst_is_dir {
            if src_is_dir {
                return Err(format!(
                    "Cannot mount directory '{source}' onto file '{target}'."
                ));
            } else {
                return Err(format!(
                    "Cannot mount file '{source}' onto directory '{target}'."
                ));
            }
        }
    }
    Ok(())
}

/// `LinuxSandboxUtil.getInaccessibleHelperFile`: creates `<sandbox_base>/inaccessibleHelperFile`
/// and clears executable, writable, and readable permission bits (`0o000`).
pub fn get_inaccessible_helper_file(sandbox_base: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(sandbox_base)?;
    let file = sandbox_base.join("inaccessibleHelperFile");
    if let Ok(md) = std::fs::symlink_metadata(&file) {
        if md.is_dir() && !md.file_type().is_symlink() {
            crate::delete_tree(&file)?;
        }
    }
    if !file.exists() {
        let _ = std::fs::File::create(&file)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000))?;
    }
    #[cfg(not(unix))]
    {
        let mut perms = std::fs::metadata(&file)?.permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&file, perms)?;
    }
    Ok(file)
}

/// `LinuxSandboxUtil.getInaccessibleHelperDir`: creates `<sandbox_base>/inaccessibleHelperDir`
/// and clears executable, writable, and readable permission bits (`0o000`).
pub fn get_inaccessible_helper_dir(sandbox_base: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(sandbox_base)?;
    let dir = sandbox_base.join("inaccessibleHelperDir");
    if let Ok(md) = std::fs::symlink_metadata(&dir) {
        if !md.is_dir() || md.file_type().is_symlink() {
            crate::delete_tree(&dir)?;
        }
    }
    if std::fs::symlink_metadata(&dir).is_err() {
        std::fs::create_dir(&dir)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000))?;
    }
    Ok(dir)
}

/// `LinuxSandboxedStrategy.create`: creates the inaccessible helper file and directory when
/// `block_paths` is non-empty, returning `(helper_file, helper_dir, optional_warning)`.
pub fn create_inaccessible_helpers(
    sandbox_base: &Path,
    block_paths: &[String],
) -> (Option<PathBuf>, Option<PathBuf>, Option<String>) {
    if block_paths.is_empty() {
        return (None, None, None);
    }
    let warn_msg = || format!("Could not block access to: {}", block_paths.join(","));
    let file = match get_inaccessible_helper_file(sandbox_base) {
        Ok(f) => f,
        Err(_) => return (None, None, Some(warn_msg())),
    };
    match get_inaccessible_helper_dir(sandbox_base) {
        Ok(d) => (Some(file), Some(d), None),
        Err(_) => (Some(file), None, Some(warn_msg())),
    }
}

/// Why `linux-sandbox` is unavailable here (`LinuxSandboxedSpawnRunner.getUnsupportedReason`);
/// `None` means supported.
pub fn get_unsupported_reason(linux_sandbox_binary: Option<&str>) -> Option<String> {
    get_unsupported_reason_with(linux_sandbox_binary, None, 15)
}

/// Live-probed availability check with configurable exec_root and `local_sigkill_grace_seconds`.
pub fn get_unsupported_reason_with(
    linux_sandbox_binary: Option<&str>,
    exec_root: Option<&Path>,
    kill_grace_secs: u64,
) -> Option<String> {
    if !cfg!(target_os = "linux") {
        return Some("linux-sandbox is only supported on Linux".to_string());
    }
    match linux_sandbox_binary {
        None => Some("linux-sandbox binary is not installed".to_string()),
        Some(p) => probe_linux_sandbox(p, exec_root, kill_grace_secs).err(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_full_command_line_in_bazel_order() {
        let mut b = LinuxSandboxCommandLineBuilder::new("/ls");
        b.working_directory = Some("/sbx/execroot/ws".into());
        b.timeout = Some(Duration::from_secs(60));
        b.kill_delay = Some(Duration::from_secs(15));
        b.stdout_path = Some("/o".into());
        b.stderr_path = Some("/e".into());
        b.writable_files_and_directories = vec!["/w1".into(), "/w2".into()];
        b.tmpfs_directories = vec!["/tmp".into()];
        b.bind_mounts.push(("/dst".into(), "/src".into()));
        b.bind_mounts.push(("/same".into(), "/same".into()));
        b.statistics_path = Some("/stats".into());
        b.hermetic_sandbox_path = Some("/sbx".into());
        b.use_fake_hostname = true;
        b.create_network_namespace = NetworkNamespace::NetnsWithLoopback;
        b.use_fake_username = true;
        b.enable_pseudoterminal = true;
        b.sandbox_debug_path = Some("/dbg".into());
        b.add_execution_info(
            &[("supports-graceful-termination".to_string(), "1".to_string())].into(),
        );
        b.persistent_process = true;
        b.cgroups_dirs = vec!["/sys/fs/cgroup/x".into()];
        let got = b.build_for_command(&["cc".into(), "-c".into()]).unwrap();
        let want: Vec<&str> = vec![
            "/ls",
            "-W",
            "/sbx/execroot/ws",
            "-T",
            "60",
            "-t",
            "15",
            "-l",
            "/o",
            "-L",
            "/e",
            "-w",
            "/w1",
            "-w",
            "/w2",
            "-e",
            "/tmp",
            "-M",
            "/src",
            "-m",
            "/dst",
            "-M",
            "/same",
            "-S",
            "/stats",
            "-h",
            "/sbx",
            "-H",
            "-N",
            "-U",
            "-P",
            "-D",
            "/dbg",
            "-i",
            "-p",
            "-C",
            "/sys/fs/cgroup/x",
            "--",
            "cc",
            "-c",
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn fake_root_and_username_are_exclusive() {
        let mut b = LinuxSandboxCommandLineBuilder::new("/ls");
        b.use_fake_root = true;
        b.use_fake_username = true;
        assert_eq!(
            b.build_for_command(&[]).unwrap_err(),
            "useFakeUsername and useFakeRoot are exclusive"
        );
    }

    #[test]
    fn unsupported_off_linux() {
        if !cfg!(target_os = "linux") {
            assert!(get_unsupported_reason(Some("/x"))
                .unwrap()
                .contains("only supported on Linux"));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn probe_runs_binary_with_timeout_empty_env_and_caches() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("zel-ls-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let ok_bin = d.join("ok-sandbox");
        let log = d.join("probe.log");
        std::fs::write(
            &ok_bin,
            format!(
                "#!/bin/sh\necho \"$*\" > {}\nenv | wc -l >> {}\nexit 0\n",
                log.display(),
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&ok_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            get_unsupported_reason_with(Some(ok_bin.to_str().unwrap()), Some(&d), 12),
            None
        );
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("-T 12 -- /bin/true"), "{text}");

        // Failing binary is reported unsupported and cached per path.
        let bad_bin = d.join("bad-sandbox");
        std::fs::write(&bad_bin, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&bad_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            get_unsupported_reason_with(Some(bad_bin.to_str().unwrap()), Some(&d), 15).is_some()
        );
        // Overwriting the binary after probe still returns the cached failure.
        std::fs::write(&bad_bin, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&bad_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            get_unsupported_reason_with(Some(bad_bin.to_str().unwrap()), Some(&d), 15).is_some()
        );
    }
}
