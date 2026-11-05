//! `IndexRegistry`: registries laid out as `modules/<name>/<version>/{MODULE.bazel,source.json}`,
//! `modules/<name>/metadata.json` and `bazel_registry.json`. Supports `file://` and `http(s)://`.

use crate::hash::sha256_hex;
use crate::json::Json;
use crate::model::*;
use crate::version::Version;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// Result of recording registry file hashes: url -> sha256 (`None` if the file did not exist).
pub type FileHashes = BTreeMap<String, Option<String>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownHashesMode {
    /// No lockfile information: just fetch and record.
    Ignore,
    /// Lockfile hashes are used to verify fetched files; new files may be fetched (lockfile update).
    UseAndUpdate,
    /// Strict: every file must be recorded in the lockfile (`--lockfile_mode=error`).
    Enforce,
}

pub struct Registry {
    url: String,
    /// The registry URL exactly as configured (error messages print it unnormalised).
    orig_url: String,
    local_dir: Option<PathBuf>,
    mirrors: Vec<String>,
    known: Option<FileHashes>,
    mode: KnownHashesMode,
    registry_json: Mutex<Option<Option<Json>>>,
    pub timeout_secs: u64,
    /// Repository cache consulted (by lockfile hash) before touching the network, and fed after downloads.
    repo_cache: Option<crate::download::RepositoryCache>,
}

pub fn sha256_hex_to_sri(h: &str) -> String {
    format!("sha256-{}", crate::hash::base64_encode(&crate::hash::from_hex(h).unwrap_or_default()))
}

fn join_url(base: &str, segs: &[&str]) -> String {
    let mut u = base.to_string();
    for s in segs {
        if !u.ends_with('/') && !s.starts_with('/') {
            u.push('/');
        }
        u.push_str(s);
    }
    u
}

fn construct_url(base: &str, segments: &[&str]) -> String {
    let mut url = base.to_string();
    for segment in segments {
        if !url.is_empty() && !url.ends_with('/') && !segment.starts_with('/') {
            url.push('/');
        }
        url.push_str(segment);
    }
    url
}

pub fn file_url_to_path(url: &str) -> Result<PathBuf, String> {
    let rest = url.strip_prefix("file://").ok_or_else(|| format!("not a file URL: {url}"))?;
    // file://host/path is not supported; accept file:///path and file://localhost/path.
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    if !path.starts_with('/') {
        return Err(format!(
            "Local registry URL must have an absolute path -- did you mean to use file:///foo/bar or file:///c:/foo/bar for Windows? ({url})"
        ));
    }
    Ok(PathBuf::from(percent_decode(path)))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 1 && s.is_char_boundary(i + 1) && s.is_char_boundary(i + 3.min(b.len())) && i + 3 <= b.len() {
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

static URL_CACHE: std::sync::Mutex<Option<std::collections::HashMap<String, Result<Option<Vec<u8>>, String>>>> =
    std::sync::Mutex::new(None);

/// Fetches raw bytes from `url`; `Ok(None)` means "not found".
pub fn fetch_url(url: &str, timeout_secs: u64) -> Result<Option<Vec<u8>>, String> {
    {
        let mut guard = URL_CACHE.lock().unwrap();
        let cache = guard.get_or_insert_with(std::collections::HashMap::new);
        if let Some(res) = cache.get(url) {
            return res.clone();
        }
    }
    let res = fetch_url_uncached(url, timeout_secs);
    {
        let mut guard = URL_CACHE.lock().unwrap();
        let cache = guard.get_or_insert_with(std::collections::HashMap::new);
        cache.insert(url.to_string(), res.clone());
    }
    res
}

fn fetch_url_uncached(url: &str, timeout_secs: u64) -> Result<Option<Vec<u8>>, String> {
    if url.starts_with("file://") {
        let rest = &url["file://".len()..];
        {
            let auth = rest.split('/').next().unwrap_or("");
            if !auth.is_empty() && auth != "localhost" {
                let base_url = if let Some(idx) = url.find("/modules/") { &url[..idx] } else { url };
                return Err(format!("Error accessing registry {base_url}/: Failed to fetch registry file {url}: Unsupported non-local file URL"));
            }
        }
        let p = file_url_to_path(url)?;
        return match std::fs::read(&p) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("Failed to fetch registry file {url}: {e}")),
        };
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return fetch_http_with_retries(url, timeout_secs);
    }
    Err(format!("Unsupported registry URL scheme: {url}"))
}

/// Number of attempts per URL; Bazel's `HttpConnector.MAX_ATTEMPTS` (overridable like
/// `--http_connector_attempts`).
static HTTP_ATTEMPTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(8);

/// Sets `--http_connector_attempts` (0 keeps the default of 8).
pub fn set_http_connector_attempts(n: u32) {
    HTTP_ATTEMPTS.store(if n > 0 { n } else { 8 }, std::sync::atomic::Ordering::Relaxed);
}

/// One HTTP GET through `curl` (no TLS stack without external crates). `Err((retryable, msg))`.
fn curl_once(url: &str, connect_timeout_ms: u64, max_time: u64) -> Result<Option<Vec<u8>>, (bool, String)> {
    let out = std::process::Command::new("curl")
        .args([
            "-sS",
            "-L",
            "--max-redirs",
            "40",
            "--connect-timeout",
            &format!("{:.3}", connect_timeout_ms as f64 / 1000.0),
            "--max-time",
            &max_time.to_string(),
            "-w",
            "\n%{http_code}",
            url,
        ])
        .output()
        .map_err(|e| (false, e.to_string()))?;
    if !out.status.success() {
        // Mirror the Java connector: unknown host and certificate problems are permanent, everything else
        // (refused, reset, timeout, ...) is an IOException that is retried.
        return Err(match out.status.code() {
            Some(7) => (true, "Connection refused".to_string()),
            Some(6) => {
                let host = url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or("");
                (false, format!("Unknown host: {host}"))
            }
            Some(28) => (true, "connect timed out".to_string()),
            Some(35) | Some(51) | Some(58) | Some(60) | Some(77) => {
                (false, format!("TLS error: {}", String::from_utf8_lossy(&out.stderr).trim()))
            }
            _ => (true, String::from_utf8_lossy(&out.stderr).trim().to_string()),
        });
    }
    let body = out.stdout;
    let nl = body.iter().rposition(|b| *b == b'\n').unwrap_or(0);
    let code = String::from_utf8_lossy(&body[nl + 1..]).trim().parse::<u32>().unwrap_or(0);
    let content = body[..nl].to_vec();
    match code {
        200 | 206 => Ok(Some(content)),
        404 | 410 => Ok(None),
        // HttpConnector: 403, 408, 429 and 5xx other than 501/505 are retried; other 4xx are permanent.
        403 | 408 | 429 => Err((true, format!("HTTP status {code}"))),
        c if c < 500 || c == 501 || c == 505 => Err((false, format!("HTTP status {c}"))),
        c => Err((true, format!("HTTP status {c}"))),
    }
}

/// Port of `HttpConnector.connect`'s retry loop: up to `HTTP_ATTEMPTS` attempts, exponential backoff
/// (`100ms << retries`, jittered 0.75..1.25) between failures, connect timeout doubling from 1s to 10s on
/// timeouts.
fn fetch_http_with_retries(url: &str, max_time: u64) -> Result<Option<Vec<u8>>, String> {
    let attempts = HTTP_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed);
    let mut connect_ms: u64 = 1000;
    let mut retries: u32 = 0;
    loop {
        match curl_once(url, connect_ms, max_time.max(20)) {
            Ok(r) => return Ok(r),
            Err((retryable, msg)) => {
                let base_url = if let Some(idx) = url.find("/modules/") { &url[..idx] } else { url };
                let fail = |m: &str| {
                    if m.starts_with("HTTP status") {
                        format!("Failed to fetch registry file {url}: {m}")
                    } else {
                        format!("Error accessing registry {base_url}/: Failed to fetch registry file {url}: {m}")
                    }
                };
                if !retryable {
                    return Err(fail(&msg));
                }
                let timed_out = msg == "connect timed out";
                retries += 1;
                if retries >= attempts {
                    return Err(fail(&msg));
                }
                if timed_out {
                    connect_ms = (connect_ms * 2).min(10_000);
                } else {
                    let raw = (100u64 << (retries - 1).min(20)) as f64;
                    let nanos = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.subsec_nanos())
                        .unwrap_or(0);
                    let jitter = 0.75 + (nanos % 1000) as f64 / 2000.0;
                    std::thread::sleep(std::time::Duration::from_millis((jitter * raw) as u64));
                }
            }
        }
    }
}

thread_local! {
    static REGISTRY_WARNINGS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Warnings produced while reading registry files since the last call.
pub fn take_registry_warnings() -> Vec<String> {
    REGISTRY_WARNINGS.with(|w| std::mem::take(&mut *w.borrow_mut()))
}

/// Mirrors `java.net.URI`'s parser far enough to produce its `URISyntaxException` reasons.
/// Returns `(scheme, has_path)` where `has_path` is `false` for opaque URIs (`URI.getPath() == null`).
fn java_uri_parse(input: &str) -> Result<(Option<String>, bool), String> {
    let b = input.as_bytes();
    let n = b.len();
    let illegal = |c: char| c.is_ascii_control() || matches!(c, ' ' | '"' | '<' | '>' | '\\' | '^' | '`' | '{' | '|' | '}');
    let check = |from: usize, to: usize, what: &str, extra_ok: &str| -> Result<(), String> {
        let mut i = from;
        while i < to {
            let c = input[i..].chars().next().unwrap();
            if c == '%' {
                if !(b.get(i + 1).is_some_and(|x| x.is_ascii_hexdigit()) && b.get(i + 2).is_some_and(|x| x.is_ascii_hexdigit())) {
                    return Err(format!("Malformed escape pair at index {i}"));
                }
                i += 3;
                continue;
            }
            if (illegal(c) && !extra_ok.contains(c)) || c == '\u{7f}' {
                return Err(format!("Illegal character in {what} at index {i}"));
            }
            i += c.len_utf8();
        }
        Ok(())
    };
    let scan_to = |from: usize, stops: &str| -> usize { input[from..].find(|c| stops.contains(c)).map(|x| x + from).unwrap_or(n) };
    // Scheme?
    let p = scan_to(0, "/?#:");
    let (scheme, mut p) = if p < n && b[p] == b':' {
        if p == 0 {
            return Err("Expected scheme name at index 0".into());
        }
        if !b[0].is_ascii_alphabetic() {
            return Err("Illegal character in scheme name at index 0".into());
        }
        if let Some(i) = (1..p).find(|&i| !(b[i].is_ascii_alphanumeric() || matches!(b[i], b'+' | b'-' | b'.'))) {
            return Err(format!("Illegal character in scheme name at index {i}"));
        }
        (Some(input[..p].to_string()), p + 1)
    } else {
        (None, 0)
    };
    if scheme.is_some() && !(p < n && b[p] == b'/') {
        // Opaque URI.
        let q = scan_to(p, "#");
        if q <= p {
            return Err(format!("Expected scheme-specific part at index {p}"));
        }
        check(p, q, "opaque part", "")?;
        return Ok((scheme, false));
    }
    // Hierarchical: [//authority][path][?query][#fragment].
    if input[p..].starts_with("//") {
        p += 2;
        let q = scan_to(p, "/?#");
        if input[p..q].starts_with('[') && !input[p..q].contains(']') {
            return Err(format!("Expected closing bracket for IPv6 address at index {q}"));
        }
        check(p, q, "authority", "")?;
        p = q;
    }
    let q = scan_to(p, "?#");
    check(p, q, "path", "")?;
    p = q;
    if p < n && b[p] == b'?' {
        let q = scan_to(p + 1, "#");
        check(p + 1, q, "query", "")?;
        p = q;
    }
    if p < n && b[p] == b'#' {
        check(p + 1, n, "fragment", "")?;
    }
    Ok((scheme, true))
}

/// `RegistryFactoryImpl.getRegistryWithUrl`'s validation. The error is a `URISyntaxException`
/// message (`<reason>: <input>`), without the `Invalid registry URL: ` prefix.
pub fn validate_registry_url(url: &str) -> Result<(), String> {
    let (scheme, has_path) = java_uri_parse(url).map_err(|r| format!("{r}: {url}"))?;
    let Some(scheme) = scheme else {
        return Err(format!("Registry URL has no scheme -- supported schemes are: http://, https:// and file://: {url}"));
    };
    if !has_path {
        return Err(format!("Registry URL path is not valid -- did you mean to use file:///foo/bar or file:///c:/foo/bar for Windows?: {url}"));
    }
    match scheme.as_str() {
        "http" | "https" | "file" => Ok(()),
        _ => Err(format!("Unrecognized registry URL protocol: {url}")),
    }
}

impl Registry {
    pub fn new(url: &str) -> Result<Self, String> {
        validate_registry_url(url).map_err(|e| format!("Invalid registry URL: {url}: {e}"))?;
        let orig_url = url.to_string();
        let url = url.trim_end_matches('/').to_string();
        let mut local_dir = None;
        if url.starts_with("file:") {
            if !url.starts_with("file://") {
                return Err(format!("Invalid registry URL: {url}: Registry URL path is not valid -- did you mean to use file:///foo/bar or file:///c:/foo/bar for Windows?: {url}"));
            }
            // `file://host/path` (a non-local authority) is rejected when the registry file is opened.
            if let Some(auth) = url["file://".len()..].split('/').next().filter(|a| !a.is_empty() && *a != "localhost") {
                let _ = auth;
            } else {
                local_dir = Some(file_url_to_path(&url)?);
            }
        }
        Ok(Self {
            url,
            orig_url,
            local_dir,
            mirrors: vec![],
            known: None,
            mode: KnownHashesMode::Ignore,
            registry_json: Mutex::new(None),
            timeout_secs: 3,
            repo_cache: None,
        })
    }

    pub fn is_file_registry(&self) -> bool {
        self.local_dir.is_some() || self.url.starts_with("file://")
    }

    pub fn with_known_hashes(mut self, known: FileHashes, mode: KnownHashesMode) -> Self {
        self.known = Some(known);
        // Bazel explicitly sets KnownFileHashesMode.IGNORE for file:// registries.
        self.mode = if self.is_file_registry() {
            KnownHashesMode::Ignore
        } else {
            mode
        };
        self
    }

    pub fn with_repo_cache(mut self, root: Option<PathBuf>) -> Self {
        self.repo_cache = root.map(crate::download::RepositoryCache::new);
        self
    }

    pub fn with_mirrors(mut self, m: Vec<String>) -> Self {
        self.mirrors = m;
        self
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// `fetch_url` with the registry prefix of access errors restored to the configured URL.
    fn fetch(&self, url: &str) -> Result<Option<Vec<u8>>, String> {
        fetch_url(url, self.timeout_secs).map_err(|e| {
            let generic = format!("Error accessing registry {}/:", self.url);
            match e.strip_prefix(&generic) {
                Some(rest) => format!("Error accessing registry {}:{rest}", self.orig_url),
                None => e,
            }
        })
    }

    pub fn module_file_url(&self, key: &ModuleKey) -> String {
        join_url(&self.url, &["modules", &key.name, &key.version.to_string(), "MODULE.bazel"])
    }

    /// Fetches a file; verifies/records its hash when `use_checksum`.
    fn grab(&self, url: &str, use_checksum: bool, hashes: &mut FileHashes) -> Result<Option<Vec<u8>>, String> {
        let is_file = self.is_file_registry() || url.starts_with("file://");
        if is_file || self.mode == KnownHashesMode::Ignore {
            let content = self.fetch(url)?;
            if use_checksum {
                match &content {
                    Some(c) => {
                        hashes.insert(url.to_string(), Some(sha256_hex(c)));
                    }
                    None => {
                        hashes.insert(url.to_string(), None);
                    }
                }
            }
            return Ok(content);
        }
        let mut expected: Option<String> = None;
        if use_checksum && self.mode != KnownHashesMode::Ignore {
            match self.known.as_ref().and_then(|k| k.get(url)) {
                None => {
                    if self.mode == KnownHashesMode::Enforce {
                        return Err(format!(
                            "Missing checksum for registry file {url} not permitted with --lockfile_mode=error. Please run `bazel mod deps --lockfile_mode=update` to update your lockfile."
                        ));
                    }
                }
                Some(None) => {
                    // Previously not found: treated as not found without hitting the registry.
                    hashes.insert(url.to_string(), None);
                    return Ok(None);
                }
                Some(Some(h)) => {
                    expected = Some(h.clone());
                    // Lockfile-first: a file whose hash the lockfile records is served from the
                    // repository cache without any registry access (Bazel's DownloadManager).
                    if let (Some(rc), Ok(ck)) = (&self.repo_cache, crate::hash::Checksum::sha256_hex(h)) {
                        if let Ok(Some(f)) = rc.get(&ck, "") {
                            if let Ok(c) = std::fs::read(&f) {
                                hashes.insert(url.to_string(), Some(h.clone()));
                                return Ok(Some(c));
                            }
                        }
                    }
                }
            }
        }
        let content = self.fetch(url)?;
        if use_checksum {
            match &content {
                Some(c) => {
                    let h = sha256_hex(c);
                    if let Some(e) = expected {
                        if e != h {
                            return Err(format!(
                                "Failed to fetch registry file {url}: Checksum was {h} but wanted {e}"
                            ));
                        }
                    }
                    if let Some(rc) = &self.repo_cache {
                        if let Ok(ck) = crate::hash::Checksum::sha256_hex(&h) {
                            let _ = rc.put(&ck, c, "");
                        }
                    }
                    hashes.insert(url.to_string(), Some(h));
                }
                None => {
                    hashes.insert(url.to_string(), None);
                }
            }
        }
        Ok(content)
    }

    fn grab_text(&self, url: &str, use_checksum: bool, hashes: &mut FileHashes) -> Result<Option<String>, String> {
        Ok(self.grab(url, use_checksum, hashes)?.map(|b| String::from_utf8_lossy(&b).into_owned()))
    }

    /// Returns the MODULE.bazel content of `key`, or `None` if the registry doesn't have it.
    pub fn get_module_file(&self, key: &ModuleKey, hashes: &mut FileHashes) -> Result<Option<String>, String> {
        let url = self.module_file_url(key);
        self.grab_text(&url, true, hashes)
    }

    fn bazel_registry_json(&self, hashes: &mut FileHashes) -> Result<Option<Json>, String> {
        if let Some(c) = self.registry_json.lock().unwrap().clone() {
            return Ok(c);
        }
        let url = join_url(&self.url, &["bazel_registry.json"]);
        let txt = self.grab_text(&url, true, hashes)?;
        let parsed = match txt {
            Some(t) if !t.trim().is_empty() => {
                Some(Json::parse(&t).map_err(|e| format!("Unable to parse json at url {url}: {e}"))?)
            }
            _ => None,
        };
        *self.registry_json.lock().unwrap() = Some(parsed.clone());
        Ok(parsed)
    }

    /// Builds the `RepoSpec` for `key` from `source.json`; failures read
    /// `Unable to get module repo spec for <key> from registry: <cause>` (`RepoSpecFunction`).
    pub fn get_repo_spec(
        &self,
        key: &ModuleKey,
        module_file_hashes: &FileHashes,
        hashes: &mut FileHashes,
    ) -> Result<RepoSpec, String> {
        self.get_repo_spec_inner(key, module_file_hashes, hashes).map_err(|e| {
            if e.starts_with("Unable to get module repo spec") {
                e
            } else {
                format!("Unable to get module repo spec for {key} from registry: {e}")
            }
        })
    }

    fn get_repo_spec_inner(
        &self,
        key: &ModuleKey,
        module_file_hashes: &FileHashes,
        hashes: &mut FileHashes,
    ) -> Result<RepoSpec, String> {
        let json_url = join_url(&self.url, &["modules", &key.name, &key.version.to_string(), "source.json"]);
        let txt = self
            .grab_text(&json_url, true, hashes)?
            .ok_or_else(|| format!("Module {key}'s source.json not found in registry {}", self.url))?;
        let sj = Json::parse(&txt).map_err(|e| format!("Unable to parse json at url {json_url}: {e}"))?;
        let ty = sj.get("type").and_then(|t| t.as_str()).unwrap_or("archive");
        let s = |k: &str| sj.get(k).and_then(|v| v.as_str()).map(String::from);
        let module_file_url = self.module_file_url(key);
        let module_file_sri = module_file_hashes
            .get(&module_file_url)
            .or_else(|| hashes.get(&module_file_url))
            .cloned()
            .flatten()
            .map(|h| sha256_hex_to_sri(&h))
            .unwrap_or_default();
        let patches_of = |sj: &Json| -> Result<Vec<(String, AttrVal)>, String> {
            let mut out = vec![];
            if let Some(Json::Obj(p)) = sj.get("patches") {
                for (name, integ) in p {
                    let i = integ.as_str().unwrap_or("");
                    if i.trim().is_empty() {
                        return Err(format!("Unable to get module repo spec for {key} from registry: Missing integrity for patch {name} of module {key}"));
                    }
                    out.push((
                        join_url(&self.url, &["modules", &key.name, &key.version.to_string(), "patches", name]),
                        AttrVal::Str(i.to_string()),
                    ));
                }
            }
            Ok(out)
        };
        match ty {
            "archive" => {
                let url = s("url").ok_or_else(|| format!("Missing source URL for module {key}"))?;
                let integrity = s("integrity").filter(|i| !i.trim().is_empty()).ok_or_else(|| format!("Unable to get module repo spec for {key} from registry: Missing integrity for module {key}"))?;
                let reg_json = self.bazel_registry_json(hashes)?;
                let mut all_mirrors: Vec<String> = self.mirrors.clone();
                if let Some(Json::Arr(ms)) = reg_json.as_ref().and_then(|j| j.get("mirrors")) {
                    for m in ms {
                        if let Some(m) = m.as_str() {
                            if !all_mirrors.iter().any(|x| x == m) {
                                all_mirrors.push(m.to_string());
                            }
                        }
                    }
                }
                let mut urls = vec![];
                for m in &all_mirrors {
                    let (scheme, rest) = url.split_once("://").unwrap_or(("", &url));
                    let (auth, path) = if scheme == "file" {
                        ("", rest.to_string())
                    } else {
                        match rest.split_once('/') {
                            Some((a, p)) => (a, format!("/{p}")),
                            None => (rest, String::new()),
                        }
                    };
                    let constructed = construct_url(m, &[auth, &path]);
                    urls.push(constructed);
                }
                urls.push(url.clone());
                if let Some(Json::Arr(mu)) = sj.get("mirror_urls") {
                    urls.extend(mu.iter().filter_map(|x| x.as_str().map(String::from)));
                }
                let mut overlay_urls = vec![];
                let mut overlay_integrity = vec![];
                if let Some(Json::Obj(ov)) = sj.get("overlay") {
                    for (name, integ) in ov {
                        let i = integ.as_str().unwrap_or("");
                        if i.trim().is_empty() {
                            return Err(format!("Missing integrity for overlay file {name} of module {key}"));
                        }
                        overlay_urls.push((
                            name.clone(),
                            AttrVal::List(vec![AttrVal::Str(join_url(
                                &self.url,
                                &["modules", &key.name, &key.version.to_string(), "overlay", name],
                            ))]),
                        ));
                        overlay_integrity.push((name.clone(), AttrVal::Str(i.to_string())));
                    }
                }
                let mut attrs = vec![
                    ("urls".to_string(), AttrVal::List(urls.into_iter().map(AttrVal::Str).collect())),
                    ("integrity".into(), AttrVal::Str(integrity)),
                    ("strip_prefix".into(), AttrVal::Str(s("strip_prefix").unwrap_or_default())),
                    ("remote_patches".into(), AttrVal::Dict(patches_of(&sj)?.into_iter().map(|(k, v)| (k, v)).collect())),
                    ("remote_file_urls".into(), AttrVal::Dict(overlay_urls)),
                    ("remote_file_integrity".into(), AttrVal::Dict(overlay_integrity)),
                    ("remote_module_file_urls".into(), AttrVal::List(vec![AttrVal::Str(module_file_url)])),
                    ("remote_module_file_integrity".into(), AttrVal::Str(module_file_sri)),
                    (
                        "remote_patch_strip".into(),
                        AttrVal::Int(sj.get("patch_strip").and_then(|v| v.as_i64()).unwrap_or(0)),
                    ),
                ];
                if let Some(t) = s("archive_type").filter(|t| !t.is_empty()) {
                    attrs.push(("type".into(), AttrVal::Str(t)));
                }
                Ok(RepoSpec::new(HTTP_BZL, "http_archive", attrs))
            }
            "local_path" => {
                let path = s("path").unwrap_or_default();
                let mut p = path.clone();
                if !path.starts_with('/') {
                    let reg_json = self.bazel_registry_json(hashes)?;
                    let base = reg_json
                        .as_ref()
                        .and_then(|j| j.get("module_base_path"))
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .ok_or_else(|| format!("Provided non local registry for module {key}"))?;
                    p = format!("{base}/{path}");
                    if !base.starts_with('/') {
                        match &self.local_dir {
                            Some(d) => p = format!("{}/{p}", d.display()),
                            None => return Err(format!("Provided non local registry for module {key}")),
                        }
                    }
                }
                Ok(RepoSpec::new(LOCAL_BZL, "local_repository", vec![("path".into(), AttrVal::Str(p))]))
            }
            "git_repository" => {
                let remote = s("remote").ok_or_else(|| format!("Missing remote in git_repository source for module {key}"))?;
                const SCHEMES: &[&str] = &["http://", "https://", "file://", "ssh://", "git://"];
                if !SCHEMES.iter().any(|p| remote.starts_with(p)) {
                    return Err(format!(
                        "Invalid remote URL scheme: \"{remote}\" for module {key}. Only {} are allowed.",
                        SCHEMES.join(", ")
                    ));
                }
                if let Some(c) = s("commit") {
                    let ok = (7..=64).contains(&c.len()) && c.chars().all(|x| x.is_ascii_hexdigit());
                    if !ok {
                        return Err(format!("Invalid commit: \"{c}\" for module {key}. Must be a valid commit hash."));
                    }
                }
                if let Some(t) = s("tag") {
                    if t.starts_with('-') {
                        return Err(format!("Invalid tag: \"{t}\" for module {key}. Cannot start with '-'."));
                    }
                }
                let mut attrs: Vec<(String, AttrVal)> = vec![("remote".into(), AttrVal::Str(remote))];
                for k in ["commit", "shallow_since", "tag", "strip_prefix", "add_prefix"] {
                    if let Some(v) = s(k) {
                        attrs.push((k.into(), AttrVal::Str(v)));
                    }
                }
                for k in ["init_submodules", "verbose"] {
                    if let Some(b) = sj.get(k).and_then(|v| v.as_bool()) {
                        attrs.push((k.into(), AttrVal::Bool(b)));
                    }
                }
                attrs.push((
                    "remote_module_file_urls".into(),
                    AttrVal::List(vec![AttrVal::Str(module_file_url)]),
                ));
                attrs.push(("remote_module_file_integrity".into(), AttrVal::Str(module_file_sri)));
                attrs.push(("remote_patches".into(), AttrVal::Dict(patches_of(&sj)?)));
                attrs.push((
                    "remote_patch_strip".into(),
                    AttrVal::Int(sj.get("patch_strip").and_then(|v| v.as_i64()).unwrap_or(0)),
                ));
                Ok(RepoSpec::new(GIT_BZL, "git_repository", attrs))
            }
            other => Err(format!("Invalid source type \"{other}\" for module {key}")),
        }
    }

    /// `metadata.json` yanked versions (version -> reason). `None` if metadata is missing.
    pub fn yanked_versions(&self, module: &str, hashes: &mut FileHashes) -> Result<Option<BTreeMap<Version, String>>, String> {
        let url = join_url(&self.url, &["modules", module, "metadata.json"]);
        let Some(txt) = self.grab_text(&url, false, hashes)? else { return Ok(None) };
        if txt.trim().is_empty() {
            return Ok(None);
        }
        let j = match Json::parse(&txt) {
            Ok(j) => j,
            Err(e) => {
                // `IndexRegistry.getYankedVersions`: a malformed metadata file is a warning, the module is
                // treated as having no metadata. (The cause text is the JSON parser's, not Gson's.)
                let cause = match gson_lenient_error(&txt) {
                    Some(g) => format!("com.google.gson.stream.MalformedJsonException: {g}\nSee https://github.com/google/gson/blob/main/Troubleshooting.md#malformed-json"),
                    None => e.to_string(),
                };
                REGISTRY_WARNINGS.with(|w| {
                    w.borrow_mut().push(format!(
                        "WARNING: Could not read metadata file for module {module} from registry {}: Unable to parse json at url {url}: {cause}",
                        self.orig_url
                    ))
                });
                return Ok(None);
            }
        };
        let mut out = BTreeMap::new();
        if let Some(Json::Obj(y)) = j.get("yanked_versions") {
            for (k, v) in y {
                let ver = Version::parse(k).map_err(|e| format!("Unable to parse yanked version '{k}' in {url}: {e}"))?;
                out.insert(ver, v.as_str().unwrap_or("").to_string());
            }
        }
        Ok(Some(out))
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};

    pub fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zel-bzlmod-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    pub fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    /// Adds `modules/<name>/<version>/{MODULE.bazel,source.json}` (local_path source) to a registry.
    pub fn add_module(reg: &Path, name: &str, version: &str, module_bazel: &str, source_json: &str) {
        write(reg, &format!("modules/{name}/{version}/MODULE.bazel"), module_bazel);
        write(reg, &format!("modules/{name}/{version}/source.json"), source_json);
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    #[test]
    fn local_registry_module_and_source() {
        let d = tmpdir("reg1");
        add_module(&d, "foo", "1.0", "module(name='foo', version='1.0')", r#"{"type":"local_path","path":"/x/foo"}"#);
        write(&d, "modules/foo/metadata.json", r#"{"yanked_versions":{"0.9":"bad"}}"#);
        let r = Registry::new(&format!("file://{}", d.display())).unwrap();
        let mut hashes = FileHashes::new();
        let key = ModuleKey::new("foo", Version::parse("1.0").unwrap());
        assert!(r.get_module_file(&key, &mut hashes).unwrap().unwrap().contains("foo"));
        let missing = ModuleKey::new("bar", Version::parse("1.0").unwrap());
        assert!(r.get_module_file(&missing, &mut hashes).unwrap().is_none());
        assert!(!hashes.is_empty(), "hashes must be recorded for file:// registries as in Bazel IndexRegistry.grabFile");
        let spec = r.get_repo_spec(&key, &hashes.clone(), &mut hashes).unwrap();
        assert_eq!(spec.rule_name, "local_repository");
        let y = r.yanked_versions("foo", &mut hashes).unwrap().unwrap();
        assert_eq!(y.get(&Version::parse("0.9").unwrap()).unwrap(), "bad");
    }

    #[test]
    fn archive_source_with_mirrors_and_patches() {
        let d = tmpdir("reg2");
        add_module(
            &d,
            "foo",
            "1.0",
            "module(name='foo', version='1.0')",
            r#"{"url":"https://example.com/foo.tgz","integrity":"sha256-AAAA","strip_prefix":"foo-1.0","patches":{"p.patch":"sha256-BBBB"},"patch_strip":1}"#,
        );
        write(&d, "bazel_registry.json", r#"{"mirrors":["https://mirror.example/"]}"#);
        let r = Registry::new(&format!("file://{}", d.display())).unwrap();
        let mut h = FileHashes::new();
        let key = ModuleKey::new("foo", Version::parse("1.0").unwrap());
        r.get_module_file(&key, &mut h).unwrap();
        let spec = r.get_repo_spec(&key, &h.clone(), &mut h).unwrap();
        assert_eq!(spec.rule_name, "http_archive");
        let urls = spec.attr("urls").unwrap().str_list();
        assert_eq!(urls[0], "https://mirror.example/example.com/foo.tgz");
        assert_eq!(urls[1], "https://example.com/foo.tgz");
        assert_eq!(spec.attr("remote_patch_strip"), Some(&AttrVal::Int(1)));
    }

    #[test]
    fn enforce_mode_requires_known_hash_on_remote_registry() {
        let r = Registry::new("https://example.com")
            .unwrap()
            .with_known_hashes(FileHashes::new(), KnownHashesMode::Enforce);
        let key = ModuleKey::new("foo", Version::parse("1.0").unwrap());
        let e = r.get_module_file(&key, &mut FileHashes::new()).unwrap_err();
        assert!(e.contains("Missing checksum for registry file"), "{e}");
    }

    #[test]
    fn lockfile_hashed_file_served_from_repo_cache_without_network() {
        let d = tmpdir("reg_lockfirst");
        let content = "module(name='foo', version='1.0')";
        let h = crate::hash::sha256_hex(content.as_bytes());
        let cache = crate::download::RepositoryCache::new(d.join("cache"));
        cache.put(&crate::hash::Checksum::sha256_hex(&h).unwrap(), content.as_bytes(), "").unwrap();
        let url = "http://127.0.0.1:9/reg";
        let key = ModuleKey::new("foo", Version::parse("1.0").unwrap());
        let mut known = FileHashes::new();
        known.insert(format!("{url}/modules/foo/1.0/MODULE.bazel"), Some(h.clone()));
        let r = Registry::new(url)
            .unwrap()
            .with_known_hashes(known.clone(), KnownHashesMode::Enforce)
            .with_repo_cache(Some(d.join("cache")));
        let mut hashes = FileHashes::new();
        assert_eq!(r.get_module_file(&key, &mut hashes).unwrap().as_deref(), Some(content));
        assert_eq!(hashes.values().next().unwrap().as_deref(), Some(h.as_str()));
        // Without the cache the (unreachable) registry is contacted and fails.
        let r2 = Registry::new(url).unwrap().with_known_hashes(known, KnownHashesMode::Enforce);
        assert!(r2.get_module_file(&key, &mut FileHashes::new()).is_err());
    }

    #[test]
    fn file_registry_ignores_hashes_and_permits_updates() {
        let d = tmpdir("reg_file_ignore");
        add_module(&d, "foo", "1.0", "module(name='foo', version='1.0')", "{}");
        let reg_url = format!("file://{}", d.display());
        let r = Registry::new(&reg_url)
            .unwrap()
            .with_known_hashes(FileHashes::new(), KnownHashesMode::Enforce);
        let key = ModuleKey::new("foo", Version::parse("1.0").unwrap());
        let mut hashes = FileHashes::new();
        // File registry must succeed even with Enforce mode because file:// ignores hashes
        let content = r.get_module_file(&key, &mut hashes).unwrap();
        assert!(content.is_some());
        // And hashes MUST be recorded for file:// registries per Bazel IndexRegistry.grabFile
        assert!(!hashes.is_empty());
        assert!(hashes.iter().any(|(u, h)| u.starts_with("file://") && h.is_some()));
    }
}


/// Gson's (lenient) `JsonReader` message for a top-level object whose first member name is unquoted and is not
/// followed by a `:` (`{not json` -> `Expected ':' at line 1 column 7 path $.not`). Only this shape is modelled.
fn gson_lenient_error(txt: &str) -> Option<String> {
    let t = txt.trim_start();
    let lead = txt.len() - t.len();
    let rest = t.strip_prefix('{')?;
    let name_start = rest.len() - rest.trim_start().len();
    let r = &rest[name_start..];
    let name_len = r.find(|c: char| matches!(c, ':' | '=' | ',' | ';' | '#' | '[' | ']' | '{' | '}' | '\\' | '/' | ' ' | '\n' | '\r' | '\t' | '"' | '\''))?;
    if name_len == 0 {
        return None;
    }
    let name = &r[..name_len];
    let after = &r[name_len..];
    let ws = after.len() - after.trim_start().len();
    let bad = after.trim_start().chars().next()?;
    if matches!(bad, ':' | '=') {
        return None;
    }
    // Position of the offending character (1-based line, column counted after consuming it).
    let consumed = &txt[..lead + 1 + name_start + name_len + ws];
    let line = consumed.matches('\n').count() + 1;
    let col = consumed.rsplit('\n').next().map(|l| l.chars().count()).unwrap_or(0) + 2;
    Some(format!("Expected ':' at line {line} column {col} path $.{name}"))
}

#[cfg(test)]
mod gson_tests {
    use super::gson_lenient_error;

    #[test]
    fn unquoted_name_without_colon_matches_gson() {
        assert_eq!(gson_lenient_error("{not json").as_deref(), Some("Expected ':' at line 1 column 7 path $.not"));
        assert_eq!(gson_lenient_error("{\n  abc 1}").as_deref(), Some("Expected ':' at line 2 column 8 path $.abc"));
        assert_eq!(gson_lenient_error("{a: 1}"), None);
        assert_eq!(gson_lenient_error("[1]"), None);
    }
}
