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

use crate::eval::builtins::universal::value_to_sequence;
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
            let slice = get_slice(s, pos_args.get(1), pos_args.get(2))?;
            if sub.is_empty() {
                Ok(Value::Int(crate::bigint::BigInt::from_u64(
                    (slice.chars().count() + 1) as u64,
                )))
            } else {
                let cnt = slice.matches(sub).count();
                Ok(Value::Int(crate::bigint::BigInt::from_u64(cnt as u64)))
            }
        }
        "elems" => {
            let elems: Vec<Value> = s.chars().map(|c| Value::String(c.to_string())).collect();
            // Bazel returns a list, whose slice `[:]` is a mutable copy.
            Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new_in(
                elems,
                Arc::clone(mutability),
            )))))
        }
        "endswith" => {
            let (start, end) = (pos_args.get(1), pos_args.get(2));
            let slice = get_slice(s, start, end)?;
            match pos_args.first() {
                Some(Value::String(suffix)) => Ok(Value::Bool(slice.ends_with(suffix))),
                Some(Value::Tuple(suffixes)) => {
                    for suf in suffixes {
                        if let Some(suffix_str) = suf.as_str() {
                            if slice.ends_with(suffix_str) {
                                return Ok(Value::Bool(true));
                            }
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
            let (start_idx, _) = get_offsets(s, pos_args.get(1), pos_args.get(2));
            let slice = get_slice(s, pos_args.get(1), pos_args.get(2))?;
            if let Some(pos) = slice.find(sub) {
                // Character index
                let char_offset = slice[..pos].chars().count();
                Ok(Value::Int(crate::bigint::BigInt::from_i64(
                    (start_idx + char_offset) as i64,
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
            let (start_idx, _) = get_offsets(s, pos_args.get(1), pos_args.get(2));
            let slice = get_slice(s, pos_args.get(1), pos_args.get(2))?;
            if let Some(pos) = slice.find(sub) {
                let char_offset = slice[..pos].chars().count();
                Ok(Value::Int(crate::bigint::BigInt::from_i64(
                    (start_idx + char_offset) as i64,
                )))
            } else {
                Err(StarlarkError(format!("substring not found: {sub:?}")))
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
            let prefix = pos_args.first().and_then(|v| v.as_str()).unwrap_or("");
            Ok(Value::String(
                s.strip_prefix(prefix).unwrap_or(s).to_string(),
            ))
        }
        "removesuffix" => {
            let suffix = pos_args.first().and_then(|v| v.as_str()).unwrap_or("");
            Ok(Value::String(
                s.strip_suffix(suffix).unwrap_or(s).to_string(),
            ))
        }
        "replace" => {
            let old = pos_args.first().and_then(|v| v.as_str()).unwrap_or("");
            let new = pos_args.get(1).and_then(|v| v.as_str()).unwrap_or("");
            let maxsplit = pos_args
                .get(2)
                .or_else(|| kw_args.get("maxsplit"))
                .and_then(|v| v.as_i64())
                .unwrap_or(-1);

            if maxsplit == 0 {
                return Ok(Value::String(s.to_string()));
            }
            if maxsplit < 0 {
                Ok(Value::String(s.replace(old, new)))
            } else {
                Ok(Value::String(s.replacen(old, new, maxsplit as usize)))
            }
        }
        "rfind" => {
            let sub = pos_args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| StarlarkError("rfind() requires sub string".to_string()))?;
            let (start_idx, _) = get_offsets(s, pos_args.get(1), pos_args.get(2));
            let slice = get_slice(s, pos_args.get(1), pos_args.get(2))?;
            if let Some(pos) = slice.rfind(sub) {
                let char_offset = slice[..pos].chars().count();
                Ok(Value::Int(crate::bigint::BigInt::from_i64(
                    (start_idx + char_offset) as i64,
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
            let (start_idx, _) = get_offsets(s, pos_args.get(1), pos_args.get(2));
            let slice = get_slice(s, pos_args.get(1), pos_args.get(2))?;
            if let Some(pos) = slice.rfind(sub) {
                let char_offset = slice[..pos].chars().count();
                Ok(Value::Int(crate::bigint::BigInt::from_i64(
                    (start_idx + char_offset) as i64,
                )))
            } else {
                Err(StarlarkError(format!("substring not found: {sub:?}")))
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
                Some(Value::Int(i)) => i
                    .to_i64()
                    .ok_or_else(|| StarlarkError("maxsplit out of range".to_string()))?,
                Some(other) => {
                    return Err(StarlarkError(format!(
                    "in call to rsplit(), parameter 'maxsplit' got value of type '{}', want 'int'",
                    other.type_name()
                )))
                }
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
                Some(Value::Int(i)) => i
                    .to_i64()
                    .ok_or_else(|| StarlarkError("maxsplit out of range".to_string()))?,
                Some(other) => {
                    return Err(StarlarkError(format!(
                    "in call to split(), parameter 'maxsplit' got value of type '{}', want 'int'",
                    other.type_name()
                )))
                }
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
            let keepends = pos_args.first().map(|v| v.is_truthy()).unwrap_or(false);
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
            let slice = get_slice(s, start, end)?;
            match pos_args.first() {
                Some(Value::String(prefix)) => Ok(Value::Bool(slice.starts_with(prefix))),
                Some(Value::Tuple(prefixes)) => {
                    for pre in prefixes {
                        if let Some(prefix_str) = pre.as_str() {
                            if slice.starts_with(prefix_str) {
                                return Ok(Value::Bool(true));
                            }
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
            let mut prev_alphanumeric = false;
            for c in s.chars() {
                if c.is_alphabetic() {
                    if !prev_alphanumeric {
                        out.extend(c.to_uppercase());
                    } else {
                        out.extend(c.to_lowercase());
                    }
                    prev_alphanumeric = true;
                } else {
                    out.push(c);
                    prev_alphanumeric = c.is_alphanumeric();
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

fn get_offsets(s: &str, start_val: Option<&Value>, end_val: Option<&Value>) -> (usize, usize) {
    let len = s.chars().count();
    let s_idx = match start_val.and_then(|v| v.as_i64()) {
        Some(i) if i < 0 => (len as isize + i as isize).max(0) as usize,
        Some(i) => (i as usize).min(len),
        None => 0,
    };
    let e_idx = match end_val.and_then(|v| v.as_i64()) {
        Some(i) if i < 0 => (len as isize + i as isize).max(0) as usize,
        Some(i) => (i as usize).min(len),
        None => len,
    };
    (s_idx.min(e_idx), e_idx)
}

fn get_slice<'a>(
    s: &'a str,
    start_val: Option<&Value>,
    end_val: Option<&Value>,
) -> Result<&'a str, StarlarkError> {
    let (s_idx, e_idx) = get_offsets(s, start_val, end_val);
    let mut byte_start = s.len();
    let mut byte_end = s.len();
    for (char_idx, (b_idx, _)) in s.char_indices().enumerate() {
        if char_idx == s_idx {
            byte_start = b_idx;
        }
        if char_idx == e_idx {
            byte_end = b_idx;
            break;
        }
    }
    if s_idx >= e_idx {
        Ok("")
    } else {
        Ok(&s[byte_start..byte_end])
    }
}
