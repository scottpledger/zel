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

//! Authentication & credential providers mirroring Bazel's `authandtls`:
//! - `CallCredentialsProvider` (`NO_CREDENTIALS`, per-RPC metadata + `refresh()`)
//! - `CredentialHelperProvider` (`--credential_helper=[scope=]path`, exact/wildcard/default scope
//!   precedence, `--credential_helper_timeout`, `--credential_helper_cache_duration` & RFC-3339
//!   `expires`)
//! - `GoogleAuthCallCredentialsProvider` (`--google_credentials`, `--google_default_credentials`,
//!   `--google_auth_scopes`, OAuth2 refresh_token & service-account RS256 JWT token exchange,
//!   1-second refresh throttle)
//! - `NetrcCredentialsProvider` (`$NETRC` / `~/.netrc` with column-0 `#` comments and
//!   `URI.getHost()` normalization)

use crate::hpack::Header;
use crate::transport::{Connector, TcpConnector, TlsConfig, TlsConnector};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use zel_util::base64_encode;

/// Error returned while acquiring or refreshing call credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialError {
    /// Mirrors Bazel's `CredentialHelperException` (never swallowed by `RemoteSpawnCache`).
    Helper(String),
    /// General Google / OAuth / auth failure.
    Auth(String),
}

impl CredentialError {
    pub fn is_helper_error(&self) -> bool {
        matches!(self, Self::Helper(_))
    }
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Helper(m) | Self::Auth(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for CredentialError {}

/// Mirrors Bazel's `CallCredentialsProvider`.
pub trait CallCredentialsProvider: Send + Sync {
    /// Returns lowercased `(header_name, header_value)` pairs to attach to an RPC/HTTP request
    /// targeting `uri`.
    fn get_credentials(&self, uri: &str) -> Result<Vec<Header>, CredentialError>;

    /// Refreshes cached credentials (invoked on `UNAUTHENTICATED` / `PERMISSION_DENIED` or 401).
    fn refresh(&self) -> Result<(), CredentialError>;
}

/// Mirrors `CallCredentialsProvider.NO_CREDENTIALS`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoCredentialsProvider;

impl CallCredentialsProvider for NoCredentialsProvider {
    fn get_credentials(&self, _uri: &str) -> Result<Vec<Header>, CredentialError> {
        Ok(Vec::new())
    }

    fn refresh(&self) -> Result<(), CredentialError> {
        Ok(())
    }
}

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

fn parse_json_obj(s: &str) -> Result<BTreeMap<String, Json>, String> {
    let mut p = P {
        b: s.as_bytes(),
        i: 0,
    };
    let v = p.value()?;
    match v {
        Json::Obj(m) => Ok(m),
        _ => Err("JSON root is not an object".into()),
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

fn percent_decode_str(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Extracts and normalizes the host component of `uri` following `URI.getHost()`:
/// percent-decoded, IPv6 brackets removed, ASCII-lowercased.
pub fn extract_uri_host(uri: &str) -> String {
    if uri.starts_with("unix:") {
        return String::new();
    }
    let rest = uri.split_once("://").map(|(_, r)| r).unwrap_or(uri);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    let raw = if let Some(after_bracket) = hostport.strip_prefix('[') {
        after_bracket.split(']').next().unwrap_or(after_bracket)
    } else {
        match hostport.rsplit_once(':') {
            Some((h, _)) if !h.contains(':') => h,
            _ => hostport,
        }
    };
    percent_decode_str(raw).to_ascii_lowercase()
}

/// Parses an RFC-3339 timestamp (`2030-01-01T00:00:00Z`, `2030-01-01T02:30:00+02:00`,
/// optional fractional seconds) into `SystemTime`.
pub fn parse_rfc3339(s: &str) -> Result<SystemTime, String> {
    let s = s.trim();
    if s.len() < 20 {
        return Err(format!("invalid RFC-3339 timestamp: '{s}'"));
    }
    let b = s.as_bytes();
    if b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b't') {
        return Err(format!("invalid RFC-3339 date separator: '{s}'"));
    }
    let year: i64 = s[0..4].parse().map_err(|_| "bad year")?;
    let month: i64 = s[5..7].parse().map_err(|_| "bad month")?;
    let day: i64 = s[8..10].parse().map_err(|_| "bad day")?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err("month/day out of range".into());
    }
    let time_part = &s[11..];
    if time_part.len() < 9 || time_part.as_bytes()[2] != b':' || time_part.as_bytes()[5] != b':' {
        return Err("bad time format".into());
    }
    let hour: i64 = time_part[0..2].parse().map_err(|_| "bad hour")?;
    let min: i64 = time_part[3..5].parse().map_err(|_| "bad minute")?;
    let sec: i64 = time_part[6..8].parse().map_err(|_| "bad second")?;
    if hour > 23 || min > 59 || sec > 60 {
        return Err("time component out of range".into());
    }

    let mut rem = &time_part[8..];
    let mut nanos: u32 = 0;
    if let Some(after_dot) = rem.strip_prefix('.') {
        let digits_len = after_dot.bytes().take_while(|c| c.is_ascii_digit()).count();
        if digits_len == 0 {
            return Err("empty fractional seconds".into());
        }
        let mut frac = after_dot[..digits_len.min(9)].to_string();
        while frac.len() < 9 {
            frac.push('0');
        }
        nanos = frac.parse().map_err(|_| "bad fractional seconds")?;
        rem = &after_dot[digits_len..];
    }

    let offset_secs: i64 = if rem == "Z" || rem == "z" {
        0
    } else if rem.len() == 6
        && (rem.starts_with('+') || rem.starts_with('-'))
        && rem.as_bytes()[3] == b':'
    {
        let sign = if rem.starts_with('+') { 1i64 } else { -1i64 };
        let oh: i64 = rem[1..3].parse().map_err(|_| "bad tz hour")?;
        let om: i64 = rem[4..6].parse().map_err(|_| "bad tz minute")?;
        sign * (oh * 3600 + om * 60)
    } else {
        return Err(format!("invalid timezone offset: '{rem}'"));
    };

    // Howard Hinnant civil-from-days / days-from-civil algorithm (exact proleptic Gregorian).
    let y = year - if month <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let unix_secs = days * 86400 + hour * 3600 + min * 60 + sec - offset_secs;

    if unix_secs >= 0 {
        Ok(UNIX_EPOCH + Duration::new(unix_secs as u64, nanos))
    } else {
        Ok(UNIX_EPOCH - Duration::from_secs((-unix_secs) as u64))
    }
}

/// Parsed output of a credential-helper invocation.
#[derive(Debug, Clone)]
pub struct HelperOutput {
    pub headers: Vec<Header>,
    pub expires: Option<SystemTime>,
}

/// Parses a credential-helper response into request headers and optional expiry.
pub fn parse_helper_response_with_expiry(out: &str) -> Result<HelperOutput, String> {
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return Ok(HelperOutput {
            headers: Vec::new(),
            expires: None,
        });
    }
    let m = parse_json_obj(trimmed)
        .map_err(|_| "credential helper output is not a JSON object".to_string())?;
    let expires = match m.get("expires") {
        Some(Json::Str(s)) => Some(parse_rfc3339(s)?),
        Some(Json::Null) | None => None,
        _ => return Err("\"expires\" must be an RFC-3339 string".into()),
    };
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
                        if !joined.is_empty() {
                            headers.push((k.to_ascii_lowercase(), joined.join(",")));
                        }
                    }
                    _ => return Err(format!("header {k}: expected an array of strings")),
                }
            }
        }
        Some(Json::Null) | None => {}
        _ => return Err("\"headers\" must be a JSON object".into()),
    }
    Ok(HelperOutput { headers, expires })
}

/// Parses a credential-helper response into request headers.
pub fn parse_helper_response(out: &str) -> Result<Vec<Header>, String> {
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return Err("credential helper output is empty".into());
    }
    Ok(parse_helper_response_with_expiry(out)?.headers)
}

/// Returns scope match specificity (`usize::MAX` for exact host, `1 + domain.len()` for
/// `*.domain` matching both subdomains and apex `domain`, `0` for unscoped default).
fn scope_specificity(scope: Option<&str>, host: &str) -> Option<usize> {
    match scope {
        None | Some("") => Some(0),
        Some(sc) => {
            if host.is_empty() {
                return None;
            }
            if let Some(domain) = sc.strip_prefix("*.") {
                let dom = domain.to_ascii_lowercase();
                if host == dom || host.ends_with(&format!(".{dom}")) {
                    Some(1 + dom.len())
                } else {
                    None
                }
            } else if host.eq_ignore_ascii_case(sc) {
                Some(usize::MAX)
            } else {
                None
            }
        }
    }
}

/// Normalizes a path string to Bazel `PathFragment` forward-slash form
/// (`WindowsOsPathPolicy.java:132-186`): converts `\` to `/`, uppercases a Windows drive letter,
/// collapses repeated slashes, drops `.` segments, and trims trailing `/` except on `/` or `X:/`.
fn normalize_bazel_path_str(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let has_drive = bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes.len() == 2 || matches!(bytes[2], b'/' | b'\\'));
    let is_abs_slash = !has_drive && matches!(bytes.first(), Some(b'/' | b'\\'));
    let after_prefix = if has_drive { &raw[2..] } else { raw };
    let segs: Vec<&str> = after_prefix
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    if has_drive {
        let drive = (bytes[0] as char).to_ascii_uppercase();
        if segs.is_empty() {
            format!("{drive}:/")
        } else {
            format!("{drive}:/{}", segs.join("/"))
        }
    } else if is_abs_slash {
        if segs.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", segs.join("/"))
        }
    } else {
        segs.join("/")
    }
}

/// Expands `%workspace%` in a `--credential_helper` path following Bazel's
/// `CommandLinePathFactory.create` (`CommandLinePathFactory.java:49, 86-105`) and `PathFragment`
/// normalization (`WindowsOsPathPolicy.java:132-186`).
pub fn expand_credential_helper_path(raw: &str, workspace: Option<&std::path::Path>) -> String {
    if let Some(ws) = workspace {
        let ws_norm = normalize_bazel_path_str(&ws.to_string_lossy());
        if raw == "%workspace%" {
            return ws_norm;
        }
        if let Some(rest) = raw
            .strip_prefix("%workspace%")
            .filter(|r| r.starts_with(['/', '\\']))
        {
            let segs: Vec<&str> = rest
                .split(['/', '\\'])
                .filter(|s| !s.is_empty() && *s != ".")
                .collect();
            if segs.is_empty() {
                return ws_norm;
            }
            let base = ws_norm.trim_end_matches('/');
            return format!("{base}/{}", segs.join("/"));
        }
    }
    raw.to_string()
}

/// `--credential_helper=[scope=]path`: whether the helper applies to `uri`'s host.
pub fn helper_applies(spec: &str, uri: &str) -> Option<String> {
    let host = extract_uri_host(uri);
    match spec.split_once('=') {
        Some((scope, path)) => scope_specificity(Some(scope), &host).map(|_| path.to_string()),
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
    #[cfg(windows)]
    let exec_path = path.replace('/', "\\");
    #[cfg(not(windows))]
    let exec_path = path;
    let mut cmd = Command::new(exec_path);
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

fn resolve_helper_path(raw: &str, workspace: Option<&Path>) -> PathBuf {
    if raw == "%workspace%"
        || raw
            .strip_prefix("%workspace%")
            .is_some_and(|r| r.starts_with(['/', '\\']))
    {
        return PathBuf::from(expand_credential_helper_path(raw, workspace));
    }
    if let Some(rel) = raw.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rel);
        }
    }
    PathBuf::from(raw)
}

fn invoke_helper_process(
    path: &Path,
    uri: &str,
    timeout: Duration,
) -> Result<HelperOutput, CredentialError> {
    let display = path.display().to_string();
    #[cfg(windows)]
    let exec_path = PathBuf::from(path.to_string_lossy().replace('/', "\\"));
    #[cfg(not(windows))]
    let exec_path = path;
    let mut cmd = Command::new(exec_path);
    cmd.arg("get")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("RUN_UNDER_RUNFILES");
    let mut child = cmd.spawn().map_err(|e| {
        CredentialError::Helper(format!(
            "Failed to get credentials for '{uri}' from helper '{display}': {e}"
        ))
    })?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(format!("{{\"uri\":\"{}\"}}", json_escape(uri)).as_bytes());
    }

    let mut stdout_pipe = child.stdout.take().unwrap();
    let mut stderr_pipe = child.stderr.take().unwrap();
    let out_handle = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout_pipe.read_to_string(&mut s);
        s
    });
    let err_handle = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr_pipe.read_to_string(&mut s);
        s
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {
                if !timeout.is_zero() && start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(CredentialError::Helper(format!(
                        "Credential helper '{display}' timed out after {:?} for '{uri}'",
                        timeout
                    )));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => {
                return Err(CredentialError::Helper(format!(
                    "Failed waiting for credential helper '{display}': {e}"
                )));
            }
        }
    };

    let stdout_str = out_handle.join().unwrap_or_default();
    let stderr_str = err_handle.join().unwrap_or_default();
    if !status.success() {
        return Err(CredentialError::Helper(format!(
            "Credential helper '{display}' failed: {}",
            stderr_str.trim()
        )));
    }
    parse_helper_response_with_expiry(&stdout_str).map_err(|e| {
        CredentialError::Helper(format!(
            "Credential helper '{display}' returned invalid output: {e}"
        ))
    })
}

/// Runs a single helper spec (`<path> get`) for `uri` and returns headers.
pub fn run_credential_helper(spec: &str, uri: &str) -> Result<Vec<Header>, String> {
    let Some(path) = helper_applies(spec, uri) else {
        return Ok(vec![]);
    };
    invoke_helper_process(Path::new(&path), uri, Duration::from_secs(5))
        .map(|o| o.headers)
        .map_err(|e| e.to_string())
}

#[derive(Clone)]
struct HelperEntry {
    scope: Option<String>,
    path: PathBuf,
}

struct CachedCreds {
    headers: Vec<Header>,
    expires_at: Option<Instant>,
    expires_wall: Option<SystemTime>,
}

impl CachedCreds {
    fn is_valid(&self) -> bool {
        if let Some(wall) = self.expires_wall {
            return SystemTime::now() < wall;
        }
        if let Some(inst) = self.expires_at {
            return Instant::now() < inst;
        }
        false
    }
}

/// Multi-helper `--credential_helper` provider with Bazel scope resolution, timeout, and
/// `CredentialCacheExpiry` caching.
pub struct CredentialHelperProvider {
    entries: Vec<HelperEntry>,
    timeout: Duration,
    cache_duration: Duration,
    cache: Mutex<HashMap<String, CachedCreds>>,
}

impl CredentialHelperProvider {
    pub fn from_specs(
        specs: &[String],
        workspace: Option<&Path>,
        timeout: Duration,
        cache_duration: Duration,
    ) -> Self {
        let entries = specs
            .iter()
            .filter(|s| !s.is_empty())
            .map(|spec| match spec.split_once('=') {
                Some((sc, p)) => HelperEntry {
                    scope: Some(sc.to_string()),
                    path: resolve_helper_path(p, workspace),
                },
                None => HelperEntry {
                    scope: None,
                    path: resolve_helper_path(spec, workspace),
                },
            })
            .collect();
        Self {
            entries,
            timeout,
            cache_duration,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Picks the highest-precedence helper for `uri`:
    /// exact host > longest `*.domain` wildcard > unscoped default; later flag wins on tie.
    pub fn select_helper(&self, uri: &str) -> Option<&Path> {
        let host = extract_uri_host(uri);
        let mut best: Option<(usize, &Path)> = None;
        for entry in &self.entries {
            if let Some(prio) = scope_specificity(entry.scope.as_deref(), &host) {
                if best.map(|(p, _)| prio >= p).unwrap_or(true) {
                    best = Some((prio, entry.path.as_path()));
                }
            }
        }
        best.map(|(_, p)| p)
    }

    pub fn helper_for_uri(&self, uri: &str) -> Option<&str> {
        self.select_helper(uri).and_then(|p| p.to_str())
    }
}

impl CallCredentialsProvider for CredentialHelperProvider {
    fn get_credentials(&self, uri: &str) -> Result<Vec<Header>, CredentialError> {
        let Some(helper_path) = self.select_helper(uri) else {
            return Ok(Vec::new());
        };
        {
            let mut cache = self.cache.lock().unwrap();
            if let Some(entry) = cache.get(uri) {
                if entry.is_valid() {
                    return Ok(entry.headers.clone());
                }
                cache.remove(uri);
            }
        }
        let out = invoke_helper_process(helper_path, uri, self.timeout)?;
        let cached = match out.expires {
            Some(wall) => CachedCreds {
                headers: out.headers.clone(),
                expires_at: None,
                expires_wall: Some(wall),
            },
            None if !self.cache_duration.is_zero() => CachedCreds {
                headers: out.headers.clone(),
                expires_at: Some(Instant::now() + self.cache_duration),
                expires_wall: None,
            },
            None => {
                return Ok(out.headers);
            }
        };
        if cached.is_valid() {
            self.cache.lock().unwrap().insert(uri.to_string(), cached);
        }
        Ok(out.headers)
    }

    fn refresh(&self) -> Result<(), CredentialError> {
        self.cache.lock().unwrap().clear();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// GoogleAuthCallCredentialsProvider (`--google_credentials` / ADC)
// ---------------------------------------------------------------------------

struct GoogleTokenState {
    access_token: Option<String>,
    expires_at: Option<Instant>,
    last_refresh: Instant,
}

/// Mirrors Bazel's `GoogleAuthCallCredentialsProvider`, including the 1-second stopwatch
/// throttle on `refresh()`.
pub struct GoogleAuthCallCredentialsProvider {
    credentials_path: PathBuf,
    scopes: Vec<String>,
    tls: TlsConfig,
    state: Mutex<GoogleTokenState>,
}

impl GoogleAuthCallCredentialsProvider {
    pub fn from_file(path: PathBuf, scopes: Vec<String>, tls: TlsConfig) -> Self {
        Self {
            credentials_path: path,
            scopes: if scopes.is_empty() {
                vec!["https://www.googleapis.com/auth/cloud-platform".to_string()]
            } else {
                scopes
            },
            tls,
            state: Mutex::new(GoogleTokenState {
                access_token: None,
                expires_at: None,
                // Stopwatch starts on construction (mirrors GoogleAuthCallCredentialsProvider).
                last_refresh: Instant::now(),
            }),
        }
    }

    pub fn from_adc(scopes: Vec<String>, tls: TlsConfig) -> Result<Self, CredentialError> {
        if let Ok(p) = std::env::var("GOOGLE_APPLICATION_CREDENTIALS") {
            if !p.is_empty() {
                let pb = PathBuf::from(p);
                if pb.is_file() {
                    return Ok(Self::from_file(pb, scopes, tls));
                }
                return Err(CredentialError::Auth(format!(
                    "GOOGLE_APPLICATION_CREDENTIALS file not found: {}",
                    pb.display()
                )));
            }
        }
        if let Ok(home) = std::env::var("HOME") {
            let adc = PathBuf::from(home)
                .join(".config")
                .join("gcloud")
                .join("application_default_credentials.json");
            if adc.is_file() {
                return Ok(Self::from_file(adc, scopes, tls));
            }
        }
        Err(CredentialError::Auth(
            "Unable to find Application Default Credentials".into(),
        ))
    }

    fn fetch_token(&self) -> Result<(String, Option<Duration>), CredentialError> {
        let raw = std::fs::read_to_string(&self.credentials_path).map_err(|e| {
            CredentialError::Auth(format!(
                "Failed to read Google credentials '{}': {e}",
                self.credentials_path.display()
            ))
        })?;
        let obj = parse_json_obj(raw.trim()).map_err(|e| {
            CredentialError::Auth(format!(
                "Invalid Google credentials JSON '{}': {e}",
                self.credentials_path.display()
            ))
        })?;

        // 1. Direct pre-issued access_token JSON (`{"access_token": "...", "expires_in": 3600}`)
        if let Some(Json::Str(tok)) = obj.get("access_token").or_else(|| obj.get("token")) {
            let ttl = match obj.get("expires_in") {
                Some(Json::Num(secs)) if *secs > 0.0 => Some(Duration::from_secs_f64(*secs)),
                _ => Some(Duration::from_secs(3600)),
            };
            return Ok((tok.clone(), ttl));
        }

        let cred_type = match obj.get("type") {
            Some(Json::Str(t)) => t.as_str(),
            _ => "",
        };
        let token_uri = match obj.get("token_uri") {
            Some(Json::Str(u)) if !u.is_empty() => u.as_str(),
            _ => "https://oauth2.googleapis.com/token",
        };

        match cred_type {
            "authorized_user" => {
                let get_s = |k: &str| -> Result<&str, CredentialError> {
                    match obj.get(k) {
                        Some(Json::Str(s)) => Ok(s.as_str()),
                        _ => Err(CredentialError::Auth(format!(
                            "authorized_user credentials missing '{k}'"
                        ))),
                    }
                };
                let body = format!(
                    "grant_type=refresh_token&client_id={}&client_secret={}&refresh_token={}",
                    url_form_encode(get_s("client_id")?),
                    url_form_encode(get_s("client_secret")?),
                    url_form_encode(get_s("refresh_token")?),
                );
                exchange_oauth_token(token_uri, &body, &self.tls)
            }
            "service_account" => {
                let client_email = match obj.get("client_email") {
                    Some(Json::Str(s)) => s.as_str(),
                    _ => {
                        return Err(CredentialError::Auth(
                            "service_account credentials missing 'client_email'".into(),
                        ))
                    }
                };
                let private_key = match obj.get("private_key") {
                    Some(Json::Str(s)) => s.as_str(),
                    _ => {
                        return Err(CredentialError::Auth(
                            "service_account credentials missing 'private_key'".into(),
                        ))
                    }
                };
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let scope_joined = self.scopes.join(" ");
                let jwt = sign_rs256_jwt(client_email, &scope_joined, token_uri, now, private_key)?;
                let body = format!(
                    "grant_type={}&assertion={}",
                    url_form_encode("urn:ietf:params:oauth:grant-type:jwt-bearer"),
                    url_form_encode(&jwt),
                );
                exchange_oauth_token(token_uri, &body, &self.tls)
            }
            other => Err(CredentialError::Auth(format!(
                "Unsupported Google credentials type: '{other}'"
            ))),
        }
    }
}

impl CallCredentialsProvider for GoogleAuthCallCredentialsProvider {
    fn get_credentials(&self, _uri: &str) -> Result<Vec<Header>, CredentialError> {
        let mut st = self.state.lock().unwrap();
        let valid = st.access_token.is_some()
            && st
                .expires_at
                .map(|exp| Instant::now() < exp)
                .unwrap_or(true);
        if !valid {
            let (tok, ttl) = self.fetch_token()?;
            st.access_token = Some(tok);
            st.expires_at = ttl.map(|d| Instant::now() + d);
            st.last_refresh = Instant::now();
        }
        let tok = st.access_token.as_ref().unwrap();
        Ok(vec![("authorization".to_string(), format!("Bearer {tok}"))])
    }

    fn refresh(&self) -> Result<(), CredentialError> {
        let mut st = self.state.lock().unwrap();
        // Bazel's GoogleAuthCallCredentialsProvider throttles refresh() calls within 1 second.
        if st.last_refresh.elapsed() <= Duration::from_secs(1) {
            return Ok(());
        }
        let (tok, ttl) = self.fetch_token()?;
        st.access_token = Some(tok);
        st.expires_at = ttl.map(|d| Instant::now() + d);
        st.last_refresh = Instant::now();
        Ok(())
    }
}

fn base64url_encode(data: &[u8]) -> String {
    base64_encode(data)
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_")
}

fn url_form_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn sign_rs256_jwt(
    iss: &str,
    scope: &str,
    aud: &str,
    now_secs: u64,
    private_key_pem: &str,
) -> Result<String, CredentialError> {
    let header_b64 = base64url_encode(b"{\"alg\":\"RS256\",\"typ\":\"JWT\"}");
    let claims = format!(
        "{{\"iss\":\"{}\",\"scope\":\"{}\",\"aud\":\"{}\",\"iat\":{},\"exp\":{}}}",
        json_escape(iss),
        json_escape(scope),
        json_escape(aud),
        now_secs,
        now_secs + 3600
    );
    let claims_b64 = base64url_encode(claims.as_bytes());
    let signing_input = format!("{header_b64}.{claims_b64}");

    let key_path = std::env::temp_dir().join(format!("zel-sa-key-{}.pem", zel_util::uuid_v4()));
    {
        let mut f = std::fs::File::create(&key_path).map_err(|e| {
            CredentialError::Auth(format!("Failed to stage temporary signing key: {e}"))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
        if let Err(e) = f.write_all(private_key_pem.as_bytes()) {
            let _ = std::fs::remove_file(&key_path);
            return Err(CredentialError::Auth(format!(
                "Failed writing temporary signing key: {e}"
            )));
        }
    }

    let mut child = match Command::new("openssl")
        .arg("dgst")
        .arg("-sha256")
        .arg("-sign")
        .arg(&key_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&key_path);
            return Err(CredentialError::Auth(format!(
                "Failed invoking openssl to sign service_account JWT: {e}"
            )));
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(signing_input.as_bytes());
    }
    let out = child.wait_with_output();
    let _ = std::fs::remove_file(&key_path);
    let out = out.map_err(|e| CredentialError::Auth(format!("openssl dgst failed: {e}")))?;
    if !out.status.success() {
        return Err(CredentialError::Auth(format!(
            "Failed signing service_account JWT: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let sig_b64 = base64url_encode(&out.stdout);
    Ok(format!("{signing_input}.{sig_b64}"))
}

fn exchange_oauth_token(
    token_uri: &str,
    form_body: &str,
    tls: &TlsConfig,
) -> Result<(String, Option<Duration>), CredentialError> {
    let (tls_scheme, rest) = if let Some(r) = token_uri.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = token_uri.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(CredentialError::Auth(format!(
            "Unsupported token_uri scheme: '{token_uri}'"
        )));
    };
    let (hostport, path) = match rest.split_once('/') {
        Some((hp, p)) => (hp, format!("/{p}")),
        None => (rest, "/".to_string()),
    };
    let default_port = if tls_scheme { 443 } else { 80 };
    let (host, port) = if let Some(r) = hostport.strip_prefix('[') {
        let (h, p) = r
            .split_once(']')
            .ok_or_else(|| CredentialError::Auth("bad IPv6 host in token_uri".into()))?;
        let port = match p.strip_prefix(':') {
            Some(ps) => ps
                .parse()
                .map_err(|_| CredentialError::Auth("bad port in token_uri".into()))?,
            None => default_port,
        };
        (h.to_string(), port)
    } else if let Some((h, p)) = hostport.rsplit_once(':') {
        let port = p
            .parse()
            .map_err(|_| CredentialError::Auth("bad port in token_uri".into()))?;
        (h.to_string(), port)
    } else {
        (hostport.to_string(), default_port)
    };

    let mut transport = if tls_scheme {
        TlsConnector {
            host: host.clone(),
            port,
            connect_timeout: Duration::from_secs(10),
            alpn: None,
            tls: tls.clone(),
            unix_proxy: None,
        }
        .connect()
    } else {
        TcpConnector::new(host.clone(), port, Duration::from_secs(10)).connect()
    }
    .map_err(|e| CredentialError::Auth(format!("Failed connecting to {token_uri}: {e}")))?;

    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {hostport}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{form_body}",
        form_body.len()
    );
    transport
        .writer
        .write_all(req.as_bytes())
        .and_then(|_| transport.writer.flush())
        .map_err(|e| CredentialError::Auth(format!("Failed sending OAuth token request: {e}")))?;

    let mut resp = Vec::new();
    transport
        .reader
        .read_to_end(&mut resp)
        .map_err(|e| CredentialError::Auth(format!("Failed reading OAuth token response: {e}")))?;
    (transport.shutdown)();

    let header_end = resp
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| CredentialError::Auth("Malformed HTTP response from token_uri".into()))?;
    let head = String::from_utf8_lossy(&resp[..header_end]);
    let status_line = head.lines().next().unwrap_or("");
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body_bytes = &resp[header_end + 4..];
    let body_text = String::from_utf8_lossy(body_bytes);
    if !(200..300).contains(&status_code) {
        return Err(CredentialError::Auth(format!(
            "OAuth token exchange failed with HTTP {status_code}: {}",
            body_text.trim()
        )));
    }
    let obj = parse_json_obj(body_text.trim()).map_err(|e| {
        CredentialError::Auth(format!("Invalid JSON from OAuth token endpoint: {e}"))
    })?;
    let tok = match obj.get("access_token") {
        Some(Json::Str(t)) => t.clone(),
        _ => {
            return Err(CredentialError::Auth(
                "OAuth token response missing 'access_token'".into(),
            ))
        }
    };
    let ttl = match obj.get("expires_in") {
        Some(Json::Num(secs)) if *secs > 0.0 => Some(Duration::from_secs_f64(*secs)),
        _ => Some(Duration::from_secs(3600)),
    };
    Ok((tok, ttl))
}

// ---------------------------------------------------------------------------
// NetrcCredentialsProvider (`NetrcParser` + `NetrcCredentials`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct NetrcEntry {
    login: String,
    password: String,
}

/// Mirrors Bazel's `NetrcCredentials` + `NetrcParser`:
/// - `#` starts a comment ONLY when column 0 of the raw line is `#`;
/// - `macdef` consumes lines until a blank line;
/// - matched entry always emits `Authorization: Basic base64(login + ":" + password)` with
///   omitted `login` or `password` treated as `""`;
/// - lookup key is `URI.getHost()` (lowercased, percent-decoded, IPv6 brackets removed).
#[derive(Debug, Clone, Default)]
pub struct NetrcCredentialsProvider {
    machines: HashMap<String, NetrcEntry>,
    default_entry: Option<NetrcEntry>,
}

impl NetrcCredentialsProvider {
    pub fn parse(content: &str) -> Result<Self, String> {
        let mut tokens: Vec<String> = Vec::new();
        let mut in_macdef = false;
        for raw_line in content.lines() {
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            if in_macdef {
                if line.trim().is_empty() {
                    in_macdef = false;
                }
                continue;
            }
            if line.starts_with('#') {
                continue;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            for tok in trimmed.split_ascii_whitespace() {
                if tok == "macdef" {
                    in_macdef = true;
                    break;
                }
                tokens.push(tok.to_string());
            }
        }

        let mut machines = HashMap::new();
        let mut default_entry = None;
        let mut i = 0usize;
        while i < tokens.len() {
            match tokens[i].as_str() {
                "machine" => {
                    i += 1;
                    if i >= tokens.len() {
                        return Err("Unexpected EOF after 'machine' in .netrc".into());
                    }
                    let name = tokens[i].clone();
                    i += 1;
                    let entry = Self::parse_entry(&tokens, &mut i)?;
                    machines.insert(name, entry);
                }
                "default" => {
                    i += 1;
                    default_entry = Some(Self::parse_entry(&tokens, &mut i)?);
                    // Bazel NetrcParser stops after `default`.
                    break;
                }
                other => return Err(format!("Unexpected token in .netrc: '{other}'")),
            }
        }
        Ok(Self {
            machines,
            default_entry,
        })
    }

    fn parse_entry(tokens: &[String], i: &mut usize) -> Result<NetrcEntry, String> {
        let mut entry = NetrcEntry::default();
        while *i < tokens.len() {
            match tokens[*i].as_str() {
                "login" => {
                    *i += 1;
                    if *i >= tokens.len() {
                        return Err("Unexpected EOF after 'login'".into());
                    }
                    entry.login = tokens[*i].clone();
                    *i += 1;
                }
                "password" => {
                    *i += 1;
                    if *i >= tokens.len() {
                        return Err("Unexpected EOF after 'password'".into());
                    }
                    entry.password = tokens[*i].clone();
                    *i += 1;
                }
                "account" => {
                    *i += 1;
                    if *i >= tokens.len() {
                        return Err("Unexpected EOF after 'account'".into());
                    }
                    *i += 1;
                }
                "machine" | "default" | "macdef" => break,
                other => return Err(format!("Unexpected token in .netrc machine: '{other}'")),
            }
        }
        Ok(entry)
    }

    pub fn from_file(path: &Path) -> Result<Self, String> {
        let bytes =
            std::fs::read(path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        // ISO-8859-1 / Latin-1 decoding matching Bazel's NetrcParser.
        let text: String = bytes.into_iter().map(|b| b as char).collect();
        Self::parse(&text).map_err(|e| format!("Failed to parse {}: {e}", path.display()))
    }

    pub fn from_client_env(client_env: &BTreeMap<String, String>) -> Result<Option<Self>, String> {
        let candidate = client_env
            .get("NETRC")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                client_env
                    .get("HOME")
                    .filter(|s| !s.is_empty())
                    .map(|h| PathBuf::from(h).join(".netrc"))
            })
            .or_else(|| {
                client_env
                    .get("USERPROFILE")
                    .filter(|s| !s.is_empty())
                    .map(|h| PathBuf::from(h).join(".netrc"))
            });
        let Some(path) = candidate else {
            return Ok(None);
        };
        if !path.is_file() {
            return Ok(None);
        }
        Self::from_file(&path).map(Some)
    }

    pub fn from_env() -> Option<Self> {
        let candidates = [
            std::env::var("NETRC")
                .ok()
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
            std::env::var("HOME")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|h| PathBuf::from(h).join(".netrc")),
            std::env::var("USERPROFILE")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|h| PathBuf::from(h).join(".netrc")),
        ];
        for path in candidates.into_iter().flatten() {
            if path.is_file() {
                if let Ok(parsed) = Self::from_file(&path) {
                    return Some(parsed);
                }
            }
        }
        None
    }
}

impl CallCredentialsProvider for NetrcCredentialsProvider {
    fn get_credentials(&self, uri: &str) -> Result<Vec<Header>, CredentialError> {
        let host = extract_uri_host(uri);
        if host.is_empty() {
            return Ok(Vec::new());
        }
        let Some(entry) = self.machines.get(&host).or(self.default_entry.as_ref()) else {
            return Ok(Vec::new());
        };
        let raw = format!("{}:{}", entry.login, entry.password);
        // ISO-8859-1 bytes for Basic encoding, matching Bazel's NetrcCredentials.
        let latin1: Vec<u8> = raw.chars().map(|c| c as u8).collect();
        Ok(vec![(
            "authorization".to_string(),
            format!("Basic {}", base64_encode(&latin1)),
        )])
    }

    fn refresh(&self) -> Result<(), CredentialError> {
        Ok(())
    }
}

/// Chains `CredentialHelperProvider` -> `GoogleAuthCallCredentialsProvider` -> `NetrcCredentialsProvider`,
/// matching Bazel's credential precedence (`CredentialModule` + `GoogleAuthUtils`).
pub struct CompositeCredentialsProvider {
    pub helper: Option<CredentialHelperProvider>,
    pub google: Option<GoogleAuthCallCredentialsProvider>,
    pub netrc: Option<NetrcCredentialsProvider>,
}

impl CallCredentialsProvider for CompositeCredentialsProvider {
    fn get_credentials(&self, uri: &str) -> Result<Vec<Header>, CredentialError> {
        if let Some(h) = &self.helper {
            if h.select_helper(uri).is_some() {
                let hdrs = h.get_credentials(uri)?;
                if !hdrs.is_empty() {
                    return Ok(hdrs);
                }
            }
        }
        if let Some(g) = &self.google {
            return g.get_credentials(uri);
        }
        if let Some(n) = &self.netrc {
            return n.get_credentials(uri);
        }
        Ok(Vec::new())
    }

    fn refresh(&self) -> Result<(), CredentialError> {
        if let Some(h) = &self.helper {
            h.refresh()?;
        }
        if let Some(g) = &self.google {
            g.refresh()?;
        }
        if let Some(n) = &self.netrc {
            n.refresh()?;
        }
        Ok(())
    }
}

/// Resolves the best matching helper from `specs` and returns its HTTP/2 headers for `uri`.
pub fn run_credential_helpers(specs: &[String], uri: &str) -> Result<Vec<Header>, String> {
    run_credential_helpers_in_workspace(specs, uri, None)
}

/// Resolves the best matching helper from `specs` (expanding `%workspace%` against `workspace`)
/// and returns its HTTP/2 headers for `uri`.
pub fn run_credential_helpers_in_workspace(
    specs: &[String],
    uri: &str,
    workspace: Option<&std::path::Path>,
) -> Result<Vec<Header>, String> {
    let provider = CredentialHelperProvider::from_specs(
        specs,
        workspace,
        Duration::from_secs(5),
        Duration::from_secs(30 * 60),
    );
    let Some(path) = provider.helper_for_uri(uri) else {
        return Ok(vec![]);
    };
    let resp = invoke_credential_helper_process(path, uri, workspace, None)?;
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
    fn parses_helper_output_and_rfc3339_expiry() {
        let out = parse_helper_response_with_expiry(
            r#"{"expires":"2030-01-01T00:00:00Z","headers":{"Authorization":["Bearer abc"],"X-Multi":["a","b"]}}"#,
        )
        .unwrap();
        assert!(out
            .headers
            .contains(&("authorization".to_string(), "Bearer abc".to_string())));
        assert!(out
            .headers
            .contains(&("x-multi".to_string(), "a,b".to_string())));
        assert!(out.expires.unwrap() > SystemTime::now());
        assert!(parse_helper_response("[]").is_err());
        assert!(parse_helper_response(r#"{"headers":{"a":"s"}}"#).is_err());
        assert_eq!(
            parse_helper_response(r#"{"headers":{"k":["v\u00e9\n"]}}"#).unwrap()[0].1,
            "vé\n"
        );
    }

    #[test]
    fn scopes_precedence_and_apex_matching() {
        assert_eq!(
            helper_applies("/bin/h", "grpc://a.b:1"),
            Some("/bin/h".into())
        );
        assert_eq!(
            helper_applies("*.b=/bin/h", "grpc://a.b:1"),
            Some("/bin/h".into())
        );
        // `*.example.com` also matches apex `example.com` in Bazel.
        assert_eq!(
            helper_applies("*.example.com=/bin/h", "https://Example.COM:443"),
            Some("/bin/h".into())
        );
        assert_eq!(helper_applies("*.b=/bin/h", "grpc://x.c:1"), None);
        assert_eq!(
            helper_applies("a.b=/bin/h", "grpc://a.b"),
            Some("/bin/h".into())
        );

        let prov = CredentialHelperProvider::from_specs(
            &[
                "/default/h".into(),
                "*.example.com=/wild/h".into(),
                "*.sub.example.com=/subwild/h".into(),
                "api.sub.example.com=/exact/h".into(),
            ],
            None,
            Duration::from_secs(5),
            Duration::from_secs(60),
        );
        assert_eq!(
            prov.select_helper("https://api.sub.example.com"),
            Some(Path::new("/exact/h"))
        );
        assert_eq!(
            prov.select_helper("https://other.sub.example.com"),
            Some(Path::new("/subwild/h"))
        );
        assert_eq!(
            prov.select_helper("https://example.com"),
            Some(Path::new("/wild/h"))
        );
        assert_eq!(
            prov.select_helper("https://unrelated.org"),
            Some(Path::new("/default/h"))
        );
    }

    #[test]
    fn netrc_credentials_and_google_refresh_throttle() {
        let netrc = NetrcCredentialsProvider::parse(
            "# comment\nmachine foo.example.org login alice password s#cret\n",
        )
        .unwrap();
        let hdrs = netrc
            .get_credentials("https://Foo.Ex%61mple.ORG:8443/cas")
            .unwrap();
        assert_eq!(
            hdrs,
            vec![(
                "authorization".to_string(),
                format!("Basic {}", base64_encode(b"alice:s#cret"))
            )]
        );

        let dir = std::env::temp_dir().join(format!("zel-gcred-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cred_file = dir.join("creds.json");
        std::fs::write(&cred_file, r#"{"access_token":"tok-1"}"#).unwrap();
        let g = GoogleAuthCallCredentialsProvider::from_file(
            cred_file.clone(),
            vec![],
            TlsConfig::default(),
        );
        assert_eq!(
            g.get_credentials("https://remote.example.com").unwrap(),
            vec![("authorization".to_string(), "Bearer tok-1".to_string())]
        );
        // Update file on disk; immediate refresh() within 1s must be throttled!
        std::fs::write(&cred_file, r#"{"access_token":"tok-2"}"#).unwrap();
        g.refresh().unwrap();
        assert_eq!(
            g.get_credentials("https://remote.example.com").unwrap()[0].1,
            "Bearer tok-1"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn runs_a_real_helper_script_and_caches_until_refresh() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("zel-cred-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let count_file = dir.join("count");
        let script = dir.join("helper.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nread req\nn=$(cat {} 2>/dev/null || echo 0)\nn=$((n + 1))\necho $n > {}\ncase \"$1\" in get) echo '{{\"headers\":{{\"Authorization\":[\"Bearer tok-'$n'\"]}}}}';; *) exit 1;; esac\n",
                count_file.display(),
                count_file.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let prov = CredentialHelperProvider::from_specs(
            &[script.to_str().unwrap().to_string()],
            None,
            Duration::from_secs(5),
            Duration::from_secs(600),
        );
        let h1 = prov.get_credentials("https://h:443").unwrap();
        let h2 = prov.get_credentials("https://h:443").unwrap();
        assert_eq!(
            h1,
            vec![("authorization".to_string(), "Bearer tok-1".to_string())]
        );
        assert_eq!(h1, h2);
        prov.refresh().unwrap();
        let h3 = prov.get_credentials("https://h:443").unwrap();
        assert_eq!(
            h3,
            vec![("authorization".to_string(), "Bearer tok-2".to_string())]
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
            Duration::from_secs(5),
            Duration::from_secs(60),
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

        // Windows drive-letter workspace (`C:\my\ws` / `c:\my\ws\`) + both `/` and `\` `%workspace%`
        // specs and repeated slashes normalize cleanly to Bazel `PathFragment` forward-slash form.
        let win_ws = std::path::Path::new(r"c:\my\ws\");
        let win_provider = CredentialHelperProvider::from_specs(
            &[
                r"%workspace%\tools\default.bat".to_string(),
                "*.example.com=%workspace%//tools//./wild.exe".to_string(),
                "exact.example.com=%workspace%".to_string(),
            ],
            Some(win_ws),
            Duration::from_secs(5),
            Duration::from_secs(60),
        );
        assert_eq!(
            win_provider.helper_for_uri("https://exact.example.com/x"),
            Some("C:/my/ws")
        );
        assert_eq!(
            win_provider.helper_for_uri("https://sub.example.com/x"),
            Some("C:/my/ws/tools/wild.exe")
        );
        assert_eq!(
            win_provider.helper_for_uri("https://unrelated.io/x"),
            Some("C:/my/ws/tools/default.bat")
        );
        assert_eq!(
            expand_credential_helper_path(
                r"%workspace%\tools\wild.exe",
                Some(std::path::Path::new(r"C:\"))
            ),
            "C:/tools/wild.exe"
        );
        assert_eq!(
            expand_credential_helper_path(
                "%workspace%//tools/wild",
                Some(std::path::Path::new("/"))
            ),
            "/tools/wild"
        );
    }
}
