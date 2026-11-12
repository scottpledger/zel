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

use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkNamespace {
    NoNetns,
    Netns,
    NetnsWithLoopback,
}

impl Default for NetworkNamespace {
    fn default() -> Self {
        NetworkNamespace::NoNetns
    }
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
        Self { linux_sandbox_path: path.to_string(), ..Default::default() }
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

/// Why `linux-sandbox` is unavailable here (`LinuxSandboxedSpawnRunner.getUnsupportedReason`);
/// `None` means supported.
pub fn get_unsupported_reason(linux_sandbox_binary: Option<&str>) -> Option<String> {
    if !cfg!(target_os = "linux") {
        return Some("linux-sandbox is only supported on Linux".to_string());
    }
    match linux_sandbox_binary {
        None => Some("linux-sandbox binary is not installed".to_string()),
        Some(p) if !std::path::Path::new(p).exists() => {
            Some(format!("linux-sandbox binary {p} does not exist"))
        }
        Some(_) => None,
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
        b.add_execution_info(&[("supports-graceful-termination".to_string(), "1".to_string())].into());
        b.persistent_process = true;
        b.cgroups_dirs = vec!["/sys/fs/cgroup/x".into()];
        let got = b.build_for_command(&["cc".into(), "-c".into()]).unwrap();
        let want: Vec<&str> = vec![
            "/ls", "-W", "/sbx/execroot/ws", "-T", "60", "-t", "15", "-l", "/o", "-L", "/e", "-w", "/w1", "-w", "/w2",
            "-e", "/tmp", "-M", "/src", "-m", "/dst", "-M", "/same", "-S", "/stats", "-h", "/sbx", "-H", "-N", "-U",
            "-P", "-D", "/dbg", "-i", "-p", "-C", "/sys/fs/cgroup/x", "--", "cc", "-c",
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn fake_root_and_username_are_exclusive() {
        let mut b = LinuxSandboxCommandLineBuilder::new("/ls");
        b.use_fake_root = true;
        b.use_fake_username = true;
        assert_eq!(b.build_for_command(&[]).unwrap_err(), "useFakeUsername and useFakeRoot are exclusive");
    }

    #[test]
    fn unsupported_off_linux() {
        if !cfg!(target_os = "linux") {
            assert!(get_unsupported_reason(Some("/x")).unwrap().contains("only supported on Linux"));
        }
    }
}
