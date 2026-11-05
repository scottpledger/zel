//! Core Bzlmod data model: module keys, attribute values, repo specs, overrides, extension usages.
//! Mirrors `ModuleKey`, `InterimModule`, `RepoSpec`, `ModuleOverride`, `ModuleExtensionUsage`.

use crate::json::Json;
use crate::version::Version;
use std::collections::BTreeMap;
use std::fmt;
use zel_starlark::Value;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModuleKey {
    pub name: String,
    pub version: Version,
}

impl ModuleKey {
    pub fn new(name: impl Into<String>, version: Version) -> Self {
        Self { name: name.into(), version }
    }
    pub fn root() -> Self {
        Self { name: String::new(), version: Version::empty() }
    }
    pub fn is_root(&self) -> bool {
        self.name.is_empty() && self.version.is_empty()
    }
    pub fn display_string(&self) -> String {
        if self.is_root() {
            "root module".into()
        } else {
            format!("module '{self}'")
        }
    }
    /// `getCanonicalRepoName(includeVersion)`; well-known modules keep their bare names.
    pub fn canonical_repo_name(&self, include_version: bool) -> String {
        match self.name.as_str() {
            "bazel_tools" => return "bazel_tools".into(),
            "platforms" => return "platforms".into(),
            _ => {}
        }
        if self.is_root() {
            return String::new();
        }
        if include_version {
            format!("{}+{}", self.name, self.version)
        } else {
            format!("{}+", self.name)
        }
    }
    pub fn parse(s: &str) -> Result<Self, String> {
        if s == "<root>" {
            return Ok(Self::root());
        }
        let (n, v) = s.split_once('@').ok_or_else(|| format!("invalid module key '{s}'"))?;
        let version = if v == "_" { Version::empty() } else { Version::parse(v)? };
        Ok(Self::new(n, version))
    }
}

impl fmt::Display for ModuleKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_root() {
            return f.write_str("<root>");
        }
        write!(
            f,
            "{}@{}",
            self.name,
            if self.version.is_empty() { "_".to_string() } else { self.version.to_string() }
        )
    }
}

const STRING_ESCAPE_SEQUENCE: &str = "'";

/// `AttributeValuesAdapter.serializeObjToString` for a `String`.
fn escape_attr_string(s: &str) -> String {
    if s.starts_with("@@") || (s.starts_with(STRING_ESCAPE_SEQUENCE) && s.ends_with(STRING_ESCAPE_SEQUENCE)) {
        format!("{STRING_ESCAPE_SEQUENCE}{s}{STRING_ESCAPE_SEQUENCE}")
    } else {
        s.to_string()
    }
}

/// `AttributeValuesAdapter.deserializeStringToObject` for a non-label string.
fn unescape_attr_string(s: &str) -> String {
    if s.len() >= 2 && s.starts_with(STRING_ESCAPE_SEQUENCE) && s.ends_with(STRING_ESCAPE_SEQUENCE) {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Attribute value attached to repo specs and extension tags (JSON-like, insertion-ordered dicts).
#[derive(Debug, Clone, PartialEq)]
pub enum AttrVal {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    /// A `Label` value (kept apart from strings so the lockfile can escape label-like strings).
    Label(String),
    List(Vec<AttrVal>),
    Dict(Vec<(String, AttrVal)>),
}

impl AttrVal {
    /// Starlark type name of the value (`Starlark.type`).
    pub fn type_name(&self) -> &'static str {
        match self {
            AttrVal::None => "NoneType",
            AttrVal::Bool(_) => "bool",
            AttrVal::Int(_) => "int",
            AttrVal::Float(_) => "float",
            AttrVal::Str(_) => "string",
            AttrVal::Label(_) => "Label",
            AttrVal::List(_) => "list",
            AttrVal::Dict(_) => "dict",
        }
    }

    pub fn from_value(v: &Value) -> AttrVal {
        match v {
            Value::None => AttrVal::None,
            Value::Bool(b) => AttrVal::Bool(*b),
            Value::Int(i) => i.to_i64().map(AttrVal::Int).unwrap_or_else(|| AttrVal::Str(i.to_string())),
            Value::Float(f) => AttrVal::Float(*f),
            Value::String(s) => AttrVal::Str(s.clone()),
            Value::Label(l) => AttrVal::Label(l.canonical_form()),
            Value::List(l) => AttrVal::List(l.lock().unwrap().elements.iter().map(Self::from_value).collect()),
            Value::Tuple(t) => AttrVal::List(t.iter().map(Self::from_value).collect()),
            Value::Dict(d) => AttrVal::Dict(
                d.lock()
                    .unwrap()
                    .entries
                    .iter()
                    .map(|(k, v)| (k.to_string(), Self::from_value(v)))
                    .collect(),
            ),
            other => AttrVal::Str(other.to_string()),
        }
    }

    pub fn to_value(&self) -> Value {
        use std::sync::{Arc, Mutex};
        use zel_starlark::{BigInt, StarlarkDict, StarlarkList};
        match self {
            AttrVal::None => Value::None,
            AttrVal::Bool(b) => Value::Bool(*b),
            AttrVal::Int(i) => Value::Int(BigInt::from_i64(*i)),
            AttrVal::Float(f) => Value::Float(*f),
            AttrVal::Str(s) => Value::String(s.clone()),
            AttrVal::Label(s) => Value::String(s.clone()),
            AttrVal::List(l) => Value::List(Arc::new(Mutex::new(StarlarkList::new(
                l.iter().map(|x| x.to_value()).collect(),
            )))),
            AttrVal::Dict(d) => {
                let mut dict = StarlarkDict::new();
                for (k, v) in d {
                    let _ = dict.insert(Value::String(k.clone()), v.to_value());
                }
                Value::Dict(Arc::new(Mutex::new(dict)))
            }
        }
    }

    pub fn to_json(&self) -> Json {
        match self {
            AttrVal::None => Json::Null,
            AttrVal::Bool(b) => Json::Bool(*b),
            AttrVal::Int(i) => Json::int(*i),
            AttrVal::Float(f) => Json::Num(format!("{f:?}")),
            // `AttributeValuesAdapter.serializeObjToString`: labels are written as is, strings that look like a
            // label (`@@...`) or like an escaped string (`'...'`) are wrapped in one more layer of `'`.
            AttrVal::Label(s) => Json::Str(s.clone()),
            AttrVal::Str(s) => Json::Str(escape_attr_string(s)),
            AttrVal::List(l) => Json::Arr(l.iter().map(|x| x.to_json()).collect()),
            AttrVal::Dict(d) => Json::Obj(d.iter().map(|(k, v)| (escape_attr_string(k), v.to_json())).collect()),
        }
    }

    pub fn from_json(j: &Json) -> AttrVal {
        match j {
            Json::Null => AttrVal::None,
            Json::Bool(b) => AttrVal::Bool(*b),
            Json::Num(n) => n
                .parse::<i64>()
                .map(AttrVal::Int)
                .unwrap_or_else(|_| AttrVal::Float(n.parse().unwrap_or(0.0))),
            Json::Str(s) if s.starts_with("@@") => AttrVal::Label(s.clone()),
            Json::Str(s) => AttrVal::Str(unescape_attr_string(s)),
            Json::Arr(a) => AttrVal::List(a.iter().map(Self::from_json).collect()),
            Json::Obj(o) => AttrVal::Dict(o.iter().map(|(k, v)| (unescape_attr_string(k), Self::from_json(v))).collect()),
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            AttrVal::Str(s) | AttrVal::Label(s) => Some(s),
            _ => None,
        }
    }

    pub fn str_list(&self) -> Vec<String> {
        match self {
            AttrVal::List(l) => l.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
            AttrVal::Str(s) | AttrVal::Label(s) => vec![s.clone()],
            _ => vec![],
        }
    }

    /// Starlark `repr`-style rendering used by `mod show_repo` / `show_extension`.
    pub fn repr(&self) -> String {
        match self {
            AttrVal::None => "None".into(),
            AttrVal::Bool(true) => "True".into(),
            AttrVal::Bool(false) => "False".into(),
            AttrVal::Int(i) => i.to_string(),
            AttrVal::Float(f) => format!("{f:?}"),
            AttrVal::Label(s) => AttrVal::Str(s.clone()).repr(),
            AttrVal::Str(s) => {
                let mut o = String::from("\"");
                for c in s.chars() {
                    match c {
                        '"' => o.push_str("\\\""),
                        '\\' => o.push_str("\\\\"),
                        '\n' => o.push_str("\\n"),
                        '\t' => o.push_str("\\t"),
                        '\r' => o.push_str("\\r"),
                        c => o.push(c),
                    }
                }
                o.push('"');
                o
            }
            AttrVal::List(l) => format!("[{}]", l.iter().map(|x| x.repr()).collect::<Vec<_>>().join(", ")),
            AttrVal::Dict(d) => format!(
                "{{{}}}",
                d.iter()
                    .map(|(k, v)| format!("{}: {}", AttrVal::Str(k.clone()).repr(), v.repr()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

/// `RepoSpec`: a repository rule (identified by bzl file label + name) and its attributes.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoSpec {
    /// e.g. `@bazel_tools//tools/build_defs/repo:http.bzl`
    pub bzl_file: String,
    /// e.g. `http_archive`
    pub rule_name: String,
    pub attrs: Vec<(String, AttrVal)>,
}

impl RepoSpec {
    pub fn new(bzl_file: &str, rule_name: &str, attrs: Vec<(String, AttrVal)>) -> Self {
        Self { bzl_file: bzl_file.into(), rule_name: rule_name.into(), attrs }
    }
    pub fn attr(&self, k: &str) -> Option<&AttrVal> {
        self.attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v)
    }
    pub fn set_attr(&mut self, k: &str, v: AttrVal) {
        if let Some(e) = self.attrs.iter_mut().find(|(n, _)| n == k) {
            e.1 = v;
        } else {
            self.attrs.push((k.to_string(), v));
        }
    }
    /// Lockfile `repoRuleId`: `@@<repo>//<pkg>:<file>%<name>`.
    pub fn rule_id(&self) -> String {
        let bzl = if self.bzl_file.starts_with("@@") {
            self.bzl_file.clone()
        } else if let Some(r) = self.bzl_file.strip_prefix('@') {
            format!("@@{r}")
        } else {
            self.bzl_file.clone()
        };
        format!("{bzl}%{}", self.rule_name)
    }
    pub fn to_json(&self) -> Json {
        Json::Obj(vec![
            ("repoRuleId".into(), Json::Str(self.rule_id())),
            (
                "attributes".into(),
                Json::Obj(self.attrs.iter().map(|(k, v)| (k.clone(), v.to_json())).collect()),
            ),
        ])
    }
    pub fn from_json(j: &Json) -> Option<RepoSpec> {
        let id = j.get("repoRuleId")?.as_str()?;
        let (bzl, name) = id.rsplit_once('%')?;
        let attrs = match j.get("attributes")? {
            Json::Obj(o) => o.iter().map(|(k, v)| (k.clone(), AttrVal::from_json(v))).collect(),
            _ => vec![],
        };
        let bzl = bzl.strip_prefix("@@bazel_tools//").map(|r| format!("@bazel_tools//{r}")).unwrap_or_else(|| bzl.to_string());
        Some(RepoSpec { bzl_file: bzl, rule_name: name.to_string(), attrs })
    }
}

pub const HTTP_BZL: &str = "@bazel_tools//tools/build_defs/repo:http.bzl";
pub const GIT_BZL: &str = "@bazel_tools//tools/build_defs/repo:git.bzl";
pub const LOCAL_BZL: &str = "@bazel_tools//tools/build_defs/repo:local.bzl";

#[derive(Debug, Clone, PartialEq)]
pub enum ModuleOverride {
    SingleVersion {
        version: Version,
        registry: String,
        patches: Vec<String>,
        patch_cmds: Vec<String>,
        patch_strip: i64,
    },
    MultipleVersion {
        versions: Vec<Version>,
        registry: String,
    },
    /// archive_override / git_override / local_path_override / built-in modules.
    NonRegistry(RepoSpec),
}

impl std::fmt::Display for ModuleOverride {
    /// Java record `toString()` forms (`SingleVersionOverride[...]`, ...), used in error messages.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = |p: &String| p.strip_prefix("@@").filter(|r| r.starts_with("//")).unwrap_or(p).to_string();
        match self {
            ModuleOverride::SingleVersion { version, registry, patches, patch_cmds, patch_strip } => write!(
                f,
                "SingleVersionOverride[version={version}, registry={registry}, patches=[{}], patchCmds=[{}], patchStrip={patch_strip}]",
                patches.iter().map(label).collect::<Vec<_>>().join(", "),
                patch_cmds.join(", ")
            ),
            ModuleOverride::MultipleVersion { versions, registry } => write!(
                f,
                "MultipleVersionOverride[versions=[{}], registry={registry}]",
                versions.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ")
            ),
            ModuleOverride::NonRegistry(spec) => {
                let attrs: Vec<(String, AttrVal)> = spec
                    .attrs
                    .iter()
                    .map(|(k, v)| match (k.as_str(), v) {
                        ("patches", AttrVal::List(l)) => (
                            k.clone(),
                            AttrVal::List(l.iter().map(|x| match x { AttrVal::Str(s) => AttrVal::Str(label(s)), o => o.clone() }).collect()),
                        ),
                        _ => (k.clone(), v.clone()),
                    })
                    .collect();
                let bzl = spec.bzl_file.strip_prefix('@').map(|r| format!("@@{r}")).unwrap_or_else(|| spec.bzl_file.clone());
                write!(
                    f,
                    "NonRegistryOverride[repoSpec=RepoSpec[repoRuleId={bzl}%{}, attributes=AttributeValues{{attributes={}}}]]",
                    spec.rule_name,
                    AttrVal::Dict(attrs).repr()
                )
            }
        }
    }
}

impl ModuleOverride {
    pub fn is_non_registry(&self) -> bool {
        matches!(self, ModuleOverride::NonRegistry(_))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tag {
    pub tag_name: String,
    pub attrs: Vec<(String, AttrVal)>,
    pub dev_dependency: bool,
    pub location: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Proxy {
    pub proxy_name: String,
    pub dev_dependency: bool,
    pub location: String,
    /// (local repo name, exported repo name)
    pub imports: Vec<(String, String)>,
    pub containing_module_file: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RepoOverrideSpec {
    pub overriding_repo_name: String,
    pub must_exist: bool,
    pub location: String,
}

/// `ModuleExtensionUsage`: all uses of one extension within one module.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionUsage {
    /// Normalized label, e.g. `@rules_python//python/extensions:python.bzl` or `//:MODULE.bazel`.
    pub bzl_file: String,
    pub extension_name: String,
    /// Isolation key: (module key, proxy name).
    pub isolation_key: Option<(ModuleKey, String)>,
    pub proxies: Vec<Proxy>,
    pub tags: Vec<Tag>,
    pub repo_overrides: BTreeMap<String, RepoOverrideSpec>,
}

impl ExtensionUsage {
    pub fn is_innate(&self) -> bool {
        self.bzl_file == "//:MODULE.bazel"
    }
    /// All imported (local -> exported) names in declaration order.
    pub fn imports(&self) -> Vec<(String, String)> {
        self.proxies.iter().flat_map(|p| p.imports.clone()).collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InterimModule {
    pub key: ModuleKey,
    pub name: String,
    pub version: Version,
    pub repo_name: String,
    pub bazel_compatibility: Vec<String>,
    pub toolchains_to_register: Vec<String>,
    pub execution_platforms_to_register: Vec<String>,
    pub deps: Vec<(String, ModuleKey)>,
    pub original_deps: Vec<(String, ModuleKey)>,
    pub nodep_deps: Vec<ModuleKey>,
    pub registry_url: Option<String>,
    pub extension_usages: Vec<ExtensionUsage>,
    pub flag_aliases: Vec<(String, String)>,
}

impl InterimModule {
    pub fn with_deps_transformed(&self, f: &dyn Fn(&ModuleKey) -> ModuleKey) -> InterimModule {
        let mut m = self.clone();
        m.deps = self.deps.iter().map(|(n, k)| (n.clone(), f(k))).collect();
        m.nodep_deps = self.nodep_deps.iter().map(f).collect();
        m
    }
    pub fn dep(&self, repo_name: &str) -> Option<&ModuleKey> {
        self.deps.iter().find(|(n, _)| n == repo_name).map(|(_, k)| k)
    }
}

/// Final module in the resolved graph (`Module`).
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub interim: InterimModule,
    pub repo_spec: Option<RepoSpec>,
}

impl std::ops::Deref for Module {
    type Target = InterimModule;
    fn deref(&self) -> &InterimModule {
        &self.interim
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lockfile_attr_strings_are_escaped_like_gson_adapter() {
        // A string that looks like a label is wrapped in `'`; a Label is written as is.
        assert_eq!(AttrVal::Str("@@//:x".into()).to_json(), Json::Str("'@@//:x'".into()));
        assert_eq!(AttrVal::Label("@@//:x".into()).to_json(), Json::Str("@@//:x".into()));
        assert_eq!(AttrVal::Str("'q'".into()).to_json(), Json::Str("''q''".into()));
        assert_eq!(AttrVal::Str("plain".into()).to_json(), Json::Str("plain".into()));
        // Round trip, including dict keys.
        for v in [
            AttrVal::Str("@@//:x".into()),
            AttrVal::Str("'q'".into()),
            AttrVal::Label("@@r+//p:t".into()),
            AttrVal::Dict(vec![("@@k".into(), AttrVal::Str("@@v".into()))]),
        ] {
            assert_eq!(AttrVal::from_json(&v.to_json()), v);
        }
    }
}
