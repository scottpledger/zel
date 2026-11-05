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

    /// Look up credential for a host.
    pub fn get_credential(&self, host: &str) -> Option<&NetrcMachine> {
        self.machines.get(host).or(self.default_machine.as_ref())
    }
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
