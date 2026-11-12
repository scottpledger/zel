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
use crate::types::dict::StarlarkDict;
use crate::types::list::{StarlarkError, StarlarkList};
use crate::types::range::RangeList;
use crate::types::set::StarlarkSet;
use crate::types::value::Value;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

pub fn eval_universal_function(
    name: &str,
    pos_args: &[Value],
    kw_args: &BTreeMap<String, Value>,
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
            let mut dict = StarlarkDict::new();
            if let Some(first) = pos_args.first() {
                match first {
                    Value::Dict(d) => {
                        let guard = d.lock().unwrap();
                        for (k, v) in &guard.entries {
                            let _ = dict.insert(k.clone(), v.clone());
                        }
                    }
                    Value::List(l) => {
                        let guard = l.lock().unwrap();
                        for item in &guard.elements {
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
            let mut attrs = Vec::new();
            match x {
                Value::Struct(s) => {
                    attrs.extend(s.fields.keys().cloned());
                }
                Value::Dict(_) => {
                    attrs.extend(vec![
                        "clear".to_string(),
                        "get".to_string(),
                        "items".to_string(),
                        "keys".to_string(),
                        "pop".to_string(),
                        "popitem".to_string(),
                        "setdefault".to_string(),
                        "update".to_string(),
                        "values".to_string(),
                    ]);
                }
                Value::List(_) => {
                    attrs.extend(vec![
                        "append".to_string(),
                        "clear".to_string(),
                        "extend".to_string(),
                        "index".to_string(),
                        "insert".to_string(),
                        "pop".to_string(),
                        "remove".to_string(),
                    ]);
                }
                Value::Set(_) => {
                    attrs.extend(vec![
                        "add".to_string(),
                        "clear".to_string(),
                        "difference".to_string(),
                        "difference_update".to_string(),
                        "discard".to_string(),
                        "intersection".to_string(),
                        "intersection_update".to_string(),
                        "isdisjoint".to_string(),
                        "issubset".to_string(),
                        "issuperset".to_string(),
                        "pop".to_string(),
                        "remove".to_string(),
                        "symmetric_difference".to_string(),
                        "symmetric_difference_update".to_string(),
                        "union".to_string(),
                        "update".to_string(),
                    ]);
                }
                Value::String(_) => {
                    attrs.extend(vec![
                        "capitalize".to_string(),
                        "count".to_string(),
                        "elems".to_string(),
                        "endswith".to_string(),
                        "find".to_string(),
                        "format".to_string(),
                        "index".to_string(),
                        "isalnum".to_string(),
                        "isalpha".to_string(),
                        "isdigit".to_string(),
                        "islower".to_string(),
                        "isspace".to_string(),
                        "istitle".to_string(),
                        "isupper".to_string(),
                        "join".to_string(),
                        "lower".to_string(),
                        "lstrip".to_string(),
                        "partition".to_string(),
                        "removeprefix".to_string(),
                        "removesuffix".to_string(),
                        "replace".to_string(),
                        "rfind".to_string(),
                        "rindex".to_string(),
                        "rpartition".to_string(),
                        "rsplit".to_string(),
                        "rstrip".to_string(),
                        "split".to_string(),
                        "splitlines".to_string(),
                        "startswith".to_string(),
                        "strip".to_string(),
                        "title".to_string(),
                        "upper".to_string(),
                    ]);
                }
                Value::Host(kind, _) => match kind.as_str() {
                    "target" => {
                        attrs.extend(vec![
                            "actions".to_string(),
                            "data_runfiles".to_string(),
                            "default_runfiles".to_string(),
                            "files".to_string(),
                            "files_to_run".to_string(),
                            "label".to_string(),
                            "output_groups".to_string(),
                        ]);
                    }
                    "repository_ctx" => {
                        attrs.extend(vec![
                            "attr".to_string(),
                            "delete".to_string(),
                            "download".to_string(),
                            "download_and_extract".to_string(),
                            "execute".to_string(),
                            "extract".to_string(),
                            "file".to_string(),
                            "getenv".to_string(),
                            "name".to_string(),
                            "original_name".to_string(),
                            "os".to_string(),
                            "patch".to_string(),
                            "path".to_string(),
                            "read".to_string(),
                            "rename".to_string(),
                            "repo_metadata".to_string(),
                            "report_progress".to_string(),
                            "symlink".to_string(),
                            "template".to_string(),
                            "watch".to_string(),
                            "watch_tree".to_string(),
                            "which".to_string(),
                            "workspace_root".to_string(),
                        ]);
                    }
                    "module_ctx" => {
                        attrs.extend(vec![
                            "download".to_string(),
                            "download_and_extract".to_string(),
                            "execute".to_string(),
                            "extension_metadata".to_string(),
                            "extract".to_string(),
                            "facts".to_string(),
                            "file".to_string(),
                            "getenv".to_string(),
                            "is_dev_dependency".to_string(),
                            "modules".to_string(),
                            "os".to_string(),
                            "path".to_string(),
                            "read".to_string(),
                            "report_progress".to_string(),
                            "root_module_has_non_dev_dependency".to_string(),
                            "tag_sort_key".to_string(),
                            "watch".to_string(),
                            "which".to_string(),
                        ]);
                    }
                    "path" => {
                        attrs.extend(vec![
                            "basename".to_string(),
                            "dirname".to_string(),
                            "exists".to_string(),
                            "get_child".to_string(),
                            "is_dir".to_string(),
                            "readdir".to_string(),
                            "realpath".to_string(),
                        ]);
                    }
                    "exec_result" => {
                        attrs.extend(vec![
                            "return_code".to_string(),
                            "stderr".to_string(),
                            "stdout".to_string(),
                        ]);
                    }
                    "pending_download" => {
                        attrs.extend(vec!["wait".to_string()]);
                    }
                    _ => {}
                },
                _ => {}
            }
            attrs.sort();
            Ok(Some(Value::List(Arc::new(Mutex::new(StarlarkList::new(
                attrs.into_iter().map(Value::String).collect(),
            ))))))
        }
        "enumerate" => {
            let x = pos_args
                .first()
                .ok_or_else(|| StarlarkError("enumerate() takes at least 1 argument".to_string()))?;
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
            Ok(Some(Value::List(Arc::new(Mutex::new(StarlarkList::new(pairs))))))
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
            let x = pos_args
                .first()
                .ok_or_else(|| StarlarkError("getattr() requires at least 2 arguments".to_string()))?;
            let name = pos_args
                .get(1)
                .and_then(|v| v.as_str())
                .ok_or_else(|| StarlarkError("getattr() attribute name must be string".to_string()))?;
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
                Value::Dict(d) => {
                    let guard = d.lock().unwrap();
                    if let Some(val) = guard.get(&Value::String(name.to_string())) {
                        Ok(Some(val.clone()))
                    } else if let Some(def) = default {
                        Ok(Some(def))
                    } else {
                        Err(StarlarkError(format!("attribute {name:?} not found")))
                    }
                }
                Value::ProviderInstance { fields, .. } => match fields.get(name) {
                    Some(v) => Ok(Some(v.clone())),
                    None => match default {
                        Some(d) => Ok(Some(d)),
                        None => Err(StarlarkError(format!("field '{name}' not found"))),
                    },
                },
                _ => {
                    if let Some(def) = default {
                        Ok(Some(def))
                    } else {
                        Err(StarlarkError(format!(
                            "type '{}' has no attribute '{name}'",
                            x.type_name()
                        )))
                    }
                }
            }
        }
        "hasattr" => {
            let x = pos_args
                .first()
                .ok_or_else(|| StarlarkError("hasattr() requires at least 2 arguments".to_string()))?;
            let name = pos_args
                .get(1)
                .and_then(|v| v.as_str())
                .ok_or_else(|| StarlarkError("hasattr() attribute name must be string".to_string()))?;

            let res = match x {
                Value::Struct(s) => s.fields.contains_key(name),
                Value::Dict(_) => [
                    "clear", "get", "items", "keys", "pop", "popitem", "setdefault", "update", "values",
                ]
                .contains(&name),
                Value::ProviderInstance { fields, .. } => fields.contains_key(name),
                Value::Depset(_) => name == "to_list",
                Value::Label(_) => ["relative", "package_relative", "same_package_label"].contains(&name),
                Value::List(_) => ["append", "clear", "extend", "index", "insert", "pop", "remove"].contains(&name),
                Value::Tuple(_) => false,
                Value::String(_) => [
                    "capitalize", "count", "elems", "endswith", "find", "format", "index", "isalnum",
                    "isalpha", "isdigit", "islower", "isspace", "istitle", "isupper", "join", "lower",
                    "lstrip", "partition", "removeprefix", "removesuffix", "replace", "rfind", "rindex",
                    "rpartition", "rsplit", "rstrip", "split", "splitlines", "startswith", "strip",
                    "title", "upper",
                ]
                .contains(&name),
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
                None => return Err(StarlarkError("hash() missing 1 required positional argument: value".to_string())),
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
                    if f.is_nan() || f.is_infinite() {
                        return Err(StarlarkError("cannot convert float NaN or Inf to int".to_string()));
                    }
                    BigInt::from_i64(*f as i64)
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
                return Ok(Some(Value::List(Arc::new(Mutex::new(StarlarkList::new(
                    Vec::new(),
                ))))));
            }
            let items = value_to_sequence(&pos_args[0])?;
            Ok(Some(Value::List(Arc::new(Mutex::new(StarlarkList::new(
                items,
            ))))))
        }
        "max" => {
            let items = if pos_args.len() == 1 {
                value_to_sequence(&pos_args[0])?
            } else if pos_args.is_empty() {
                return Err(StarlarkError("max() expected at least 1 argument, got 0".to_string()));
            } else {
                pos_args.to_vec()
            };
            if items.is_empty() {
                return Err(StarlarkError("max() arg is an empty sequence".to_string()));
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
            let items = if pos_args.len() == 1 {
                value_to_sequence(&pos_args[0])?
            } else if pos_args.is_empty() {
                return Err(StarlarkError("min() expected at least 1 argument, got 0".to_string()));
            } else {
                pos_args.to_vec()
            };
            if items.is_empty() {
                return Err(StarlarkError("min() arg is an empty sequence".to_string()));
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
                _ => return Err(StarlarkError("range() requires 1 to 3 arguments".to_string())),
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
            Ok(Some(Value::List(Arc::new(Mutex::new(StarlarkList::new(items))))))
        }
        "set" => {
            if pos_args.is_empty() {
                return Ok(Some(Value::Set(Arc::new(Mutex::new(StarlarkSet::new())))));
            }
            if pos_args.len() > 1 {
                return Err(StarlarkError(
                    "set() accepts no more than 1 positional argument".to_string(),
                ));
            }
            let items = value_to_sequence(&pos_args[0])?;
            let s = StarlarkSet::from_elements(items)?;
            Ok(Some(Value::Set(Arc::new(Mutex::new(s)))))
        }
        "sorted" => {
            let x = pos_args
                .first()
                .ok_or_else(|| StarlarkError("sorted() requires 1 argument".to_string()))?;
            let reverse = kw_args
                .get("reverse")
                .map(|v| v.is_truthy())
                .unwrap_or(false);
            let mut items = value_to_sequence(x)?;
            // Stable sort
            items.sort_by(|a, b| {
                let ord = compare_values(a, b).unwrap_or(Ordering::Equal);
                if reverse {
                    ord.reverse()
                } else {
                    ord
                }
            });
            Ok(Some(Value::List(Arc::new(Mutex::new(StarlarkList::new(items))))))
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
                return Ok(Some(Value::List(Arc::new(Mutex::new(StarlarkList::new(
                    Vec::new(),
                ))))));
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
            Ok(Some(Value::List(Arc::new(Mutex::new(StarlarkList::new(out))))))
        }
        _ => Ok(None),
    }
}

pub fn value_to_sequence(val: &Value) -> Result<Vec<Value>, StarlarkError> {
    match val {
        Value::List(l) => Ok(l.lock().unwrap().elements.clone()),
        Value::Tuple(t) => Ok(t.clone()),
        Value::Set(s) => Ok(s.lock().unwrap().elements.clone()),
        Value::Range(r) => Ok(r.to_vec().into_iter().map(|i| Value::Int(BigInt::from_i64(i))).collect()),
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
                Err(StarlarkError("dictionary item must be a 2-element sequence".to_string()))
            }
        }
        Value::Tuple(t) => {
            if t.len() == 2 {
                Ok((t[0].clone(), t[1].clone()))
            } else {
                Err(StarlarkError("dictionary item must be a 2-element sequence".to_string()))
            }
        }
        _ => Err(StarlarkError("dictionary item must be a 2-element sequence".to_string())),
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
        (Value::Int(i), Value::Float(f)) => {
            if f.is_nan() {
                Ok(Ordering::Less)
            } else {
                let f_int = BigInt::from_i64(*f as i64);
                Ok(i.cmp(&f_int))
            }
        }
        (Value::Float(f), Value::Int(i)) => {
            if f.is_nan() {
                Ok(Ordering::Greater)
            } else {
                let f_int = BigInt::from_i64(*f as i64);
                Ok(f_int.cmp(i))
            }
        }
        (Value::String(s1), Value::String(s2)) => Ok(s1.cmp(s2)),
        (Value::Bool(b1), Value::Bool(b2)) => Ok(b1.cmp(b2)),
        // Apple `DottedVersion` host objects are comparable.
        (Value::Host(k1, v1), Value::Host(k2, v2)) if k1 == "dotted_version" && k2 == "dotted_version" => {
            Ok(dotted_version_cmp(v1, v2))
        }
        (Value::List(l1), Value::List(l2)) => {
            let g1 = l1.lock().unwrap();
            let g2 = l2.lock().unwrap();
            for (x, y) in g1.elements.iter().zip(g2.elements.iter()) {
                let ord = compare_values(x, y)?;
                if ord != Ordering::Equal {
                    return Ok(ord);
                }
            }
            Ok(g1.elements.len().cmp(&g2.elements.len()))
        }
        (Value::Tuple(t1), Value::Tuple(t2)) => {
            for (x, y) in t1.iter().zip(t2.iter()) {
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
