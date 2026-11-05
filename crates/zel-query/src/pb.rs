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

//! A tiny dynamic protobuf message model with the three encodings used by query output:
//! binary wire format, text format (`--output=textproto`) and canonical JSON
//! (`--output=jsonproto`). Messages are assembled field-by-field by the formatters following
//! `build.proto` / `analysis_v2.proto` numbering exactly.

#[derive(Debug, Clone, PartialEq)]
pub enum PbVal {
    Str(String),
    Bytes(Vec<u8>),
    Bool(bool),
    Int32(i32),
    Uint32(u32),
    Int64(i64),
    Uint64(u64),
    /// Enum value (name, number).
    Enum(&'static str, i32),
    Msg(Msg),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Field {
    pub num: u32,
    pub name: &'static str,
    pub val: PbVal,
    /// Whether the schema declares the field `repeated` (JSON prints it as an array even for a
    /// single element).
    pub rep: bool,
}

impl Default for PbVal {
    fn default() -> Self {
        PbVal::Bool(false)
    }
}

/// A message: ordered list of (possibly repeated) set fields.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Msg {
    pub fields: Vec<Field>,
}

impl Msg {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, num: u32, name: &'static str, val: PbVal) -> &mut Self {
        self.fields.push(Field { num, name, val, rep: false });
        self
    }

    /// Adds an element of a repeated field.
    pub fn rep(&mut self, num: u32, name: &'static str, val: PbVal) -> &mut Self {
        self.fields.push(Field { num, name, val, rep: true });
        self
    }

    pub fn rstr(&mut self, num: u32, name: &'static str, v: impl Into<String>) -> &mut Self {
        self.rep(num, name, PbVal::Str(v.into()))
    }

    pub fn rmsg(&mut self, num: u32, name: &'static str, m: Msg) -> &mut Self {
        self.rep(num, name, PbVal::Msg(m))
    }

    pub fn str(&mut self, num: u32, name: &'static str, v: impl Into<String>) -> &mut Self {
        self.add(num, name, PbVal::Str(v.into()))
    }

    pub fn bool(&mut self, num: u32, name: &'static str, v: bool) -> &mut Self {
        self.add(num, name, PbVal::Bool(v))
    }

    pub fn int32(&mut self, num: u32, name: &'static str, v: i32) -> &mut Self {
        self.add(num, name, PbVal::Int32(v))
    }

    pub fn uint32(&mut self, num: u32, name: &'static str, v: u32) -> &mut Self {
        self.add(num, name, PbVal::Uint32(v))
    }

    pub fn int64(&mut self, num: u32, name: &'static str, v: i64) -> &mut Self {
        self.add(num, name, PbVal::Int64(v))
    }

    pub fn en(&mut self, num: u32, name: &'static str, ename: &'static str, v: i32) -> &mut Self {
        self.add(num, name, PbVal::Enum(ename, v))
    }

    pub fn msg(&mut self, num: u32, name: &'static str, m: Msg) -> &mut Self {
        self.add(num, name, PbVal::Msg(m))
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Fields sorted by number (stable, so repeated fields keep their relative order), which is
    /// the order protobuf-java serializes and prints in.
    fn sorted(&self) -> Vec<&Field> {
        let mut v: Vec<&Field> = self.fields.iter().collect();
        v.sort_by_key(|f| f.num);
        v
    }

    // ---- binary -------------------------------------------------------------------------

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    pub fn encode_into(&self, out: &mut Vec<u8>) {
        for f in self.sorted() {
            match &f.val {
                PbVal::Str(s) => {
                    tag(f.num, 2, out);
                    varint(s.len() as u64, out);
                    out.extend_from_slice(s.as_bytes());
                }
                PbVal::Bytes(b) => {
                    tag(f.num, 2, out);
                    varint(b.len() as u64, out);
                    out.extend_from_slice(b);
                }
                PbVal::Bool(b) => {
                    tag(f.num, 0, out);
                    varint(*b as u64, out);
                }
                PbVal::Int32(i) => {
                    tag(f.num, 0, out);
                    varint(*i as i64 as u64, out);
                }
                PbVal::Uint32(i) => {
                    tag(f.num, 0, out);
                    varint(*i as u64, out);
                }
                PbVal::Int64(i) => {
                    tag(f.num, 0, out);
                    varint(*i as u64, out);
                }
                PbVal::Uint64(i) => {
                    tag(f.num, 0, out);
                    varint(*i, out);
                }
                PbVal::Enum(_, n) => {
                    tag(f.num, 0, out);
                    varint(*n as i64 as u64, out);
                }
                PbVal::Msg(m) => {
                    let body = m.encode();
                    tag(f.num, 2, out);
                    varint(body.len() as u64, out);
                    out.extend_from_slice(&body);
                }
            }
        }
    }

    /// Length-delimited encoding (`writeDelimitedTo`).
    pub fn encode_delimited(&self) -> Vec<u8> {
        let body = self.encode();
        let mut out = Vec::new();
        varint(body.len() as u64, &mut out);
        out.extend_from_slice(&body);
        out
    }

    // ---- text format --------------------------------------------------------------------

    /// `TextFormat.printer().printToString`.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        self.text_into(&mut out, 0);
        out
    }

    /// Single-line text (`shortDebugString`).
    pub fn to_text_single_line(&self) -> String {
        let mut out = String::new();
        self.text_single(&mut out);
        out.trim_end().to_string()
    }

    fn text_into(&self, out: &mut String, indent: usize) {
        let pad = "  ".repeat(indent);
        for f in self.sorted() {
            match &f.val {
                PbVal::Msg(m) => {
                    out.push_str(&format!("{pad}{} {{\n", f.name));
                    m.text_into(out, indent + 1);
                    out.push_str(&format!("{pad}}}\n"));
                }
                v => out.push_str(&format!("{pad}{}: {}\n", f.name, scalar_text(v))),
            }
        }
    }

    fn text_single(&self, out: &mut String) {
        for f in self.sorted() {
            match &f.val {
                PbVal::Msg(m) => {
                    out.push_str(&format!("{} {{ ", f.name));
                    m.text_single(out);
                    out.push_str("} ");
                }
                v => out.push_str(&format!("{}: {} ", f.name, scalar_text(v))),
            }
        }
    }

    // ---- JSON ---------------------------------------------------------------------------

    /// `JsonFormat.printer().print` (pretty, 2-space indent, repeated fields as arrays).
    pub fn to_json_pretty(&self) -> String {
        let mut out = String::new();
        self.json_into(&mut out, 0, true);
        out
    }

    /// `JsonFormat.printer().omittingInsignificantWhitespace().print`.
    pub fn to_json_compact(&self) -> String {
        let mut out = String::new();
        self.json_into(&mut out, 0, false);
        out
    }

    fn json_into(&self, out: &mut String, indent: usize, pretty: bool) {
        // Group repeated fields by number preserving order.
        let sorted = self.sorted();
        let mut groups: Vec<(&Field, Vec<&Field>)> = Vec::new();
        for f in sorted {
            match groups.last_mut() {
                Some((first, v)) if first.num == f.num => v.push(f),
                _ => groups.push((f, vec![f])),
            }
        }
        out.push('{');
        let inner = indent + 1;
        let mut first = true;
        for (head, items) in groups {
            if !first {
                out.push(',');
            }
            first = false;
            if pretty {
                out.push('\n');
                out.push_str(&"  ".repeat(inner));
            }
            out.push_str(&json_string(&camel(head.name)));
            out.push(':');
            if pretty {
                out.push(' ');
            }
            let repeated = items.len() > 1 || head.rep;
            if repeated {
                out.push('[');
                for (i, f) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                        if pretty {
                            out.push(' ');
                        }
                    }
                    json_val(&f.val, out, inner, pretty);
                }
                out.push(']');
            } else {
                json_val(&head.val, out, inner, pretty);
            }
        }
        if pretty {
            out.push('\n');
            if !first {
                out.push_str(&"  ".repeat(indent));
            }
        }
        out.push('}');
    }
}

fn json_val(v: &PbVal, out: &mut String, indent: usize, pretty: bool) {
    match v {
        PbVal::Str(s) => out.push_str(&json_string(s)),
        PbVal::Bytes(b) => out.push_str(&format!("\"{}\"", base64(b))),
        PbVal::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        PbVal::Int32(i) => out.push_str(&i.to_string()),
        PbVal::Uint32(i) => out.push_str(&i.to_string()),
        PbVal::Int64(i) => out.push_str(&format!("\"{i}\"")),
        PbVal::Uint64(i) => out.push_str(&format!("\"{i}\"")),
        PbVal::Enum(n, _) => out.push_str(&format!("\"{n}\"")),
        PbVal::Msg(m) => m.json_into(out, indent, pretty),
    }
}

fn camel(name: &str) -> String {
    let mut out = String::new();
    let mut up = false;
    for c in name.chars() {
        if c == '_' {
            up = true;
        } else if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            // `JsonFormat` prints through Gson's HTML-safe escaper.
            '<' | '>' | '&' | '=' | '\'' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in b.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn scalar_text(v: &PbVal) -> String {
    match v {
        PbVal::Str(s) => text_string(s.as_bytes()),
        PbVal::Bytes(b) => text_string(b),
        PbVal::Bool(b) => b.to_string(),
        PbVal::Int32(i) => i.to_string(),
        PbVal::Uint32(i) => i.to_string(),
        PbVal::Int64(i) => i.to_string(),
        PbVal::Uint64(i) => i.to_string(),
        PbVal::Enum(n, _) => n.to_string(),
        PbVal::Msg(_) => unreachable!(),
    }
}

/// `TextFormat.escapeBytes` quoting.
fn text_string(b: &[u8]) -> String {
    let mut out = String::from("\"");
    for &c in b {
        match c {
            7 => out.push_str("\\a"),
            8 => out.push_str("\\b"),
            12 => out.push_str("\\f"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            11 => out.push_str("\\v"),
            b'\\' => out.push_str("\\\\"),
            b'\'' => out.push_str("\\'"),
            b'"' => out.push_str("\\\""),
            0x20..=0x7e => out.push(c as char),
            _ => out.push_str(&format!("\\{:03o}", c)),
        }
    }
    out.push('"');
    out
}

fn varint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn tag(num: u32, wire: u32, out: &mut Vec<u8>) {
    varint(((num << 3) | wire) as u64, out);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_all_formats() {
        let mut inner = Msg::new();
        inner.str(1, "name", "//a:b").str(2, "rule_class", "genrule").rstr(5, "rule_input", "//x").rstr(5, "rule_input", "//y");
        let mut t = Msg::new();
        t.en(1, "type", "RULE", 1).msg(2, "rule", inner);
        assert_eq!(
            t.to_text(),
            "type: RULE\nrule {\n  name: \"//a:b\"\n  rule_class: \"genrule\"\n  rule_input: \"//x\"\n  rule_input: \"//y\"\n}\n"
        );
        assert_eq!(
            t.to_json_compact(),
            "{\"type\":\"RULE\",\"rule\":{\"name\":\"//a:b\",\"ruleClass\":\"genrule\",\"ruleInput\":[\"//x\",\"//y\"]}}"
        );
        assert_eq!(
            t.to_json_pretty(),
            "{\n  \"type\": \"RULE\",\n  \"rule\": {\n    \"name\": \"//a:b\",\n    \"ruleClass\": \"genrule\",\n    \"ruleInput\": [\"//x\", \"//y\"]\n  }\n}"
        );
        let b = t.encode();
        assert_eq!(&b[..2], &[0x08, 0x01]);
        assert_eq!(b[2], 0x12);
    }

    #[test]
    fn text_escaping() {
        let mut m = Msg::new();
        m.str(1, "name", "a\"b\\c\né");
        assert_eq!(m.to_text(), "name: \"a\\\"b\\\\c\\n\\303\\251\"\n");
    }
}
