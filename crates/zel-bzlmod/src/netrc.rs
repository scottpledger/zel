//! Pure-Rust .netrc parsing and credential resolution.
//!
//! Mirrors `com.google.devtools.build.lib.authandtls.NetrcParser` and
//! `GoogleAuthUtils.newCredentialsFromNetrc`.

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
    pub fn parse(content: &str) -> Result<Self, String> {
        let mut tokens = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            // Strip any comment starting with '#' mid-line
            let clean_line = if let Some(idx) = line.find('#') {
                &line[..idx]
            } else {
                line
            };
            for word in clean_line.split_whitespace() {
                tokens.push(word);
            }
        }

        let mut machines = BTreeMap::new();
        let mut default_machine = None;
        let mut pos = 0usize;

        while pos < tokens.len() {
            let token = tokens[pos];
            pos += 1;
            match token {
                "machine" => {
                    if pos >= tokens.len() {
                        return Err("Unexpected EOF after 'machine' in .netrc".into());
                    }
                    let name = tokens[pos].to_string();
                    pos += 1;
                    let machine = parse_machine_tokens(&name, &tokens, &mut pos)?;
                    machines.insert(name, machine);
                }
                "default" => {
                    let machine = parse_machine_tokens("default", &tokens, &mut pos)?;
                    default_machine = Some(machine);
                }
                "macdef" => {
                    // Skip macdef macro definition until empty line or next token
                    if pos < tokens.len() {
                        pos += 1;
                    }
                    while pos < tokens.len() {
                        let t = tokens[pos];
                        if t == "machine" || t == "default" || t == "macdef" {
                            break;
                        }
                        pos += 1;
                    }
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

    /// Read and parse a `.netrc` file from disk.
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read .netrc at {}: {e}", path.display()))?;
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

    /// Locate `.netrc` checking `client_env` first, then process `std::env`.
    pub fn load_from_env(client_env: &BTreeMap<String, String>) -> Option<Self> {
        let get = |k: &str| -> Option<String> {
            client_env
                .get(k)
                .filter(|v| !v.is_empty())
                .cloned()
                .or_else(|| std::env::var(k).ok().filter(|v| !v.is_empty()))
        };
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

    /// Look up credential for a host.
    pub fn get_credential(&self, host: &str) -> Option<&NetrcMachine> {
        self.machines.get(host).or(self.default_machine.as_ref())
    }
}

fn url_host(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
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

fn helper_matches_host(scope: &str, host: &str) -> bool {
    if scope.is_empty() {
        return true;
    }
    if let Some(sub) = scope.strip_prefix("*.") {
        host.eq_ignore_ascii_case(sub)
            || host
                .to_ascii_lowercase()
                .ends_with(&format!(".{}", sub.to_ascii_lowercase()))
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

fn run_credential_helper(
    helper: &Path,
    url: &str,
    client_env: &BTreeMap<String, String>,
) -> Result<Vec<(String, String)>, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(helper);
    cmd.arg("get")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !client_env.is_empty() {
        cmd.envs(client_env);
    }
    let mut child = cmd.spawn().map_err(|e| {
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
/// Mirrors `CredentialHelperCredentials.getCredentialsFromHelper`:
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
    match find_matching_credential_helper(host, helpers) {
        Some(helper) => run_credential_helper(helper, url, client_env).map(Some),
        None => Ok(None),
    }
}

/// Resolves fallback `.netrc` `Authorization` header for `url` (`Basic base64(login:password)`
/// using empty login when `login` is omitted, matching Bazel's `NetrcCredentials`).
pub fn resolve_netrc_headers(
    url: &str,
    client_env: &BTreeMap<String, String>,
) -> Vec<(String, String)> {
    let host = url_host(url);
    if let Some(netrc) = Netrc::load_from_env(client_env) {
        if let Some(m) = netrc.get_credential(host) {
            if let Some(pwd) = &m.password {
                let login = m.login.as_deref().unwrap_or("");
                let b64 = crate::hash::base64_encode(format!("{login}:{pwd}").as_bytes());
                return vec![("Authorization".to_string(), format!("Basic {b64}"))];
            }
        }
    }
    Vec::new()
}

/// Resolves auth headers for `url` using `--credential_helper` (highest priority) or `.netrc`.
///
/// Mirrors `CredentialHelperCredentials.getRequestMetadata`: when a `--credential_helper`
/// matches `url`, its response headers are returned directly without falling back to `.netrc`
/// (even when the helper returns `{}` or only non-`Authorization` headers).
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
    tokens: &[&str],
    pos: &mut usize,
) -> Result<NetrcMachine, String> {
    let mut machine = NetrcMachine {
        machine: name.to_string(),
        login: None,
        password: None,
        account: None,
    };

    while *pos < tokens.len() {
        let t = tokens[*pos];
        match t {
            "login" => {
                *pos += 1;
                if *pos >= tokens.len() {
                    return Err("Unexpected EOF after 'login' in .netrc".into());
                }
                machine.login = Some(tokens[*pos].to_string());
                *pos += 1;
            }
            "password" => {
                *pos += 1;
                if *pos >= tokens.len() {
                    return Err("Unexpected EOF after 'password' in .netrc".into());
                }
                machine.password = Some(tokens[*pos].to_string());
                *pos += 1;
            }
            "account" => {
                *pos += 1;
                if *pos >= tokens.len() {
                    return Err("Unexpected EOF after 'account' in .netrc".into());
                }
                machine.account = Some(tokens[*pos].to_string());
                *pos += 1;
            }
            "machine" | "default" | "macdef" => {
                break;
            }
            _ => {
                // Ignore unknown per-machine tokens (e.g. force, port, etc.)
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
machine example.com login user1 password secret
default login defuser password defpass
"#;
        let netrc = Netrc::parse(text).unwrap();
        let m = netrc.get_credential("example.com").unwrap();
        assert_eq!(m.login.as_deref(), Some("user1"));
        assert_eq!(m.password.as_deref(), Some("secret"));

        let d = netrc.get_credential("other.com").unwrap();
        assert_eq!(d.login.as_deref(), Some("defuser"));
        assert_eq!(d.password.as_deref(), Some("defpass"));
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
}
