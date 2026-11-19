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
        return Ok(Vec::new());
    }
    let json = crate::json::Json::parse(trimmed).map_err(|e| {
        format!(
            "Invalid JSON from credential helper {}: {e}",
            helper.display()
        )
    })?;
    let crate::json::Json::Obj(top) = json else {
        return Ok(Vec::new());
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

/// Resolves headers for `url` from configured `--credential_helper` entries, if any match.
pub fn resolve_credential_helper_headers(
    url: &str,
    helpers: &[(String, PathBuf)],
    client_env: &BTreeMap<String, String>,
) -> Result<Vec<(String, String)>, String> {
    let host = url_host(url);
    if !helpers.is_empty() {
        // Specific scope wins over wildcard; later flag overrides earlier flag.
        let mut best: Option<&PathBuf> = None;
        for (scope, path) in helpers {
            if !scope.is_empty() && helper_matches_host(scope, host) {
                best = Some(path);
            }
        }
        if best.is_none() {
            for (scope, path) in helpers {
                if scope.is_empty() {
                    best = Some(path);
                }
            }
        }
        if let Some(helper) = best {
            return run_credential_helper(helper, url, client_env);
        }
    }
    Ok(Vec::new())
}

/// Resolves fallback `.netrc` `Authorization` header for `url` from `NETRC` / `HOME`.
pub fn resolve_netrc_headers(
    url: &str,
    client_env: &BTreeMap<String, String>,
) -> Vec<(String, String)> {
    let host = url_host(url);
    if let Some(netrc) = Netrc::load_from_env(client_env) {
        if let Some(m) = netrc.get_credential(host) {
            if let Some(pwd) = &m.password {
                if let Some(login) = &m.login {
                    let b64 = crate::hash::base64_encode(format!("{login}:{pwd}").as_bytes());
                    return vec![("Authorization".to_string(), format!("Basic {b64}"))];
                } else {
                    return vec![("Authorization".to_string(), format!("Bearer {pwd}"))];
                }
            }
        }
    }
    Vec::new()
}

/// Resolves auth headers for `url` using `--credential_helper` (highest priority) or `.netrc`.
pub fn resolve_auth_headers(
    url: &str,
    helpers: &[(String, PathBuf)],
    client_env: &BTreeMap<String, String>,
) -> Result<Vec<(String, String)>, String> {
    let hdrs = resolve_credential_helper_headers(url, helpers, client_env)?;
    if !hdrs.is_empty() {
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
}
