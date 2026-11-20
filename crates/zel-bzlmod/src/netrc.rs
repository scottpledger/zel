//! Pure-Rust .netrc parsing and credential resolution.
//!
//! Mirrors `com.google.devtools.build.lib.authandtls.NetrcParser`,
//! `NetrcCredentials`, and `GoogleAuthUtils.newCredentialsFromNetrc`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetrcMachine {
    pub machine: String,
    pub login: Option<String>,
    pub password: Option<String>,
    pub account: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Netrc {
    pub machines: BTreeMap<String, NetrcMachine>,
    pub default_machine: Option<NetrcMachine>,
}

impl Netrc {
    /// Parse `.netrc` text into a `Netrc` structure.
    ///
    /// Matches Bazel's `NetrcParser`: `#` starts a comment ONLY when it is the very first
    /// character of the raw line (before trimming); mid-line or indented `#` characters are
    /// ordinary token characters (e.g. passwords containing `#`). `macdef` blocks consume lines
    /// until the next blank line.
    pub fn parse(content: &str) -> Result<Self, String> {
        let mut tokens: Vec<String> = Vec::new();
        let mut in_macdef_body = false;

        for raw_line in content.lines() {
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            if in_macdef_body {
                if line.trim().is_empty() {
                    in_macdef_body = false;
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
            let line_tokens: Vec<&str> = trimmed.split_whitespace().collect();
            let mut idx = 0usize;
            while idx < line_tokens.len() {
                let w = line_tokens[idx];
                if w == "macdef" {
                    // `macdef <name>` consumes the rest of this line and subsequent lines until an empty line.
                    in_macdef_body = true;
                    break;
                }
                tokens.push(w.to_string());
                idx += 1;
            }
        }

        let mut machines = BTreeMap::new();
        let mut default_machine = None;
        let mut pos = 0usize;

        while pos < tokens.len() {
            let token = tokens[pos].as_str();
            pos += 1;
            match token {
                "machine" => {
                    if pos >= tokens.len() {
                        return Err("Unexpected EOF after 'machine' in .netrc".into());
                    }
                    let name = tokens[pos].clone();
                    pos += 1;
                    let machine = parse_machine_tokens(&name, &tokens, &mut pos)?;
                    machines.insert(name.to_ascii_lowercase(), machine);
                }
                "default" => {
                    let machine = parse_machine_tokens("default", &tokens, &mut pos)?;
                    default_machine = Some(machine);
                }
                other => {
                    return Err(format!("Unexpected token in .netrc: '{other}'"));
                }
            }
        }

        Ok(Self {
            machines,
            default_machine,
        })
    }

    /// Read and parse a `.netrc` file from disk (decoded as ISO-8859-1 like `NetrcParser`).
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path)
            .map_err(|e| format!("Failed to read .netrc at {}: {e}", path.display()))?;
        let content: String = bytes.into_iter().map(|b| b as char).collect();
        Self::parse(&content)
    }

    /// Locate user default `.netrc` file ($NETRC, then $HOME/.netrc or %USERPROFILE%/.netrc).
    pub fn find_user_netrc() -> Option<PathBuf> {
        if let Ok(netrc_env) = std::env::var("NETRC") {
            if !netrc_env.is_empty() {
                let p = PathBuf::from(netrc_env);
                if p.exists() {
                    return Some(p);
                }
            }
        }

        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                let p = PathBuf::from(home).join(".netrc");
                if p.exists() {
                    return Some(p);
                }
            }
        }

        if let Ok(userprofile) = std::env::var("USERPROFILE") {
            if !userprofile.is_empty() {
                let p = PathBuf::from(userprofile).join(".netrc");
                if p.exists() {
                    return Some(p);
                }
            }
        }

        None
    }

    /// Read the user's default `.netrc` if present.
    pub fn load_user_netrc() -> Option<Self> {
        let path = Self::find_user_netrc()?;
        Self::from_file(&path).ok()
    }

    /// Locate `.netrc` using `NETRC`, `HOME/.netrc`, or `USERPROFILE/.netrc` from `client_env`
    /// only (`BazelRepositoryModule.newCredentialsFromNetrc` never falls back to server JVM env).
    pub fn load_from_env(client_env: &BTreeMap<String, String>) -> Option<Self> {
        Self::load_from_client_env_only(client_env)
    }

    /// Loads `.netrc` using `NETRC`, `HOME/.netrc`, or `USERPROFILE/.netrc` from `client_env`
    /// only, without falling back to the server process environment.
    pub fn load_from_client_env_only(client_env: &BTreeMap<String, String>) -> Option<Self> {
        let get =
            |k: &str| -> Option<String> { client_env.get(k).filter(|v| !v.is_empty()).cloned() };
        if let Some(p) = get("NETRC").map(PathBuf::from).filter(|p| p.exists()) {
            return Self::from_file(&p).ok();
        }
        if let Some(p) = get("HOME")
            .map(|h| PathBuf::from(h).join(".netrc"))
            .filter(|p| p.exists())
        {
            return Self::from_file(&p).ok();
        }
        if let Some(p) = get("USERPROFILE")
            .map(|h| PathBuf::from(h).join(".netrc"))
            .filter(|p| p.exists())
        {
            return Self::from_file(&p).ok();
        }
        None
    }

    /// Look up credential for a host or URI host (`URI.getHost()` semantics: lowercased,
    /// percent-decoded, IPv6 brackets stripped, case-insensitive machine matching).
    pub fn get_credential(&self, host: &str) -> Option<&NetrcMachine> {
        let normalized = normalize_uri_host(host);
        self.machines
            .get(&normalized)
            .or_else(|| self.machines.get(&host.to_ascii_lowercase()))
            .or_else(|| self.machines.get(host))
            .or(self.default_machine.as_ref())
    }
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn percent_decode_str(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(hi), Some(lo)) = (hex_nibble(b[i + 1]), hex_nibble(b[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Normalizes a host string the way `NetrcCredentials` normalizes `URI.getHost()`:
/// strips `[...]`, percent-decodes, and ASCII-lowercases DNS names.
pub fn normalize_uri_host(raw_host: &str) -> String {
    let unbracketed = raw_host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(raw_host);
    percent_decode_str(unbracketed).to_ascii_lowercase()
}

pub fn url_host(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    let raw = if hostport.starts_with('[') {
        hostport
            .split(']')
            .next()
            .map(|s| s.trim_start_matches('['))
            .unwrap_or(hostport)
    } else {
        hostport.split(':').next().unwrap_or(hostport)
    };
    normalize_uri_host(raw)
}

fn helper_matches_host(scope: &str, host: &str) -> bool {
    if scope.is_empty() {
        return true;
    }
    if let Some(sub) = scope.strip_prefix("*.") {
        let sub_lc = sub.to_ascii_lowercase();
        let host_lc = host.to_ascii_lowercase();
        host_lc == sub_lc || host_lc.ends_with(&format!(".{sub_lc}"))
    } else {
        host.eq_ignore_ascii_case(scope)
    }
}

fn find_matching_credential_helper<'a>(
    host: &str,
    helpers: &'a [(String, PathBuf)],
) -> Option<&'a Path> {
    if helpers.is_empty() {
        return None;
    }
    // Mirrors `CredentialHelperProvider.findCredentialHelper`:
    // 1. Exact host match (later flag overrides earlier flag).
    // 2. Most specific `*.suffix` wildcard match (longest suffix wins; later flag breaks ties).
    // 3. Default un-scoped helper (later flag overrides earlier flag).
    let mut exact: Option<&'a Path> = None;
    for (scope, path) in helpers {
        if !scope.is_empty() && !scope.starts_with("*.") && host.eq_ignore_ascii_case(scope) {
            exact = Some(path.as_path());
        }
    }
    if exact.is_some() {
        return exact;
    }
    let mut best_wildcard: Option<(usize, &'a Path)> = None;
    for (scope, path) in helpers {
        if let Some(suffix) = scope.strip_prefix("*.") {
            if helper_matches_host(scope, host)
                && best_wildcard
                    .as_ref()
                    .is_none_or(|(best_len, _)| suffix.len() >= *best_len)
            {
                best_wildcard = Some((suffix.len(), path.as_path()));
            }
        }
    }
    if let Some((_, path)) = best_wildcard {
        return Some(path);
    }
    let mut default_helper: Option<&'a Path> = None;
    for (scope, path) in helpers {
        if scope.is_empty() {
            default_helper = Some(path.as_path());
        }
    }
    default_helper
}

fn is_etxtbsy(err: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        // ETXTBSY == 26 on Linux, macOS, and BSDs: raised transiently when a concurrent
        // thread forks while a newly-written executable's writable fd is closing.
        err.raw_os_error() == Some(26)
    }
    #[cfg(not(unix))]
    {
        let _ = err;
        false
    }
}

pub(crate) fn retry_etxtbsy<T>(mut op: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut attempt = 0u32;
    loop {
        match op() {
            Ok(val) => return Ok(val),
            Err(e) if is_etxtbsy(&e) && attempt < 20 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(
                    (attempt as u64 * 2).min(20),
                ));
            }
            Err(e) => return Err(e),
        }
    }
}

fn run_credential_helper(
    helper: &Path,
    url: &str,
    client_env: &BTreeMap<String, String>,
) -> Result<Vec<(String, String)>, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    #[cfg(windows)]
    let exec_buf = PathBuf::from(helper.to_string_lossy().replace('/', "\\"));
    #[cfg(windows)]
    let exec_helper: &Path = exec_buf.as_path();
    #[cfg(not(windows))]
    let exec_helper: &Path = helper;
    let mut child = retry_etxtbsy(|| {
        let mut cmd = Command::new(exec_helper);
        cmd.arg("get")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if !client_env.is_empty() {
            cmd.envs(client_env);
        }
        cmd.env_remove("RUN_UNDER_RUNFILES");
        cmd.spawn()
    })
    .map_err(|e| {
        format!(
            "Failed to invoke credential helper {}: {e}",
            helper.display()
        )
    })?;
    if let Some(mut stdin) = child.stdin.take() {
        let escaped = url.replace('\\', "\\\\").replace('"', "\\\"");
        let _ = write!(stdin, "{{\"uri\":\"{escaped}\"}}");
    }
    let out = child.wait_with_output().map_err(|e| {
        format!(
            "Failed to wait for credential helper {}: {e}",
            helper.display()
        )
    })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "Credential helper {} exited with {}: {}",
            helper.display(),
            out.status,
            stderr.trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "Credential helper {} exited without output",
            helper.display()
        ));
    }
    let json = crate::json::Json::parse(trimmed).map_err(|e| {
        format!(
            "Invalid JSON from credential helper {}: {e}",
            helper.display()
        )
    })?;
    let crate::json::Json::Obj(top) = json else {
        return Err(format!(
            "Expected JSON object from credential helper {}",
            helper.display()
        ));
    };
    let mut result = Vec::new();
    if let Some((_, crate::json::Json::Obj(hdrs))) = top.into_iter().find(|(k, _)| k == "headers") {
        for (name, vals) in hdrs {
            if let crate::json::Json::Arr(items) = vals {
                for item in items {
                    if let crate::json::Json::Str(s) = item {
                        result.push((name.clone(), s));
                    }
                }
            }
        }
    }
    Ok(result)
}

/// Resolves headers for `url` from configured `--credential_helper` entries.
///
/// Mirrors `CredentialHelperCredentials.getCredentialsFromHelper` (`CredentialHelperCredentials.java:57-97`):
/// - `Ok(Some(headers))` when a configured helper matches `url`'s host (even if
///   `headers` is empty `{}` or contains no `Authorization` header);
/// - `Ok(None)` when no configured helper matches `url`;
/// - `Err(_)` when a matching helper fails.
pub fn resolve_credential_helper_headers(
    url: &str,
    helpers: &[(String, PathBuf)],
    client_env: &BTreeMap<String, String>,
) -> Result<Option<Vec<(String, String)>>, String> {
    let host = url_host(url);
    match find_matching_credential_helper(&host, helpers) {
        Some(helper) => run_credential_helper(helper, url, client_env).map(Some),
        None => Ok(None),
    }
}

/// Convenience wrapper for [`resolve_credential_helper_headers`] with an empty `client_env`.
pub fn resolve_credential_helper_opt(
    url: &str,
    helpers: &[(String, PathBuf)],
) -> Result<Option<Vec<(String, String)>>, String> {
    resolve_credential_helper_headers(url, helpers, &BTreeMap::new())
}

/// Resolves fallback `.netrc` `Authorization` header for `url` (`Basic base64(login:password)`
/// using empty login when `login` is omitted, matching Bazel's `NetrcCredentials`).
pub fn resolve_netrc_headers(
    url: &str,
    client_env: &BTreeMap<String, String>,
) -> Vec<(String, String)> {
    let host = url_host(url);
    if let Some(netrc) = Netrc::load_from_client_env_only(client_env) {
        if let Some(m) = netrc.get_credential(&host) {
            if let Some(pwd) = &m.password {
                let login = m.login.as_deref().unwrap_or("");
                let b64 = crate::hash::base64_encode(format!("{login}:{pwd}").as_bytes());
                return vec![("Authorization".to_string(), format!("Basic {b64}"))];
            }
        }
    }
    Vec::new()
}

/// Resolves auth headers for `url` using `--credential_helper` (when a helper matches `url`) or
/// falling back to `.netrc` only when no helper matches (`CredentialHelperCredentials.java:68-76`).
pub fn resolve_auth_headers(
    url: &str,
    helpers: &[(String, PathBuf)],
    client_env: &BTreeMap<String, String>,
) -> Result<Vec<(String, String)>, String> {
    if let Some(hdrs) = resolve_credential_helper_headers(url, helpers, client_env)? {
        return Ok(hdrs);
    }
    Ok(resolve_netrc_headers(url, client_env))
}

fn parse_machine_tokens(
    name: &str,
    tokens: &[String],
    pos: &mut usize,
) -> Result<NetrcMachine, String> {
    let mut machine = NetrcMachine {
        machine: name.to_string(),
        login: None,
        password: None,
        account: None,
    };

    while *pos < tokens.len() {
        let t = tokens[*pos].as_str();
        match t {
            "login" => {
                *pos += 1;
                if *pos >= tokens.len() {
                    return Err("Unexpected EOF after 'login' in .netrc".into());
                }
                machine.login = Some(tokens[*pos].clone());
                *pos += 1;
            }
            "password" => {
                *pos += 1;
                if *pos >= tokens.len() {
                    return Err("Unexpected EOF after 'password' in .netrc".into());
                }
                machine.password = Some(tokens[*pos].clone());
                *pos += 1;
            }
            "account" => {
                *pos += 1;
                if *pos >= tokens.len() {
                    return Err("Unexpected EOF after 'account' in .netrc".into());
                }
                machine.account = Some(tokens[*pos].clone());
                *pos += 1;
            }
            "machine" | "default" | "macdef" => {
                break;
            }
            _ => {
                *pos += 1;
            }
        }
    }

    Ok(machine)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_netrc() {
        let text = r#"
# comment line
machine example.com login user1 password secret#with#hashes
default login defuser password defpass
"#;
        let netrc = Netrc::parse(text).unwrap();
        let m = netrc.get_credential("example.com").unwrap();
        assert_eq!(m.login.as_deref(), Some("user1"));
        assert_eq!(m.password.as_deref(), Some("secret#with#hashes"));

        // Uppercase, percent-encoded, and IPv6 hosts match via URI.getHost() normalization.
        assert_eq!(
            netrc
                .get_credential(&url_host("https://Ex%61mple.COM/path"))
                .unwrap()
                .login
                .as_deref(),
            Some("user1")
        );

        let d = netrc.get_credential("other.com").unwrap();
        assert_eq!(d.login.as_deref(), Some("defuser"));
        assert_eq!(d.password.as_deref(), Some("defpass"));
    }

    #[test]
    fn resolve_netrc_headers_ignores_server_process_env() {
        if let Some(child_netrc) = std::env::var_os("ZEL_NETRC_PROC_ISOLATION_CHILD") {
            let server_netrc = PathBuf::from(&child_netrc);
            assert_eq!(
                std::env::var_os("NETRC").as_deref(),
                Some(child_netrc.as_os_str())
            );
            assert!(server_netrc.is_file());

            let empty_client_env = BTreeMap::new();
            let leaked =
                resolve_netrc_headers("https://leak.example.com/pkg.tar.gz", &empty_client_env);
            let leaked_auth = resolve_auth_headers(
                "https://leak.example.com/pkg.tar.gz",
                &[],
                &empty_client_env,
            )
            .unwrap();

            let mut explicit_client_env = BTreeMap::new();
            explicit_client_env.insert(
                "NETRC".to_string(),
                server_netrc.to_string_lossy().into_owned(),
            );
            let allowed =
                resolve_netrc_headers("https://leak.example.com/pkg.tar.gz", &explicit_client_env);

            assert!(
                leaked.is_empty(),
                "server process NETRC must not leak when absent from client_env: {leaked:?}"
            );
            assert!(leaked_auth.is_empty(), "{leaked_auth:?}");
            assert_eq!(
                allowed,
                vec![(
                    "Authorization".to_string(),
                    "Basic c3J2dXNlcjpzcnZzZWNyZXQ=".to_string()
                )]
            );
            println!("ZEL_NETRC_PROC_ISOLATION_CHILD_OK");
            return;
        }

        let dir =
            std::env::temp_dir().join(format!("zel-netrc-proc-isolation-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server_netrc = dir.join(".netrc");
        std::fs::write(
            &server_netrc,
            "machine leak.example.com login srvuser password srvsecret\n",
        )
        .unwrap();

        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "netrc::tests::resolve_netrc_headers_ignores_server_process_env",
                "--nocapture",
            ])
            .env_remove("TESTBRIDGE_TEST_ONLY")
            .env("ZEL_NETRC_PROC_ISOLATION_CHILD", &server_netrc)
            .env("NETRC", &server_netrc)
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success() && stdout.contains("ZEL_NETRC_PROC_ISOLATION_CHILD_OK"),
            "isolated child failed (status={:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
            out.status.code()
        );
    }

    #[test]
    fn concurrent_proc_isolation_child_never_mutates_parent_netrc_env() {
        use std::sync::{Arc, Barrier};

        let baseline = std::env::var_os("NETRC");
        let barrier = Arc::new(Barrier::new(3));
        let b1 = Arc::clone(&barrier);
        let baseline_sampler = baseline.clone();
        let sampler = std::thread::spawn(move || {
            b1.wait();
            for _ in 0..200 {
                assert_eq!(
                    std::env::var_os("NETRC"),
                    baseline_sampler,
                    "parent process NETRC env must never change while isolation child runs"
                );
                std::thread::yield_now();
            }
        });

        let b2 = Arc::clone(&barrier);
        let runner = std::thread::spawn(move || {
            let dir = std::env::temp_dir().join(format!(
                "zel-netrc-proc-isolation-race-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let server_netrc = dir.join(".netrc");
            std::fs::write(
                &server_netrc,
                "machine leak.example.com login srvuser password srvsecret\n",
            )
            .unwrap();
            b2.wait();
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "netrc::tests::resolve_netrc_headers_ignores_server_process_env",
                    "--nocapture",
                ])
                .env_remove("TESTBRIDGE_TEST_ONLY")
                .env("ZEL_NETRC_PROC_ISOLATION_CHILD", &server_netrc)
                .env("NETRC", &server_netrc)
                .output()
                .unwrap();
            let _ = std::fs::remove_dir_all(&dir);
            assert!(
                out.status.success()
                    && String::from_utf8_lossy(&out.stdout)
                        .contains("ZEL_NETRC_PROC_ISOLATION_CHILD_OK")
            );
        });

        barrier.wait();
        sampler.join().unwrap();
        runner.join().unwrap();
        assert_eq!(std::env::var_os("NETRC"), baseline);
    }

    #[cfg(unix)]
    #[test]
    fn credential_helper_match_suppresses_netrc_fallback_while_non_match_falls_back() {
        use std::os::unix::fs::PermissionsExt;

        let dir =
            std::env::temp_dir().join(format!("zel-netrc-cred-precedence-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let netrc = dir.join(".netrc");
        std::fs::write(
            &netrc,
            "machine target.example.com login netrc_user password netrc_secret\n",
        )
        .unwrap();
        let mut env = BTreeMap::new();
        env.insert("NETRC".to_string(), netrc.to_string_lossy().into_owned());

        let custom_helper = dir.join("custom_helper.sh");
        std::fs::write(
            &custom_helper,
            "#!/bin/sh\necho '{\"headers\":{\"X-Custom-Auth\":[\"custom-tok\"]}}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&custom_helper, std::fs::Permissions::from_mode(0o755)).unwrap();

        let empty_helper = dir.join("empty_helper.sh");
        std::fs::write(&empty_helper, "#!/bin/sh\necho '{}'\n").unwrap();
        std::fs::set_permissions(&empty_helper, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Matching helper returning custom non-Authorization header does NOT fall back to .netrc.
        let hdrs_custom = resolve_auth_headers(
            "https://target.example.com/archive.tar.gz",
            &[("target.example.com".to_string(), custom_helper.clone())],
            &env,
        )
        .unwrap();
        assert_eq!(
            hdrs_custom,
            vec![("X-Custom-Auth".to_string(), "custom-tok".to_string())]
        );

        // Matching helper returning empty `{}` headers does NOT fall back to .netrc.
        let hdrs_empty = resolve_auth_headers(
            "https://target.example.com/archive.tar.gz",
            &[("target.example.com".to_string(), empty_helper)],
            &env,
        )
        .unwrap();
        assert!(hdrs_empty.is_empty(), "{hdrs_empty:?}");

        // Non-matching helper (configured for other.example.com) DOES fall back to .netrc.
        let hdrs_fallback = resolve_auth_headers(
            "https://target.example.com/archive.tar.gz",
            &[("other.example.com".to_string(), custom_helper)],
            &env,
        )
        .unwrap();
        let expected_b64 = crate::hash::base64_encode(b"netrc_user:netrc_secret");
        assert_eq!(
            hdrs_fallback,
            vec![("Authorization".to_string(), format!("Basic {expected_b64}"))]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn credential_helper_get_subcommand_scope_specificity_and_netrc_basic_without_login() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("zel-netrc-parity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let make_helper = |name: &str, token: &str| -> PathBuf {
            let p = dir.join(name);
            // Fails unless invoked with `get` as $1!
            let script = format!(
                "#!/bin/sh\nif [ \"$1\" != \"get\" ]; then echo \"missing get\" >&2; exit 2; fi\ncat >/dev/null\necho '{{\"headers\":{{\"Authorization\":[\"Bearer {token}\"]}}}}'\n"
            );
            std::fs::write(&p, script).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };

        let h_default = make_helper("h_default.sh", "tok-default");
        let h_short_wild = make_helper("h_short.sh", "tok-short-wild");
        let h_long_wild = make_helper("h_long.sh", "tok-long-wild");
        let h_exact = make_helper("h_exact.sh", "tok-exact");

        // Exact host beats wildcard even when wildcard is listed later; longer wildcard beats shorter.
        let helpers = vec![
            (String::new(), h_default),
            ("a.sub.example.com".to_string(), h_exact),
            ("*.sub.example.com".to_string(), h_long_wild),
            ("*.example.com".to_string(), h_short_wild),
        ];
        let empty_env = BTreeMap::new();
        assert_eq!(
            resolve_credential_helper_headers(
                "https://a.sub.example.com/pkg.tgz",
                &helpers,
                &empty_env
            )
            .unwrap(),
            Some(vec![(
                "Authorization".to_string(),
                "Bearer tok-exact".to_string()
            )])
        );
        assert_eq!(
            resolve_credential_helper_headers(
                "https://b.sub.example.com/pkg.tgz",
                &helpers,
                &empty_env
            )
            .unwrap(),
            Some(vec![(
                "Authorization".to_string(),
                "Bearer tok-long-wild".to_string()
            )])
        );
        assert_eq!(
            resolve_credential_helper_headers("https://example.com/pkg.tgz", &helpers, &empty_env)
                .unwrap(),
            Some(vec![(
                "Authorization".to_string(),
                "Bearer tok-short-wild".to_string()
            )])
        );
        assert_eq!(
            resolve_credential_helper_headers(
                "https://unmatched.org/pkg.tgz",
                &helpers,
                &empty_env
            )
            .unwrap(),
            Some(vec![(
                "Authorization".to_string(),
                "Bearer tok-default".to_string()
            )])
        );

        // .netrc without `login` encodes Basic `:password` (`OmZvb2Jhcg==` for `foobar`), not Bearer.
        let netrc_path = dir.join(".netrc");
        std::fs::write(&netrc_path, "machine basic.example.com password foobar\n").unwrap();
        let mut env = BTreeMap::new();
        env.insert("NETRC".to_string(), netrc_path.display().to_string());
        assert!(Netrc::load_from_client_env_only(&BTreeMap::new()).is_none());
        assert!(Netrc::load_from_client_env_only(&env).is_some());
        assert_eq!(
            resolve_netrc_headers("https://basic.example.com/archive.tar.gz", &env),
            vec![(
                "Authorization".to_string(),
                "Basic OmZvb2Jhcg==".to_string()
            )]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn netrc_latin1_file_and_case_insensitive_machine_lookup() {
        let dir = std::env::temp_dir().join(format!("zel-netrc-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let netrc_path = dir.join(".netrc");
        // Write raw ISO-8859-1 byte 0xe9 ('é') in password and uppercase machine host.
        let mut raw = Vec::new();
        raw.extend_from_slice(b"machine Example.ORG login alice password s");
        raw.push(0xe9);
        raw.extend_from_slice(b"cret\n");
        std::fs::write(&netrc_path, &raw).unwrap();

        let netrc = Netrc::from_file(&netrc_path).unwrap();
        let cred = netrc.get_credential("example.org").unwrap();
        assert_eq!(cred.login.as_deref(), Some("alice"));
        assert_eq!(cred.password.as_deref(), Some("sécret"));
        assert!(netrc.get_credential("EXAMPLE.ORG").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn netrc_hash_only_starts_comment_at_column_zero() {
        // Indented '#' is not a comment in Bazel's NetrcParser and triggers unexpected token.
        assert!(Netrc::parse("  # indented").is_err());
        // Column-0 '#' is ignored, while '#' inside password is kept intact.
        let n = Netrc::parse("#header\nmachine ::1 login u password p#1\n").unwrap();
        let m = n.get_credential(&url_host("https://[::1]:8443/x")).unwrap();
        assert_eq!(m.machine, "::1");
        assert_eq!(m.password.as_deref(), Some("p#1"));
    }

    #[test]
    fn percent_decode_host_handles_multibyte_utf8_and_malformed_escapes_without_panicking() {
        // `%` followed by 2-byte (`é`), 3-byte (`日`), and 4-byte (`🦀`) UTF-8 sequences
        // must never slice across non-char boundaries; non-hex sequences stay literal.
        assert_eq!(percent_decode_str("%é"), "%é");
        assert_eq!(percent_decode_str("%日"), "%日");
        assert_eq!(percent_decode_str("%🦀"), "%🦀");
        assert_eq!(percent_decode_str("host%"), "host%");
        assert_eq!(percent_decode_str("host%a"), "host%a");
        assert_eq!(percent_decode_str("host%zz.example"), "host%zz.example");
        // Valid UTF-8 percent-encoded sequence (`%C3%A9` -> `é`).
        assert_eq!(percent_decode_str("caf%C3%A9.example"), "café.example");

        // End-to-end through `url_host` and `Netrc::get_credential`.
        let netrc = Netrc::parse(
            "machine café.example login utf8user password utf8pass\n\
             default login defuser password defpass\n",
        )
        .unwrap();
        for malformed_url in [
            "https://host%é.example/archive.tar.gz",
            "https://host%日.example/archive.tar.gz",
            "https://host%🦀.example/archive.tar.gz",
            "https://host%/archive.tar.gz",
            "https://host%a/archive.tar.gz",
            "https://host%zz.example/archive.tar.gz",
        ] {
            let host = url_host(malformed_url);
            let cred = netrc.get_credential(&host).unwrap();
            assert_eq!(cred.login.as_deref(), Some("defuser"), "{malformed_url}");
        }
        // Malformed hosts passed directly to `get_credential` also must not panic.
        assert_eq!(
            netrc.get_credential("%日").unwrap().login.as_deref(),
            Some("defuser")
        );
        assert_eq!(
            netrc
                .get_credential(&url_host("https://CAF%C3%A9.EXAMPLE/file.tgz"))
                .unwrap()
                .login
                .as_deref(),
            Some("utf8user")
        );
    }

    #[cfg(unix)]
    #[test]
    fn retry_etxtbsy_retries_transient_busy_surfaces_persistent_and_skips_other_errors() {
        use std::os::unix::fs::PermissionsExt;

        // 1. Transient ETXTBSY (errno 26) succeeds once the busy fd closes.
        let mut calls = 0u32;
        let val = retry_etxtbsy(|| {
            calls += 1;
            if calls < 4 {
                Err(std::io::Error::from_raw_os_error(26))
            } else {
                Ok(42)
            }
        })
        .unwrap();
        assert_eq!(val, 42);
        assert_eq!(calls, 4);

        // 2. Non-ETXTBSY errors (e.g. ENOENT) fail immediately without retrying.
        let mut enoent_calls = 0u32;
        let err = retry_etxtbsy::<()>(|| {
            enoent_calls += 1;
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"))
        })
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(enoent_calls, 1);

        // 3. Persistent ETXTBSY surfaces the underlying os error 26 after bounded retries.
        let mut persistent_calls = 0u32;
        let err = retry_etxtbsy::<()>(|| {
            persistent_calls += 1;
            Err(std::io::Error::from_raw_os_error(26))
        })
        .unwrap_err();
        assert_eq!(err.raw_os_error(), Some(26));
        assert_eq!(persistent_calls, 21);

        // 4. Real open-write-fd race on an executable credential helper script.
        let dir = std::env::temp_dir().join(format!("zel-netrc-etxtbsy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let helper = dir.join("helper_busy.sh");
        std::fs::write(
            &helper,
            "#!/bin/sh\necho '{\"headers\":{\"Authorization\":[\"Bearer RETRIED\"]}}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();

        let write_fd = std::fs::OpenOptions::new()
            .write(true)
            .open(&helper)
            .unwrap();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(25));
            drop(write_fd);
        });

        let hdrs = resolve_credential_helper_headers(
            "https://example.com/archive.tar.gz",
            &[("example.com".to_string(), helper)],
            &BTreeMap::new(),
        )
        .unwrap()
        .unwrap();
        releaser.join().unwrap();
        assert_eq!(
            hdrs,
            vec![("Authorization".to_string(), "Bearer RETRIED".to_string())]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn credential_helper_retries_transient_etxtbsy() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::{Arc, Barrier};

        let dir = std::env::temp_dir().join(format!(
            "zel-netrc-etxtbsy-unit-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let helper = dir.join("helper.sh");

        let barrier = Arc::new(Barrier::new(2));
        let barrier_writer = Arc::clone(&barrier);
        let helper_writer = helper.clone();

        let writer = std::thread::spawn(move || {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&helper_writer)
                .unwrap();
            file.write_all(
                b"#!/bin/sh\necho '{\"headers\":{\"Authorization\":[\"Bearer RETRY_OK\"]}}'\n",
            )
            .unwrap();
            std::fs::set_permissions(&helper_writer, std::fs::Permissions::from_mode(0o755))
                .unwrap();
            barrier_writer.wait();
            std::thread::sleep(std::time::Duration::from_millis(25));
            drop(file);
        });

        barrier.wait();
        let hdrs = resolve_auth_headers(
            "https://example.com/modules/a/1.0/MODULE.bazel",
            &[("example.com".to_string(), helper)],
            &BTreeMap::new(),
        )
        .unwrap();
        writer.join().unwrap();
        assert_eq!(
            hdrs,
            vec![("Authorization".to_string(), "Bearer RETRY_OK".to_string())]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
