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

//! Bzlmod external dependency system: `MODULE.bazel` evaluation, registries, discovery,
//! Minimal Version Selection, repo mapping, `MODULE.bazel.lock`, module extensions, repository
//! rules and the `mod` / `fetch` / `vendor` commands.
//! Mirrors `src/main/java/com/google/devtools/build/lib/bazel/{bzlmod,repository}`.

pub mod archive;
pub mod codecs;
pub mod commands;
pub mod ctx;
mod ctxargs;
pub mod download;
pub mod fetch;
pub mod hash;
pub mod json;
pub mod lockfile;
pub mod modcommand;
pub mod model;
pub mod module_file;
pub mod netrc;
pub mod patch;
pub mod registry;
pub mod repo_contents_cache;
pub mod resolve;
pub mod resolver;
#[cfg(test)]
mod round7a_tests;
pub mod rt;
pub mod semantics_fp;
pub mod session;
pub mod tidy;
pub mod usages_digest;
pub mod vendor;
pub mod version;
pub mod wasm;

pub use model::*;
pub use version::Version;

/// Formats a local filesystem path as a `file://` (or `file:///C:/...` on Windows) URL.
pub fn file_url_for_path(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

pub(crate) fn canonical_no_unc(p: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let c = std::fs::canonicalize(p)?;
    #[cfg(windows)]
    {
        let s = c.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            return Ok(std::path::PathBuf::from(format!(r"\\{rest}")));
        }
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return Ok(std::path::PathBuf::from(rest));
        }
    }
    Ok(c)
}
