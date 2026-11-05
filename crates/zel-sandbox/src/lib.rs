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

//! Bazel Spawn Runners, Local & Sandboxed Execution, and Persistent Worker Protocol.
//! Mirrors `src/main/java/com/google/devtools/build/lib/sandbox` and `worker`.

pub mod linux;
pub mod process_wrapper;
pub mod sandbox;
pub mod worker;

use std::collections::BTreeMap;
use std::io;
use std::time::Duration;
use zel_proto::worker_protocol::{Input as WorkerInput, WorkRequest, WorkResponse};
use zel_util::{Clock, JavaClock};
use zel_vfs::{FileSystem, PathFragment};

pub use process_wrapper::ResourceUsage;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Spawn {
    pub mnemonic: String,
    pub owner_label: String,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub inputs: Vec<PathFragment>,
    pub outputs: Vec<PathFragment>,
    /// The subset of `outputs` that are directories (tree artifacts): created before the spawn
    /// runs (an empty directory is a valid tree) like `SandboxOutputs.dirs()`.
    pub output_dirs: Vec<PathFragment>,
    pub execution_info: BTreeMap<String, String>,
    /// `--local_sigkill_grace_seconds`; `None` = Bazel's default of 15s.
    pub kill_delay: Option<Duration>,
    /// Inputs that are part of the tool (hashed into the persistent worker key,
    /// `Spawn.getToolFiles` / `WorkerFilesHash`).
    pub tool_inputs: Vec<PathFragment>,
    /// Outputs that may legitimately be absent after a successful run
    /// (`Spawn.isMandatoryOutput` is false: e.g. the jar of a `JavaCompileAction` attempt with a
    /// reduced classpath that asked for the full-classpath fallback).
    pub optional_outputs: Vec<PathFragment>,
}

impl Spawn {
    /// Declared outputs that must exist after a successful run.
    pub fn mandatory_outputs(&self) -> impl Iterator<Item = &PathFragment> {
        self.outputs.iter().filter(move |o| !self.optional_outputs.contains(o))
    }

    /// The `timeout` execution requirement in seconds (`Spawn.getTimeout`).
    pub fn timeout_secs(&self) -> Option<f64> {
        self.execution_info
            .get("timeout")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|s| *s > 0.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnStatus {
    Success,
    NonZeroExit,
    Timeout,
    ExecutionFailed,
    MissingInputs,
    MissingOutputs,
}

/// The command line/environment/cwd actually used (shown in failure messages; differs from the
/// spawn's own with `--sandbox_debug`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutedCommand {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnResult {
    pub status: SpawnStatus,
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub wall_time_ms: u64,
    pub runner_name: String,
    /// Paths of missing declared inputs/outputs for `MissingInputs`/`MissingOutputs`.
    pub missing: Vec<String>,
    pub usage: Option<ResourceUsage>,
    pub executed_command: Option<ExecutedCommand>,
}

impl SpawnResult {
    pub fn is_ok(&self) -> bool {
        self.status == SpawnStatus::Success && self.exit_code == 0
    }
}

/// Default grace period between SIGTERM and SIGKILL (`--local_sigkill_grace_seconds`, 15s).
pub const DEFAULT_KILL_DELAY_MS: u64 = 15_000;

/// Hooks used by dynamic execution (`SandboxedSpawnStrategy.StopConcurrentSpawns` and thread
/// interruption in Bazel). `lock_outputs(exit_code)` is called by a runner right before it
/// publishes its outputs into the exec root; returning `false` means this branch lost the race
/// and must not touch any output (the runner then reports `ErrorKind::Interrupted`). `cancel`
/// kills the running process.
#[derive(Clone, Default)]
pub struct ExecHooks {
    pub cancel: Option<process_wrapper::CancelFlag>,
    pub lock_outputs: Option<std::sync::Arc<dyn Fn(i32) -> bool + Send + Sync>>,
}

impl ExecHooks {
    /// Calls the output lock; `true` when execution may publish outputs.
    pub fn lock(&self, exit_code: i32) -> bool {
        self.lock_outputs.as_ref().map_or(true, |f| f(exit_code))
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancel.as_ref().map_or(false, |c| c.is_cancelled())
    }
}

/// Error returned by a branch that was cancelled / lost the dynamic-execution race.
pub fn cancelled_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "spawn cancelled")
}

/// Executes a `Spawn` locally inside `execroot` through the process-wrapper port
/// (`LocalSpawnRunner`): own process group, captured stdout/stderr, `timeout` execution
/// requirement enforced with SIGTERM then SIGKILL after the kill delay, declared inputs must
/// exist, stale outputs are removed and output parent directories created beforehand, and
/// declared outputs must exist afterwards (otherwise status is `MissingOutputs`).
pub fn execute_spawn_local(
    spawn: &Spawn,
    execroot: &PathFragment,
    fs: &dyn FileSystem,
) -> io::Result<SpawnResult> {
    execute_spawn_local_with(spawn, execroot, fs, &ExecHooks::default())
}

/// [`execute_spawn_local`] with dynamic-execution hooks. A standalone spawn writes its outputs
/// in place, so (like `LocalSpawnRunner`) the output lock is taken before the process starts.
pub fn execute_spawn_local_with(
    spawn: &Spawn,
    execroot: &PathFragment,
    fs: &dyn FileSystem,
    hooks: &ExecHooks,
) -> io::Result<SpawnResult> {
    let clock = JavaClock;
    let start_ms = clock.current_time_millis();
    let mk = |status, exit_code, stdout: Vec<u8>, stderr: Vec<u8>| SpawnResult {
        status,
        exit_code,
        stdout,
        stderr,
        wall_time_ms: clock.current_time_millis().saturating_sub(start_ms),
        runner_name: "local".to_string(),
        missing: Vec::new(),
        usage: None,
        executed_command: None,
    };
    if spawn.argv.is_empty() {
        return Ok(mk(SpawnStatus::ExecutionFailed, 1, Vec::new(), b"empty command line\n".to_vec()));
    }

    // Declared inputs must exist (symlinks are followed like stat()).
    let missing_inputs: Vec<String> = spawn
        .inputs
        .iter()
        .filter(|inp| std::fs::symlink_metadata(execroot.get_relative(inp).path_string()).is_err())
        .map(|i| i.path_string().to_string())
        .collect();
    if !missing_inputs.is_empty() {
        let mut r = mk(SpawnStatus::MissingInputs, 1, Vec::new(), Vec::new());
        r.missing = missing_inputs;
        return Ok(r);
    }

    if hooks.is_cancelled() || !hooks.lock(0) {
        return Err(cancelled_error());
    }
    // Prepare outputs: delete stale ones, create parent directories.
    for out in &spawn.outputs {
        let abs_out = execroot.get_relative(out);
        let p = abs_out.path_string();
        if let Ok(md) = std::fs::symlink_metadata(p) {
            if md.is_dir() {
                let _ = std::fs::remove_dir_all(p);
            } else {
                let _ = std::fs::remove_file(p);
            }
        }
        if let Some(parent) = abs_out.parent_directory() {
            fs.create_directory_and_parents(&parent)?;
        }
    }
    for dir in &spawn.output_dirs {
        fs.create_directory_and_parents(&execroot.get_relative(dir))?;
    }

    let mut env = spawn.env.clone();
    if !env.contains_key("PATH") {
        // Mirrors the default shell-toolchain PATH Bazel injects for shell-based actions.
        env.insert("PATH".to_string(), "/bin:/usr/bin:/usr/local/bin".to_string());
    }
    let kill_delay = spawn
        .execution_info
        .get("zel-kill-delay-ms")
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .or(spawn.kill_delay)
        .unwrap_or(Duration::from_millis(DEFAULT_KILL_DELAY_MS));
    let opt = process_wrapper::ProcessWrapperOptions {
        graceful_sigterm: spawn.execution_info.contains_key("supports-graceful-termination"),
        timeout_secs: spawn.timeout_secs().unwrap_or(0.0),
        kill_delay_secs: kill_delay.as_secs_f64(),
        args: spawn.argv.clone(),
        cancel: hooks.cancel.clone(),
        ..Default::default()
    };
    let o = match process_wrapper::run(&opt, Some(&env), Some(execroot.path_string())) {
        Ok(o) => o,
        Err(e) => {
            return Ok(mk(
                SpawnStatus::ExecutionFailed,
                127,
                Vec::new(),
                format!("Failed to execute '{}': {e}\n", spawn.argv[0]).into_bytes(),
            ));
        }
    };
    if o.cancelled {
        return Err(cancelled_error());
    }
    let usage = o.usage;
    let done = |status, code, stdout, stderr| {
        let mut r = mk(status, code, stdout, stderr);
        r.usage = usage;
        r
    };
    if o.timed_out {
        let secs = spawn.timeout_secs().unwrap_or(0.0);
        let mut stderr = o.stderr;
        stderr.extend_from_slice(format!("Action timed out after {secs:.0}s\n").as_bytes());
        return Ok(done(SpawnStatus::Timeout, 142, o.stdout, stderr));
    }
    let code = o.termination.exit_code();
    if code != 0 {
        return Ok(done(SpawnStatus::NonZeroExit, code, o.stdout, o.stderr));
    }
    let missing_outputs: Vec<String> = spawn
        .mandatory_outputs()
        .filter(|o| std::fs::symlink_metadata(execroot.get_relative(o).path_string()).is_err())
        .map(|o| o.path_string().to_string())
        .collect();
    if !missing_outputs.is_empty() {
        let mut r = done(SpawnStatus::MissingOutputs, 0, o.stdout, o.stderr);
        r.missing = missing_outputs;
        return Ok(r);
    }
    Ok(done(SpawnStatus::Success, 0, o.stdout, o.stderr))
}

/// Builds a `WorkRequest` protobuf message for persistent workers (`worker_protocol.proto`).
pub fn build_worker_request(
    spawn: &Spawn,
    request_id: i32,
    input_digests: &[(String, Vec<u8>)],
) -> WorkRequest {
    let inputs = input_digests
        .iter()
        .map(|(path, digest)| WorkerInput {
            path: path.clone(),
            digest: digest.clone(),
        })
        .collect();
    WorkRequest {
        arguments: spawn.argv.clone(),
        inputs,
        request_id,
        cancel: false,
        verbosity: 0,
        sandbox_dir: String::new(),
    }
}

pub fn worker_response_to_spawn_result(resp: &WorkResponse) -> SpawnResult {
    SpawnResult {
        status: if resp.exit_code == 0 {
            SpawnStatus::Success
        } else {
            SpawnStatus::NonZeroExit
        },
        exit_code: resp.exit_code,
        stdout: Vec::new(),
        stderr: resp.output.as_bytes().to_vec(),
        wall_time_ms: 0,
        runner_name: "worker".to_string(),
        missing: Vec::new(),
        usage: None,
        executed_command: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worker_request_round_trip() {
        let spawn = Spawn {
            mnemonic: "Javac".to_string(),
            owner_label: "//src:lib".to_string(),
            argv: vec!["--flagfile=params".to_string()],
            ..Default::default()
        };
        let req = build_worker_request(&spawn, 7, &[("src/A.java".to_string(), vec![1, 2, 3])]);
        let encoded = req.encode_to_vec();
        let decoded = WorkRequest::decode(&encoded).unwrap();
        assert_eq!(decoded.request_id, 7);
        assert_eq!(decoded.inputs[0].path, "src/A.java");
    }

    fn tmp(name: &str) -> PathFragment {
        let d = std::env::temp_dir().join(format!("zel-sbx-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        PathFragment::create(d.to_str().unwrap())
    }

    fn sh(script: &str, outputs: &[&str], inputs: &[&str]) -> Spawn {
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
    fn nonzero_exit_is_reported_with_output() {
        let root = tmp("nz");
        let fs = zel_vfs::LocalFileSystem::default();
        let r = execute_spawn_local(&sh("echo out; echo err >&2; exit 3", &[], &[]), &root, &fs).unwrap();
        assert_eq!(r.status, SpawnStatus::NonZeroExit);
        assert_eq!(r.exit_code, 3);
        assert_eq!(r.stdout, b"out\n");
        assert_eq!(r.stderr, b"err\n");
    }

    #[test]
    fn missing_output_and_input_are_errors_not_stubs() {
        let root = tmp("mo");
        let fs = zel_vfs::LocalFileSystem::default();
        let r = execute_spawn_local(&sh("true", &["a/out.txt"], &[]), &root, &fs).unwrap();
        assert_eq!(r.status, SpawnStatus::MissingOutputs);
        assert_eq!(r.missing, vec!["a/out.txt"]);
        let r = execute_spawn_local(&sh("true", &[], &["nope.txt"]), &root, &fs).unwrap();
        assert_eq!(r.status, SpawnStatus::MissingInputs);
        let r = execute_spawn_local(&sh("echo hi > a/out.txt", &["a/out.txt"], &[]), &root, &fs).unwrap();
        assert!(r.is_ok());
        assert_eq!(std::fs::read_to_string(format!("{}/a/out.txt", root)).unwrap(), "hi\n");
    }

    #[test]
    fn stale_outputs_removed_and_env_is_exact() {
        let root = tmp("env");
        let fs = zel_vfs::LocalFileSystem::default();
        std::fs::write(format!("{}/o", root), "stale").unwrap();
        let mut s = sh("test ! -e o && echo \"$FOO:$HOME\" > o", &["o"], &[]);
        s.env.insert("FOO".into(), "bar".into());
        let r = execute_spawn_local(&s, &root, &fs).unwrap();
        assert!(r.is_ok(), "{:?}", r);
        assert_eq!(std::fs::read_to_string(format!("{}/o", root)).unwrap(), "bar:\n");
    }

    #[test]
    fn timeout_kills_process_group() {
        let root = tmp("to");
        let fs = zel_vfs::LocalFileSystem::default();
        let mut s = sh("sleep 30 & sleep 30", &[], &[]);
        s.execution_info.insert("timeout".into(), "0.2".into());
        s.execution_info.insert("zel-kill-delay-ms".into(), "100".into());
        let t = std::time::Instant::now();
        let r = execute_spawn_local(&s, &root, &fs).unwrap();
        assert_eq!(r.status, SpawnStatus::Timeout);
        assert!(t.elapsed().as_secs() < 5);
    }
}
