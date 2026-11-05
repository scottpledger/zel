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

//! `--credential_helper` support: the Bazel credential-helper protocol (`<helper> get`, request
//! JSON `{"uri": ...}` on stdin, response JSON `{"expires": ..., "headers": {name: [values]}}`
//! on stdout). OAuth via `--google_credentials` is not supported (it needs TLS).

use crate::hpack::Header;
use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(BTreeMap<String, Json>),
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn expect(&mut self, c: u8) -> Result<(), String> {
        self.ws();
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at offset {}", c as char, self.i))
        }
    }
    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        match self.b.get(self.i).copied() {
            Some(b'{') => {
                self.i += 1;
                let mut m = BTreeMap::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Json::Obj(m));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.expect(b':')?;
                    m.insert(k, self.value()?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Json::Obj(m));
                        }
                        _ => return Err("expected ',' or '}'".into()),
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
                    v.push(self.value()?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Json::Arr(v));
                        }
                        _ => return Err("expected ',' or ']'".into()),
                    }
                }
            }
            Some(b'"') => Ok(Json::Str(self.string()?)),
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
            Some(c) if c == b'-' || c.is_ascii_digit() => {
                let s = self.i;
                while self.i < self.b.len() && (self.b[self.i] == b'-' || self.b[self.i] == b'+' || self.b[self.i] == b'.' || self.b[self.i] == b'e' || self.b[self.i] == b'E' || self.b[self.i].is_ascii_digit()) {
                    self.i += 1;
                }
                std::str::from_utf8(&self.b[s..self.i]).unwrap().parse().map(Json::Num).map_err(|_| "bad number".to_string())
            }
            _ => Err(format!("unexpected token at offset {}", self.i)),
        }
    }
    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = Vec::new();
        while let Some(&c) = self.b.get(self.i) {
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).map_err(|e| e.to_string()),
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or("bad escape")?;
                    self.i += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'/' | b'\\' | b'"' => out.push(e),
                        b'u' => {
                            let h = std::str::from_utf8(self.b.get(self.i..self.i + 4).ok_or("bad \\u")?).map_err(|e| e.to_string())?;
                            let cp = u32::from_str_radix(h, 16).map_err(|e| e.to_string())?;
                            self.i += 4;
                            let ch = char::from_u32(cp).unwrap_or('\u{fffd}');
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => return Err("bad escape".into()),
                    }
                }
                _ => out.push(c),
            }
        }
        Err("unterminated string".into())
    }
}

fn json_escape(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

/// Parses a credential-helper response into request headers.
pub fn parse_helper_response(out: &str) -> Result<Vec<Header>, String> {
    let v = P { b: out.as_bytes(), i: 0 }.value()?;
    let Json::Obj(m) = v else { return Err("credential helper output is not a JSON object".into()) };
    let mut headers = Vec::new();
    match m.get("headers") {
        Some(Json::Obj(h)) => {
            for (k, v) in h {
                match v {
                    Json::Arr(vals) => {
                        let mut joined = Vec::new();
                        for x in vals {
                            if let Json::Str(s) = x {
                                joined.push(s.clone());
                            } else {
                                return Err(format!("header {k}: values must be strings"));
                            }
                        }
                        headers.push((k.to_ascii_lowercase(), joined.join(",")));
                    }
                    _ => return Err(format!("header {k}: expected an array of strings")),
                }
            }
        }
        _ => return Err("credential helper output has no \"headers\" object".into()),
    }
    Ok(headers)
}

/// `--credential_helper=[scope=]path`: whether the helper applies to `uri`'s host.
pub fn helper_applies(spec: &str, uri: &str) -> Option<String> {
    let host = uri
        .split("://")
        .last()
        .unwrap_or(uri)
        .split('/')
        .next()
        .unwrap_or("")
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or_else(|| uri.split("://").last().unwrap_or(uri).split('/').next().unwrap_or(""));
    match spec.split_once('=') {
        Some((scope, path)) => {
            let ok = match scope.strip_prefix("*.") {
                Some(suffix) => host.ends_with(&format!(".{suffix}")),
                None => scope == host,
            };
            ok.then(|| path.to_string())
        }
        None => Some(spec.to_string()),
    }
}

/// Runs the helper (`<path> get`) for `uri` and returns the headers to attach.
pub fn run_credential_helper(spec: &str, uri: &str) -> Result<Vec<Header>, String> {
    let Some(path) = helper_applies(spec, uri) else { return Ok(vec![]) };
    let mut child = Command::new(&path)
        .arg("get")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run credential helper '{path}': {e}"))?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{{\"uri\":\"{}\"}}", json_escape(uri)).as_bytes())
        .map_err(|e| format!("credential helper '{path}': {e}"))?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "Credential helper '{path}' failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    parse_helper_response(&String::from_utf8_lossy(&out.stdout))
        .map_err(|e| format!("Credential helper '{path}' returned invalid output: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_helper_output() {
        let h = parse_helper_response(
            r#"{"expires":"2030-01-01T00:00:00Z","headers":{"Authorization":["Bearer abc"],"X-Multi":["a","b"]}}"#,
        )
        .unwrap();
        assert!(h.contains(&("authorization".to_string(), "Bearer abc".to_string())));
        assert!(h.contains(&("x-multi".to_string(), "a,b".to_string())));
        assert!(parse_helper_response("[]").is_err());
        assert!(parse_helper_response(r#"{"headers":{"a":"s"}}"#).is_err());
        assert_eq!(parse_helper_response(r#"{"headers":{"k":["v\u00e9\n"]}}"#).unwrap()[0].1, "vé\n");
    }

    #[test]
    fn scopes() {
        assert_eq!(helper_applies("/bin/h", "grpc://a.b:1"), Some("/bin/h".into()));
        assert_eq!(helper_applies("*.b=/bin/h", "grpc://a.b:1"), Some("/bin/h".into()));
        assert_eq!(helper_applies("*.b=/bin/h", "grpc://x.c:1"), None);
        assert_eq!(helper_applies("a.b=/bin/h", "grpc://a.b"), Some("/bin/h".into()));
    }

    #[cfg(unix)]
    #[test]
    fn runs_a_real_helper_script() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("zel-cred-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("helper.sh");
        std::fs::write(&script, "#!/bin/sh\nread req\ncase \"$1\" in get) echo '{\"headers\":{\"Authorization\":[\"Bearer from-helper\"]}}';; *) exit 1;; esac\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let h = run_credential_helper(script.to_str().unwrap(), "grpc://h:1").unwrap();
        assert_eq!(h, vec![("authorization".to_string(), "Bearer from-helper".to_string())]);
        assert!(run_credential_helper("/nonexistent/helper", "grpc://h:1").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
