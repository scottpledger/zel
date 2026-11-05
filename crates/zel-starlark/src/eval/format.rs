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

use crate::types::list::StarlarkError;
use crate::types::value::{format_float, Value};
use std::collections::BTreeMap;

/// String `%` formatting matching Python / Starlark Printer.java.
pub fn percent_format(
    pattern: &str,
    args: &[Value],
    dict_map: Option<&BTreeMap<String, Value>>,
) -> Result<String, StarlarkError> {
    let mut out = String::new();
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    let mut a = 0;

    while i < chars.len() {
        if chars[i] != '%' {
            out.push(chars[i]);
            i += 1;
            continue;
        }

        i += 1;
        if i >= chars.len() {
            return Err(StarlarkError(format!(
                "incomplete format pattern ends with %: {pattern:?}"
            )));
        }

        // %%: literal %
        if chars[i] == '%' {
            out.push('%');
            i += 1;
            continue;
        }

        // Check for dict mapping %(key)s
        if chars[i] == '(' {
            i += 1;
            let mut key = String::new();
            while i < chars.len() && chars[i] != ')' {
                key.push(chars[i]);
                i += 1;
            }
            if i >= chars.len() {
                return Err(StarlarkError("incomplete format key".to_string()));
            }
            i += 1; // consume ')'
            let conv = if i < chars.len() {
                chars[i]
            } else {
                return Err(StarlarkError("incomplete format pattern".to_string()));
            };
            i += 1;

            let val = dict_map
                .and_then(|m| m.get(&key))
                .ok_or_else(|| StarlarkError(format!("key {key:?} not found in dict")))?;

            format_one_value(&mut out, conv, val)?;
            continue;
        }

        // Parse optional width and precision: %[flags][width][.precision]conv
        let mut zero_pad = false;
        if chars[i] == '0' {
            zero_pad = true;
            i += 1;
        }

        let mut width: usize = 0;
        while i < chars.len() && chars[i].is_ascii_digit() {
            width = width * 10 + chars[i].to_digit(10).unwrap() as usize;
            i += 1;
        }

        let mut precision: Option<usize> = None;
        if i < chars.len() && chars[i] == '.' {
            i += 1;
            let mut prec = 0usize;
            while i < chars.len() && chars[i].is_ascii_digit() {
                prec = prec * 10 + chars[i].to_digit(10).unwrap() as usize;
                i += 1;
            }
            precision = Some(prec);
        }

        if i >= chars.len() {
            return Err(StarlarkError(format!(
                "incomplete format pattern ends with %: {pattern:?}"
            )));
        }

        let conv = chars[i];
        i += 1;

        if a >= args.len() {
            return Err(StarlarkError(format!(
                "not enough arguments for format pattern {pattern:?}"
            )));
        }

        let arg = &args[a];
        a += 1;

        let mut formatted = String::new();
        format_one_value(&mut formatted, conv, arg)?;

        if let Some(prec) = precision {
            if matches!(conv, 'f' | 'g' | 'e' | 'F' | 'G' | 'E') {
                if let Value::Float(f) = arg {
                    formatted = format!("{f:.prec$}");
                }
            }
        }

        if width > formatted.len() {
            let pad_len = width - formatted.len();
            if zero_pad && (formatted.starts_with('-') || formatted.starts_with('+')) {
                let sign = formatted.remove(0);
                out.push(sign);
                for _ in 0..pad_len {
                    out.push('0');
                }
                out.push_str(&formatted);
            } else if zero_pad {
                for _ in 0..pad_len {
                    out.push('0');
                }
                out.push_str(&formatted);
            } else {
                for _ in 0..pad_len {
                    out.push(' ');
                }
                out.push_str(&formatted);
            }
        } else {
            out.push_str(&formatted);
        }
    }

    if a < args.len() {
        return Err(StarlarkError(
            "not all arguments converted during string formatting".to_string(),
        ));
    }

    Ok(out)
}

fn format_one_value(out: &mut String, conv: char, arg: &Value) -> Result<(), StarlarkError> {
    match conv {
        's' => {
            out.push_str(&arg.to_string());
        }
        'r' => {
            out.push_str(&arg.repr());
        }
        'd' | 'i' => match arg {
            Value::Int(n) => out.push_str(&n.to_string()),
            Value::Float(f) => {
                let bi = crate::bigint::BigInt::from_i64(*f as i64);
                out.push_str(&bi.to_string());
            }
            _ => {
                return Err(StarlarkError(format!(
                    "got {} for '%{conv}' format, want int or float",
                    arg.type_name()
                )));
            }
        },
        'x' => match arg {
            Value::Int(n) => out.push_str(&format!("{n:x}")),
            Value::Float(f) => {
                let bi = crate::bigint::BigInt::from_i64(*f as i64);
                out.push_str(&format!("{bi:x}"));
            }
            _ => {
                return Err(StarlarkError(format!(
                    "got {} for '%x' format, want int or float",
                    arg.type_name()
                )));
            }
        },
        'X' => match arg {
            Value::Int(n) => out.push_str(&format!("{n:X}")),
            Value::Float(f) => {
                let bi = crate::bigint::BigInt::from_i64(*f as i64);
                out.push_str(&format!("{bi:X}"));
            }
            _ => {
                return Err(StarlarkError(format!(
                    "got {} for '%X' format, want int or float",
                    arg.type_name()
                )));
            }
        },
        'o' => match arg {
            Value::Int(n) => out.push_str(&format!("{n:o}")),
            Value::Float(f) => {
                let bi = crate::bigint::BigInt::from_i64(*f as i64);
                out.push_str(&format!("{bi:o}"));
            }
            _ => {
                return Err(StarlarkError(format!(
                    "got {} for '%o' format, want int or float",
                    arg.type_name()
                )));
            }
        },
        'f' | 'F' | 'e' | 'E' | 'g' | 'G' => match arg {
            Value::Float(f) => out.push_str(&format_float(*f)),
            Value::Int(n) => out.push_str(&format_float(n.to_f64())),
            _ => {
                return Err(StarlarkError(format!(
                    "got {} for '%{conv}' format, want int or float",
                    arg.type_name()
                )));
            }
        },
        other => {
            return Err(StarlarkError(format!(
                "unsupported format character \"{other}\""
            )));
        }
    }
    Ok(())
}

/// Implementation of `str.format(*args, **kwargs)` matching Bazel's `FormatParser.java`.
pub fn str_format(
    input: &str,
    args: &[Value],
    kwargs: &BTreeMap<String, Value>,
) -> Result<String, StarlarkError> {
    let chars: Vec<char> = input.chars().collect();
    let mut output = String::new();
    let mut auto_idx = 0usize;
    let mut mode: Option<bool> = None; // None: not chosen, Some(true): automatic, Some(false): manual

    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '{' {
            if i + 1 < chars.len() && chars[i + 1] == '{' {
                // Escaped left brace `{{`
                output.push('{');
                i += 2;
                continue;
            }

            // Inside replacement field: find closing `}`
            let start = i + 1;
            let mut end = start;
            while end < chars.len() && chars[end] != '}' {
                if chars[end] == '{' {
                    return Err(StarlarkError(
                        "Nested replacement fields are not supported".to_string(),
                    ));
                }
                end += 1;
            }
            if end >= chars.len() {
                return Err(StarlarkError("unclosed '{' in format string".to_string()));
            }

            let field: String = chars[start..end].iter().collect();
            i = end + 1; // move past `}`

            // Check field contents
            for fc in field.chars() {
                if !fc.is_ascii_alphanumeric() && fc != '_' && fc != '(' && fc != ')' && fc != '-' {
                    return Err(StarlarkError(format!(
                        "Invalid character '{fc}' inside replacement field"
                    )));
                }
            }

            if field.is_empty() {
                // Automatic numbering
                if mode == Some(false) {
                    return Err(StarlarkError(
                        "Cannot mix manual and automatic numbering of positional fields".to_string(),
                    ));
                }
                mode = Some(true);
                if auto_idx >= args.len() {
                    return Err(StarlarkError(format!(
                        "No replacement found for index {auto_idx}"
                    )));
                }
                output.push_str(&args[auto_idx].to_string());
                auto_idx += 1;
            } else if let Ok(idx) = field.parse::<isize>() {
                if idx < 0 {
                    return Err(StarlarkError(format!(
                        "No replacement found for index {idx}"
                    )));
                }
                let idx = idx as usize;
                // Manual numbering
                if mode == Some(true) {
                    return Err(StarlarkError(
                        "Cannot mix manual and automatic numbering of positional fields".to_string(),
                    ));
                }
                mode = Some(false);
                if idx >= args.len() {
                    return Err(StarlarkError(format!(
                        "No replacement found for index {idx}"
                    )));
                }
                output.push_str(&args[idx].to_string());
            } else {
                // Named argument
                if let Some(val) = kwargs.get(&field) {
                    output.push_str(&val.to_string());
                } else {
                    return Err(StarlarkError(format!("Missing argument '{field}'")));
                }
            }
        } else if c == '}' {
            if i + 1 < chars.len() && chars[i + 1] == '}' {
                // Escaped right brace `}}`
                output.push('}');
                i += 2;
                continue;
            } else {
                return Err(StarlarkError("Found '}' without matching '{'".to_string()));
            }
        } else {
            output.push(c);
            i += 1;
        }
    }

    Ok(output)
}
