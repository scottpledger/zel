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

//! Native (Rust) side of Bazel's language-rule builtins: the `cc_common.internal_DO_NOT_USE()`
//! API (`CcStarlarkInternal.java`) that rules_cc's Starlark implementation is built on, and its
//! siblings for Java / Python / proto.
//!
//! Functions are addressed by name (`cc_internal.<fn>`); hosts route calls here via [`call`].
//! Functions that do not depend on analysis state live here; those that register actions need the
//! analysis engine and are handled by `zel-analysis`. Anything not yet ported returns an explicit
//! "not implemented" error — never a fake result.

pub mod cc_features;
pub mod cc_internal;
pub mod natives;

use std::collections::BTreeMap;
use zel_starlark::{StarlarkError, Value};

pub type Kwargs = BTreeMap<String, Value>;

/// Dispatches `name` (e.g. `cc_internal.freeze`). `None` if `name` is not a builtin of this crate.
pub fn call(name: &str, pos: Vec<Value>, kw: Kwargs) -> Option<Result<Value, StarlarkError>> {
    if let Some(f) = name.strip_prefix("cc_internal.") {
        return Some(cc_internal::call(f, pos, kw));
    }
    if let Some(f) = name.strip_prefix("builtins.") {
        return Some(natives::call(f, pos, kw));
    }
    if name == "cc_common.internal_DO_NOT_USE" {
        return Some(Ok(cc_internal::internal_struct()));
    }
    if let Some(m) = name.strip_prefix("method:cc_toolchain_features.") {
        return Some(cc_internal::features_method(m, pos, kw));
    }
    if let Some(m) = name.strip_prefix("method:FeatureConfiguration.") {
        return Some(cc_internal::feature_configuration_method(m, pos, kw));
    }
    if let Some(m) = name.strip_prefix("method:dotted_version.") {
        return Some(natives::dotted_version_member(true, m, &pos));
    }
    if let Some(a) = name.strip_prefix("attr:dotted_version.") {
        return Some(natives::dotted_version_member(false, a, &pos));
    }
    None
}

type PathResolver = fn(&Value) -> Option<String>;
static PATH_OF: std::sync::OnceLock<PathResolver> = std::sync::OnceLock::new();

/// Installs the function that maps a `File`/artifact value to its exec path (set once by the
/// analysis engine, which owns the artifact representation).
pub fn set_artifact_path_resolver(f: PathResolver) {
    let _ = PATH_OF.set(f);
}

pub(crate) fn artifact_path(v: &Value) -> Option<String> {
    PATH_OF.get().and_then(|f| f(v))
}
