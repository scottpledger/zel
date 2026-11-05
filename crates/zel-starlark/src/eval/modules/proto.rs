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
use crate::types::struct_value::StructValue;
use crate::types::value::{format_float, Value};

pub fn proto_encode_text(x: &Value) -> Result<String, StarlarkError> {
    let struct_val = match x {
        Value::Struct(s) => s.as_ref(),
        other => {
            return Err(StarlarkError(format!(
                "invalid message: got {}, want struct",
                other.type_name()
            )))
        }
    };
    let mut out = String::new();
    encode_struct_message(struct_val, &mut out, 0)?;
    Ok(out)
}

fn encode_struct_message(
    s: &StructValue,
    out: &mut String,
    indent: usize,
) -> Result<(), StarlarkError> {
    for (name, val) in &s.fields {
        encode_field(name, val, out, indent)?;
    }
    Ok(())
}

fn encode_field(
    name: &str,
    val: &Value,
    out: &mut String,
    indent: usize,
) -> Result<(), StarlarkError> {
    match val {
        Value::None => Ok(()),
        Value::List(l) => {
            let guard = l.lock().unwrap();
            for (i, elem) in guard.elements.iter().enumerate() {
                encode_field_element(name, elem, out, indent).map_err(|e| {
                    StarlarkError(format!("at list index {i}: {}", e.0))
                })?;
            }
            Ok(())
        }
        Value::Tuple(items) => {
            for (i, elem) in items.iter().enumerate() {
                encode_field_element(name, elem, out, indent).map_err(|e| {
                    StarlarkError(format!("at tuple index {i}: {}", e.0))
                })?;
            }
            Ok(())
        }
        Value::Dict(d) => {
            let guard = d.lock().unwrap();
            for (k, v) in &guard.entries {
                if !matches!(k, Value::String(_) | Value::Int(_)) {
                    return Err(StarlarkError(format!(
                        "invalid dict key: got {}, want int or string",
                        k.type_name()
                    )));
                }
                emit_indent(out, indent);
                out.push_str(name);
                out.push_str(" {\n");

                encode_field_element("key", k, out, indent + 1)?;
                encode_field_element("value", v, out, indent + 1)?;

                emit_indent(out, indent);
                out.push_str("}\n");
            }
            Ok(())
        }
        other => encode_field_element(name, other, out, indent),
    }
}

fn encode_field_element(
    name: &str,
    val: &Value,
    out: &mut String,
    indent: usize,
) -> Result<(), StarlarkError> {
    match val {
        Value::Struct(sub_struct) => {
            emit_indent(out, indent);
            out.push_str(name);
            out.push_str(" {\n");
            encode_struct_message(sub_struct, out, indent + 1)?;
            emit_indent(out, indent);
            out.push_str("}\n");
        }
        Value::String(s) => {
            emit_indent(out, indent);
            out.push_str(name);
            out.push_str(": \"");
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    other => out.push(other),
                }
            }
            out.push_str("\"\n");
        }
        Value::Int(i) => {
            emit_indent(out, indent);
            out.push_str(name);
            out.push_str(": ");
            out.push_str(&i.to_string());
            out.push('\n');
        }
        Value::Bool(b) => {
            emit_indent(out, indent);
            out.push_str(name);
            out.push_str(": ");
            out.push_str(if *b { "true" } else { "false" });
            out.push('\n');
        }
        Value::Float(f) => {
            emit_indent(out, indent);
            out.push_str(name);
            out.push_str(": ");
            let mut s = format_float(*f);
            if s == "+inf" {
                s = "inf".to_string();
            }
            out.push_str(&s);
            out.push('\n');
        }
        other => {
            return Err(StarlarkError(format!(
                "got {}, want string, int, float, bool, or struct",
                other.type_name()
            )));
        }
    }
    Ok(())
}

fn emit_indent(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push_str("  ");
    }
}
