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

use crate::eval::builtins::universal::{to_int32, value_to_sequence};
use crate::eval::format::str_format;
use crate::types::list::{StarlarkError, StarlarkList};
use crate::types::mutability::Mutability;
use crate::types::value::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

pub fn eval_string_method(
    s: &str,
    method: &str,
    pos_args: &[Value],
    kw_args: &BTreeMap<String, Value>,
) -> Result<Value, StarlarkError> {
    eval_string_method_in(s, method, pos_args, kw_args, &Mutability::create())
}

pub fn eval_string_method_in(
    s: &str,
    method: &str,
    pos_args: &[Value],
    kw_args: &BTreeMap<String, Value>,
    mutability: &Arc<Mutability>,
) -> Result<Value, StarlarkError> {
    match method {
        "capitalize" => {
            let mut chars = s.chars();
            let res = match chars.next() {
                None => String::new(),
                Some(first) => {
                    let mut res = first.to_uppercase().to_string();
                    res.push_str(&chars.as_str().to_lowercase());
                    res
                }
            };
            Ok(Value::String(res))
        }
        "count" => {
            let sub = pos_args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| StarlarkError("count() requires sub string".to_string()))?;
            let slice = get_slice_utf16(s, pos_args.get(1), pos_args.get(2))?;
            if sub.is_empty() {
                Ok(Value::Int(crate::bigint::BigInt::from_u64(
                    (slice.len() + 1) as u64,
                )))
            } else {
                let sub_u16: Vec<u16> = sub.encode_utf16().collect();
                let mut cnt = 0usize;
                let mut i = 0usize;
                while i + sub_u16.len() <= slice.len() {
                    if slice[i..i + sub_u16.len()] == sub_u16[..] {
                        cnt += 1;
                        i += sub_u16.len();
                    } else {
                        i += 1;
                    }
                }
                Ok(Value::Int(crate::bigint::BigInt::from_u64(cnt as u64)))
            }
        }
        "elems" => {
            let elems: Vec<Value> = s
                .encode_utf16()
                .map(|u| Value::String(String::from_utf16_lossy(&[u])))
                .collect();
            Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new_in(
                elems,
                Arc::clone(mutability),
            )))))
        }
        "endswith" => {
            let (start, end) = (pos_args.get(1), pos_args.get(2));
            let slice = get_slice_utf16(s, start, end)?;
            match pos_args.first() {
                Some(Value::String(suffix)) => {
                    let suf_u16: Vec<u16> = suffix.encode_utf16().collect();
                    Ok(Value::Bool(slice.ends_with(&suf_u16)))
                }
                Some(Value::Tuple(suffixes)) => {
                    for (i, suf) in suffixes.iter().enumerate() {
                        if !matches!(suf, Value::String(_)) {
                            return Err(StarlarkError(format!(
                                "at index {i} of sub, got element of type {}, want string",
                                suf.type_name()
                            )));
                        }
                    }
                    for suf in suffixes {
                        let suf_u16: Vec<u16> = suf.as_str().unwrap().encode_utf16().collect();
                        if slice.ends_with(&suf_u16) {
                            return Ok(Value::Bool(true));
                        }
                    }
                    Ok(Value::Bool(false))
                }
                _ => Err(StarlarkError(
                    "endswith() suffix must be str or tuple of str".to_string(),
                )),
            }
        }
        "find" => {
            let sub = pos_args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| StarlarkError("find() requires sub string".to_string()))?;
            let (start_idx, _) = get_offsets_utf16(s, pos_args.get(1), pos_args.get(2))?;
            let slice = get_slice_utf16(s, pos_args.get(1), pos_args.get(2))?;
            if let Some(pos) = find_sub_utf16(&slice, &sub.encode_utf16().collect::<Vec<_>>()) {
                Ok(Value::Int(crate::bigint::BigInt::from_i64(
                    (start_idx + pos) as i64,
                )))
            } else {
                Ok(Value::Int(crate::bigint::BigInt::from_i64(-1)))
            }
        }
        "format" => Ok(Value::String(str_format(s, pos_args, kw_args)?)),
        "index" => {
            let sub = pos_args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| StarlarkError("index() requires sub string".to_string()))?;
            let (start_idx, _) = get_offsets_utf16(s, pos_args.get(1), pos_args.get(2))?;
            let slice = get_slice_utf16(s, pos_args.get(1), pos_args.get(2))?;
            if let Some(pos) = find_sub_utf16(&slice, &sub.encode_utf16().collect::<Vec<_>>()) {
                Ok(Value::Int(crate::bigint::BigInt::from_i64(
                    (start_idx + pos) as i64,
                )))
            } else {
                Err(StarlarkError("substring not found".to_string()))
            }
        }
        "isalnum" => {
            let b = !s.is_empty() && s.chars().all(|c| c.is_alphanumeric());
            Ok(Value::Bool(b))
        }
        "isalpha" => {
            let b = !s.is_empty() && s.chars().all(|c| c.is_alphabetic());
            Ok(Value::Bool(b))
        }
        "isdigit" => {
            let b = !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
            Ok(Value::Bool(b))
        }
        "islower" => {
            let has_cased = s.chars().any(|c| c.is_alphabetic());
            let all_lower = s.chars().all(|c| !c.is_alphabetic() || c.is_lowercase());
            Ok(Value::Bool(has_cased && all_lower))
        }
        "isspace" => {
            let b = !s.is_empty() && s.chars().all(|c| c.is_whitespace());
            Ok(Value::Bool(b))
        }
        "istitle" => {
            let mut is_title = false;
            let mut prev_cased = false;
            for c in s.chars() {
                if c.is_alphabetic() {
                    if !prev_cased {
                        if !c.is_uppercase() {
                            return Ok(Value::Bool(false));
                        }
                        is_title = true;
                    } else if !c.is_lowercase() {
                        return Ok(Value::Bool(false));
                    }
                    prev_cased = true;
                } else {
                    prev_cased = false;
                }
            }
            Ok(Value::Bool(is_title))
        }
        "isupper" => {
            let has_cased = s.chars().any(|c| c.is_alphabetic());
            let all_upper = s.chars().all(|c| !c.is_alphabetic() || c.is_uppercase());
            Ok(Value::Bool(has_cased && all_upper))
        }
        "join" => {
            let Some(arg) = pos_args.first() else {
                return Err(StarlarkError(
                    "join() missing 1 required positional argument: elements".to_string(),
                ));
            };
            if matches!(arg, Value::String(_)) {
                return Err(StarlarkError(
                    "got value of type 'string', want 'iterable'".to_string(),
                ));
            }
            let items = value_to_sequence(arg)?;
            let mut parts = Vec::with_capacity(items.len());
            for (i, item) in items.into_iter().enumerate() {
                match item {
                    Value::String(st) => parts.push(st),
                    other => {
                        return Err(StarlarkError(format!(
                            "expected string for sequence element {i}, got '{}' of type {}",
                            other,
                            other.type_name()
                        )));
                    }
                }
            }
            Ok(Value::String(parts.join(s)))
        }
        "lower" => Ok(Value::String(s.to_lowercase())),
        "lstrip" => {
            let chars_to_strip = pos_args.first().and_then(|v| v.as_str());
            let res = match chars_to_strip {
                Some(chars) => s.trim_start_matches(|c| chars.contains(c)).to_string(),
                None => s.trim_start().to_string(),
            };
            Ok(Value::String(res))
        }
        "partition" => {
            if pos_args.is_empty() {
                return Err(StarlarkError(
                    "missing 1 required positional argument".to_string(),
                ));
            }
            let sep = match pos_args.first() {
                Some(Value::String(sep)) => sep.as_str(),
                Some(other) => {
                    return Err(StarlarkError(format!(
                        "got value of type '{}', want 'string'",
                        other.type_name()
                    )))
                }
                None => {
                    return Err(StarlarkError(
                        "missing 1 required positional argument".to_string(),
                    ))
                }
            };
            if sep.is_empty() {
                return Err(StarlarkError("empty separator".to_string()));
            }
            let (before, matched, after) = match s.find(sep) {
                Some(idx) => (
                    s[..idx].to_string(),
                    sep.to_string(),
                    s[idx + sep.len()..].to_string(),
                ),
                None => (s.to_string(), String::new(), String::new()),
            };
            Ok(Value::Tuple(vec![
                Value::String(before),
                Value::String(matched),
                Value::String(after),
            ]))
        }
        "removeprefix" => {
            if pos_args.len() > 1 {
                return Err(StarlarkError(format!(
                    "removeprefix() accepts no more than 1 positional argument but got {}",
                    pos_args.len()
                )));
            }
            for k in kw_args.keys() {
                if k != "prefix" {
                    return Err(StarlarkError(format!(
                        "removeprefix() got unexpected keyword argument '{k}'"
                    )));
                }
            }
            if !pos_args.is_empty() && kw_args.contains_key("prefix") {
                return Err(StarlarkError(
                    "removeprefix() got multiple values for parameter 'prefix'".to_string(),
                ));
            }
            let p_val = pos_args
                .first()
                .or_else(|| kw_args.get("prefix"))
                .ok_or_else(|| {
                    StarlarkError(
                        "removeprefix() missing 1 required positional argument: prefix".to_string(),
                    )
                })?;
            let Some(prefix) = p_val.as_str() else {
                return Err(StarlarkError(format!(
                    "in call to removeprefix(), parameter 'prefix' got value of type '{}', want 'string'",
                    p_val.type_name()
                )));
            };
            Ok(Value::String(
                s.strip_prefix(prefix).unwrap_or(s).to_string(),
            ))
        }
        "removesuffix" => {
            if pos_args.len() > 1 {
                return Err(StarlarkError(format!(
                    "removesuffix() accepts no more than 1 positional argument but got {}",
                    pos_args.len()
                )));
            }
            for k in kw_args.keys() {
                if k != "suffix" {
                    return Err(StarlarkError(format!(
                        "removesuffix() got unexpected keyword argument '{k}'"
                    )));
                }
            }
            if !pos_args.is_empty() && kw_args.contains_key("suffix") {
                return Err(StarlarkError(
                    "removesuffix() got multiple values for parameter 'suffix'".to_string(),
                ));
            }
            let s_val = pos_args
                .first()
                .or_else(|| kw_args.get("suffix"))
                .ok_or_else(|| {
                    StarlarkError(
                        "removesuffix() missing 1 required positional argument: suffix".to_string(),
                    )
                })?;
            let Some(suffix) = s_val.as_str() else {
                return Err(StarlarkError(format!(
                    "in call to removesuffix(), parameter 'suffix' got value of type '{}', want 'string'",
                    s_val.type_name()
                )));
            };
            Ok(Value::String(
                s.strip_suffix(suffix).unwrap_or(s).to_string(),
            ))
        }
        "replace" => {
            if !kw_args.is_empty() {
                let k = kw_args.keys().next().unwrap();
                return Err(StarlarkError(format!(
                    "replace() got unexpected keyword argument '{k}'"
                )));
            }
            if pos_args.len() < 2 {
                return Err(StarlarkError(
                    "replace() missing required positional arguments: old, new".to_string(),
                ));
            }
            if pos_args.len() > 3 {
                return Err(StarlarkError(format!(
                    "replace() accepts no more than 3 positional arguments but got {}",
                    pos_args.len()
                )));
            }
            let Some(old) = pos_args[0].as_str() else {
                return Err(StarlarkError(format!(
                    "in call to replace(), parameter 'old' got value of type '{}', want 'string'",
                    pos_args[0].type_name()
                )));
            };
            let Some(new) = pos_args[1].as_str() else {
                return Err(StarlarkError(format!(
                    "in call to replace(), parameter 'new' got value of type '{}', want 'string'",
                    pos_args[1].type_name()
                )));
            };
            let count = match pos_args.get(2) {
                None | Some(Value::None) => -1i32,
                Some(v) => to_int32(v, "count")?,
            };

            if count == 0 {
                return Ok(Value::String(s.to_string()));
            }
            if count < 0 {
                Ok(Value::String(s.replace(old, new)))
            } else {
                Ok(Value::String(s.replacen(old, new, count as usize)))
            }
        }
        "rfind" => {
            let sub = pos_args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| StarlarkError("rfind() requires sub string".to_string()))?;
            let (start_idx, _) = get_offsets_utf16(s, pos_args.get(1), pos_args.get(2))?;
            let slice = get_slice_utf16(s, pos_args.get(1), pos_args.get(2))?;
            if let Some(pos) = rfind_sub_utf16(&slice, &sub.encode_utf16().collect::<Vec<_>>()) {
                Ok(Value::Int(crate::bigint::BigInt::from_i64(
                    (start_idx + pos) as i64,
                )))
            } else {
                Ok(Value::Int(crate::bigint::BigInt::from_i64(-1)))
            }
        }
        "rindex" => {
            let sub = pos_args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| StarlarkError("rindex() requires sub string".to_string()))?;
            let (start_idx, _) = get_offsets_utf16(s, pos_args.get(1), pos_args.get(2))?;
            let slice = get_slice_utf16(s, pos_args.get(1), pos_args.get(2))?;
            if let Some(pos) = rfind_sub_utf16(&slice, &sub.encode_utf16().collect::<Vec<_>>()) {
                Ok(Value::Int(crate::bigint::BigInt::from_i64(
                    (start_idx + pos) as i64,
                )))
            } else {
                Err(StarlarkError("substring not found".to_string()))
            }
        }
        "rpartition" => {
            if pos_args.is_empty() {
                return Err(StarlarkError(
                    "missing 1 required positional argument".to_string(),
                ));
            }
            let sep = match pos_args.first() {
                Some(Value::String(sep)) => sep.as_str(),
                Some(other) => {
                    return Err(StarlarkError(format!(
                        "got value of type '{}', want 'string'",
                        other.type_name()
                    )))
                }
                None => {
                    return Err(StarlarkError(
                        "missing 1 required positional argument".to_string(),
                    ))
                }
            };
            if sep.is_empty() {
                return Err(StarlarkError("empty separator".to_string()));
            }
            let (before, matched, after) = match s.rfind(sep) {
                Some(idx) => (
                    s[..idx].to_string(),
                    sep.to_string(),
                    s[idx + sep.len()..].to_string(),
                ),
                None => (String::new(), String::new(), s.to_string()),
            };
            Ok(Value::Tuple(vec![
                Value::String(before),
                Value::String(matched),
                Value::String(after),
            ]))
        }
        "rsplit" => {
            for k in kw_args.keys() {
                if k != "sep" && k != "maxsplit" {
                    return Err(StarlarkError(format!(
                        "rsplit() got unexpected keyword argument: {k}"
                    )));
                }
            }
            if !pos_args.is_empty() && kw_args.contains_key("sep") {
                return Err(StarlarkError(
                    "rsplit() got multiple values for parameter 'sep'".to_string(),
                ));
            }
            if pos_args.len() > 1 && kw_args.contains_key("maxsplit") {
                return Err(StarlarkError(
                    "rsplit() got multiple values for parameter 'maxsplit'".to_string(),
                ));
            }
            let sep_val = pos_args
                .first()
                .or_else(|| kw_args.get("sep"))
                .ok_or_else(|| {
                    StarlarkError(
                        "rsplit() missing 1 required positional argument: sep".to_string(),
                    )
                })?;
            let sep = match sep_val {
                Value::String(sep) => sep.as_str(),
                other => {
                    return Err(StarlarkError(format!(
                    "in call to rsplit(), parameter 'sep' got value of type '{}', want 'string'",
                    other.type_name()
                )))
                }
            };
            if sep.is_empty() {
                return Err(StarlarkError("Empty separator".to_string()));
            }
            let maxsplit_val = pos_args.get(1).or_else(|| kw_args.get("maxsplit"));
            let maxsplit = match maxsplit_val {
                None => -1i64,
                Some(v) => to_int32(v, "maxsplit")? as i64,
            };

            let mut parts: Vec<String> = Vec::new();
            let mut end = s.len();
            let mut rem = maxsplit;
            loop {
                if rem == 0 {
                    parts.push(s[..end].to_string());
                    break;
                }
                if let Some(start) = s[..end].rfind(sep) {
                    parts.push(s[start + sep.len()..end].to_string());
                    end = start;
                    rem -= 1;
                } else {
                    parts.push(s[..end].to_string());
                    break;
                }
            }
            parts.reverse();
            Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new_in(
                parts.into_iter().map(Value::String).collect(),
                Arc::clone(mutability),
            )))))
        }
        "rstrip" => {
            let chars_to_strip = pos_args.first().and_then(|v| v.as_str());
            let res = match chars_to_strip {
                Some(chars) => s.trim_end_matches(|c| chars.contains(c)).to_string(),
                None => s.trim_end().to_string(),
            };
            Ok(Value::String(res))
        }
        "split" => {
            for k in kw_args.keys() {
                if k != "sep" && k != "maxsplit" {
                    return Err(StarlarkError(format!(
                        "split() got unexpected keyword argument: {k}"
                    )));
                }
            }
            if !pos_args.is_empty() && kw_args.contains_key("sep") {
                return Err(StarlarkError(
                    "split() got multiple values for parameter 'sep'".to_string(),
                ));
            }
            if pos_args.len() > 1 && kw_args.contains_key("maxsplit") {
                return Err(StarlarkError(
                    "split() got multiple values for parameter 'maxsplit'".to_string(),
                ));
            }
            let sep_val = pos_args
                .first()
                .or_else(|| kw_args.get("sep"))
                .ok_or_else(|| {
                    StarlarkError("split() missing 1 required positional argument: sep".to_string())
                })?;
            let sep = match sep_val {
                Value::String(sep) => sep.as_str(),
                other => {
                    return Err(StarlarkError(format!(
                        "in call to split(), parameter 'sep' got value of type '{}', want 'string'",
                        other.type_name()
                    )))
                }
            };
            if sep.is_empty() {
                return Err(StarlarkError("Empty separator".to_string()));
            }
            let maxsplit_val = pos_args.get(1).or_else(|| kw_args.get("maxsplit"));
            let maxsplit = match maxsplit_val {
                None => -1i64,
                Some(v) => to_int32(v, "maxsplit")? as i64,
            };

            let mut parts: Vec<String> = Vec::new();
            let mut start = 0usize;
            let mut rem = maxsplit;
            loop {
                if rem == 0 {
                    parts.push(s[start..].to_string());
                    break;
                }
                if let Some(rel) = s[start..].find(sep) {
                    parts.push(s[start..start + rel].to_string());
                    start = start + rel + sep.len();
                    rem -= 1;
                } else {
                    parts.push(s[start..].to_string());
                    break;
                }
            }
            Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new_in(
                parts.into_iter().map(Value::String).collect(),
                Arc::clone(mutability),
            )))))
        }
        "splitlines" => {
            if pos_args.len() > 1 {
                return Err(StarlarkError(format!(
                    "splitlines() accepts no more than 1 positional argument but got {}",
                    pos_args.len()
                )));
            }
            for k in kw_args.keys() {
                if k != "keepends" {
                    return Err(StarlarkError(format!(
                        "splitlines() got unexpected keyword argument '{k}'"
                    )));
                }
            }
            if !pos_args.is_empty() && kw_args.contains_key("keepends") {
                return Err(StarlarkError(
                    "splitlines() got multiple values for parameter 'keepends'".to_string(),
                ));
            }
            let keepends = match pos_args.first().or_else(|| kw_args.get("keepends")) {
                None => false,
                Some(Value::Bool(b)) => *b,
                Some(other) => {
                    return Err(StarlarkError(format!(
                        "in call to splitlines(), parameter 'keepends' got value of type '{}', want 'bool'",
                        other.type_name()
                    )));
                }
            };
            let mut lines = Vec::new();
            let mut start = 0;
            let bytes = s.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'\n' {
                    let end = if keepends { i + 1 } else { i };
                    lines.push(s[start..end].to_string());
                    start = i + 1;
                } else if bytes[i] == b'\r' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                        let end = if keepends { i + 2 } else { i };
                        lines.push(s[start..end].to_string());
                        start = i + 2;
                        i += 1;
                    } else {
                        let end = if keepends { i + 1 } else { i };
                        lines.push(s[start..end].to_string());
                        start = i + 1;
                    }
                }
                i += 1;
            }
            if start < bytes.len() {
                lines.push(s[start..].to_string());
            }
            Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new_in(
                lines.into_iter().map(Value::String).collect(),
                Arc::clone(mutability),
            )))))
        }
        "startswith" => {
            let (start, end) = (pos_args.get(1), pos_args.get(2));
            let slice = get_slice_utf16(s, start, end)?;
            match pos_args.first() {
                Some(Value::String(prefix)) => {
                    let pre_u16: Vec<u16> = prefix.encode_utf16().collect();
                    Ok(Value::Bool(slice.starts_with(&pre_u16)))
                }
                Some(Value::Tuple(prefixes)) => {
                    for (i, pre) in prefixes.iter().enumerate() {
                        if !matches!(pre, Value::String(_)) {
                            return Err(StarlarkError(format!(
                                "at index {i} of sub, got element of type {}, want string",
                                pre.type_name()
                            )));
                        }
                    }
                    for pre in prefixes {
                        let pre_u16: Vec<u16> = pre.as_str().unwrap().encode_utf16().collect();
                        if slice.starts_with(&pre_u16) {
                            return Ok(Value::Bool(true));
                        }
                    }
                    Ok(Value::Bool(false))
                }
                _ => Err(StarlarkError(
                    "startswith() prefix must be str or tuple of str".to_string(),
                )),
            }
        }
        "strip" => {
            let chars_to_strip = pos_args.first().and_then(|v| v.as_str());
            let res = match chars_to_strip {
                Some(chars) => s.trim_matches(|c| chars.contains(c)).to_string(),
                None => s.trim().to_string(),
            };
            Ok(Value::String(res))
        }
        "title" => {
            let mut out = String::new();
            let mut prev_letter = false;
            for c in s.chars() {
                if c.is_alphabetic() {
                    if !prev_letter {
                        out.extend(c.to_uppercase());
                    } else {
                        out.extend(c.to_lowercase());
                    }
                    prev_letter = true;
                } else {
                    out.push(c);
                    prev_letter = false;
                }
            }
            Ok(Value::String(out))
        }
        "upper" => Ok(Value::String(s.to_uppercase())),
        _ => Err(StarlarkError(format!(
            "'string' value has no field or method '{method}'"
        ))),
    }
}

fn get_offsets_utf16(
    s: &str,
    start_val: Option<&Value>,
    end_val: Option<&Value>,
) -> Result<(usize, usize), StarlarkError> {
    let len = s.encode_utf16().count() as i64;
    let s_idx = match start_val {
        None | Some(Value::None) => 0usize,
        Some(v) => {
            let i = to_int32(v, "start")? as i64;
            if i < 0 {
                (len + i).max(0) as usize
            } else {
                i.min(len) as usize
            }
        }
    };
    let e_idx = match end_val {
        None | Some(Value::None) => len as usize,
        Some(v) => {
            let i = to_int32(v, "end")? as i64;
            if i < 0 {
                (len + i).max(0) as usize
            } else {
                i.min(len) as usize
            }
        }
    };
    Ok((s_idx.min(e_idx), e_idx))
}

fn get_slice_utf16(
    s: &str,
    start_val: Option<&Value>,
    end_val: Option<&Value>,
) -> Result<Vec<u16>, StarlarkError> {
    let (s_idx, e_idx) = get_offsets_utf16(s, start_val, end_val)?;
    let units: Vec<u16> = s.encode_utf16().collect();
    if s_idx >= e_idx {
        Ok(Vec::new())
    } else {
        Ok(units[s_idx..e_idx].to_vec())
    }
}

fn find_sub_utf16(hay: &[u16], needle: &[u16]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

fn rfind_sub_utf16(hay: &[u16], needle: &[u16]) -> Option<usize> {
    if needle.is_empty() {
        return Some(hay.len());
    }
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len())
        .rev()
        .find(|&i| hay[i..i + needle.len()] == *needle)
}
