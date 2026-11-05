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

//! Minimal JSON value, parser and string escaping (used by the profiler and BEP tooling).

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// Integers that fit in `i64` keep full precision.
    Int(i64),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    /// Objects preserve insertion order.
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(v) => v.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        if let Json::Str(s) = self { Some(s) } else { None }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Int(i) => Some(*i),
            Json::Num(f) => Some(*f as i64),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Int(i) => Some(*i as f64),
            Json::Num(f) => Some(*f),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Json]> {
        if let Json::Arr(a) = self { Some(a) } else { None }
    }
    pub fn as_object(&self) -> Option<&[(String, Json)]> {
        if let Json::Obj(o) = self { Some(o) } else { None }
    }

    /// Key → value map view of an object.
    pub fn to_map(&self) -> BTreeMap<&str, &Json> {
        self.as_object().map(|o| o.iter().map(|(k, v)| (k.as_str(), v)).collect()).unwrap_or_default()
    }

    /// Compact serialization.
    pub fn to_compact(&self) -> String {
        let mut s = String::new();
        self.write(&mut s);
        s
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Int(i) => out.push_str(&i.to_string()),
            Json::Num(f) => out.push_str(&f.to_string()),
            Json::Str(s) => escape_into(s, out),
            Json::Arr(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Json::Obj(o) => {
                out.push('{');
                for (i, (k, v)) in o.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    escape_into(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// Appends `s` as a quoted JSON string (Gson `JsonWriter` escaping, non-HTML-safe).
pub fn escape_into(s: &str, out: &mut String) {
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
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

pub fn escape(s: &str) -> String {
    let mut o = String::new();
    escape_into(s, &mut o);
    o
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\n' | b'\r' | b'\t') {
            self.i += 1;
        }
    }

    fn err<T>(&self, m: &str) -> Result<T, String> {
        Err(format!("{m} at byte {}", self.i))
    }

    fn lit(&mut self, s: &str, v: Json) -> Result<Json, String> {
        if self.b[self.i..].starts_with(s.as_bytes()) {
            self.i += s.len();
            Ok(v)
        } else {
            self.err("invalid literal")
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        match self.b.get(self.i) {
            None => self.err("unexpected end of JSON"),
            Some(b'n') => self.lit("null", Json::Null),
            Some(b't') => self.lit("true", Json::Bool(true)),
            Some(b'f') => self.lit("false", Json::Bool(false)),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b'[') => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Json::Arr(v));
                }
                loop {
                    v.push(self.value()?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Json::Arr(v));
                        }
                        _ => return self.err("expected ',' or ']'"),
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Json::Obj(v));
                }
                loop {
                    self.ws();
                    if self.b.get(self.i) != Some(&b'"') {
                        return self.err("expected object key");
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return self.err("expected ':'");
                    }
                    self.i += 1;
                    v.push((k, self.value()?));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Json::Obj(v));
                        }
                        _ => return self.err("expected ',' or '}'"),
                    }
                }
            }
            Some(_) => self.number(),
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let s = self.i;
        while self.i < self.b.len() && matches!(self.b[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
            self.i += 1;
        }
        let t = std::str::from_utf8(&self.b[s..self.i]).unwrap_or("");
        if t.is_empty() {
            return self.err("unexpected character");
        }
        if let Ok(i) = t.parse::<i64>() {
            return Ok(Json::Int(i));
        }
        t.parse::<f64>().map(Json::Num).or_else(|_| self.err("bad number"))
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(&c) = self.b.get(self.i) else { return self.err("unterminated string") };
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let Some(&e) = self.b.get(self.i) else { return self.err("bad escape") };
                    self.i += 1;
                    let ch = match e {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'/' => '/',
                        b'\\' => '\\',
                        b'"' => '"',
                        b'u' => {
                            let hex = |p: &Self, at: usize| -> Option<u32> {
                                std::str::from_utf8(p.b.get(at..at + 4)?).ok().and_then(|h| u32::from_str_radix(h, 16).ok())
                            };
                            let Some(mut cp) = hex(self, self.i) else { return self.err("bad \\u escape") };
                            self.i += 4;
                            if (0xD800..0xDC00).contains(&cp) && self.b.get(self.i) == Some(&b'\\') && self.b.get(self.i + 1) == Some(&b'u') {
                                if let Some(lo) = hex(self, self.i + 2) {
                                    if (0xDC00..0xE000).contains(&lo) {
                                        cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                        self.i += 6;
                                    }
                                }
                            }
                            char::from_u32(cp).unwrap_or('\u{fffd}')
                        }
                        _ => return self.err("bad escape"),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c => out.push(c),
            }
        }
        // Bazel writes profiles as ISO-8859-1 bytes; accept valid UTF-8 and fall back to Latin-1.
        Ok(match String::from_utf8(out) {
            Ok(s) => s,
            Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
        })
    }
}

/// Parses a complete JSON document.
pub fn parse(s: &[u8]) -> Result<Json, String> {
    let mut p = P { b: s, i: 0 };
    let v = p.value()?;
    p.ws();
    if p.i != s.len() {
        return p.err("trailing characters");
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_roundtrip() {
        let j = parse(br#"{"a":[1,2.5,"x\n\u00e9\ud83d\ude00"],"b":{"c":null,"d":true},"e":-7}"#).unwrap();
        assert_eq!(j.get("e").unwrap().as_i64(), Some(-7));
        assert_eq!(j.get("a").unwrap().as_array().unwrap()[2].as_str(), Some("x\né😀"));
        assert_eq!(j.get("b").unwrap().get("d"), Some(&Json::Bool(true)));
        let again = parse(j.to_compact().as_bytes()).unwrap();
        assert_eq!(j, again);
        assert!(parse(b"{").is_err());
        assert!(parse(b"[1,]").is_err());
        assert_eq!(escape("a\"b\u{1}"), "\"a\\\"b\\u0001\"");
    }
}
