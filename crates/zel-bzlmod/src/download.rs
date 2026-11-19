//! Download support: URL fallback, `file://`, plain `http://` over `std::net`, `https://` via the
//! system `curl` binary, repository cache, `--distdir`, checksum verification and
//! `--downloader_config` rewriting. Error messages follow Bazel's wording where known.

use crate::hash::{base64_encode, Algorithm, Checksum};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

// ===========================================================================
// URL parsing
// ===========================================================================

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub scheme: String,
    pub userinfo: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    /// Path including query (always starts with '/' unless empty for non-hierarchical URLs).
    pub path_and_query: String,
}

impl Url {
    pub fn parse(s: &str) -> Result<Url, String> {
        let (scheme, rest) = s
            .split_once(':')
            .ok_or_else(|| format!("Invalid URL (no scheme): {}", s))?;
        if scheme.is_empty()
            || !scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        {
            return Err(format!("Invalid URL scheme: {}", s));
        }
        let scheme = scheme.to_ascii_lowercase();
        let rest = match rest.strip_prefix("//") {
            Some(r) => r,
            None => {
                return Ok(Url {
                    scheme,
                    userinfo: None,
                    host: String::new(),
                    port: None,
                    path_and_query: rest.to_string(),
                })
            }
        };
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..end];
        let mut tail = rest[end..].to_string();
        if let Some(h) = tail.find('#') {
            tail.truncate(h);
        }
        if tail.is_empty() {
            tail = "/".to_string();
        } else if tail.starts_with('?') {
            tail = format!("/{}", tail);
        }
        let (userinfo, hostport) = match authority.rsplit_once('@') {
            Some((u, h)) => (Some(u.to_string()), h),
            None => (None, authority),
        };
        let (host, port) = if let Some(r) = hostport.strip_prefix('[') {
            let (h, after) = r
                .split_once(']')
                .ok_or_else(|| format!("Invalid URL (bad IPv6 host): {}", s))?;
            let port = match after.strip_prefix(':') {
                Some(p) if !p.is_empty() => Some(
                    p.parse::<u16>()
                        .map_err(|_| format!("Invalid port in URL: {}", s))?,
                ),
                _ => None,
            };
            (h.to_string(), port)
        } else if let Some((h, p)) = hostport.rsplit_once(':') {
            let port = if p.is_empty() {
                None
            } else {
                Some(
                    p.parse::<u16>()
                        .map_err(|_| format!("Invalid port in URL: {}", s))?,
                )
            };
            (h.to_string(), port)
        } else {
            (hostport.to_string(), None)
        };
        Ok(Url {
            scheme,
            userinfo,
            host,
            port,
            path_and_query: tail,
        })
    }

    pub fn path(&self) -> &str {
        self.path_and_query.split('?').next().unwrap_or("")
    }

    pub fn host_header(&self) -> String {
        let h = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        match self.port {
            Some(p) if Some(p) != default_port(&self.scheme) => format!("{}:{}", h, p),
            _ => h,
        }
    }

    pub fn to_url_string(&self) -> String {
        let h = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let ui = self
            .userinfo
            .as_ref()
            .map(|u| format!("{}@", u))
            .unwrap_or_default();
        let port = self.port.map(|p| format!(":{}", p)).unwrap_or_default();
        format!(
            "{}://{}{}{}{}",
            self.scheme, ui, h, port, self.path_and_query
        )
    }

    /// Resolve a (possibly relative) redirect location against this URL.
    pub fn join(&self, loc: &str) -> Result<Url, String> {
        if loc.contains("://") {
            return Url::parse(loc);
        }
        if let Some(rest) = loc.strip_prefix("//") {
            return Url::parse(&format!("{}://{}", self.scheme, rest));
        }
        let mut u = self.clone();
        u.userinfo = None;
        if loc.starts_with('/') {
            u.path_and_query = loc.to_string();
        } else if loc.starts_with('?') {
            u.path_and_query = format!("{}{}", self.path(), loc);
        } else {
            let dir = match self.path().rfind('/') {
                Some(i) => &self.path()[..=i],
                None => "/",
            };
            u.path_and_query = format!("{}{}", dir, loc);
        }
        if let Some(h) = u.path_and_query.find('#') {
            u.path_and_query.truncate(h);
        }
        Ok(u)
    }
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if let Some(v) = s.get(i + 1..i + 3).and_then(crate::hash::from_hex) {
                out.push(v[0]);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn file_url_to_path(u: &Url) -> Result<PathBuf, String> {
    if !u.host.is_empty() && u.host != "localhost" {
        return Err(format!(
            "Unsupported file URL host '{}': file:// URLs must be absolute paths",
            u.host
        ));
    }
    let p = percent_decode(u.path());
    if !p.starts_with('/') {
        return Err(format!(
            "file URL must have an absolute path: {}",
            u.to_url_string()
        ));
    }
    #[cfg(windows)]
    let p = {
        let b = p.as_bytes();
        if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
            p[1..].to_string()
        } else {
            p
        }
    };
    Ok(PathBuf::from(p))
}

// ===========================================================================
// Minimal backtracking regex (enough for downloader_config rewrite rules)
// ===========================================================================

#[derive(Debug, Clone)]
enum Node {
    Lit(char),
    Any,
    Class(bool, Vec<(char, char)>),
    Start,
    End,
    Group(Vec<Vec<Node>>, Option<usize>),
    Repeat(Box<Node>, usize, usize, bool),
}

#[derive(Debug, Clone)]
pub struct Regex {
    alts: Vec<Vec<Node>>,
    groups: usize,
}

struct RParser<'a> {
    c: &'a [char],
    i: usize,
    groups: usize,
}

fn class_escape(c: char) -> Option<(bool, Vec<(char, char)>)> {
    let d = vec![('0', '9')];
    let w = vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')];
    let s = vec![(' ', ' '), ('\t', '\r')];
    match c {
        'd' => Some((false, d)),
        'D' => Some((true, d)),
        'w' => Some((false, w)),
        'W' => Some((true, w)),
        's' => Some((false, s)),
        'S' => Some((true, s)),
        _ => None,
    }
}

impl RParser<'_> {
    fn parse_alts(&mut self, depth: usize) -> Result<Vec<Vec<Node>>, String> {
        let mut alts = vec![self.parse_seq(depth)?];
        while self.i < self.c.len() && self.c[self.i] == '|' {
            self.i += 1;
            alts.push(self.parse_seq(depth)?);
        }
        Ok(alts)
    }
    fn parse_seq(&mut self, depth: usize) -> Result<Vec<Node>, String> {
        let mut seq = Vec::new();
        while self.i < self.c.len() {
            let ch = self.c[self.i];
            if ch == '|' || (ch == ')' && depth > 0) {
                break;
            }
            let atom = self.parse_atom(depth)?;
            let atom = self.parse_quant(atom)?;
            seq.push(atom);
        }
        Ok(seq)
    }
    fn parse_quant(&mut self, atom: Node) -> Result<Node, String> {
        if self.i >= self.c.len() {
            return Ok(atom);
        }
        let (min, max) = match self.c[self.i] {
            '*' => (0, usize::MAX),
            '+' => (1, usize::MAX),
            '?' => (0, 1),
            '{' => {
                let close = self.c[self.i..].iter().position(|&c| c == '}');
                let Some(close) = close else {
                    return Err(format!("Unclosed counted closure at index {}", self.i));
                };
                let body: String = self.c[self.i + 1..self.i + close].iter().collect();
                let (a, b) = match body.split_once(',') {
                    Some((a, b)) => (a, Some(b)),
                    None => (body.as_str(), None),
                };
                let min: usize = a
                    .parse()
                    .map_err(|_| format!("Illegal repetition at index {}", self.i))?;
                let max = match b {
                    None => min,
                    Some("") => usize::MAX,
                    Some(b) => b
                        .parse()
                        .map_err(|_| format!("Illegal repetition at index {}", self.i))?,
                };
                self.i += close;
                (min, max)
            }
            _ => return Ok(atom),
        };
        self.i += 1;
        let mut greedy = true;
        if self.i < self.c.len() && self.c[self.i] == '?' {
            greedy = false;
            self.i += 1;
        }
        if matches!(atom, Node::Start | Node::End) {
            return Ok(atom);
        }
        Ok(Node::Repeat(Box::new(atom), min, max, greedy))
    }
    fn parse_atom(&mut self, depth: usize) -> Result<Node, String> {
        let ch = self.c[self.i];
        let start = self.i;
        self.i += 1;
        match ch {
            '.' => Ok(Node::Any),
            '^' => Ok(Node::Start),
            '$' => Ok(Node::End),
            '*' | '+' | '?' => Err(format!(
                "Dangling meta character '{}' near index {}",
                ch, start
            )),
            '(' => {
                let mut idx = None;
                if self.c.get(self.i) == Some(&'?') && self.c.get(self.i + 1) == Some(&':') {
                    self.i += 2;
                } else {
                    self.groups += 1;
                    idx = Some(self.groups);
                }
                let alts = self.parse_alts(depth + 1)?;
                if self.c.get(self.i) != Some(&')') {
                    return Err(format!("Unclosed group near index {}", self.c.len()));
                }
                self.i += 1;
                Ok(Node::Group(alts, idx))
            }
            ')' => Err(format!("Unmatched closing ')' near index {}", start)),
            '[' => {
                let mut neg = false;
                if self.c.get(self.i) == Some(&'^') {
                    neg = true;
                    self.i += 1;
                }
                let mut ranges = Vec::new();
                let mut first = true;
                loop {
                    let Some(&c) = self.c.get(self.i) else {
                        return Err(format!(
                            "Unclosed character class near index {}",
                            self.c.len().saturating_sub(1)
                        ));
                    };
                    self.i += 1;
                    if c == ']' && !first {
                        break;
                    }
                    first = false;
                    let mut lo = c;
                    if c == '\\' {
                        let Some(&e) = self.c.get(self.i) else {
                            return Err("Unexpected end after backslash".to_string());
                        };
                        self.i += 1;
                        if let Some((n, r)) = class_escape(e) {
                            if !n {
                                ranges.extend(r);
                            }
                            continue;
                        }
                        lo = e;
                    }
                    if self.c.get(self.i) == Some(&'-')
                        && self.c.get(self.i + 1).map(|&c| c != ']').unwrap_or(false)
                    {
                        let mut hi = self.c[self.i + 1];
                        self.i += 2;
                        if hi == '\\' {
                            hi = *self.c.get(self.i).ok_or("Unexpected end after backslash")?;
                            self.i += 1;
                        }
                        if hi < lo {
                            return Err("Illegal character range".to_string());
                        }
                        ranges.push((lo, hi));
                    } else {
                        ranges.push((lo, lo));
                    }
                }
                Ok(Node::Class(neg, ranges))
            }
            '\\' => {
                let Some(&e) = self.c.get(self.i) else {
                    return Err("Unexpected internal error or trailing backslash".to_string());
                };
                self.i += 1;
                if let Some((n, r)) = class_escape(e) {
                    return Ok(Node::Class(n, r));
                }
                Ok(Node::Lit(match e {
                    't' => '\t',
                    'n' => '\n',
                    'r' => '\r',
                    o => o,
                }))
            }
            c => Ok(Node::Lit(c)),
        }
    }
}

type Caps = Vec<Option<(usize, usize)>>;

impl Regex {
    pub fn new(pattern: &str) -> Result<Regex, String> {
        let chars: Vec<char> = pattern.chars().collect();
        let mut p = RParser {
            c: &chars,
            i: 0,
            groups: 0,
        };
        let alts = p.parse_alts(0)?;
        if p.i < chars.len() {
            return Err(format!("Unmatched closing ')' near index {}", p.i));
        }
        Ok(Regex {
            alts,
            groups: p.groups,
        })
    }

    /// Full-string match; returns capture groups (index 0 = whole match).
    pub fn full_match(&self, text: &str) -> Option<Vec<Option<String>>> {
        let s: Vec<char> = text.chars().collect();
        let mut caps: Caps = vec![None; self.groups + 1];
        let top = Node::Group(self.alts.clone(), None);
        let mut ok_caps = None;
        let matched = m_node(&s, &top, 0, &mut caps, &mut |p, c| {
            if p == s.len() {
                ok_caps = Some(c.clone());
                true
            } else {
                false
            }
        });
        if !matched {
            return None;
        }
        let caps = ok_caps?;
        let mut out = vec![Some(text.to_string())];
        for c in caps.iter().skip(1) {
            out.push(c.map(|(a, b)| s[a..b].iter().collect()));
        }
        Some(out)
    }
}

fn m_seq(
    s: &[char],
    nodes: &[Node],
    pos: usize,
    caps: &mut Caps,
    k: &mut dyn FnMut(usize, &mut Caps) -> bool,
) -> bool {
    let Some((first, rest)) = nodes.split_first() else {
        return k(pos, caps);
    };
    m_node(s, first, pos, caps, &mut |p, c| m_seq(s, rest, p, c, k))
}

fn m_node(
    s: &[char],
    n: &Node,
    pos: usize,
    caps: &mut Caps,
    k: &mut dyn FnMut(usize, &mut Caps) -> bool,
) -> bool {
    match n {
        Node::Lit(c) => s.get(pos) == Some(c) && k(pos + 1, caps),
        Node::Any => pos < s.len() && s[pos] != '\n' && k(pos + 1, caps),
        Node::Class(neg, ranges) => match s.get(pos) {
            Some(&c) => (ranges.iter().any(|&(a, b)| c >= a && c <= b) != *neg) && k(pos + 1, caps),
            None => false,
        },
        Node::Start => pos == 0 && k(pos, caps),
        Node::End => pos == s.len() && k(pos, caps),
        Node::Group(alts, idx) => {
            for alt in alts {
                let saved = caps.clone();
                let matched = m_seq(s, alt, pos, caps, &mut |p, c| {
                    let old = idx.map(|i| {
                        let o = c[i];
                        c[i] = Some((pos, p));
                        (i, o)
                    });
                    if k(p, c) {
                        true
                    } else {
                        if let Some((i, o)) = old {
                            c[i] = o;
                        }
                        false
                    }
                });
                if matched {
                    return true;
                }
                *caps = saved;
            }
            false
        }
        Node::Repeat(inner, min, max, greedy) => {
            m_rep(s, inner, *min, *max, *greedy, 0, pos, caps, k)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn m_rep(
    s: &[char],
    inner: &Node,
    min: usize,
    max: usize,
    greedy: bool,
    count: usize,
    pos: usize,
    caps: &mut Caps,
    k: &mut dyn FnMut(usize, &mut Caps) -> bool,
) -> bool {
    if count < min {
        return m_node(s, inner, pos, caps, &mut |p, c| {
            m_rep(s, inner, min, max, greedy, count + 1, p, c, k)
        });
    }
    if greedy {
        if count < max
            && m_node(s, inner, pos, caps, &mut |p, c| {
                if p == pos {
                    return false;
                }
                m_rep(s, inner, min, max, greedy, count + 1, p, c, k)
            })
        {
            return true;
        }
        k(pos, caps)
    } else {
        if k(pos, caps) {
            return true;
        }
        count < max
            && m_node(s, inner, pos, caps, &mut |p, c| {
                if p == pos {
                    return false;
                }
                m_rep(s, inner, min, max, greedy, count + 1, p, c, k)
            })
    }
}

fn percent_encode_segment(s: &str) -> String {
    let mut out = String::new();
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn escape_url_path(path: &str) -> String {
    path.split('/')
        .map(percent_encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

fn expand_replacement(repl: &str, caps: &[Option<String>]) -> String {
    let mut out = String::new();
    let mut rest = repl;

    while !rest.is_empty() {
        if rest.starts_with('\\') && rest.len() >= 2 {
            out.push(rest.chars().nth(1).unwrap());
            rest = &rest[2..];
        } else if let Some(after) = rest.strip_prefix("${urlencode($") {
            if let Some(end) = after.find(")}") {
                let num_str = &after[..end];
                if let Ok(n) = num_str.parse::<usize>() {
                    if let Some(Some(g)) = caps.get(n) {
                        out.push_str(&escape_url_path(g));
                    }
                }
                rest = &after[end + 2..];
                continue;
            } else {
                out.push('$');
                rest = &rest[1..];
            }
        } else if let Some(after) = rest.strip_prefix("urlencode($") {
            if let Some(end) = after.find(')') {
                let num_str = &after[..end];
                if let Ok(n) = num_str.parse::<usize>() {
                    if let Some(Some(g)) = caps.get(n) {
                        out.push_str(&escape_url_path(g));
                    }
                }
                rest = &after[end + 1..];
                continue;
            } else {
                out.push('u');
                rest = &rest[1..];
            }
        } else if let Some(after) = rest.strip_prefix("${") {
            if let Some(end) = after.find('}') {
                let num_str = &after[..end];
                if let Ok(n) = num_str.parse::<usize>() {
                    if let Some(Some(g)) = caps.get(n) {
                        out.push_str(g);
                    }
                }
                rest = &after[end + 1..];
                continue;
            } else {
                out.push('$');
                rest = &rest[1..];
            }
        } else if rest.starts_with('$') && rest.len() >= 2 && rest.as_bytes()[1].is_ascii_digit() {
            let mut end = 1;
            while end < rest.len() && rest.as_bytes()[end].is_ascii_digit() {
                end += 1;
            }
            if let Ok(n) = rest[1..end].parse::<usize>() {
                if let Some(Some(g)) = caps.get(n) {
                    out.push_str(g);
                }
            }
            rest = &rest[end..];
        } else {
            let ch = rest.chars().next().unwrap();
            out.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }
    out
}

// ===========================================================================
// Downloader config (--downloader_config)
// ===========================================================================

#[derive(Clone, Debug, Default)]
pub struct UrlRewriter {
    allow: Vec<String>,
    block: Vec<String>,
    rewrites: Vec<(String, Regex, String)>,
    pub all_blocked_message: Option<String>,
}

impl UrlRewriter {
    /// Parse the contents of a downloader config file. `path` is used for error locations.
    pub fn parse(path: &str, contents: &str) -> Result<UrlRewriter, String> {
        let mut r = UrlRewriter::default();
        r.parse_into(path, contents)?;
        Ok(r)
    }

    /// Adds the directives of one config file (`UrlRewriterConfig.parseConfig`); several files
    /// share the allow/block/rewrite lists and the single `all_blocked_message`.
    pub fn parse_into(&mut self, path: &str, contents: &str) -> Result<(), String> {
        let r = self;
        for (n, line) in contents.lines().enumerate() {
            // `Location.toString` omits a zero column.
            let loc = format!("{}:{}", path, n + 1);
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.is_empty() || parts[0].starts_with('#') {
                continue;
            }
            match parts[0] {
                "allow" | "block" => {
                    if parts.len() != 2 {
                        return Err(format!(
                            "{}: Only the host name is allowed after `{}`: {}",
                            loc, parts[0], line
                        ));
                    }
                    let h = parts[1].to_ascii_lowercase();
                    if parts[0] == "allow" {
                        r.allow.push(h);
                    } else {
                        r.block.push(h);
                    }
                }
                "rewrite" => {
                    if parts.len() != 3 {
                        return Err(format!(
                            "{}: Only the matching pattern and rewrite pattern is allowed after `rewrite`: {}",
                            loc, line
                        ));
                    }
                    let re = Regex::new(parts[1]).map_err(|e| {
                        format!(
                            "{}: Invalid regex in `rewrite`: {} in `{}`",
                            loc,
                            e.to_string().replace(" near index ", " at index "),
                            parts[1]
                        )
                    })?;
                    r.rewrites
                        .push((parts[1].to_string(), re, parts[2].to_string()));
                }
                "all_blocked_message" => {
                    if parts.len() == 1 {
                        return Err(format!(
                            "{}: all_blocked_message must be followed by a message",
                            loc
                        ));
                    }
                    if r.all_blocked_message.is_some() {
                        return Err(format!(
                            "{}: At most one all_blocked_message directive is allowed",
                            loc
                        ));
                    }
                    let idx =
                        line.find("all_blocked_message").unwrap() + "all_blocked_message".len() + 1;
                    r.all_blocked_message = Some(line.get(idx..).unwrap_or("").to_string());
                }
                _ => return Err(format!("{}: Unable to parse: {}", loc, line)),
            }
        }
        Ok(())
    }

    /// `UrlRewriter.getDownloaderUrlRewriter`: `configs` are relative to the workspace root; an
    /// empty entry disables the rewriter.
    pub fn from_config_paths(
        workspace: &Path,
        configs: &[String],
    ) -> Result<Option<UrlRewriter>, String> {
        if configs.is_empty() || configs.iter().any(|c| c.is_empty()) {
            return Ok(None);
        }
        let abs: Vec<PathBuf> = configs
            .iter()
            .map(|c| {
                if Path::new(c).is_absolute() {
                    PathBuf::from(c)
                } else {
                    workspace.join(c)
                }
            })
            .collect();
        let missing: Vec<String> = abs
            .iter()
            .filter(|p| !p.exists())
            .map(|p| p.display().to_string())
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "Failed to parse downloader config: Unable to find downloader config file {}",
                missing.join(",")
            ));
        }
        let mut r = UrlRewriter::default();
        for (c, p) in configs.iter().zip(&abs) {
            let text = fs::read_to_string(p)
                .map_err(|e| format!("Failed to parse downloader config: {e}"))?;
            r.parse_into(c, &text)
                .map_err(|e| format!("Failed to parse downloader config at {e}"))?;
        }
        Ok(Some(r))
    }

    pub fn parse_file(path: &Path) -> Result<UrlRewriter, String> {
        let s = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read downloader config {}: {}", path.display(), e))?;
        UrlRewriter::parse(&path.to_string_lossy(), &s)
    }

    fn host_matches(url: &Url, host: &str) -> bool {
        let h = url.host.to_ascii_lowercase();
        h == host || h.ends_with(&format!(".{}", host))
    }

    /// Apply rewrite, then allow/block. May return an empty list.
    pub fn amend(&self, urls: &[String]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for u in urls {
            let parsed = match Url::parse(u) {
                Ok(p) => p,
                Err(_) => {
                    out.push(u.clone());
                    continue;
                }
            };
            if parsed.scheme != "http" && parsed.scheme != "https" {
                out.push(u.clone());
                continue;
            }
            let mut without_scheme = u[parsed.scheme.len() + 3..].to_string();
            // lower-case host
            if let Some(pos) = without_scheme.find(&parsed.host) {
                if pos == parsed.userinfo.as_ref().map(|x| x.len() + 1).unwrap_or(0) {
                    let lower = parsed.host.to_ascii_lowercase();
                    without_scheme.replace_range(pos..pos + parsed.host.len(), &lower);
                }
            }
            let mut candidates: Vec<String> = Vec::new();
            let mut matched = false;
            for (_, re, repl) in &self.rewrites {
                if let Some(caps) = re.full_match(&without_scheme) {
                    matched = true;
                    let expanded = expand_replacement(repl, &caps);
                    let lower_exp = expanded.to_ascii_lowercase();
                    let s = if lower_exp.starts_with("http://") || lower_exp.starts_with("https://")
                    {
                        expanded
                    } else {
                        format!("{}://{}", parsed.scheme, expanded)
                    };
                    if !candidates.contains(&s) {
                        candidates.push(s);
                    }
                }
            }
            if !matched {
                candidates.push(u.clone());
            }
            for c in candidates {
                let cu = match Url::parse(&c) {
                    Ok(x) => x,
                    Err(_) => continue,
                };
                if self.allow.iter().any(|h| Self::host_matches(&cu, h)) {
                    out.push(c);
                    continue;
                }
                let blocked = self
                    .block
                    .iter()
                    .any(|h| h == "*" || Self::host_matches(&cu, h));
                if !blocked {
                    out.push(c);
                }
            }
        }
        out
    }

    pub fn blocked_message(&self, original: &[String]) -> String {
        let mut m = format!(
            "Configured URL rewriter blocked all URLs: [{}]",
            original.join(", ")
        );
        if let Some(x) = &self.all_blocked_message {
            m.push_str(" - ");
            m.push_str(x);
        }
        m
    }
}

// ===========================================================================
// Repository cache
// ===========================================================================

/// `<root>/content_addressable/<algo>/<hex>/file` plus `id-<hash(canonical_id)>` markers.
#[derive(Clone, Debug)]
pub struct RepositoryCache {
    pub root: PathBuf,
}

impl RepositoryCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        RepositoryCache { root: root.into() }
    }

    pub fn entry_dir(&self, c: &Checksum) -> PathBuf {
        self.root
            .join("content_addressable")
            .join(c.algorithm.name())
            .join(c.hex())
    }

    pub fn entry_file(&self, c: &Checksum) -> PathBuf {
        self.entry_dir(c).join("file")
    }

    fn id_file(&self, c: &Checksum, canonical_id: &str) -> PathBuf {
        let h = crate::hash::to_hex(&c.algorithm.digest(canonical_id.as_bytes()));
        self.entry_dir(c).join(format!("id-{}", h))
    }

    /// Returns the cached file if present (and, when `canonical_id` is non-empty, if it was
    /// stored with that id). Verifies the checksum of the cached content.
    pub fn get(&self, c: &Checksum, canonical_id: &str) -> Result<Option<PathBuf>, String> {
        let f = self.entry_file(c);
        if !f.is_file() {
            return Ok(None);
        }
        let data = fs::read(&f).map_err(|e| {
            format!(
                "Failed to read {}: {}\n\nPlease delete the directory {} and try again.",
                f.display(),
                e,
                self.entry_dir(c).display()
            )
        })?;
        if let Err(e) = c.verify(&data) {
            return Err(format!(
                "{}\n\nPlease delete the directory {} and try again.",
                e,
                self.entry_dir(c).display()
            ));
        }
        if !canonical_id.is_empty() && !self.id_file(c, canonical_id).exists() {
            return Ok(None);
        }
        Ok(Some(f))
    }

    pub fn put(&self, c: &Checksum, data: &[u8], canonical_id: &str) -> Result<(), String> {
        let dir = self.entry_dir(c);
        fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create {}: {}", dir.display(), e))?;
        let tmp = dir.join(format!("tmp-{}-{}", std::process::id(), next_counter()));
        fs::write(&tmp, data).map_err(|e| format!("Failed to write {}: {}", tmp.display(), e))?;
        fs::rename(&tmp, dir.join("file"))
            .map_err(|e| format!("Failed to store cache entry {}: {}", dir.display(), e))?;
        if !canonical_id.is_empty() {
            let _ = fs::write(self.id_file(c, canonical_id), b"");
        }
        Ok(())
    }
}

fn next_counter() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static C: AtomicU64 = AtomicU64::new(0);
    C.fetch_add(1, Ordering::Relaxed)
}

// ===========================================================================
// Downloader
// ===========================================================================

/// One download request (what `repository_ctx.download` supplies).
#[derive(Clone, Debug, Default)]
pub struct DownloadRequest {
    pub urls: Vec<String>,
    pub output: PathBuf,
    /// Hex sha256 (may be empty).
    pub sha256: String,
    /// Subresource Integrity string (may be empty).
    pub integrity: String,
    pub headers: BTreeMap<String, String>,
    /// url -> {"type": "basic", "login", "password"} or {"type": "pattern", "pattern", "login", "password"}.
    pub auth: BTreeMap<String, BTreeMap<String, String>>,
    pub executable: bool,
    pub canonical_id: String,
    /// Used in error messages ("Failed to download <context>: download is disabled.").
    pub context: String,
    /// Optional archive type (`gz`, `tgz`, `tar.gz`, ...) used to suppress double gunzip.
    pub archive_type: String,
}

#[derive(Clone, Debug)]
pub struct DownloadResult {
    pub path: PathBuf,
    pub sha256: String,
    pub integrity: String,
    /// URL (after rewriting) that served the file; empty for cache/distdir/pre-existing hits.
    pub url: String,
    pub from_cache: bool,
}

#[derive(Clone, Debug)]
pub struct Downloader {
    pub repo_cache: Option<RepositoryCache>,
    pub distdirs: Vec<PathBuf>,
    pub disable_download: bool,
    /// Extra attempts after the first failure for transient errors (Bazel default: 5).
    pub retries: u32,
    pub retry_wait: Duration,
    pub rewriter: Option<UrlRewriter>,
    pub timeout: Duration,
    pub max_redirects: usize,
    pub curl_binary: String,
    pub credential_helpers: Vec<(String, PathBuf)>,
    pub client_env: BTreeMap<String, String>,
}

impl Default for Downloader {
    fn default() -> Self {
        Downloader {
            repo_cache: None,
            distdirs: Vec::new(),
            disable_download: false,
            retries: 5,
            retry_wait: Duration::from_millis(100),
            rewriter: None,
            timeout: Duration::from_secs(60),
            max_redirects: 20,
            curl_binary: "curl".to_string(),
            credential_helpers: Vec::new(),
            client_env: BTreeMap::new(),
        }
    }
}

struct FetchError {
    msg: String,
    retryable: bool,
}

fn fe(msg: impl Into<String>, retryable: bool) -> FetchError {
    FetchError {
        msg: msg.into(),
        retryable,
    }
}

/// Build the effective checksum from `sha256`/`integrity`.
pub fn request_checksum(sha256: &str, integrity: &str) -> Result<Option<Checksum>, String> {
    match (sha256.is_empty(), integrity.is_empty()) {
        (true, true) => Ok(None),
        (false, false) => Err("Expected either 'sha256' or 'integrity', but not both".to_string()),
        (false, true) => {
            let h = sha256.trim();
            if h.len() != 64 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(format!("Invalid SHA-256 checksum '{}'", sha256));
            }
            Checksum::sha256_hex(h).map(Some)
        }
        (true, false) => Checksum::parse_integrity(integrity).map(Some),
    }
}

impl Downloader {
    pub fn new() -> Self {
        Downloader::default()
    }

    /// Download according to `req`, writing the result to `req.output`.
    pub fn download(&self, req: &DownloadRequest) -> Result<DownloadResult, String> {
        let checksum = request_checksum(&req.sha256, &req.integrity)?;
        let context = if req.context.is_empty() {
            req.urls.first().cloned().unwrap_or_default()
        } else {
            req.context.clone()
        };

        let finish =
            |data: &[u8], url: String, from_cache: bool| -> Result<DownloadResult, String> {
                let integrity = if !req.integrity.is_empty() {
                    req.integrity.clone()
                } else if let Some(c) = &checksum {
                    c.to_integrity()
                } else {
                    format!("sha256-{}", base64_encode(&crate::hash::sha256(data)))
                };
                Ok(DownloadResult {
                    path: req.output.clone(),
                    sha256: crate::hash::sha256_hex(data),
                    integrity,
                    url,
                    from_cache,
                })
            };

        // 1. Already present with the right checksum.
        let mut cache_err: Option<String> = None;
        if let Some(c) = &checksum {
            if let Ok(existing) = fs::read(&req.output) {
                if c.matches(&existing) {
                    self.set_mode(&req.output, req.executable)?;
                    return finish(&existing, String::new(), true);
                }
            }
            // 2. Repository cache (a corrupt entry falls through to re-download and repair).
            if let Some(cache) = &self.repo_cache {
                match cache.get(c, &req.canonical_id) {
                    Ok(Some(f)) => {
                        if let Ok(data) = fs::read(&f) {
                            self.write_output(&req.output, &data, req.executable)?;
                            // `DownloadCacheHitEvent(context, cacheKey, mainUrl)`.
                            return finish(
                                &data,
                                req.urls.first().cloned().unwrap_or_default(),
                                true,
                            );
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        cache_err = Some(e);
                    }
                }
            }
            // 3. distdir.
            let names = candidate_file_names(&req.urls, &req.output);
            for dir in &self.distdirs {
                for n in &names {
                    let cand = dir.join(n);
                    if !cand.is_file() {
                        continue;
                    }
                    if let Ok(data) = fs::read(&cand) {
                        if c.matches(&data) {
                            if let Some(cache) = &self.repo_cache {
                                if let Err(e) = cache.put(c, &data, &req.canonical_id) {
                                    eprintln!(
                                        "WARNING: Failed to copy {} to repository cache: {}",
                                        cand.display(),
                                        e
                                    );
                                }
                            }
                            self.write_output(&req.output, &data, req.executable)?;
                            return finish(&data, String::new(), true);
                        }
                    }
                }
            }
        }

        if self.disable_download {
            if let Some(ce) = cache_err {
                return Err(ce);
            }
            return Err(format!(
                "Failed to download {}: download is disabled.",
                context
            ));
        }

        let urls: Vec<String> = match &self.rewriter {
            Some(r) => {
                let u = r.amend(&req.urls);
                if u.is_empty() && !req.urls.is_empty() {
                    return Err(r.blocked_message(&req.urls));
                }
                u
            }
            None => req.urls.clone(),
        };
        if urls.is_empty() {
            return Err(format!(
                "Error downloading [] to {}: no URLs given",
                req.output.display()
            ));
        }

        let mut attempt = 0u32;
        let mut wait = self.retry_wait;
        loop {
            let mut last_err = String::new();
            let mut any_retryable = false;
            for url in &urls {
                match self.fetch_one(url, req, attempt) {
                    Ok(data) => {
                        if let Some(c) = &checksum {
                            if let Err(e) = c.verify(&data) {
                                let e = if !req.integrity.is_empty() {
                                    format!(
                                        "Checksum was {}-{} but wanted {}",
                                        c.algorithm.name(),
                                        base64_encode(&c.algorithm.digest(&data)),
                                        c.to_integrity()
                                    )
                                } else {
                                    e
                                };
                                record_warning(url, &e);
                                last_err = e;
                                continue;
                            }
                        }
                        if req.output.is_dir() {
                            let e = format!("{} (Is a directory)", req.output.display());
                            record_warning(url, &e);
                            last_err = e;
                            continue;
                        }
                        self.write_output(&req.output, &data, req.executable)?;
                        if let Some(cache) = &self.repo_cache {
                            let key = match &checksum {
                                Some(c) => c.clone(),
                                None => Checksum {
                                    algorithm: Algorithm::Sha256,
                                    digest: crate::hash::sha256(&data).to_vec(),
                                },
                            };
                            if let Err(e) = cache.put(&key, &data, &req.canonical_id) {
                                eprintln!(
                                    "WARNING: Failed to copy {} to repository cache: {}",
                                    req.output.display(),
                                    e
                                );
                            }
                        }
                        return finish(&data, url.clone(), false);
                    }
                    Err(e) => {
                        any_retryable |= e.retryable;
                        // Retries of HTTP-level failures are silent (`HttpConnector`); `DownloadManager`'s own
                        // retries (`UnknownHostException`) call the downloader again, which reports every URL
                        // failure of every pass (`HttpDownloader.recordFailure`).
                        let outer =
                            e.msg.starts_with("Unknown host") || e.msg == "Connection refused";
                        if outer || !(e.retryable && attempt < self.retries) {
                            record_warning(url, &e.msg);
                        }
                        last_err = e.msg;
                    }
                }
            }
            if any_retryable && attempt < self.retries {
                if !last_err.starts_with("Unknown host") && last_err != "Connection refused" {
                    std::thread::sleep(wait);
                    wait = wait.saturating_mul(2);
                }
                attempt += 1;
                continue;
            }
            if let Some(ce) = cache_err {
                return Err(format!(
                    "Error downloading [{}] to {}: {} ({})",
                    urls.join(", "),
                    req.output.display(),
                    last_err,
                    ce
                ));
            }
            return Err(format!(
                "Error downloading [{}] to {}: {}",
                urls.join(", "),
                req.output.display(),
                last_err
            ));
        }
    }

    fn set_mode(&self, p: &Path, exec: bool) -> Result<(), String> {
        #[cfg(unix)]
        {
            let mode = if exec { 0o755 } else { 0o644 };
            fs::set_permissions(p, fs::Permissions::from_mode(mode))
                .map_err(|e| format!("Failed to chmod {}: {}", p.display(), e))
        }
        #[cfg(not(unix))]
        {
            let _ = (p, exec);
            Ok(())
        }
    }

    fn write_output(&self, out: &Path, data: &[u8], exec: bool) -> Result<(), String> {
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
        }
        let tmp = out.with_extension(format!("zel-tmp-{}-{}", std::process::id(), next_counter()));
        fs::write(&tmp, data).map_err(|e| format!("Failed to write {}: {}", tmp.display(), e))?;
        if let Err(e) = fs::rename(&tmp, out) {
            let _ = fs::remove_file(&tmp);
            return Err(format!("Failed to write {}: {}", out.display(), e));
        }
        self.set_mode(out, exec)
    }

    fn connect_timeout_for(&self, attempt: u32) -> Duration {
        let secs = (1u64 << attempt.min(4)).min(10);
        Duration::from_secs(secs).min(self.timeout)
    }

    fn headers_for(
        &self,
        url: &str,
        req: &DownloadRequest,
        parsed: &Url,
    ) -> Result<Vec<(String, String)>, FetchError> {
        let mut h: Vec<(String, String)> = req
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let set_header = |hdrs: &mut Vec<(String, String)>, key: String, val: String| {
            hdrs.retain(|(ek, _)| !ek.eq_ignore_ascii_case(&key));
            hdrs.push((key, val));
        };
        // Mirrors `CredentialHelperCredentials.getRequestMetadata` +
        // `HttpConnectorMultiplexer.getHeaderFunction`:
        // - When a `--credential_helper` matches `url`, its headers override matching keys in
        //   `req.headers` (`buildKeepingLast`) and fallback credentials (`req.auth`, URL
        //   userinfo, `.netrc`) are never consulted — even if the helper returned `{}` or
        //   only non-`Authorization` headers; helper failures propagate as `FetchError`.
        // - Only when no `--credential_helper` matches `url` (`Ok(None)`) do we fall back to
        //   `req.auth` (`StaticCredentials`, overwriting `baseHeaders`), URL userinfo, and `.netrc`.
        if let Some(extra) = crate::netrc::resolve_credential_helper_headers(
            url,
            &self.credential_helpers,
            &self.client_env,
        )
        .map_err(|e| fe(e, false))?
        {
            for (k, _) in &extra {
                h.retain(|(existing, _)| !existing.eq_ignore_ascii_case(k));
            }
            h.extend(extra);
            return Ok(h);
        }
        let matched_auth = req.auth.get(url).or_else(|| {
            req.auth
                .iter()
                .find(|(k, _)| java_uri_equals(k, url))
                .map(|(_, v)| v)
        });
        if let Some(a) = matched_auth {
            let login = a.get("login").cloned().unwrap_or_default();
            let password = a.get("password").cloned().unwrap_or_default();
            match a.get("type").map(|s| s.as_str()) {
                Some("basic") => set_header(
                    &mut h,
                    "Authorization".into(),
                    format!(
                        "Basic {}",
                        base64_encode(format!("{}:{}", login, password).as_bytes())
                    ),
                ),
                Some("netrc") => {
                    let val = if !login.is_empty() {
                        format!(
                            "Basic {}",
                            base64_encode(format!("{}:{}", login, password).as_bytes())
                        )
                    } else {
                        format!("Bearer {}", password)
                    };
                    set_header(&mut h, "Authorization".into(), val);
                }
                Some("pattern") => {
                    let p = a.get("pattern").cloned().unwrap_or_default();
                    set_header(
                        &mut h,
                        "Authorization".into(),
                        p.replace("<login>", &login)
                            .replace("<password>", &password),
                    );
                }
                _ => {}
            }
            return Ok(h);
        }
        let has_auth = |hdrs: &[(String, String)]| {
            hdrs.iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        };
        if !has_auth(&h) {
            if let Some(ui) = &parsed.userinfo {
                h.push((
                    "Authorization".into(),
                    format!("Basic {}", base64_encode(percent_decode(ui).as_bytes())),
                ));
            }
        }
        if !has_auth(&h) {
            for (k, v) in crate::netrc::resolve_netrc_headers(url, &self.client_env) {
                h.push((k, v));
            }
        }
        Ok(h)
    }

    fn fetch_one(
        &self,
        url: &str,
        req: &DownloadRequest,
        attempt: u32,
    ) -> Result<Vec<u8>, FetchError> {
        let parsed = Url::parse(url).map_err(|e| fe(e, false))?;
        match parsed.scheme.as_str() {
            "file" => {
                let p = file_url_to_path(&parsed).map_err(|e| fe(e, false))?;
                fs::read(&p)
                    .map_err(|e| fe(format!("{} ({})", p.display(), java_io_reason(&e)), false))
            }
            "http" => self.http_get(&parsed, url, req, attempt),
            "https" => {
                let headers = self.headers_for(url, req, &parsed)?;
                self.curl_get(&parsed.to_url_string_without_userinfo(), &headers, attempt)
            }
            other => Err(fe(
                format!("Unsupported URL scheme '{}' in {}", other, url),
                false,
            )),
        }
    }

    fn curl_get(
        &self,
        url: &str,
        headers: &[(String, String)],
        attempt: u32,
    ) -> Result<Vec<u8>, FetchError> {
        let tmp = std::env::temp_dir().join(format!(
            "zel-curl-{}-{}",
            std::process::id(),
            next_counter()
        ));
        let connect_secs = self.connect_timeout_for(attempt).as_secs_f64().max(0.1);
        let connect_str = if (connect_secs.fract()).abs() < 1e-6 {
            format!("{}", connect_secs as u64)
        } else {
            format!("{connect_secs:.3}")
        };
        let mut cmd = Command::new(&self.curl_binary);
        cmd.args([
            "-fsSL",
            "--connect-timeout",
            &connect_str,
            "--max-redirs",
            &self.max_redirects.to_string(),
        ]);
        let speed_secs = self.timeout.as_secs();
        if speed_secs > 0 {
            cmd.args([
                "--speed-time",
                &speed_secs.to_string(),
                "--speed-limit",
                "1",
            ]);
        }
        if !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
        {
            cmd.args(["-A", "bazel/9.3.2"]);
        }
        if let Some(p) = select_proxy_env("https", &self.client_env) {
            if p.is_empty() {
                cmd.args(["--noproxy", "*"]);
            } else {
                cmd.args(["--proxy", &p]);
            }
        }
        if let Some(np) = self
            .client_env
            .get("no_proxy")
            .or_else(|| self.client_env.get("NO_PROXY"))
        {
            cmd.args(["--noproxy", np]);
        }
        cmd.arg("-o").arg(&tmp);
        for (k, v) in headers {
            cmd.arg("-H").arg(format!("{}: {}", k, v));
        }
        cmd.arg(url);
        let out = crate::netrc::retry_etxtbsy(|| cmd.output()).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                fe(
                    format!(
                        "Cannot download {}: the system '{}' binary is required for https URLs but was not found",
                        url, self.curl_binary
                    ),
                    false,
                )
            } else {
                fe(format!("Failed to run {}: {}", self.curl_binary, e), false)
            }
        })?;
        if !out.status.success() {
            let _ = fs::remove_file(&tmp);
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let code = out.status.code().unwrap_or(-1);
            if code == 6 {
                // `UnknownHostException` (retried by `DownloadManager`).
                let host = url
                    .split("://")
                    .nth(1)
                    .and_then(|r| r.split(['/', ':']).next())
                    .unwrap_or("");
                return Err(fe(format!("Unknown host: {host}"), false));
            }
            if code == 7 {
                return Err(fe("Connection refused".to_string(), true));
            }
            let unrecoverable_5xx =
                stderr.contains("error: 501") || stderr.contains("error: 505");
            let retryable = (!unrecoverable_5xx && stderr.contains("error: 5"))
                || stderr.contains("error: 403")
                || stderr.contains("error: 408")
                || stderr.contains("error: 429")
                || matches!(code, 6 | 7 | 18 | 28 | 35 | 52 | 55 | 56);
            return Err(fe(format!("GET {} failed: {}", url, stderr), retryable));
        }
        let data =
            fs::read(&tmp).map_err(|e| fe(format!("Failed to read curl output: {}", e), false));
        let _ = fs::remove_file(&tmp);
        data
    }

    fn http_get(
        &self,
        start: &Url,
        orig_url: &str,
        req: &DownloadRequest,
        attempt: u32,
    ) -> Result<Vec<u8>, FetchError> {
        let mut url = start.clone();
        let mut current_url_str = orig_url.to_string();
        let mut visited: Vec<String> = vec![start.to_url_string()];
        let connect_timeout = self.connect_timeout_for(attempt);
        for _ in 0..=self.max_redirects {
            let headers = self.headers_for(&current_url_str, req, &url)?;
            let proxy = resolve_http_proxy(&url.host, &self.client_env);
            let (connect_host, connect_port, request_target, proxy_auth) = match &proxy {
                Some(px) => (
                    px.host.clone(),
                    px.port.unwrap_or(80),
                    url.to_url_string_without_userinfo(),
                    px.userinfo.as_ref().and_then(|ui| {
                        ui.contains(':').then(|| {
                            format!("Basic {}", base64_encode(percent_decode(ui).as_bytes()))
                        })
                    }),
                ),
                None => (
                    url.host.clone(),
                    url.port.unwrap_or(80),
                    url.path_and_query.clone(),
                    None,
                ),
            };
            let addr_str = format!("{}:{}", connect_host, connect_port);
            let addrs: Vec<_> = addr_str
                .to_socket_addrs()
                .map_err(|_| fe(format!("Unknown host: {}", connect_host), false))?
                .collect();
            let mut stream = None;
            let mut last = String::from("no addresses");
            for a in addrs {
                match TcpStream::connect_timeout(&a, connect_timeout) {
                    Ok(s) => {
                        stream = Some(s);
                        break;
                    }
                    Err(e) => {
                        last = if e.kind() == std::io::ErrorKind::ConnectionRefused {
                            "Connection refused".to_string()
                        } else {
                            format!("{}: {}", addr_str, e)
                        }
                    }
                }
            }
            let stream = stream.ok_or_else(|| fe(last, true))?;
            let _ = stream.set_read_timeout(Some(self.timeout));
            let _ = stream.set_write_timeout(Some(self.timeout));
            let mut w = stream.try_clone().map_err(|e| fe(e.to_string(), true))?;
            let mut reqs = format!(
                "GET {} HTTP/1.1\r\nHost: {}\r\n",
                request_target,
                url.host_header()
            );
            if !headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
            {
                reqs.push_str("User-Agent: bazel/9.3.2\r\n");
            }
            reqs.push_str("Accept: */*\r\nConnection: close\r\n");
            if let Some(pa) = proxy_auth {
                reqs.push_str(&format!("Proxy-Authorization: {}\r\n", pa));
            }
            for (k, v) in &headers {
                if k.contains(['\r', '\n']) || v.contains(['\r', '\n']) {
                    return Err(fe("Invalid header containing newline", false));
                }
                reqs.push_str(&format!("{}: {}\r\n", k, v));
            }
            reqs.push_str("\r\n");
            w.write_all(reqs.as_bytes())
                .map_err(|e| fe(format!("{}: {}", addr_str, e), true))?;
            let mut r = BufReader::new(stream);
            let mut line = String::new();
            r.read_line(&mut line)
                .map_err(|e| fe(format!("{}: {}", addr_str, e), true))?;
            let status_line = line.trim_end().to_string();
            let mut it = status_line.splitn(3, ' ');
            let version = it.next().unwrap_or("");
            if !version.starts_with("HTTP/") {
                return Err(fe(
                    format!("Invalid HTTP response from {}: '{}'", addr_str, status_line),
                    true,
                ));
            }
            let code: u16 = it
                .next()
                .and_then(|c| c.parse().ok())
                .ok_or_else(|| fe(format!("Invalid HTTP status line: '{}'", status_line), true))?;
            let reason = it.next().unwrap_or("").to_string();
            let mut hdrs: Vec<(String, String)> = Vec::new();
            loop {
                line.clear();
                let n = r
                    .read_line(&mut line)
                    .map_err(|e| fe(format!("{}: {}", addr_str, e), true))?;
                if n == 0 || line == "\r\n" || line == "\n" {
                    break;
                }
                if let Some((k, v)) = line.split_once(':') {
                    hdrs.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
                }
            }
            let get = |k: &str| hdrs.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v.clone());
            if matches!(code, 301 | 302 | 303 | 307 | 308) {
                let loc = get("location").ok_or_else(|| {
                    fe(format!("Redirect {} without Location header", code), false)
                })?;
                let next = url.join(&loc).map_err(|e| fe(e, false))?;
                let next_str = next.to_url_string();
                match next.scheme.as_str() {
                    "http" => {}
                    "https" => {
                        let next_headers = self.headers_for(&next_str, req, &next)?;
                        return self.curl_get(
                            &next.to_url_string_without_userinfo(),
                            &next_headers,
                            attempt,
                        );
                    }
                    s => return Err(fe(format!("Redirect to unsupported scheme '{}'", s), false)),
                }
                if visited.contains(&next_str) {
                    return Err(fe("Redirect loop detected", false));
                }
                visited.push(next_str.clone());
                current_url_str = next_str;
                url = next;
                continue;
            }
            if code != 200 {
                let retryable = code == 403
                    || code == 408
                    || code == 429
                    || ((500..600).contains(&code) && code != 501 && code != 505);
                return Err(fe(format!("GET returned {} {}", code, reason), retryable));
            }
            let chunked = get("transfer-encoding")
                .map(|v| v.to_ascii_lowercase().contains("chunked"))
                .unwrap_or(false);
            let mut body = Vec::new();
            if chunked {
                loop {
                    line.clear();
                    r.read_line(&mut line)
                        .map_err(|e| fe(format!("Error reading chunk: {}", e), true))?;
                    let size_s = line.trim().split(';').next().unwrap_or("").to_string();
                    let size = usize::from_str_radix(&size_s, 16)
                        .map_err(|_| fe(format!("Invalid chunk size '{}'", size_s), true))?;
                    if size == 0 {
                        break;
                    }
                    let start = body.len();
                    body.resize(start + size, 0);
                    r.read_exact(&mut body[start..])
                        .map_err(|e| fe(format!("Premature end of chunked body: {}", e), true))?;
                    let mut crlf = [0u8; 2];
                    r.read_exact(&mut crlf)
                        .map_err(|e| fe(format!("Premature end of chunked body: {}", e), true))?;
                }
            } else if let Some(cl) = get("content-length") {
                let n: usize = cl
                    .parse()
                    .map_err(|_| fe(format!("Invalid Content-Length '{}'", cl), false))?;
                let mut got = Vec::new();
                r.take(n as u64)
                    .read_to_end(&mut got)
                    .map_err(|e| fe(format!("Error reading body: {}", e), true))?;
                if got.len() != n {
                    return Err(fe(
                        format!(
                            "Download size mismatch: Content-Length was {} but got {} bytes",
                            n,
                            got.len()
                        ),
                        true,
                    ));
                }
                body = got;
            } else {
                r.read_to_end(&mut body)
                    .map_err(|e| fe(format!("Error reading body: {}", e), true))?;
            }
            let is_gzip_ce = get("content-encoding")
                .is_some_and(|v| v == "gzip" || v == "x-gzip");
            let orig_path = Url::parse(orig_url)
                .map(|u| u.path().to_string())
                .unwrap_or_default();
            let skip_gunzip = is_gz_extension(&orig_path)
                || is_gz_extension(url.path())
                || is_gz_extension(&req.archive_type);
            if is_gzip_ce && !skip_gunzip {
                body = crate::archive::gunzip(&body)
                    .map_err(|e| fe(format!("Bad gzip content-encoding: {}", e), false))?;
            }
            return Ok(body);
        }
        Err(fe(
            format!(
                "Too many redirects (more than {}) fetching {}",
                self.max_redirects,
                start.to_url_string()
            ),
            false,
        ))
    }
}

fn is_gz_extension(path_or_type: &str) -> bool {
    let ext = match path_or_type.rsplit_once('.') {
        Some((_, e)) => e,
        None => path_or_type,
    };
    ext.eq_ignore_ascii_case("gz") || ext.eq_ignore_ascii_case("tgz")
}

fn select_proxy_env(scheme: &str, client_env: &BTreeMap<String, String>) -> Option<String> {
    if scheme == "https" {
        client_env
            .get("https_proxy")
            .or_else(|| client_env.get("HTTPS_PROXY"))
            .cloned()
    } else {
        client_env
            .get("http_proxy")
            .or_else(|| client_env.get("HTTP_PROXY"))
            .cloned()
    }
}

fn host_bypassed_by_no_proxy(host: &str, client_env: &BTreeMap<String, String>) -> bool {
    let Some(np) = client_env
        .get("no_proxy")
        .or_else(|| client_env.get("NO_PROXY"))
    else {
        return false;
    };
    let h = host.to_ascii_lowercase();
    for raw in np.split(',') {
        let pat = raw.trim().to_ascii_lowercase();
        if pat.is_empty() {
            continue;
        }
        if pat == "*" {
            return true;
        }
        if let Some(suf) = pat.strip_prefix('.') {
            if h.ends_with(&format!(".{suf}")) {
                return true;
            }
        } else if h == pat || h.ends_with(&format!(".{pat}")) {
            return true;
        }
    }
    false
}

fn resolve_http_proxy(host: &str, client_env: &BTreeMap<String, String>) -> Option<Url> {
    if host_bypassed_by_no_proxy(host, client_env) {
        return None;
    }
    let raw = select_proxy_env("http", client_env)?;
    if raw.is_empty() {
        return None;
    }
    let normalized = if raw.contains("://") {
        raw
    } else {
        format!("http://{raw}")
    };
    Url::parse(&normalized)
        .ok()
        .filter(|u| !u.host.is_empty() && (u.scheme == "http" || u.scheme == "https"))
}

impl Url {
    fn to_url_string_without_userinfo(&self) -> String {
        let mut u = self.clone();
        u.userinfo = None;
        u.to_url_string()
    }
}

/// File names to look for in distdirs: basenames of each URL's path, and of the output.
fn candidate_file_names(urls: &[String], output: &Path) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for u in urls {
        if let Ok(p) = Url::parse(u) {
            if let Some(b) = p.path().rsplit('/').next() {
                let b = percent_decode(b);
                if !b.is_empty() && !v.contains(&b) {
                    v.push(b);
                }
            }
        }
    }
    if let Some(n) = output.file_name() {
        let n = n.to_string_lossy().to_string();
        if !v.contains(&n) {
            v.push(n);
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::{sha1_hex, sha256_hex};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    fn tmpdir(name: &str) -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("zel-download-test-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn file_url(p: &Path) -> String {
        crate::file_url_for_path(p)
    }

    fn req(urls: Vec<String>, out: PathBuf) -> DownloadRequest {
        DownloadRequest {
            urls,
            output: out,
            ..Default::default()
        }
    }

    fn quick() -> Downloader {
        Downloader {
            retries: 0,
            retry_wait: Duration::from_millis(1),
            timeout: Duration::from_secs(5),
            ..Default::default()
        }
    }

    /// Serve `n` connections with a handler returning raw response bytes for (path, request text).
    fn serve(
        n: usize,
        handler: impl Fn(&str, &str) -> Vec<u8> + Send + 'static,
    ) -> (u16, Arc<Mutex<Vec<String>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            for _ in 0..n {
                let Ok((mut s, _)) = l.accept() else { return };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 1024];
                loop {
                    let k = s.read(&mut tmp).unwrap_or(0);
                    if k == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..k]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
                seen2.lock().unwrap().push(text.clone());
                let resp = handler(&path, &text);
                let _ = s.write_all(&resp);
            }
        });
        (port, seen)
    }

    /// Loopback connections may be forbidden in sandboxes; network tests skip then.
    fn net_ok() -> bool {
        match TcpListener::bind("127.0.0.1:0") {
            Ok(l) => TcpStream::connect(l.local_addr().unwrap()).is_ok(),
            Err(_) => false,
        }
    }

    fn ok(body: &[u8]) -> Vec<u8> {
        let mut r =
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        r.extend_from_slice(body);
        r
    }

    #[test]
    fn url_parsing() {
        let u = Url::parse("http://user:pw@Example.com:8080/a/b?x=1#frag").unwrap();
        assert_eq!(u.host, "Example.com");
        assert_eq!(u.port, Some(8080));
        assert_eq!(u.userinfo.as_deref(), Some("user:pw"));
        assert_eq!(u.path_and_query, "/a/b?x=1");
        assert_eq!(u.path(), "/a/b");
        let u = Url::parse("https://h").unwrap();
        assert_eq!(u.path_and_query, "/");
        let u = Url::parse("http://[::1]:99/x").unwrap();
        assert_eq!(u.host, "::1");
        assert_eq!(u.host_header(), "[::1]:99");
        assert!(Url::parse("nonsense").is_err());
        assert!(Url::parse("http://h:abc/").is_err());
        let base = Url::parse("http://h/a/b/c").unwrap();
        assert_eq!(base.join("/z").unwrap().path_and_query, "/z");
        assert_eq!(base.join("d").unwrap().path_and_query, "/a/b/d");
        assert_eq!(base.join("https://o/p").unwrap().host, "o");
        assert_eq!(base.join("//o2/p").unwrap().host, "o2");
    }

    #[test]
    fn file_download_and_checksum() {
        let d = tmpdir("file");
        fs::write(d.join("src.txt"), b"hello").unwrap();
        let sha = sha256_hex(b"hello");
        let mut r = req(vec![file_url(&d.join("src.txt"))], d.join("out/dl.txt"));
        r.sha256 = sha.clone();
        r.executable = true;
        let res = quick().download(&r).unwrap();
        assert_eq!(res.sha256, sha);
        assert_eq!(fs::read(d.join("out/dl.txt")).unwrap(), b"hello");
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(d.join("out/dl.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert!(res.integrity.starts_with("sha256-"));

        // bad checksum
        let mut r = req(vec![file_url(&d.join("src.txt"))], d.join("out/bad.txt"));
        r.sha256 = sha256_hex(b"other");
        let e = quick().download(&r).unwrap_err();
        assert!(e.starts_with("Error downloading ["), "{}", e);
        assert!(e.contains("Checksum was "), "{}", e);
        assert!(e.contains(&format!("but wanted {}", r.sha256)), "{}", e);
        assert!(!d.join("out/bad.txt").exists());
    }

    #[test]
    fn integrity_and_conflicts() {
        let d = tmpdir("integrity");
        fs::write(d.join("s"), b"abc").unwrap();
        let mut r = req(vec![file_url(&d.join("s"))], d.join("o"));
        r.integrity = format!("sha256-{}", base64_encode(&crate::hash::sha256(b"abc")));
        quick().download(&r).unwrap();
        r.sha256 = sha256_hex(b"abc");
        assert!(quick().download(&r).unwrap_err().contains("not both"));
        r.sha256 = "zz".into();
        r.integrity.clear();
        assert!(quick()
            .download(&r)
            .unwrap_err()
            .contains("Invalid SHA-256"));
    }

    #[test]
    fn url_fallback() {
        let d = tmpdir("fallback");
        fs::write(d.join("good"), b"data").unwrap();
        let r = req(
            vec![
                file_url(&d.join("missing")),
                "ftp://x/y".to_string(),
                file_url(&d.join("good")),
            ],
            d.join("o"),
        );
        let res = quick().download(&r).unwrap();
        assert_eq!(res.url, file_url(&d.join("good")));
        // all fail
        let r = req(
            vec![file_url(&d.join("m1")), file_url(&d.join("m2"))],
            d.join("o2"),
        );
        let e = quick().download(&r).unwrap_err();
        assert!(e.starts_with("Error downloading ["), "{}", e);
        assert!(quick().download(&req(vec![], d.join("o3"))).is_err());
    }

    #[test]
    fn repository_cache_roundtrip() {
        let d = tmpdir("cache");
        fs::write(d.join("s"), b"cached").unwrap();
        let sha = sha256_hex(b"cached");
        let dl = Downloader {
            repo_cache: Some(RepositoryCache::new(d.join("cache"))),
            ..quick()
        };
        let mut r = req(vec![file_url(&d.join("s"))], d.join("o1"));
        r.sha256 = sha.clone();
        dl.download(&r).unwrap();
        let entry = d
            .join("cache/content_addressable/sha256")
            .join(&sha)
            .join("file");
        assert_eq!(fs::read(&entry).unwrap(), b"cached");
        // delete the source; second download must hit cache
        fs::remove_file(d.join("s")).unwrap();
        let mut r2 = req(vec![file_url(&d.join("s"))], d.join("o2"));
        r2.sha256 = sha.clone();
        let res = dl.download(&r2).unwrap();
        assert!(res.from_cache);
        assert_eq!(fs::read(d.join("o2")).unwrap(), b"cached");
        // download disabled but cached ok
        let dl2 = Downloader {
            disable_download: true,
            ..dl.clone()
        };
        r2.output = d.join("o3");
        dl2.download(&r2).unwrap();
        // corrupt cache entry gives error
        fs::write(&entry, b"corrupt").unwrap();
        r2.output = d.join("o4");
        let e = dl.download(&r2).unwrap_err();
        assert!(e.contains("Please delete the directory"), "{}", e);
    }

    #[test]
    fn cache_without_checksum_stores_by_computed_sha() {
        let d = tmpdir("cache-nosum");
        fs::write(d.join("s"), b"zzz").unwrap();
        let dl = Downloader {
            repo_cache: Some(RepositoryCache::new(d.join("cache"))),
            ..quick()
        };
        dl.download(&req(vec![file_url(&d.join("s"))], d.join("o")))
            .unwrap();
        assert!(d
            .join("cache/content_addressable/sha256")
            .join(sha256_hex(b"zzz"))
            .join("file")
            .exists());
    }

    #[test]
    fn canonical_id_cache_semantics() {
        let d = tmpdir("canon");
        let cache = RepositoryCache::new(d.join("cache"));
        let c = Checksum::sha256_hex(&sha256_hex(b"x")).unwrap();
        cache.put(&c, b"x", "").unwrap();
        assert!(cache.get(&c, "").unwrap().is_some());
        assert!(cache.get(&c, "idA").unwrap().is_none());
        cache.put(&c, b"x", "idA").unwrap();
        assert!(cache.get(&c, "idA").unwrap().is_some());
        assert!(cache.get(&c, "idB").unwrap().is_none());
    }

    #[test]
    fn sha1_cache_dir() {
        let d = tmpdir("sha1");
        fs::write(d.join("s"), b"one").unwrap();
        let dl = Downloader {
            repo_cache: Some(RepositoryCache::new(d.join("cache"))),
            ..quick()
        };
        let mut r = req(vec![file_url(&d.join("s"))], d.join("o"));
        r.integrity = format!("sha1-{}", base64_encode(&crate::hash::sha1(b"one")));
        dl.download(&r).unwrap();
        assert!(d
            .join("cache/content_addressable/sha1")
            .join(sha1_hex(b"one"))
            .join("file")
            .exists());
    }

    #[test]
    fn distdir_lookup() {
        let d = tmpdir("distdir");
        fs::create_dir_all(d.join("dist")).unwrap();
        fs::write(d.join("dist/archive-1.0.tar.gz"), b"from distdir").unwrap();
        fs::write(d.join("dist/wrong-1.0.tar.gz"), b"tampered").unwrap();
        let dl = Downloader {
            distdirs: vec![d.join("dist")],
            disable_download: true,
            repo_cache: Some(RepositoryCache::new(d.join("cache"))),
            ..quick()
        };
        let mut r = req(
            vec!["https://example.com/x/archive-1.0.tar.gz?foo=1".into()],
            d.join("out"),
        );
        r.sha256 = sha256_hex(b"from distdir");
        let res = dl.download(&r).unwrap();
        assert!(res.from_cache);
        assert_eq!(fs::read(d.join("out")).unwrap(), b"from distdir");
        assert!(d
            .join("cache/content_addressable/sha256")
            .join(&r.sha256)
            .join("file")
            .exists());
        // checksum mismatch in distdir is ignored -> disabled download error
        let mut r = req(
            vec!["https://example.com/wrong-1.0.tar.gz".into()],
            d.join("out2"),
        );
        r.sha256 = sha256_hex(b"expected");
        r.context = "repository @foo".into();
        let e = dl.download(&r).unwrap_err();
        assert_eq!(
            e,
            "Failed to download repository @foo: download is disabled."
        );
    }

    #[test]
    fn disable_download_message_default_context() {
        let d = tmpdir("disabled");
        let dl = Downloader {
            disable_download: true,
            ..quick()
        };
        let e = dl
            .download(&req(vec!["http://h/f".into()], d.join("o")))
            .unwrap_err();
        assert_eq!(e, "Failed to download http://h/f: download is disabled.");
    }

    #[test]
    fn existing_output_with_matching_checksum_skips_download() {
        let d = tmpdir("existing");
        fs::write(d.join("o"), b"already").unwrap();
        let mut r = req(vec!["http://127.0.0.1:1/never".into()], d.join("o"));
        r.sha256 = sha256_hex(b"already");
        assert!(quick().download(&r).is_ok());
    }

    #[test]
    fn http_basic_and_headers_and_auth() {
        if !net_ok() {
            return;
        }
        let d = tmpdir("http");
        let (port, seen) = serve(2, |_, _| ok(b"body-bytes"));
        let url = format!("http://127.0.0.1:{}/file.bin", port);
        let mut r = req(vec![url.clone()], d.join("o"));
        r.headers.insert("X-Custom".into(), "v1".into());
        let mut a = BTreeMap::new();
        a.insert("type".to_string(), "basic".to_string());
        a.insert("login".to_string(), "u".to_string());
        a.insert("password".to_string(), "p".to_string());
        r.auth.insert(url.clone(), a);
        r.sha256 = sha256_hex(b"body-bytes");
        quick().download(&r).unwrap();
        assert_eq!(fs::read(d.join("o")).unwrap(), b"body-bytes");
        let reqtext = seen.lock().unwrap()[0].clone();
        assert!(reqtext.starts_with("GET /file.bin HTTP/1.1\r\n"));
        assert!(reqtext.contains("X-Custom: v1"));
        assert!(reqtext.contains(&format!("Authorization: Basic {}", base64_encode(b"u:p"))));

        // pattern auth
        let mut r = req(vec![url.clone()], d.join("o2"));
        let mut a = BTreeMap::new();
        a.insert("type".to_string(), "pattern".to_string());
        a.insert("pattern".to_string(), "Bearer <password>".to_string());
        a.insert("login".to_string(), "l".to_string());
        a.insert("password".to_string(), "tok".to_string());
        r.auth.insert(url, a);
        quick().download(&r).unwrap();
        assert!(seen.lock().unwrap()[1].contains("Authorization: Bearer tok"));
    }

    #[cfg(unix)]
    #[test]
    fn credential_helper_failure_propagates_and_matching_helper_never_falls_back_to_netrc() {
        use std::os::unix::fs::PermissionsExt;
        if !net_ok() {
            return;
        }
        let d = tmpdir("cred-helper-netrc");
        let netrc_path = d.join(".netrc");
        fs::write(
            &netrc_path,
            "machine 127.0.0.1 login netrc_user password netrc_pass\n",
        )
        .unwrap();
        let mut client_env = BTreeMap::new();
        client_env.insert("NETRC".to_string(), netrc_path.display().to_string());

        let write_helper = |name: &str, body: &str| -> PathBuf {
            let p = d.join(name);
            fs::write(&p, body).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let failing_helper = write_helper(
            "fail_helper.sh",
            "#!/bin/sh\ncat >/dev/null\necho 'helper boom' >&2\nexit 1\n",
        );
        let empty_helper =
            write_helper("empty_helper.sh", "#!/bin/sh\ncat >/dev/null\necho '{}'\n");

        let (port, seen) = serve(2, |_, _| ok(b"payload"));
        let url = format!("http://127.0.0.1:{port}/artifact.bin");

        // 1. Matching helper fails -> download fails immediately without contacting server or using .netrc.
        let dl_fail = Downloader {
            credential_helpers: vec![("127.0.0.1".to_string(), failing_helper.clone())],
            client_env: client_env.clone(),
            ..quick()
        };
        let err = dl_fail
            .download(&req(vec![url.clone()], d.join("out_fail")))
            .unwrap_err();
        assert!(
            err.contains("Credential helper") && err.contains("helper boom"),
            "unexpected error: {err}"
        );
        assert!(
            seen.lock().unwrap().is_empty(),
            "failed helper must not send HTTP request with .netrc fallback"
        );

        // 2. Matching helper succeeds with `{}` -> no Authorization header is sent (.netrc suppressed).
        let dl_empty = Downloader {
            credential_helpers: vec![("127.0.0.1".to_string(), empty_helper)],
            client_env: client_env.clone(),
            ..quick()
        };
        dl_empty
            .download(&req(vec![url.clone()], d.join("out_empty")))
            .unwrap();
        let req_empty = seen.lock().unwrap()[0].clone();
        assert!(
            !req_empty.to_ascii_lowercase().contains("authorization:"),
            "matching helper returning empty headers must not fall back to .netrc:\n{req_empty}"
        );

        // 3. Helper scoped only to another host -> `findCredentialHelper` is None, so .netrc is used.
        let dl_unmatched = Downloader {
            credential_helpers: vec![("other.example.com".to_string(), failing_helper)],
            client_env,
            ..quick()
        };
        dl_unmatched
            .download(&req(vec![url], d.join("out_netrc")))
            .unwrap();
        let req_netrc = seen.lock().unwrap()[1].clone();
        let expected_basic = format!(
            "Authorization: Basic {}",
            base64_encode(b"netrc_user:netrc_pass")
        );
        assert!(
            req_netrc.contains(&expected_basic),
            "unmatched helper scope should fall back to .netrc:\n{req_netrc}"
        );
    }

    #[test]
    fn http_chunked_redirect_and_404() {
        if !net_ok() {
            return;
        }
        let d = tmpdir("http2");
        let (port, _) = serve(6, |path, _| {
            match path {
            "/redir" => b"HTTP/1.1 302 Found\r\nLocation: /chunked\r\nContent-Length: 0\r\n\r\n".to_vec(),
            "/chunked" => b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\n\r\n".to_vec(),
            "/short" => b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nabc".to_vec(),
            _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
        }
        });
        let base = format!("http://127.0.0.1:{}", port);
        quick()
            .download(&req(vec![format!("{}/redir", base)], d.join("a")))
            .unwrap();
        assert_eq!(fs::read(d.join("a")).unwrap(), b"hello world");
        let e = quick()
            .download(&req(vec![format!("{}/nothing", base)], d.join("b")))
            .unwrap_err();
        assert!(e.contains("GET returned 404 Not Found"), "{}", e);
        let e = quick()
            .download(&req(vec![format!("{}/short", base)], d.join("c")))
            .unwrap_err();
        assert!(e.contains("size mismatch"), "{}", e);
        // fallback from 404 to chunked
        quick()
            .download(&req(
                vec![format!("{}/nothing", base), format!("{}/chunked", base)],
                d.join("e"),
            ))
            .unwrap();
        assert_eq!(fs::read(d.join("e")).unwrap(), b"hello world");
    }

    #[test]
    fn http_retries_on_5xx() {
        if !net_ok() {
            return;
        }
        let d = tmpdir("http-retry");
        let counter = Arc::new(Mutex::new(0));
        let c2 = counter.clone();
        let (port, _) = serve(3, move |_, _| {
            let mut c = c2.lock().unwrap();
            *c += 1;
            if *c < 3 {
                b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n".to_vec()
            } else {
                ok(b"finally")
            }
        });
        let dl = Downloader {
            retries: 5,
            ..quick()
        };
        dl.download(&req(
            vec![format!("http://127.0.0.1:{}/x", port)],
            d.join("o"),
        ))
        .unwrap();
        assert_eq!(fs::read(d.join("o")).unwrap(), b"finally");
        assert_eq!(*counter.lock().unwrap(), 3);
    }

    #[test]
    fn http_connect_failure_and_redirect_loop() {
        if !net_ok() {
            return;
        }
        let d = tmpdir("http-fail");
        let e = quick()
            .download(&req(vec!["http://127.0.0.1:1/x".into()], d.join("o")))
            .unwrap_err();
        assert!(e.starts_with("Error downloading ["), "{}", e);
        let (port, _) = serve(30, |_, _| {
            b"HTTP/1.1 301 Moved\r\nLocation: /again\r\nContent-Length: 0\r\n\r\n".to_vec()
        });
        let dl = Downloader {
            max_redirects: 3,
            ..quick()
        };
        let e = dl
            .download(&req(
                vec![format!("http://127.0.0.1:{}/x", port)],
                d.join("o"),
            ))
            .unwrap_err();
        assert!(
            e.contains("Too many redirects") || e.contains("Redirect loop detected"),
            "{}",
            e
        );
    }

    #[test]
    fn https_without_curl_is_clear_error() {
        let d = tmpdir("https");
        let dl = Downloader {
            curl_binary: "zel-nonexistent-curl".into(),
            ..quick()
        };
        let e = dl
            .download(&req(vec!["https://example.invalid/f".into()], d.join("o")))
            .unwrap_err();
        assert!(e.contains("binary is required for https URLs"), "{}", e);
    }

    #[test]
    fn regex_engine() {
        let r = Regex::new("github.com/([^/]+)/([^/]+)/archive/(.+)\\.(tar\\.gz|zip)").unwrap();
        let c = r.full_match("github.com/a/b/archive/v1.0.tar.gz").unwrap();
        assert_eq!(c[1].as_deref(), Some("a"));
        assert_eq!(c[2].as_deref(), Some("b"));
        assert_eq!(c[3].as_deref(), Some("v1.0"));
        assert_eq!(c[4].as_deref(), Some("tar.gz"));
        assert!(r.full_match("github.com/a/b/archive/v1.0.rar").is_none());
        let r = Regex::new("a*b?c+").unwrap();
        assert!(r.full_match("aaabcc").is_some());
        assert!(r.full_match("").is_none());
        assert!(Regex::new("x{2,3}").unwrap().full_match("xxx").is_some());
        assert!(Regex::new("x{2,3}").unwrap().full_match("xxxx").is_none());
        assert!(Regex::new("[a-c]+|\\d+")
            .unwrap()
            .full_match("123")
            .is_some());
        assert!(Regex::new("(").is_err());
        assert!(Regex::new("[a").is_err());
        assert!(Regex::new("*a").is_err());
        assert!(Regex::new("a)").is_err());
        let r = Regex::new("(a|ab)(c|bcd)(d*)").unwrap();
        assert!(r.full_match("abcd").is_some());
        let r = Regex::new("(.*)-(.*)").unwrap();
        let c = r.full_match("a-b-c").unwrap();
        assert_eq!(c[1].as_deref(), Some("a-b"));
        let r = Regex::new("(.*?)-(.*)").unwrap();
        assert_eq!(r.full_match("a-b-c").unwrap()[1].as_deref(), Some("a"));
    }

    #[test]
    fn downloader_config() {
        let cfg = "# comment\n\
                   all_blocked_message See docs for more\n\
                   block mvnrepository.com\n\
                   allow good.mvnrepository.com\n\
                   rewrite repo.maven.apache.org/maven2/(.*) artifacts.example.com/libs/$1\n\
                   block github.com\n\
                   rewrite github.com/([^/]+)/([^/]+)/releases/download/(.*) mirror.example.com/gh/$1/$2/$3\n\
                   rewrite proxy.example.com/(.*) internal.proxy/fetch?url=${urlencode($1)}\n";
        let r = UrlRewriter::parse("cfg", cfg).unwrap();
        let out = r.amend(&[
            "https://repo.maven.apache.org/maven2/a/b/c.jar".to_string(),
            "https://mvnrepository.com/x".to_string(),
            "https://good.mvnrepository.com/x".to_string(),
            "https://github.com/o/r/releases/download/v1/f.tgz".to_string(),
            "https://github.com/o/r/archive/v1.tgz".to_string(),
            "https://proxy.example.com/some/path?query=val".to_string(),
            "file:///local/f".to_string(),
        ]);
        assert_eq!(
            out,
            vec![
                "https://artifacts.example.com/libs/a/b/c.jar",
                "https://good.mvnrepository.com/x",
                "https://mirror.example.com/gh/o/r/v1/f.tgz",
                "https://internal.proxy/fetch?url=some/path%3Fquery%3Dval",
                "file:///local/f"
            ]
        );
        let all = UrlRewriter::parse("c", "block *\n").unwrap();
        assert!(all.amend(&["http://a/b".to_string()]).is_empty());
        assert!(all.amend(&["file:///a".to_string()]).len() == 1);
        assert_eq!(
            r.blocked_message(&["https://mvnrepository.com/x".to_string()]),
            "Configured URL rewriter blocked all URLs: [https://mvnrepository.com/x] - See docs for more"
        );
    }

    #[test]
    fn downloader_config_errors() {
        assert!(UrlRewriter::parse("c", "block a b\n")
            .unwrap_err()
            .contains("Only the host name is allowed after `block`"));
        assert!(UrlRewriter::parse("c", "rewrite a\n").is_err());
        assert!(UrlRewriter::parse("c", "rewrite ( x\n")
            .unwrap_err()
            .contains("Invalid regex"));
        assert!(UrlRewriter::parse("c", "bogus x\n")
            .unwrap_err()
            .contains("Unable to parse"));
        assert!(UrlRewriter::parse("c", "all_blocked_message\n").is_err());
        assert!(UrlRewriter::parse("c", "all_blocked_message a\nall_blocked_message b\n").is_err());
        assert!(UrlRewriter::parse("c", "").is_ok());
    }

    #[test]
    fn rewriter_applied_in_download_and_blocking() {
        if !net_ok() {
            return;
        }
        let d = tmpdir("rewrite");
        fs::write(d.join("real"), b"mirrored").unwrap();
        let (port, _) = serve(1, |_, _| ok(b"from-mirror"));
        let cfg = format!("rewrite original.example/(.*) 127.0.0.1:{}/$1\n", port);
        let dl = Downloader {
            rewriter: Some(UrlRewriter::parse("c", &cfg).unwrap()),
            ..quick()
        };
        dl.download(&req(
            vec!["http://original.example/path/f".into()],
            d.join("o"),
        ))
        .unwrap();
        assert_eq!(fs::read(d.join("o")).unwrap(), b"from-mirror");
        let dl = Downloader {
            rewriter: Some(UrlRewriter::parse("c", "block *\n").unwrap()),
            ..quick()
        };
        let e = dl
            .download(&req(vec!["http://x.example/f".into()], d.join("o2")))
            .unwrap_err();
        assert!(
            e.starts_with("Configured URL rewriter blocked all URLs"),
            "{}",
            e
        );
    }

    #[test]
    fn percent_decode_works() {
        assert_eq!(percent_decode("a%20b%2Fc"), "a b/c");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%4"), "%4");
    }

    #[test]
    fn file_url_forms() {
        assert_eq!(
            file_url_to_path(&Url::parse("file:///a/b%20c").unwrap()).unwrap(),
            PathBuf::from("/a/b c")
        );
        assert_eq!(
            file_url_to_path(&Url::parse("file://localhost/a").unwrap()).unwrap(),
            PathBuf::from("/a")
        );
        assert!(file_url_to_path(&Url::parse("file://remote/a").unwrap()).is_err());
    }

    #[test]
    fn integrity_preserved_user_agent_timeout_and_credential_helper_priority() {
        let d = tmpdir("dl-parity");
        fs::write(d.join("payload"), b"hello-integrity").unwrap();
        let sha512_sri = format!(
            "sha512-{}",
            base64_encode(&crate::hash::sha512(b"hello-integrity"))
        );
        let dl = Downloader {
            repo_cache: Some(RepositoryCache::new(d.join("cache"))),
            timeout: Duration::from_secs(30),
            ..quick()
        };
        assert_eq!(dl.connect_timeout_for(0), Duration::from_secs(1));
        assert_eq!(dl.connect_timeout_for(1), Duration::from_secs(2));
        assert_eq!(dl.connect_timeout_for(2), Duration::from_secs(4));
        assert_eq!(dl.connect_timeout_for(3), Duration::from_secs(8));
        assert_eq!(dl.connect_timeout_for(4), Duration::from_secs(10));
        assert_eq!(dl.connect_timeout_for(9), Duration::from_secs(10));

        let mut r1 = req(vec![file_url(&d.join("payload"))], d.join("o1"));
        r1.integrity = sha512_sri.clone();
        let res1 = dl.download(&r1).unwrap();
        assert_eq!(res1.integrity, sha512_sri);

        // Cache hit also preserves caller-supplied SHA-512 integrity.
        let mut r2 = req(vec![file_url(&d.join("missing"))], d.join("o2"));
        r2.integrity = sha512_sri.clone();
        let res2 = dl.download(&r2).unwrap();
        assert!(res2.from_cache);
        assert_eq!(res2.integrity, sha512_sri);

        #[cfg(unix)]
        if net_ok() {
            use std::os::unix::fs::PermissionsExt;
            let helper = d.join("cred_helper.sh");
            fs::write(
                &helper,
                "#!/bin/sh\n[ \"$1\" = \"get\" ] || exit 2\necho '{\"headers\":{\"Authorization\":[\"Bearer FROM-HELPER\"],\"X-Helper\":[\"ok\"]}}'\n",
            )
            .unwrap();
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
            let netrc = d.join(".netrc");
            fs::write(&netrc, "machine 127.0.0.1 login netrc_u password netrc_p\n").unwrap();

            let (port, seen) = serve(1, |_, _| ok(b"authed-body"));
            let url = format!("http://127.0.0.1:{port}/pkg.tgz");
            let mut r = req(vec![url.clone()], d.join("authed_out"));
            r.headers.insert(
                "Authorization".to_string(),
                "Bearer FROM-STARLARK".to_string(),
            );
            r.headers
                .insert("X-Starlark-Keep".to_string(), "kept".to_string());
            let mut auth_entry = BTreeMap::new();
            auth_entry.insert("type".to_string(), "basic".to_string());
            auth_entry.insert("login".to_string(), "auth_u".to_string());
            auth_entry.insert("password".to_string(), "auth_p".to_string());
            r.auth.insert(url, auth_entry);

            let mut client_env = BTreeMap::new();
            client_env.insert("NETRC".to_string(), netrc.to_string_lossy().into_owned());
            let dl_http = Downloader {
                credential_helpers: vec![("127.0.0.1".to_string(), helper)],
                client_env: client_env.clone(),
                ..quick()
            };
            dl_http.download(&r).unwrap();
            let reqtext = seen.lock().unwrap()[0].clone();
            assert!(reqtext.contains("User-Agent: bazel/9.3.2\r\n"), "{reqtext}");
            assert!(
                reqtext.contains("Authorization: Bearer FROM-HELPER\r\n"),
                "{reqtext}"
            );
            assert!(!reqtext.contains("FROM-STARLARK"), "{reqtext}");
            assert!(reqtext.contains("X-Starlark-Keep: kept\r\n"), "{reqtext}");
            assert!(reqtext.contains("X-Helper: ok\r\n"), "{reqtext}");

            // Matching `--credential_helper` returning non-Authorization custom headers or `{}`
            // must NOT attach `.netrc` or Starlark `auth` `Authorization`, whereas a non-matching
            // `--credential_helper` (`other.example.com`) MUST still fall back to `.netrc`.
            let custom_only_helper = d.join("cred_helper_custom_only.sh");
            fs::write(
                &custom_only_helper,
                "#!/bin/sh\n[ \"$1\" = \"get\" ] || exit 2\necho '{\"headers\":{\"X-Custom-Auth\":[\"custom-only\"]}}'\n",
            )
            .unwrap();
            fs::set_permissions(&custom_only_helper, fs::Permissions::from_mode(0o755)).unwrap();

            let empty_helper = d.join("cred_helper_empty.sh");
            fs::write(
                &empty_helper,
                "#!/bin/sh\n[ \"$1\" = \"get\" ] || exit 2\necho '{}'\n",
            )
            .unwrap();
            fs::set_permissions(&empty_helper, fs::Permissions::from_mode(0o755)).unwrap();

            let (port2, seen2) = serve(3, |_, _| ok(b"body2"));
            let url2 = format!("http://127.0.0.1:{port2}/pkg2.tgz");

            // 1. Matching helper with custom header only (both Starlark `auth` and `.netrc` set).
            let mut r_custom = req(vec![url2.clone()], d.join("custom_only_out"));
            let mut fallback_auth = BTreeMap::new();
            fallback_auth.insert("type".to_string(), "basic".to_string());
            fallback_auth.insert("login".to_string(), "auth_u".to_string());
            fallback_auth.insert("password".to_string(), "auth_p".to_string());
            r_custom.auth.insert(url2.clone(), fallback_auth.clone());
            Downloader {
                credential_helpers: vec![("127.0.0.1".to_string(), custom_only_helper.clone())],
                client_env: client_env.clone(),
                ..quick()
            }
            .download(&r_custom)
            .unwrap();

            // 2. Matching helper returning `{}` (both Starlark `auth` and `.netrc` set).
            let mut r_empty = req(vec![url2.clone()], d.join("empty_helper_out"));
            r_empty.auth.insert(url2.clone(), fallback_auth);
            Downloader {
                credential_helpers: vec![("127.0.0.1".to_string(), empty_helper)],
                client_env: client_env.clone(),
                ..quick()
            }
            .download(&r_empty)
            .unwrap();

            // 3. Non-matching helper (`other.example.com`) falls back to `.netrc` for `127.0.0.1`.
            let r_non_match = req(vec![url2], d.join("non_match_out"));
            Downloader {
                credential_helpers: vec![("other.example.com".to_string(), custom_only_helper)],
                client_env,
                ..quick()
            }
            .download(&r_non_match)
            .unwrap();

            let reqs2 = seen2.lock().unwrap().clone();
            assert!(
                reqs2[0].contains("X-Custom-Auth: custom-only\r\n"),
                "{}",
                reqs2[0]
            );
            assert!(
                !reqs2[0].to_ascii_lowercase().contains("authorization:"),
                "matching custom-header helper must not attach fallback auth:\n{}",
                reqs2[0]
            );
            assert!(
                !reqs2[1].to_ascii_lowercase().contains("authorization:"),
                "matching empty helper must not attach fallback auth:\n{}",
                reqs2[1]
            );
            let expected_netrc_b64 = base64_encode(b"netrc_u:netrc_p");
            assert!(
                reqs2[2].contains(&format!("Authorization: Basic {expected_netrc_b64}\r\n")),
                "non-matching helper must still fall back to .netrc:\n{}",
                reqs2[2]
            );
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn blake3_cache_corrupt_repair_and_rewriter_full_scheme() {
        let d = tmpdir("b3-repair-rewriter");
        fs::write(d.join("src.bin"), b"blake3-payload").unwrap();
        let dl = Downloader {
            repo_cache: Some(RepositoryCache::new(d.join("cache"))),
            ..quick()
        };
        let b3_hex = crate::hash::blake3_hex(b"blake3-payload");
        let b3_sri = format!(
            "blake3-{}",
            base64_encode(&crate::hash::blake3(b"blake3-payload"))
        );
        let mut r = req(vec![file_url(&d.join("src.bin"))], d.join("out1"));
        r.integrity = b3_sri.clone();
        let res = dl.download(&r).unwrap();
        assert_eq!(res.integrity, b3_sri);
        let cached_file = d
            .join("cache/content_addressable/blake3")
            .join(&b3_hex)
            .join("file");
        assert_eq!(fs::read(&cached_file).unwrap(), b"blake3-payload");

        // Corrupt the cached file while source URL is still live -> re-downloads and repairs cache!
        fs::write(&cached_file, b"corrupted").unwrap();
        r.output = d.join("out2");
        let res2 = dl.download(&r).unwrap();
        assert!(!res2.from_cache);
        assert_eq!(fs::read(d.join("out2")).unwrap(), b"blake3-payload");
        assert_eq!(fs::read(&cached_file).unwrap(), b"blake3-payload");

        // URL rewriter with explicit scheme in replacement must not produce double scheme.
        let rw = UrlRewriter::parse(
            "cfg",
            "rewrite example.com/(.*) https://mirror.internal/cache/$1\n",
        )
        .unwrap();
        let amended = rw.amend(&["http://example.com/pkg/a.tar.gz".to_string()]);
        assert_eq!(amended, vec!["https://mirror.internal/cache/pkg/a.tar.gz"]);
    }

    #[test]
    fn gzip_encoding_redirect_auth_retry_codes_and_http_proxy() {
        if !net_ok() {
            return;
        }
        let d = tmpdir("http-parity");

        // 1. Content-Encoding: gzip on .tar.gz must NOT gunzip the archive bytes, whereas .txt with x-gzip DOES gunzip.
        let gz_bytes = {
            // Minimal valid gzip wrapping "raw-tar"
            let payload = b"raw-tar";
            let mut g = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
            g.extend_from_slice(&[1, payload.len() as u8, 0, !(payload.len() as u8), 0xff]);
            g.extend_from_slice(payload);
            // Compute gzip CRC32 via archive helper or precomputed for "raw-tar" (0x3ef3ef7f -> check via uncompressed test)
            g
        };
        let _ = gz_bytes;
        // Build gzip via system gzip or exact CRC32: CRC32("hi") = 0xd89328e9? Let's compute standard IEEE CRC32 inline!
        let crc32_ieee = |data: &[u8]| -> u32 {
            let mut c = 0xffff_ffffu32;
            for &b in data {
                c ^= b as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 {
                        (c >> 1) ^ 0xedb8_8320
                    } else {
                        c >> 1
                    };
                }
            }
            !c
        };
        let make_gz = |payload: &[u8]| -> Vec<u8> {
            let mut g = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
            g.extend_from_slice(&[1, payload.len() as u8, 0, !(payload.len() as u8), 0xff]);
            g.extend_from_slice(payload);
            g.extend_from_slice(&crc32_ieee(payload).to_le_bytes());
            g.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            g
        };
        let gz_tar = make_gz(b"hello");
        let gz_tar_clone = gz_tar.clone();
        let (port_gz, _) = serve(2, move |path, _| {
            let enc = if path.ends_with(".tar.gz") { "gzip" } else { "x-gzip" };
            let mut resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Encoding: {enc}\r\nContent-Length: {}\r\n\r\n",
                gz_tar_clone.len()
            )
            .into_bytes();
            resp.extend_from_slice(&gz_tar_clone);
            resp
        });
        let r_tgz = req(
            vec![format!("http://127.0.0.1:{port_gz}/pkg.tar.gz")],
            d.join("pkg.tar.gz"),
        );
        quick().download(&r_tgz).unwrap();
        assert_eq!(
            fs::read(d.join("pkg.tar.gz")).unwrap(),
            gz_tar,
            ".tar.gz with Content-Encoding: gzip must remain compressed"
        );
        let r_txt = req(
            vec![format!("http://127.0.0.1:{port_gz}/readme.txt")],
            d.join("readme.txt"),
        );
        quick().download(&r_txt).unwrap();
        assert_eq!(
            fs::read(d.join("readme.txt")).unwrap(),
            b"hello",
            "non-.gz path with Content-Encoding: x-gzip must be gunzipped"
        );

        // 2. HTTP 403 is retried while HTTP 501 is NOT retried.
        let attempts_403 = Arc::new(Mutex::new(0usize));
        let a403 = attempts_403.clone();
        let (port_403, _) = serve(2, move |_, _| {
            let mut n = a403.lock().unwrap();
            *n += 1;
            if *n == 1 {
                b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n".to_vec()
            } else {
                ok(b"recovered-403")
            }
        });
        let mut dl_retry = quick();
        dl_retry.retries = 2;
        dl_retry
            .download(&req(
                vec![format!("http://127.0.0.1:{port_403}/f")],
                d.join("out403"),
            ))
            .unwrap();
        assert_eq!(fs::read(d.join("out403")).unwrap(), b"recovered-403");

        let attempts_501 = Arc::new(Mutex::new(0usize));
        let a501 = attempts_501.clone();
        let (port_501, _) = serve(1, move |_, _| {
            *a501.lock().unwrap() += 1;
            b"HTTP/1.1 501 Not Implemented\r\nContent-Length: 0\r\n\r\n".to_vec()
        });
        assert!(dl_retry
            .download(&req(
                vec![format!("http://127.0.0.1:{port_501}/f")],
                d.join("out501"),
            ))
            .is_err());
        assert_eq!(*attempts_501.lock().unwrap(), 1, "501 must not be retried");

        // 3. Per-redirect auth re-evaluation: initial URL redirects to target URL which has auth configured.
        let (port_target, seen_target) = serve(1, |_, _| ok(b"authed-body"));
        let target_url = format!("http://127.0.0.1:{port_target}/secret");
        let target_clone = target_url.clone();
        let (port_redir, seen_redir) = serve(1, move |_, _| {
            format!("HTTP/1.1 302 Found\r\nLocation: {target_clone}\r\nContent-Length: 0\r\n\r\n")
                .into_bytes()
        });
        let start_url = format!("http://127.0.0.1:{port_redir}/start");
        let mut r_redir = req(vec![start_url], d.join("authed"));
        let mut auth_entry = BTreeMap::new();
        auth_entry.insert("type".to_string(), "pattern".to_string());
        auth_entry.insert("pattern".to_string(), "Bearer <password>".to_string());
        auth_entry.insert("password".to_string(), "redir-tok-99".to_string());
        r_redir.auth.insert(target_url, auth_entry);
        quick().download(&r_redir).unwrap();
        assert!(
            !seen_redir.lock().unwrap()[0].contains("redir-tok-99"),
            "initial unauthed host must not receive target token"
        );
        assert!(
            seen_target.lock().unwrap()[0].contains("Bearer redir-tok-99"),
            "redirect target must receive its re-evaluated auth header"
        );

        // 4. HTTP proxy routing with Proxy-Authorization from client_env.
        let (port_proxy, seen_proxy) = serve(1, |_, _| ok(b"via-proxy"));
        let mut dl_proxy = quick();
        dl_proxy.client_env.insert(
            "http_proxy".to_string(),
            format!("http://puser:ppass@127.0.0.1:{port_proxy}"),
        );
        dl_proxy
            .download(&req(
                vec!["http://upstream.example.invalid:8080/item.bin".to_string()],
                d.join("proxied"),
            ))
            .unwrap();
        assert_eq!(fs::read(d.join("proxied")).unwrap(), b"via-proxy");
        let proxy_req = &seen_proxy.lock().unwrap()[0];
        assert!(
            proxy_req.starts_with("GET http://upstream.example.invalid:8080/item.bin HTTP/1.1"),
            "{proxy_req}"
        );
        assert!(proxy_req.contains("Proxy-Authorization: Basic "), "{proxy_req}");
    }
}

thread_local! {
    static WARNINGS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Per-URL failure warnings (`WARNING: Download from <url> failed: class <exception> <message>`) recorded since the
/// last call, mirroring `HttpDownloader.recordFailure`.
pub fn take_warnings() -> Vec<String> {
    WARNINGS.with(|w| std::mem::take(&mut *w.borrow_mut()))
}

fn record_warning(url: &str, msg: &str) {
    let unrecoverable =
        "com.google.devtools.build.lib.bazel.repository.downloader.UnrecoverableHttpException";
    let class = if let Some(rest) = msg.strip_prefix("GET returned ") {
        let code: u32 = rest
            .split(' ')
            .next()
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        if code == 404 || code == 410 {
            "java.io.FileNotFoundException"
        } else if (code < 500 && code != 403 && code != 408 && code != 429)
            || code == 501
            || code == 505
        {
            unrecoverable
        } else {
            "java.io.IOException"
        }
    } else if msg == "Connection refused" {
        "java.net.ConnectException"
    } else if msg.starts_with("Checksum was")
        || msg.starts_with("Redirect loop")
        || msg.starts_with("Unknown host")
    {
        unrecoverable
    } else if msg.ends_with("(No such file or directory)")
        || msg.ends_with("(Permission denied)")
        || msg.ends_with("(Is a directory)")
    {
        "java.io.FileNotFoundException"
    } else {
        "java.io.IOException"
    };
    WARNINGS.with(|w| {
        w.borrow_mut().push(format!(
            "WARNING: Download from {url} failed: class {class} {msg}"
        ))
    });
}

/// The libc-style reason Java appends to `FileNotFoundException` messages.
fn java_io_reason(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => "No such file or directory".to_string(),
        std::io::ErrorKind::PermissionDenied => "Permission denied".to_string(),
        _ => {
            let s = e.to_string();
            match s.find(" (os error") {
                Some(i) => s[..i].to_string(),
                None => s,
            }
        }
    }
}

/// Normalizes a URI string into a canonical key matching `java.net.URI.equals(Object)`:
/// - scheme and authority host are ASCII case-insensitive;
/// - percent-encoding hex digits (`%2f` vs `%2F`) are case-insensitive;
/// - explicit ports (even default ports like `:80` / `:443`), userinfo, path, query, and
///   fragment are otherwise compared case-sensitively.
pub fn canonical_java_uri_key(uri: &str) -> String {
    fn normalize_pct(s: &str, lowercase_non_pct: bool) -> String {
        let bytes = s.as_bytes();
        let mut out = String::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%'
                && i + 2 < bytes.len()
                && bytes[i + 1].is_ascii_hexdigit()
                && bytes[i + 2].is_ascii_hexdigit()
            {
                out.push('%');
                out.push((bytes[i + 1] as char).to_ascii_uppercase());
                out.push((bytes[i + 2] as char).to_ascii_uppercase());
                i += 3;
            } else {
                let ch = s[i..].chars().next().unwrap();
                if lowercase_non_pct {
                    out.push(ch.to_ascii_lowercase());
                } else {
                    out.push(ch);
                }
                i += ch.len_utf8();
            }
        }
        out
    }

    let (without_frag, frag) = match uri.split_once('#') {
        Some((pre, f)) => (pre, Some(f)),
        None => (uri, None),
    };
    let (scheme, after_scheme) = match without_frag.split_once(':') {
        Some((sc, rest))
            if !sc.is_empty()
                && sc.as_bytes()[0].is_ascii_alphabetic()
                && sc
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')) =>
        {
            (Some(sc.to_ascii_lowercase()), rest)
        }
        _ => (None, without_frag),
    };

    let mut out = String::with_capacity(uri.len());
    if let Some(sc) = scheme {
        out.push_str(&sc);
        out.push(':');
    }
    if let Some(after_slashes) = after_scheme.strip_prefix("//") {
        out.push_str("//");
        let auth_end = after_slashes
            .find(['/', '?', '#'])
            .unwrap_or(after_slashes.len());
        let authority = &after_slashes[..auth_end];
        let path_and_query = &after_slashes[auth_end..];
        let (userinfo, hostport) = match authority.rsplit_once('@') {
            Some((u, hp)) => (Some(u), hp),
            None => (None, authority),
        };
        if let Some(u) = userinfo {
            out.push_str(&normalize_pct(u, false));
            out.push('@');
        }
        let (host, port) = if hostport.starts_with('[') {
            match hostport.find(']') {
                Some(idx) => (&hostport[..=idx], &hostport[idx + 1..]),
                None => (hostport, ""),
            }
        } else {
            match hostport.rsplit_once(':') {
                Some((h, _)) => (h, &hostport[h.len()..]),
                None => (hostport, ""),
            }
        };
        out.push_str(&normalize_pct(host, true));
        out.push_str(port);
        out.push_str(&normalize_pct(path_and_query, false));
    } else {
        out.push_str(&normalize_pct(after_scheme, false));
    }
    if let Some(f) = frag {
        out.push('#');
        out.push_str(&normalize_pct(f, false));
    }
    out
}

pub fn java_uri_equals(a: &str, b: &str) -> bool {
    a == b || canonical_java_uri_key(a) == canonical_java_uri_key(b)
}

#[cfg(test)]
mod static_credentials_uri_tests {
    use super::*;

    #[test]
    fn java_uri_equality_matches_static_credentials_rules() {
        assert!(java_uri_equals(
            "HTTPS://Example.COM/a%2fb?x=%3a#Frag%2f",
            "https://example.com/a%2Fb?x=%3A#Frag%2F"
        ));
        // Path case outside percent-escapes remains sensitive.
        assert!(!java_uri_equals(
            "https://example.com/Path",
            "https://example.com/path"
        ));
        // Explicit default port is preserved and distinct from omitted port, just like java.net.URI.
        assert!(!java_uri_equals(
            "https://example.com:443/a",
            "https://example.com/a"
        ));
        assert!(java_uri_equals(
            "HTTPS://EXAMPLE.COM:443/a",
            "https://example.com:443/a"
        ));
    }
}
