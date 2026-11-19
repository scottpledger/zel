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

use crate::bigint::BigInt;
use crate::syntax::ast::UserFunctionDef;
use crate::types::depset::Depset;
use crate::types::dict::StarlarkDict;
use crate::types::list::{StarlarkError, StarlarkList};
use crate::types::range::RangeList;
use crate::types::select::{SelectorElement, SelectorList};
use crate::types::set::StarlarkSet;
use crate::types::struct_value::StructValue;
use std::collections::BTreeMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use zel_cmdline::Label;

/// Embedder hook rendering `Host(kind, id)` values (`repr()`/`str()`), e.g. `<source file a/b>`.
pub static HOST_REPR: std::sync::OnceLock<fn(&str, &str) -> Option<String>> =
    std::sync::OnceLock::new();

/// Embedder hook checking whether a `Host(kind, id)` value is immutable (and therefore hashable).
/// Used for unexported Starlark `provider()` symbols, which are not hashable until exported.
pub static HOST_IS_IMMUTABLE: std::sync::OnceLock<fn(&str, &str) -> bool> =
    std::sync::OnceLock::new();

/// Java class name Bazel prints for repository-rule context objects without a `repr` override
/// (`<unknown object com.google...>`).
fn unknown_object_class(kind: &str) -> Option<&'static str> {
    match kind {
        "repository_ctx" => Some(
            "com.google.devtools.build.lib.bazel.repository.starlark.StarlarkRepositoryContext",
        ),
        "module_ctx" => Some("com.google.devtools.build.lib.bazel.bzlmod.ModuleExtensionContext"),
        "exec_result" => {
            Some("com.google.devtools.build.lib.bazel.repository.starlark.StarlarkExecutionResult")
        }
        "module_extension" => Some("com.google.devtools.build.lib.bazel.bzlmod.ModuleExtension"),
        "tag_class" => Some("com.google.devtools.build.lib.bazel.bzlmod.TagClass"),
        _ => None,
    }
}

/// `<function NAME from LABEL>` (Bazel prints the label of the `.bzl` file the function was defined in).
fn user_function_repr(func: &crate::syntax::ast::UserFunctionDef) -> String {
    if let Some(repr) = &func.repr_override {
        return repr.clone();
    }
    let label = func
        .module
        .as_ref()
        .and_then(|m| m.label_string.lock().unwrap().clone());
    match label {
        Some(l) => format!("<function {} from {l}>", func.name),
        None => format!("<function {}>", func.name),
    }
}

/// `<built-in function f>` / `<built-in method m of T value>` (host-provided builtins are named `host:...`).
fn builtin_function_repr(name: &str) -> String {
    let Some(h) = name.strip_prefix("host:") else {
        return format!("<built-in function {name}>");
    };
    if let Some(m) = h.strip_prefix("method:") {
        if let Some((recv, method)) = m.split_once('.') {
            return format!("<built-in method {method} of {recv} value>");
        }
    }
    if let Some((recv, method)) = h.split_once('.') {
        return format!("<built-in method {method} of {recv} value>");
    }
    format!("<built-in function {h}>")
}

fn host_repr(kind: &str, id: &str) -> Option<String> {
    HOST_REPR.get().and_then(|f| f(kind, id))
}

/// Runtime value in the Starlark interpreter.
#[derive(Debug, Clone)]
pub enum Value {
    None,
    Bool(bool),
    Int(BigInt),
    Float(f64),
    String(String),
    List(Arc<Mutex<StarlarkList>>),
    Tuple(Vec<Value>),
    Dict(Arc<Mutex<StarlarkDict>>),
    Set(Arc<Mutex<StarlarkSet>>),
    Range(RangeList),
    Depset(Arc<Depset>),
    SelectorList(SelectorList),
    Label(Label),
    Struct(Arc<StructValue>),
    Provider {
        name: String,
    },
    ProviderInstance {
        provider_name: String,
        fields: BTreeMap<String, Value>,
    },
    RuleCallable {
        rule_class: String,
        executable: bool,
        test: bool,
    },
    UserFunction(Arc<UserFunctionDef>),
    BuiltinFunction(String),
    /// Opaque host-provided object (`kind`, `id`), e.g. `repository_ctx.path(...)`.
    /// Attribute access and method calls are dispatched to the embedder's host hook.
    Host(String, String),
}

impl Value {
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::None => false,
            Self::Bool(b) => *b,
            Self::Int(i) => !i.is_zero(),
            Self::Float(f) => *f != 0.0,
            Self::String(s) => !s.is_empty(),
            Self::List(l) => !l.lock().unwrap().elements.is_empty(),
            Self::Tuple(l) => !l.is_empty(),
            Self::Dict(d) => !d.lock().unwrap().is_empty(),
            Self::Set(s) => !s.lock().unwrap().is_empty(),
            Self::Range(r) => r.len > 0,
            Self::Depset(d) => !d.is_empty(),
            Self::SelectorList(s) => !s.elements.is_empty(),
            _ => true,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Self::None => "NoneType",
            Self::Bool(_) => "bool",
            Self::Int(_) => "int",
            Self::Float(_) => "float",
            Self::String(_) => "string",
            Self::List(_) => "list",
            Self::Tuple(_) => "tuple",
            Self::Dict(_) => "dict",
            Self::Set(_) => "set",
            Self::Range(_) => "range",
            Self::Depset(_) => "depset",
            Self::SelectorList(_) => "select",
            Self::Label(_) => "Label",
            Self::Struct(s)
                if s.opaque_class
                    .as_deref()
                    .is_some_and(|c| c.ends_with(".StarlarkOS")) =>
            {
                "repository_os"
            }
            Self::Struct(s)
                if s.opaque_class
                    .as_deref()
                    .is_some_and(|c| c.ends_with(".RepoDefinition")) =>
            {
                "RepoDefinition"
            }
            Self::Struct(_) => "struct",
            Self::Provider { .. } => "Provider",
            Self::ProviderInstance { .. } => "ProviderInstance",
            Self::RuleCallable { .. } => "rule",
            Self::UserFunction(_) => "function",
            Self::BuiltinFunction(_) => "builtin_function_or_method",
            Self::Host(k, _) => match k.as_str() {
                "path" => "path",
                "Facts" => "Facts",
                "file" => "File",
                "target" => "Target",
                "runfiles" => "runfiles",
                "args" => "Args",
                "actions" => "actions",
                "ctx" => "ctx",
                "provider" | "provider_raw" => "Provider",
                "rule" => "rule",
                "pending_download" => "PendingDownload",
                "exec_result" => "exec_result",
                "module_extension" => "ModuleExtension",
                "tag_class" => "TagClass",
                "repository_rule" => "repository_rule",
                "repository_ctx" => "repository_ctx",
                "module_ctx" => "module_ctx",
                "extension_metadata" => "extension_metadata",
                "dotted_version" => "DottedVersion",
                "macro" => "macro",
                "aspect" => "Aspect",
                "subrule" => "subrule",
                "transition" => "transition",
                "toolchain_type" => "ToolchainTypeRequirement",
                "action" => "Action",
                "expanded_directory" => "ExpandedDirectory",
                "template_ctx" => "template_ctx",
                "analysis_failure" => "AnalysisFailure",
                "cc_compilation_outputs" => "CcCompilationOutputs",
                "cc_module_map" => "CcModuleMap",
                "linker_input" => "LinkerInput",
                "lto_compilation_context" => "LtoCompilationContext",
                "attribute" => "Attribute",
                _ => "host_object",
            },
        }
    }

    pub fn is_immutable(&self) -> bool {
        match self {
            Self::None
            | Self::Bool(_)
            | Self::Int(_)
            | Self::Float(_)
            | Self::String(_)
            | Self::Label(_)
            | Self::Range(_)
            | Self::Depset(_)
            | Self::SelectorList(_)
            | Self::UserFunction(_)
            | Self::BuiltinFunction(_)
            | Self::Provider { .. }
            | Self::RuleCallable { .. } => true,
            Self::List(l) => l.lock().unwrap().is_frozen(),
            Self::Dict(d) => d.lock().unwrap().is_frozen(),
            Self::Set(s) => s.lock().unwrap().is_frozen(),
            Self::Tuple(items) => items.iter().all(|x| x.is_immutable()),
            Self::Struct(s) => s.fields.values().all(|x| x.is_immutable()),
            Self::ProviderInstance { fields, .. } => fields.values().all(|x| x.is_immutable()),
            Self::Host(k, id) => HOST_IS_IMMUTABLE.get().map(|f| f(k, id)).unwrap_or(true),
        }
    }

    pub fn is_hashable(&self) -> bool {
        match self {
            Self::None
            | Self::Bool(_)
            | Self::Int(_)
            | Self::Float(_)
            | Self::String(_)
            | Self::Label(_)
            | Self::Depset(_)
            | Self::UserFunction(_)
            | Self::Provider { .. }
            | Self::RuleCallable { .. } => true,
            Self::Tuple(items) => items.iter().all(|x| x.is_hashable()),
            Self::Struct(s) => s.fields.values().all(|x| x.is_hashable()),
            Self::ProviderInstance { fields, .. } => fields.values().all(|x| x.is_hashable()),
            Self::Host(k, id) => HOST_IS_IMMUTABLE.get().map(|f| f(k, id)).unwrap_or(true),
            Self::List(_)
            | Self::Dict(_)
            | Self::Set(_)
            | Self::Range(_)
            | Self::SelectorList(_)
            | Self::BuiltinFunction(_) => false,
        }
    }

    pub fn freeze(&self) {
        match self {
            Self::List(l) => l.lock().unwrap().freeze(),
            Self::Dict(d) => d.lock().unwrap().freeze(),
            Self::Set(s) => s.lock().unwrap().freeze(),
            Self::Tuple(items) => {
                for item in items {
                    item.freeze();
                }
            }
            Self::Struct(s) => {
                for v in s.fields.values() {
                    v.freeze();
                }
            }
            Self::ProviderInstance { fields, .. } => {
                for v in fields.values() {
                    v.freeze();
                }
            }
            _ => {}
        }
    }

    pub fn to_string_list(&self) -> Vec<String> {
        match self {
            Self::List(l) => {
                let guard = l.lock().unwrap();
                guard
                    .elements
                    .iter()
                    .filter_map(|v| match v {
                        Self::String(s) => Some(s.clone()),
                        Self::Label(lbl) => Some(lbl.canonical_form()),
                        _ => None,
                    })
                    .collect()
            }
            Self::Tuple(items) => items
                .iter()
                .filter_map(|v| match v {
                    Self::String(s) => Some(s.clone()),
                    Self::Label(lbl) => Some(lbl.canonical_form()),
                    _ => None,
                })
                .collect(),
            Self::SelectorList(sel) => sel
                .resolve(&[])
                .map(|v| v.to_string_list())
                .unwrap_or_default(),
            Self::Depset(d) => d
                .to_list()
                .iter()
                .filter_map(|v| match v {
                    Self::String(s) => Some(s.clone()),
                    Self::Label(lbl) => Some(lbl.canonical_form()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(i) => i.to_i64(),
            _ => None,
        }
    }

    pub fn add(&self, other: &Self) -> Result<Value, StarlarkError> {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => Ok(Self::Int(a.add(b))),
            (Self::Float(a), Self::Float(b)) => Ok(Self::Float(a + b)),
            (Self::Int(a), Self::Float(b)) => Ok(Self::Float(a.to_f64() + b)),
            (Self::Float(a), Self::Int(b)) => Ok(Self::Float(a + b.to_f64())),
            (Self::String(a), Self::String(b)) => Ok(Self::String(format!("{a}{b}"))),
            (Self::List(a), Self::List(b)) => {
                let mut v = a.lock().unwrap().elements.clone();
                v.extend(b.lock().unwrap().elements.clone());
                Ok(Self::List(Arc::new(Mutex::new(StarlarkList::new(v)))))
            }
            (Self::Tuple(a), Self::Tuple(b)) => {
                let mut v = a.clone();
                v.extend(b.iter().cloned());
                Ok(Self::Tuple(v))
            }
            (Self::Struct(a), Self::Struct(b)) => Ok(Self::Struct(Arc::new(a.add(b)?))),
            (
                Self::ProviderInstance {
                    provider_name: prov_a,
                    fields: f1,
                },
                Self::ProviderInstance {
                    provider_name: prov_b,
                    fields: f2,
                },
            ) if prov_a != "OutputGroupInfo" && prov_b != "OutputGroupInfo" => {
                if prov_a != prov_b {
                    fn printable(p: &str) -> &str {
                        if p.starts_with("%anonymous#") {
                            "<no name>"
                        } else {
                            p.rsplit('%').next().unwrap_or(p)
                        }
                    }
                    return Err(StarlarkError(format!(
                        "Cannot use '+' operator on instances of different providers ({} and {})",
                        printable(prov_a),
                        printable(prov_b)
                    )));
                }
                let mut merged = f1.clone();
                for (k, v) in f2 {
                    if merged.contains_key(k) {
                        return Err(StarlarkError(format!(
                            "cannot add struct instances with common field '{k}'"
                        )));
                    }
                    merged.insert(k.clone(), v.clone());
                }
                Ok(Self::ProviderInstance {
                    provider_name: prov_a.clone(),
                    fields: merged,
                })
            }
            (Self::SelectorList(_), Self::Dict(_)) => Err(StarlarkError(
                "unsupported binary operation: select + dict".to_string(),
            )),
            (Self::Dict(_), Self::SelectorList(_)) => Err(StarlarkError(
                "unsupported binary operation: dict + select".to_string(),
            )),
            (Self::SelectorList(s1), Self::SelectorList(s2)) => {
                if let (Some(t1), Some(t2)) = (s1.elem_type_name(), s2.elem_type_name()) {
                    if t1 != t2 || t1 == "dict" {
                        return Err(StarlarkError(format!(
                            "Cannot combine incompatible types ({}, {})",
                            s1.display_type_name(),
                            s2.display_type_name()
                        )));
                    }
                }
                let mut elements = s1.elements.clone();
                elements.extend(s2.elements.iter().cloned());
                Ok(Self::SelectorList(SelectorList { elements }))
            }
            (Self::SelectorList(s1), rhs) => {
                if let Some(t1) = s1.elem_type_name() {
                    if !matches!(rhs, Self::None) && t1 != rhs.type_name() {
                        return Err(StarlarkError(format!(
                            "Cannot combine incompatible types ({}, {})",
                            s1.display_type_name(),
                            rhs.type_name()
                        )));
                    }
                }
                let mut elements = s1.elements.clone();
                elements.push(SelectorElement::Native(rhs.clone()));
                Ok(Self::SelectorList(SelectorList { elements }))
            }
            (lhs, Self::SelectorList(s2)) => {
                if let Some(t2) = s2.elem_type_name() {
                    if !matches!(lhs, Self::None) && lhs.type_name() != t2 {
                        return Err(StarlarkError(format!(
                            "Cannot combine incompatible types ({}, {})",
                            lhs.type_name(),
                            s2.display_type_name()
                        )));
                    }
                }
                let mut elements = vec![SelectorElement::Native(lhs.clone())];
                elements.extend(s2.elements.iter().cloned());
                Ok(Self::SelectorList(SelectorList { elements }))
            }
            _ => Err(StarlarkError(format!(
                "unsupported binary operation: {} + {}",
                self.type_name(),
                other.type_name()
            ))),
        }
    }

    pub fn bitor(&self, other: &Self) -> Result<Value, StarlarkError> {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => Ok(Self::Int(a.bit_or(b))),
            (Self::Set(a), Self::Set(b)) => {
                let s1 = a.lock().unwrap();
                let s2 = b.lock().unwrap();
                Ok(Self::Set(Arc::new(Mutex::new(s1.union(&s2)))))
            }
            (Self::Dict(a), Self::Dict(b)) => {
                let d1 = a.lock().unwrap();
                let d2 = b.lock().unwrap();
                Ok(Self::Dict(Arc::new(Mutex::new(d1.union(&d2)))))
            }
            (Self::SelectorList(s1), Self::SelectorList(s2)) => {
                if let (Some(t1), Some(t2)) = (s1.elem_type_name(), s2.elem_type_name()) {
                    if t1 != t2 || !matches!(t1, "dict" | "set" | "int") {
                        return Err(StarlarkError(format!(
                            "Cannot combine incompatible types ({}, {})",
                            s1.display_type_name(),
                            s2.display_type_name()
                        )));
                    }
                }
                let mut elements = s1.elements.clone();
                elements.extend(s2.elements.iter().cloned());
                Ok(Self::SelectorList(SelectorList { elements }))
            }
            // `select({...}) | {...}` concatenates dict-valued selects (SelectorList's PIPE).
            (Self::SelectorList(s1), rhs) => {
                if let Some(t1) = s1.elem_type_name() {
                    if !matches!(rhs, Self::None) && t1 != rhs.type_name() {
                        return Err(StarlarkError(format!(
                            "Cannot combine incompatible types ({}, {})",
                            s1.display_type_name(),
                            rhs.type_name()
                        )));
                    }
                }
                if matches!(rhs, Self::Dict(_) | Self::Set(_) | Self::Int(_)) {
                    let mut elements = s1.elements.clone();
                    elements.push(SelectorElement::Native(rhs.clone()));
                    Ok(Self::SelectorList(SelectorList { elements }))
                } else {
                    Err(StarlarkError(format!(
                        "unsupported binary operation: {} | {}",
                        self.type_name(),
                        other.type_name()
                    )))
                }
            }
            (lhs, Self::SelectorList(s2)) => {
                if let Some(t2) = s2.elem_type_name() {
                    if !matches!(lhs, Self::None) && lhs.type_name() != t2 {
                        return Err(StarlarkError(format!(
                            "Cannot combine incompatible types ({}, {})",
                            lhs.type_name(),
                            s2.display_type_name()
                        )));
                    }
                }
                if matches!(lhs, Self::Dict(_) | Self::Set(_) | Self::Int(_)) {
                    let mut elements = vec![SelectorElement::Native(lhs.clone())];
                    elements.extend(s2.elements.iter().cloned());
                    Ok(Self::SelectorList(SelectorList { elements }))
                } else {
                    Err(StarlarkError(format!(
                        "unsupported binary operation: {} | {}",
                        self.type_name(),
                        other.type_name()
                    )))
                }
            }
            _ => Err(StarlarkError(format!(
                "unsupported binary operation: {} | {}",
                self.type_name(),
                other.type_name()
            ))),
        }
    }

    pub fn repr(&self) -> String {
        let mut out = String::new();
        let mut stack = Vec::new();
        self.repr_into(&mut out, &mut stack);
        out
    }

    pub fn repr_into(&self, out: &mut String, stack: &mut Vec<usize>) {
        match self {
            Self::None => out.push_str("None"),
            Self::Bool(true) => out.push_str("True"),
            Self::Bool(false) => out.push_str("False"),
            Self::Int(i) => out.push_str(&i.to_string()),
            Self::Float(f) => out.push_str(&format_float(*f)),
            Self::String(s) => quote_starlark_string(s, out),
            Self::Label(l) => {
                let f = l.unambiguous_canonical_form();
                // The main repository is shown without its `@@`.
                let shown = f
                    .strip_prefix("@@")
                    .filter(|r| r.starts_with("//"))
                    .unwrap_or(&f);
                out.push_str("Label(");
                quote_starlark_string(shown, out);
                out.push(')');
            }
            Self::List(l) => {
                let ptr = Arc::as_ptr(l) as usize;
                if stack.contains(&ptr) {
                    out.push_str("...");
                    return;
                }
                stack.push(ptr);
                let elems = l.lock().unwrap().elements.clone();
                out.push('[');
                for (i, item) in elems.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    item.repr_into(out, stack);
                }
                out.push(']');
                stack.pop();
            }
            Self::Tuple(items) => {
                out.push('(');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    item.repr_into(out, stack);
                }
                if items.len() == 1 {
                    out.push(',');
                }
                out.push(')');
            }
            Self::Dict(d) => {
                let ptr = Arc::as_ptr(d) as usize;
                if stack.contains(&ptr) {
                    out.push_str("...");
                    return;
                }
                stack.push(ptr);
                let entries = d.lock().unwrap().entries.clone();
                out.push('{');
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    k.repr_into(out, stack);
                    out.push_str(": ");
                    v.repr_into(out, stack);
                }
                out.push('}');
                stack.pop();
            }
            Self::Set(s) => {
                let ptr = Arc::as_ptr(s) as usize;
                if stack.contains(&ptr) {
                    out.push_str("...");
                    return;
                }
                let elems = s.lock().unwrap().elements.clone();
                if elems.is_empty() {
                    out.push_str("set()");
                } else {
                    stack.push(ptr);
                    out.push_str("set([");
                    for (i, item) in elems.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        item.repr_into(out, stack);
                    }
                    out.push_str("])");
                    stack.pop();
                }
            }
            Self::Range(r) => out.push_str(&r.to_string()),
            Self::Depset(d) => out.push_str(&d.to_string()),
            Self::SelectorList(s) => {
                for (idx, elem) in s.elements.iter().enumerate() {
                    if idx > 0 {
                        out.push_str(" + ");
                    }
                    match elem {
                        SelectorElement::Native(v) => v.repr_into(out, stack),
                        SelectorElement::Select(entries) => {
                            out.push_str("select({");
                            for (i, (k, v)) in entries.iter().enumerate() {
                                if i > 0 {
                                    out.push_str(", ");
                                }
                                if let Ok(lbl) = zel_cmdline::Label::parse_canonical(k) {
                                    Value::Label(lbl).repr_into(out, stack);
                                } else {
                                    Value::String(k.clone()).repr_into(out, stack);
                                }
                                out.push_str(": ");
                                v.repr_into(out, stack);
                            }
                            out.push_str("})");
                        }
                    }
                }
            }
            Self::Struct(s) => {
                let ptr = Arc::as_ptr(s) as usize;
                if stack.contains(&ptr) {
                    out.push_str("...");
                    return;
                }
                stack.push(ptr);
                s.repr_into(out, stack);
                stack.pop();
            }
            Self::Provider { name } => out.push_str(&format!("<provider {name}>")),
            Self::ProviderInstance {
                provider_name,
                fields,
            } => {
                let shown = provider_name.rsplit('%').next().unwrap_or(provider_name);
                out.push_str(shown);
                out.push('(');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(k);
                    out.push_str(" = ");
                    v.repr_into(out, stack);
                }
                out.push(')');
            }
            Self::RuleCallable { rule_class, .. } => out.push_str(&format!("<rule {rule_class}>")),
            Self::UserFunction(func) => out.push_str(&user_function_repr(func)),
            Self::BuiltinFunction(name) => out.push_str(&builtin_function_repr(name)),
            Self::Host(k, id) => {
                if k == "Facts" {
                    out.push_str("Facts(<opaque, inspect with print()>)");
                } else if k == "path" {
                    quote_starlark_string(id, out);
                } else if k == "pending_download" {
                    let path = id.split_once('|').map(|(_, p)| p).unwrap_or(id);
                    out.push_str(&format!("<pending download to '{path}'>"));
                } else if k == "dotted_version" || k == "obj" {
                    out.push_str(id);
                } else if let Some(r) = unknown_object_class(k) {
                    out.push_str(&format!("<unknown object {r}>"));
                } else if let Some(r) = host_repr(k, id) {
                    out.push_str(&r);
                } else {
                    out.push_str(&format!("<{k} {id}>"));
                }
            }
        }
    }
}

/// Quotes and escapes `s` matching Bazel's `Printer.appendQuoted` (`\"`, `\\`, `\r`, `\n`, `\t`,
/// `\xHH` for `< 32`, and verbatim for `>= 32`).
pub fn quote_starlark_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 32 => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

pub fn format_float(v: f64) -> String {
    if v.is_nan() {
        return "nan".to_string();
    }
    if v == f64::INFINITY {
        return "+inf".to_string();
    }
    if v == f64::NEG_INFINITY {
        return "-inf".to_string();
    }
    if v == 0.0 {
        return if v.is_sign_negative() {
            "-0.0".to_string()
        } else {
            "0.0".to_string()
        };
    }
    // Shortest round-tripping digits (Rust's `{:e}`), laid out like Python's `repr(float)`: fixed notation
    // for decimal exponents in [-4, 16), scientific (`1e+20`, `1e-05`) otherwise.
    let sci = format!("{:e}", v.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let sign = if v < 0.0 { "-" } else { "" };
    if (-4..16).contains(&exp) {
        if exp >= 0 {
            let int_len = exp as usize + 1;
            let (int_part, frac) = if digits.len() <= int_len {
                (
                    format!("{digits}{}", "0".repeat(int_len - digits.len())),
                    String::new(),
                )
            } else {
                (digits[..int_len].to_string(), digits[int_len..].to_string())
            };
            let frac = if frac.is_empty() {
                "0".to_string()
            } else {
                frac
            };
            format!("{sign}{int_part}.{frac}")
        } else {
            format!("{sign}0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let m = if digits.len() > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits
        };
        format!(
            "{sign}{m}e{}{:02}",
            if exp < 0 { '-' } else { '+' },
            exp.abs()
        )
    }
}

pub fn int_equals_float(x: &BigInt, y: f64) -> bool {
    !y.is_nan() && crate::bigint::compare_int_and_float(x, y) == std::cmp::Ordering::Equal
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::None, Self::None) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Int(a), Self::Int(b)) => a == b,
            (Self::Float(a), Self::Float(b)) => {
                if (a.is_nan() && b.is_nan()) || (*a == 0.0 && *b == 0.0) {
                    true
                } else {
                    a == b
                }
            }
            (Self::Int(a), Self::Float(b)) => int_equals_float(a, *b),
            (Self::Float(a), Self::Int(b)) => int_equals_float(b, *a),
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Tuple(a), Self::Tuple(b)) => a == b,
            (Self::List(a), Self::List(b)) => {
                if Arc::ptr_eq(a, b) {
                    true
                } else {
                    let e1 = a.lock().unwrap().elements.clone();
                    let e2 = b.lock().unwrap().elements.clone();
                    e1 == e2
                }
            }
            (Self::Dict(a), Self::Dict(b)) => {
                if Arc::ptr_eq(a, b) {
                    true
                } else {
                    let e1 = a.lock().unwrap().entries.clone();
                    let e2 = b.lock().unwrap().entries.clone();
                    if e1.len() != e2.len() {
                        return false;
                    }
                    for (k, v) in &e1 {
                        match e2.iter().find(|(k2, _)| k2 == k).map(|(_, v2)| v2) {
                            Some(v2) if v2 == v => {}
                            _ => return false,
                        }
                    }
                    true
                }
            }
            (Self::Set(a), Self::Set(b)) => {
                if Arc::ptr_eq(a, b) {
                    true
                } else {
                    let e1 = a.lock().unwrap().elements.clone();
                    let e2 = b.lock().unwrap().elements.clone();
                    e1.len() == e2.len() && e1.iter().all(|x| e2.iter().any(|y| x == y))
                }
            }
            (Self::Range(a), Self::Range(b)) => a == b,
            (Self::Depset(a), Self::Depset(b)) => a.id == b.id,
            (Self::Label(a), Self::Label(b)) => a == b,
            (Self::Struct(a), Self::Struct(b)) => a.fields == b.fields,
            (Self::SelectorList(a), Self::SelectorList(b)) => a == b,
            (
                Self::RuleCallable {
                    rule_class: r1,
                    executable: e1,
                    test: t1,
                },
                Self::RuleCallable {
                    rule_class: r2,
                    executable: e2,
                    test: t2,
                },
            ) => r1 == r2 && e1 == e2 && t1 == t2,
            (Self::Provider { name: n1 }, Self::Provider { name: n2 }) => n1 == n2,
            (
                Self::ProviderInstance {
                    provider_name: p1,
                    fields: f1,
                },
                Self::ProviderInstance {
                    provider_name: p2,
                    fields: f2,
                },
            ) => p1 == p2 && f1 == f2,
            (Self::UserFunction(f1), Self::UserFunction(f2)) => Arc::ptr_eq(f1, f2),
            (Self::BuiltinFunction(b1), Self::BuiltinFunction(b2)) => b1 == b2,
            (Self::Host(k1, i1), Self::Host(k2, i2)) => {
                if k1 != k2 {
                    false
                } else if k1 == "dotted_version" {
                    crate::eval::builtins::dotted_version_canonical_components(i1)
                        == crate::eval::builtins::dotted_version_canonical_components(i2)
                } else {
                    k1 == k2 && i1 == i2
                }
            }
            _ => false,
        }
    }
}

impl Eq for Value {}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::None => 0u8.hash(state),
            Self::Bool(b) => {
                1u8.hash(state);
                b.hash(state);
            }
            Self::Int(i) => {
                2u8.hash(state);
                i.hash(state);
            }
            Self::Float(f) => {
                // Consistency with int: if finite and exact int, hash as BigInt!
                if f.is_nan() {
                    3u8.hash(state);
                    0x7FF8_0000_0000_0000u64.hash(state);
                } else if *f == 0.0 {
                    2u8.hash(state);
                    0i64.hash(state);
                } else {
                    let bits = f.to_bits();
                    let exp = ((bits >> 52) & 0x7FF) as i32;
                    let (mantissa_raw, shift) = if exp == 0 {
                        (bits & 0x000F_FFFF_FFFF_FFFF, -1022 - 52)
                    } else {
                        (
                            (bits & 0x000F_FFFF_FFFF_FFFF) | (1u64 << 52),
                            exp - 1023 - 52,
                        )
                    };
                    if shift >= 0 {
                        // Exact integer
                        let is_neg = (bits >> 63) != 0;
                        let mut bi = BigInt::from_u64(mantissa_raw).shl_bits(shift as usize);
                        if is_neg {
                            bi = bi.neg();
                        }
                        2u8.hash(state);
                        bi.hash(state);
                    } else {
                        3u8.hash(state);
                        f.to_bits().hash(state);
                    }
                }
            }
            Self::String(s) => {
                4u8.hash(state);
                s.hash(state);
            }
            Self::Tuple(items) => {
                5u8.hash(state);
                for item in items {
                    item.hash(state);
                }
            }
            Self::Label(lbl) => {
                6u8.hash(state);
                lbl.canonical_form().hash(state);
            }
            Self::Range(r) => {
                7u8.hash(state);
                r.hash(state);
            }
            Self::Depset(d) => {
                8u8.hash(state);
                d.id.hash(state);
            }
            Self::Host(k, id) => {
                9u8.hash(state);
                k.hash(state);
                if k == "dotted_version" {
                    crate::eval::builtins::dotted_version_canonical_components(id).hash(state);
                } else {
                    id.hash(state);
                }
            }
            Self::Struct(s) => {
                10u8.hash(state);
                for (k, v) in &s.fields {
                    k.hash(state);
                    v.hash(state);
                }
            }
            Self::ProviderInstance {
                provider_name,
                fields,
            } => {
                11u8.hash(state);
                provider_name.hash(state);
                for (k, v) in fields {
                    k.hash(state);
                    v.hash(state);
                }
            }
            Self::List(l) => {
                12u8.hash(state);
                for x in &l.lock().unwrap().elements {
                    x.hash(state);
                }
            }
            Self::Dict(d) => {
                13u8.hash(state);
                for (k, v) in &d.lock().unwrap().entries {
                    k.hash(state);
                    v.hash(state);
                }
            }
            Self::UserFunction(f) => {
                14u8.hash(state);
                (Arc::as_ptr(f) as usize).hash(state);
            }
            Self::BuiltinFunction(n) => {
                15u8.hash(state);
                n.hash(state);
            }
            Self::Provider { name } => {
                16u8.hash(state);
                name.hash(state);
            }
            Self::RuleCallable {
                rule_class,
                executable,
                test,
            } => {
                17u8.hash(state);
                rule_class.hash(state);
                executable.hash(state);
                test.hash(state);
            }
            Self::Set(s) => {
                18u8.hash(state);
                let guard = s.lock().unwrap();
                guard.elements.len().hash(state);
                let mut acc = 0u64;
                for x in &guard.elements {
                    let mut sub = std::collections::hash_map::DefaultHasher::new();
                    x.hash(&mut sub);
                    acc ^= sub.finish();
                }
                acc.hash(state);
            }
            _ => {
                // Non-hashable
                0xFFu8.hash(state);
            }
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("None"),
            Self::Bool(true) => f.write_str("True"),
            Self::Bool(false) => f.write_str("False"),
            Self::Int(i) => write!(f, "{i}"),
            Self::Float(fl) => write!(f, "{}", format_float(*fl)),
            Self::String(s) => f.write_str(s),
            Self::Label(l) => write!(f, "{}", l.unambiguous_canonical_form()),
            Self::List(_) | Self::Tuple(_) | Self::Dict(_) | Self::Set(_) => {
                f.write_str(&self.repr())
            }
            Self::Range(r) => write!(f, "{r}"),
            Self::Depset(d) => write!(f, "{d}"),
            Self::SelectorList(s) => write!(f, "select({:?})", s.elements),
            Self::Struct(s) => write!(f, "{s}"),
            Self::Provider { name } => write!(f, "<provider {name}>"),
            Self::ProviderInstance {
                provider_name,
                fields,
            } => {
                let shown = provider_name.rsplit('%').next().unwrap_or(provider_name);
                write!(f, "{shown}(")?;
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{k} = {}", v.repr())?;
                }
                f.write_str(")")
            }
            Self::RuleCallable { rule_class, .. } => write!(f, "<rule {rule_class}>"),
            Self::UserFunction(func) => write!(f, "{}", user_function_repr(func)),
            Self::BuiltinFunction(name) => write!(f, "{}", builtin_function_repr(name)),
            Self::Host(k, id) => {
                if k == "Facts" {
                    write!(f, "Facts(<opaque, inspect with print()>)")
                } else if k == "path" || k == "dotted_version" || k == "obj" {
                    write!(f, "{id}")
                } else if k == "pending_download" {
                    let path = id.split_once('|').map(|(_, p)| p).unwrap_or(id);
                    write!(f, "<pending download to '{path}'>")
                } else if let Some(r) = unknown_object_class(k) {
                    write!(f, "<unknown object {r}>")
                } else if let Some(r) = host_repr(k, id) {
                    write!(f, "{r}")
                } else {
                    write!(f, "<{k} {id}>")
                }
            }
        }
    }
}
