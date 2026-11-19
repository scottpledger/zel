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

/// String `%` formatting matching Bazel's `Printer.formatWithList`.
pub fn percent_format(
    pattern: &str,
    args: &[Value],
    _dict_map: Option<&BTreeMap<String, Value>>,
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

        let p = i;
        i += 1;
        if i >= chars.len() {
            return Err(StarlarkError(format!(
                "incomplete format pattern ends with %: {}",
                Value::String(pattern.to_string()).repr()
            )));
        }

        let conv = chars[i];
        i += 1;

        // %%: literal %
        if conv == '%' {
            out.push('%');
            continue;
        }

        if a >= args.len() {
            return Err(StarlarkError(format!(
                "not enough arguments for format pattern {}: {}",
                Value::String(pattern.to_string()).repr(),
                Value::Tuple(args.to_vec()).repr()
            )));
        }

        let arg = &args[a];
        a += 1;

        format_one_value(&mut out, conv, arg, p + 1, pattern)?;
    }

    if a < args.len() {
        return Err(StarlarkError(
            "not all arguments converted during string formatting".to_string(),
        ));
    }

    Ok(out)
}

fn format_one_value(
    out: &mut String,
    conv: char,
    arg: &Value,
    conv_idx: usize,
    pattern: &str,
) -> Result<(), StarlarkError> {
    match conv {
        's' => {
            out.push_str(&arg.to_string());
        }
        'r' => {
            out.push_str(&arg.repr());
        }
        'd' | 'o' | 'x' | 'X' => {
            let n = match arg {
                Value::Int(n) => n.clone(),
                Value::Float(f) => crate::bigint::finite_double_to_int_exact(*f)
                    .map_err(|_| StarlarkError(format!("got {arg}, want a finite number")))?,
                _ => {
                    return Err(StarlarkError(format!(
                        "got {} for '%{conv}' format, want int or float",
                        arg.type_name()
                    )));
                }
            };
            match conv {
                'd' => out.push_str(&n.to_string()),
                'o' => out.push_str(&format!("{n:o}")),
                'x' => out.push_str(&format!("{n:x}")),
                'X' => out.push_str(&format!("{n:X}")),
                _ => unreachable!(),
            }
        }
        'e' | 'f' | 'g' | 'E' | 'F' | 'G' => match arg {
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
                "unsupported format character \"{other}\" at index {conv_idx} in {}",
                Value::String(pattern.to_string()).repr()
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
                        "Cannot mix manual and automatic numbering of positional fields"
                            .to_string(),
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
                        "Cannot mix manual and automatic numbering of positional fields"
                            .to_string(),
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
