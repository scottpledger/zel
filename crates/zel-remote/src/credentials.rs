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
                while self.i < self.b.len()
                    && (self.b[self.i] == b'-'
                        || self.b[self.i] == b'+'
                        || self.b[self.i] == b'.'
                        || self.b[self.i] == b'e'
                        || self.b[self.i] == b'E'
                        || self.b[self.i].is_ascii_digit())
                {
                    self.i += 1;
                }
                std::str::from_utf8(&self.b[s..self.i])
                    .unwrap()
                    .parse()
                    .map(Json::Num)
                    .map_err(|_| "bad number".to_string())
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
                            let h = std::str::from_utf8(
                                self.b.get(self.i..self.i + 4).ok_or("bad \\u")?,
                            )
                            .map_err(|e| e.to_string())?;
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

/// Structured credential-helper response (`GetCredentialsResponse`), preserving exact header
/// casing, multi-value header lists (including empty lists and repeated values), and optional
/// RFC 3339 `expires` timestamp (as epoch seconds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetCredentialsResponse {
    pub expires_epoch_secs: Option<i64>,
    pub headers: BTreeMap<String, Vec<String>>,
}

impl GetCredentialsResponse {
    pub fn parse(out: &str) -> Result<Self, String> {
        let mut parser = P {
            b: out.as_bytes(),
            i: 0,
        };
        let v = parser.value()?;
        parser.ws();
        if parser.i != parser.b.len() {
            return Err(format!("trailing characters at offset {}", parser.i));
        }
        let Json::Obj(m) = v else {
            return Err("credential helper output is not a JSON object".into());
        };
        let expires_epoch_secs = match m.get("expires") {
            None | Some(Json::Null) => None,
            Some(Json::Str(s)) => Some(parse_rfc3339_epoch_secs(s)?),
            _ => return Err("\"expires\" must be an RFC 3339 string".into()),
        };
        let mut headers = BTreeMap::new();
        match m.get("headers") {
            None | Some(Json::Null) => {}
            Some(Json::Obj(h)) => {
                for (k, v) in h {
                    match v {
                        Json::Arr(vals) => {
                            let mut list = Vec::with_capacity(vals.len());
                            for x in vals {
                                if let Json::Str(s) = x {
                                    list.push(s.clone());
                                } else {
                                    return Err(format!("header {k}: values must be strings"));
                                }
                            }
                            headers.insert(k.clone(), list);
                        }
                        _ => return Err(format!("header {k}: expected an array of strings")),
                    }
                }
            }
            _ => return Err("\"headers\" must be a JSON object".into()),
        }
        Ok(Self {
            expires_epoch_secs,
            headers,
        })
    }
}

/// Parses an RFC 3339 timestamp (`YYYY-MM-DDTHH:MM:SS[.frac](Z|+HH:MM|-HH:MM)`) into Unix epoch seconds.
pub fn parse_rfc3339_epoch_secs(s: &str) -> Result<i64, String> {
    let t_pos = s
        .find(['T', 't'])
        .ok_or_else(|| format!("invalid RFC 3339 timestamp '{s}'"))?;
    let date = &s[..t_pos];
    let time_and_tz = &s[t_pos + 1..];
    let mut dparts = date.split('-');
    let year: i64 = dparts
        .next()
        .filter(|p| p.len() == 4)
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| format!("invalid year in '{s}'"))?;
    let month: u32 = dparts
        .next()
        .filter(|p| p.len() == 2)
        .and_then(|p| p.parse().ok())
        .filter(|&m| (1..=12).contains(&m))
        .ok_or_else(|| format!("invalid month in '{s}'"))?;
    let day: u32 = dparts
        .next()
        .filter(|p| p.len() == 2)
        .and_then(|p| p.parse().ok())
        .filter(|&d| (1..=31).contains(&d))
        .ok_or_else(|| format!("invalid day in '{s}'"))?;
    if dparts.next().is_some() {
        return Err(format!("invalid date in '{s}'"));
    }
    let (time_part, offset_secs): (&str, i64) = if let Some(tp) = time_and_tz
        .strip_suffix('Z')
        .or_else(|| time_and_tz.strip_suffix('z'))
    {
        (tp, 0)
    } else if let Some(idx) = time_and_tz.rfind(['+', '-']) {
        let sign = if time_and_tz.as_bytes()[idx] == b'+' {
            1i64
        } else {
            -1i64
        };
        let off = &time_and_tz[idx + 1..];
        let (oh, om) = off
            .split_once(':')
            .ok_or_else(|| format!("invalid timezone offset in '{s}'"))?;
        let h: i64 = oh
            .parse()
            .ok()
            .filter(|&h| (0..=23).contains(&h))
            .ok_or_else(|| format!("invalid offset hour in '{s}'"))?;
        let m: i64 = om
            .parse()
            .ok()
            .filter(|&m| (0..=59).contains(&m))
            .ok_or_else(|| format!("invalid offset minute in '{s}'"))?;
        (&time_and_tz[..idx], sign * (h * 3600 + m * 60))
    } else {
        return Err(format!("missing timezone in '{s}'"));
    };
    let main_time = match time_part.split_once('.') {
        Some((hms, frac)) => {
            if frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!("invalid fractional seconds in '{s}'"));
            }
            hms
        }
        None => time_part,
    };
    let mut tparts = main_time.split(':');
    let hour: u32 = tparts
        .next()
        .filter(|p| p.len() == 2)
        .and_then(|p| p.parse().ok())
        .filter(|&h| h <= 23)
        .ok_or_else(|| format!("invalid hour in '{s}'"))?;
    let minute: u32 = tparts
        .next()
        .filter(|p| p.len() == 2)
        .and_then(|p| p.parse().ok())
        .filter(|&m| m <= 59)
        .ok_or_else(|| format!("invalid minute in '{s}'"))?;
    let second: u32 = tparts
        .next()
        .filter(|p| p.len() == 2)
        .and_then(|p| p.parse().ok())
        .filter(|&sec| sec <= 60)
        .ok_or_else(|| format!("invalid second in '{s}'"))?;
    if tparts.next().is_some() {
        return Err(format!("invalid time in '{s}'"));
    }
    // Howard Hinnant civil-from-days / days-from-civil algorithm.
    let y = year - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let m_adj = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * m_adj + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + i64::from(doe) - 719468;
    let utc_secs =
        days * 86400 + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second.min(59))
            - offset_secs;
    Ok(utc_secs)
}

/// Parses a credential-helper response into HTTP/2 request headers.
pub fn parse_helper_response(out: &str) -> Result<Vec<Header>, String> {
    let parsed = GetCredentialsResponse::parse(out)?;
    let mut headers = Vec::new();
    for (k, vals) in parsed.headers {
        if !vals.is_empty() {
            headers.push((k.to_ascii_lowercase(), vals.join(",")));
        }
    }
    Ok(headers)
}

fn extract_uri_host(uri: &str) -> &str {
    let rest = uri.split_once("://").map(|(_, r)| r).unwrap_or(uri);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    if hostport.starts_with('[') {
        hostport
            .split(']')
            .next()
            .map(|s| s.trim_start_matches('['))
            .unwrap_or(hostport)
    } else {
        hostport.split(':').next().unwrap_or(hostport)
    }
}

/// Resolves `--credential_helper=[scope=]path` entries following Bazel's `CredentialHelperProvider`:
/// exact host match > longest matching `*.domain` wildcard (matching both `domain` and subdomains
/// of `domain`) > unscoped default helper; later registrations override earlier ones of equal scope.
#[derive(Debug, Clone, Default)]
pub struct CredentialHelperProvider {
    default_helper: Option<String>,
    exact_hosts: BTreeMap<String, String>,
    wildcard_domains: BTreeMap<String, String>,
}

impl CredentialHelperProvider {
    pub fn from_specs(specs: &[String], workspace: Option<&std::path::Path>) -> Self {
        let mut p = Self::default();
        for spec in specs {
            if spec.is_empty() {
                continue;
            }
            p.add(spec, workspace);
        }
        p
    }

    pub fn add(&mut self, spec: &str, workspace: Option<&std::path::Path>) {
        let expand = |raw: &str| -> String {
            if let Some(ws) = workspace {
                if let Some(rest) = raw.strip_prefix("%workspace%/") {
                    return ws.join(rest).display().to_string();
                } else if raw == "%workspace%" {
                    return ws.display().to_string();
                }
            }
            raw.to_string()
        };
        match spec.split_once('=') {
            Some((scope, path)) if !scope.is_empty() => {
                let expanded = expand(path);
                if let Some(domain) = scope.strip_prefix("*.") {
                    self.wildcard_domains
                        .insert(domain.to_ascii_lowercase(), expanded);
                } else {
                    self.exact_hosts
                        .insert(scope.to_ascii_lowercase(), expanded);
                }
            }
            Some((_, path)) => {
                self.default_helper = Some(expand(path));
            }
            None => {
                self.default_helper = Some(expand(spec));
            }
        }
    }

    pub fn helper_for_uri(&self, uri: &str) -> Option<&str> {
        let host = extract_uri_host(uri).to_ascii_lowercase();
        if !host.is_empty() {
            if let Some(exact) = self.exact_hosts.get(&host) {
                return Some(exact.as_str());
            }
            // Check wildcard domains from most specificsubdomain suffix to least specific,
            // including matching the bare domain itself (`*.example.com` matches `example.com`).
            let mut best: Option<(usize, &str)> = None;
            for (domain, helper) in &self.wildcard_domains {
                if (host == *domain || host.ends_with(&format!(".{domain}")))
                    && best.is_none_or(|(len, _)| domain.len() >= len)
                {
                    best = Some((domain.len(), helper.as_str()));
                }
            }
            if let Some((_, helper)) = best {
                return Some(helper);
            }
        }
        self.default_helper.as_deref()
    }
}

/// `--credential_helper=[scope=]path`: whether the helper applies to `uri`'s host.
pub fn helper_applies(spec: &str, uri: &str) -> Option<String> {
    let host = extract_uri_host(uri).to_ascii_lowercase();
    match spec.split_once('=') {
        Some((scope, path)) => {
            let scope_lc = scope.to_ascii_lowercase();
            let ok = match scope_lc.strip_prefix("*.") {
                Some(suffix) => host == suffix || host.ends_with(&format!(".{suffix}")),
                None => scope_lc == host,
            };
            ok.then(|| path.to_string())
        }
        None => Some(spec.to_string()),
    }
}

/// Runs a resolved helper binary (`<path> get`) for `uri` in `cwd` with `client_env` (stripping
/// `RUN_UNDER_RUNFILES` just like Bazel's `CredentialHelper.getCredentials`).
pub fn invoke_credential_helper_process(
    path: &str,
    uri: &str,
    cwd: Option<&std::path::Path>,
    client_env: Option<&BTreeMap<String, String>>,
) -> Result<GetCredentialsResponse, String> {
    let mut cmd = Command::new(path);
    cmd.arg("get")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    if let Some(env) = client_env {
        cmd.env_clear();
        for (k, v) in env {
            if k != "RUN_UNDER_RUNFILES" {
                cmd.env(k, v);
            }
        }
    } else {
        cmd.env_remove("RUN_UNDER_RUNFILES");
    }
    let mut child = cmd
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
    GetCredentialsResponse::parse(&String::from_utf8_lossy(&out.stdout))
        .map_err(|e| format!("Credential helper '{path}' returned invalid output: {e}"))
}

/// Runs the helper (`<path> get`) for `uri` and returns the headers to attach.
pub fn run_credential_helper(spec: &str, uri: &str) -> Result<Vec<Header>, String> {
    let Some(path) = helper_applies(spec, uri) else {
        return Ok(vec![]);
    };
    let resp = invoke_credential_helper_process(&path, uri, None, None)?;
    let mut headers = Vec::new();
    for (k, vals) in resp.headers {
        if !vals.is_empty() {
            headers.push((k.to_ascii_lowercase(), vals.join(",")));
        }
    }
    Ok(headers)
}

/// Resolves the best matching helper from `specs` and returns its HTTP/2 headers for `uri`.
pub fn run_credential_helpers(specs: &[String], uri: &str) -> Result<Vec<Header>, String> {
    let provider = CredentialHelperProvider::from_specs(specs, None);
    let Some(path) = provider.helper_for_uri(uri) else {
        return Ok(vec![]);
    };
    let resp = invoke_credential_helper_process(path, uri, None, None)?;
    let mut headers = Vec::new();
    for (k, vals) in resp.headers {
        if !vals.is_empty() {
            headers.push((k.to_ascii_lowercase(), vals.join(",")));
        }
    }
    Ok(headers)
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
        assert_eq!(
            parse_helper_response(r#"{"headers":{"k":["v\u00e9\n"]}}"#).unwrap()[0].1,
            "vé\n"
        );
    }

    #[test]
    fn scopes() {
        assert_eq!(
            helper_applies("/bin/h", "grpc://a.b:1"),
            Some("/bin/h".into())
        );
        assert_eq!(
            helper_applies("*.b=/bin/h", "grpc://a.b:1"),
            Some("/bin/h".into())
        );
        assert_eq!(helper_applies("*.b=/bin/h", "grpc://x.c:1"), None);
        assert_eq!(
            helper_applies("a.b=/bin/h", "grpc://a.b"),
            Some("/bin/h".into())
        );
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
        assert_eq!(
            h,
            vec![(
                "authorization".to_string(),
                "Bearer from-helper".to_string()
            )]
        );
        assert!(run_credential_helper("/nonexistent/helper", "grpc://h:1").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_credentials_response_optional_headers_rfc3339_and_provider_precedence() {
        // Absent or null `headers` and RFC 3339 timestamps with `Z` or `+/-HH:MM` offsets.
        let empty_obj = GetCredentialsResponse::parse("{}").unwrap();
        assert!(empty_obj.headers.is_empty());
        assert_eq!(empty_obj.expires_epoch_secs, None);

        let null_hdrs = GetCredentialsResponse::parse(
            r#"{"expires":"1970-01-01T01:00:00+01:00","headers":null}"#,
        )
        .unwrap();
        assert!(null_hdrs.headers.is_empty());
        assert_eq!(null_hdrs.expires_epoch_secs, Some(0));
        assert_eq!(parse_helper_response("{}").unwrap(), Vec::<Header>::new());

        // Preserves exact header case, empty arrays, and repeated values.
        let rich = GetCredentialsResponse::parse(
            r#"{"headers":{"Authorization":["Bearer tok"],"Empty":[],"Repeat":["v1","v1"]}}"#,
        )
        .unwrap();
        assert_eq!(
            rich.headers.get("Authorization").unwrap(),
            &vec!["Bearer tok".to_string()]
        );
        assert_eq!(rich.headers.get("Empty").unwrap(), &Vec::<String>::new());
        assert_eq!(
            rich.headers.get("Repeat").unwrap(),
            &vec!["v1".to_string(), "v1".to_string()]
        );

        // Provider scope precedence + `%workspace%` expansion.
        let ws = std::path::Path::new("/my/ws");
        let provider = CredentialHelperProvider::from_specs(
            &[
                "%workspace%/tools/default".to_string(),
                "*.example.com=%workspace%/tools/wild".to_string(),
                "*.sub.example.com=/abs/subwild".to_string(),
                "host.sub.example.com=/abs/exact".to_string(),
            ],
            Some(ws),
        );
        assert_eq!(
            provider.helper_for_uri("https://host.sub.example.com/x"),
            Some("/abs/exact")
        );
        assert_eq!(
            provider.helper_for_uri("https://other.sub.example.com/x"),
            Some("/abs/subwild")
        );
        assert_eq!(
            provider.helper_for_uri("https://example.com/x"),
            Some("/my/ws/tools/wild")
        );
        assert_eq!(
            provider.helper_for_uri("https://unrelated.io/x"),
            Some("/my/ws/tools/default")
        );
    }
}
