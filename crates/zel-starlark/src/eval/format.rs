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
use crate::types::value::Value;
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
                Value::Float(f) => {
                    crate::bigint::finite_double_to_int_exact(*f).map_err(StarlarkError)?
                }
                _ => {
                    return Err(StarlarkError(format!(
                        "got {} for '%{conv}' format, want int or float",
                        arg.type_name()
                    )));
                }
            };
            match conv {
                'd' => out.push_str(&n.to_string()),
                'o' => {
                    if let Some(i) = n.to_i64() {
                        if let Ok(i32_v) = i32::try_from(i) {
                            out.push_str(&format!("{:o}", i32_v as u32));
                        } else {
                            out.push_str(&format!("{:o}", i as u64));
                        }
                    } else {
                        out.push_str(&format!("{n:o}"));
                    }
                }
                'x' => {
                    if let Some(i) = n.to_i64() {
                        if let Ok(i32_v) = i32::try_from(i) {
                            out.push_str(&format!("{:x}", i32_v as u32));
                        } else {
                            out.push_str(&format!("{:x}", i as u64));
                        }
                    } else {
                        out.push_str(&format!("{n:x}"));
                    }
                }
                'X' => {
                    if let Some(i) = n.to_i64() {
                        if let Ok(i32_v) = i32::try_from(i) {
                            out.push_str(&format!("{:X}", i32_v as u32));
                        } else {
                            out.push_str(&format!("{:X}", i as u64));
                        }
                    } else {
                        out.push_str(&format!("{n:X}"));
                    }
                }
                _ => unreachable!(),
            }
        }
        'e' | 'f' | 'g' | 'E' | 'F' | 'G' => match arg {
            Value::Float(f) => out.push_str(&format_float_conv(*f, conv)),
            Value::Int(n) => out.push_str(&format_float_conv(n.to_f64(), conv)),
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

/// Formats `v` according to Starlark's `StarlarkFloat.format(v, conv)` for `%e`/`%E`/`%f`/`%F`/`%g`/`%G`.
pub fn format_float_conv(v: f64, conv: char) -> String {
    if v.is_nan() {
        return if conv.is_ascii_uppercase() {
            "NAN".to_string()
        } else {
            "nan".to_string()
        };
    }
    if v.is_infinite() {
        let s = if v.is_sign_positive() { "+inf" } else { "-inf" };
        return if conv.is_ascii_uppercase() {
            s.to_ascii_uppercase()
        } else {
            s.to_string()
        };
    }
    let sign = if v.is_sign_negative() { "-" } else { "" };
    let abs = v.abs();
    match conv {
        'f' | 'F' => {
            let sci = format!("{abs:e}");
            let (mant, exp_s) = sci.split_once('e').unwrap_or((&sci, "0"));
            let exp: i32 = exp_s.parse().unwrap_or(0);
            let digits: String = mant.chars().filter(|c| *c != '.').collect();
            if exp >= 16 && digits.len() <= (exp as usize + 1) {
                let pad = (exp as usize + 1) - digits.len();
                format!("{sign}{digits}{}.000000", "0".repeat(pad))
            } else {
                format!("{sign}{abs:.6}")
            }
        }
        'e' | 'E' => {
            let sci = format!("{abs:.6e}");
            let (mant, exp_s) = sci.split_once('e').unwrap_or((&sci, "0"));
            let exp: i32 = exp_s.parse().unwrap_or(0);
            let e_ch = if conv == 'E' { 'E' } else { 'e' };
            let exp_sign = if exp < 0 { '-' } else { '+' };
            format!("{sign}{mant}{e_ch}{exp_sign}{:02}", exp.abs())
        }
        'g' | 'G' => {
            if v == 0.0 {
                return if v.is_sign_negative() {
                    "-0.0".to_string()
                } else {
                    "0.0".to_string()
                };
            }
            let neg = v.is_sign_negative();
            let abs = v.abs();
            let sci = format!("{abs:e}");
            let (mant, exp_s) = sci.split_once('e').unwrap();
            let exp: i32 = exp_s.parse().unwrap();
            let digits: String = mant.chars().filter(|&c| c != '.').collect();
            let e_ch = if conv == 'G' { 'E' } else { 'e' };
            let mut out = if (-4..17).contains(&exp) {
                if exp < 0 {
                    let mut r = String::from("0.");
                    for _ in 0..(-exp - 1) {
                        r.push('0');
                    }
                    r.push_str(&digits);
                    r
                } else {
                    let split = (exp + 1) as usize;
                    if digits.len() <= split {
                        let mut r = digits;
                        for _ in r.len()..split {
                            r.push('0');
                        }
                        r.push_str(".0");
                        r
                    } else {
                        format!("{}.{}", &digits[..split], &digits[split..])
                    }
                }
            } else {
                let sign_ch = if exp >= 0 { '+' } else { '-' };
                let aexp = exp.unsigned_abs();
                let m = if digits.len() == 1 {
                    digits
                } else {
                    format!("{}.{}", &digits[..1], &digits[1..])
                };
                format!("{m}{e_ch}{sign_ch}{aexp:02}")
            };
            if neg {
                out.insert(0, '-');
            }
            out
        }
        _ => crate::types::value::format_float(v),
    }
}
