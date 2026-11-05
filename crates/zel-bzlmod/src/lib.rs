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
pub mod patch;
pub mod codecs;
pub mod download;
pub mod commands;
pub mod ctx;
mod ctxargs;
pub mod fetch;
pub mod hash;
pub mod json;
pub mod lockfile;
pub mod model;
pub mod netrc;
pub mod module_file;
pub mod modcommand;
pub mod registry;
pub mod repo_contents_cache;
pub mod resolve;
pub mod resolver;
pub mod rt;
pub mod semantics_fp;
pub mod session;
pub mod tidy;
pub mod usages_digest;
pub mod vendor;
pub mod version;
pub mod wasm;
#[cfg(test)]
mod round7a_tests;

pub use model::*;
pub use version::Version;
