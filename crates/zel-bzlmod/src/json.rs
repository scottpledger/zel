//! Minimal order-preserving JSON model used for the lockfile, registry files and `mod --output=json`.

use std::fmt::Write;
use zel_starlark::Value;

/// Formats a finite `f64` for JSON so integer-valued floats always retain `.0` or an exponent,
/// matching Bazel's `json.encode` / `FactsAdapter` so round-trip decoding stays a float.
pub fn format_json_float(f: f64) -> String {
    let s = format!("{f:?}");
    if !s.contains('.') && !s.contains('e') && !s.contains('E') && f.is_finite() {
        format!("{s}.0")
    } else {
        s
    }
}

pub fn json_to_starlark(j: &Json) -> Value {
    json_to_starlark_ext(j, false)
}

pub fn json_to_starlark_facts(j: &Json) -> Value {
    json_to_starlark_ext(j, true)
}

fn json_to_starlark_ext(j: &Json, freeze_and_sort: bool) -> Value {
    match j {
        Json::Null => Value::None,
        Json::Bool(b) => Value::Bool(*b),
        Json::Num(n) => {
            if !n.contains('.') && !n.contains('e') && !n.contains('E') {
                if let Ok(i) = n.parse::<i64>() {
                    Value::Int(zel_starlark::BigInt::from_i64(i))
                } else if let Ok(bi) = zel_starlark::BigInt::from_str_radix(n, 10) {
                    Value::Int(bi)
                } else if let Ok(f) = n.parse::<f64>() {
                    Value::Float(f)
                } else {
                    Value::String(n.clone())
                }
            } else if let Ok(f) = n.parse::<f64>() {
                Value::Float(f)
            } else {
                Value::String(n.clone())
            }
        }
        Json::Str(s) => Value::String(s.clone()),
        Json::Arr(a) => {
            let items: Vec<Value> = a
                .iter()
                .map(|x| json_to_starlark_ext(x, freeze_and_sort))
                .collect();
            let mut list = zel_starlark::StarlarkList::new(items);
            if freeze_and_sort {
                list.freeze();
            }
            Value::List(std::sync::Arc::new(std::sync::Mutex::new(list)))
        }
        Json::Obj(o) => {
            let mut pairs: Vec<(&String, &Json)> = o.iter().map(|(k, v)| (k, v)).collect();
            if freeze_and_sort {
                pairs.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
            }
            let mut entries = Vec::with_capacity(pairs.len());
            for (k, v) in pairs {
                entries.push((
                    Value::String(k.clone()),
                    json_to_starlark_ext(v, freeze_and_sort),
                ));
            }
            let mut dict = zel_starlark::StarlarkDict::from_entries(entries).unwrap_or_default();
            if freeze_and_sort {
                dict.freeze();
            }
            Value::Dict(std::sync::Arc::new(std::sync::Mutex::new(dict)))
        }
    }
}

/// Validates and encodes a `module_ctx.extension_metadata(facts = ...)` value per `Facts.java`:
/// root must be a dict, all dict keys must be strings, dict keys are sorted in UTF-16 order,
/// container depth is capped at 7, full `BigInt` precision is preserved, and floats must be finite.
pub fn validate_and_encode_facts(v: &Value) -> Result<Json, zel_starlark::StarlarkError> {
    fn walk(v: &Value, depth: usize) -> Result<Json, zel_starlark::StarlarkError> {
        let err = |m: String| Err(zel_starlark::StarlarkError(m));
        match v {
            Value::None => Ok(Json::Null),
            Value::Bool(b) => Ok(Json::Bool(*b)),
            Value::Int(i) => Ok(Json::Num(i.to_string())),
            Value::Float(f) => {
                if !f.is_finite() {
                    return err(format!("cannot encode non-finite float {f:?} in facts"));
                }
                Ok(Json::Num(format_json_float(*f)))
            }
            Value::String(s) => Ok(Json::Str(s.clone())),
            Value::List(l) => {
                if depth >= 7 {
                    return err("Facts nesting depth exceeds maximum of 7".to_string());
                }
                let elems = l.lock().unwrap().elements.clone();
                let mut arr = Vec::with_capacity(elems.len());
                for x in &elems {
                    arr.push(walk(x, depth + 1)?);
                }
                Ok(Json::Arr(arr))
            }
            Value::Tuple(t) => {
                if depth >= 7 {
                    return err("Facts nesting depth exceeds maximum of 7".to_string());
                }
                let mut arr = Vec::with_capacity(t.len());
                for x in t.iter() {
                    arr.push(walk(x, depth + 1)?);
                }
                Ok(Json::Arr(arr))
            }
            Value::Dict(d) => {
                if depth >= 7 {
                    return err("Facts nesting depth exceeds maximum of 7".to_string());
                }
                let entries = d.lock().unwrap().entries.clone();
                let mut obj = Vec::with_capacity(entries.len());
                for (k, val) in &entries {
                    let Value::String(ks) = k else {
                        return err(format!(
                            "Facts dict keys must be strings, got '{}'",
                            k.type_name()
                        ));
                    };
                    obj.push((ks.clone(), walk(val, depth + 1)?));
                }
                obj.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
                Ok(Json::Obj(obj))
            }
            other => err(format!(
                "Facts values must be JSON-like (None, bool, int, float, string, list, tuple, or dict), got '{}'",
                other.type_name()
            )),
        }
    }
    if !matches!(v, Value::Dict(_)) {
        return Err(zel_starlark::StarlarkError(format!(
            "in call to extension_metadata(), parameter 'facts' got value of type '{}', want 'dict or NoneType'",
            v.type_name()
        )));
    }
    walk(v, 0)
}

pub fn starlark_to_json(v: &Value) -> Json {
    match v {
        Value::None => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Int(i) => Json::Num(i.to_string()),
        Value::Float(f) => Json::Num(format_json_float(*f)),
        Value::String(s) => Json::Str(s.clone()),
        Value::List(l) => {
            let guard = l.lock().unwrap();
            let arr = guard.elements.iter().map(starlark_to_json).collect();
            Json::Arr(arr)
        }
        Value::Tuple(t) => {
            let arr = t.iter().map(starlark_to_json).collect();
            Json::Arr(arr)
        }
        Value::Dict(d) => {
            let guard = d.lock().unwrap();
            let mut obj = Vec::new();
            for (k, v) in &guard.entries {
                obj.push((k.to_string(), starlark_to_json(v)));
            }
            Json::Obj(obj)
        }
        Value::Struct(s) => {
            let mut obj = Vec::new();
            for (k, v) in &s.fields {
                obj.push((k.clone(), starlark_to_json(v)));
            }
            Json::Obj(obj)
        }
        other => Json::Str(other.to_string()),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// Number kept as its textual form to preserve integers/floats verbatim.
    Num(String),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn int(i: i64) -> Json {
        Json::Num(i.to_string())
    }
    pub fn str(s: impl Into<String>) -> Json {
        Json::Str(s.into())
    }
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(v) => v.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Num(n) => n.parse().ok(),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_obj(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Obj(o) => Some(o),
            _ => None,
        }
    }

    pub fn parse(s: &str) -> Result<Json, String> {
        let mut p = Parser {
            b: s.as_bytes(),
            i: 0,
        };
        p.ws();
        let v = p.value()?;
        p.ws();
        if p.i != p.b.len() {
            return Err(format!("trailing characters at offset {}", p.i));
        }
        Ok(v)
    }

    /// Compact serialization.
    pub fn to_compact(&self) -> String {
        let mut s = String::new();
        self.write(&mut s, None, 0, false);
        s
    }

    /// Pretty serialization with `indent` spaces, Gson-style (`"k": v`, empty containers as `{}`/`[]`).
    pub fn to_pretty(&self, indent: usize, html_escape: bool) -> String {
        let mut s = String::new();
        self.write(&mut s, Some(indent), 0, html_escape);
        s
    }

    fn write(&self, out: &mut String, indent: Option<usize>, level: usize, html: bool) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Num(n) => out.push_str(n),
            Json::Str(s) => write_json_string(out, s, html),
            Json::Arr(a) => {
                if a.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    newline(out, indent, level + 1);
                    v.write(out, indent, level + 1, html);
                }
                newline(out, indent, level);
                out.push(']');
            }
            Json::Obj(o) => {
                if o.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push('{');
                for (i, (k, v)) in o.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    newline(out, indent, level + 1);
                    write_json_string(out, k, html);
                    out.push(':');
                    if indent.is_some() {
                        out.push(' ');
                    }
                    v.write(out, indent, level + 1, html);
                }
                newline(out, indent, level);
                out.push('}');
            }
        }
    }
}

fn newline(out: &mut String, indent: Option<usize>, level: usize) {
    if let Some(n) = indent {
        out.push('\n');
        for _ in 0..n * level {
            out.push(' ');
        }
    }
}

pub fn write_json_string(out: &mut String, s: &str, html: bool) {
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
            '<' | '>' | '&' | '=' | '\'' if html => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\n' | b'\r' | b'\t') {
            self.i += 1;
        }
    }
    fn err<T>(&self, m: &str) -> Result<T, String> {
        Err(format!("{m} at offset {}", self.i))
    }
    fn value(&mut self) -> Result<Json, String> {
        match self.b.get(self.i) {
            None => self.err("unexpected end of JSON"),
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
                    let k = match self.value()? {
                        Json::Str(s) => s,
                        _ => return self.err("object key must be a string"),
                    };
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return self.err("expected ':'");
                    }
                    self.i += 1;
                    self.ws();
                    let val = self.value()?;
                    v.push((k, val));
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
            Some(b'[') => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Json::Arr(v));
                }
                loop {
                    self.ws();
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
            Some(b'"') => {
                self.i += 1;
                let mut out = Vec::new();
                loop {
                    let Some(&c) = self.b.get(self.i) else {
                        return self.err("unterminated string");
                    };
                    self.i += 1;
                    match c {
                        b'"' => break,
                        b'\\' => {
                            let Some(&e) = self.b.get(self.i) else {
                                return self.err("bad escape");
                            };
                            self.i += 1;
                            match e {
                                b'n' => out.push(b'\n'),
                                b't' => out.push(b'\t'),
                                b'r' => out.push(b'\r'),
                                b'b' => out.push(8),
                                b'f' => out.push(12),
                                b'/' => out.push(b'/'),
                                b'\\' => out.push(b'\\'),
                                b'"' => out.push(b'"'),
                                b'u' => {
                                    let mut cp = self.hex4()?;
                                    if (0xD800..0xDC00).contains(&cp)
                                        && self.b.get(self.i) == Some(&b'\\')
                                        && self.b.get(self.i + 1) == Some(&b'u')
                                    {
                                        self.i += 2;
                                        let lo = self.hex4()?;
                                        cp = 0x10000
                                            + ((cp - 0xD800) << 10)
                                            + (lo.wrapping_sub(0xDC00));
                                    }
                                    let ch = char::from_u32(cp).unwrap_or('\u{fffd}');
                                    let mut buf = [0u8; 4];
                                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                                }
                                _ => return self.err("bad escape"),
                            }
                        }
                        c => out.push(c),
                    }
                }
                Ok(Json::Str(String::from_utf8_lossy(&out).into_owned()))
            }
            Some(b't') if self.b[self.i..].starts_with(b"true") => {
                self.i += 4;
                Ok(Json::Bool(true))
            }
            Some(b'f') if self.b[self.i..].starts_with(b"false") => {
                self.i += 5;
                Ok(Json::Bool(false))
            }
            Some(b'n') if self.b[self.i..].starts_with(b"null") => {
                self.i += 4;
                Ok(Json::Null)
            }
            Some(c) if *c == b'-' || c.is_ascii_digit() => {
                let st = self.i;
                self.i += 1;
                while self.i < self.b.len()
                    && matches!(
                        self.b[self.i],
                        b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-'
                    )
                {
                    self.i += 1;
                }
                Ok(Json::Num(
                    String::from_utf8_lossy(&self.b[st..self.i]).into_owned(),
                ))
            }
            _ => self.err("unexpected character"),
        }
    }
    fn hex4(&mut self) -> Result<u32, String> {
        if self.i + 4 > self.b.len() {
            return self.err("bad \\u escape");
        }
        let s = std::str::from_utf8(&self.b[self.i..self.i + 4]).map_err(|e| e.to_string())?;
        let v = u32::from_str_radix(s, 16).map_err(|e| e.to_string())?;
        self.i += 4;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let src = r#"{"a":[1,2,{"b":"x\ny\u00e9"}],"c":null,"d":true,"e":{}}"#;
        let j = Json::parse(src).unwrap();
        assert_eq!(j.get("c"), Some(&Json::Null));
        assert_eq!(j.get("a").unwrap().as_arr().unwrap().len(), 3);
        assert_eq!(Json::parse(&j.to_compact()).unwrap(), j);
        let p = j.to_pretty(2, false);
        assert!(p.contains("\"e\": {}"));
        assert_eq!(Json::parse(&p).unwrap(), j);
        assert!(Json::parse("{").is_err());
    }
}
