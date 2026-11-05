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

//! Schema-driven dynamic protobuf messages with binary, canonical-JSON and text-format codecs.
//!
//! Serialization matches the Java protobuf runtime used by Bazel: fields are written in field
//! number order, proto3 default values are omitted, repeated scalars are packed in proto3,
//! JSON uses `JsonFormat` conventions (lowerCamelCase names, 64-bit integers as strings, enum
//! names, base64 bytes, RFC 3339 timestamps) and text format follows `TextFormat.printer()`.

use crate::schema::{registry, FieldDesc, FieldType, MessageDesc};
use crate::wire;
use crate::DecodeError;
use std::collections::BTreeMap;

/// A single scalar/message value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    UInt(u64),
    Bool(bool),
    Float(f32),
    Double(f64),
    Str(String),
    Bytes(Vec<u8>),
    Enum(i32),
    Msg(Msg),
    /// An entry of a `map<K, V>` field.
    MapEntry(Box<Value>, Box<Value>),
}

/// A dynamically typed protobuf message bound to a schema in the global registry.
#[derive(Debug, Clone)]
pub struct Msg {
    desc: &'static MessageDesc,
    fields: BTreeMap<u32, Vec<Value>>,
    unknown: Vec<u8>,
}

impl PartialEq for Msg {
    fn eq(&self, o: &Self) -> bool {
        self.desc.full_name == o.desc.full_name && self.fields == o.fields && self.unknown == o.unknown
    }
}

impl Msg {
    /// Creates an empty message of the given fully-qualified type. Panics for unknown types
    /// (schemas are fixed at build time, so that is a programming error).
    pub fn new(full_name: &str) -> Self {
        Self::try_new(full_name).unwrap_or_else(|| panic!("unknown proto message type {full_name}"))
    }

    pub fn try_new(full_name: &str) -> Option<Self> {
        registry().message(full_name).map(|desc| Self {
            desc,
            fields: BTreeMap::new(),
            unknown: Vec::new(),
        })
    }

    pub fn type_name(&self) -> &str {
        &self.desc.full_name
    }

    pub fn desc(&self) -> &'static MessageDesc {
        self.desc
    }

    fn field(&self, name: &str) -> &'static FieldDesc {
        self.desc
            .field_by_name(name)
            .unwrap_or_else(|| panic!("{} has no field {name}", self.desc.full_name))
    }

    /// Clears other members of the same oneof, then stores `v` for singular/repeated semantics.
    fn put(&mut self, f: &'static FieldDesc, v: Value) {
        if f.is_repeated() {
            self.fields.entry(f.number).or_default().push(v);
            return;
        }
        if let Some(o) = &f.oneof {
            let others: Vec<u32> = self
                .desc
                .fields
                .iter()
                .filter(|g| g.oneof.as_ref() == Some(o) && g.number != f.number)
                .map(|g| g.number)
                .collect();
            for n in others {
                self.fields.remove(&n);
            }
        }
        self.fields.insert(f.number, vec![v]);
    }

    fn coerce_int(f: &FieldDesc, v: i64) -> Value {
        match &f.ty {
            FieldType::Enum(_) => Value::Enum(v as i32),
            t if t.is_unsigned() => Value::UInt(v as u64),
            FieldType::Double => Value::Double(v as f64),
            FieldType::Float => Value::Float(v as f32),
            _ => Value::Int(v),
        }
    }

    /// Sets an integer-typed (or enum, by number) field. Appends for repeated fields.
    pub fn set_int(&mut self, name: &str, v: i64) -> &mut Self {
        let f = self.field(name);
        self.put(f, Self::coerce_int(f, v));
        self
    }

    pub fn set_uint(&mut self, name: &str, v: u64) -> &mut Self {
        let f = self.field(name);
        let val = if f.ty.is_unsigned() { Value::UInt(v) } else { Value::Int(v as i64) };
        self.put(f, val);
        self
    }

    pub fn set_bool(&mut self, name: &str, v: bool) -> &mut Self {
        let f = self.field(name);
        self.put(f, Value::Bool(v));
        self
    }

    pub fn set_double(&mut self, name: &str, v: f64) -> &mut Self {
        let f = self.field(name);
        let val = if f.ty == FieldType::Float { Value::Float(v as f32) } else { Value::Double(v) };
        self.put(f, val);
        self
    }

    pub fn set_str(&mut self, name: &str, v: impl Into<String>) -> &mut Self {
        let f = self.field(name);
        self.put(f, Value::Str(v.into()));
        self
    }

    pub fn set_bytes(&mut self, name: &str, v: impl Into<Vec<u8>>) -> &mut Self {
        let f = self.field(name);
        self.put(f, Value::Bytes(v.into()));
        self
    }

    /// Sets an enum field by value name. Panics for unknown names (programming error).
    pub fn set_enum(&mut self, name: &str, value_name: &str) -> &mut Self {
        let f = self.field(name);
        let FieldType::Enum(en) = &f.ty else {
            panic!("{} is not an enum field", name)
        };
        let n = registry()
            .enum_desc(en)
            .and_then(|e| e.number_of(value_name))
            .unwrap_or_else(|| panic!("enum {en} has no value {value_name}"));
        self.put(f, Value::Enum(n));
        self
    }

    /// Sets (or appends, for repeated fields) a message-typed field.
    pub fn set_msg(&mut self, name: &str, m: Msg) -> &mut Self {
        let f = self.field(name);
        self.put(f, Value::Msg(m));
        self
    }

    /// Sets a message-typed field to an empty message (marks presence).
    pub fn set_empty(&mut self, name: &str) -> &mut Self {
        let f = self.field(name);
        let FieldType::Message(t) = &f.ty else {
            panic!("{name} is not a message field")
        };
        let m = Msg::new(t);
        self.put(f, Value::Msg(m));
        self
    }

    /// Creates a new empty message for message-typed field `name`.
    pub fn new_child(&self, name: &str) -> Msg {
        let FieldType::Message(t) = &self.field(name).ty else {
            panic!("{name} is not a message field")
        };
        Msg::new(t)
    }

    pub fn put_map(&mut self, name: &str, key: Value, value: Value) -> &mut Self {
        let f = self.field(name);
        assert!(f.map.is_some(), "{name} is not a map field");
        self.fields
            .entry(f.number)
            .or_default()
            .push(Value::MapEntry(Box::new(key), Box::new(value)));
        self
    }

    pub fn has(&self, name: &str) -> bool {
        self.fields.contains_key(&self.field(name).number)
    }

    pub fn clear(&mut self, name: &str) {
        let n = self.field(name).number;
        self.fields.remove(&n);
    }

    /// All values of a field (empty if unset).
    pub fn values(&self, name: &str) -> &[Value] {
        self.fields
            .get(&self.field(name).number)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn get_i64(&self, name: &str) -> i64 {
        match self.values(name).first() {
            Some(Value::Int(i)) => *i,
            Some(Value::UInt(u)) => *u as i64,
            Some(Value::Enum(e)) => *e as i64,
            Some(Value::Bool(b)) => *b as i64,
            _ => 0,
        }
    }

    pub fn get_bool(&self, name: &str) -> bool {
        matches!(self.values(name).first(), Some(Value::Bool(true)))
    }

    pub fn get_str(&self, name: &str) -> &str {
        match self.values(name).first() {
            Some(Value::Str(s)) => s,
            _ => "",
        }
    }

    pub fn get_bytes(&self, name: &str) -> &[u8] {
        match self.values(name).first() {
            Some(Value::Bytes(b)) => b,
            Some(Value::Str(s)) => s.as_bytes(),
            _ => &[],
        }
    }

    pub fn get_msg(&self, name: &str) -> Option<&Msg> {
        match self.values(name).first() {
            Some(Value::Msg(m)) => Some(m),
            _ => None,
        }
    }

    pub fn get_msgs(&self, name: &str) -> Vec<&Msg> {
        self.values(name)
            .iter()
            .filter_map(|v| if let Value::Msg(m) = v { Some(m) } else { None })
            .collect()
    }

    /// Enum value name of the first value (or the number as a string if unknown).
    pub fn get_enum_name(&self, name: &str) -> String {
        let f = self.field(name);
        let n = self.get_i64(name) as i32;
        match &f.ty {
            FieldType::Enum(en) => registry()
                .enum_desc(en)
                .and_then(|e| e.name_of(n))
                .map(str::to_string)
                .unwrap_or_else(|| n.to_string()),
            _ => n.to_string(),
        }
    }

    /// Name of the oneof member that is set, if any.
    pub fn which_oneof(&self, oneof: &str) -> Option<&'static str> {
        self.desc
            .fields
            .iter()
            .find(|f| f.oneof.as_deref() == Some(oneof) && self.fields.contains_key(&f.number))
            .map(|f| f.name.as_str())
    }

    // ----------------------------------------------------------------------------------
    // Binary encoding
    // ----------------------------------------------------------------------------------

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.encode(&mut buf);
        buf
    }

    pub fn encode(&self, buf: &mut Vec<u8>) {
        for f in &self.desc.fields {
            let Some(vals) = self.fields.get(&f.number) else {
                continue;
            };
            if f.is_repeated() {
                if f.packed && f.map.is_none() {
                    if vals.is_empty() {
                        continue;
                    }
                    let mut inner = Vec::new();
                    for v in vals {
                        encode_scalar_value(&f.ty, v, &mut inner);
                    }
                    wire::encode_tag(f.number, wire::WIRE_TYPE_LENGTH_DELIMITED, buf);
                    wire::encode_varint(inner.len() as u64, buf);
                    buf.extend_from_slice(&inner);
                } else {
                    for v in vals {
                        encode_field_value(f, v, buf);
                    }
                }
            } else if let Some(v) = vals.first() {
                if !self.desc.has_presence(f) && is_default(&f.ty, v) {
                    continue;
                }
                encode_field_value(f, v, buf);
            }
        }
        buf.extend_from_slice(&self.unknown);
    }

    // ----------------------------------------------------------------------------------
    // Binary decoding
    // ----------------------------------------------------------------------------------

    pub fn decode(full_name: &str, buf: &[u8]) -> Result<Msg, DecodeError> {
        let mut m = Msg::try_new(full_name)
            .ok_or_else(|| DecodeError(format!("unknown message type {full_name}")))?;
        m.merge_from(buf)?;
        Ok(m)
    }

    pub fn merge_from(&mut self, buf: &[u8]) -> Result<(), DecodeError> {
        let mut pos = 0;
        while pos < buf.len() {
            let start = pos;
            let (num, wt) = wire::decode_tag(buf, &mut pos)?;
            let Some(f) = self.desc.field_by_number(num) else {
                wire::skip_field(wt, buf, &mut pos)?;
                self.unknown.extend_from_slice(&buf[start..pos]);
                continue;
            };
            if let Some((kt, vt)) = &f.map {
                if wt != wire::WIRE_TYPE_LENGTH_DELIMITED {
                    return Err(DecodeError(format!("bad wire type for map {}", f.name)));
                }
                let entry = wire::decode_bytes(buf, &mut pos)?;
                let mut ep = 0;
                let mut k = default_value(kt);
                let mut v = default_value(vt);
                while ep < entry.len() {
                    let (en, ew) = wire::decode_tag(entry, &mut ep)?;
                    match en {
                        1 => k = decode_value(kt, ew, entry, &mut ep)?,
                        2 => v = decode_value(vt, ew, entry, &mut ep)?,
                        _ => wire::skip_field(ew, entry, &mut ep)?,
                    }
                }
                self.fields
                    .entry(f.number)
                    .or_default()
                    .push(Value::MapEntry(Box::new(k), Box::new(v)));
                continue;
            }
            let expected = wire_type_of(&f.ty);
            if f.is_repeated() && wt == wire::WIRE_TYPE_LENGTH_DELIMITED && f.ty.is_packable() {
                let chunk = wire::decode_bytes(buf, &mut pos)?;
                let mut cp = 0;
                let list = self.fields.entry(f.number).or_default();
                while cp < chunk.len() {
                    list.push(decode_value(&f.ty, expected, chunk, &mut cp)?);
                }
            } else if wt == expected {
                let v = decode_value(&f.ty, wt, buf, &mut pos)?;
                if f.is_repeated() {
                    self.fields.entry(f.number).or_default().push(v);
                } else if let (Value::Msg(new), Some(Value::Msg(old))) =
                    (&v, self.fields.get_mut(&f.number).and_then(|l| l.first_mut()))
                {
                    // Singular message fields merge.
                    let mut merged = old.clone();
                    merged.merge_from(&new.encode_to_vec())?;
                    *old = merged;
                } else {
                    self.put(f, v);
                }
            } else {
                wire::skip_field(wt, buf, &mut pos)?;
                self.unknown.extend_from_slice(&buf[start..pos]);
            }
        }
        Ok(())
    }

    // ----------------------------------------------------------------------------------
    // JSON (protobuf JsonFormat)
    // ----------------------------------------------------------------------------------

    /// Compact JSON (`JsonFormat.printer().omittingInsignificantWhitespace()`).
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        self.write_json(&mut out, None, 0);
        out
    }

    /// Pretty JSON (`JsonFormat.printer()`), two-space indentation.
    pub fn to_json_pretty(&self) -> String {
        let mut out = String::new();
        self.write_json(&mut out, Some(2), 0);
        out
    }

    fn write_json(&self, out: &mut String, indent: Option<usize>, depth: usize) {
        match self.desc.full_name.as_str() {
            "google.protobuf.Timestamp" => {
                let (s, n) = (self.get_i64("seconds"), self.get_i64("nanos") as i32);
                out.push('"');
                out.push_str(&format_timestamp(s, n));
                out.push('"');
                return;
            }
            "google.protobuf.Duration" => {
                let (s, n) = (self.get_i64("seconds"), self.get_i64("nanos") as i32);
                out.push('"');
                out.push_str(&format_duration(s, n));
                out.push('"');
                return;
            }
            _ => {}
        }
        let mut items: Vec<(&str, String)> = Vec::new();
        for f in &self.desc.fields {
            let Some(vals) = self.fields.get(&f.number) else {
                continue;
            };
            if f.is_repeated() {
                if vals.is_empty() {
                    continue;
                }
                let mut s = String::new();
                if f.map.is_some() {
                    s.push('{');
                    let mut first = true;
                    for v in vals {
                        let Value::MapEntry(k, val) = v else { continue };
                        if !first {
                            s.push(',');
                        }
                        first = false;
                        nl(&mut s, indent, depth + 2);
                        let (_, vt) = f.map.as_ref().unwrap();
                        let mut ks = String::new();
                        json_key(k, &mut ks);
                        json_str(&ks, &mut s);
                        s.push(':');
                        if indent.is_some() {
                            s.push(' ');
                        }
                        json_value(vt, val, &mut s, indent, depth + 2);
                    }
                    nl(&mut s, indent, depth + 1);
                    s.push('}');
                } else {
                    s.push('[');
                    let mut first = true;
                    for v in vals {
                        if !first {
                            s.push(',');
                        }
                        first = false;
                        nl(&mut s, indent, depth + 2);
                        json_value(&f.ty, v, &mut s, indent, depth + 2);
                    }
                    nl(&mut s, indent, depth + 1);
                    s.push(']');
                }
                items.push((&f.json_name, s));
            } else if let Some(v) = vals.first() {
                if !self.desc.has_presence(f) && is_default(&f.ty, v) {
                    continue;
                }
                let mut s = String::new();
                json_value(&f.ty, v, &mut s, indent, depth + 1);
                items.push((&f.json_name, s));
            }
        }
        if items.is_empty() {
            out.push_str("{}");
            return;
        }
        out.push('{');
        for (i, (k, v)) in items.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            nl(out, indent, depth + 1);
            json_str(k, out);
            out.push(':');
            if indent.is_some() {
                out.push(' ');
            }
            out.push_str(v);
        }
        nl(out, indent, depth);
        out.push('}');
    }

    // ----------------------------------------------------------------------------------
    // Text format (TextFormat.printer())
    // ----------------------------------------------------------------------------------

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        self.write_text(&mut out, 0);
        out
    }

    fn write_text(&self, out: &mut String, depth: usize) {
        let pad = "  ".repeat(depth);
        for f in &self.desc.fields {
            let Some(vals) = self.fields.get(&f.number) else {
                continue;
            };
            if !f.is_repeated() {
                if let Some(v) = vals.first() {
                    if !self.desc.has_presence(f) && is_default(&f.ty, v) {
                        continue;
                    }
                }
            }
            for v in vals {
                match v {
                    Value::MapEntry(k, val) => {
                        let (kt, vt) = f.map.as_ref().unwrap();
                        out.push_str(&format!("{pad}{} {{\n", f.name));
                        text_field("key", kt, k, out, depth + 1);
                        text_field("value", vt, val, out, depth + 1);
                        out.push_str(&format!("{pad}}}\n"));
                    }
                    _ => text_field(&f.name, &f.ty, v, out, depth),
                }
            }
        }
    }
}

fn nl(out: &mut String, indent: Option<usize>, depth: usize) {
    if let Some(n) = indent {
        out.push('\n');
        out.push_str(&" ".repeat(n * depth));
    }
}

fn json_key(v: &Value, out: &mut String) {
    match v {
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::UInt(u) => out.push_str(&u.to_string()),
        Value::Bool(b) => out.push_str(&b.to_string()),
        Value::Str(s) => out.push_str(s),
        Value::Enum(e) => out.push_str(&e.to_string()),
        _ => {}
    }
}

fn json_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            // JsonFormat escapes <, >, & and ' ? No: it only escapes control chars + U+2028/9.
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn json_float(f: f64, is32: bool, out: &mut String) {
    if f.is_nan() {
        out.push_str("\"NaN\"");
    } else if f.is_infinite() {
        out.push_str(if f > 0.0 { "\"Infinity\"" } else { "\"-Infinity\"" });
    } else if is32 {
        out.push_str(&fmt_java_float32(f as f32));
    } else {
        out.push_str(&fmt_java_double(f));
    }
}

fn fmt_java_double(f: f64) -> String {
    if f == f.trunc() && f.abs() < 1e7 {
        format!("{f:.1}")
    } else {
        let s = format!("{f}");
        s
    }
}

fn fmt_java_float32(f: f32) -> String {
    if f == f.trunc() && f.abs() < 1e7 {
        format!("{f:.1}")
    } else {
        format!("{f}")
    }
}

fn json_value(ty: &FieldType, v: &Value, out: &mut String, indent: Option<usize>, depth: usize) {
    match (ty, v) {
        (_, Value::Msg(m)) => m.write_json(out, indent, depth - 1),
        (FieldType::Enum(en), Value::Enum(_) | Value::Int(_)) => {
            let n = match v {
                Value::Enum(e) => *e,
                Value::Int(i) => *i as i32,
                _ => 0,
            };
            match registry().enum_desc(en).and_then(|e| e.name_of(n)) {
                Some(name) => json_str(name, out),
                None => out.push_str(&n.to_string()),
            }
        }
        (t, Value::Int(i)) => {
            if t.is_64bit_int() {
                out.push_str(&format!("\"{i}\""));
            } else {
                out.push_str(&i.to_string());
            }
        }
        (t, Value::UInt(u)) => {
            if t.is_64bit_int() {
                out.push_str(&format!("\"{u}\""));
            } else {
                out.push_str(&u.to_string());
            }
        }
        (_, Value::Enum(n)) => out.push_str(&n.to_string()),
        (_, Value::Bool(b)) => out.push_str(if *b { "true" } else { "false" }),
        (_, Value::Float(f)) => json_float(*f as f64, true, out),
        (_, Value::Double(f)) => json_float(*f, false, out),
        (_, Value::Str(s)) => json_str(s, out),
        (_, Value::Bytes(b)) => {
            out.push('"');
            out.push_str(&base64_encode(b));
            out.push('"');
        }
        (_, Value::MapEntry(..)) => {}
    }
}

fn text_escape(bytes: &[u8]) -> String {
    let mut s = String::new();
    for &b in bytes {
        match b {
            b'\n' => s.push_str("\\n"),
            b'\r' => s.push_str("\\r"),
            b'\t' => s.push_str("\\t"),
            b'"' => s.push_str("\\\""),
            b'\'' => s.push_str("\\'"),
            b'\\' => s.push_str("\\\\"),
            0x20..=0x7e => s.push(b as char),
            _ => s.push_str(&format!("\\{:03o}", b)),
        }
    }
    s
}

fn text_field(name: &str, ty: &FieldType, v: &Value, out: &mut String, depth: usize) {
    let pad = "  ".repeat(depth);
    match v {
        Value::Msg(m) => {
            out.push_str(&format!("{pad}{name} {{\n"));
            m.write_text(out, depth + 1);
            out.push_str(&format!("{pad}}}\n"));
        }
        Value::Str(s) => out.push_str(&format!("{pad}{name}: \"{}\"\n", text_escape(s.as_bytes()))),
        Value::Bytes(b) => out.push_str(&format!("{pad}{name}: \"{}\"\n", text_escape(b))),
        Value::Bool(b) => out.push_str(&format!("{pad}{name}: {b}\n")),
        Value::Int(i) => out.push_str(&format!("{pad}{name}: {i}\n")),
        Value::UInt(u) => out.push_str(&format!("{pad}{name}: {u}\n")),
        Value::Float(f) => out.push_str(&format!("{pad}{name}: {}\n", fmt_java_float32(*f))),
        Value::Double(f) => out.push_str(&format!("{pad}{name}: {}\n", fmt_java_double(*f))),
        Value::Enum(n) => {
            let label = match ty {
                FieldType::Enum(en) => registry()
                    .enum_desc(en)
                    .and_then(|e| e.name_of(*n))
                    .map(str::to_string)
                    .unwrap_or_else(|| n.to_string()),
                _ => n.to_string(),
            };
            out.push_str(&format!("{pad}{name}: {label}\n"));
        }
        Value::MapEntry(..) => {}
    }
}

fn is_default(ty: &FieldType, v: &Value) -> bool {
    let _ = ty;
    match v {
        Value::Int(i) => *i == 0,
        Value::UInt(u) => *u == 0,
        Value::Bool(b) => !*b,
        Value::Float(f) => *f == 0.0 && f.is_sign_positive(),
        Value::Double(f) => *f == 0.0 && f.is_sign_positive(),
        Value::Str(s) => s.is_empty(),
        Value::Bytes(b) => b.is_empty(),
        Value::Enum(e) => *e == 0,
        Value::Msg(_) | Value::MapEntry(..) => false,
    }
}

fn default_value(ty: &FieldType) -> Value {
    match ty {
        FieldType::Double => Value::Double(0.0),
        FieldType::Float => Value::Float(0.0),
        FieldType::Bool => Value::Bool(false),
        FieldType::String => Value::Str(String::new()),
        FieldType::Bytes => Value::Bytes(Vec::new()),
        FieldType::Enum(_) => Value::Enum(0),
        FieldType::Message(n) => Msg::try_new(n).map(Value::Msg).unwrap_or(Value::Int(0)),
        t if t.is_unsigned() => Value::UInt(0),
        _ => Value::Int(0),
    }
}

fn wire_type_of(ty: &FieldType) -> u32 {
    match ty {
        FieldType::Double | FieldType::Fixed64 | FieldType::SFixed64 => wire::WIRE_TYPE_64BIT,
        FieldType::Float | FieldType::Fixed32 | FieldType::SFixed32 => wire::WIRE_TYPE_32BIT,
        FieldType::String | FieldType::Bytes | FieldType::Message(_) => {
            wire::WIRE_TYPE_LENGTH_DELIMITED
        }
        _ => wire::WIRE_TYPE_VARINT,
    }
}

fn zigzag32(n: i32) -> u64 {
    ((n << 1) ^ (n >> 31)) as u32 as u64
}

fn zigzag64(n: i64) -> u64 {
    ((n << 1) ^ (n >> 63)) as u64
}

fn as_i64(v: &Value) -> i64 {
    match v {
        Value::Int(i) => *i,
        Value::UInt(u) => *u as i64,
        Value::Enum(e) => *e as i64,
        Value::Bool(b) => *b as i64,
        _ => 0,
    }
}

fn encode_scalar_value(ty: &FieldType, v: &Value, buf: &mut Vec<u8>) {
    match ty {
        FieldType::Double => {
            let f = if let Value::Double(f) = v { *f } else { 0.0 };
            buf.extend_from_slice(&f.to_le_bytes());
        }
        FieldType::Float => {
            let f = if let Value::Float(f) = v { *f } else { 0.0 };
            buf.extend_from_slice(&f.to_le_bytes());
        }
        FieldType::Fixed32 | FieldType::SFixed32 => {
            buf.extend_from_slice(&(as_i64(v) as u32).to_le_bytes())
        }
        FieldType::Fixed64 | FieldType::SFixed64 => {
            buf.extend_from_slice(&(as_i64(v) as u64).to_le_bytes())
        }
        FieldType::SInt32 => wire::encode_varint(zigzag32(as_i64(v) as i32), buf),
        FieldType::SInt64 => wire::encode_varint(zigzag64(as_i64(v)), buf),
        FieldType::Int32 | FieldType::Enum(_) => {
            wire::encode_varint(as_i64(v) as i32 as i64 as u64, buf)
        }
        FieldType::UInt32 => wire::encode_varint(as_i64(v) as u32 as u64, buf),
        FieldType::Bool => wire::encode_varint(matches!(v, Value::Bool(true)) as u64, buf),
        FieldType::String => {
            if let Value::Str(s) = v {
                wire::encode_varint(s.len() as u64, buf);
                buf.extend_from_slice(s.as_bytes());
            }
        }
        FieldType::Bytes => {
            if let Value::Bytes(b) = v {
                wire::encode_varint(b.len() as u64, buf);
                buf.extend_from_slice(b);
            }
        }
        FieldType::Message(_) => {
            if let Value::Msg(m) = v {
                let inner = m.encode_to_vec();
                wire::encode_varint(inner.len() as u64, buf);
                buf.extend_from_slice(&inner);
            }
        }
        _ => wire::encode_varint(as_i64(v) as u64, buf),
    }
}

fn encode_field_value(f: &FieldDesc, v: &Value, buf: &mut Vec<u8>) {
    if let (Some((kt, vt)), Value::MapEntry(k, val)) = (&f.map, v) {
        let mut inner = Vec::new();
        wire::encode_tag(1, wire_type_of(kt), &mut inner);
        encode_scalar_value(kt, k, &mut inner);
        wire::encode_tag(2, wire_type_of(vt), &mut inner);
        encode_scalar_value(vt, val, &mut inner);
        wire::encode_tag(f.number, wire::WIRE_TYPE_LENGTH_DELIMITED, buf);
        wire::encode_varint(inner.len() as u64, buf);
        buf.extend_from_slice(&inner);
        return;
    }
    wire::encode_tag(f.number, wire_type_of(&f.ty), buf);
    encode_scalar_value(&f.ty, v, buf);
}

fn decode_value(ty: &FieldType, wt: u32, buf: &[u8], pos: &mut usize) -> Result<Value, DecodeError> {
    let need = |n: usize, pos: &usize| -> Result<(), DecodeError> {
        if *pos + n > buf.len() {
            Err(DecodeError("unexpected EOF".into()))
        } else {
            Ok(())
        }
    };
    if wt != wire_type_of(ty) {
        return Err(DecodeError(format!("wire type {wt} mismatch for {ty:?}")));
    }
    Ok(match ty {
        FieldType::Double => {
            need(8, pos)?;
            let v = f64::from_le_bytes(buf[*pos..*pos + 8].try_into().unwrap());
            *pos += 8;
            Value::Double(v)
        }
        FieldType::Float => {
            need(4, pos)?;
            let v = f32::from_le_bytes(buf[*pos..*pos + 4].try_into().unwrap());
            *pos += 4;
            Value::Float(v)
        }
        FieldType::Fixed32 => {
            need(4, pos)?;
            let v = u32::from_le_bytes(buf[*pos..*pos + 4].try_into().unwrap());
            *pos += 4;
            Value::UInt(v as u64)
        }
        FieldType::SFixed32 => {
            need(4, pos)?;
            let v = i32::from_le_bytes(buf[*pos..*pos + 4].try_into().unwrap());
            *pos += 4;
            Value::Int(v as i64)
        }
        FieldType::Fixed64 => {
            need(8, pos)?;
            let v = u64::from_le_bytes(buf[*pos..*pos + 8].try_into().unwrap());
            *pos += 8;
            Value::UInt(v)
        }
        FieldType::SFixed64 => {
            need(8, pos)?;
            let v = i64::from_le_bytes(buf[*pos..*pos + 8].try_into().unwrap());
            *pos += 8;
            Value::Int(v)
        }
        FieldType::Int32 => Value::Int(wire::decode_varint(buf, pos)? as i32 as i64),
        FieldType::Int64 => Value::Int(wire::decode_varint(buf, pos)? as i64),
        FieldType::UInt32 => Value::UInt(wire::decode_varint(buf, pos)? as u32 as u64),
        FieldType::UInt64 => Value::UInt(wire::decode_varint(buf, pos)?),
        FieldType::SInt32 => {
            let n = wire::decode_varint(buf, pos)? as u32;
            Value::Int(((n >> 1) as i32 ^ -((n & 1) as i32)) as i64)
        }
        FieldType::SInt64 => {
            let n = wire::decode_varint(buf, pos)?;
            Value::Int(((n >> 1) as i64) ^ -((n & 1) as i64))
        }
        FieldType::Bool => Value::Bool(wire::decode_varint(buf, pos)? != 0),
        FieldType::Enum(_) => Value::Enum(wire::decode_varint(buf, pos)? as i32),
        FieldType::String => Value::Str(wire::decode_string(buf, pos)?),
        FieldType::Bytes => Value::Bytes(wire::decode_bytes(buf, pos)?.to_vec()),
        FieldType::Message(n) => {
            let b = wire::decode_bytes(buf, pos)?;
            Value::Msg(Msg::decode(n, b)?)
        }
    })
}

/// Standard base64 with padding (as used by `JsonFormat` for `bytes`).
pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn fraction(nanos: u32) -> String {
    if nanos == 0 {
        String::new()
    } else if nanos % 1_000_000 == 0 {
        format!(".{:03}", nanos / 1_000_000)
    } else if nanos % 1_000 == 0 {
        format!(".{:06}", nanos / 1_000)
    } else {
        format!(".{:09}", nanos)
    }
}

/// Formats a `google.protobuf.Timestamp` as RFC 3339 UTC like `Timestamps.toString`.
pub fn format_timestamp(secs: i64, nanos: i32) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}{}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        fraction(nanos.max(0) as u32)
    )
}

/// Formats a `google.protobuf.Duration` like `Durations.toString` (`"1.500s"`).
pub fn format_duration(secs: i64, nanos: i32) -> String {
    let neg = secs < 0 || nanos < 0;
    format!(
        "{}{}{}s",
        if neg { "-" } else { "" },
        secs.unsigned_abs(),
        fraction(nanos.unsigned_abs())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_formats() {
        let mut m = Msg::new("failure_details.FailureDetail");
        m.set_str("message", "boom");
        let mut i = m.new_child("interrupted");
        i.set_enum("code", "INTERRUPTED");
        m.set_msg("interrupted", i);
        let bytes = m.encode_to_vec();
        // field 1 string, field 2 (interrupted) message { code=1 }
        assert_eq!(bytes, vec![0x0a, 4, b'b', b'o', b'o', b'm', 0xaa, 0x06, 2, 0x08, 28]);
        let d = Msg::decode("failure_details.FailureDetail", &bytes).unwrap();
        assert_eq!(d, m);
        assert_eq!(m.to_json(), r#"{"message":"boom","interrupted":{"code":"INTERRUPTED"}}"#);
        assert_eq!(
            m.to_text(),
            "message: \"boom\"\ninterrupted {\n  code: INTERRUPTED\n}\n"
        );
    }

    #[test]
    fn timestamp_and_int64_json() {
        assert_eq!(format_timestamp(0, 0), "1970-01-01T00:00:00Z");
        assert_eq!(format_timestamp(1_700_000_000, 123_000_000), "2023-11-14T22:13:20.123Z");
        assert_eq!(format_duration(1, 500_000_000), "1.500s");
        let mut m = Msg::new("build_event_stream.BuildEventId");
        m.set_msg("started", Msg::new("build_event_stream.BuildEventId.BuildStartedId"));
        assert_eq!(m.to_json(), r#"{"started":{}}"#);
    }

    #[test]
    fn oneof_exclusive_and_unknown_preserved() {
        let mut m = Msg::new("build_event_stream.BuildEventId");
        m.set_empty("started");
        m.set_empty("progress");
        assert_eq!(m.which_oneof("id"), Some("progress"));
        let mut raw = m.encode_to_vec();
        raw.extend_from_slice(&[0xf8, 0x7f, 0x01]); // unknown field 2047 varint
        let d = Msg::decode("build_event_stream.BuildEventId", &raw).unwrap();
        assert_eq!(d.encode_to_vec(), raw);
    }
}
