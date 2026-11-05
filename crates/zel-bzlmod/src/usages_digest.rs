//! `usagesDigest` of a module extension: a port of `SingleExtensionUsagesValue.hashForEvaluation`.
//!
//! Bazel serializes the usages value trimmed to what influences the extension's evaluation
//! (`trimForEvaluation`) with Gson (`GsonTypeAdapterUtil.SINGLE_EXTENSION_USAGES_VALUE_GSON`:
//! compact output, no HTML escaping, declaration-order fields, absent `Optional`s omitted) and
//! hashes the JSON text as UTF-16 code units (`Hashing.sha256().hashUnencodedChars`).
//!
//! The JSON layout below follows the Bazel 9.3.2 sources:
//!
//! ```text
//! {"extensionUsages":{<module key>:{"extensionBzlFile":..,"extensionName":..,
//!   ["isolationKey":..,]"proxies":[],"tags":[..],"repoOverrides":{}}},
//!  "extensionUniqueName":..,"abridgedModules":[{"name","version","key"}],
//!  "repoMappings":{},"repoOverrides":{<local name>:<canonical repo name>}}
//! ```
//!
//! `ModuleExtensionUsage.trimForEvaluation` clears all proxies (`"proxies":[]`).

use crate::model::{AttrVal, ExtensionUsage, ModuleKey};
use crate::hash::sha256;

/// One module that uses the extension (`AbridgedModule`).
pub struct AbridgedModule {
    pub name: String,
    pub version: String,
    pub key: String,
}

pub struct UsagesDigestInput<'a> {
    pub usages: Vec<(&'a ModuleKey, &'a ExtensionUsage)>,
    pub unique_name: &'a str,
    /// Repo name of the root module (`module(repo_name=...)` or its name).
    pub root_repo_name: &'a str,
    pub abridged_modules: Vec<AbridgedModule>,
    /// Extension-local repo name -> canonical name of the root repo overriding it.
    pub repo_overrides: Vec<(String, String)>,
}

/// Gson string escaping with HTML escaping disabled.
fn json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `AttributeValuesAdapter.serializeObjToString`: strings that look like labels (`@@...`) or are
/// already quote-wrapped are escaped by wrapping them in `'`.
fn attr_string(s: &str) -> String {
    if s.starts_with("@@") || (s.starts_with('\'') && s.ends_with('\'')) {
        format!("'{s}'")
    } else {
        s.to_string()
    }
}

fn json_attr(out: &mut String, v: &AttrVal) {
    match v {
        AttrVal::None => out.push_str("null"),
        AttrVal::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        AttrVal::Int(i) => out.push_str(&i.to_string()),
        AttrVal::Float(f) => out.push_str(&f.to_string()),
        AttrVal::Str(s) | AttrVal::Label(s) => json_str(out, &attr_string(s)),
        AttrVal::List(l) => {
            out.push('[');
            for (i, x) in l.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_attr(out, x);
            }
            out.push(']');
        }
        AttrVal::Dict(d) => {
            out.push('{');
            for (i, (k, x)) in d.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_str(out, &attr_string(k));
                out.push(':');
                json_attr(out, x);
            }
            out.push('}');
        }
    }
}

const BUILTIN_LOCATION: &str = "{\"file\":\"<builtin>\",\"line\":0,\"column\":0}";

/// The JSON text hashed for the usages digest (exposed for tests).
pub fn usages_json(input: &UsagesDigestInput<'_>) -> String {
    let mut out = String::from("{\"extensionUsages\":{");
    for (i, (mk, u)) in input.usages.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        json_str(&mut out, &mk.to_string());
        out.push_str(":{\"extensionBzlFile\":");
        // `normalizeLabelString`: a main-repo reference is translated to the module's repo name.
        let bzl = match u.bzl_file.strip_prefix("//") {
            Some(rest) => format!("@{}//{rest}", input.root_repo_name_for(mk)),
            None => u.bzl_file.clone(),
        };
        json_str(&mut out, &bzl);
        out.push_str(",\"extensionName\":");
        json_str(&mut out, &u.extension_name);
        if let Some((k, name)) = &u.isolation_key {
            out.push_str(",\"isolationKey\":");
            json_str(&mut out, &format!("{k}+{name}"));
        }
        // `trimForEvaluation` clears all proxies.
        out.push_str(",\"proxies\":[");
        out.push_str("],\"tags\":[");
        for (j, t) in u.tags.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push_str("{\"tagName\":");
            json_str(&mut out, &t.tag_name);
            out.push_str(",\"attributeValues\":");
            json_attr(&mut out, &AttrVal::Dict(t.attrs.clone()));
            out.push_str(&format!(",\"devDependency\":{},\"location\":{BUILTIN_LOCATION}}}", t.dev_dependency));
        }
        out.push_str("],\"repoOverrides\":{}}");
    }
    out.push_str("},\"extensionUniqueName\":");
    json_str(&mut out, input.unique_name);
    out.push_str(",\"abridgedModules\":[");
    for (i, m) in input.abridged_modules.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"name\":");
        json_str(&mut out, &m.name);
        out.push_str(",\"version\":");
        json_str(&mut out, &m.version);
        out.push_str(",\"key\":");
        json_str(&mut out, &m.key);
        out.push('}');
    }
    out.push_str("],\"repoMappings\":{},\"repoOverrides\":{");
    for (i, (k, v)) in input.repo_overrides.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        json_str(&mut out, k);
        out.push(':');
        json_str(&mut out, v);
    }
    out.push_str("}}");
    out
}

impl UsagesDigestInput<'_> {
    fn root_repo_name_for(&self, mk: &ModuleKey) -> String {
        if mk.is_root() {
            self.root_repo_name.to_string()
        } else {
            // Non-root usages are normalized by `ModuleFileFunction` with their own repo name
            // already (`@own//...`), so a bare `//` label cannot occur here.
            String::new()
        }
    }
}

/// `Hashing.sha256().hashUnencodedChars(json)`: SHA-256 over the UTF-16LE code units.
pub fn hash_unencoded_chars(json: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(json.len() * 2);
    for u in json.encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    sha256(&bytes).to_vec()
}

pub fn usages_digest(input: &UsagesDigestInput<'_>) -> Vec<u8> {
    hash_unencoded_chars(&usages_json(input))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::base64_encode;
    use crate::model::{Tag};

    fn usage(bzl: &str, tags: Vec<Tag>) -> ExtensionUsage {
        ExtensionUsage {
            bzl_file: bzl.to_string(),
            extension_name: "ex".to_string(),
            isolation_key: None,
            proxies: vec![],
            tags,
            repo_overrides: Default::default(),
        }
    }

    /// Verified against `SingleExtensionUsagesValue.hashForEvaluation` of Bazel 9.3.2 (and the
    /// `MODULE.bazel.lock` it wrote for the same module): `usagesDigest` of a root module using
    /// `//:ext.bzl%ex` with `ext.mk(name = "a")`.
    #[test]
    fn matches_bazel_9_3_2() {
        let root = ModuleKey::root();
        let u = usage(
            "//:ext.bzl",
            vec![Tag { tag_name: "mk".into(), attrs: vec![("name".into(), AttrVal::Str("a".into()))], dev_dependency: false, location: "x:1:1".into() }],
        );
        let input = UsagesDigestInput {
            usages: vec![(&root, &u)],
            unique_name: "+ex",
            root_repo_name: "root",
            abridged_modules: vec![AbridgedModule { name: "root".into(), version: "1.0".into(), key: "<root>".into() }],
            repo_overrides: vec![],
        };
        let json = usages_json(&input);
        assert_eq!(
            json,
            "{\"extensionUsages\":{\"<root>\":{\"extensionBzlFile\":\"@root//:ext.bzl\",\"extensionName\":\"ex\",\"proxies\":[],\"tags\":[{\"tagName\":\"mk\",\"attributeValues\":{\"name\":\"a\"},\"devDependency\":false,\"location\":{\"file\":\"<builtin>\",\"line\":0,\"column\":0}}],\"repoOverrides\":{}}},\"extensionUniqueName\":\"+ex\",\"abridgedModules\":[{\"name\":\"root\",\"version\":\"1.0\",\"key\":\"<root>\"}],\"repoMappings\":{},\"repoOverrides\":{}}"
        );
        assert_eq!(base64_encode(&hash_unencoded_chars(&json)), "WXv8CruOzDCxfYYi3/901rT/qPeXmrpy0zjMxjtOINg=");
    }

    #[test]
    fn proxies_are_always_trimmed_and_repo_overrides_are_serialized() {
        use crate::model::Proxy;
        let root = ModuleKey::root();
        let proxy = |dev: bool| Proxy {
            proxy_name: "ext".into(),
            dev_dependency: dev,
            location: "MODULE.bazel:1:1".into(),
            imports: vec![("a".into(), "a".into())],
            containing_module_file: "MODULE.bazel".into(),
        };
        let mut u = usage("//:ext.bzl", vec![]);
        u.proxies = vec![proxy(true), proxy(false), proxy(true)];
        let input = UsagesDigestInput {
            usages: vec![(&root, &u)],
            unique_name: "+ex",
            root_repo_name: "root",
            abridged_modules: vec![],
            repo_overrides: vec![("x".into(), "mx+".into())],
        };
        let json = usages_json(&input);
        assert!(json.contains("\"proxies\":[]"), "{json}");
        assert!(json.ends_with("\"repoMappings\":{},\"repoOverrides\":{\"x\":\"mx+\"}}"), "{json}");
    }

    #[test]
    fn escapes_like_gson() {
        let mut s = String::new();
        json_str(&mut s, "a\"b\\c\n<>&'\u{1}é");
        assert_eq!(s, "\"a\\\"b\\\\c\\n<>&'\\u0001é\"");
        assert_eq!(attr_string("@@x//:y"), "'@@x//:y'");
        assert_eq!(attr_string("'q'"), "''q''");
        assert_eq!(attr_string("plain"), "plain");
    }
}
