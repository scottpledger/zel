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

//! The built-in `@bazel_tools` repository. Content is Bazel's own `//tools` tree (BUILD.tools
//! renamed to BUILD like `embedded_tools`), embedded into the binary from `resources/`.

use std::path::Path;

include!(concat!(env!("OUT_DIR"), "/files.rs"));

/// All embedded files as `(repo-relative path, bytes)`.
pub fn files() -> &'static [(&'static str, &'static [u8])] {
    FILES
}

/// Files of Bazel's Starlark builtins (`@_builtins`, from `src/main/starlark/builtins_bzl`).
pub fn builtins_files() -> &'static [(&'static str, &'static [u8])] {
    BUILTINS
}

/// Writes the `@_builtins` tree to `dir`.
pub fn materialize_builtins(dir: &Path) -> Result<(), String> {
    for (rel, bytes) in BUILTINS {
        let dest = dir.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&dest, bytes).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    Ok(())
}

/// Returns the content of a repo-relative file (e.g. `tools/cpp/unix_cc_toolchain_config.bzl`).
pub fn read(path: &str) -> Option<&'static [u8]> {
    FILES.iter().find(|(p, _)| *p == path).map(|(_, b)| *b)
}

/// Writes the repository to `dir`.
pub fn materialize(dir: &Path) -> Result<(), String> {
    for (rel, bytes) in FILES {
        let dest = dir.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&dest, bytes).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_real_toolchain_sources() {
        let cfg = read("tools/cpp/unix_cc_toolchain_config.bzl").expect("unix cc config");
        assert!(std::str::from_utf8(cfg).unwrap().contains("rules_cc"));
        assert!(read("tools/cpp/cc_configure.bzl").is_some());
        assert!(read("tools/cpp/lib_cc_configure.bzl").is_some());
        assert!(read("tools/python/python_bootstrap_template.txt").is_some());
        assert!(read("tools/jdk/default_java_toolchain.bzl").is_some());
        assert!(read("tools/build_defs/repo/http.bzl").is_some());
        assert!(read("MODULE.bazel").is_some());
        let b = std::str::from_utf8(read("tools/cpp/BUILD").unwrap()).unwrap();
        assert!(b.contains("toolchain_type(name = \"toolchain_type\")"));
    }

    #[test]
    fn builtins_present() {
        let exports = builtins_files().iter().find(|(p, _)| *p == "exports.bzl").expect("exports.bzl");
        assert!(std::str::from_utf8(exports.1).unwrap().contains("@_builtins//:bazel/exports.bzl"));
        assert!(builtins_files().iter().any(|(p, _)| *p == "common/cc/cc_common_bazel.bzl"));
    }

    #[test]
    fn materializes() {
        let d = std::env::temp_dir().join(format!("zel-bt-{}", std::process::id()));
        materialize(&d).unwrap();
        assert!(d.join("tools/cpp/BUILD").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
