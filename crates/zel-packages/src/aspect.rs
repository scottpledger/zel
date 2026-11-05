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

//! Starlark `aspect(...)` definitions (`StarlarkDefinedAspect`): the parsed definition plus a
//! process-wide registry. An aspect value in Starlark is `Value::Host("aspect", <index>)`.

use crate::attr::Attribute;
use std::sync::{Arc, Mutex};
use zel_starlark::Value;

/// A parsed `aspect()` call.
pub struct AspectDef {
    pub id: usize,
    pub implementation: Value,
    /// `attr_aspects`: attribute names along which the aspect propagates (`"*"` = all label
    /// attributes).
    pub attr_aspects: Vec<String>,
    /// The aspect's own attributes (implicit `_x` dependencies and parameters).
    pub attrs: Vec<Attribute>,
    /// `required_providers`: a target is only visited if it advertises/has all providers of one
    /// of the groups (provider references, see [`crate::provider::resolve_provider_ref`]).
    pub required_providers: Vec<Vec<String>>,
    /// `required_aspect_providers`.
    pub required_aspect_providers: Vec<Vec<String>>,
    /// `provides`: provider keys the implementation must return.
    pub provides: Vec<String>,
    /// `requires`: aspects that run before this one on the same target.
    pub requires: Vec<Value>,
    /// `toolchains`: raw toolchain type values.
    pub toolchains: Vec<Value>,
    pub fragments: Vec<String>,
    pub apply_to_generating_rules: bool,
    pub exec_compatible_with: Vec<String>,
    /// Canonical label of the `.bzl` file (set when exported).
    pub defined_in: Mutex<String>,
    /// Exported global name (set when the defining module finishes evaluating).
    pub name: Mutex<Option<String>>,
    pub doc: String,
}

impl AspectDef {
    /// `//pkg:file.bzl%name` once exported, `<unexported aspect #N>` before.
    pub fn display_id(&self) -> String {
        match self.name.lock().unwrap().as_ref() {
            Some(n) => format!("{}%{n}", self.defined_in.lock().unwrap()),
            None => format!("<unexported aspect #{}>", self.id),
        }
    }
}

static ASPECTS: Mutex<Vec<Arc<AspectDef>>> = Mutex::new(Vec::new());

/// Registers a new aspect; the returned value is what `aspect()` evaluates to.
pub fn register_aspect(mut def: AspectDef) -> Value {
    let mut reg = ASPECTS.lock().unwrap();
    def.id = reg.len();
    let id = def.id;
    reg.push(Arc::new(def));
    Value::Host("aspect".to_string(), id.to_string())
}

/// The aspect definition behind an `aspect()` value.
pub fn aspect_of(v: &Value) -> Option<Arc<AspectDef>> {
    match v {
        Value::Host(kind, id) if kind == "aspect" => {
            let idx: usize = id.parse().ok()?;
            ASPECTS.lock().unwrap().get(idx).cloned()
        }
        _ => None,
    }
}

/// Records the exported name of an aspect (assignment to a global of a `.bzl` file).
pub fn export_aspect(idx: usize, name: &str, defined_in: &str) {
    if let Some(a) = ASPECTS.lock().unwrap().get(idx) {
        let mut n = a.name.lock().unwrap();
        if n.is_none() {
            *n = Some(name.to_string());
            *a.defined_in.lock().unwrap() = defined_in.to_string();
        }
    }
}

/// A parsed `subrule(implementation, attrs, toolchains, fragments)` definition. A subrule value
/// is `Value::Host("subrule", <index>)`; calling it inside a rule/aspect implementation runs
/// `implementation(ctx, *args, **kwargs, **implicit_attrs)`.
pub struct SubruleDef {
    pub implementation: Value,
    /// Implicit (`_`-prefixed) attributes passed to the implementation as keyword arguments.
    pub attrs: Vec<Attribute>,
    pub toolchains: Vec<Value>,
    pub fragments: Vec<String>,
}

static SUBRULES: Mutex<Vec<Arc<SubruleDef>>> = Mutex::new(Vec::new());

pub fn register_subrule(def: SubruleDef) -> Value {
    let mut reg = SUBRULES.lock().unwrap();
    reg.push(Arc::new(def));
    Value::Host("subrule".to_string(), (reg.len() - 1).to_string())
}

pub fn subrule_of(v: &Value) -> Option<Arc<SubruleDef>> {
    match v {
        Value::Host(kind, id) if kind == "subrule" => SUBRULES.lock().unwrap().get(id.parse::<usize>().ok()?).cloned(),
        _ => None,
    }
}
