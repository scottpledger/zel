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

//! `clean` (`CleanCommand`): output cleaning, `--expunge`, `--async`, `--expunge_async`, removal
//! of the convenience symlinks and server shutdown on expunge.

use super::product_name;
use crate::{writeln_bytes, BlazeRuntime};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use zel_options::ParsedCommandOptions;
use zel_util::ExitCode;

fn random_uuid_string() -> String {
    let mut b = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut b);
    } else {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        b[..8].copy_from_slice(&(t as u64).to_le_bytes());
        b[8..].copy_from_slice(&((t >> 64) as u64).to_le_bytes());
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// `AsyncDirectoryCleaner.moveToTempDirectory`: `<parent>/<name>_tmp_<pid>_<uuid>`.
fn move_to_temp(path: &Path) -> std::io::Result<PathBuf> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "Cannot move root directory {} to temporary directory",
                path.display()
            ),
        )
    })?;
    let uuid = random_uuid_string();
    let base = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = parent.join(format!("{base}_tmp_{}_{uuid}", std::process::id()));
    std::fs::rename(path, &temp)?;
    Ok(temp)
}

/// `AsyncDirectoryCleaner.spawnDaemonizedDeletion` without the `daemonize` helper: a detached
/// shell (own process group) that makes directories writable and removes the tree.
#[cfg(unix)]
fn spawn_deletion(parent: &Path, name: &str) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "/usr/bin/find \"./$1\" -type d -not -perm -u=rwx -exec /bin/chmod -f u=rwx {} +; /bin/rm -rf \"./$1\"",
            "_",
            name,
        ])
        .current_dir(parent)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
}

#[cfg(not(unix))]
fn spawn_deletion(parent: &Path, name: &str) -> std::io::Result<()> {
    let target = parent.join(name);
    std::thread::spawn(move || {
        make_writable_tree(&target);
        let _ = std::fs::remove_dir_all(&target);
    });
    Ok(())
}

/// `deleteTreesBelow`: removes everything inside `dir` but keeps `dir`.
fn delete_trees_below(dir: &Path) -> std::io::Result<()> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in rd.flatten() {
        let p = entry.path();
        make_writable_tree(&p);
        let md = std::fs::symlink_metadata(&p)?;
        if md.is_dir() {
            std::fs::remove_dir_all(&p)?;
        } else {
            std::fs::remove_file(&p)?;
        }
    }
    Ok(())
}

/// Output trees may be read-only (`chmod -w` on artifacts); make directories writable first.
fn make_writable_tree(p: &Path) {
    if let Ok(md) = std::fs::symlink_metadata(p) {
        #[cfg(unix)]
        if md.is_dir() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755));
            if let Ok(rd) = std::fs::read_dir(p) {
                for e in rd.flatten() {
                    make_writable_tree(&e.path());
                }
            }
        }
        #[cfg(not(unix))]
        #[allow(clippy::permissions_set_readonly_false)]
        {
            let mut perms = md.permissions();
            if perms.readonly() {
                perms.set_readonly(false);
                let _ = std::fs::set_permissions(p, perms);
            }
            if md.is_dir() {
                if let Ok(rd) = std::fs::read_dir(p) {
                    for e in rd.flatten() {
                        make_writable_tree(&e.path());
                    }
                }
            }
        }
    }
}

impl BlazeRuntime {
    /// `OutputDirectoryLinksUtils.removeOutputDirectoryLinks`.
    fn remove_convenience_symlinks(&self, prefix: &str, stderr: &mut Vec<u8>) {
        if prefix.starts_with('/')
            || Path::new(prefix)
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return;
        }
        let ws = self.dirs.workspace_dir.path_string();
        if ws.is_empty() {
            return;
        }
        let ws_name = Path::new(ws)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut failures = Vec::new();
        for suffix in [ws_name.as_str(), "out", "bin", "testlogs", "genfiles"] {
            let name = format!("{prefix}{suffix}");
            let link = Path::new(ws).join(&name);
            match std::fs::symlink_metadata(&link) {
                Ok(md) if md.file_type().is_symlink() => {
                    if let Err(e) = std::fs::remove_file(&link) {
                        failures.push(format!("{}: {e}", link.display()));
                    }
                }
                _ => {}
            }
        }
        // `removeDirectoryAndParents(workspace, prefix)`: a prefix naming a directory, e.g. `out/`.
        if prefix.contains('/') {
            let base = if prefix.ends_with('/') {
                Some(PathBuf::from(prefix.trim_end_matches('/')))
            } else {
                Path::new(prefix).parent().map(Path::to_path_buf)
            };
            let mut dir = base;
            while let Some(d) = dir {
                if d.as_os_str().is_empty() || d == Path::new(".") {
                    break;
                }
                if std::fs::remove_dir(Path::new(ws).join(&d)).is_err() {
                    break;
                }
                dir = d.parent().map(Path::to_path_buf);
            }
        }
        if !failures.is_empty() {
            writeln_bytes(
                stderr,
                &format!(
                    "WARNING: failed to remove one or more convenience symlinks for prefix '{prefix}':\n  {}",
                    failures.join("\n  ")
                ),
            );
        }
    }

    /// `CleanCommand.exec`.
    pub(crate) fn cmd_clean(
        &self,
        args: &[String],
        _stdout: &mut [u8],
        stderr: &mut Vec<u8>,
        termination_expected: &mut bool,
    ) -> ExitCode {
        let parsed = match ParsedCommandOptions::parse_for_command("clean", args) {
            Ok(p) => p,
            Err(e) => {
                writeln_bytes(stderr, &e.format_bazel_error());
                return ExitCode::COMMAND_LINE_ERROR;
            }
        };
        if !parsed.targets.is_empty() {
            writeln_bytes(
                stderr,
                &format!(
                    "ERROR: Unrecognized arguments: {}",
                    parsed.targets.join(" ")
                ),
            );
            return ExitCode::COMMAND_LINE_ERROR;
        }
        let expunge =
            parsed.expunge || parsed.expunge_async || parsed.get_bool("expunge").unwrap_or(false);
        let r#async = parsed.get_bool("async").unwrap_or(false) || parsed.expunge_async;
        // `canUseAsync`: asynchronous deletion is unavailable on Windows.
        let async_ok = !cfg!(windows);
        let r#async = if r#async && !async_ok {
            let fallback = if expunge {
                "--expunge"
            } else {
                "synchronous clean"
            };
            writeln_bytes(
                stderr,
                &format!("INFO: --async cannot be used on non-Linux platforms, falling back to {fallback}"),
            );
            false
        } else {
            r#async
        };
        let banner = if r#async || !async_ok {
            "Starting clean."
        } else {
            "Starting clean (this may take a while). Use --async if the clean takes more than several minutes."
        };
        writeln_bytes(stderr, &format!("INFO: {banner}"));

        let output_base = PathBuf::from(self.dirs.output_base.path_string());
        let fail = |stderr: &mut Vec<u8>, msg: String| {
            writeln_bytes(stderr, &format!("ERROR: {msg}"));
            ExitCode::LOCAL_ENVIRONMENTAL_ERROR
        };

        // `clearCaches` + `WorkerModule.beforeCommand`: shut down persistent workers and clear caches.
        crate::exec_phase::shutdown_worker_pools(self.dirs.output_base.path_string());
        for cache_name in ["action_cache", "action_cache.bad", "action_cache.tmp"] {
            let p = output_base.join(cache_name);
            if let Ok(md) = std::fs::symlink_metadata(&p) {
                let res = if md.is_dir() {
                    make_writable_tree(&p);
                    std::fs::remove_dir_all(&p)
                } else {
                    std::fs::remove_file(&p)
                };
                if let Err(e) = res {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        return fail(stderr, format!("{}: {e}", p.display()));
                    }
                }
            }
        }
        let _ = self.fs.delete(&self.dirs.action_cache_file());
        if let Ok(cache) = self.action_cache.lock() {
            cache.clear();
        }

        let symlink_prefix = match parsed.get_str("symlink_prefix") {
            Some(p) => p.to_string(),
            None => format!("{}-", product_name()),
        };

        if expunge && !r#async {
            self.skyframe.evaluator.clear();
            // Delete the subdirectories first, keeping server directory intact until daemon shutdown
            make_writable_tree(&output_base);
            if let Ok(rd) = std::fs::read_dir(&output_base) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    if p.file_name().map(|n| n == "server").unwrap_or(false) {
                        continue;
                    }
                    make_writable_tree(&p);
                    if p.is_dir() {
                        let _ = std::fs::remove_dir_all(&p);
                    } else {
                        let _ = std::fs::remove_file(&p);
                    }
                }
            }
        } else if expunge {
            self.skyframe.evaluator.clear();
            match move_to_temp(&output_base) {
                Ok(tmp) => {
                    let real_tmp = std::fs::canonicalize(&tmp).unwrap_or_else(|_| tmp.clone());
                    writeln_bytes(
                        stderr,
                        &format!(
                            "INFO: Output base moved to {} for deletion",
                            real_tmp.display()
                        ),
                    );
                    let name = tmp
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if let Err(e) = spawn_deletion(tmp.parent().unwrap_or(Path::new("/")), &name) {
                        return fail(stderr, format!("{}: {e}", real_tmp.display()));
                    }
                }
                Err(e) => return fail(stderr, format!("{}: {e}", output_base.display())),
            }
        } else {
            self.skyframe.evaluator.clear();
            let execroot = output_base.join("execroot");
            let is_symlink = std::fs::symlink_metadata(&execroot)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false);
            if is_symlink {
                if let Err(e) = std::fs::remove_file(&execroot) {
                    return fail(stderr, format!("{}: {e}", execroot.display()));
                }
            } else if r#async {
                if execroot.exists() {
                    match move_to_temp(&execroot) {
                        Ok(tmp) => {
                            let real_tmp =
                                std::fs::canonicalize(&tmp).unwrap_or_else(|_| tmp.clone());
                            writeln_bytes(
                                stderr,
                                &format!(
                                    "INFO: Output tree moved to {} for deletion",
                                    real_tmp.display()
                                ),
                            );
                            let name = tmp
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            if let Err(e) =
                                spawn_deletion(tmp.parent().unwrap_or(Path::new("/")), &name)
                            {
                                return fail(stderr, format!("{}: {e}", tmp.display()));
                            }
                        }
                        Err(e) => return fail(stderr, format!("{}: {e}", execroot.display())),
                    }
                }
            } else {
                make_writable_tree(&execroot);
                if let Err(e) = delete_trees_below(&execroot) {
                    return fail(stderr, format!("{}: {e}", execroot.display()));
                }
            }
        }

        self.remove_convenience_symlinks(&symlink_prefix, stderr);

        if expunge {
            // `BlazeCommandResult.shutdownOnSuccess()`.
            self.shutdown_requested.store(true, Ordering::SeqCst);
            self.skip_action_cache_save.store(true, Ordering::SeqCst);
            if !r#async {
                self.expunge_requested.store(true, Ordering::SeqCst);
            }
            *termination_expected = true;
        }
        ExitCode::SUCCESS
    }
}
