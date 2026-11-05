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

//! `StarlarkSemantics.toString()` as printed by `bazel info starlark-semantics`: the non-default key/value pairs
//! of the semantics derived from `BuildLanguageOptions` (`setFlags`), in `TreeMap` key order (keys carry their
//! `+`/`-` prefix while sorting, which is stripped when printed).

use crate::semantics_keys::{SEMANTICS_BOOL_KEYS, SEMANTICS_OTHER_KEYS};

/// `bool_of(flag)`: the effective boolean value of `flag` if it was set; `values_of(flag)`: the raw values given
/// for a (possibly repeated) non-boolean flag, in order.
pub fn starlark_semantics_string(
    bool_of: &dyn Fn(&str) -> Option<bool>,
    values_of: &dyn Fn(&str) -> Vec<String>,
) -> String {
    let mut entries: Vec<(String, String)> = Vec::new();
    for (key, flag, flag_default) in SEMANTICS_BOOL_KEYS {
        let value = bool_of(flag).unwrap_or(*flag_default);
        let key_default = key.starts_with('+');
        if value != key_default {
            entries.push((key.to_string(), value.to_string()));
        }
    }
    for (key, flag, _flag_default, key_default) in SEMANTICS_OTHER_KEYS {
        let given = values_of(flag);
        if given.is_empty() {
            continue; // flag defaults equal the key defaults for every non-boolean key at HEAD
        }
        let is_list = key_default.contains("ImmutableList");
        let is_num = key_default.ends_with('L') || key_default.chars().all(|c| c.is_ascii_digit());
        let is_enum = key_default.contains("Mode.");
        let (shown, default_shown) = if is_list {
            let items: Vec<String> =
                given.iter().flat_map(|v| v.split(',').filter(|s| !s.is_empty()).map(str::to_string)).collect();
            (format!("[{}]", items.join(", ")), "[]".to_string())
        } else if is_num {
            let n = given.last().unwrap().trim();
            let n = n.strip_prefix('+').unwrap_or(n);
            let norm = n.parse::<i64>().map(|v| v.to_string()).unwrap_or_else(|_| n.to_string());
            (norm, key_default.trim_end_matches('L').to_string())
        } else if is_enum {
            let d = key_default.rsplit('.').next().unwrap_or("").to_string();
            (given.last().unwrap().to_ascii_uppercase(), d)
        } else {
            let strip = *key == "bzl_file_size_limit_allowlist";
            let v = given.last().unwrap().clone();
            let v = if strip { v.trim().to_string() } else { v };
            (v, key_default.trim_matches('"').to_string())
        };
        if shown != default_shown {
            entries.push((key.to_string(), shown));
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let body: Vec<String> = entries
        .into_iter()
        .map(|(k, v)| format!("{}={v}", k.strip_prefix(['+', '-']).unwrap_or(&k)))
        .collect();
    format!("StarlarkSemantics{{{}}}", body.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_show_only_utf8_byte_strings() {
        let s = starlark_semantics_string(&|_| None, &|_| Vec::new());
        assert_eq!(s, "StarlarkSemantics{internal_bazel_only_utf_8_byte_strings=true}");
    }

    #[test]
    fn flags_change_the_output() {
        let s = starlark_semantics_string(
            &|f| match f {
                "experimental_builtins_dummy" => Some(true),
                "incompatible_disallow_empty_glob" => Some(false),
                _ => None,
            },
            &|f| if f == "max_computation_steps" { vec!["5".into()] } else { Vec::new() },
        );
        assert_eq!(
            s,
            "StarlarkSemantics{incompatible_disallow_empty_glob=false, experimental_builtins_dummy=true, internal_bazel_only_utf_8_byte_strings=true, max_computation_steps=5}"
        );
    }
}
