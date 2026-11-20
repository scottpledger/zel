//! `MODULE.bazel.lock` (version 28, Bazel 9.3.2): reading, validation, and writing.
//! Mirrors `BazelLockFileValue`, `BazelLockFileFunction`, `BazelLockFileModule`, `GsonTypeAdapterUtil`.

use crate::hash::{base64_decode, base64_encode, sha256};
use crate::json::Json;
use crate::model::*;
use crate::registry::FileHashes;
use crate::resolve::ExtensionId;
use crate::rt::{ExtensionMetadata, ExtensionResult};
use std::collections::BTreeMap;
use std::path::Path;

/// `BazelLockFileValue.LOCK_FILE_VERSION` of Bazel 9.3.2; only lockfiles of exactly this version are read.
pub const LOCK_FILE_VERSION: i64 = 28;
pub const NOT_FOUND_MARKER: &str = "not found";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockfileMode {
    Off,
    Update,
    Refresh,
    Error,
}

impl LockfileMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "update" => Some(Self::Update),
            "refresh" => Some(Self::Refresh),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LockExt {
    pub bzl_transitive_digest: Vec<u8>,
    pub usages_digest: Vec<u8>,
    /// `<input> <value>` strings, e.g. `ENV:FOO bar`.
    pub recorded_inputs: Vec<String>,
    pub generated_repo_specs: Vec<(String, RepoSpec)>,
    pub metadata: Option<LockMetadata>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LockMetadata {
    pub explicit_root_direct_deps: Option<Vec<String>>,
    pub explicit_root_direct_dev_deps: Option<Vec<String>>,
    /// `NO`, `REGULAR` or `DEV`.
    pub use_all_repos: String,
    pub reproducible: bool,
}

impl LockExt {
    pub fn reproducible(&self) -> bool {
        self.metadata.as_ref().is_some_and(|m| m.reproducible)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Lockfile {
    pub version: i64,
    pub registry_file_hashes: FileHashes,
    pub selected_yanked_versions: BTreeMap<ModuleKey, String>,
    /// extension id string -> factors string -> entry
    pub module_extensions: BTreeMap<String, BTreeMap<String, LockExt>>,
    pub facts: BTreeMap<String, Json>,
    pub facts_versions: BTreeMap<String, i64>,
}

impl Default for Lockfile {
    fn default() -> Self {
        Self {
            version: LOCK_FILE_VERSION,
            registry_file_hashes: FileHashes::new(),
            selected_yanked_versions: BTreeMap::new(),
            module_extensions: BTreeMap::new(),
            facts: BTreeMap::new(),
            facts_versions: BTreeMap::new(),
        }
    }
}

/// `ModuleExtensionId.toString()`: the unambiguous form used in messages.
pub fn extension_display(id: &ExtensionId) -> String {
    match &id.isolation_key {
        Some((m, u)) => format!("{}%{}%{}+{}", id.bzl_file, id.name, m, u),
        None => format!("{}%{}", id.bzl_file, id.name),
    }
}

pub fn extension_key(id: &ExtensionId) -> String {
    // `Label.toString()` renders the main repository as the empty string (`//:ext.bzl`) and
    // other repositories as `@@canonical//...`.
    let bzl = id
        .bzl_file
        .strip_prefix("@@")
        .filter(|r| r.starts_with("//"))
        .unwrap_or(&id.bzl_file);
    match &id.isolation_key {
        Some((m, u)) => format!("{}%{}%{}+{}", bzl, id.name, m, u),
        None => format!("{}%{}", bzl, id.name),
    }
}

pub fn digest_b64(parts: &[&str]) -> Vec<u8> {
    let mut data = Vec::new();
    for p in parts {
        data.extend_from_slice(&(p.len() as u64).to_be_bytes());
        data.extend_from_slice(p.as_bytes());
    }
    sha256(&data).to_vec()
}

impl LockExt {
    fn to_json(&self) -> Json {
        let mut o = vec![
            (
                "bzlTransitiveDigest".to_string(),
                Json::str(base64_encode(&self.bzl_transitive_digest)),
            ),
            (
                "usagesDigest".to_string(),
                Json::str(base64_encode(&self.usages_digest)),
            ),
            (
                "recordedInputs".to_string(),
                Json::Arr(
                    self.recorded_inputs
                        .iter()
                        .map(|s| Json::str(s.clone()))
                        .collect(),
                ),
            ),
            (
                "generatedRepoSpecs".to_string(),
                Json::Obj(
                    self.generated_repo_specs
                        .iter()
                        .map(|(n, s)| {
                            // `RepoRule.instantiate` stores only explicitly specified attributes and
                            // never the reserved `name` attribute.
                            let mut s = s.clone();
                            s.attrs
                                .retain(|(k, v)| k != "name" && !matches!(v, AttrVal::None));
                            (n.clone(), s.to_json())
                        })
                        .collect(),
                ),
            ),
        ];
        if let Some(m) = &self.metadata {
            let mut mo = vec![];
            if let Some(d) = m.explicit_root_direct_deps.as_ref() {
                mo.push((
                    "explicitRootModuleDirectDeps".to_string(),
                    Json::Arr(d.iter().map(|x| Json::str(x.clone())).collect()),
                ));
            }
            if let Some(d) = m.explicit_root_direct_dev_deps.as_ref() {
                mo.push((
                    "explicitRootModuleDirectDevDeps".to_string(),
                    Json::Arr(d.iter().map(|x| Json::str(x.clone())).collect()),
                ));
            }
            mo.push((
                "useAllRepos".to_string(),
                Json::str(m.use_all_repos.clone()),
            ));
            mo.push(("reproducible".to_string(), Json::Bool(m.reproducible)));
            o.push(("moduleExtensionMetadata".to_string(), Json::Obj(mo)));
        }
        Json::Obj(o)
    }

    fn from_json(j: &Json) -> Result<LockExt, String> {
        let b64 = |k: &str| -> Result<Vec<u8>, String> {
            base64_decode(
                j.get(k)
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| format!("missing {k}"))?,
            )
        };
        let mut specs = vec![];
        match j.get("generatedRepoSpecs") {
            Some(Json::Obj(o)) => {
                for (n, s) in o {
                    let mut spec = RepoSpec::from_json(s).ok_or("invalid repoSpec in lockfile")?;
                    if !spec.attrs.iter().any(|(k, _)| k == "name") {
                        spec.attrs
                            .insert(0, ("name".to_string(), AttrVal::Str(n.clone())));
                    }
                    specs.push((n.clone(), spec));
                }
            }
            _ => return Err("missing generatedRepoSpecs".into()),
        }
        let metadata = j.get("moduleExtensionMetadata").map(|m| {
            let list = |k: &str| {
                m.get(k).and_then(|v| v.as_arr()).map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
            };
            LockMetadata {
                explicit_root_direct_deps: list("explicitRootModuleDirectDeps"),
                explicit_root_direct_dev_deps: list("explicitRootModuleDirectDevDeps"),
                use_all_repos: m
                    .get("useAllRepos")
                    .and_then(|v| v.as_str())
                    .unwrap_or("NO")
                    .to_string(),
                reproducible: m
                    .get("reproducible")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            }
        });
        Ok(LockExt {
            bzl_transitive_digest: b64("bzlTransitiveDigest")?,
            usages_digest: b64("usagesDigest")?,
            recorded_inputs: j
                .get("recordedInputs")
                .and_then(|v| v.as_arr())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            generated_repo_specs: specs,
            metadata,
        })
    }
}

impl Lockfile {
    pub fn to_json(&self) -> Json {
        let hashes: Vec<(String, Json)> = self
            .registry_file_hashes
            .iter()
            .map(|(u, h)| {
                (
                    u.clone(),
                    Json::str(match h {
                        Some(h) => h.to_string(),
                        None => NOT_FOUND_MARKER.to_string(),
                    }),
                )
            })
            .collect();
        let yanked: Vec<(String, Json)> = self
            .selected_yanked_versions
            .iter()
            .map(|(k, v)| (k.to_string(), Json::str(v.clone())))
            .collect();
        let exts: Vec<(String, Json)> = self
            .module_extensions
            .iter()
            .map(|(id, f)| {
                (
                    id.clone(),
                    Json::Obj(f.iter().map(|(fk, e)| (fk.clone(), e.to_json())).collect()),
                )
            })
            .collect();
        let facts: Vec<(String, Json)> = self
            .facts
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let facts_versions: Vec<(String, Json)> = self
            .facts_versions
            .iter()
            .map(|(k, v)| (k.clone(), Json::int(*v)))
            .collect();
        Json::Obj(vec![
            ("lockFileVersion".into(), Json::int(self.version)),
            ("registryFileHashes".into(), Json::Obj(hashes)),
            ("selectedYankedVersions".into(), Json::Obj(yanked)),
            ("moduleExtensions".into(), Json::Obj(exts)),
            ("facts".into(), Json::Obj(facts)),
            ("factsVersions".into(), Json::Obj(facts_versions)),
        ])
    }

    pub fn to_text(&self) -> String {
        let mut s = self.to_json().to_pretty(2, false);
        s.push('\n');
        s
    }

    pub fn parse(text: &str) -> Result<Lockfile, String> {
        let j = Json::parse(text)?;
        let version = j
            .get("lockFileVersion")
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);
        if version != LOCK_FILE_VERSION {
            return Err(format!("unsupported lockfile version {version}"));
        }
        let mut lf = Lockfile::default();
        if let Some(Json::Obj(o)) = j.get("registryFileHashes") {
            for (u, h) in o {
                let h = h.as_str().unwrap_or("");
                if h == NOT_FOUND_MARKER {
                    lf.registry_file_hashes.insert(u.clone(), None);
                } else {
                    let c = crate::hash::Checksum::parse_integrity(h)
                        .or_else(|_| crate::hash::Checksum::sha256_hex(h))
                        .map_err(|e| format!("Invalid checksum: {h} ({e})"))?;
                    lf.registry_file_hashes.insert(u.clone(), Some(c.hex()));
                }
            }
        }
        if let Some(Json::Obj(o)) = j.get("selectedYankedVersions") {
            for (k, v) in o {
                lf.selected_yanked_versions
                    .insert(ModuleKey::parse(k)?, v.as_str().unwrap_or("").to_string());
            }
        }
        if let Some(Json::Obj(o)) = j.get("moduleExtensions") {
            for (id, f) in o {
                let mut m = BTreeMap::new();
                if let Json::Obj(fo) = f {
                    for (fk, e) in fo {
                        m.insert(fk.clone(), LockExt::from_json(e)?);
                    }
                }
                lf.module_extensions.insert(id.clone(), m);
            }
        } else {
            return Err("missing moduleExtensions".into());
        }
        if let Some(Json::Obj(o)) = j.get("facts") {
            for (id, v) in o {
                lf.facts.insert(id.clone(), v.clone());
            }
        }
        if let Some(Json::Obj(o)) = j.get("factsVersions") {
            for (id, v) in o {
                if let Some(n) = v.as_i64() {
                    lf.facts_versions.insert(id.clone(), n);
                }
            }
        }
        Ok(lf)
    }

    /// `BazelLockFileFunction.getLockfileValue`.
    pub fn read(path: &Path, mode: LockfileMode) -> Result<Lockfile, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Lockfile::default()),
            Err(e) => {
                return Err(format!(
                    "Failed to read and parse the MODULE.bazel.lock file with error: {e}. Try deleting it and rerun the build."
                ))
            }
        };
        let version = text
            .split("\"lockFileVersion\"")
            .nth(1)
            .and_then(|r| r.trim_start().strip_prefix(':'))
            .and_then(|r| {
                r.trim_start()
                    .split(|c: char| !c.is_ascii_digit() && c != '-')
                    .next()
                    .map(|s| s.to_string())
            })
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(-1);
        if version != LOCK_FILE_VERSION {
            if mode == LockfileMode::Error {
                return Err("The version of MODULE.bazel.lock is not supported by this version of Bazel. Please run `bazel mod deps --lockfile_mode=update` to update your lockfile.".into());
            }
            return Ok(Lockfile::default());
        }
        Lockfile::parse(&text).map_err(|e| {
            let hint = if text.contains("<<<<<<<") || text.contains(">>>>>>>") {
                " This looks like a merge conflict. See https://bazel.build/external/lockfile#merge-conflicts for advice."
            } else {
                " Try deleting it and rerun the build."
            };
            format!("Failed to read and parse the MODULE.bazel.lock file with error: {e}.{hint}")
        })
    }
}

pub fn metadata_to_lock(m: &ExtensionMetadata) -> Option<LockMetadata> {
    let facts_empty = match &m.facts {
        None => true,
        Some(crate::json::Json::Obj(o)) => o.is_empty(),
        Some(crate::json::Json::Null) => true,
        Some(_) => false,
    };
    let default = !m.reproducible
        && m.explicit_root_direct_deps.is_none()
        && m.explicit_root_direct_dev_deps.is_none()
        && m.use_all == "NO"
        && facts_empty;
    if default {
        return None;
    }
    Some(LockMetadata {
        explicit_root_direct_deps: m.explicit_root_direct_deps.clone(),
        explicit_root_direct_dev_deps: m.explicit_root_direct_dev_deps.clone(),
        use_all_repos: m.use_all.clone(),
        reproducible: m.reproducible,
    })
}

pub fn lock_to_metadata(m: &Option<LockMetadata>) -> Option<ExtensionMetadata> {
    m.as_ref().map(|m| ExtensionMetadata {
        explicit_root_direct_deps: m.explicit_root_direct_deps.clone(),
        explicit_root_direct_dev_deps: m.explicit_root_direct_dev_deps.clone(),
        use_all: m.use_all_repos.clone(),
        use_all_repos: m.use_all_repos != "NO",
        reproducible: m.reproducible,
        facts: None,
    })
}

pub fn result_from_lock(e: &LockExt) -> ExtensionResult {
    ExtensionResult {
        generated: e.generated_repo_specs.clone(),
        metadata: lock_to_metadata(&e.metadata),
        env_variables: BTreeMap::new(),
        recorded_file_inputs: BTreeMap::new(),
        recorded_inputs: e.recorded_inputs.clone(),
    }
}

/// Checks whether recorded inputs (`ENV:NAME value`) are still valid. Returns a reason if outdated.
/// `RepoRecordedInput.unescape`: `\0` is the null value.
pub fn unescape_input(s: &str) -> Option<String> {
    if s == "\\0" {
        return None;
    }
    let mut out = String::new();
    let mut esc = false;
    for c in s.chars() {
        if esc {
            out.push(match c {
                'n' => '\n',
                's' => ' ',
                other => other,
            });
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else {
            out.push(c);
        }
    }
    Some(out)
}

#[cfg(test)]
pub fn escape_unescape_roundtrip_for_tests(s: &str) -> bool {
    unescape_input(&crate::ctx::escape_input(s)).as_deref() == Some(s)
}

/// Computes a deterministic recursive digest of a directory tree (`DIRTREE:` recorded input),
/// mirroring `DirectoryTreeDigestFunction`: resolves symlinks on the root directory (while also
/// hashing the root symlink target when `root` itself is a symlink so retargeting always
/// invalidates), hashes nested file/symlink targets and resolved file/directory contents, and
/// guards canonicalized ancestor paths against symlink cycles.
pub fn compute_dirtree_digest(root: &std::path::Path) -> String {
    let lmeta = match std::fs::symlink_metadata(root) {
        Ok(m) => m,
        Err(_) => return "ENOENT".to_string(),
    };
    let meta = match std::fs::metadata(root) {
        Ok(m) => m,
        Err(_) => return "ENOENT".to_string(),
    };
    if !meta.is_dir() {
        return "NOT_DIR".to_string();
    }
    let mut fp = crate::hash::Fingerprint::new();
    if lmeta.file_type().is_symlink() {
        let target = std::fs::read_link(root)
            .map(|t| t.to_string_lossy().into_owned())
            .unwrap_or_default();
        fp.add_string(&format!("ROOT_SYM:{target}"));
    }
    let mut ancestors = std::collections::BTreeSet::new();
    if let Ok(canon) = std::fs::canonicalize(root) {
        ancestors.insert(canon);
    }
    fn walk(
        dir: &std::path::Path,
        rel_prefix: &str,
        fp: &mut crate::hash::Fingerprint,
        ancestors: &mut std::collections::BTreeSet<std::path::PathBuf>,
    ) {
        let mut entries: Vec<(String, std::path::PathBuf)> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
                    .collect()
            })
            .unwrap_or_default();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, path) in entries {
            let rel = if rel_prefix.is_empty() {
                name
            } else {
                format!("{rel_prefix}/{name}")
            };
            let Ok(m) = std::fs::symlink_metadata(&path) else {
                fp.add_string(&format!("ERR:{rel}"));
                continue;
            };
            let ft = m.file_type();
            if ft.is_symlink() {
                let target = std::fs::read_link(&path)
                    .map(|t| t.to_string_lossy().into_owned())
                    .unwrap_or_default();
                fp.add_string(&format!("SYM:{rel}:{target}"));
                match std::fs::metadata(&path) {
                    Ok(tmeta) if tmeta.is_dir() => match std::fs::canonicalize(&path) {
                        Ok(canon) if ancestors.contains(&canon) => {
                            fp.add_string(&format!("SYM_CYCLE:{rel}"));
                        }
                        Ok(canon) => {
                            fp.add_string(&format!("SYM_DIR:{rel}"));
                            ancestors.insert(canon.clone());
                            walk(&path, &rel, fp, ancestors);
                            ancestors.remove(&canon);
                        }
                        Err(_) => {
                            fp.add_string(&format!("SYM_ERR:{rel}"));
                        }
                    },
                    Ok(_) => {
                        let h = std::fs::read(&path)
                            .map(|b| crate::hash::sha256_hex(&b))
                            .unwrap_or_default();
                        fp.add_string(&format!("SYM_FILE:{rel}:{h}"));
                    }
                    Err(_) => {
                        fp.add_string(&format!("SYM_DANGLING:{rel}"));
                    }
                }
            } else if ft.is_dir() {
                match std::fs::canonicalize(&path) {
                    Ok(canon) if ancestors.contains(&canon) => {
                        fp.add_string(&format!("DIR_CYCLE:{rel}"));
                    }
                    Ok(canon) => {
                        fp.add_string(&format!("DIR:{rel}"));
                        ancestors.insert(canon.clone());
                        walk(&path, &rel, fp, ancestors);
                        ancestors.remove(&canon);
                    }
                    Err(_) => {
                        fp.add_string(&format!("DIR:{rel}"));
                        walk(&path, &rel, fp, ancestors);
                    }
                }
            } else {
                let h = std::fs::read(&path)
                    .map(|b| crate::hash::sha256_hex(&b))
                    .unwrap_or_default();
                fp.add_string(&format!("FILE:{rel}:{h}"));
            }
        }
    }
    walk(root, "", &mut fp, &mut ancestors);
    fp.hex_digest()
}

/// Checks recorded inputs (`ENV:`, `FILE:`, `DIRENTS:`, `DIRTREE:`, and `REPO_MAPPING:`) against
/// the current state; returns the description of the first change.
pub fn recorded_inputs_outdated(
    inputs: &[String],
    env: &BTreeMap<String, String>,
    workspace: &std::path::Path,
    external: &std::path::Path,
) -> Option<String> {
    recorded_inputs_outdated_with_lookup(inputs, env, workspace, external, None)
}

/// Like [`recorded_inputs_outdated`], additionally validating `REPO_MAPPING:` entries against
/// `repo_mapping_lookup(source_repo)` (`None` if `source_repo` no longer exists).
#[allow(clippy::type_complexity)]
pub fn recorded_inputs_outdated_with_lookup(
    inputs: &[String],
    env: &BTreeMap<String, String>,
    workspace: &std::path::Path,
    external: &std::path::Path,
    repo_mapping_lookup: Option<&dyn Fn(&str) -> Option<BTreeMap<String, String>>>,
) -> Option<String> {
    let resolve = |p: &str| -> Option<std::path::PathBuf> {
        if let Some(rest) = p.strip_prefix("@@").or_else(|| p.strip_prefix('@')) {
            let (repo, rel) = rest.split_once("//")?;
            Some(if repo.is_empty() {
                workspace.join(rel)
            } else {
                external.join(repo).join(rel)
            })
        } else if let Some(rel) = p.strip_prefix("//") {
            Some(workspace.join(rel))
        } else {
            Some(std::path::PathBuf::from(p))
        }
    };
    for i in inputs {
        let Some((raw_input, raw_value)) = i.split_once(' ') else {
            return Some("malformed recorded input".into());
        };
        let Some(input) = unescape_input(raw_input) else {
            return Some("malformed recorded input".into());
        };
        let value = unescape_input(raw_value);
        if let Some(name) = input.strip_prefix("ENV:") {
            let cur = env.get(name).cloned();
            if cur != value {
                let show = |v: &Option<String>| {
                    v.as_ref()
                        .map(|x| format!("'{x}'"))
                        .unwrap_or_else(|| "<unset>".into())
                };
                return Some(format!(
                    "environment variable {name} changed: {} -> {}",
                    show(&value),
                    show(&cur)
                ));
            }
        } else if let Some(p) = input.strip_prefix("FILE:") {
            let Some(path) = resolve(p) else {
                return Some("malformed recorded input".into());
            };
            let cur = match std::fs::metadata(&path) {
                Ok(meta) if meta.is_dir() => "DIR".to_string(),
                Ok(_) => match std::fs::read(&path) {
                    Ok(b) => crate::hash::sha256_hex(&b),
                    Err(e) => return Some(format!("failed to stat {p}: {e}")),
                },
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    "ENOENT".to_string()
                }
                Err(e) => return Some(format!("failed to stat {p}: {e}")),
            };
            if Some(&cur) != value.as_ref() {
                return Some(format!("file info or contents of {p} changed"));
            }
        } else if let Some(p) = input.strip_prefix("DIRENTS:") {
            let Some(path) = resolve(p) else {
                return Some("malformed recorded input".into());
            };
            if !path.is_dir() {
                return Some(format!("directory entries of {p} changed"));
            }
            let mut names: Vec<String> = std::fs::read_dir(&path)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            if Some(
                crate::hash::Fingerprint::new()
                    .add_strings(&names)
                    .hex_digest(),
            ) != value
            {
                return Some(format!("directory entries of {p} changed"));
            }
        } else if let Some(p) = input.strip_prefix("DIRTREE:") {
            let Some(path) = resolve(p) else {
                return Some("malformed recorded input".into());
            };
            let cur = compute_dirtree_digest(&path);
            if matches!(cur.as_str(), "ENOENT" | "NOT_DIR") || Some(&cur) != value.as_ref() {
                return Some(format!("directory tree at {p} changed"));
            }
        } else if let Some(spec) = input.strip_prefix("REPO_MAPPING:") {
            let Some((source_repo, apparent)) = spec.split_once(',') else {
                return Some("malformed recorded input".into());
            };
            if let Some(lookup) = repo_mapping_lookup {
                let Some(map) = lookup(source_repo) else {
                    return Some(format!("source repo {source_repo} doesn't exist anymore"));
                };
                let cur = map.get(apparent).cloned();
                if cur != value {
                    let show = |v: &Option<String>| {
                        v.as_ref()
                            .map(|x| {
                                if x.is_empty() {
                                    "@@".to_string()
                                } else {
                                    format!("@@{x}")
                                }
                            })
                            .unwrap_or_else(|| "<doesn't exist>".into())
                    };
                    return Some(format!(
                        "canonical name for @{apparent} in {source_repo} changed: {} -> {}",
                        show(&value),
                        show(&cur)
                    ));
                }
            }
        } else {
            return Some("malformed recorded input".into());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_lockfile() {
        let mut lf = Lockfile::default();
        lf.registry_file_hashes
            .insert("https://x/a".into(), Some(crate::hash::sha256_hex(b"hi")));
        lf.registry_file_hashes
            .insert("https://x/missing".into(), None);
        lf.selected_yanked_versions
            .insert(ModuleKey::parse("foo@1.0").unwrap(), "because".into());
        let mut m = BTreeMap::new();
        m.insert(
            "general".to_string(),
            LockExt {
                bzl_transitive_digest: vec![1, 2, 3],
                usages_digest: vec![4, 5],
                recorded_inputs: vec!["ENV:FOO bar".into()],
                generated_repo_specs: vec![(
                    "r".into(),
                    RepoSpec::new(
                        "@bazel_tools//tools/build_defs/repo:http.bzl",
                        "http_archive",
                        vec![
                            ("name".into(), AttrVal::Str("r".into())),
                            ("urls".into(), AttrVal::List(vec![AttrVal::Str("u".into())])),
                        ],
                    ),
                )],
                metadata: Some(LockMetadata {
                    explicit_root_direct_deps: Some(vec!["r".into()]),
                    explicit_root_direct_dev_deps: None,
                    use_all_repos: "NO".into(),
                    reproducible: false,
                }),
            },
        );
        lf.module_extensions.insert("@@foo+//:e.bzl%e".into(), m);
        let text = lf.to_text();
        assert!(text.contains("\"lockFileVersion\": 28"));
        assert!(text.contains("\"not found\""));
        assert!(text.contains(
            "\"repoRuleId\": \"@@bazel_tools//tools/build_defs/repo:http.bzl%http_archive\""
        ));
        // `name` is reserved (`RepoRule.RESERVED_ATTRIBUTES`) and never serialized; it is re-added on read.
        assert!(!text.contains("\"name\""), "{text}");
        let back = Lockfile::parse(&text).unwrap();
        assert_eq!(back, lf);
    }

    #[test]
    fn extension_key_matches_label_to_string() {
        let id = |bzl: &str| crate::resolve::ExtensionId {
            bzl_file: bzl.into(),
            name: "ex".into(),
            isolation_key: None,
        };
        assert_eq!(extension_key(&id("@@//:ext.bzl")), "//:ext.bzl%ex");
        assert_eq!(extension_key(&id("@@mx+//:ext.bzl")), "@@mx+//:ext.bzl%ex");
        assert_eq!(extension_display(&id("@@//:ext.bzl")), "@@//:ext.bzl%ex");
    }

    #[test]
    fn read_modes() {
        let d = crate::registry::testutil::tmpdir("lockread");
        let p = d.join("MODULE.bazel.lock");
        assert_eq!(
            Lockfile::read(&p, LockfileMode::Error).unwrap(),
            Lockfile::default()
        );
        std::fs::write(&p, "{\"lockFileVersion\": 3}").unwrap();
        assert!(Lockfile::read(&p, LockfileMode::Error)
            .unwrap_err()
            .contains("not supported by this version of Bazel"));
        assert_eq!(
            Lockfile::read(&p, LockfileMode::Update).unwrap(),
            Lockfile::default()
        );
        std::fs::write(&p, "{\"lockFileVersion\": 28, <<<<<<< x").unwrap();
        assert!(Lockfile::read(&p, LockfileMode::Update)
            .unwrap_err()
            .contains("merge conflict"));
        // Reading a directory instead of a missing file must report an error rather than empty Lockfile.
        let dir_as_lock = d.join("dir_lock");
        std::fs::create_dir_all(&dir_as_lock).unwrap();
        assert!(Lockfile::read(&dir_as_lock, LockfileMode::Update)
            .unwrap_err()
            .contains("Failed to read and parse the MODULE.bazel.lock file"));
        // Malformed recorded input without space is flagged outdated, and DIRTREE detects file edits.
        let env = BTreeMap::new();
        assert_eq!(
            recorded_inputs_outdated(&["NO_SPACE_HERE".into()], &env, &d, &d),
            Some("malformed recorded input".into())
        );
        let tree = d.join("tree");
        std::fs::create_dir_all(tree.join("sub")).unwrap();
        std::fs::write(tree.join("sub/a.txt"), "v1").unwrap();
        let digest1 = compute_dirtree_digest(&tree);
        let rec_entry = format!("DIRTREE:@@//tree {digest1}");
        assert_eq!(
            recorded_inputs_outdated(&[rec_entry.clone()], &env, &d, &d),
            None
        );
        std::fs::write(tree.join("sub/a.txt"), "v2").unwrap();
        assert!(recorded_inputs_outdated(&[rec_entry], &env, &d, &d)
            .unwrap()
            .contains("directory tree"));
    }

    #[test]
    fn reads_9_3_2_lockfile_registry_hashes() {
        let d = crate::registry::testutil::tmpdir("lockread28");
        let p = d.join("MODULE.bazel.lock");
        let h = "72e76b0eea4e81611ef5452aa82b3da34caca0c8b7b5c0c9584338aa93bae26b";
        std::fs::write(&p, format!("{{\"lockFileVersion\": 28, \"registryFileHashes\": {{\"https://r/modules/a/1/MODULE.bazel\": \"{h}\"}}, \"selectedYankedVersions\": {{}}, \"moduleExtensions\": {{}}}}")).unwrap();
        let lf = Lockfile::read(&p, LockfileMode::Error).unwrap();
        assert_eq!(
            lf.registry_file_hashes
                .get("https://r/modules/a/1/MODULE.bazel"),
            Some(&Some(h.to_string()))
        );
    }

    #[test]
    fn recorded_inputs_support_dirtree_workspace_labels_and_repo_mappings() {
        let ws = crate::registry::testutil::tmpdir("rec_ws");
        let ext = crate::registry::testutil::tmpdir("rec_ext");
        std::fs::create_dir_all(ws.join("pkg/sub")).unwrap();
        std::fs::write(ws.join("pkg/sub/a.txt"), b"v1").unwrap();
        std::fs::create_dir_all(ext.join("dep+/dir")).unwrap();
        std::fs::write(ext.join("dep+/dir/b.txt"), b"ext1").unwrap();

        let ws_tree = crate::ctx::compute_dirtree_digest(&ws.join("pkg")).unwrap();
        let file_sha = crate::hash::sha256_hex(b"v1");
        let ext_sha = crate::hash::sha256_hex(b"ext1");
        let inputs = vec![
            format!("FILE://pkg/sub/a.txt {file_sha}"),
            format!("FILE:@dep+//dir/b.txt {ext_sha}"),
            format!("DIRTREE:@@//pkg {ws_tree}"),
            "REPO_MAPPING:,my_dep dep+".to_string(),
        ];
        let env = BTreeMap::new();
        let mut root_map = BTreeMap::new();
        root_map.insert("my_dep".to_string(), "dep+".to_string());
        let lookup_ok = |src: &str| -> Option<BTreeMap<String, String>> {
            if src.is_empty() {
                Some(root_map.clone())
            } else {
                None
            }
        };
        assert_eq!(
            recorded_inputs_outdated_with_lookup(&inputs, &env, &ws, &ext, Some(&lookup_ok)),
            None
        );

        // Changing a nested file invalidates DIRTREE even when top-level DIRENTS are unchanged.
        std::fs::write(ws.join("pkg/sub/a.txt"), b"v2").unwrap();
        let inputs_tree_only = vec![format!("DIRTREE:@@//pkg {ws_tree}")];
        assert_eq!(
            recorded_inputs_outdated_with_lookup(
                &inputs_tree_only,
                &env,
                &ws,
                &ext,
                Some(&lookup_ok)
            ),
            Some("directory tree at @@//pkg changed".to_string())
        );

        // Changing a repo mapping entry reports canonical name diff.
        let mut changed_map = BTreeMap::new();
        changed_map.insert("my_dep".to_string(), "dep+v2".to_string());
        let lookup_changed = |src: &str| -> Option<BTreeMap<String, String>> {
            if src.is_empty() {
                Some(changed_map.clone())
            } else {
                None
            }
        };
        assert_eq!(
            recorded_inputs_outdated_with_lookup(
                &["REPO_MAPPING:,my_dep dep+".to_string()],
                &env,
                &ws,
                &ext,
                Some(&lookup_changed),
            ),
            Some("canonical name for @my_dep in  changed: @@dep+ -> @@dep+v2".to_string())
        );

        // Deleted source repo reports missing source repo.
        let lookup_missing = |_src: &str| -> Option<BTreeMap<String, String>> { None };
        assert_eq!(
            recorded_inputs_outdated_with_lookup(
                &["REPO_MAPPING:gone_repo+,my_dep dep+".to_string()],
                &env,
                &ws,
                &ext,
                Some(&lookup_missing),
            ),
            Some("source repo gone_repo+ doesn't exist anymore".to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn dirtree_symlink_root_retarget_nested_symlinks_and_cycles() {
        let d = crate::registry::testutil::tmpdir("dirtree_symlink_root");
        let dir_a = d.join("dir_a");
        let dir_b = d.join("dir_b");
        std::fs::create_dir_all(dir_a.join("sub")).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();
        std::fs::write(dir_a.join("sub/item.txt"), "initial").unwrap();
        std::fs::write(dir_b.join("sub_item.txt"), "other").unwrap();

        let root_link = d.join("watched_link");
        std::os::unix::fs::symlink(&dir_a, &root_link).unwrap();

        let d0 = compute_dirtree_digest(&root_link);
        assert_ne!(d0, "NOT_DIR");
        assert_ne!(d0, "ENOENT");

        // Modifying, adding, and deleting files inside the symlinked target directory must change digest.
        std::fs::write(dir_a.join("sub/item.txt"), "modified").unwrap();
        let d_mod = compute_dirtree_digest(&root_link);
        assert_ne!(d_mod, d0);

        std::fs::write(dir_a.join("added.txt"), "extra").unwrap();
        let d_add = compute_dirtree_digest(&root_link);
        assert_ne!(d_add, d_mod);

        std::fs::remove_file(dir_a.join("added.txt")).unwrap();
        assert_eq!(compute_dirtree_digest(&root_link), d_mod);

        // Symlink cycle inside the directory tree terminates deterministically without stack overflow.
        std::os::unix::fs::symlink(&dir_a, dir_a.join("self_loop")).unwrap();
        let d_cycle = compute_dirtree_digest(&root_link);
        assert_ne!(d_cycle, d_mod);
        assert_eq!(compute_dirtree_digest(&root_link), d_cycle);

        // Retargeting the root symlink to a different directory changes the digest (even between two empty dirs).
        let empty_one = d.join("empty_one");
        let empty_two = d.join("empty_two");
        std::fs::create_dir_all(&empty_one).unwrap();
        std::fs::create_dir_all(&empty_two).unwrap();
        std::fs::remove_file(&root_link).unwrap();
        std::os::unix::fs::symlink(&empty_one, &root_link).unwrap();
        let d_empty_one = compute_dirtree_digest(&root_link);
        std::fs::remove_file(&root_link).unwrap();
        std::os::unix::fs::symlink(&empty_two, &root_link).unwrap();
        let d_empty_two = compute_dirtree_digest(&root_link);
        assert_ne!(d_empty_one, d_empty_two);

        // Replacing root symlink with a regular file -> NOT_DIR; deleting -> ENOENT.
        std::fs::remove_file(&root_link).unwrap();
        std::fs::write(&root_link, "plain-file").unwrap();
        assert_eq!(compute_dirtree_digest(&root_link), "NOT_DIR");
        std::fs::remove_file(&root_link).unwrap();
        assert_eq!(compute_dirtree_digest(&root_link), "ENOENT");
    }
}
