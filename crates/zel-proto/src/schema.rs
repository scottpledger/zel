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

//! A `.proto` schema parser and descriptor registry.
//!
//! The registry is populated at first use from the Bazel `.proto` sources embedded in this
//! crate (`crates/zel-proto/protos`). Combined with [`crate::dynamic`], this gives
//! field-for-field binary, JSON and text-format codecs for every embedded schema without any
//! code generation or external crates.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;

/// The type of a protobuf field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldType {
    Double,
    Float,
    Int32,
    Int64,
    UInt32,
    UInt64,
    SInt32,
    SInt64,
    Fixed32,
    Fixed64,
    SFixed32,
    SFixed64,
    Bool,
    String,
    Bytes,
    /// Fully-qualified message name (no leading dot). A leading `?` marks an unresolved type.
    Message(String),
    /// Fully-qualified enum name.
    Enum(String),
}

impl FieldType {
    fn scalar(name: &str) -> Option<Self> {
        Some(match name {
            "double" => Self::Double,
            "float" => Self::Float,
            "int32" => Self::Int32,
            "int64" => Self::Int64,
            "uint32" => Self::UInt32,
            "uint64" => Self::UInt64,
            "sint32" => Self::SInt32,
            "sint64" => Self::SInt64,
            "fixed32" => Self::Fixed32,
            "fixed64" => Self::Fixed64,
            "sfixed32" => Self::SFixed32,
            "sfixed64" => Self::SFixed64,
            "bool" => Self::Bool,
            "string" => Self::String,
            "bytes" => Self::Bytes,
            _ => return None,
        })
    }

    /// Whether repeated fields of this type may be packed.
    pub fn is_packable(&self) -> bool {
        !matches!(
            self,
            Self::String | Self::Bytes | Self::Message(_)
        )
    }

    pub fn is_unsigned(&self) -> bool {
        matches!(self, Self::UInt32 | Self::UInt64 | Self::Fixed32 | Self::Fixed64)
    }

    pub fn is_64bit_int(&self) -> bool {
        matches!(
            self,
            Self::Int64 | Self::UInt64 | Self::SInt64 | Self::Fixed64 | Self::SFixed64
        )
    }
}

/// Field cardinality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label {
    Singular,
    Optional,
    Required,
    Repeated,
}

/// A field of a message.
#[derive(Debug, Clone)]
pub struct FieldDesc {
    pub name: String,
    pub json_name: String,
    pub number: u32,
    pub label: Label,
    pub ty: FieldType,
    /// `Some((key, value))` for `map<K, V>` fields (label is `Repeated`).
    pub map: Option<(FieldType, FieldType)>,
    pub oneof: Option<String>,
    pub packed: bool,
}

impl FieldDesc {
    pub fn is_repeated(&self) -> bool {
        self.label == Label::Repeated
    }
}

/// A message descriptor.
#[derive(Debug, Clone)]
pub struct MessageDesc {
    pub full_name: String,
    pub proto3: bool,
    pub fields: Vec<FieldDesc>,
    by_name: HashMap<String, usize>,
    by_number: HashMap<u32, usize>,
}

impl MessageDesc {
    pub fn field_by_name(&self, name: &str) -> Option<&FieldDesc> {
        self.by_name.get(name).map(|&i| &self.fields[i])
    }

    pub fn field_by_number(&self, n: u32) -> Option<&FieldDesc> {
        self.by_number.get(&n).map(|&i| &self.fields[i])
    }

    /// Whether the field tracks presence (so default values are still serialized).
    pub fn has_presence(&self, f: &FieldDesc) -> bool {
        !self.proto3
            || f.label == Label::Optional
            || f.oneof.is_some()
            || matches!(f.ty, FieldType::Message(_))
    }
}

/// An enum descriptor.
#[derive(Debug, Clone)]
pub struct EnumDesc {
    pub full_name: String,
    pub values: Vec<(String, i32)>,
    /// `(metadata) = { exit_code: N }` per value number (failure_details.proto).
    pub exit_codes: BTreeMap<i32, i32>,
}

impl EnumDesc {
    /// Exit code declared in the value's `FailureDetailMetadata`, if any.
    pub fn exit_code_of(&self, n: i32) -> Option<i32> {
        self.exit_codes.get(&n).copied()
    }

    pub fn name_of(&self, n: i32) -> Option<&str> {
        self.values.iter().find(|(_, v)| *v == n).map(|(s, _)| s.as_str())
    }

    pub fn number_of(&self, name: &str) -> Option<i32> {
        self.values.iter().find(|(s, _)| s == name).map(|(_, v)| *v)
    }
}

/// Descriptor registry over every embedded schema.
#[derive(Debug, Default)]
pub struct Registry {
    messages: BTreeMap<String, MessageDesc>,
    enums: BTreeMap<String, EnumDesc>,
}

impl Registry {
    pub fn message(&self, full_name: &str) -> Option<&MessageDesc> {
        self.messages.get(full_name)
    }

    pub fn enum_desc(&self, full_name: &str) -> Option<&EnumDesc> {
        self.enums.get(full_name)
    }

    pub fn message_names(&self) -> impl Iterator<Item = &String> {
        self.messages.keys()
    }

    pub fn enum_names(&self) -> impl Iterator<Item = &String> {
        self.enums.keys()
    }

    /// Parses additional `.proto` source text into this registry and re-links types.
    pub fn from_sources(sources: &[(&str, &str)]) -> Result<Self, String> {
        let mut raws = Vec::new();
        for (name, src) in sources {
            raws.push(parse_file(src).map_err(|e| format!("{name}: {e}"))?);
        }
        Ok(link(raws))
    }
}

/// The global registry of all embedded Bazel protos.
pub fn registry() -> &'static Registry {
    static REG: OnceLock<Registry> = OnceLock::new();
    REG.get_or_init(|| {
        Registry::from_sources(crate::embedded::SOURCES).expect("embedded protos must parse")
    })
}

// ---------------------------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Num(String),
    Sym(char),
}

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let b: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '/' && i + 1 < b.len() && b[i + 1] == '/' {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && i + 1 < b.len() && b[i + 1] == '*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else if c.is_alphabetic() || c == '_' {
            let s = i;
            while i < b.len() && (b[i].is_alphanumeric() || b[i] == '_') {
                i += 1;
            }
            out.push(Tok::Ident(b[s..i].iter().collect()));
        } else if c.is_ascii_digit() || (c == '-' && i + 1 < b.len() && b[i + 1].is_ascii_digit()) {
            let s = i;
            i += 1;
            while i < b.len() && (b[i].is_alphanumeric() || b[i] == '.' || b[i] == '_') {
                i += 1;
            }
            out.push(Tok::Num(b[s..i].iter().collect()));
        } else if c == '"' || c == '\'' {
            let q = c;
            i += 1;
            let mut s = String::new();
            while i < b.len() && b[i] != q {
                if b[i] == '\\' && i + 1 < b.len() {
                    i += 1;
                    s.push(match b[i] {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        o => o,
                    });
                } else {
                    s.push(b[i]);
                }
                i += 1;
            }
            i += 1;
            out.push(Tok::Str(s));
        } else {
            out.push(Tok::Sym(c));
            i += 1;
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Parser (produces unresolved raw descriptors)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Default)]
struct RawFile {
    package: String,
    proto3: bool,
    messages: Vec<RawMessage>,
    enums: Vec<RawEnum>,
}

#[derive(Debug, Default)]
struct RawMessage {
    name: String,
    fields: Vec<RawField>,
    nested: Vec<RawMessage>,
    enums: Vec<RawEnum>,
}

#[derive(Debug)]
struct RawEnum {
    name: String,
    values: Vec<(String, i32)>,
    exit_codes: BTreeMap<i32, i32>,
}

#[derive(Debug)]
struct RawField {
    name: String,
    json_name: Option<String>,
    number: u32,
    label: Label,
    ty: String,
    map: Option<(String, String)>,
    oneof: Option<String>,
    packed: Option<bool>,
}

struct Parser {
    t: Vec<Tok>,
    p: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.p)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.t.get(self.p).cloned();
        self.p += 1;
        t
    }

    fn is_sym(&self, c: char) -> bool {
        matches!(self.peek(), Some(Tok::Sym(x)) if *x == c)
    }

    fn eat_sym(&mut self, c: char) -> bool {
        if self.is_sym(c) {
            self.p += 1;
            true
        } else {
            false
        }
    }

    fn expect_sym(&mut self, c: char) -> Result<(), String> {
        if self.eat_sym(c) {
            Ok(())
        } else {
            Err(format!("expected '{c}' at token {} got {:?}", self.p, self.peek()))
        }
    }

    fn ident(&mut self) -> Result<String, String> {
        match self.next() {
            Some(Tok::Ident(s)) => Ok(s),
            o => Err(format!("expected identifier, got {o:?}")),
        }
    }

    /// Parses `a.b.c` or `.a.b.c`.
    fn dotted(&mut self) -> Result<String, String> {
        let mut s = String::new();
        if self.eat_sym('.') {
            s.push('.');
        }
        s.push_str(&self.ident()?);
        while self.is_sym('.') {
            self.p += 1;
            s.push('.');
            s.push_str(&self.ident()?);
        }
        Ok(s)
    }

    /// Skips tokens up to and including the terminating `;` at depth 0 (or a full `{}` block).
    fn skip_statement(&mut self) {
        let mut depth = 0;
        while let Some(t) = self.next() {
            match t {
                Tok::Sym('{') | Tok::Sym('[') | Tok::Sym('(') => depth += 1,
                Tok::Sym('}') | Tok::Sym(']') | Tok::Sym(')') => depth -= 1,
                Tok::Sym(';') if depth == 0 => return,
                _ => {}
            }
            if depth == 0 && matches!(self.t.get(self.p - 1), Some(Tok::Sym('}'))) {
                // End of a block statement such as `service X { ... }`.
                if !self.is_sym(';') {
                    return;
                }
            }
        }
    }

    fn skip_block(&mut self) -> Result<(), String> {
        self.expect_sym('{')?;
        let mut depth = 1;
        while depth > 0 {
            match self.next() {
                Some(Tok::Sym('{')) => depth += 1,
                Some(Tok::Sym('}')) => depth -= 1,
                None => return Err("unterminated block".into()),
                _ => {}
            }
        }
        Ok(())
    }

    fn parse_int(s: &str) -> Result<i64, String> {
        let (neg, body) = match s.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, s),
        };
        let v = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            i64::from_str_radix(h, 16)
        } else {
            body.parse::<i64>()
        }
        .map_err(|e| format!("bad int {s}: {e}"))?;
        Ok(if neg { -v } else { v })
    }

    fn parse_file(&mut self) -> Result<RawFile, String> {
        let mut f = RawFile::default();
        while let Some(t) = self.peek().cloned() {
            match t {
                Tok::Ident(k) => match k.as_str() {
                    "syntax" => {
                        self.p += 1;
                        self.expect_sym('=')?;
                        if let Some(Tok::Str(s)) = self.next() {
                            f.proto3 = s == "proto3";
                        }
                        self.skip_statement();
                    }
                    "package" => {
                        self.p += 1;
                        f.package = self.dotted()?;
                        self.expect_sym(';')?;
                    }
                    "message" => {
                        self.p += 1;
                        f.messages.push(self.parse_message()?);
                    }
                    "enum" => {
                        self.p += 1;
                        f.enums.push(self.parse_enum()?);
                    }
                    "service" | "extend" => {
                        self.p += 1;
                        while !self.is_sym('{') {
                            self.p += 1;
                        }
                        self.skip_block()?;
                    }
                    _ => {
                        self.p += 1;
                        self.skip_statement();
                    }
                },
                _ => self.p += 1,
            }
        }
        Ok(f)
    }

    fn parse_enum(&mut self) -> Result<RawEnum, String> {
        let name = self.ident()?;
        self.expect_sym('{')?;
        let mut values = Vec::new();
        let mut exit_codes = BTreeMap::new();
        while !self.eat_sym('}') {
            match self.peek().cloned() {
                Some(Tok::Ident(k)) if k == "option" || k == "reserved" => {
                    self.p += 1;
                    self.skip_statement();
                }
                Some(Tok::Ident(n)) => {
                    self.p += 1;
                    self.expect_sym('=')?;
                    let Some(Tok::Num(v)) = self.next() else {
                        return Err(format!("bad enum value for {n}"));
                    };
                    let num = Self::parse_int(&v)? as i32;
                    values.push((n, num));
                    // Scan options up to ';' capturing `exit_code: N`.
                    let mut depth = 0;
                    while let Some(t) = self.next() {
                        match t {
                            Tok::Sym('[') | Tok::Sym('{') | Tok::Sym('(') => depth += 1,
                            Tok::Sym(']') | Tok::Sym('}') | Tok::Sym(')') => depth -= 1,
                            Tok::Sym(';') if depth == 0 => break,
                            Tok::Ident(i) if i == "exit_code" => {
                                self.eat_sym(':');
                                if let Some(Tok::Num(x)) = self.next() {
                                    exit_codes.insert(num, Self::parse_int(&x)? as i32);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Some(Tok::Sym(';')) => self.p += 1,
                o => return Err(format!("unexpected in enum {name}: {o:?}")),
            }
        }
        Ok(RawEnum { name, values, exit_codes })
    }

    fn parse_message(&mut self) -> Result<RawMessage, String> {
        let name = self.ident()?;
        let mut m = RawMessage {
            name,
            ..Default::default()
        };
        self.expect_sym('{')?;
        self.parse_message_body(&mut m, None)?;
        Ok(m)
    }

    fn parse_message_body(&mut self, m: &mut RawMessage, oneof: Option<&str>) -> Result<(), String> {
        while !self.eat_sym('}') {
            let Some(t) = self.peek().cloned() else {
                return Err(format!("unterminated message {}", m.name));
            };
            match t {
                Tok::Sym(';') => self.p += 1,
                Tok::Ident(k) => match k.as_str() {
                    "message" => {
                        self.p += 1;
                        let n = self.parse_message()?;
                        m.nested.push(n);
                    }
                    "enum" => {
                        self.p += 1;
                        let e = self.parse_enum()?;
                        m.enums.push(e);
                    }
                    "oneof" => {
                        self.p += 1;
                        let on = self.ident()?;
                        self.expect_sym('{')?;
                        self.parse_message_body(m, Some(&on))?;
                    }
                    "option" | "reserved" | "extensions" => {
                        self.p += 1;
                        self.skip_statement();
                    }
                    "extend" => {
                        self.p += 1;
                        while !self.is_sym('{') {
                            self.p += 1;
                        }
                        self.skip_block()?;
                    }
                    _ => self.parse_field(m, oneof)?,
                },
                o => return Err(format!("unexpected token in message {}: {o:?}", m.name)),
            }
        }
        Ok(())
    }

    fn parse_field(&mut self, m: &mut RawMessage, oneof: Option<&str>) -> Result<(), String> {
        let mut label = if oneof.is_some() { Label::Optional } else { Label::Singular };
        let first = self.dotted()?;
        let mut ty = first.clone();
        match first.as_str() {
            "optional" => {
                label = Label::Optional;
                ty = self.dotted()?;
            }
            "repeated" => {
                label = Label::Repeated;
                ty = self.dotted()?;
            }
            "required" => {
                label = Label::Required;
                ty = self.dotted()?;
            }
            _ => {}
        }
        let mut map = None;
        if ty == "map" && self.is_sym('<') {
            self.p += 1;
            let k = self.dotted()?;
            self.expect_sym(',')?;
            let v = self.dotted()?;
            self.expect_sym('>')?;
            map = Some((k, v));
            label = Label::Repeated;
        }
        let name = self.ident()?;
        self.expect_sym('=')?;
        let Some(Tok::Num(n)) = self.next() else {
            return Err(format!("expected field number for {name}"));
        };
        let number = Self::parse_int(&n)? as u32;
        let mut json_name = None;
        let mut packed = None;
        if self.eat_sym('[') {
            let mut depth = 1;
            while depth > 0 {
                match self.next() {
                    Some(Tok::Sym('[')) | Some(Tok::Sym('{')) | Some(Tok::Sym('(')) => depth += 1,
                    Some(Tok::Sym(']')) | Some(Tok::Sym('}')) | Some(Tok::Sym(')')) => depth -= 1,
                    Some(Tok::Ident(i)) if depth == 1 && i == "json_name" => {
                        self.expect_sym('=')?;
                        if let Some(Tok::Str(s)) = self.next() {
                            json_name = Some(s);
                        }
                    }
                    Some(Tok::Ident(i)) if depth == 1 && i == "packed" => {
                        self.expect_sym('=')?;
                        if let Some(Tok::Ident(b)) = self.next() {
                            packed = Some(b == "true");
                        }
                    }
                    None => return Err("unterminated field options".into()),
                    _ => {}
                }
            }
        }
        self.expect_sym(';')?;
        m.fields.push(RawField {
            name,
            json_name,
            number,
            label,
            ty,
            map,
            oneof: oneof.map(str::to_string),
            packed,
        });
        Ok(())
    }
}

fn parse_file(src: &str) -> Result<RawFile, String> {
    let toks = lex(src)?;
    Parser { t: toks, p: 0 }.parse_file()
}

// ---------------------------------------------------------------------------------------------
// Linking
// ---------------------------------------------------------------------------------------------

fn json_name_of(name: &str) -> String {
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

struct Collected<'a> {
    msgs: Vec<(String, &'a RawMessage, bool)>,
    enums: Vec<(String, &'a RawEnum)>,
    msg_names: HashSet<String>,
    enum_names: HashSet<String>,
}

fn collect<'a>(prefix: &str, m: &'a RawMessage, proto3: bool, c: &mut Collected<'a>) {
    let full = if prefix.is_empty() {
        m.name.clone()
    } else {
        format!("{prefix}.{}", m.name)
    };
    c.msg_names.insert(full.clone());
    c.msgs.push((full.clone(), m, proto3));
    for e in &m.enums {
        let en = format!("{full}.{}", e.name);
        c.enum_names.insert(en.clone());
        c.enums.push((en, e));
    }
    for n in &m.nested {
        collect(&full, n, proto3, c);
    }
}

fn resolve(scope: &str, name: &str, c: &Collected) -> FieldType {
    if let Some(s) = FieldType::scalar(name) {
        return s;
    }
    let candidates: Vec<String> = if let Some(abs) = name.strip_prefix('.') {
        vec![abs.to_string()]
    } else {
        let mut v = Vec::new();
        let mut sc = scope.to_string();
        loop {
            v.push(if sc.is_empty() { name.to_string() } else { format!("{sc}.{name}") });
            match sc.rfind('.') {
                Some(i) => sc.truncate(i),
                None if !sc.is_empty() => sc.clear(),
                None => break,
            }
        }
        v
    };
    for cand in &candidates {
        if c.msg_names.contains(cand) {
            return FieldType::Message(cand.clone());
        }
        if c.enum_names.contains(cand) {
            return FieldType::Enum(cand.clone());
        }
    }
    FieldType::Message(format!("?{name}"))
}

fn link(files: Vec<RawFile>) -> Registry {
    let mut c = Collected {
        msgs: Vec::new(),
        enums: Vec::new(),
        msg_names: HashSet::new(),
        enum_names: HashSet::new(),
    };
    for f in &files {
        for m in &f.messages {
            collect(&f.package, m, f.proto3, &mut c);
        }
        for e in &f.enums {
            let en = if f.package.is_empty() {
                e.name.clone()
            } else {
                format!("{}.{}", f.package, e.name)
            };
            c.enum_names.insert(en.clone());
            c.enums.push((en, e));
        }
    }
    let mut reg = Registry::default();
    for (full, m, proto3) in &c.msgs {
        let mut fields = Vec::new();
        for f in &m.fields {
            let (ty, map) = match &f.map {
                Some((k, v)) => (
                    FieldType::Message(format!("{full}.{}Entry", f.name)),
                    Some((resolve(full, k, &c), resolve(full, v, &c))),
                ),
                None => (resolve(full, &f.ty, &c), None),
            };
            let packed = match f.packed {
                Some(p) => p,
                None => *proto3 && f.label == Label::Repeated && ty.is_packable() && map.is_none(),
            } && ty.is_packable();
            fields.push(FieldDesc {
                name: f.name.clone(),
                json_name: f.json_name.clone().unwrap_or_else(|| json_name_of(&f.name)),
                number: f.number,
                label: f.label,
                ty,
                map,
                oneof: f.oneof.clone(),
                packed,
            });
        }
        fields.sort_by_key(|f| f.number);
        let by_name = fields.iter().enumerate().map(|(i, f)| (f.name.clone(), i)).collect();
        let by_number = fields.iter().enumerate().map(|(i, f)| (f.number, i)).collect();
        reg.messages.insert(
            full.clone(),
            MessageDesc {
                full_name: full.clone(),
                proto3: *proto3,
                fields,
                by_name,
                by_number,
            },
        );
    }
    for (full, e) in &c.enums {
        reg.enums.insert(
            full.clone(),
            EnumDesc {
                full_name: full.clone(),
                values: e.values.clone(),
                exit_codes: e.exit_codes.clone(),
            },
        );
    }
    reg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_embedded_protos() {
        let r = registry();
        assert!(r.message("build_event_stream.BuildEvent").is_some());
        assert!(r.message("failure_details.FailureDetail").is_some());
        assert!(r.enum_desc("failure_details.Interrupted.Code").is_some());
        let be = r.message("build_event_stream.BuildEvent").unwrap();
        assert!(be.field_by_name("children").unwrap().is_repeated());
        // No unresolved types except those from deliberately omitted dependencies.
        let mut unresolved = Vec::new();
        for (n, m) in &r.messages {
            for f in &m.fields {
                if let FieldType::Message(t) = &f.ty {
                    if t.starts_with('?') {
                        unresolved.push(format!("{n}.{} -> {t}", f.name));
                    }
                }
            }
        }
        eprintln!("unresolved: {unresolved:?}");
        assert!(unresolved.len() < 25, "{unresolved:?}");
    }

    #[test]
    fn nested_scope_resolution() {
        let r = Registry::from_sources(&[(
            "t",
            "syntax=\"proto3\"; package a.b; message X { message Y { int32 q = 1; } Y y = 1; Z z = 2; map<string, Y> m = 3; oneof o { E e = 4; } enum E { A = 0; } } message Z {}",
        )])
        .unwrap();
        let x = r.message("a.b.X").unwrap();
        assert_eq!(x.field_by_name("y").unwrap().ty, FieldType::Message("a.b.X.Y".into()));
        assert_eq!(x.field_by_name("z").unwrap().ty, FieldType::Message("a.b.Z".into()));
        assert_eq!(x.field_by_name("e").unwrap().ty, FieldType::Enum("a.b.X.E".into()));
        assert!(x.field_by_name("m").unwrap().map.is_some());
    }
}

#[cfg(test)]
mod failure_detail_tests {
    use crate::failure_details::*;

    #[test]
    fn all_categories_have_exit_codes() {
        let cats = all_categories();
        assert!(cats.len() >= 64, "got {}", cats.len());
        for (_, name) in &cats {
            let c = Category::named(name, &first_code(name)).expect(name);
            assert!(c.exit_code().is_some(), "{name}");
        }
        let c = Category::named("spawn", "NON_ZERO_EXIT").unwrap();
        assert_eq!(c.exit_code(), Some(1));
        assert_eq!(Category::Interrupted(InterruptedCode::Interrupted).exit_code(), Some(8));
        assert_eq!(Category::Command(CommandCode::AnotherCommandRunning).exit_code(), Some(9));
    }

    fn first_code(cat: &str) -> String {
        let reg = crate::schema::registry();
        let f = reg.message("failure_details.FailureDetail").unwrap().field_by_name(cat).unwrap();
        let crate::schema::FieldType::Message(sub) = &f.ty else { panic!() };
        let cf = reg.message(sub).unwrap().field_by_number(1).unwrap();
        let crate::schema::FieldType::Enum(en) = &cf.ty else { panic!() };
        reg.enum_desc(en).unwrap().values[0].0.clone()
    }
}
