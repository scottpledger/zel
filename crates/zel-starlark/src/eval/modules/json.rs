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
use crate::types::value::{format_float, Value};
use std::sync::{Arc, Mutex};

pub fn json_encode(x: &Value) -> Result<String, StarlarkError> {
    let mut out = String::new();
    encode_val(x, &mut out, 0)?;
    Ok(out)
}

fn encode_val(x: &Value, out: &mut String, depth: usize) -> Result<(), StarlarkError> {
    if depth > 500 {
        return Err(StarlarkError("nesting depth limit exceeded".to_string()));
    }
    match x {
        Value::None => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Float(f) => {
            if f.is_nan() || f.is_infinite() {
                return Err(StarlarkError(format!(
                    "cannot encode non-finite float {}",
                    format_float(*f)
                )));
            }
            out.push_str(&format_float(*f));
        }
        Value::String(s) => append_quoted_json(s, out),
        Value::List(l) => {
            out.push('[');
            let guard = l.lock().unwrap();
            for (i, elem) in guard.elements.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                encode_val(elem, out, depth + 1).map_err(|e| {
                    StarlarkError(format!("at list index {i}: {}", e.0))
                })?;
            }
            out.push(']');
        }
        Value::Tuple(items) => {
            out.push('[');
            for (i, elem) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                encode_val(elem, out, depth + 1).map_err(|e| {
                    StarlarkError(format!("at tuple index {i}: {}", e.0))
                })?;
            }
            out.push(']');
        }
        Value::Range(r) => {
            out.push('[');
            for (i, val) in r.to_vec().iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&val.to_string());
            }
            out.push(']');
        }
        Value::Set(s) => {
            out.push('[');
            let guard = s.lock().unwrap();
            for (i, elem) in guard.elements.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                encode_val(elem, out, depth + 1)?;
            }
            out.push(']');
        }
        Value::Dict(d) => {
            let guard = d.lock().unwrap();
            // Validate all keys are strings
            for (k, _) in &guard.entries {
                if !matches!(k, Value::String(_)) {
                    return Err(StarlarkError(format!(
                        "dict has {} key, want string",
                        k.type_name()
                    )));
                }
            }
            // Sort keys lexicographically for determinism
            let mut sorted_entries = guard.entries.clone();
            sorted_entries.sort_by(|(k1, _), (k2, _)| {
                k1.as_str().unwrap_or("").cmp(k2.as_str().unwrap_or(""))
            });

            out.push('{');
            for (i, (k, v)) in sorted_entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                append_quoted_json(k.as_str().unwrap(), out);
                out.push(':');
                encode_val(v, out, depth + 1).map_err(|e| {
                    StarlarkError(format!("in dict key {:?}: {}", k.as_str().unwrap(), e.0))
                })?;
            }
            out.push('}');
        }
        Value::Struct(s) => {
            out.push('{');
            for (i, (k, v)) in s.fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                append_quoted_json(k, out);
                out.push(':');
                encode_val(v, out, depth + 1).map_err(|e| {
                    StarlarkError(format!("in struct field .{k}: {}", e.0))
                })?;
            }
            out.push('}');
        }
        other => {
            return Err(StarlarkError(format!(
                "cannot encode {} as JSON",
                other.type_name()
            )));
        }
    }
    Ok(())
}

fn append_quoted_json(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

pub fn json_decode(s: &str, default: Option<Value>) -> Result<Value, StarlarkError> {
    match JsonDecoder::new(s).decode() {
        Ok(v) => Ok(v),
        Err(e) => {
            if let Some(def) = default {
                Ok(def)
            } else {
                Err(e)
            }
        }
    }
}

pub fn json_indent(s: &str, prefix: &str, indent: &str) -> Result<String, StarlarkError> {
    let mut out = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut depth = 0usize;
    let mut in_str = false;

    while i < chars.len() {
        let c = chars[i];
        if in_str {
            if c == '\\' {
                out.push(c);
                if i + 1 < chars.len() {
                    i += 1;
                    out.push(chars[i]);
                }
            } else if c == '"' {
                in_str = false;
                out.push(c);
            } else {
                out.push(c);
            }
            i += 1;
            continue;
        }

        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '{' | '[' => {
                out.push(c);
                // Check if immediately closed
                let mut next_idx = i + 1;
                while next_idx < chars.len() && chars[next_idx].is_whitespace() {
                    next_idx += 1;
                }
                if next_idx < chars.len() && ((c == '{' && chars[next_idx] == '}') || (c == '[' && chars[next_idx] == ']')) {
                    i = next_idx;
                    out.push(chars[i]);
                } else {
                    depth += 1;
                    out.push('\n');
                    out.push_str(prefix);
                    for _ in 0..depth {
                        out.push_str(indent);
                    }
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                out.push('\n');
                out.push_str(prefix);
                for _ in 0..depth {
                    out.push_str(indent);
                }
                out.push(c);
            }
            ':' => {
                out.push(':');
                out.push(' ');
            }
            ',' => {
                out.push(',');
                out.push('\n');
                out.push_str(prefix);
                for _ in 0..depth {
                    out.push_str(indent);
                }
            }
            c if c.is_whitespace() => {}
            other => out.push(other),
        }
        i += 1;
    }
    Ok(out)
}

struct JsonDecoder {
    chars: Vec<char>,
    pos: usize,
}

impl JsonDecoder {
    fn new(src: &str) -> Self {
        Self {
            chars: src.chars().collect(),
            pos: 0,
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        if self.pos < self.chars.len() {
            self.pos += 1;
        }
        c
    }

    fn decode(&mut self) -> Result<Value, StarlarkError> {
        self.skip_ws();
        let val = self.parse_value()?;
        self.skip_ws();
        if self.pos < self.chars.len() {
            return Err(StarlarkError("unexpected trailing data in JSON".to_string()));
        }
        Ok(val)
    }

    fn parse_value(&mut self) -> Result<Value, StarlarkError> {
        self.skip_ws();
        let c = self
            .peek()
            .ok_or_else(|| StarlarkError("unexpected end of JSON".to_string()))?;
        match c {
            'n' => self.parse_literal("null", Value::None),
            't' => self.parse_literal("true", Value::Bool(true)),
            'f' => self.parse_literal("false", Value::Bool(false)),
            '"' => Ok(Value::String(self.parse_string()?)),
            '[' => self.parse_array(),
            '{' => self.parse_object(),
            '-' | '0'..='9' => self.parse_number(),
            other => Err(StarlarkError(format!("unexpected character '{other}' in JSON"))),
        }
    }

    fn parse_literal(&mut self, expected: &str, val: Value) -> Result<Value, StarlarkError> {
        for ch in expected.chars() {
            if self.next() != Some(ch) {
                return Err(StarlarkError(format!("expected '{expected}' in JSON")));
            }
        }
        Ok(val)
    }

    fn parse_string(&mut self) -> Result<String, StarlarkError> {
        self.next(); // consume opening quote '"'
        let mut out = String::new();
        while let Some(c) = self.next() {
            if c == '"' {
                return Ok(out);
            }
            if c == '\\' {
                let esc = self
                    .next()
                    .ok_or_else(|| StarlarkError("incomplete escape in JSON string".to_string()))?;
                match esc {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\x08'),
                    'f' => out.push('\x0c'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let mut hex = String::new();
                        for _ in 0..4 {
                            hex.push(
                                self.next()
                                    .ok_or_else(|| StarlarkError("incomplete \\uXXXX escape".to_string()))?,
                            );
                        }
                        let code = u32::from_str_radix(&hex, 16)
                            .map_err(|_| StarlarkError(format!("invalid hex in \\u escape: {hex}")))?;

                        // Check for surrogate pairs: if high surrogate \uD800..\uDBFF
                        if (0xD800..=0xDBFF).contains(&code) {
                            if self.peek() == Some('\\') && self.chars.get(self.pos + 1) == Some(&'u') {
                                self.pos += 2; // consume \u
                                let mut hex2 = String::new();
                                for _ in 0..4 {
                                    hex2.push(self.next().ok_or_else(|| {
                                        StarlarkError("incomplete \\uXXXX escape".to_string())
                                    })?);
                                }
                                let code2 = u32::from_str_radix(&hex2, 16).map_err(|_| {
                                    StarlarkError(format!("invalid hex in \\u escape: {hex2}"))
                                })?;
                                if (0xDC00..=0xDFFF).contains(&code2) {
                                    let full = 0x10000 + (((code - 0xD800) << 10) | (code2 - 0xDC00));
                                    out.push(char::from_u32(full).unwrap_or('\u{FFFD}'));
                                    continue;
                                } else {
                                    out.push('\u{FFFD}');
                                    out.push(char::from_u32(code2).unwrap_or('\u{FFFD}'));
                                    continue;
                                }
                            }
                        }
                        out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                    }
                    other => {
                        return Err(StarlarkError(format!("invalid escape '\\{other}' in JSON")));
                    }
                }
            } else {
                out.push(c);
            }
        }
        Err(StarlarkError("unclosed string literal in JSON".to_string()))
    }

    fn parse_number(&mut self) -> Result<Value, StarlarkError> {
        let start = self.pos;
        let mut is_float = false;
        if self.peek() == Some('-') {
            self.next();
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.next();
            } else if c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-' {
                is_float = true;
                self.next();
            } else {
                break;
            }
        }
        let end = self.pos;
        let num_str: String = self.chars[start..end].iter().collect();

        if is_float {
            let f = num_str.parse::<f64>().map_err(|e| {
                StarlarkError(format!("invalid float in JSON '{num_str}': {e}"))
            })?;
            Ok(Value::Float(f))
        } else {
            let bi = BigInt::from_str_radix(&num_str, 10).map_err(|e| {
                StarlarkError(format!("invalid integer in JSON '{num_str}': {e}"))
            })?;
            Ok(Value::Int(bi))
        }
    }

    fn parse_array(&mut self) -> Result<Value, StarlarkError> {
        self.next(); // consume '['
        self.skip_ws();
        let mut items = Vec::new();
        if self.peek() == Some(']') {
            self.next();
            return Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new(items)))));
        }
        loop {
            let item = self.parse_value()?;
            items.push(item);
            self.skip_ws();
            match self.peek() {
                Some(',') => {
                    self.next();
                }
                Some(']') => {
                    self.next();
                    break;
                }
                _ => return Err(StarlarkError("expected ',' or ']' in JSON array".to_string())),
            }
        }
        Ok(Value::List(Arc::new(Mutex::new(StarlarkList::new(items)))))
    }

    fn parse_object(&mut self) -> Result<Value, StarlarkError> {
        self.next(); // consume '{'
        self.skip_ws();
        let mut dict = StarlarkDict::new();
        if self.peek() == Some('}') {
            self.next();
            return Ok(Value::Dict(Arc::new(Mutex::new(dict))));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some('"') {
                return Err(StarlarkError("expected string key in JSON object".to_string()));
            }
            let key = Value::String(self.parse_string()?);
            self.skip_ws();
            if self.next() != Some(':') {
                return Err(StarlarkError("expected ':' after key in JSON object".to_string()));
            }
            let val = self.parse_value()?;
            let _ = dict.insert(key, val);
            self.skip_ws();
            match self.peek() {
                Some(',') => {
                    self.next();
                }
                Some('}') => {
                    self.next();
                    break;
                }
                _ => return Err(StarlarkError("expected ',' or '}' in JSON object".to_string())),
            }
        }
        Ok(Value::Dict(Arc::new(Mutex::new(dict))))
    }
}
