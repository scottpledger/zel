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
    /// True only for builtin pre-registered providers (`ProtoInfo`, `ProtoLangToolchainInfo`).
    pre_registered: bool,
}

static REGISTRY: Mutex<Vec<ProviderDef>> = Mutex::new(Vec::new());

fn host_is_immutable(kind: &str, id: &str) -> bool {
    if kind == "provider" || kind == "provider_raw" {
        if let Ok(idx) = id.parse::<usize>() {
            return REGISTRY
                .lock()
                .unwrap()
                .get(idx)
                .is_some_and(|d| d.key.is_some());
        }
        return false;
    }
    true
}

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
            return Err(err(
                ctx,
                format!("provider() got unexpected keyword argument '{k}'"),
            ));
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
    let _ = zel_starlark::HOST_IS_IMMUTABLE.set(host_is_immutable);
    let mut reg = REGISTRY.lock().unwrap();
    reg.push(ProviderDef {
        key: None,
        fields,
        init: init.clone(),
        pre_registered: false,
    });
    let id = (reg.len() - 1).to_string();
    let sym = Value::Host("provider".to_string(), id.clone());
    if init.is_some() {
        Ok(Value::Tuple(vec![
            sym,
            Value::Host("provider_raw".to_string(), id),
        ]))
    } else {
        Ok(sym)
    }
}

/// Pre-registers (or looks up) an exported Starlark provider symbol for `key`
/// (`<canonical bzl label>%<exported name>`), returning the shared `Value::Host("provider", id)`.
pub fn exported_builtin_provider(key: &str, fields: Option<&[&str]>) -> Value {
    let _ = zel_starlark::HOST_IS_IMMUTABLE.set(host_is_immutable);
    let mut reg = REGISTRY.lock().unwrap();
    if let Some((idx, _)) = reg
        .iter()
        .enumerate()
        .find(|(_, d)| d.key.as_deref() == Some(key))
    {
        return Value::Host("provider".to_string(), idx.to_string());
    }
    reg.push(ProviderDef {
        key: Some(key.to_string()),
        fields: fields.map(|fs| fs.iter().map(|s| (*s).to_string()).collect()),
        init: None,
        pre_registered: true,
    });
    let id = (reg.len() - 1).to_string();
    Value::Host("provider".to_string(), id)
}

/// The three external `ProtoInfo` Starlark provider symbols published by `ProtoConstants.java`.
pub fn external_proto_infos() -> Vec<Value> {
    [
        "@@com_google_protobuf//bazel/private:proto_info.bzl%ProtoInfo",
        "@@com_google_protobuf+//bazel/private:proto_info.bzl%ProtoInfo",
        "@@protobuf+//bazel/private:proto_info.bzl%ProtoInfo",
    ]
    .into_iter()
    .map(|k| exported_builtin_provider(k, None))
    .collect()
}

/// The `ProtoLangToolchainInfo` Starlark provider registered for
/// `@@com_google_protobuf+//bazel/common:proto_lang_toolchain_info.bzl`.
pub fn proto_lang_toolchain_info_provider() -> Value {
    const FIELDS: &[&str] = &[
        "out_replacement_format_flag",
        "output_files",
        "plugin_format_flag",
        "plugin",
        "runtime",
        "provided_proto_sources",
        "proto_compiler",
        "protoc_opts",
        "progress_message",
        "mnemonic",
        "allowlist_different_package",
        "toolchain_type",
    ];
    exported_builtin_provider(
        "@@com_google_protobuf+//bazel/common:proto_lang_toolchain_info.bzl%ProtoLangToolchainInfo",
        Some(FIELDS),
    )
}

/// Called when a `.bzl` file finishes loading and assigns `name` to provider `idx`.
/// If a pre-registered builtin provider symbol already owns `{bzl_label}%{name}`, updates its
/// schema/init from `idx` and returns that existing symbol so identity (`==` / hashing) matches.
pub fn export_provider(idx: usize, name: &str, bzl_label: &str) -> Option<Value> {
    let full_key = format!("{bzl_label}%{name}");
    let mut reg = REGISTRY.lock().unwrap();
    let existing_idx = reg
        .iter()
        .enumerate()
        .find(|(i, d)| *i != idx && d.pre_registered && d.key.as_deref() == Some(full_key.as_str()))
        .map(|(i, _)| i);
    let (new_fields, new_init) = match reg.get_mut(idx) {
        Some(d) => {
            if d.key.is_none() {
                d.key = Some(full_key.clone());
            }
            (d.fields.clone(), d.init.clone())
        }
        None => return None,
    };
    if let Some(ex_idx) = existing_idx {
        if let Some(ex) = reg.get_mut(ex_idx) {
            if new_fields.is_some() {
                ex.fields = new_fields;
            }
            if new_init.is_some() {
                ex.init = new_init;
            }
        }
        return Some(Value::Host("provider".to_string(), ex_idx.to_string()));
    }
    None
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
            Some(
                d.key
                    .clone()
                    .unwrap_or_else(|| format!("%anonymous#{idx}%<provider>")),
            )
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
        Value::Host(kind, id) if kind == "provider" || kind == "provider_raw" => {
            Some(format!("#pid:{id}"))
        }
        _ => None,
    }
}

/// Resolves a [`provider_ref_of`] reference to the provider key.
pub fn resolve_provider_ref(r: &str) -> String {
    match r.strip_prefix("#pid:") {
        Some(id) => provider_key_of(&Value::Host("provider".into(), id.to_string()))
            .unwrap_or_else(|| r.to_string()),
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
            d.key
                .clone()
                .unwrap_or_else(|| format!("%anonymous#{idx}%struct")),
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
                    unexpected
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join("', '"),
                    name
                ),
            ));
        }
    }
    Ok(Value::ProviderInstance {
        provider_name: key,
        fields: values,
    })
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
