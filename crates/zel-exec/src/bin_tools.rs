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

//! Port of `com.google.devtools.build.lib.exec.BinTools`: auxiliary embedded binaries under `_bin`.

use crate::digest::DigestFunction;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use zel_sandbox::{set_executable_mode, VirtualInput};
use zel_vfs::PathFragment;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinToolMetadata {
    pub digest: Vec<u8>,
    pub size: u64,
}

/// An embedded tool input pointing at an absolute install-base path (`BinTools.PathActionInput`).
#[derive(Debug)]
pub struct PathActionInput {
    pub path: PathBuf,
    pub exec_path: PathFragment,
    lock: Mutex<()>,
    metadata: Mutex<Option<BinToolMetadata>>,
    digest: Mutex<Option<Vec<u8>>>,
}

impl PathActionInput {
    pub fn new(path: PathBuf, exec_path: PathFragment) -> Self {
        Self {
            path,
            exec_path,
            lock: Mutex::new(()),
            metadata: Mutex::new(None),
            digest: Mutex::new(None),
        }
    }

    pub fn to_virtual_input(&self) -> VirtualInput {
        VirtualInput::Path {
            exec_path: self.exec_path.clone(),
            source_path: self.path.clone(),
            executable: true,
        }
    }

    /// Materializes the embedded binary to `output_path`, creating parent directories and
    /// setting the executable bit twice (`PathActionInput.atomicallyWriteTo`).
    pub fn atomically_write_to(
        &self,
        output_path: &Path,
        digest_fn: DigestFunction,
    ) -> io::Result<Vec<u8>> {
        let _guard = self.lock.lock().unwrap();
        let cached_digest = self.digest.lock().unwrap().clone();
        if let Some(d) = cached_digest {
            if output_path.exists() {
                return Ok(d);
            }
        }
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = std::fs::read(&self.path)?;
        std::fs::write(output_path, &bytes)?;
        set_executable_mode(output_path)?;
        set_executable_mode(output_path)?;
        let d = digest_fn.hash(&bytes);
        *self.digest.lock().unwrap() = Some(d.clone());
        Ok(d)
    }

    pub fn get_metadata(&self, digest_fn: DigestFunction) -> io::Result<BinToolMetadata> {
        let _guard = self.lock.lock().unwrap();
        if let Some(m) = self.metadata.lock().unwrap().clone() {
            return Ok(m);
        }
        let bytes = std::fs::read(&self.path)?;
        let m = BinToolMetadata {
            digest: digest_fn.hash(&bytes),
            size: bytes.len() as u64,
        };
        *self.metadata.lock().unwrap() = Some(m.clone());
        Ok(m)
    }
}

/// Maintains the mapping from embedded tool paths to `PathActionInput`s (`_bin/<embedded_path>`).
#[derive(Debug, Clone)]
pub struct BinTools {
    embedded_binaries_root: PathBuf,
    embedded_tools: Vec<String>,
    action_inputs: BTreeMap<String, Arc<PathActionInput>>,
}

impl BinTools {
    pub fn for_embedded_bin(embedded_binaries_root: &Path, tools: &[String]) -> Self {
        let bin_root = PathFragment::create("_bin");
        let mut kept = Vec::new();
        let mut action_inputs = BTreeMap::new();
        for tool in tools {
            if tool.starts_with("embedded_tools/") {
                continue;
            }
            kept.push(tool.clone());
            let abs_path = embedded_binaries_root.join(tool);
            let exec_path = bin_root.get_relative_str(tool);
            action_inputs.insert(
                tool.clone(),
                Arc::new(PathActionInput::new(abs_path, exec_path)),
            );
        }
        Self {
            embedded_binaries_root: embedded_binaries_root.to_path_buf(),
            embedded_tools: kept,
            action_inputs,
        }
    }

    pub fn from_embedded_root(embedded_binaries_root: &Path) -> io::Result<Self> {
        fn scan(root: &Path, rel: &str, out: &mut Vec<String>) -> io::Result<()> {
            for e in std::fs::read_dir(root)? {
                let e = e?;
                let ft = e.file_type()?;
                let name = e.file_name().to_string_lossy().into_owned();
                let child_rel = if rel.is_empty() {
                    name
                } else {
                    format!("{rel}/{name}")
                };
                if ft.is_file() {
                    out.push(child_rel);
                } else if ft.is_dir() {
                    scan(&e.path(), &child_rel, out)?;
                }
            }
            Ok(())
        }
        let mut tools = Vec::new();
        if embedded_binaries_root.exists() {
            scan(embedded_binaries_root, "", &mut tools)?;
        }
        tools.sort();
        Ok(Self::for_embedded_bin(embedded_binaries_root, &tools))
    }

    pub fn get_action_input(&self, embedded_path: &str) -> Option<&Arc<PathActionInput>> {
        self.action_inputs.get(embedded_path)
    }

    pub fn get_embedded_path(&self, embedded_path: &str) -> Option<PathBuf> {
        if !self.embedded_tools.iter().any(|t| t == embedded_path) {
            return None;
        }
        Some(self.embedded_binaries_root.join(embedded_path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bin_tools_exec_paths_and_materialization() {
        let d = std::env::temp_dir().join(format!("zel-bintools-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let emb = d.join("install");
        let er = d.join("execroot");
        std::fs::create_dir_all(emb.join("sub")).unwrap();
        std::fs::create_dir_all(&er).unwrap();
        std::fs::write(emb.join("process-wrapper"), b"pw-bytes").unwrap();
        std::fs::write(emb.join("sub/tool"), b"sub-bytes").unwrap();

        let bt = BinTools::for_embedded_bin(
            &emb,
            &[
                "embedded_tools/skip.sh".into(),
                "process-wrapper".into(),
                "sub/../escape_up".into(),
                "/abs/override".into(),
                "sub/tool".into(),
            ],
        );
        assert!(bt.get_action_input("embedded_tools/skip.sh").is_none());
        assert_eq!(
            bt.get_action_input("process-wrapper")
                .unwrap()
                .exec_path
                .path_string(),
            "_bin/process-wrapper"
        );
        assert_eq!(
            bt.get_action_input("sub/../escape_up")
                .unwrap()
                .exec_path
                .path_string(),
            "_bin/escape_up"
        );
        assert_eq!(
            bt.get_action_input("/abs/override")
                .unwrap()
                .exec_path
                .path_string(),
            "/abs/override"
        );
        assert_eq!(
            bt.get_embedded_path("process-wrapper").unwrap(),
            emb.join("process-wrapper")
        );

        let inp = bt.get_action_input("sub/tool").unwrap();
        let out_file = er.join(inp.exec_path.path_string());
        let dig = inp
            .atomically_write_to(&out_file, DigestFunction::Sha256)
            .unwrap();
        assert_eq!(std::fs::read(&out_file).unwrap(), b"sub-bytes");
        assert_eq!(
            dig,
            inp.get_metadata(DigestFunction::Sha256).unwrap().digest
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&out_file).unwrap().permissions().mode();
            assert_ne!(mode & 0o111, 0);
        }
    }
}
