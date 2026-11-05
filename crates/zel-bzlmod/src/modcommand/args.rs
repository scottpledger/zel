//! Port of `ModuleArg` and `ExtensionArg`: references to modules / extensions given on the
//! `mod` command line (or as the value of `--from`, `--base_module`, ...).

use super::inspector::Inspector;
use super::{ModError, ModErrorKind};
use crate::model::ModuleKey;
use crate::resolve::ExtensionId;
use crate::version::Version;
use std::collections::BTreeMap;
use std::fmt;

fn invalid(msg: String) -> ModError {
    ModError::raw(ModErrorKind::InvalidArguments, msg)
}

fn push_unique(v: &mut Vec<ModuleKey>, k: ModuleKey) {
    if !v.contains(&k) {
        v.push(k);
    }
}

/// `[a, b, c]` rendering of a list (Java `ImmutableSet/List.toString()`).
pub fn list_string<T: fmt::Display>(items: &[T]) -> String {
    format!("[{}]", items.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", "))
}

/// The apparent -> canonical repo mapping of the base module (with its context repo).
#[derive(Debug, Clone, Default)]
pub struct RepoMapping {
    pub entries: BTreeMap<String, String>,
    /// The same entries in `RepositoryMapping` iteration order (extension repos first).
    pub ordered: Vec<(String, String)>,
    /// Canonical name of the repo this mapping belongs to ("" for the main repo).
    pub context_repo: String,
}

impl RepoMapping {
    fn context_display(&self) -> String {
        if self.context_repo.is_empty() {
            "main repository".to_string()
        } else {
            format!("repository '@@{}'", self.context_repo)
        }
    }
}

/// A reference to one or more modules in the external dependency graph.
#[derive(Debug, Clone, PartialEq)]
pub enum ModuleArg {
    /// `<module>@<version>` (`_` for the empty version) or `<root>`.
    SpecificVersionOfModule(ModuleKey),
    /// `<module>`: all versions of a module.
    AllVersionsOfModule(String),
    /// `@<name>`: a repo with the given apparent name (relative to the base module).
    ApparentRepoName(String),
    /// `@@<name>`: a repo with the given canonical name.
    CanonicalRepoName(String),
}

impl fmt::Display for ModuleArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModuleArg::SpecificVersionOfModule(k) => write!(f, "{k}"),
            ModuleArg::AllVersionsOfModule(n) => f.write_str(n),
            ModuleArg::ApparentRepoName(n) => write!(f, "@{n}"),
            ModuleArg::CanonicalRepoName(n) => write!(f, "@@{n}"),
        }
    }
}

fn validate_canonical_repo_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Ok(());
    }
    if name == "." || name == ".." {
        return Err(format!("invalid repository name '{name}': repo names are not allowed to be '{name}'"));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+')) {
        return Err(format!(
            "invalid repository name '{name}': repo names may contain only A-Z, a-z, 0-9, '-', '_', '.' and '+'"
        ));
    }
    Ok(())
}

fn validate_user_provided_repo_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let ok = match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_')),
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid user-provided repo name '{name}': valid names may contain only A-Z, a-z, 0-9, '-', '_', '.', and must start with a letter or a number"
        ))
    }
}

impl ModuleArg {
    /// `ModuleArgConverter.convert`. The error is the `OptionsParsingException` message.
    pub fn parse(input: &str) -> Result<ModuleArg, String> {
        if input.eq_ignore_ascii_case("<root>") {
            return Ok(ModuleArg::SpecificVersionOfModule(ModuleKey::root()));
        }
        if let Some(name) = input.strip_prefix("@@") {
            validate_canonical_repo_name(name).map_err(|e| format!("invalid argument '{input}': {e}"))?;
            return Ok(ModuleArg::CanonicalRepoName(name.to_string()));
        }
        if let Some(name) = input.strip_prefix('@') {
            validate_user_provided_repo_name(name).map_err(|e| format!("invalid argument '{input}': {e}"))?;
            return Ok(ModuleArg::ApparentRepoName(name.to_string()));
        }
        if let Some(at) = input.find('@') {
            let module_name = &input[..at];
            let version_str = &input[at + 1..];
            if version_str.is_empty() {
                return Err(format!("invalid argument '{input}': use _ for the empty version"));
            }
            let version = if version_str == "_" {
                Version::empty()
            } else {
                Version::parse(version_str).map_err(|e| format!("invalid argument '{input}': {e}"))?
            };
            return Ok(ModuleArg::SpecificVersionOfModule(ModuleKey::new(module_name, version)));
        }
        Ok(ModuleArg::AllVersionsOfModule(input.to_string()))
    }

    /// `CommaSeparatedModuleArgListConverter.convert`.
    pub fn parse_list(input: &str) -> Result<Vec<ModuleArg>, String> {
        let mut out = vec![];
        for part in split_non_empty(input)? {
            out.push(ModuleArg::parse(part)?);
        }
        Ok(out)
    }

    /// Resolves this argument to a set of module keys.
    pub fn resolve_to_module_keys(
        &self,
        insp: &Inspector,
        base_deps: &[(String, ModuleKey)],
        base_unused_deps: &[(String, ModuleKey)],
        include_unused: bool,
        warn_unused: bool,
    ) -> Result<Vec<ModuleKey>, ModError> {
        match self {
            ModuleArg::SpecificVersionOfModule(key) => {
                throw_if_nonexistent(insp, key, include_unused, warn_unused)?;
                Ok(vec![key.clone()])
            }
            ModuleArg::AllVersionsOfModule(name) => resolve_all_versions(insp, name, include_unused, warn_unused),
            ModuleArg::ApparentRepoName(name) => {
                let mut out = vec![];
                if let Some((_, k)) = base_deps.iter().find(|(n, _)| n == name) {
                    push_unique(&mut out, k.clone());
                }
                if include_unused {
                    if let Some((_, k)) = base_unused_deps.iter().find(|(n, _)| n == name) {
                        push_unique(&mut out, k.clone());
                    }
                }
                if out.is_empty() {
                    return Err(invalid(format!(
                        "No module with the apparent repo name @{name} exists in the dependency graph"
                    )));
                }
                Ok(out)
            }
            ModuleArg::CanonicalRepoName(name) => {
                let m = insp.dep_graph.values().find(|m| insp.canonical_name(&m.key) == Some(name.as_str()));
                if let Some(m) = m {
                    if !include_unused && warn_unused && !m.is_used() {
                        return Err(invalid(format!(
                            "Module version {} is unused as a result of module resolution. Use the --include_unused flag to include it.",
                            m.key
                        )));
                    }
                }
                match m {
                    Some(m) if include_unused || m.is_used() => Ok(vec![m.key.clone()]),
                    _ => Err(invalid(format!(
                        "No module with the canonical repo name @@{name} exists in the dependency graph"
                    ))),
                }
            }
        }
    }

    /// Resolves this argument to `(display key, canonical repo name)` pairs.
    pub fn resolve_to_repo_names(
        &self,
        insp: &Inspector,
        mapping: &RepoMapping,
    ) -> Result<Vec<(String, String)>, ModError> {
        match self {
            ModuleArg::SpecificVersionOfModule(key) => {
                throw_if_nonexistent(insp, key, false, false)?;
                Ok(vec![(key.to_string(), insp.canonical_name(key).unwrap_or("").to_string())])
            }
            ModuleArg::AllVersionsOfModule(name) => {
                let keys = resolve_all_versions(insp, name, false, false)?;
                Ok(keys
                    .iter()
                    .map(|k| (k.to_string(), insp.canonical_name(k).unwrap_or("").to_string()))
                    .collect())
            }
            ModuleArg::ApparentRepoName(name) => match mapping.entries.get(name) {
                Some(c) => Ok(vec![(self.to_string(), c.clone())]),
                None => Err(invalid(format!(
                    "No repo visible as {name} from @{}",
                    mapping.context_display()
                ))),
            },
            ModuleArg::CanonicalRepoName(name) => Ok(vec![(self.to_string(), name.clone())]),
        }
    }
}

/// `CommaSeparatedNonEmptyOptionListConverter`.
fn split_non_empty(input: &str) -> Result<Vec<&str>, String> {
    let parts: Vec<&str> = input.split(',').collect();
    if parts.iter().any(|p| p.is_empty()) {
        return Err(format!("Empty values are not allowed as part of this comma-separated list of options"));
    }
    Ok(parts)
}

fn throw_if_nonexistent(
    insp: &Inspector,
    key: &ModuleKey,
    include_unused: bool,
    warn_unused: bool,
) -> Result<(), ModError> {
    let m = insp.dep_graph.get(key);
    if let Some(m) = m {
        if !include_unused && warn_unused && !m.is_used() {
            return Err(invalid(format!(
                "Module version {key} is unused as a result of module resolution. Use the --include_unused flag to include it."
            )));
        }
    }
    if m.is_none() || (!include_unused && !m.map(|m| m.is_used()).unwrap_or(false)) {
        let Some(existing) = insp.modules_index.get(&key.name) else {
            return Err(invalid(format!("Module {} does not exist in the dependency graph.", key.name)));
        };
        let filtered: Vec<&ModuleKey> = existing
            .iter()
            .filter(|k| include_unused || insp.dep_graph.get(*k).map(|m| m.is_used()).unwrap_or(false))
            .collect();
        return Err(invalid(format!(
            "Module version {key} does not exist, available versions: {}.",
            list_string(&filtered)
        )));
    }
    Ok(())
}

fn resolve_all_versions(
    insp: &Inspector,
    name: &str,
    include_unused: bool,
    warn_unused: bool,
) -> Result<Vec<ModuleKey>, ModError> {
    let Some(existing) = insp.modules_index.get(name) else {
        return Err(invalid(format!("Module {name} does not exist in the dependency graph.")));
    };
    let filtered: Vec<ModuleKey> = existing
        .iter()
        .filter(|k| include_unused || insp.dep_graph.get(*k).map(|m| m.is_used()).unwrap_or(false))
        .cloned()
        .collect();
    if filtered.is_empty() {
        if warn_unused {
            return Err(invalid(format!(
                "Module {name} is unused as a result of module resolution. Use the --include_unused flag to include it."
            )));
        }
        return Err(invalid(format!("Module {name} does not exist in the dependency graph.")));
    }
    Ok(filtered)
}

/// A reference to a module extension: `<module><bzl_file_label>%<extension_name>`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionArg {
    pub module_arg: ModuleArg,
    pub repo_relative_bzl_label: String,
    pub extension_name: String,
}

impl fmt::Display for ExtensionArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}%{}", self.module_arg, self.repo_relative_bzl_label, self.extension_name)
    }
}

impl ExtensionArg {
    /// `ExtensionArgConverter.convert`.
    pub fn parse(input: &str) -> Result<ExtensionArg, String> {
        let Some(slash) = input.find('/') else {
            return Err(format!("Invalid argument {input}: missing .bzl label"));
        };
        let percent = match input.find('%') {
            Some(p) if p >= slash => p,
            _ => return Err(format!("Invalid argument {input}: missing extension name")),
        };
        let module_arg = ModuleArg::parse(&input[..slash])?;
        Ok(ExtensionArg {
            module_arg,
            repo_relative_bzl_label: input[slash..percent].to_string(),
            extension_name: input[percent + 1..].to_string(),
        })
    }

    /// `CommaSeparatedExtensionArgListConverter.convert`.
    pub fn parse_list(input: &str) -> Result<Vec<ExtensionArg>, String> {
        let mut out = vec![];
        for part in split_non_empty(input)? {
            out.push(ExtensionArg::parse(part)?);
        }
        Ok(out)
    }

    /// Resolves this argument to a [`ExtensionId`] (non-isolated usage).
    pub fn resolve_to_extension_id(
        &self,
        insp: &Inspector,
        base_deps: &[(String, ModuleKey)],
        base_unused_deps: &[(String, ModuleKey)],
    ) -> Result<ExtensionId, ModError> {
        let ref_modules = self.module_arg.resolve_to_module_keys(insp, base_deps, base_unused_deps, false, false)?;
        if ref_modules.len() != 1 {
            return Err(invalid(format!(
                "Module {}, as part of the extension specifier, should represent exactly one module version. Choose one of: {}.",
                self.module_arg,
                list_string(&ref_modules)
            )));
        }
        let key = &ref_modules[0];
        let repo = insp.canonical_name(key).unwrap_or("");
        let label = parse_repo_relative_label(&self.repo_relative_bzl_label)
            .map_err(|e| invalid(format!("bad label format in {}: {e}", self.repo_relative_bzl_label)))?;
        Ok(ExtensionId {
            bzl_file: format!("@@{repo}{label}"),
            name: self.extension_name.clone(),
            isolation_key: None,
        })
    }
}

/// Parses `//pkg:name` (or `//pkg`) into its normalized `//pkg:name` form.
fn parse_repo_relative_label(label: &str) -> Result<String, String> {
    let Some(rest) = label.strip_prefix("//") else {
        return Err(format!("invalid label '{label}': absolute label must begin with '@' or '//'"));
    };
    let (pkg, name) = match rest.split_once(':') {
        Some((p, n)) => (p, n.to_string()),
        None => (rest, rest.rsplit('/').next().unwrap_or("").to_string()),
    };
    if name.is_empty() {
        return Err(format!("invalid label '{label}': empty target name"));
    }
    Ok(format!("//{pkg}:{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_module_args() {
        assert_eq!(ModuleArg::parse("<ROOT>").unwrap(), ModuleArg::SpecificVersionOfModule(ModuleKey::root()));
        assert_eq!(ModuleArg::parse("foo").unwrap(), ModuleArg::AllVersionsOfModule("foo".into()));
        assert_eq!(ModuleArg::parse("@foo").unwrap(), ModuleArg::ApparentRepoName("foo".into()));
        assert_eq!(ModuleArg::parse("@@foo+").unwrap(), ModuleArg::CanonicalRepoName("foo+".into()));
        assert_eq!(ModuleArg::parse("@@").unwrap(), ModuleArg::CanonicalRepoName("".into()));
        let k = ModuleKey::new("foo", Version::parse("1.2").unwrap());
        assert_eq!(ModuleArg::parse("foo@1.2").unwrap(), ModuleArg::SpecificVersionOfModule(k));
        let e = ModuleKey::new("foo", Version::empty());
        assert_eq!(ModuleArg::parse("foo@_").unwrap(), ModuleArg::SpecificVersionOfModule(e));
        assert_eq!(
            ModuleArg::parse("foo@").unwrap_err(),
            "invalid argument 'foo@': use _ for the empty version"
        );
        assert!(ModuleArg::parse("@a+b").unwrap_err().starts_with("invalid argument '@a+b': invalid user-provided repo name 'a+b'"));
        assert!(ModuleArg::parse("@@a b").unwrap_err().contains("invalid repository name 'a b'"));
        assert!(ModuleArg::parse("foo@1..2").unwrap_err().starts_with("invalid argument 'foo@1..2': "));
    }

    #[test]
    fn module_arg_display_round_trips() {
        for s in ["<root>", "foo", "@foo", "@@foo+", "foo@1.0", "foo@_"] {
            assert_eq!(ModuleArg::parse(s).unwrap().to_string(), s);
        }
    }

    #[test]
    fn parse_lists() {
        let l = ModuleArg::parse_list("a,b@1.0,@c").unwrap();
        assert_eq!(list_string(&l), "[a, b@1.0, @c]");
        assert!(ModuleArg::parse_list("a,,b").is_err());
    }

    #[test]
    fn parse_extension_args() {
        let a = ExtensionArg::parse("@rules_java//java:extensions.bzl%toolchains").unwrap();
        assert_eq!(a.module_arg, ModuleArg::ApparentRepoName("rules_java".into()));
        assert_eq!(a.repo_relative_bzl_label, "//java:extensions.bzl");
        assert_eq!(a.extension_name, "toolchains");
        assert_eq!(a.to_string(), "@rules_java//java:extensions.bzl%toolchains");
        let b = ExtensionArg::parse("rules_java@6.1.1//java:extensions.bzl%toolchains").unwrap();
        assert_eq!(b.to_string(), "rules_java@6.1.1//java:extensions.bzl%toolchains");
        assert_eq!(ExtensionArg::parse("foo").unwrap_err(), "Invalid argument foo: missing .bzl label");
        assert_eq!(ExtensionArg::parse("foo//x.bzl").unwrap_err(), "Invalid argument foo//x.bzl: missing extension name");
        assert_eq!(ExtensionArg::parse("a%b/c").unwrap_err(), "Invalid argument a%b/c: missing extension name");
    }

    #[test]
    fn repo_relative_label() {
        assert_eq!(parse_repo_relative_label("//a/b:c.bzl").unwrap(), "//a/b:c.bzl");
        assert_eq!(parse_repo_relative_label("//a/b").unwrap(), "//a/b:b");
        assert!(parse_repo_relative_label("/a").is_err());
    }
}
