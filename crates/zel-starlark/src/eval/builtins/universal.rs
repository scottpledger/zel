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
use crate::syntax::checks::did_you_mean;
use crate::types::dict::StarlarkDict;
use crate::types::list::{StarlarkError, StarlarkList};
use crate::types::mutability::Mutability;
use crate::types::range::RangeList;
use crate::types::set::StarlarkSet;
use crate::types::value::Value;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

pub const DICT_METHODS: &[&str] = &[
    "clear",
    "get",
    "items",
    "keys",
    "pop",
    "popitem",
    "setdefault",
    "update",
    "values",
];
pub const LIST_METHODS: &[&str] = &[
    "append", "clear", "extend", "index", "insert", "pop", "remove",
];
pub const SET_METHODS: &[&str] = &[
    "add",
    "clear",
    "difference",
    "difference_update",
    "discard",
    "intersection",
    "intersection_update",
    "isdisjoint",
    "issubset",
    "issuperset",
    "pop",
    "remove",
    "symmetric_difference",
    "symmetric_difference_update",
    "union",
    "update",
];
pub const STRING_METHODS: &[&str] = &[
    "capitalize",
    "count",
    "elems",
    "endswith",
    "find",
    "format",
    "index",
    "isalnum",
    "isalpha",
    "isdigit",
    "islower",
    "isspace",
    "istitle",
    "isupper",
    "join",
    "lower",
    "lstrip",
    "partition",
    "removeprefix",
    "removesuffix",
    "replace",
    "rfind",
    "rindex",
    "rpartition",
    "rsplit",
    "rstrip",
    "split",
    "splitlines",
    "startswith",
    "strip",
    "title",
    "upper",
];
pub const LABEL_FIELDS: &[&str] = &[
    "name",
    "package",
    "repo_name",
    "workspace_name",
    "workspace_root",
];
pub const LABEL_METHODS: &[&str] = &["package_relative", "relative", "same_package_label"];

pub fn dir_attrs(x: &Value) -> Vec<String> {
    let mut attrs = Vec::new();
    match x {
        Value::Struct(s) => attrs.extend(s.fields.keys().cloned()),
        Value::ProviderInstance { fields, .. } => attrs.extend(fields.keys().cloned()),
        Value::Dict(_) => attrs.extend(DICT_METHODS.iter().map(|s| (*s).to_string())),
        Value::List(_) => attrs.extend(LIST_METHODS.iter().map(|s| (*s).to_string())),
        Value::Set(_) => attrs.extend(SET_METHODS.iter().map(|s| (*s).to_string())),
        Value::String(_) => attrs.extend(STRING_METHODS.iter().map(|s| (*s).to_string())),
        Value::Depset(_) => attrs.push("to_list".to_string()),
        Value::Label(_) => {
            attrs.extend(LABEL_FIELDS.iter().map(|s| (*s).to_string()));
            attrs.extend(LABEL_METHODS.iter().map(|s| (*s).to_string()));
        }
        Value::Host(kind, _) => match kind.as_str() {
            "target" => attrs.extend(
                [
                    "actions",
                    "data_runfiles",
                    "default_runfiles",
                    "files",
                    "files_to_run",
                    "label",
                    "output_groups",
                ]
                .iter()
                .map(|s| (*s).to_string()),
            ),
            "file" => attrs.extend(
                [
                    "basename",
                    "dirname",
                    "extension",
                    "is_directory",
                    "is_source",
                    "is_symlink",
                    "owner",
                    "path",
                    "root",
                    "short_path",
                    "tree_relative_path",
                ]
                .iter()
                .map(|s| (*s).to_string()),
            ),
            "dotted_version" => {
                attrs.extend(["compare_to", "version"].iter().map(|s| (*s).to_string()))
            }
            "repository_ctx" => attrs.extend(
                [
                    "attr",
                    "delete",
                    "download",
                    "download_and_extract",
                    "execute",
                    "extract",
                    "file",
                    "getenv",
                    "name",
                    "original_name",
                    "os",
                    "patch",
                    "path",
                    "read",
                    "rename",
                    "repo_metadata",
                    "report_progress",
                    "symlink",
                    "template",
                    "watch",
                    "watch_tree",
                    "which",
                    "workspace_root",
                ]
                .iter()
                .map(|s| (*s).to_string()),
            ),
            "module_ctx" => attrs.extend(
                [
                    "download",
                    "download_and_extract",
                    "execute",
                    "extension_metadata",
                    "extract",
                    "facts",
                    "file",
                    "getenv",
                    "is_dev_dependency",
                    "modules",
                    "os",
                    "path",
                    "read",
                    "report_progress",
                    "root_module_has_non_dev_dependency",
                    "tag_sort_key",
                    "watch",
                    "which",
                ]
                .iter()
                .map(|s| (*s).to_string()),
            ),
            "path" => attrs.extend(
                [
                    "basename",
                    "dirname",
                    "exists",
                    "get_child",
                    "is_dir",
                    "readdir",
                    "realpath",
                ]
                .iter()
                .map(|s| (*s).to_string()),
            ),
            "exec_result" => attrs.extend(
                ["return_code", "stderr", "stdout"]
                    .iter()
                    .map(|s| (*s).to_string()),
            ),
            "pending_download" => attrs.push("wait".to_string()),
            _ => {}
        },
        _ => {}
    }
    attrs.sort();
    attrs.dedup();
    attrs
}

pub fn fallible_merge_sort<T: Clone, F>(items: &mut [T], cmp: &mut F) -> Result<(), StarlarkError>
where
    F: FnMut(&T, &T) -> Result<Ordering, StarlarkError>,
{
    let n = items.len();
    if n <= 1 {
        return Ok(());
    }
    let mid = n / 2;
    fallible_merge_sort(&mut items[..mid], cmp)?;
    fallible_merge_sort(&mut items[mid..], cmp)?;
    let left = items[..mid].to_vec();
    let right = items[mid..].to_vec();
    let (mut i, mut j, mut k) = (0, 0, 0);
    while i < left.len() && j < right.len() {
        if cmp(&left[i], &right[j])? != Ordering::Greater {
            items[k] = left[i].clone();
            i += 1;
        } else {
            items[k] = right[j].clone();
            j += 1;
        }
        k += 1;
    }
    while i < left.len() {
        items[k] = left[i].clone();
        i += 1;
        k += 1;
    }
    while j < right.len() {
        items[k] = right[j].clone();
        j += 1;
        k += 1;
    }
    Ok(())
}

pub fn eval_universal_function(
    name: &str,
    pos_args: &[Value],
    kw_args: &BTreeMap<String, Value>,
) -> Result<Option<Value>, StarlarkError> {
    eval_universal_function_in(name, pos_args, kw_args, &Mutability::create())
}

pub fn eval_universal_function_in(
    name: &str,
    pos_args: &[Value],
    kw_args: &BTreeMap<String, Value>,
    mutability: &Arc<Mutability>,
) -> Result<Option<Value>, StarlarkError> {
    match name {
        "abs" => {
            let x = pos_args
                .first()
                .ok_or_else(|| StarlarkError("abs() takes exactly one argument".to_string()))?;
            match x {
                Value::Int(i) => Ok(Some(Value::Int(i.abs()))),
                Value::Float(f) => Ok(Some(Value::Float(f.abs()))),
                other => Err(StarlarkError(format!(
                    "type '{}' does not support abs()",
                    other.type_name()
                ))),
            }
        }
        "all" => {
            let x = pos_args
                .first()
                .ok_or_else(|| StarlarkError("all() takes exactly one argument".to_string()))?;
            let items = value_to_sequence(x)?;
            for item in items {
                if !item.is_truthy() {
                    return Ok(Some(Value::Bool(false)));
                }
            }
            Ok(Some(Value::Bool(true)))
        }
        "any" => {
            let x = pos_args
                .first()
                .ok_or_else(|| StarlarkError("any() takes exactly one argument".to_string()))?;
            let items = value_to_sequence(x)?;
            for item in items {
                if item.is_truthy() {
                    return Ok(Some(Value::Bool(true)));
                }
            }
            Ok(Some(Value::Bool(false)))
        }
        "bool" => {
            let b = pos_args.first().map(|v| v.is_truthy()).unwrap_or(false);
            Ok(Some(Value::Bool(b)))
        }
        "dict" => {
            let mut dict = StarlarkDict::new_in(Arc::clone(mutability));
            if let Some(first) = pos_args.first() {
                match first {
                    Value::Dict(d) => {
                        let entries = d.lock().unwrap().entries.clone();
                        for (k, v) in entries {
                            let _ = dict.insert(k, v);
                        }
                    }
                    Value::List(l) => {
                        let elems = l.lock().unwrap().elements.clone();
                        for item in &elems {
                            let (k, v) = unpack_pair(item)?;
                            dict.insert(k, v)?;
                        }
                    }
                    Value::Tuple(t) => {
                        for item in t {
                            let (k, v) = unpack_pair(item)?;
                            dict.insert(k, v)?;
                        }
                    }
                    other => {
                        return Err(StarlarkError(format!(
                            "cannot convert {} to dict",
                            other.type_name()
                        )));
                    }
                }
            }
            for (k, v) in kw_args {
                dict.insert(Value::String(k.clone()), v.clone())?;
            }
            Ok(Some(Value::Dict(Arc::new(Mutex::new(dict)))))
        }
        "dir" => {
            let x = pos_args
                .first()
                .ok_or_else(|| StarlarkError("dir() takes exactly one argument".to_string()))?;
            let attrs = dir_attrs(x);
            Ok(Some(Value::List(Arc::new(Mutex::new(
                StarlarkList::new_in(
                    attrs.into_iter().map(Value::String).collect(),
                    Arc::clone(mutability),
                ),
            )))))
        }
        "enumerate" => {
            let x = pos_args.first().ok_or_else(|| {
                StarlarkError("enumerate() takes at least 1 argument".to_string())
            })?;
            let start = pos_args
                .get(1)
                .or_else(|| kw_args.get("start"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let items = value_to_sequence(x)?;
            let mut pairs = Vec::with_capacity(items.len());
            for (i, item) in items.into_iter().enumerate() {
                let idx = BigInt::from_i64(start + i as i64);
                pairs.push(Value::Tuple(vec![Value::Int(idx), item]));
            }
            Ok(Some(Value::List(Arc::new(Mutex::new(
                StarlarkList::new_in(pairs, Arc::clone(mutability)),
            )))))
        }
        "float" => {
            if pos_args.is_empty() {
                return Ok(Some(Value::Float(0.0)));
            }
            let val = match &pos_args[0] {
                Value::Float(f) => *f,
                Value::Int(i) => i.to_f64(),
                Value::Bool(b) => {
                    if *b {
                        1.0
                    } else {
                        0.0
                    }
                }
                Value::String(s) => match s.trim() {
                    "NaN" | "nan" => f64::NAN,
                    "+Inf" | "+inf" | "Inf" | "inf" => f64::INFINITY,
                    "-Inf" | "-inf" => f64::NEG_INFINITY,
                    other => other.parse::<f64>().map_err(|e| {
                        StarlarkError(format!("could not convert string to float: {other:?}: {e}"))
                    })?,
                },
                other => {
                    return Err(StarlarkError(format!(
                        "cannot convert {} to float",
                        other.type_name()
                    )))
                }
            };
            Ok(Some(Value::Float(val)))
        }
        "getattr" => {
            let x = pos_args.first().ok_or_else(|| {
                StarlarkError("getattr() requires at least 2 arguments".to_string())
            })?;
            let name = pos_args.get(1).and_then(|v| v.as_str()).ok_or_else(|| {
                StarlarkError("getattr() attribute name must be string".to_string())
            })?;
            let default = pos_args
                .get(2)
                .cloned()
                .or_else(|| kw_args.get("default").cloned());

            match x {
                Value::Struct(s) => match s.get(name) {
                    Ok(val) => Ok(Some(val)),
                    Err(e) => {
                        if let Some(def) = default {
                            Ok(Some(def))
                        } else {
                            Err(e)
                        }
                    }
                },
                Value::ProviderInstance { fields, .. } => match fields.get(name) {
                    Some(v) => Ok(Some(v.clone())),
                    None => match default {
                        Some(d) => Ok(Some(d)),
                        None => {
                            let dym = did_you_mean(name, fields.keys().map(|s| s.as_str()));
                            Err(StarlarkError(format!(
                                "'{}' value has no field or method '{name}'{dym}",
                                x.type_name()
                            )))
                        }
                    },
                },
                Value::Label(l) => match name {
                    "name" => Ok(Some(Value::String(l.name().to_string()))),
                    "package" => Ok(Some(Value::String(l.package_name().to_string()))),
                    "repo_name" | "workspace_name" => {
                        if let Some(msg) = l.repository().not_visible_error_message() {
                            return Err(StarlarkError(msg));
                        }
                        Ok(Some(Value::String(l.repository().name().to_string())))
                    }
                    "workspace_root" => {
                        if let Some(msg) = l.repository().not_visible_error_message() {
                            return Err(StarlarkError(msg));
                        }
                        Ok(Some(Value::String(if l.repository().is_main() {
                            String::new()
                        } else {
                            format!("external/{}", l.repository().name())
                        })))
                    }
                    _ => match default {
                        Some(d) => Ok(Some(d)),
                        None => {
                            let attrs = dir_attrs(x);
                            let dym = did_you_mean(name, attrs.iter().map(|s| s.as_str()));
                            Err(StarlarkError(format!(
                                "'Label' value has no field or method '{name}'{dym}"
                            )))
                        }
                    },
                },
                _ => {
                    if let Some(def) = default {
                        Ok(Some(def))
                    } else {
                        let attrs = dir_attrs(x);
                        let dym = did_you_mean(name, attrs.iter().map(|s| s.as_str()));
                        Err(StarlarkError(format!(
                            "'{}' value has no field or method '{name}'{dym}",
                            x.type_name()
                        )))
                    }
                }
            }
        }
        "hasattr" => {
            let x = pos_args.first().ok_or_else(|| {
                StarlarkError("hasattr() requires at least 2 arguments".to_string())
            })?;
            let name = pos_args.get(1).and_then(|v| v.as_str()).ok_or_else(|| {
                StarlarkError("hasattr() attribute name must be string".to_string())
            })?;

            let res = match x {
                Value::Struct(s) => s.fields.contains_key(name),
                Value::Dict(_) => DICT_METHODS.contains(&name),
                Value::ProviderInstance { fields, .. } => fields.contains_key(name),
                Value::Depset(_) => name == "to_list",
                Value::Label(_) => LABEL_FIELDS.contains(&name) || LABEL_METHODS.contains(&name),
                Value::List(_) => LIST_METHODS.contains(&name),
                Value::Set(_) => SET_METHODS.contains(&name),
                Value::Tuple(_) => false,
                Value::String(_) => STRING_METHODS.contains(&name),
                Value::Host(kind, _) => match kind.as_str() {
                    "dotted_version" => name == "compare_to" || name == "version",
                    _ => false,
                },
                _ => false,
            };
            Ok(Some(Value::Bool(res)))
        }
        "hash" => {
            let s = match pos_args.first() {
                Some(Value::String(s)) => s.as_str(),
                Some(other) => {
                    return Err(StarlarkError(format!(
                    "in call to hash(), parameter 'value' got value of type '{}', want 'string'",
                    other.type_name()
                )))
                }
                None => {
                    return Err(StarlarkError(
                        "hash() missing 1 required positional argument: value".to_string(),
                    ))
                }
            };
            // Deterministic Java String.hashCode() returning 32-bit integer
            let mut h: i32 = 0;
            for c in s.chars() {
                h = h.wrapping_mul(31).wrapping_add(c as i32);
            }
            Ok(Some(Value::Int(BigInt::from_i64(h as i64))))
        }
        "int" => {
            if pos_args.is_empty() {
                return Ok(Some(Value::Int(BigInt::zero())));
            }
            let base = pos_args
                .get(1)
                .or_else(|| kw_args.get("base"))
                .and_then(|v| v.as_i64())
                .unwrap_or(10) as u32;

            let val = match &pos_args[0] {
                Value::Int(i) => i.clone(),
                Value::Float(f) => {
                    crate::bigint::finite_double_to_int_exact(*f).map_err(StarlarkError)?
                }
                Value::Bool(b) => BigInt::from_u64(if *b { 1 } else { 0 }),
                Value::String(s) => {
                    if base == 0 {
                        BigInt::from_str_auto(s).map_err(StarlarkError)?
                    } else {
                        BigInt::from_str_radix(s, base).map_err(StarlarkError)?
                    }
                }
                other => {
                    return Err(StarlarkError(format!(
                        "cannot convert {} to int",
                        other.type_name()
                    )))
                }
            };
            Ok(Some(Value::Int(val)))
        }
        "len" => {
            let x = pos_args
                .first()
                .ok_or_else(|| StarlarkError("len() takes exactly one argument".to_string()))?;
            let len = match x {
                Value::String(s) => s.chars().count(),
                Value::List(l) => l.lock().unwrap().elements.len(),
                Value::Tuple(t) => t.len(),
                Value::Dict(d) => d.lock().unwrap().len(),
                Value::Set(s) => s.lock().unwrap().len(),
                Value::Range(r) => r.len,
                Value::Struct(s) => s.fields.len(),
                other => {
                    return Err(StarlarkError(format!(
                        "type '{}' has no len()",
                        other.type_name()
                    )))
                }
            };
            Ok(Some(Value::Int(BigInt::from_u64(len as u64))))
        }
        "list" => {
            if pos_args.is_empty() {
                return Ok(Some(Value::List(Arc::new(Mutex::new(
                    StarlarkList::new_in(Vec::new(), Arc::clone(mutability)),
                )))));
            }
            let items = value_to_sequence(&pos_args[0])?;
            Ok(Some(Value::List(Arc::new(Mutex::new(
                StarlarkList::new_in(items, Arc::clone(mutability)),
            )))))
        }
        "max" => {
            for k in kw_args.keys() {
                if k != "key" {
                    return Err(StarlarkError(format!(
                        "max() got unexpected keyword argument: {k}"
                    )));
                }
            }
            let items = if pos_args.len() == 1 {
                value_to_sequence(&pos_args[0])?
            } else if pos_args.is_empty() {
                return Err(StarlarkError("expected at least one item".to_string()));
            } else {
                pos_args.to_vec()
            };
            if items.is_empty() {
                return Err(StarlarkError("expected at least one item".to_string()));
            }
            if let Some(k) = kw_args.get("key") {
                if !matches!(k, Value::None) && !is_callable(k) {
                    return Err(StarlarkError(format!(
                        "parameter 'key' got value of type '{}', want 'callable or NoneType'",
                        k.type_name()
                    )));
                }
            }
            let mut max_item = items[0].clone();
            for item in &items[1..] {
                if compare_values(item, &max_item)? == Ordering::Greater {
                    max_item = item.clone();
                }
            }
            Ok(Some(max_item))
        }
        "min" => {
            for k in kw_args.keys() {
                if k != "key" {
                    return Err(StarlarkError(format!(
                        "min() got unexpected keyword argument: {k}"
                    )));
                }
            }
            let items = if pos_args.len() == 1 {
                value_to_sequence(&pos_args[0])?
            } else if pos_args.is_empty() {
                return Err(StarlarkError("expected at least one item".to_string()));
            } else {
                pos_args.to_vec()
            };
            if items.is_empty() {
                return Err(StarlarkError("expected at least one item".to_string()));
            }
            if let Some(k) = kw_args.get("key") {
                if !matches!(k, Value::None) && !is_callable(k) {
                    return Err(StarlarkError(format!(
                        "parameter 'key' got value of type '{}', want 'callable or NoneType'",
                        k.type_name()
                    )));
                }
            }
            let mut min_item = items[0].clone();
            for item in &items[1..] {
                if compare_values(item, &min_item)? == Ordering::Less {
                    min_item = item.clone();
                }
            }
            Ok(Some(min_item))
        }
        "range" => {
            let (start, stop, step) = match pos_args {
                [stop_val] => (0, stop_val.as_i64().unwrap_or(0), 1),
                [start_val, stop_val] => (
                    start_val.as_i64().unwrap_or(0),
                    stop_val.as_i64().unwrap_or(0),
                    1,
                ),
                [start_val, stop_val, step_val] => (
                    start_val.as_i64().unwrap_or(0),
                    stop_val.as_i64().unwrap_or(0),
                    step_val.as_i64().unwrap_or(1),
                ),
                _ => {
                    return Err(StarlarkError(
                        "range() requires 1 to 3 arguments".to_string(),
                    ))
                }
            };
            let r = RangeList::new(start, stop, step)?;
            Ok(Some(Value::Range(r)))
        }
        "repr" => {
            let s = pos_args.first().map(|v| v.repr()).unwrap_or_default();
            Ok(Some(Value::String(s)))
        }
        "reversed" => {
            let x = pos_args
                .first()
                .ok_or_else(|| StarlarkError("reversed() requires 1 argument".to_string()))?;
            let mut items = value_to_sequence(x)?;
            items.reverse();
            Ok(Some(Value::List(Arc::new(Mutex::new(
                StarlarkList::new_in(items, Arc::clone(mutability)),
            )))))
        }
        "set" => {
            if pos_args.is_empty() {
                return Ok(Some(Value::Set(Arc::new(Mutex::new(StarlarkSet::new_in(
                    Arc::clone(mutability),
                ))))));
            }
            if pos_args.len() > 1 {
                return Err(StarlarkError(
                    "set() accepts no more than 1 positional argument".to_string(),
                ));
            }
            let items = value_to_sequence(&pos_args[0])?;
            let s = StarlarkSet::from_elements_in(items, Arc::clone(mutability))?;
            Ok(Some(Value::Set(Arc::new(Mutex::new(s)))))
        }
        "sorted" => {
            for k in kw_args.keys() {
                if k != "key" && k != "reverse" {
                    return Err(StarlarkError(format!(
                        "sorted() got unexpected keyword argument: {k}"
                    )));
                }
            }
            let x = pos_args
                .first()
                .ok_or_else(|| StarlarkError("sorted() requires 1 argument".to_string()))?;
            let reverse = kw_args
                .get("reverse")
                .map(|v| v.is_truthy())
                .unwrap_or(false);
            if let Some(k) = kw_args.get("key") {
                if !matches!(k, Value::None) && !is_callable(k) {
                    return Err(StarlarkError(format!(
                        "parameter 'key' got value of type '{}', want 'callable or NoneType'",
                        k.type_name()
                    )));
                }
            }
            let mut items = value_to_sequence(x)?;
            fallible_merge_sort(&mut items, &mut |a, b| {
                let ord = compare_values(a, b)?;
                Ok(if reverse { ord.reverse() } else { ord })
            })?;
            Ok(Some(Value::List(Arc::new(Mutex::new(
                StarlarkList::new_in(items, Arc::clone(mutability)),
            )))))
        }
        "str" => {
            let s = match pos_args.first() {
                Some(Value::Label(l)) => l.unambiguous_canonical_form(),
                Some(v) => v.to_string(),
                None => String::new(),
            };
            Ok(Some(Value::String(s)))
        }
        "tuple" => {
            if pos_args.is_empty() {
                return Ok(Some(Value::Tuple(Vec::new())));
            }
            let items = value_to_sequence(&pos_args[0])?;
            Ok(Some(Value::Tuple(items)))
        }
        "type" => {
            let x = pos_args
                .first()
                .ok_or_else(|| StarlarkError("type() requires 1 argument".to_string()))?;
            Ok(Some(Value::String(x.type_name().to_string())))
        }
        "zip" => {
            if pos_args.is_empty() {
                return Ok(Some(Value::List(Arc::new(Mutex::new(
                    StarlarkList::new_in(Vec::new(), Arc::clone(mutability)),
                )))));
            }
            let mut seqs = Vec::new();
            for arg in pos_args {
                seqs.push(value_to_sequence(arg)?);
            }
            let min_len = seqs.iter().map(|s| s.len()).min().unwrap_or(0);
            let mut out = Vec::with_capacity(min_len);
            for i in 0..min_len {
                let tuple_items: Vec<Value> = seqs.iter().map(|s| s[i].clone()).collect();
                out.push(Value::Tuple(tuple_items));
            }
            Ok(Some(Value::List(Arc::new(Mutex::new(
                StarlarkList::new_in(out, Arc::clone(mutability)),
            )))))
        }
        _ => Ok(None),
    }
}

pub fn value_to_sequence(val: &Value) -> Result<Vec<Value>, StarlarkError> {
    match val {
        Value::List(l) => Ok(l.lock().unwrap().elements.clone()),
        Value::Tuple(t) => Ok(t.clone()),
        Value::Set(s) => Ok(s.lock().unwrap().elements.clone()),
        Value::Range(r) => Ok(r
            .to_vec()
            .into_iter()
            .map(|i| Value::Int(BigInt::from_i64(i)))
            .collect()),
        Value::Dict(d) => Ok(d.lock().unwrap().keys()),
        Value::String(s) => Ok(s.chars().map(|c| Value::String(c.to_string())).collect()),
        other => Err(StarlarkError(format!(
            "got value of type '{}', want 'iterable'",
            other.type_name()
        ))),
    }
}

pub fn unpack_pair(item: &Value) -> Result<(Value, Value), StarlarkError> {
    match item {
        Value::List(l) => {
            let guard = l.lock().unwrap();
            if guard.elements.len() == 2 {
                Ok((guard.elements[0].clone(), guard.elements[1].clone()))
            } else {
                Err(StarlarkError(
                    "dictionary item must be a 2-element sequence".to_string(),
                ))
            }
        }
        Value::Tuple(t) => {
            if t.len() == 2 {
                Ok((t[0].clone(), t[1].clone()))
            } else {
                Err(StarlarkError(
                    "dictionary item must be a 2-element sequence".to_string(),
                ))
            }
        }
        _ => Err(StarlarkError(
            "dictionary item must be a 2-element sequence".to_string(),
        )),
    }
}

pub fn compare_values(a: &Value, b: &Value) -> Result<Ordering, StarlarkError> {
    match (a, b) {
        (Value::Int(i1), Value::Int(i2)) => Ok(i1.cmp(i2)),
        (Value::Float(f1), Value::Float(f2)) => {
            if f1.is_nan() && f2.is_nan() {
                Ok(Ordering::Equal)
            } else if f1.is_nan() {
                Ok(Ordering::Greater)
            } else if f2.is_nan() {
                Ok(Ordering::Less)
            } else {
                Ok(f1.partial_cmp(f2).unwrap_or(Ordering::Equal))
            }
        }
        (Value::Int(i), Value::Float(f)) => Ok(crate::bigint::compare_int_and_float(i, *f)),
        (Value::Float(f), Value::Int(i)) => {
            Ok(crate::bigint::compare_int_and_float(i, *f).reverse())
        }
        (Value::String(s1), Value::String(s2)) => Ok(s1.encode_utf16().cmp(s2.encode_utf16())),
        (Value::Bool(b1), Value::Bool(b2)) => Ok(b1.cmp(b2)),
        // Apple `DottedVersion` host objects are comparable.
        (Value::Host(k1, v1), Value::Host(k2, v2))
            if k1 == "dotted_version" && k2 == "dotted_version" =>
        {
            Ok(dotted_version_cmp(v1, v2))
        }
        (Value::List(l1), Value::List(l2)) => {
            if Arc::ptr_eq(l1, l2) {
                return Ok(Ordering::Equal);
            }
            let e1 = l1.lock().unwrap().elements.clone();
            let e2 = l2.lock().unwrap().elements.clone();
            for (x, y) in e1.iter().zip(e2.iter()) {
                if x == y {
                    continue;
                }
                let ord = compare_values(x, y)?;
                if ord != Ordering::Equal {
                    return Ok(ord);
                }
            }
            Ok(e1.len().cmp(&e2.len()))
        }
        (Value::Tuple(t1), Value::Tuple(t2)) => {
            for (x, y) in t1.iter().zip(t2.iter()) {
                if x == y {
                    continue;
                }
                let ord = compare_values(x, y)?;
                if ord != Ordering::Equal {
                    return Ok(ord);
                }
            }
            Ok(t1.len().cmp(&t2.len()))
        }
        _ => Err(StarlarkError(format!(
            "unsupported comparison: {} <=> {}",
            a.type_name(),
            b.type_name()
        ))),
    }
}

/// Component-wise numeric comparison of dotted versions (`DottedVersion.compareTo`): missing
/// components are 0; an alpha suffix (`beta`) sorts before the bare number.
pub fn dotted_version_cmp(a: &str, b: &str) -> Ordering {
    let comps = |v: &str| -> Vec<(u64, String)> {
        v.split('.')
            .map(|c| {
                let digits: String = c.chars().take_while(|ch| ch.is_ascii_digit()).collect();
                let rest: String = c.chars().skip(digits.len()).collect();
                (digits.parse().unwrap_or(0), rest)
            })
            .collect()
    };
    let (x, y) = (comps(a), comps(b));
    for i in 0..x.len().max(y.len()) {
        let ca = x.get(i).cloned().unwrap_or((0, String::new()));
        let cb = y.get(i).cloned().unwrap_or((0, String::new()));
        let o = ca.0.cmp(&cb.0);
        if o != Ordering::Equal {
            return o;
        }
        let o = match (ca.1.is_empty(), cb.1.is_empty()) {
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            _ => ca.1.cmp(&cb.1),
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    Ordering::Equal
}

pub fn is_callable(v: &Value) -> bool {
    matches!(
        v,
        Value::UserFunction(_)
            | Value::BuiltinFunction(_)
            | Value::Provider { .. }
            | Value::RuleCallable { .. }
    ) || matches!(
        v,
        Value::Host(k, _)
            if matches!(
                k.as_str(),
                "provider" | "provider_raw" | "rule" | "repository_rule" | "module_extension" | "aspect" | "tag_class"
            )
    )
}

/// Stable merge sort over `(key, item)` pairs that propagates comparison errors cleanly.
pub fn merge_sort_keyed(
    mut items: Vec<(Value, Value)>,
    reverse: bool,
) -> Result<Vec<(Value, Value)>, StarlarkError> {
    let n = items.len();
    if n <= 1 {
        return Ok(items);
    }
    let mid = n / 2;
    let right = merge_sort_keyed(items.split_off(mid), reverse)?;
    let left = merge_sort_keyed(items, reverse)?;
    let mut out = Vec::with_capacity(n);
    let mut li = 0;
    let mut ri = 0;
    while li < left.len() && ri < right.len() {
        let ord = compare_values(&left[li].0, &right[ri].0)?;
        let take_left = if reverse {
            ord != Ordering::Less
        } else {
            ord != Ordering::Greater
        };
        if take_left {
            out.push(left[li].clone());
            li += 1;
        } else {
            out.push(right[ri].clone());
            ri += 1;
        }
    }
    out.extend_from_slice(&left[li..]);
    out.extend_from_slice(&right[ri..]);
    Ok(out)
}
