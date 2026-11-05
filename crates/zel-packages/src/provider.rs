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

//! `provider()` (`StarlarkProvider.java`): process-wide registry of Starlark-defined providers.
//!
//! A provider symbol is the opaque host value `Host("provider", <id>)`; calling it produces a
//! `Value::ProviderInstance` whose `provider_name` is the provider's *key*
//! (`<bzl label>%<exported name>`, so same-named providers from different files do not
//! collide). Built-in providers (`DefaultInfo`, ...) are `Value::Provider { name }` and use
//! the bare name as key.

use std::collections::BTreeMap;
use std::sync::Mutex;
use zel_starlark::{EvalContext, StarlarkError, Value};

struct ProviderDef {
    /// `Some` once exported by a `.bzl` file.
    key: Option<String>,
    /// Allowed fields (schema), or `None` for a schemaless provider.
    fields: Option<Vec<String>>,
    init: Option<Value>,
}

static REGISTRY: Mutex<Vec<ProviderDef>> = Mutex::new(Vec::new());

fn err(ctx: &EvalContext, m: impl AsRef<str>) -> StarlarkError {
    StarlarkError(format!("{}: {}", ctx.call_location, m.as_ref()))
}

/// Implements the `provider(doc, fields, init)` builtin. Returns the provider symbol, or the
/// tuple `(provider, raw_constructor)` if `init` is given.
pub fn define_provider(
    ctx: &mut EvalContext,
    pos: Vec<Value>,
    kw: BTreeMap<String, Value>,
) -> Result<Value, StarlarkError> {
    for k in kw.keys() {
        if !["doc", "fields", "init"].contains(&k.as_str()) {
            return Err(err(ctx, format!("provider() got unexpected keyword argument '{k}'")));
        }
    }
    let fields_v = kw.get("fields").cloned().or_else(|| pos.get(1).cloned());
    let init = kw.get("init").cloned().or_else(|| pos.get(2).cloned());
    let init = match init {
        None | Some(Value::None) => None,
        Some(v) => Some(v),
    };
    let fields = match fields_v {
        None | Some(Value::None) => None,
        Some(Value::Dict(d)) => Some(
            d.lock()
                .unwrap()
                .items()
                .into_iter()
                .filter_map(|(k, _)| k.as_str().map(str::to_string))
                .collect(),
        ),
        Some(v @ (Value::List(_) | Value::Tuple(_))) => {
            let items = zel_starlark::eval::builtins::value_to_sequence(&v)?;
            let mut out = Vec::new();
            for i in items {
                match i.as_str() {
                    Some(s) => out.push(s.to_string()),
                    None => {
                        return Err(err(
                            ctx,
                            format!("in call to provider(), parameter 'fields' got value of type '{}', want 'sequence of strings'", i.type_name()),
                        ))
                    }
                }
            }
            Some(out)
        }
        Some(v) => {
            return Err(err(
                ctx,
                format!("in call to provider(), parameter 'fields' got value of type '{}', want 'sequence or dict or NoneType'", v.type_name()),
            ))
        }
    };
    let mut reg = REGISTRY.lock().unwrap();
    reg.push(ProviderDef { key: None, fields, init: init.clone() });
    let id = (reg.len() - 1).to_string();
    let sym = Value::Host("provider".to_string(), id.clone());
    if init.is_some() {
        Ok(Value::Tuple(vec![sym, Value::Host("provider_raw".to_string(), id)]))
    } else {
        Ok(sym)
    }
}

/// Called when a `.bzl` file finishes loading and assigns `name` to provider `idx`.
pub fn export_provider(idx: usize, name: &str, bzl_label: &str) {
    let mut reg = REGISTRY.lock().unwrap();
    if let Some(d) = reg.get_mut(idx) {
        if d.key.is_none() {
            d.key = Some(format!("{bzl_label}%{name}"));
        }
    }
}

/// Maps the placeholder key of an instance created before its provider was exported
/// (`%anonymous#<idx>%<name>`) to the provider's key once exported; other keys are unchanged.
pub fn resolve_instance_key(name: &str) -> String {
    if let Some(rest) = name.strip_prefix("%anonymous#") {
        if let Some((idx, _)) = rest.split_once('%') {
            if let Ok(i) = idx.parse::<usize>() {
                if let Some(k) = REGISTRY.lock().unwrap().get(i).and_then(|d| d.key.clone()) {
                    return k;
                }
            }
        }
    }
    name.to_string()
}

/// The key of a provider symbol value (used for `dep[Info]`, `Info in dep`, `providers=`).
pub fn provider_key_of(v: &Value) -> Option<String> {
    match v {
        Value::Provider { name } => Some(name.clone()),
        Value::Host(kind, id) if kind == "provider" || kind == "provider_raw" => {
            let reg = REGISTRY.lock().unwrap();
            let idx: usize = id.parse().ok()?;
            let d = reg.get(idx)?;
            Some(d.key.clone().unwrap_or_else(|| format!("%anonymous#{idx}%<provider>")))
        }
        _ => None,
    }
}

/// A late-bound reference to a provider symbol, usable before the symbol is exported
/// (`attr.label(providers = [P])` is evaluated before `P` is assigned a name). Resolve with
/// [`resolve_provider_ref`] at analysis time.
pub fn provider_ref_of(v: &Value) -> Option<String> {
    match v {
        Value::Provider { name } => Some(name.clone()),
        Value::Host(kind, id) if kind == "provider" || kind == "provider_raw" => Some(format!("#pid:{id}")),
        _ => None,
    }
}

/// Resolves a [`provider_ref_of`] reference to the provider key.
pub fn resolve_provider_ref(r: &str) -> String {
    match r.strip_prefix("#pid:") {
        Some(id) => provider_key_of(&Value::Host("provider".into(), id.to_string())).unwrap_or_else(|| r.to_string()),
        None => r.to_string(),
    }
}

/// Display name of a provider key (`MyInfo` for `//a:b.bzl%MyInfo`).
pub fn provider_display_name(key: &str) -> &str {
    key.rsplit('%').next().unwrap_or(key)
}

/// `call:provider` / `call:provider_raw` dispatch (`pos[0]` is the receiver).
pub fn call_provider(
    ctx: &mut EvalContext,
    raw: bool,
    mut pos: Vec<Value>,
    kw: BTreeMap<String, Value>,
) -> Result<Value, StarlarkError> {
    let recv = pos.remove(0);
    let Value::Host(_, id) = &recv else {
        return Err(err(ctx, "internal error: provider call without receiver"));
    };
    let idx: usize = id.parse().unwrap_or(usize::MAX);
    let (fields, init, key) = {
        let reg = REGISTRY.lock().unwrap();
        let d = reg
            .get(idx)
            .ok_or_else(|| err(ctx, "internal error: unknown provider"))?;
        (
            d.fields.clone(),
            d.init.clone(),
            d.key.clone().unwrap_or_else(|| format!("%anonymous#{idx}%struct")),
        )
    };
    let name = provider_display_name(&key).to_string();
    let values: BTreeMap<String, Value> = match (&init, raw) {
        (Some(init), false) => {
            let r = ctx.call_value(init.clone(), pos, kw)?;
            match r {
                Value::Dict(d) => {
                    let mut m = BTreeMap::new();
                    for (k, v) in d.lock().unwrap().items() {
                        let Some(k) = k.as_str() else {
                            return Err(err(
                                ctx,
                                format!("init function of provider {name} must return a dict with string keys"),
                            ));
                        };
                        m.insert(k.to_string(), v);
                    }
                    m
                }
                other => {
                    return Err(err(
                        ctx,
                        format!(
                            "got {} for 'return value of provider init()', want dict",
                            other.type_name()
                        ),
                    ))
                }
            }
        }
        _ => {
            if !pos.is_empty() {
                return Err(err(ctx, format!("{name}: unexpected positional arguments")));
            }
            kw
        }
    };
    if let Some(schema) = &fields {
        let unexpected: Vec<&String> = values.keys().filter(|k| !schema.contains(k)).collect();
        if !unexpected.is_empty() {
            return Err(err(
                ctx,
                format!(
                    "got unexpected field{} '{}' in call to instantiate provider {}",
                    if unexpected.len() > 1 { "s" } else { "" },
                    unexpected.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("', '"),
                    name
                ),
            ));
        }
    }
    Ok(Value::ProviderInstance { provider_name: key, fields: values })
}

/// Host dispatch for provider symbols outside the package loader (analysis time).
pub fn host_dispatch(
    ctx: &mut EvalContext,
    name: &str,
    pos: Vec<Value>,
    kw: BTreeMap<String, Value>,
) -> Result<Value, StarlarkError> {
    match name {
        "call:provider" => call_provider(ctx, false, pos, kw),
        "call:provider_raw" => call_provider(ctx, true, pos, kw),
        "provider" => define_provider(ctx, pos, kw),
        other => Err(StarlarkError(format!("unknown host function '{other}'"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_strips_label() {
        assert_eq!(provider_display_name("//a:b.bzl%MyInfo"), "MyInfo");
        assert_eq!(provider_display_name("DefaultInfo"), "DefaultInfo");
    }
}
