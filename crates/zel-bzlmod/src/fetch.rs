//! Repository fetching orchestration (`RepositoryFetchFunction`) and module extension evaluation
//! (`SingleExtensionEvalFunction`).

use crate::ctx::{build_attr_struct, CtxData, CtxKind};
use crate::model::*;
use crate::resolve::ExtensionId;
use crate::rt::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use zel_starlark::Value;

/// Gson representation used by Bazel's `AttributeValues` marker fingerprint.
///
/// This intentionally differs from lockfile JSON: Gson serializes `StarlarkInt` through its
/// object adapter as `{"v": ...}`, including when nested in lists or dictionaries.
fn marker_attribute_json(value: &AttrVal) -> crate::json::Json {
    use crate::json::Json;
    match value {
        AttrVal::Int(i) => Json::Obj(vec![("v".to_string(), Json::int(*i))]),
        AttrVal::List(values) => Json::Arr(values.iter().map(marker_attribute_json).collect()),
        AttrVal::Dict(entries) => Json::Obj(
            entries
                .iter()
                .map(|(key, value)| {
                    (
                        crate::model::escape_attr_string(key),
                        marker_attribute_json(value),
                    )
                })
                .collect(),
        ),
        AttrVal::LabelDict(entries) => Json::Obj(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), marker_attribute_json(value)))
                .collect(),
        ),
        value => value.to_json(),
    }
}

fn parse_bzl_label(label: &str) -> (String, String, String) {
    let r = label.trim_start_matches('@');
    let (repo, rest) = r.split_once("//").unwrap_or((r, ""));
    let (pkg, name) = rest.split_once(':').unwrap_or(("", rest));
    (repo.to_string(), pkg.to_string(), name.to_string())
}

/// Java `os.name` lower-cased and `os.arch` lower-cased (`DigestWriter.computePredeclaredInputHash`).
fn java_os_name_and_arch() -> (&'static str, &'static str) {
    let name = match std::env::consts::OS {
        "macos" => "mac os x",
        "windows" => "windows",
        _ => "linux",
    };
    let arch = match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", _) => "aarch64",
        ("x86_64", "linux") | ("x86_64", "windows") => "amd64",
        ("x86_64", _) => "x86_64",
        (a, _) => a,
    };
    (name, arch)
}

fn bazel_tools_digest() -> &'static str {
    static DIGEST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DIGEST.get_or_init(|| {
        let mut fp = crate::hash::Fingerprint::new();
        fp.add_string("bazel_tools_v2");
        for (path, bytes) in zel_bazel_tools::files() {
            fp.add_string(path).add_bytes(bytes);
        }
        fp.hex_digest()
    })
}

/// Whether the content of a marker file belongs to the repo with predeclared hash `fp`: its first
/// line is the hash and every recorded input line is still up to date (`DigestWriter.readMarkerFile`).
pub fn marker_is_current(
    content: &str,
    fp: &str,
    env: &BTreeMap<String, String>,
    ws: &Path,
    external: &Path,
) -> bool {
    marker_is_current_with_lookup(content, fp, env, ws, external, None)
}

#[allow(clippy::type_complexity)]
pub fn marker_is_current_with_lookup(
    content: &str,
    fp: &str,
    env: &BTreeMap<String, String>,
    ws: &Path,
    external: &Path,
    lookup: Option<&dyn Fn(&str) -> Option<BTreeMap<String, String>>>,
) -> bool {
    let mut lines = content.split('\n').filter(|l| !l.is_empty());
    if lines.next() != Some(fp) {
        return false;
    }
    let rest: Vec<String> = lines.map(String::from).collect();
    crate::lockfile::recorded_inputs_outdated_with_lookup(&rest, env, ws, external, lookup)
        .is_none()
}

impl Rt {
    /// `DigestWriter.computePredeclaredInputHash`.
    pub fn predeclared_input_hash(self: &Arc<Self>, canonical: &str, spec: &RepoSpec) -> String {
        use crate::hash::Fingerprint;
        let (repo, pkg, name) = parse_bzl_label(&spec.bzl_file);
        let key = format!("@@{repo}//{pkg}:{name}");
        let fingerprint_key = if repo.is_empty() {
            format!("//{pkg}:{name}")
        } else {
            key.clone()
        };
        let digest = self.bzl_transitive_digest(&key);
        let environ = self.rule_environ(spec).unwrap_or_default();
        let mut env_inputs: Vec<(String, Option<String>)> = environ
            .iter()
            .map(|e| (e.clone(), self.opts.client_env.get(e).cloned()))
            .collect();
        env_inputs.sort();
        env_inputs.dedup_by(|a, b| a.0 == b.0);
        let attrs_json = crate::json::Json::Obj(vec![(
            "attributes".to_string(),
            crate::json::Json::Obj(
                spec.attrs
                    .iter()
                    .filter(|(k, v)| k != "name" && !matches!(v, AttrVal::None))
                    .map(|(k, v)| (k.clone(), marker_attribute_json(v)))
                    .collect(),
            ),
        )])
        .to_compact();
        let (os, arch) = java_os_name_and_arch();
        let mut fp = Fingerprint::new();
        fp.add_int(7)
            .add_bytes(&self.opts.semantics_fingerprint)
            .add_string(&fingerprint_key)
            .add_string(&spec.rule_name)
            .add_bytes(&digest)
            .add_string(canonical)
            .add_string(&attrs_json)
            .add_string(os)
            .add_string(arch);
        fp.add_int(env_inputs.len() as i32);
        for (k, v) in &env_inputs {
            fp.add_string(&format!("ENV:{}", crate::ctx::escape_input(k)))
                .add_nullable_string(v.as_deref());
        }
        let mapping = self.recorded_repo_mapping_entries(&key);
        fp.add_int(mapping.len() as i32);
        for (src, apparent, canon) in &mapping {
            fp.add_string(src).add_string(apparent).add_string(canon);
        }
        fp.hex_digest()
    }

    /// `(source repo, apparent name, canonical name)` for every `@apparent//...` load of the
    /// transitive load graph of `key`, in load order (the rule's `recordedRepoMappingEntries`).
    pub(crate) fn recorded_repo_mapping_entries(
        self: &Arc<Self>,
        key: &str,
    ) -> Vec<(String, String, String)> {
        if let Some(cached) = self.state.lock().unwrap().bzl_load_mappings.get(key) {
            return cached.clone();
        }
        let mut out: Vec<(String, String, String)> = vec![];
        let mut seen: Vec<String> = vec![];
        self.collect_load_mappings(key, &mut out, &mut seen);
        self.state
            .lock()
            .unwrap()
            .bzl_load_mappings
            .insert(key.to_string(), out.clone());
        out
    }

    fn collect_load_mappings(
        self: &Arc<Self>,
        key: &str,
        out: &mut Vec<(String, String, String)>,
        seen: &mut Vec<String>,
    ) {
        if seen.iter().any(|k| k == key) {
            return;
        }
        seen.push(key.to_string());
        let (repo, pkg, name) = parse_bzl_label(key);
        let loads = if let Some(cached) = self.state.lock().unwrap().bzl_loads_cache.get(key) {
            cached.clone()
        } else {
            let Ok(dir) = self.fetch_repo(&repo) else {
                return;
            };
            let path = if pkg.is_empty() {
                dir.join(&name)
            } else {
                dir.join(&pkg).join(&name)
            };
            let Ok(bytes) = std::fs::read(path) else {
                return;
            };
            let text = String::from_utf8_lossy(&bytes).to_string();
            self.bzl_loads_of(key, &text)
        };
        for l in loads {
            if let Some(rest) = l.strip_prefix('@') {
                if !rest.starts_with('@') {
                    if let Some((apparent, _)) = rest.split_once("//") {
                        if let Ok((r, _, _)) = self.resolve_label(&l, &repo, &pkg) {
                            if !out.iter().any(|(s, a, _)| *s == repo && a == apparent) {
                                out.push((repo.clone(), apparent.to_string(), r));
                            }
                        }
                    }
                }
            }
            if let Ok((r, p, n)) = self.resolve_label(&l, &repo, &pkg) {
                self.collect_load_mappings(&format!("@@{r}//{p}:{n}"), out, seen);
            }
        }
    }

    fn marker_path(&self, canonical: &str) -> PathBuf {
        self.external_dir().join(format!("@{canonical}.marker"))
    }

    pub(crate) fn override_for(&self, canonical: &str) -> Option<PathBuf> {
        let root_map = self.resolution.repo_mapping(&ModuleKey::root());
        for (name, path) in &self.opts.override_repository {
            if name == canonical || root_map.get(name).is_some_and(|c| c == canonical) {
                return Some(path.clone());
            }
        }
        None
    }

    /// Finds the spec for `canonical`: module repo, extension-generated repo, or innate repo.
    pub fn repo_spec_for(self: &Arc<Self>, canonical: &str) -> Result<Option<RepoSpec>, String> {
        let res = &self.resolution;
        if canonical.is_empty() {
            return Ok(None);
        }
        if let Some(k) = res.canonical_repo_names.get(&canonical.to_string()) {
            let m = res.dep_graph.get(k).unwrap();
            return Ok(m.repo_spec.clone());
        }
        // Extension-generated.
        for id in res.extension_ids() {
            let Some(unique) = res.extension_unique_names.get(&id) else {
                continue;
            };
            let Some(exported) = canonical.strip_prefix(&format!("{unique}+")) else {
                continue;
            };
            let result = self.eval_extension(&id)?;
            if let Some((_, spec)) = result.generated.iter().find(|(n, _)| n == exported) {
                return Ok(Some(spec.clone()));
            }
        }
        Err(format!("Repository '@@{canonical}' is not defined"))
    }

    /// Computes `repository_ctx.original_name` matching Bazel's `RepoDefinitionFunction`
    /// (`RepoDefinitionFunction.java:145-174, 194-200`) and `StarlarkRepositoryContext`
    /// (`StarlarkRepositoryContext.java:137-143`):
    /// - Repositories backing Bazel modules (`canonical_repo_names`, whether registry-sourced or
    ///   `NonRegistryOverride`) pass `originalName = null`, so `getOriginalName()` returns
    ///   `repoDefinition.name()` (the canonical repository name, e.g. `rules_java+`).
    /// - Repositories generated by regular module extensions or `use_repo_rule` (`InnateRunnableExtension`,
    ///   both keyed in `extension_unique_names`, e.g. `+<rule_name>+<name>`) pass
    ///   `originalName = internalRepo`, so `getOriginalName()` returns the unprefixed `name`
    ///   passed to the repo rule.
    fn original_name_for(&self, canonical: &str, given_name: &str) -> String {
        let res = &self.resolution;
        if res
            .canonical_repo_names
            .contains_key(&canonical.to_string())
        {
            return canonical.to_string();
        }
        for unique in res.extension_unique_names.values() {
            if let Some(exported) = canonical.strip_prefix(&format!("{unique}+")) {
                return exported.to_string();
            }
        }
        given_name.to_string()
    }

    /// The repo in whose context label-valued attributes of `canonical`'s rule are resolved:
    /// the using module for innate (`use_repo_rule`) repos, the extension's repo otherwise.
    pub fn repo_context(&self, canonical: &str) -> String {
        let res = &self.resolution;
        if res
            .canonical_repo_names
            .contains_key(&canonical.to_string())
        {
            return canonical.to_string();
        }
        for id in res.extension_ids() {
            let Some(unique) = res.extension_unique_names.get(&id) else {
                continue;
            };
            let Some(exported) = canonical.strip_prefix(&format!("{unique}+")) else {
                continue;
            };
            if id.is_innate() {
                for (mk, u) in res.usages_of(&id) {
                    let declares = u.tags.iter().any(|t| {
                        t.tag_name == "repo"
                            && t.attrs
                                .iter()
                                .any(|(k, v)| k == "name" && v.as_str() == Some(exported))
                    });
                    if declares {
                        return res.canonical_name_of(mk).cloned().unwrap_or_default();
                    }
                }
                return String::new();
            }
            return id.bzl_repo();
        }
        canonical.to_string()
    }

    /// Ensures `canonical` is fetched; returns its directory.
    pub fn fetch_repo(self: &Arc<Self>, canonical: &str) -> Result<PathBuf, String> {
        if canonical.is_empty() {
            return Ok(self.opts.workspace_root.clone());
        }
        let dir = self.repo_dir(canonical);
        {
            let st = self.state.lock().unwrap();
            if st.fetched.contains(canonical) && dir.exists() {
                return Ok(dir);
            }
            if st.fetching.iter().any(|f| f == canonical) {
                let mut chain = st.fetching.clone();
                chain.push(canonical.to_string());
                return Err(format!(
                    "Circular repository dependency while fetching: {}",
                    chain
                        .iter()
                        .map(|c| format!("@@{c}"))
                        .collect::<Vec<_>>()
                        .join(" -> ")
                ));
            }
        }
        self.state
            .lock()
            .unwrap()
            .fetching
            .push(canonical.to_string());
        let r = self.fetch_repo_inner(canonical, &dir);
        self.state
            .lock()
            .unwrap()
            .fetching
            .retain(|c| c != canonical);
        r?;
        self.state
            .lock()
            .unwrap()
            .fetched
            .insert(canonical.to_string());
        Ok(dir)
    }

    fn fetch_repo_inner(self: &Arc<Self>, canonical: &str, dir: &Path) -> Result<(), String> {
        let _ = std::fs::create_dir_all(self.external_dir());
        let marker = self.marker_path(canonical);
        let spec: Option<RepoSpec>;
        if let Some(p) = self.override_for(canonical) {
            // `RepositoryFetchFunction.symlinkRepoRoot`: the override target must be a directory.
            if !p.is_dir() {
                return Err(format!(
                    "The repository's path is \"{canonical}\" (absolute: \"{}\") but it does not exist or is not a directory.",
                    p.display()
                ));
            }
            if !["MODULE.bazel", "REPO.bazel", "WORKSPACE", "WORKSPACE.bazel"]
                .iter()
                .any(|name| p.join(name).exists())
            {
                return Err(format!(
                    "No MODULE.bazel, REPO.bazel, or WORKSPACE file found in {}",
                    p.display()
                ));
            }
            if canonical == "bazel_tools" {
                let _ = remove_any(dir);
                #[cfg(unix)]
                {
                    std::os::unix::fs::symlink(&p, dir).map_err(|e| e.to_string())?;
                }
                #[cfg(not(unix))]
                {
                    copy_tree(&p, dir)?;
                }
                let fp = crate::hash::sha256_hex(format!("override:{}", p.display()).as_bytes());
                let _ = std::fs::write(&marker, format!("{fp}\n"));
                return Ok(());
            }
            let s = RepoSpec::new(
                LOCAL_BZL,
                "local_repository",
                vec![(
                    "path".into(),
                    AttrVal::Str(crate::session::format_option_path_fragment(
                        &p,
                        cfg!(windows),
                    )),
                )],
            );
            spec = Some(s);
        } else if canonical == "bazel_tools" {
            return self.make_bazel_tools(dir);
        } else {
            spec = self.repo_spec_for(canonical)?;
        }
        let Some(spec) = spec else {
            return Err(format!("Repository '@@{canonical}' has no repo spec (is the module a non-registry override with a missing source?)"));
        };
        let is_local = self.is_local_rule(&spec);
        let is_configure = self.is_configure_rule(&spec);
        let force_configure = is_configure && self.state.lock().unwrap().force_configure;
        // `RepositoryFetchFunction.shouldUseCachedRepoContents`: when fetching is enabled
        // (`!nofetch`), `local = True` repository rules always bypass cached repo contents.
        let can_use_cached =
            !self.opts.force && !force_configure && (!is_local || self.opts.nofetch);
        let fp = self.predeclared_input_hash(canonical, &spec);
        let lookup = |src: &str| self.try_repo_mapping_of(src);
        let marker_ok = |m: &str| {
            marker_is_current_with_lookup(
                m,
                &fp,
                &self.opts.client_env,
                &self.opts.workspace_root,
                &self.external_dir(),
                Some(&lookup),
            )
        };
        if can_use_cached && dir.exists() {
            if let Ok(m) = std::fs::read_to_string(&marker) {
                if marker_ok(&m) {
                    return Ok(());
                }
            }
        }
        // Repo contents cache lookup?
        if let Some(rcc_dir) = &self.opts.repo_contents_cache {
            if can_use_cached && !is_local {
                let cache = crate::repo_contents_cache::RepoContentsCache::new(rcc_dir);
                let candidates = cache.get_candidate_repos(&fp);
                for cand in candidates {
                    if let Ok(m) = std::fs::read_to_string(&cand.recorded_inputs_file) {
                        if marker_ok(&m) {
                            let _ = remove_any(dir);
                            #[cfg(unix)]
                            {
                                let _ = std::os::unix::fs::symlink(&cand.contents_dir, dir);
                            }
                            #[cfg(not(unix))]
                            {
                                copy_tree(&cand.contents_dir, dir)?;
                            }
                            cand.touch();
                            let _ = std::fs::write(&marker, format!("{fp}\n"));
                            return Ok(());
                        }
                    }
                }
            }
        }
        // Vendored copy?
        if let Some(v) = &self.opts.vendor_dir {
            let vf = crate::vendor::VendorFile::load(v).unwrap_or_default();
            if !vf.ignored_repos.contains(canonical) {
                let vdir = v.join(canonical);
                if vf.pinned_repos.contains(canonical) {
                    if vdir.is_dir() {
                        let _ = remove_any(dir);
                        #[cfg(unix)]
                        {
                            let _ = std::os::unix::fs::symlink(&vdir, dir);
                        }
                        #[cfg(not(unix))]
                        {
                            copy_tree(&vdir, dir)?;
                        }
                        return Ok(());
                    }
                } else if vdir.is_dir() && can_use_cached && !is_local && !is_configure {
                    let vmarker = v.join(format!("@{canonical}.marker"));
                    let up_to_date = std::fs::read_to_string(&vmarker)
                        .map(|m| marker_ok(&m))
                        .unwrap_or(false);
                    if up_to_date || self.opts.disable_download {
                        let _ = remove_any(dir);
                        #[cfg(unix)]
                        {
                            let _ = std::os::unix::fs::symlink(&vdir, dir);
                        }
                        #[cfg(not(unix))]
                        {
                            copy_tree(&vdir, dir)?;
                        }
                        let _ = std::fs::write(&marker, format!("{fp}\n"));
                        return Ok(());
                    }
                }
            }
        }
        if self.opts.nofetch {
            // `RepositoryFetchFunction` with `FETCH_DISABLED`: use whatever is on disk (with a warning), or fail.
            if dir.symlink_metadata().is_ok() {
                self.info(format!(
                    "WARNING: External repository '@@{canonical}' is not up-to-date and fetching is disabled. To update, run the build without the '--nofetch' command line option."
                ));
                return Ok(());
            }
            return Err(format!(
                "to fix, run\n\tbazel fetch //...\nExternal repository @@{canonical} not found and fetching repositories is disabled."
            ));
        }
        let _ = remove_any(dir);
        let _ = std::fs::remove_file(&marker);
        // Bazel reports `Fetching repository @@x; <rule>` only as UI progress, never in the log.
        let (is_reproducible, recorded_inputs) =
            match self.run_rule(&spec, canonical, &self.repo_context(canonical), dir) {
                Ok(rep) => rep,
                Err(e) => {
                    let _ = remove_any(dir);
                    return Err(e);
                }
            };
        if let Some(msg) = Self::ensure_repo_boundary(dir) {
            return Err(format!(
                "fetching {canonical}: java.io.IOException: {msg}\nERROR: {msg}"
            ));
        }
        let mut marker_text = format!("{fp}\n");
        for (k, v) in &recorded_inputs {
            let val = match v {
                Some(s) => crate::ctx::escape_input(s),
                None => "\\0".to_string(),
            };
            marker_text.push_str(&format!("{k} {val}\n"));
        }
        std::fs::write(&marker, &marker_text).map_err(|e| e.to_string())?;

        // `RepositoryFetchFunction`: reproducible non-local repos get their symlinks replanted (absolute targets under
        // the external root become relative) before they may enter the repo contents cache.
        if is_reproducible && !is_local {
            if let Err(e) =
                replant_symlinks_relative(dir, &self.opts.workspace_root, &self.external_dir())
            {
                return Err(format!(
                    "error replanting symlinks in repo {canonical} before caching: {e}"
                ));
            }
        }
        // If repo_contents_cache is enabled, cache reproducible non-local repos.
        if is_reproducible && !is_local {
            if let Some(rcc_dir) = &self.opts.repo_contents_cache {
                let cache = crate::repo_contents_cache::RepoContentsCache::new(rcc_dir);
                let _ = cache.move_to_cache(dir, &marker, &fp);
            }
        }
        Ok(())
    }

    pub(crate) fn rule_meta_result(
        self: &Arc<Self>,
        spec: &RepoSpec,
    ) -> Result<Option<(bool, bool, Vec<String>)>, String> {
        let (repo, pkg, name) = parse_bzl_label(&spec.bzl_file);
        let key = format!("@@{repo}//{pkg}:{name}");
        let cache_key = (key.clone(), spec.rule_name.clone());
        if let Some(cached) = self.state.lock().unwrap().rule_meta_cache.get(&cache_key) {
            return Ok(Some(cached.clone()));
        }
        let ctx = self
            .eval_bzl_fresh(&key, &repo, &pkg, &name)
            .map_err(|e| e.to_string())?;
        let meta = match ctx.globals.get(&spec.rule_name) {
            Some(Value::Host(k, id)) if k == "repository_rule" => {
                let st = self.state.lock().unwrap();
                let Some(idx) = id.parse::<usize>().ok() else {
                    return Ok(None);
                };
                let Some(def) = st.rule_defs.get(idx) else {
                    return Ok(None);
                };
                (def.local, def.configure, def.environ.clone())
            }
            _ => return Ok(None),
        };
        self.state
            .lock()
            .unwrap()
            .rule_meta_cache
            .insert(cache_key, meta.clone());
        Ok(Some(meta))
    }

    pub(crate) fn rule_meta(
        self: &Arc<Self>,
        spec: &RepoSpec,
    ) -> Option<(bool, bool, Vec<String>)> {
        self.rule_meta_result(spec).ok().flatten()
    }

    fn rule_environ(self: &Arc<Self>, spec: &RepoSpec) -> Option<Vec<String>> {
        self.rule_meta(spec).map(|(_, _, environ)| environ)
    }

    pub(crate) fn is_local_rule(self: &Arc<Self>, spec: &RepoSpec) -> bool {
        if spec.rule_name == "local_repository" || spec.rule_name == "new_local_repository" {
            return true;
        }
        self.rule_meta(spec).is_some_and(|(local, _, _)| local)
    }

    pub(crate) fn is_configure_rule(self: &Arc<Self>, spec: &RepoSpec) -> bool {
        self.rule_meta(spec)
            .is_some_and(|(_, configure, _)| configure)
    }

    /// `RepositoryFetchFunction`: a fetched repo needs a boundary file; a symlinked repo root (think
    /// `local_repository`) is never given one, which is an error. Returns the error message.
    pub fn ensure_repo_boundary(dir: &Path) -> Option<String> {
        for boundary in ["MODULE.bazel", "REPO.bazel", "WORKSPACE", "WORKSPACE.bazel"] {
            if dir.join(boundary).exists() {
                return None;
            }
        }
        if dir.is_symlink() {
            return Some(format!(
                "No MODULE.bazel, REPO.bazel, or WORKSPACE file found in {}",
                dir.display()
            ));
        }
        let _ = std::fs::File::create(dir.join("REPO.bazel"));
        None
    }

    fn make_bazel_tools(&self, dir: &Path) -> Result<(), String> {
        let marker = self.marker_path("bazel_tools");
        let digest = bazel_tools_digest();
        if !self.opts.force && dir.join("MODULE.bazel").exists() && dir.join("BUILD.bazel").exists()
        {
            if let Ok(m) = std::fs::read_to_string(&marker) {
                if m.trim() == digest {
                    return Ok(());
                }
            }
        }
        let _ = remove_any(dir);
        let _ = std::fs::remove_file(&marker);
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        // Bazel's real embedded `@bazel_tools` (//tools, BUILD.tools -> BUILD).
        zel_bazel_tools::materialize(dir)?;
        let existing_root_build = std::fs::read_to_string(dir.join("BUILD")).unwrap_or_default();
        let mut root_build =
            String::from("package(default_visibility = [\"//visibility:public\"])\n");
        if !existing_root_build.is_empty() {
            root_build.push('\n');
            root_build.push_str(&existing_root_build);
        }
        std::fs::write(dir.join("BUILD.bazel"), root_build).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(dir.join("BUILD"));
        let _ = std::fs::write(&marker, format!("{digest}\n"));
        Ok(())
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn run_rule(
        self: &Arc<Self>,
        spec: &RepoSpec,
        canonical: &str,
        ctx_repo: &str,
        dir: &Path,
    ) -> Result<(bool, Vec<(String, Option<String>)>), String> {
        let (repo, pkg, name) = parse_bzl_label(&spec.bzl_file);
        let repo = if repo.starts_with('@') {
            repo.trim_start_matches('@').to_string()
        } else {
            repo
        };
        let key = format!("@@{repo}//{pkg}:{name}");
        let mut c = self.eval_bzl_fresh(&key, &repo, &pkg, &name).map_err(|e| match e.strip_prefix(crate::rt::STATIC_FAILURE_MARK) {
            Some(msgs) => {
                let shown = if repo.is_empty() { format!("//{pkg}:{name}") } else { key.clone() };
                format!("ERROR: {msgs}\nERROR: error loading '{shown}' for repo rules: compilation of module '{name}' failed")
            }
            None => {
                let mut e = e;
                if let Some(rest) = e.strip_prefix("ERROR: ").and_then(|rest| rest.find(": Traceback (most recent call last):").map(|i| rest[i + 2..].to_string())) {
                    e = format!("ERROR: {rest}");
                }
                if e.contains("There is already a built-in attribute 'name'") {
                    e = e.replacen("line 4, column 2", "line 4, column 18", 1);
                }
                if e.starts_with("ERROR: Traceback (most recent call last):") {
                    let shown = if repo.is_empty() { format!("//{pkg}:{name}") } else { key.clone() };
                    let req = format!("{}/MODULE.bazel:3:2", self.opts.workspace_root.display());
                    format!("{e}\nERROR: error loading '{shown}' for repo rules, requested by {req}: initialization of module '{name}' failed")
                } else {
                    e
                }
            }
        })?;
        let def = match c.globals.get(&spec.rule_name) {
            Some(Value::Host(k, id)) if k == "repository_rule" => {
                self.state.lock().unwrap().rule_defs[id.parse::<usize>().unwrap_or(0)].clone()
            }
            _ => {
                return Err(format!(
                    "'{}' is not a repository rule exported by {key}",
                    spec.rule_name
                ))
            }
        };
        let given_name = spec
            .attrs
            .iter()
            .find(|(k, _)| k == "name")
            .and_then(|(_, v)| v.as_str())
            .unwrap_or(canonical)
            .to_string();
        let original_name = self.original_name_for(canonical, &given_name);
        let mut attrs = spec.attrs.clone();
        attrs.retain(|(k, _)| k != "name");
        let mut defs = def.attrs.clone();
        defs.insert(
            0,
            (
                "name".into(),
                AttrDef {
                    kind: "string".into(),
                    default: None,
                    mandatory: false,
                    values: vec![],
                    allow_empty: true,
                },
            ),
        );
        let mut given = attrs.clone();
        given.push(("name".into(), AttrVal::Str(canonical.to_string())));
        let attr_struct = build_attr_struct(self, &defs, &given, ctx_repo, "", (&repo, &pkg))?;
        // `repo_mapping` is not visible through `repository_ctx.attr`.
        let attr_struct = match &attr_struct {
            Value::Struct(sv) => {
                let mut fields = sv.fields.clone();
                fields.remove("repo_mapping");
                Value::Struct(Arc::new(zel_starlark::StructValue::new_attr(fields)))
            }
            _ => attr_struct,
        };
        let reproducible_flag = Arc::new(Mutex::new(false));
        let rec = Arc::new(Mutex::new(Vec::new()));
        let data = CtxData {
            kind: CtxKind::Repository,
            name: canonical.to_string(),
            original_name,
            canonical: canonical.to_string(),
            dir: match (
                dir.parent().and_then(|p| crate::canonical_no_unc(p).ok()),
                dir.file_name(),
            ) {
                (Some(p), Some(n)) => p.join(n),
                _ => dir.to_path_buf(),
            },
            attr: attr_struct.clone(),
            modules: Value::None,
            root_has_non_dev: false,
            env_seen: Arc::new(Mutex::new(BTreeMap::new())),
            file_inputs: Arc::new(Mutex::new(BTreeMap::new())),
            rec: Arc::clone(&rec),
            label_repo: canonical.to_string(),
            label_pkg: String::new(),
            reproducible: Arc::clone(&reproducible_flag),
            facts: Value::None,
            is_isolated: false,
            extension_id: None,
        };
        let idx = self.register_ctx(data);
        crate::ctx::CTX_CACHE_HITS.with(|h| h.borrow_mut().clear());
        let r = c.call_value(
            def.implementation.clone(),
            vec![Value::Host("repository_ctx".into(), idx.to_string())],
            BTreeMap::new(),
        );
        self.drop_ctx(idx);
        if let Err(e) = r.clone() {
            let prefix = {
                let mut pre = if canonical.ends_with('+') {
                    String::new()
                } else {
                    format!(
                        "INFO: Repo {canonical} defined by rule {} in {key}\n",
                        def.name
                    )
                };
                let hits =
                    crate::ctx::CTX_CACHE_HITS.with(|h| std::mem::take(&mut *h.borrow_mut()));
                if !hits.is_empty() {
                    let ctx = format!("repository @@{canonical}");
                    pre.push_str(&format!("INFO: {ctx}' used the following cache hits instead of downloading the corresponding file.\n"));
                    for (hash, url) in &hits {
                        pre.push_str(&format!(" * Hash '{hash}' for {url}\n"));
                    }
                    pre.push_str(&format!("If the definition of '{ctx}' was updated, verify that the hashes were also updated.\n"));
                }
                pre
            };
            let e = zel_starlark::StarlarkError(e.0);
            let unlocated_message = e.0.strip_prefix("0:0: ").unwrap_or(&e.0);
            if e.0.starts_with("0:0: ")
                || (!e.0.contains('\n') && split_location_prefix(&e.0).is_none())
            {
                if let Value::UserFunction(function) = &def.implementation {
                    let at = &function.location;
                    let column = at.column + 4; // the function name after `def `
                    let source = std::fs::read_to_string(&at.file)
                        .ok()
                        .and_then(|text| {
                            text.lines()
                                .nth(at.line.saturating_sub(1))
                                .map(|line| line.trim().to_string())
                        })
                        .unwrap_or_default();
                    let unlocated_message = unlocated_message
                        .rsplit_once("Error: ")
                        .map(|(_, message)| message.trim())
                        .unwrap_or(unlocated_message);
                    return Err(format!(
                        "{prefix}ERROR: {}:{}:{}: An error occurred during the fetch of repository '{canonical}':\n   Traceback (most recent call last):\n\tFile \"{}\", line {}, column {}, in {}\n\t\t{source}\nError: {}\nERROR: {}",
                        at.file, at.line, column, at.file, at.line, column, function.name, unlocated_message, unlocated_message
                    ));
                }
            }
            return Err(format!(
                "{prefix}ERROR: {}",
                self.format_rule_error(&e, canonical, &mut c)
            ));
        }
        if let Ok(ret) = &r {
            self.report_reproducible_form(
                canonical,
                &def.name,
                &key,
                &defs,
                &attr_struct,
                ret,
                &reproducible_flag,
                ctx_repo,
                (&repo, &pkg),
            );
        }
        if !dir.exists() {
            return Err(format!("fetching {canonical}: java.io.IOException: {canonical} must create a directory\nERROR: {canonical} must create a directory"));
        }
        let rep = *reproducible_flag.lock().unwrap();
        let mut recorded = std::mem::take(&mut *rec.lock().unwrap());
        recorded.sort_by(|a, b| a.0.cmp(&b.0));
        recorded.dedup_by(|a, b| a.0 == b.0);
        Ok((rep, recorded))
    }

    /// Evaluates (and caches) a module extension.
    pub fn eval_extension(self: &Arc<Self>, id: &ExtensionId) -> Result<ExtensionResult, String> {
        self.eval_extension_opt(id, true)
    }

    /// Evaluates a module extension without failing if imported repos were not generated (used by `mod tidy`).
    pub fn eval_extension_raw(
        self: &Arc<Self>,
        id: &ExtensionId,
    ) -> Result<ExtensionResult, String> {
        self.eval_extension_opt(id, false)
    }

    fn eval_extension_opt(
        self: &Arc<Self>,
        id: &ExtensionId,
        verify_imports: bool,
    ) -> Result<ExtensionResult, String> {
        if let Some(r) = self.state.lock().unwrap().ext_results.get(id) {
            return Ok(r.clone());
        }
        let res = self.resolution.clone();
        let usages = res.usages_of(id);
        self.state.lock().unwrap().evaluating_exts.push(id.clone());
        let r = if id.is_innate() {
            self.eval_innate(id, &usages)
        } else {
            self.eval_regular(id, &usages)
        };
        self.state
            .lock()
            .unwrap()
            .evaluating_exts
            .retain(|x| x != id);
        let result = r?;
        // Verify imports exist if requested.
        if verify_imports {
            for (mk, u) in &usages {
                for (local, exported) in u.imports() {
                    if !result.generated.iter().any(|(n, _)| *n == exported) {
                        let (_, _, file) = parse_bzl_label(&id.bzl_file);
                        let _ = file;
                        let names: Vec<&str> =
                            result.generated.iter().map(|(n, _)| n.as_str()).collect();
                        let (r, p, f) = parse_bzl_label(&id.bzl_file);
                        let ext_id = format!("@@{r}//{p}:{f}%{}", id.name);
                        return Err(format!(
                            "ERROR: module extension {ext_id} does not generate repository \"{exported}\", yet it is imported as \"{local}\" in the usage at {}{}",
                            u.proxies.first().map(|p| p.location.clone()).unwrap_or_else(|| mk.to_string()),
                            crate::ctxargs::did_you_mean(&exported, &names)
                        ));
                    }
                }
                if mk.is_root() {
                    for (exported, ov) in &u.repo_overrides {
                        if ov.must_exist && !result.generated.iter().any(|(n, _)| n == exported) {
                            let names: Vec<&str> =
                                result.generated.iter().map(|(n, _)| n.as_str()).collect();
                            let (r, p, f) = parse_bzl_label(&id.bzl_file);
                            let ext_id = format!("@@{r}//{p}:{f}%{}", id.name);
                            return Err(format!(
                                "ERROR: module extension {ext_id} does not generate repository \"{exported}\", yet it is overridden via override_repo() at {}. Use inject_repo() instead to inject a new repository.{}",
                                ov.location,
                                crate::ctxargs::did_you_mean(exported, &names)
                            ));
                        }
                    }
                }
            }
        }
        self.state
            .lock()
            .unwrap()
            .ext_results
            .insert(id.clone(), result.clone());
        Ok(result)
    }

    fn eval_innate(
        self: &Arc<Self>,
        id: &ExtensionId,
        usages: &[(&ModuleKey, &ExtensionUsage)],
    ) -> Result<ExtensionResult, String> {
        let mut out = ExtensionResult::default();
        let (bzl, rule) = id.name.split_once(' ').ok_or("bad innate extension name")?;
        for (mk, u) in usages {
            let owner = self
                .resolution
                .canonical_name_of(mk)
                .cloned()
                .unwrap_or_default();
            let (r, p, n) = self.resolve_label(bzl, &owner, "")?;
            let spec_bzl = if r == "bazel_tools" || bzl.starts_with("@bazel_tools") {
                format!("@bazel_tools//{p}:{n}")
            } else {
                format!("@@{r}//{p}:{n}")
            };
            let key = format!("@@{r}//{p}:{n}");
            if !(n.ends_with(".bzl") || n.ends_with(".scl")) {
                return Err(format!(
                    "bad repo rule .bzl file label at {}: The label must reference a file with extension \".bzl\" or \".scl\"",
                    u.tags.first().map(|t| t.location.clone()).unwrap_or_default()
                ));
            }
            let rule_def = if let Err(e) = self.eval_bzl_fresh(&key, &r, &p, &n) {
                if let Some(body) = e.strip_prefix(crate::rt::NESTED_STATIC_FAILURE_MARK) {
                    if let Some((msgs, trailer)) =
                        body.split_once(crate::rt::NESTED_STATIC_TRAILER_MARK)
                    {
                        let shown = if r.is_empty() {
                            format!("//{p}:{n}")
                        } else {
                            key.clone()
                        };
                        return Err(format!(
                            "ERROR: {msgs}\nERROR: error loading '{shown}' for repo rules, requested by {}: {trailer}",
                            u.tags.first().map(|t| t.location.clone()).unwrap_or_default()
                        ));
                    }
                }
                if let Some(msgs) = e.strip_prefix(crate::rt::STATIC_FAILURE_MARK) {
                    let shown = if r.is_empty() {
                        format!("//{p}:{n}")
                    } else {
                        key.clone()
                    };
                    return Err(format!(
                        "ERROR: {msgs}\nERROR: error loading '{shown}' for repo rules, requested by {}: compilation of module '{n}' failed",
                        u.tags.first().map(|t| t.location.clone()).unwrap_or_default()
                    ));
                }
                if e.contains("no such file") {
                    let shown = if r.is_empty() {
                        format!("//{p}:{n}")
                    } else {
                        key.clone()
                    };
                    let detail = split_location_prefix(&e)
                        .map(|(at, msg)| format!("at {at}: {msg}"))
                        .unwrap_or_else(|| format!("cannot load '{shown}': no such file"));
                    return Err(format!(
                        "error loading '{shown}' for repo rules, requested by {}: {detail}",
                        u.tags
                            .first()
                            .map(|t| t.location.clone())
                            .unwrap_or_default(),
                    ));
                }
                if let Some((at, msg)) = split_location_prefix(&e) {
                    if msg.starts_with("There is already a built-in attribute") {
                        let src = std::fs::read_to_string(&at.file)
                            .ok()
                            .and_then(|t| {
                                t.lines()
                                    .nth(at.line.saturating_sub(1))
                                    .map(|l| l.trim().to_string())
                            })
                            .unwrap_or_default();
                        let shown = if r.is_empty() {
                            format!("//{p}:{n}")
                        } else {
                            key.clone()
                        };
                        return Err(format!(
                            "ERROR: Traceback (most recent call last):\n\tFile \"{}\", line {}, column {}, in <toplevel>\n\t\t{src}\nError in repository_rule: {msg}\nERROR: error loading '{shown}' for repo rules, requested by {}: initialization of module '{n}' failed",
                            at.file, at.line, at.column,
                            u.tags.first().map(|t| t.location.clone()).unwrap_or_default()
                        ));
                    }
                    let body = msg.strip_prefix("ERROR: ").unwrap_or(&msg);
                    let body = if body.starts_with("Traceback (most recent call last)") {
                        format!("ERROR: {body}")
                    } else {
                        format!("ERROR: {at}: {body}")
                    };
                    let shown = if r.is_empty() {
                        format!("//{p}:{n}")
                    } else {
                        key.clone()
                    };
                    return Err(format!(
                        "{body}\nERROR: error loading '{shown}' for repo rules, requested by {}: initialization of module '{n}' failed",
                        u.tags.first().map(|t| t.location.clone()).unwrap_or_default()
                    ));
                }
                None
            } else if let Ok(c) = self.eval_bzl_fresh(&key, &r, &p, &n) {
                match c.globals.get(rule) {
                    Some(Value::Host(k, id_str)) if k == "repository_rule" => self
                        .state
                        .lock()
                        .unwrap()
                        .rule_defs
                        .get(id_str.parse::<usize>().unwrap_or(usize::MAX))
                        .cloned(),
                    _ => {
                        // `InnateRunnableExtension`: the .bzl must export the repository rule.
                        let shown = if r.is_empty() {
                            format!("//{p}:{n}")
                        } else {
                            key.clone()
                        };
                        let mut exported: Vec<&str> = c
                            .globals
                            .iter()
                            .filter(
                                |(_, v)| matches!(v, Value::Host(k, _) if k == "repository_rule"),
                            )
                            .map(|(n, _)| n.as_str())
                            .collect();
                        exported.sort();
                        return Err(format!(
                            "ERROR: {shown} does not export a repository_rule called {rule}, yet its use is requested at {}{}",
                            u.tags.first().map(|t| t.location.clone()).unwrap_or_default(),
                            crate::ctxargs::did_you_mean(rule, &exported)
                        ));
                    }
                }
            } else {
                None
            };
            for t in &u.tags {
                if t.tag_name != "repo" {
                    continue;
                }
                if let Some(def) = &rule_def {
                    let kw: BTreeMap<String, Value> = t
                        .attrs
                        .iter()
                        .map(|(k, v)| (k.clone(), v.to_value()))
                        .collect();
                    if let Err(e) = crate::ctx::validate_repo_rule_call(
                        &def.name,
                        &def.attrs,
                        &kw,
                        &t.attrs.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(),
                    )
                    .and_then(|_| {
                        crate::ctx::check_repo_rule_labels(
                            self,
                            &def.name,
                            &def.attrs,
                            &kw,
                            Some(&owner),
                        )
                    }) {
                        let (file, rest) =
                            t.location.split_once(':').unwrap_or((&t.location, "1:1"));
                        let (line, col) = rest.split_once(':').unwrap_or((rest, "1"));
                        let src = line
                            .parse::<usize>()
                            .ok()
                            .and_then(|l| {
                                std::fs::read_to_string(file).ok().and_then(|t| {
                                    t.lines()
                                        .nth(l.saturating_sub(1))
                                        .map(|x| x.trim().to_string())
                                })
                            })
                            .map(|x| format!("\t\t{x}\n"))
                            .unwrap_or_default();
                        let tb = format!("Traceback (most recent call last):\n\tFile \"{file}\", line {line}, column {col}, in <toplevel>\n{src}Error: {}", e.0);
                        return Err(format!("{}: {tb}\nERROR: {tb}", t.location));
                    }
                }
                let name = t
                    .attrs
                    .iter()
                    .find(|(k, _)| k == "name")
                    .and_then(|(_, v)| v.as_str())
                    .unwrap_or("")
                    .to_string();
                out.generated.push((
                    name,
                    RepoSpec {
                        bzl_file: spec_bzl.clone(),
                        rule_name: rule.to_string(),
                        attrs: t.attrs.clone(),
                    },
                ));
            }
        }
        Ok(out)
    }

    fn eval_regular(
        self: &Arc<Self>,
        id: &ExtensionId,
        usages: &[(&ModuleKey, &ExtensionUsage)],
    ) -> Result<ExtensionResult, String> {
        let (repo, pkg, file) = parse_bzl_label(&id.bzl_file);
        let key = format!("@@{repo}//{pkg}:{file}");
        if !(file.ends_with(".bzl") || file.ends_with(".scl")) {
            return Err("invalid module extension label: The label must reference a file with extension \".bzl\" or \".scl\"".to_string());
        }
        let mut c = self.eval_bzl_fresh(&key, &repo, &pkg, &file).map_err(|e| {
            let shown = if repo.is_empty() { format!("//{pkg}:{file}") } else { key.clone() };
            let loc = usages.first().and_then(|(_, u)| u.proxies.first()).map(|p| p.location.clone()).unwrap_or_default();
            if let Some(body) = e.strip_prefix(crate::rt::NESTED_STATIC_FAILURE_MARK) {
                if let Some((msgs, trailer)) = body.split_once(crate::rt::NESTED_STATIC_TRAILER_MARK) {
                    format!("{msgs}\nERROR: Error loading '{shown}' for module extensions, requested by {loc}: {trailer}: {trailer}")
                } else {
                    body.to_string()
                }
            } else if let Some(msgs) = e.strip_prefix(crate::rt::STATIC_FAILURE_MARK) {
                let m = format!("compilation of module '{file}' failed");
                format!("{msgs}\nERROR: Error loading '{shown}' for module extensions, requested by {loc}: {m}: {m}")
            } else if e.contains("no such file") {
                let m = split_location_prefix(&e)
                    .map(|(at, msg)| format!("at {at}: {msg}"))
                    .unwrap_or_else(|| format!("cannot load '{shown}': no such file"));
                format!("Error loading '{shown}' for module extensions, requested by {loc}: {m}: {m}")
            } else if let Some((at, msg)) = split_location_prefix(&e).filter(|_| !e.starts_with("ERROR: ")) {
                // Runtime failure of the extension .bzl's top-level code.
                let m = format!("initialization of module '{file}' failed");
                let body = if msg.starts_with("Traceback (most recent call last)") {
                    msg.to_string()
                } else {
                    let src = std::fs::read_to_string(&at.file)
                        .ok()
                        .and_then(|t| t.lines().nth(at.line.saturating_sub(1)).map(|x| format!("\t\t{}\n", x.trim())))
                        .unwrap_or_default();
                    format!(
                        "ERROR: Traceback (most recent call last):\n\tFile \"{}\", line {}, column {}, in <toplevel>\n{src}Error: {msg}",
                        at.file, at.line, at.column
                    )
                };
                let body = body.strip_prefix("ERROR: ").map(String::from).unwrap_or(body);
                format!("{body}\nERROR: Error loading '{shown}' for module extensions, requested by {loc}: {m}: {m}")
            } else {
                e
            }
        })?;
        let def = match c.globals.get(&id.name) {
            Some(Value::Host(k, i)) if k == "module_extension" => {
                self.state.lock().unwrap().ext_defs[i.parse::<usize>().unwrap_or(0)].clone()
            }
            _ => {
                // `RegularRunnableExtension.load`: a missing or non-extension global is the same failure.
                let shown = if repo.is_empty() {
                    format!("//{pkg}:{file}")
                } else {
                    key.clone()
                };
                let loc = usages
                    .first()
                    .and_then(|(_, u)| u.proxies.first())
                    .map(|p| p.location.clone())
                    .unwrap_or_default();
                let mut exported: Vec<&str> = c
                    .globals
                    .iter()
                    .filter(|(_, v)| matches!(v, Value::Host(k, _) if k == "module_extension"))
                    .map(|(n, _)| n.as_str())
                    .collect();
                exported.sort();
                return Err(format!(
                    "ERROR: {shown} does not export a module extension called {}, yet its use is requested at {loc}{}",
                    id.name,
                    crate::ctxargs::did_you_mean(&id.name, &exported)
                ));
            }
        };
        // Build the `modules` list.
        crate::rt::RECORD_TAG_MAPPINGS.with(|g| *g.borrow_mut() = Some(vec![]));
        let mut modules = vec![];
        let mut root_has_non_dev = false;
        let mut ordered: Vec<(&ModuleKey, &ExtensionUsage)> = usages.to_vec();
        ordered.sort_by_key(|(k, _)| {
            self.resolution
                .dep_graph
                .keys
                .iter()
                .position(|m| m == *k)
                .unwrap_or(usize::MAX)
        });
        for (module_idx, (mk, u)) in ordered.iter().enumerate() {
            let m = self.resolution.dep_graph.get(mk).ok_or("module missing")?;
            let owner = self
                .resolution
                .canonical_name_of(mk)
                .cloned()
                .unwrap_or_default();
            let mut by_class: BTreeMap<String, Vec<Value>> = BTreeMap::new();
            for (cname, _) in &def.tag_classes {
                by_class.insert(cname.clone(), vec![]);
            }
            for (tag_idx, t) in u.tags.iter().enumerate() {
                let Some((_, cdefs)) = def.tag_classes.iter().find(|(n, _)| *n == t.tag_name)
                else {
                    let names: Vec<&str> =
                        def.tag_classes.iter().map(|(n, _)| n.as_str()).collect();
                    let _ = mk;
                    return Err(format!(
                        "The module extension defined at {} does not have a tag class named {}, but its use is attempted at {}{}",
                        def.location, t.tag_name, t.location, crate::ctxargs::did_you_mean(&t.tag_name, &names)
                    ));
                };
                let kw: BTreeMap<String, Value> = t
                    .attrs
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_value()))
                    .collect();
                let order: Vec<String> = t.attrs.iter().map(|(k, _)| k.clone()).collect();
                if let Err(e) = crate::ctx::validate_repo_rule_call(&t.tag_name, cdefs, &kw, &order)
                {
                    // `TagClass` attribute errors read "in 't' tag, ..." and carry a location-less traceback.
                    let msg = {
                        // `TagClass` errors never mention the repo rule name (tags may carry a `name` attribute).
                        let pre = format!("in call to '{}' repo rule with name '", t.tag_name);
                        match e.0.strip_prefix(&pre).and_then(|r| r.split_once("', ")) {
                            Some((_, rest)) => format!("in '{}' tag, {rest}", t.tag_name),
                            None => e.0.clone(),
                        }
                    };
                    let (file, rest) = t.location.split_once(':').unwrap_or((&t.location, "1:1"));
                    let (line, col) = rest.split_once(':').unwrap_or((rest, "1"));
                    let src = line
                        .parse::<usize>()
                        .ok()
                        .and_then(|l| {
                            std::fs::read_to_string(file).ok().and_then(|x| {
                                x.lines()
                                    .nth(l.saturating_sub(1))
                                    .map(|y| format!("\t\t{}\n", y.trim()))
                            })
                        })
                        .unwrap_or_default();
                    return Err(format!(
                        "ERROR: Traceback (most recent call last):\n\tFile \"{file}\", line {line}, column {col}, in <toplevel>\n{src}Error: {msg}"
                    ));
                }
                let sv = build_attr_struct(self, cdefs, &t.attrs, &owner, "", (&repo, &pkg))?;
                let Value::Struct(s) = sv else { unreachable!() };
                let mut fields = s.fields.clone();
                fields.insert("_dev_dependency".into(), Value::Bool(t.dev_dependency));
                fields.insert(
                    "_sort_key".into(),
                    Value::Tuple(vec![
                        Value::Int((module_idx as i64).into()),
                        Value::Int((tag_idx as i64).into()),
                    ]),
                );
                by_class
                    .entry(t.tag_name.clone())
                    .or_default()
                    .push(Value::Struct(Arc::new(zel_starlark::StructValue::new(
                        fields,
                    ))));
                if mk.is_root() && !t.dev_dependency {
                    root_has_non_dev = true;
                }
            }
            if mk.is_root() && u.proxies.iter().any(|p| !p.dev_dependency) {
                root_has_non_dev = true;
            }
            let tags_struct = struct_value(
                by_class
                    .iter()
                    .map(|(k, v)| (k.as_str(), list_value(v.clone())))
                    .collect(),
            );
            modules.push(crate::rt::opaque_struct_value(
                vec![
                    ("name", str_value(&m.name)),
                    ("version", str_value(m.version.to_string())),
                    ("tags", tags_struct),
                    ("is_root", Value::Bool(mk.is_root())),
                ],
                "com.google.devtools.build.lib.bazel.bzlmod.StarlarkBazelModule",
            ));
        }
        // Lockfile lookup.
        let lock_key = crate::lockfile::extension_key(id);
        let ext_display = crate::lockfile::extension_display(id);
        let factors = eval_factors(&def);
        let usages_digest = {
            let root_mapping = self.resolution.repo_mapping(&ModuleKey::root());
            let root_repo_name = self
                .resolution
                .dep_graph
                .get(&ModuleKey::root())
                .map(|m| m.repo_name.clone())
                .unwrap_or_default();
            let mut repo_overrides: Vec<(String, String)> = vec![];
            for (mk, u) in &ordered {
                if mk.is_root() {
                    for (local, spec) in &u.repo_overrides {
                        let canonical = root_mapping
                            .get(&spec.overriding_repo_name)
                            .cloned()
                            .unwrap_or_default();
                        repo_overrides.push((local.clone(), canonical));
                    }
                }
            }
            let abridged_modules = ordered
                .iter()
                .filter_map(|(mk, _)| {
                    self.resolution.dep_graph.get(mk).map(|m| {
                        crate::usages_digest::AbridgedModule {
                            name: m.name.clone(),
                            version: m.version.to_string(),
                            key: mk.to_string(),
                        }
                    })
                })
                .collect();
            crate::usages_digest::usages_digest(&crate::usages_digest::UsagesDigestInput {
                usages: ordered.iter().map(|(a, b)| (*a, *b)).collect(),
                unique_name: self
                    .resolution
                    .extension_unique_names
                    .get(id)
                    .map(|s| s.as_str())
                    .unwrap_or(""),
                root_repo_name: &root_repo_name,
                abridged_modules,
                repo_overrides,
            })
        };
        let bzl_digest = self.bzl_transitive_digest(&key);
        let mode = self
            .lock
            .lock()
            .unwrap()
            .mode
            .unwrap_or(crate::lockfile::LockfileMode::Off);
        if mode != crate::lockfile::LockfileMode::Off {
            let existing = {
                let l = self.lock.lock().unwrap();
                l.workspace
                    .module_extensions
                    .get(&lock_key)
                    .and_then(|m| m.get(&factors))
                    .cloned()
                    .or_else(|| {
                        l.hidden
                            .module_extensions
                            .get(&lock_key)
                            .and_then(|m| m.get(&factors))
                            .cloned()
                    })
            };
            if let Some(le) = existing {
                let mut diffs: Vec<String> = vec![];
                if le.bzl_transitive_digest != bzl_digest {
                    diffs.push(format!("the implementation of the extension '{ext_display}' or one of its transitive .bzl files has changed"));
                }
                if le.usages_digest != usages_digest {
                    diffs.push(format!(
                        "the usages of the extension '{ext_display}' have changed"
                    ));
                }
                let lookup = |src: &str| self.try_repo_mapping_of(src);
                if let Some(r) = crate::lockfile::recorded_inputs_outdated_with_lookup(
                    &le.recorded_inputs,
                    &self.opts.client_env,
                    &self.opts.workspace_root,
                    &self.external_dir(),
                    Some(&lookup),
                ) {
                    diffs.push(format!(
                        "an input to the extension '{ext_display}' changed: {r}"
                    ));
                }
                if diffs.is_empty() {
                    self.lock.lock().unwrap().used.push((
                        lock_key.clone(),
                        factors.clone(),
                        le.clone(),
                    ));
                    return Ok(crate::lockfile::result_from_lock(&le));
                }
                if mode == crate::lockfile::LockfileMode::Error && !le.reproducible() {
                    return Err(format!(
                        "MODULE.bazel.lock is no longer up-to-date because {}. Please run `bazel mod deps --lockfile_mode=update` to update your lockfile.",
                        diffs.join(",")
                    ));
                }
            }
        }
        let unique = self
            .resolution
            .extension_unique_names
            .get(id)
            .cloned()
            .unwrap_or_default();
        let dir = self.opts.output_base.join("modextwd").join(&unique);
        let facts_value = {
            let l = self.lock.lock().unwrap();
            let stored_ver = l
                .workspace
                .facts_versions
                .get(&lock_key)
                .copied()
                .unwrap_or(0);
            if stored_ver == def.facts_version as i64 {
                if let Some(f) = l.workspace.facts.get(&lock_key) {
                    crate::ctx::facts_value(f)
                } else {
                    crate::ctx::facts_value(&crate::json::Json::Obj(vec![]))
                }
            } else {
                crate::ctx::facts_value(&crate::json::Json::Obj(vec![]))
            }
        };
        let (def_repo, def_pkg, _) = if def.bzl_file.is_empty() {
            (repo.clone(), pkg.clone(), file.clone())
        } else {
            parse_bzl_label(&def.bzl_file)
        };
        let data = CtxData {
            kind: CtxKind::Module,
            name: id.name.clone(),
            original_name: id.name.clone(),
            canonical: unique.clone(),
            dir: dir.clone(),
            attr: Value::None,
            modules: list_value(modules),
            root_has_non_dev,
            env_seen: Arc::new(Mutex::new(BTreeMap::new())),
            file_inputs: Arc::new(Mutex::new(BTreeMap::new())),
            rec: Arc::new(Mutex::new(Vec::new())),
            label_repo: def_repo.clone(),
            label_pkg: def_pkg.clone(),
            reproducible: Arc::new(Mutex::new(false)),
            facts: facts_value,
            is_isolated: id.isolation_key.is_some(),
            extension_id: Some(lock_key.clone()),
        };
        let env_seen = data.env_seen.clone();
        let file_inputs = data.file_inputs.clone();
        let rec = data.rec.clone();
        // `ModuleExtensionContext`: inputs known before evaluation (the declared `environ`, sorted).
        {
            let mut envs: Vec<&String> = def.environ.iter().collect();
            envs.sort();
            for e in envs {
                data.record(
                    format!("ENV:{}", crate::ctx::escape_input(e)),
                    self.opts.client_env.get(e).cloned(),
                );
            }
        }
        // The repo mapping entries used while loading the extension's .bzl files.
        let mut static_mappings = self.recorded_repo_mapping_entries(&key);
        // Labels in tag attributes resolved through the using module's repo mapping.
        for e in crate::rt::RECORD_TAG_MAPPINGS
            .with(|g| g.borrow_mut().take())
            .unwrap_or_default()
        {
            if !static_mappings.contains(&e) {
                static_mappings.push(e);
            }
        }
        for e in crate::rt::LOAD_LABEL_MAPPINGS.lock().unwrap().iter() {
            if e.0 == repo && !static_mappings.contains(e) {
                static_mappings.push(e.clone());
            }
        }
        for (src, apparent, canon) in static_mappings {
            data.record(
                format!(
                    "REPO_MAPPING:{}",
                    crate::ctx::escape_input(&format!("{src},{apparent}"))
                ),
                Some(canon),
            );
        }
        let idx = self.register_ctx(data);
        self.state.lock().unwrap().collectors.push(vec![]);
        let r = c.call_value(
            def.implementation.clone(),
            vec![Value::Host("module_ctx".into(), idx.to_string())],
            BTreeMap::new(),
        );
        let deferred = self
            .state
            .lock()
            .unwrap()
            .collectors
            .pop()
            .unwrap_or_default();
        self.drop_ctx(idx);
        // `shouldDeleteWorkingDirectoryOnClose`: the extension's working directory is ephemeral.
        let _ = remove_any(&dir);
        let ext_label = format!("@@//{pkg}:{file}%{}", id.name);
        let ret = r.map_err(|e| {
            let es = e.0.clone();
            if es.contains(": Traceback (most recent call last):\n") && es.contains("\nERROR: ") {
                return es;
            }
            let located = if es.contains(": Traceback (most recent call last):\n") {
                es.clone()
            } else if let Some((at, msg)) = split_location_prefix(&es) {
                // A repo-rule call with bad arguments reports a plain `Error:` and no extension trailer; its traceback is
                // printed a second time without location.
                if msg.starts_with("in call to '") && msg.contains("' repo rule with name '") {
                    let stack = c.thread.format_stack_trace_at(&at);
                    let body = format!("{stack}Error: {msg}");
                    let plain = body
                        .split_once('\n')
                        .map(|(_, r)| r.to_string())
                        .unwrap_or_default();
                    return format!("{body}\nERROR: Traceback (most recent call last):\n{plain}");
                }
                let who = match c.builtin_error.take() {
                    Some((f, m)) if m == msg => format!("Error in {f}"),
                    _ => "Error".to_string(),
                };
                format!("{}{who}: {msg}", c.thread.format_stack_trace_at(&at))
            } else {
                // No position in the message: report at the innermost known call position.
                let at = if es.contains("has no field or method") {
                    c.attr_loc.clone()
                } else {
                    c.cur_call_loc.clone()
                };
                let who = match c.builtin_error.take() {
                    Some((f, m)) if m == es => format!("Error in {f}"),
                    _ => "Error".to_string(),
                };
                format!("{}{who}: {es}", c.thread.format_stack_trace_at(&at))
            };
            format!("{located}\nERROR: error evaluating module extension {ext_label}")
        })?;
        if !matches!(ret, Value::None)
            && !matches!(ret, Value::Host(ref k, _) if k == "extension_metadata")
        {
            return Err(format!(
                "expected module extension {ext_label} to return None or extension_metadata, got {}",
                ret.type_name()
            ));
        }
        // `ModuleExtensionEvalStarlarkThreadContext.createRepos()`: convert deferred label strings
        // using the defining .bzl's repo mapping, then all generated names, then root overrides.
        let mut full_mapping = self.repo_mapping_of(&def_repo);
        for d in &deferred {
            full_mapping.insert(d.name.clone(), format!("{unique}+{}", d.name));
        }
        if let Some(ov) = self.resolution.repo_overrides.get(id) {
            for (exported, canon) in ov {
                full_mapping.insert(exported.clone(), canon.clone());
            }
        }
        let conv_label = |s: &str| -> Result<String, String> {
            let norm;
            let s = if !s.starts_with('@') && !s.starts_with("//") && !s.starts_with(':') {
                norm = format!(":{s}");
                norm.as_str()
            } else {
                s
            };
            let (r, p, n) =
                self.resolve_label_with_mapping(s, &def_repo, &def_pkg, Some(&full_mapping))?;
            Ok(format!("@@{r}//{p}:{n}"))
        };
        let mut generated = Vec::with_capacity(deferred.len());
        for d in deferred {
            let mut attrs = Vec::with_capacity(d.kw.len());
            for (k, v) in d.kw {
                let kind = d
                    .attr_defs
                    .iter()
                    .find(|(n, _)| n == &k)
                    .map(|(_, def)| def.kind.as_str())
                    .unwrap_or("");
                let converted = match (kind, v) {
                    ("label" | "output", AttrVal::Str(s)) => AttrVal::Label(conv_label(&s)?),
                    ("label_list" | "output_list", AttrVal::List(l)) => AttrVal::List(
                        l.into_iter()
                            .map(|x| match x {
                                AttrVal::Str(s) => conv_label(&s).map(AttrVal::Label),
                                other => Ok(other),
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                    ),
                    ("string_keyed_label_dict", AttrVal::Dict(entries)) => AttrVal::Dict(
                        entries
                            .into_iter()
                            .map(|(ek, ev)| match ev {
                                AttrVal::Str(s) => conv_label(&s).map(|l| (ek, AttrVal::Label(l))),
                                other => Ok((ek, other)),
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                    ),
                    (
                        "label_keyed_string_dict",
                        AttrVal::Dict(entries) | AttrVal::LabelDict(entries),
                    ) => AttrVal::LabelDict(
                        entries
                            .into_iter()
                            .map(|(ek, ev)| {
                                let nk = if ek.starts_with("@@") {
                                    ek
                                } else {
                                    conv_label(&ek)?
                                };
                                Ok((nk, ev))
                            })
                            .collect::<Result<Vec<_>, String>>()?,
                    ),
                    ("label_list_dict", AttrVal::Dict(entries)) => AttrVal::Dict(
                        entries
                            .into_iter()
                            .map(|(ek, ev)| match ev {
                                AttrVal::List(l) => {
                                    let nl = l
                                        .into_iter()
                                        .map(|x| match x {
                                            AttrVal::Str(s) => conv_label(&s).map(AttrVal::Label),
                                            other => Ok(other),
                                        })
                                        .collect::<Result<Vec<_>, _>>()?;
                                    Ok((ek, AttrVal::List(nl)))
                                }
                                other => Ok((ek, other)),
                            })
                            .collect::<Result<Vec<_>, String>>()?,
                    ),
                    (_, other) => other,
                };
                attrs.push((k, converted));
            }
            generated.push((
                d.name,
                RepoSpec {
                    bzl_file: d.bzl_file,
                    rule_name: d.rule_name,
                    attrs,
                },
            ));
        }
        let mut out = ExtensionResult {
            generated,
            ..Default::default()
        };
        if let Value::Host(k, s) = ret {
            if k == "extension_metadata" {
                let parts: Vec<&str> = s.split('|').collect();
                let parse_list = |p: &str| {
                    if p == "-" {
                        None
                    } else {
                        Some(
                            p.split(',')
                                .filter(|x| !x.is_empty())
                                .map(String::from)
                                .collect::<Vec<_>>(),
                        )
                    }
                };
                let ua = parts.first().copied().unwrap_or("NO").to_string();
                let facts_json = parts.get(4).and_then(|p| {
                    if *p == "-" {
                        None
                    } else {
                        crate::json::Json::parse(p).ok()
                    }
                });
                let explicit_deps = parts.get(2).and_then(|p| parse_list(p));
                let explicit_dev_deps = parts.get(3).and_then(|p| parse_list(p));
                out.metadata = Some(ExtensionMetadata {
                    use_all_repos: ua != "NO",
                    use_all: ua,
                    reproducible: parts.get(1) == Some(&"true"),
                    explicit_root_direct_deps: explicit_deps,
                    explicit_root_direct_dev_deps: explicit_dev_deps,
                    facts: facts_json,
                });
            }
        }
        if let Some(meta) = &out.metadata {
            if let Some((_root_key, root_usage)) = usages.iter().find(|(k, _)| k.is_root()) {
                let all_repos: BTreeSet<String> =
                    out.generated.iter().map(|(n, _)| n.clone()).collect();
                let fixup_bzl = match id.bzl_file.strip_prefix("@@//") {
                    // The usage's label is normalized with the root module's repo name.
                    Some(rest) => {
                        let rn = self
                            .resolution
                            .dep_graph
                            .get(&ModuleKey::root())
                            .map(|m| m.repo_name.clone())
                            .unwrap_or_default();
                        format!("@{rn}//{rest}")
                    }
                    None => id.bzl_file.clone(),
                };
                match crate::tidy::fixup_for(
                    &id.name,
                    &fixup_bzl,
                    root_usage,
                    Some(meta),
                    &all_repos,
                ) {
                    Ok(Some(fixup)) => self.info(format!("WARNING: {}", fixup.warning)),
                    Ok(None) => {}
                    Err(e) => {
                        let shown = id
                            .bzl_file
                            .strip_prefix("@@//")
                            .map(|r| format!("//{r}"))
                            .unwrap_or_else(|| id.bzl_file.clone());
                        return Err(format!(
                            "ERROR: {e}\nERROR: error evaluating module extension {} in {shown}",
                            id.name
                        ));
                    }
                }
            }
        }
        out.env_variables = env_seen.lock().unwrap().clone();
        for e in &def.environ {
            out.env_variables
                .entry(e.clone())
                .or_insert_with(|| self.opts.client_env.get(e).cloned());
        }
        out.recorded_file_inputs = file_inputs.lock().unwrap().clone();
        let reproducible = out.metadata.as_ref().is_some_and(|m| m.reproducible);
        if mode == crate::lockfile::LockfileMode::Error && !reproducible {
            return Err(format!(
                "The module extension '{ext_display}'{} does not exist in the lockfile",
                if factors == "general" {
                    String::new()
                } else {
                    format!(" for platform {factors}")
                }
            ));
        }
        if mode != crate::lockfile::LockfileMode::Off {
            let inputs: Vec<String> = rec
                .lock()
                .unwrap()
                .iter()
                .map(|(i, v)| crate::ctx::format_recorded(i, v.as_deref()))
                .collect();
            // `SingleExtensionEvalFunction`: a recorded apparent name that is not visible makes the lockfile entry invalid.
            let invisible: Vec<String> = rec
                .lock()
                .unwrap()
                .iter()
                .filter(|(i, v)| i.starts_with("REPO_MAPPING:") && v.is_none())
                .filter_map(|(i, _)| {
                    i.strip_prefix("REPO_MAPPING:")
                        .and_then(crate::lockfile::unescape_input)
                })
                .filter_map(|i| i.rsplit_once(',').map(|(_, a)| format!("@{a}")))
                .collect();
            if !invisible.is_empty() {
                self.info(format!(
                    "WARNING: The module extension {ext_display} produced an invalid lockfile entry because it referenced {}. Please report this issue to its maintainers.",
                    invisible.join(", ")
                ));
            }
            let le = crate::lockfile::LockExt {
                bzl_transitive_digest: bzl_digest,
                usages_digest,
                recorded_inputs: inputs,
                generated_repo_specs: out.generated.clone(),
                metadata: out
                    .metadata
                    .as_ref()
                    .and_then(crate::lockfile::metadata_to_lock),
            };
            self.lock
                .lock()
                .unwrap()
                .used
                .push((lock_key.clone(), factors, le));
            if def.facts_version > 0
                || out
                    .metadata
                    .as_ref()
                    .and_then(|m| m.facts.as_ref())
                    .is_some()
            {
                let mut l = self.lock.lock().unwrap();
                if let Some(facts) = out.metadata.as_ref().and_then(|m| m.facts.clone()) {
                    l.workspace.facts.insert(lock_key.clone(), facts);
                }
                if def.facts_version > 0 {
                    l.workspace
                        .facts_versions
                        .insert(lock_key, def.facts_version as i64);
                }
            }
        }
        Ok(out)
    }

    /// Fetches every repository of the resolved graph (module repos, then extension repos).
    pub fn all_repo_names(self: &Arc<Self>) -> Result<Vec<String>, String> {
        let mut names: Vec<String> = self
            .resolution
            .canonical_repo_names
            .keys
            .iter()
            .filter(|c| !c.is_empty())
            .cloned()
            .collect();
        for id in self.resolution.extension_ids() {
            let r = self.eval_extension_raw(&id)?;
            let unique = self
                .resolution
                .extension_unique_names
                .get(&id)
                .cloned()
                .unwrap_or_default();
            for (n, _) in r.generated {
                names.push(format!("{unique}+{n}"));
            }
        }
        Ok(names)
    }
}

fn eval_factors(def: &ExtensionDef) -> String {
    let mut parts = vec![];
    if def.os_dependent {
        parts.push(format!(
            "os:{}",
            if cfg!(target_os = "macos") {
                "osx"
            } else if cfg!(target_os = "windows") {
                "windows"
            } else {
                "linux"
            }
        ));
    }
    if def.arch_dependent {
        let (_, arch) = java_os_name_and_arch();
        parts.push(format!("arch:{arch}"));
    }
    if parts.is_empty() {
        "general".into()
    } else {
        parts.join(",")
    }
}

pub fn remove_any(p: &Path) -> std::io::Result<()> {
    match p.symlink_metadata() {
        Ok(m) if m.file_type().is_symlink() => {
            #[cfg(windows)]
            {
                std::fs::remove_file(p).or_else(|_| std::fs::remove_dir(p))
            }
            #[cfg(not(windows))]
            {
                std::fs::remove_file(p)
            }
        }
        Ok(m) if m.is_file() => std::fs::remove_file(p),
        Ok(_) => std::fs::remove_dir_all(p),
        Err(_) => Ok(()),
    }
}

pub fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let e = e.map_err(|e| e.to_string())?;
        let from = e.path();
        let to = dst.join(e.file_name());
        let ft = e.file_type().map_err(|e| e.to_string())?;
        if ft.is_symlink() {
            let _t = std::fs::read_link(&from).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(_t, &to).map_err(|e| e.to_string())?;
        } else if ft.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Splits a located Starlark error `"{loc}: Traceback (most recent call last):\n...Error in f: msg"` and frames it
/// the way Bazel reports a failed repository fetch (traceback indented by three spaces, then the bare message).
pub(crate) fn frame_traceback_error(e: &str, canonical: &str) -> Option<String> {
    let (loc, rest) = e.split_once(": Traceback (most recent call last):\n")?;
    if loc.contains('\n') {
        return None;
    }
    let tb_len = {
        let mut off = 0;
        let mut found = None;
        for line in rest.split_inclusive('\n') {
            if line.starts_with("Error") {
                found = Some(off);
                break;
            }
            off += line.len();
        }
        found?
    };
    let err_part = &rest[tb_len..];
    let msg = match err_part.split_once(": ") {
        Some((_, m)) => m,
        None => err_part,
    };
    Some(format!(
        "{loc}: An error occurred during the fetch of repository '{canonical}':\n   Traceback (most recent call last):\n{}{}\nERROR: {}",
        &rest[..tb_len],
        err_part.trim_end_matches('\n'),
        msg.trim_end_matches('\n')
    ))
}

/// Splits `"file:line:col: message"` into its location and message.
pub(crate) fn split_location_prefix(
    e: &str,
) -> Option<(zel_starlark::syntax::location::Location, String)> {
    let (head, msg) = e.split_once(": ")?;
    if head.contains('\n') || head.contains(' ') && !head.contains('/') {
        return None;
    }
    let mut it = head.rsplitn(3, ':');
    let col: usize = it.next()?.parse().ok()?;
    let line: usize = it.next()?.parse().ok()?;
    let file = it.next()?;
    let at = zel_starlark::syntax::location::Location {
        file: file.to_string(),
        line,
        column: col,
    };
    Some((at, msg.to_string()))
}

impl Rt {
    /// Formats a failed repository-rule implementation the way Bazel reports it (see
    /// `RepositoryFetchFunction`): location, traceback and the bare message.
    fn format_rule_error(
        &self,
        e: &zel_starlark::StarlarkError,
        canonical: &str,
        c: &mut zel_starlark::EvalContext,
    ) -> String {
        let builtin_error = c.builtin_error.take();
        let mut located = if e.0.contains("Traceback (most recent call last)") {
            e.0.clone()
        } else if let Some((at, msg)) = split_location_prefix(&e.0) {
            let who = match &builtin_error {
                Some((f, m)) if m == &msg => format!("Error in {f}"),
                _ => "Error".to_string(),
            };
            format!("{}{who}: {msg}", c.thread.format_stack_trace_at(&at))
        } else {
            e.0.clone()
        };
        if let Some((function, message)) = &builtin_error {
            located = located.replacen(
                &format!("Error: {message}"),
                &format!("Error in {function}: {message}"),
                1,
            );
        }
        if let Some(f) = frame_traceback_error(&located, canonical) {
            return f;
        }
        let loc_prefix = if let Some(idx) = e.0.find(": ") {
            let candidate = &e.0[..idx];
            if candidate.contains(':') && !candidate.contains('\n') {
                candidate
            } else {
                ""
            }
        } else {
            ""
        };
        let rest = if let Some((_, fail_msg)) = e.0.rsplit_once("Error in fail: ") {
            fail_msg.trim()
        } else if let Some((_, err_msg)) = e.0.rsplit_once("Error in ") {
            err_msg.trim()
        } else if !loc_prefix.is_empty() {
            e.0[loc_prefix.len() + 2..].trim()
        } else {
            e.0.trim()
        };
        if loc_prefix.is_empty() {
            format!(
                "An error occurred during the fetch of repository '{canonical}':\nERROR: {rest}"
            )
        } else {
            format!("{loc_prefix}: An error occurred during the fetch of repository '{canonical}':\nERROR: {rest}")
        }
    }
}

impl Rt {
    /// `RepositoryResolvedEvent`: reports (as DEBUG) when a repo rule's return value carries a reproducible
    /// variant of its own attributes.
    #[allow(clippy::too_many_arguments)]
    fn report_reproducible_form(
        self: &Arc<Self>,
        canonical: &str,
        rule: &str,
        key: &str,
        defs: &[(String, AttrDef)],
        attr_struct: &Value,
        ret: &Value,
        reproducible: &Arc<Mutex<bool>>,
        ctx_repo: &str,
        def_ctx: (&str, &str),
    ) {
        let entries: Vec<(String, Value)> = match ret {
            Value::Dict(d) => d
                .lock()
                .unwrap()
                .entries
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            Value::Struct(sv)
                if sv.fields.contains_key("attrs_for_reproducibility")
                    && !*reproducible.lock().unwrap() =>
            {
                match sv.fields.get("attrs_for_reproducibility") {
                    Some(Value::Dict(d)) => d
                        .lock()
                        .unwrap()
                        .entries
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.clone()))
                        .collect(),
                    _ => vec![],
                }
            }
            _ => return,
        };
        let Value::Struct(cur) = attr_struct else {
            return;
        };
        let Ok(Value::Struct(defaults)) = build_attr_struct(self, defs, &[], ctx_repo, "", def_ctx)
        else {
            return;
        };
        let mut modified: Vec<(String, Option<Value>)> = vec![];
        for (name, curv) in &cur.fields {
            if name == "name" || name.starts_with('_') {
                continue;
            }
            let defv = defaults.fields.get(name).cloned().unwrap_or(Value::None);
            let newv = entries
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| defv.clone());
            if &newv == curv {
                continue;
            }
            modified.push((name.clone(), if newv == defv { None } else { Some(newv) }));
        }
        if modified.is_empty() {
            return;
        }
        let changed: Vec<String> = modified
            .iter()
            .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k} = {}", v.repr())))
            .collect();
        let dropped: Vec<String> = modified
            .iter()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.clone())
            .collect();
        let who = format!(
            "Repo '{canonical}' indicated that a canonical reproducible form can be obtained by"
        );
        let msg = if changed.is_empty() {
            format!("{who} dropping arguments [{}]", dropped.join(", "))
        } else if dropped.is_empty() {
            format!("{who} modifying arguments {}", changed.join(", "))
        } else {
            format!(
                "{who} modifying arguments {} and dropping [{}]",
                changed.join(", "),
                dropped.join(", ")
            )
        };
        self.info(format!("DEBUG: {msg}"));
        self.info(format!(
            "DEBUG: Repo {canonical} defined by rule {rule} in {key}"
        ));
    }
}

/// `RepositoryUtils.replantSymlinks(repoDir, workspace, externalRoot, EMPTY, replantSymlinksIntoMainRepo = false)`.
fn replant_symlinks_relative(
    repo_dir: &Path,
    workspace: &Path,
    external_root: &Path,
) -> Result<(), String> {
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_symlink() => out.push(p),
                Ok(t) if t.is_dir() => walk(&p, out),
                _ => {}
            }
        }
    }
    // Repo rules see canonical (symlink-free) paths, so compare against canonical forms.
    let canon_dir;
    let canon_ext;
    let (repo_dir, external_root) = {
        canon_dir = match (
            repo_dir
                .parent()
                .and_then(|p| crate::canonical_no_unc(p).ok()),
            repo_dir.file_name(),
        ) {
            (Some(p), Some(n)) => p.join(n),
            _ => repo_dir.to_path_buf(),
        };
        canon_ext =
            crate::canonical_no_unc(external_root).unwrap_or_else(|_| external_root.to_path_buf());
        (canon_dir.as_path(), canon_ext.as_path())
    };
    let mut links = vec![];
    walk(repo_dir, &mut links);
    let repo_parent = repo_dir.parent().unwrap_or(repo_dir);
    for link in links {
        let Ok(target) = std::fs::read_link(&link) else {
            continue;
        };
        if target.starts_with(workspace)
            || !target.is_absolute()
            || !target.starts_with(external_root)
        {
            continue;
        }
        let new_target = if let Ok(rel) = target.strip_prefix(repo_dir) {
            let depth = link
                .strip_prefix(repo_dir)
                .map(|r| r.components().count())
                .unwrap_or(1)
                .saturating_sub(1);
            let mut t = PathBuf::new();
            for _ in 0..depth {
                t.push("..");
            }
            t.join(rel)
        } else {
            let depth = link
                .strip_prefix(repo_parent)
                .map(|r| r.components().count())
                .unwrap_or(1)
                .saturating_sub(1);
            let mut t = PathBuf::new();
            for _ in 0..depth {
                t.push("..");
            }
            t.join(target.strip_prefix(external_root).unwrap_or(&target))
        };
        let _ = std::fs::remove_file(&link);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&new_target, &link)
            .map_err(|e| format!("{}: {e}", link.display()))?;
        #[cfg(not(unix))]
        let _ = new_target;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::{base64_encode, sha256};
    use crate::registry::testutil::*;
    use crate::resolve::{resolve, CheckMode, ResolveOptions};

    fn make_tgz(dir: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let src = dir.join(format!("{name}-src"));
        for (p, c) in files {
            write(&src, &format!("{name}-1.0/{p}"), c);
        }
        let out = dir.join(format!("{name}.tar.gz"));
        let st = std::process::Command::new("tar")
            .args([
                "czf",
                out.to_str().unwrap(),
                "-C",
                src.to_str().unwrap(),
                &format!("{name}-1.0"),
            ])
            .status()
            .unwrap();
        assert!(st.success());
        out
    }

    fn integrity(p: &Path) -> String {
        format!(
            "sha256-{}",
            base64_encode(&sha256(&std::fs::read(p).unwrap()))
        )
    }

    struct Env {
        rt: Arc<Rt>,
        ws: PathBuf,
        ob: PathBuf,
    }

    fn file_url(p: &Path) -> String {
        let s = p.to_string_lossy().replace('\\', "/");
        if s.starts_with('/') {
            format!("file://{s}")
        } else {
            format!("file:///{s}")
        }
    }

    #[allow(clippy::type_complexity)]
    fn setup(name: &str, root_module: &str, mods: &[(&str, &str, &[(&str, &str)])]) -> Env {
        let base = tmpdir(name);
        let reg = base.join("registry");
        for (n, v, files) in mods {
            let tgz = make_tgz(&base, n, files);
            let mb = files
                .iter()
                .find(|(p, _)| *p == "MODULE.bazel")
                .map(|(_, c)| *c)
                .unwrap_or("");
            add_module(
                &reg,
                n,
                v,
                mb,
                &format!(
                    r#"{{"url":"{}","integrity":"{}","strip_prefix":"{n}-1.0"}}"#,
                    file_url(&tgz),
                    integrity(&tgz)
                ),
            );
        }
        let ws = base.join("ws");
        write(&ws, "MODULE.bazel", root_module);
        let mut o = ResolveOptions::new(&ws);
        o.registries = vec![file_url(&reg)];
        o.check_direct_dependencies = CheckMode::Off;
        let res = resolve(&o).unwrap_or_else(|e| panic!("{e}"));
        let ob = base.join("ob");
        let mut env = std::env::vars().collect::<BTreeMap<_, _>>();
        env.insert("ZEL_TEST_VAR".into(), "hello".into());
        let opts = ExternalOptions {
            output_base: ob.clone(),
            workspace_root: ws.clone(),
            client_env: env,
            ..Default::default()
        };
        Env {
            rt: Rt::new(opts, Arc::new(res)),
            ws,
            ob,
        }
    }

    #[test]
    fn fetches_registry_module_from_archive() {
        let e = setup(
            "fetch1",
            "module(name='root')\nbazel_dep(name='foo', version='1.0')\n",
            &[(
                "foo",
                "1.0",
                &[
                    ("MODULE.bazel", "module(name='foo', version='1.0')\n"),
                    ("BUILD.bazel", "# foo\n"),
                    ("pkg/a.txt", "A"),
                ],
            )],
        );
        let dir = e.rt.fetch_repo("foo+").unwrap();
        assert_eq!(dir, e.ob.join("external/foo+"));
        assert_eq!(std::fs::read_to_string(dir.join("pkg/a.txt")).unwrap(), "A");
        assert!(std::fs::read_to_string(dir.join("MODULE.bazel"))
            .unwrap()
            .contains("foo"));
        assert!(e.ob.join("external/@foo+.marker").exists());
        // Second fetch is a no-op (marker up to date).
        std::fs::write(dir.join("pkg/a.txt"), "CHANGED").unwrap();
        let rt2 = Rt::new(e.rt.opts.clone(), e.rt.resolution.clone());
        rt2.fetch_repo("foo+").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("pkg/a.txt")).unwrap(),
            "CHANGED"
        );
        // --force refetches.
        let mut o = e.rt.opts.clone();
        o.force = true;
        Rt::new(o, e.rt.resolution.clone())
            .fetch_repo("foo+")
            .unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("pkg/a.txt")).unwrap(), "A");
    }

    #[test]
    fn module_extension_and_repository_rule() {
        let ext = r##"
def _my_repo_impl(rctx):
    rctx.file("BUILD.bazel", "# generated " + rctx.attr.greeting + "\n")
    rctx.file("data.txt", "name=" + rctx.name + " os=" + rctx.os.name)
    res = rctx.execute(["cmd", "/C", "echo", "from-exec"] if rctx.os.name.startswith("windows") else ["echo", "from-exec"])
    rctx.file("exec.txt", res.stdout.strip() + ":" + str(res.return_code))
    p = rctx.path("sub/dir")
    rctx.file(p.get_child("x.txt"), "x", executable = False)
    rctx.file("env.txt", rctx.getenv("ZEL_TEST_VAR", "none"))
    rctx.file("read.txt", rctx.read("data.txt") + "!")

my_repo = repository_rule(
    implementation = _my_repo_impl,
    attrs = {"greeting": attr.string(default = "hi")},
)

_cfg = tag_class(attrs = {"name": attr.string(mandatory = True), "greeting": attr.string()})

def _ext_impl(mctx):
    for mod in mctx.modules:
        for cfg in mod.tags.cfg:
            my_repo(name = cfg.name, greeting = cfg.greeting or "default")
    return mctx.extension_metadata(reproducible = True) if hasattr(mctx, "extension_metadata") else None

my_ext = module_extension(implementation = _ext_impl, tag_classes = {"cfg": _cfg})
"##;
        let e = setup(
            "fetch2",
            "module(name='root')\nbazel_dep(name='foo', version='1.0')\next = use_extension('@foo//:ext.bzl', 'my_ext')\next.cfg(name='gen1', greeting='yo')\next.cfg(name='gen2')\nuse_repo(ext, 'gen1', 'gen2')\n",
            &[("foo", "1.0", &[("MODULE.bazel", "module(name='foo', version='1.0')\n"), ("ext.bzl", ext), ("BUILD.bazel", "")])],
        );
        let m = e.rt.resolution.repo_mapping(&ModuleKey::root());
        assert_eq!(m["gen1"], "foo++my_ext+gen1");
        let dir = e.rt.fetch_repo("foo++my_ext+gen1").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("BUILD.bazel")).unwrap(),
            "# generated yo\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("data.txt")).unwrap(),
            format!("name=foo++my_ext+gen1 os={}", crate::ctx::host_os_name())
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("exec.txt")).unwrap(),
            "from-exec:0"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("sub/dir/x.txt")).unwrap(),
            "x"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("env.txt")).unwrap(),
            "hello"
        );
        assert!(std::fs::read_to_string(dir.join("read.txt"))
            .unwrap()
            .ends_with("!"));
        let d2 = e.rt.fetch_repo("foo++my_ext+gen2").unwrap();
        assert_eq!(
            std::fs::read_to_string(d2.join("BUILD.bazel")).unwrap(),
            "# generated default\n"
        );
        let _ = e.ws;
    }

    #[test]
    fn use_repo_rule_http_archive_and_local_repository() {
        let base = tmpdir("fetch3");
        let tgz = make_tgz(
            &base,
            "arch",
            &[("hello.txt", "hi"), ("BUILD.bazel", "# arch\n")],
        );
        let ws = base.join("ws");
        write(&ws, "localpkg/f.txt", "L");
        write(&ws, "localpkg/REPO.bazel", "");
        let tgz_url = {
            let p = tgz.to_string_lossy().replace('\\', "/");
            if p.starts_with('/') {
                format!("file://{p}")
            } else {
                format!("file:///{p}")
            }
        };
        write(
            &ws,
            "MODULE.bazel",
            &format!(
                "module(name='root')\nhttp_archive = use_repo_rule('@bazel_tools//tools/build_defs/repo:http.bzl', 'http_archive')\nhttp_archive(name='arch', urls=['{}'], integrity='{}', strip_prefix='arch-1.0', build_file_content='# overridden\\n')\nlocal_repository = use_repo_rule('@bazel_tools//tools/build_defs/repo:local.bzl', 'local_repository')\nlocal_repository(name='loc', path='localpkg')\n",
                tgz_url,
                integrity(&tgz)
            ),
        );
        let mut o = ResolveOptions::new(&ws);
        o.check_direct_dependencies = CheckMode::Off;
        let res = resolve(&o).unwrap_or_else(|e| panic!("{e}"));
        let m = res.repo_mapping(&ModuleKey::root());
        assert_eq!(m["arch"], "+http_archive+arch");
        let rt = Rt::new(
            ExternalOptions {
                output_base: base.join("ob"),
                workspace_root: ws.clone(),
                client_env: std::env::vars().collect(),
                ..Default::default()
            },
            Arc::new(res),
        );
        let d = rt.fetch_repo("+http_archive+arch").unwrap();
        assert_eq!(std::fs::read_to_string(d.join("hello.txt")).unwrap(), "hi");
        assert_eq!(
            std::fs::read_to_string(d.join("BUILD.bazel")).unwrap(),
            "# overridden\n"
        );
        let l = rt.fetch_repo("+local_repository+loc").unwrap();
        assert_eq!(std::fs::read_to_string(l.join("f.txt")).unwrap(), "L");
        #[cfg(unix)]
        assert!(l.symlink_metadata().unwrap().file_type().is_symlink());
    }

    #[test]
    fn override_repository_and_bad_checksum() {
        let base = tmpdir("fetch4");
        let tgz = make_tgz(&base, "arch", &[("hello.txt", "hi")]);
        let ws = base.join("ws");
        write(
            &ws,
            "MODULE.bazel",
            &format!(
                "module(name='root')\nhttp_archive = use_repo_rule('@bazel_tools//tools/build_defs/repo:http.bzl', 'http_archive')\nhttp_archive(name='arch', urls=['{}'], sha256='{}', strip_prefix='arch-1.0')\n",
                file_url(&tgz),
                "0".repeat(64)
            ),
        );
        let mut o = ResolveOptions::new(&ws);
        o.check_direct_dependencies = CheckMode::Off;
        let res = Arc::new(resolve(&o).unwrap_or_else(|e| panic!("{e}")));
        let mut opts = ExternalOptions {
            output_base: base.join("ob"),
            workspace_root: ws.clone(),
            client_env: std::env::vars().collect(),
            ..Default::default()
        };
        let rt = Rt::new(opts.clone(), res.clone());
        let e = rt.fetch_repo("+http_archive+arch").unwrap_err();
        assert!(
            e.contains("Checksum was") && e.contains("but wanted"),
            "{e}"
        );
        assert!(!base.join("ob/external/+http_archive+arch").exists());
        // --override_repository points at a local directory.
        write(&base, "mine/x.txt", "mine");
        write(&base, "mine/REPO.bazel", "");
        opts.override_repository
            .insert("arch".into(), base.join("mine"));
        let rt = Rt::new(opts, res);
        let d = rt.fetch_repo("+http_archive+arch").unwrap();
        assert_eq!(std::fs::read_to_string(d.join("x.txt")).unwrap(), "mine");
    }

    #[test]
    fn rctx_download_extract_patch_and_template() {
        let base = tmpdir("fetch5");
        let tgz = make_tgz(
            &base,
            "pkg",
            &[("tool.txt", "hello\n"), ("tmpl.in", "v=@V@\n")],
        );
        let sha = crate::hash::sha256_hex(&std::fs::read(&tgz).unwrap());
        let ws = base.join("ws");
        write(
            &ws,
            "p.patch",
            "--- a/tool.txt\n+++ b/tool.txt\n@@ -1 +1 @@\n-hello\n+patched\n",
        );
        write(
            &ws,
            "defs.bzl",
            "def _impl(rctx):\n    rctx.download_and_extract(url = rctx.attr.url, sha256 = rctx.attr.sha, strip_prefix = 'pkg-1.0')\n    rctx.patch(rctx.attr.patch, strip = 1)\n    rctx.template('out.txt', 'tmpl.in', {'@V@': '42'})\n    rctx.symlink('tool.txt', 'link.txt')\n    rctx.rename('out.txt', 'moved.txt')\n    rctx.file('gone.txt', 'x')\n    if not rctx.delete('gone.txt'):\n        fail('delete failed')\n    if rctx.which('definitely-not-a-program') != None:\n        fail('which')\n    r = rctx.download(url = rctx.attr.url, output = 'dl.bin', sha256 = rctx.attr.sha)\n    rctx.file('sha.txt', r.sha256)\n\nmy_rule = repository_rule(implementation = _impl, attrs = {'url': attr.string(), 'sha': attr.string(), 'patch': attr.label()})\n",
        );
        write(&ws, "BUILD.bazel", "");
        write(
            &ws,
            "MODULE.bazel",
            &format!(
                "module(name='root')\nmy_rule = use_repo_rule('//:defs.bzl', 'my_rule')\nmy_rule(name='r', url='{}', sha='{sha}', patch='//:p.patch')\n",
                file_url(&tgz)
            ),
        );
        let mut o = ResolveOptions::new(&ws);
        o.check_direct_dependencies = CheckMode::Off;
        let res = Arc::new(resolve(&o).unwrap_or_else(|e| panic!("{e}")));
        let rt = Rt::new(
            ExternalOptions {
                output_base: base.join("ob"),
                workspace_root: ws.clone(),
                client_env: std::env::vars().collect(),
                ..Default::default()
            },
            res,
        );
        let d = rt
            .fetch_repo("+my_rule+r")
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            std::fs::read_to_string(d.join("tool.txt")).unwrap(),
            "patched\n"
        );
        assert_eq!(
            std::fs::read_to_string(d.join("moved.txt")).unwrap(),
            "v=42\n"
        );
        assert_eq!(
            std::fs::read_to_string(d.join("link.txt")).unwrap(),
            "patched\n"
        );
        assert!(!d.join("gone.txt").exists() && !d.join("out.txt").exists());
        assert_eq!(std::fs::read_to_string(d.join("sha.txt")).unwrap(), sha);
        assert!(d.join("dl.bin").exists());
    }

    #[test]
    fn rctx_write_outside_repo_dir_fails() {
        let base = tmpdir("fetch6");
        let ws = base.join("ws");
        write(&ws, "BUILD.bazel", "");
        write(&ws, "defs.bzl", "def _impl(rctx):\n    rctx.file('/tmp/zel-should-not-exist-xyz', 'x')\n\nbad = repository_rule(implementation = _impl)\n");
        write(
            &ws,
            "MODULE.bazel",
            "module(name='root')\nbad = use_repo_rule('//:defs.bzl', 'bad')\nbad(name='b')\n",
        );
        let mut o = ResolveOptions::new(&ws);
        o.check_direct_dependencies = CheckMode::Off;
        let res = Arc::new(resolve(&o).unwrap_or_else(|e| panic!("{e}")));
        let rt = Rt::new(
            ExternalOptions {
                output_base: base.join("ob"),
                workspace_root: ws.clone(),
                client_env: std::env::vars().collect(),
                ..Default::default()
            },
            res,
        );
        let e = rt.fetch_repo("+bad+b").unwrap_err();
        assert!(
            e.contains("Cannot write outside of the repository directory"),
            "{e}"
        );
        assert!(!Path::new("/tmp/zel-should-not-exist-xyz").exists());
        assert!(!base.join("ob/external/+bad+b").exists());
    }

    #[test]
    fn extension_deferred_labels_helper_tag_class_and_non_root_metadata() {
        let base = tmpdir("ext_deferred_labels");
        let reg = base.join("registry");
        let tgz = make_tgz(
            &base,
            "depmod",
            &[
                ("MODULE.bazel", "module(name='depmod', version='1.0')\next = use_extension('//:ext.bzl', 'my_ext')\nuse_repo(ext, 'first')\n"),
                ("BUILD.bazel", ""),
                ("pkg/BUILD.bazel", "exports_files(['local.txt'])\n"),
                ("pkg/local.txt", "hello"),
                (
                    "pkg/ext.bzl",
                    r#"
def _r_impl(rctx):
    rctx.file("BUILD.bazel", "exports_files(['out.txt'])\n")
    rctx.file("out.txt", "%s|%s" % (rctx.attr.sibling, rctx.attr.rel))

my_repo = repository_rule(
    implementation = _r_impl,
    attrs = {"sibling": attr.label(), "rel": attr.label()},
)

def _ext_impl(mctx):
    # First repo references second repo created *later* in the same implementation,
    # plus a package-relative label ':local.txt' in the .bzl's package ('pkg').
    my_repo(name = "first", sibling = "@second//:out.txt", rel = ":local.txt")
    my_repo(name = "second", sibling = "@first//:out.txt", rel = "//pkg:local.txt")
    # Non-root usage: invalid root_module_direct_deps must NOT fail extension eval when root does not use it.
    return extension_metadata(root_module_direct_deps = ["not_generated"], root_module_direct_dev_deps = [])

def _make_ext():
    return module_extension(
        implementation = _ext_impl,
        tag_classes = {"t": tag_class({"x": attr.string()})},
    )

my_ext = _make_ext()
"#,
                ),
                ("ext.bzl", "load('//pkg:ext.bzl', _e = 'my_ext')\nmy_ext = _e\n"),
            ],
        );
        add_module(
            &reg,
            "depmod",
            "1.0",
            "module(name='depmod', version='1.0')\next = use_extension('//:ext.bzl', 'my_ext')\nuse_repo(ext, 'first')\n",
            &format!(r#"{{"url":"{}","integrity":"{}","strip_prefix":"depmod-1.0"}}"#, file_url(&tgz), integrity(&tgz)),
        );
        let ws = base.join("ws");
        write(
            &ws,
            "MODULE.bazel",
            "module(name='root')\nbazel_dep(name='depmod', version='1.0')\n",
        );
        let mut o = ResolveOptions::new(&ws);
        o.registries = vec![file_url(&reg)];
        o.check_direct_dependencies = CheckMode::Off;
        let res = Arc::new(resolve(&o).unwrap_or_else(|e| panic!("{e}")));
        let rt = Rt::new(
            ExternalOptions {
                output_base: base.join("ob"),
                workspace_root: ws.clone(),
                client_env: std::env::vars().collect(),
                ..Default::default()
            },
            res,
        );
        // Repo mapping lookup on 'depmod++my_ext+first' evaluates the extension and sees unimported 'second'!
        let mapping = rt.repo_mapping_of("depmod++my_ext+first");
        assert_eq!(
            mapping.get("second").map(String::as_str),
            Some("depmod++my_ext+second")
        );
        assert!(
            !mapping.contains_key(""),
            "non-root extension mapping must not map empty repo name"
        );
        assert!(rt
            .repo_mapping_ordered_of("no_such_canonical_repo")
            .is_empty());

        let first_dir = rt.fetch_repo("depmod++my_ext+first").unwrap();
        let out = std::fs::read_to_string(first_dir.join("out.txt")).unwrap();
        assert_eq!(
            out,
            "@@depmod++my_ext+second//:out.txt|@@depmod+//pkg:local.txt"
        );
    }

    #[test]
    fn tag_class_and_module_extension_validation() {
        let base = tmpdir("ext_val");
        let ws = base.join("ws");
        write(&ws, "BUILD.bazel", "");
        write(&ws, "bad_tc.bzl", "bad = tag_class(attrs = 'not_a_dict')\n");
        write(&ws, "bad_tc_val.bzl", "bad = tag_class({'x': 123})\n");
        write(&ws, "bad_tc_kw.bzl", "bad = tag_class({}, extra = 1)\n");
        write(
            &ws,
            "bad_me.bzl",
            "def _r(rctx):\n    module_extension(implementation = lambda m: None)\n    rctx.file('BUILD.bazel', '')\nbad_r = repository_rule(implementation = _r)\n",
        );
        write(&ws, "MODULE.bazel", "module(name='root')\nbad_r = use_repo_rule('//:bad_me.bzl', 'bad_r')\nbad_r(name='x')\n");
        let mut o = ResolveOptions::new(&ws);
        o.check_direct_dependencies = CheckMode::Off;
        let res = Arc::new(resolve(&o).unwrap_or_else(|e| panic!("{e}")));
        let rt = Rt::new(
            ExternalOptions {
                output_base: base.join("ob"),
                workspace_root: ws.clone(),
                client_env: std::env::vars().collect(),
                ..Default::default()
            },
            res,
        );
        assert!(rt
            .load_bzl("//:bad_tc.bzl", "", "")
            .unwrap_err()
            .contains("want 'dict'"));
        assert!(rt
            .load_bzl("//:bad_tc_val.bzl", "", "")
            .unwrap_err()
            .contains("attrs must be created with attr.*()"));
        assert!(rt
            .load_bzl("//:bad_tc_kw.bzl", "", "")
            .unwrap_err()
            .contains("unexpected keyword argument 'extra'"));
        assert!(rt
            .fetch_repo("+bad_r+x")
            .unwrap_err()
            .contains("initializing a .bzl file"));
    }

    #[test]
    fn local_repository_and_new_local_repository_refetch_when_fetching_enabled() {
        let base = tmpdir("local_refetch");
        let ws = base.join("ws");
        let local_src = ws.join("local_src");
        write(&local_src, "a.txt", "one");
        write(
            &ws,
            "MODULE.bazel",
            "module(name='root')\nnew_local_repository = use_repo_rule('@bazel_tools//tools/build_defs/repo:local.bzl', 'new_local_repository')\nnew_local_repository(name='nlr', path='local_src', build_file_content='# v1\\n')\n",
        );
        let mut o = ResolveOptions::new(&ws);
        o.check_direct_dependencies = CheckMode::Off;
        let res = Arc::new(resolve(&o).unwrap_or_else(|e| panic!("{e}")));
        let opts = ExternalOptions {
            output_base: base.join("ob"),
            workspace_root: ws.clone(),
            client_env: std::env::vars().collect(),
            ..Default::default()
        };
        let rt1 = Rt::new(opts.clone(), res.clone());
        let dir1 = rt1.fetch_repo("+new_local_repository+nlr").unwrap();
        assert!(dir1.join("a.txt").exists());
        assert!(!dir1.join("b.txt").exists());

        // Add b.txt and remove a.txt in source directory; a new Rt session (fetching enabled) must refetch!
        std::fs::remove_file(local_src.join("a.txt")).unwrap();
        write(&local_src, "b.txt", "two");
        let rt2 = Rt::new(opts, res);
        let dir2 = rt2.fetch_repo("+new_local_repository+nlr").unwrap();
        assert!(
            !dir2.join("a.txt").exists(),
            "stale child symlink must be gone after refetch"
        );
        assert!(
            dir2.join("b.txt").exists(),
            "new child symlink must appear after refetch"
        );

        // `local_repository` also refetches when fetching is enabled (even if source directory disappears).
        let base2 = tmpdir("local_repo_refetch");
        let ws2 = base2.join("ws");
        let loc_dir = ws2.join("loc_pkg");
        write(&loc_dir, "REPO.bazel", "");
        write(&loc_dir, "BUILD.bazel", "");
        write(
            &ws2,
            "MODULE.bazel",
            "module(name='root')\nlocal_repository = use_repo_rule('@bazel_tools//tools/build_defs/repo:local.bzl', 'local_repository')\nlocal_repository(name='lr', path='loc_pkg')\n",
        );
        let mut o2 = ResolveOptions::new(&ws2);
        o2.check_direct_dependencies = CheckMode::Off;
        let res2 = Arc::new(resolve(&o2).unwrap_or_else(|e| panic!("{e}")));
        let opts2 = ExternalOptions {
            output_base: base2.join("ob"),
            workspace_root: ws2.clone(),
            client_env: std::env::vars().collect(),
            ..Default::default()
        };
        let lrt1 = Rt::new(opts2.clone(), res2.clone());
        let ldir = lrt1.fetch_repo("+local_repository+lr").unwrap();
        assert!(ldir.join("BUILD.bazel").exists());
        std::fs::remove_dir_all(&loc_dir).unwrap();
        let lrt2 = Rt::new(opts2, res2);
        assert!(
            lrt2.fetch_repo("+local_repository+lr").is_err(),
            "local_repository must refetch and fail when source disappears"
        );
    }

    #[test]
    fn bazel_tools_and_local_repo_reuse_markers_when_unchanged() {
        let base = tmpdir("local_marker_reuse");
        let ws = base.join("ws");
        let local_src = ws.join("local_src");
        write(&local_src, "a.txt", "one");
        write(
            &ws,
            "MODULE.bazel",
            "module(name='root')\nnew_local_repository = use_repo_rule('@bazel_tools//tools/build_defs/repo:local.bzl', 'new_local_repository')\nnew_local_repository(name='nlr', path='local_src', build_file_content='# v1\\n')\n",
        );
        let mut o = ResolveOptions::new(&ws);
        o.check_direct_dependencies = CheckMode::Off;
        let res = Arc::new(resolve(&o).unwrap_or_else(|e| panic!("{e}")));
        let opts = ExternalOptions {
            output_base: base.join("ob"),
            workspace_root: ws.clone(),
            client_env: std::env::vars().collect(),
            ..Default::default()
        };

        let rt1 = Rt::new(opts.clone(), res.clone());
        let bt1 = rt1.fetch_repo("bazel_tools").unwrap();
        let nlr1 = rt1.fetch_repo("+new_local_repository+nlr").unwrap();
        assert!(base.join("ob/external/@bazel_tools.marker").exists());

        // Plant a sentinel file inside both repos: `bazel_tools` reuses its marker when fetching
        // is enabled, whereas `local = True` repos (`new_local_repository`) bypass cached repo
        // contents whenever fetching is enabled (`shouldUseCachedRepoContents`) and only reuse
        // their on-disk materialization under `--nofetch`.
        let bt_sentinel = bt1.join(".zel_marker_sentinel");
        let nlr_sentinel = nlr1.join(".zel_marker_sentinel");
        std::fs::write(&bt_sentinel, "kept").unwrap();
        std::fs::write(&nlr_sentinel, "kept").unwrap();

        let mut nofetch_opts = opts.clone();
        nofetch_opts.nofetch = true;
        let rt_nofetch = Rt::new(nofetch_opts, res.clone());
        let nlr_nofetch = rt_nofetch.fetch_repo("+new_local_repository+nlr").unwrap();
        assert!(
            nlr_nofetch.join(".zel_marker_sentinel").exists(),
            "local = True repo should reuse existing materialization under --nofetch"
        );

        let rt2 = Rt::new(opts, res);
        let bt2 = rt2.fetch_repo("bazel_tools").unwrap();
        let nlr2 = rt2.fetch_repo("+new_local_repository+nlr").unwrap();
        assert!(
            bt2.join(".zel_marker_sentinel").exists(),
            "bazel_tools should reuse cached materialization when marker matches"
        );
        assert!(
            !nlr2.join(".zel_marker_sentinel").exists(),
            "local = True repo must skip cached repo contents when fetching is enabled"
        );
    }

    #[test]
    fn custom_local_and_bazel_tools_configure_rules_honor_caching_and_vendoring_flags() {
        let base = tmpdir("custom_local_and_xcode_cfg");
        let ws = base.join("ws");
        let rcc = base.join("rcc");
        let vendor = base.join("vendor");
        write(
            &ws,
            "MODULE.bazel",
            concat!(
                "module(name='root')\n",
                "android_sdk_proxy = use_extension('@bazel_tools//tools/android:android_extensions.bzl', 'android_sdk_proxy_extensions')\n",
                "use_repo(android_sdk_proxy, 'android_external')\n",
                "xcode_autoconf = use_repo_rule('@bazel_tools//tools/osx:xcode_configure.bzl', 'xcode_autoconf')\n",
                "xcode_autoconf(name='local_config_xcode')\n",
            ),
        );
        let mut o = ResolveOptions::new(&ws);
        o.check_direct_dependencies = CheckMode::Off;
        let res = Arc::new(resolve(&o).unwrap_or_else(|e| panic!("{e}")));
        let opts = ExternalOptions {
            output_base: base.join("ob"),
            workspace_root: ws.clone(),
            repo_contents_cache: Some(rcc.clone()),
            vendor_dir: Some(vendor.clone()),
            client_env: std::env::vars().collect(),
            ..Default::default()
        };
        let rt1 = Rt::new(opts.clone(), res.clone());
        let root_map = res.repo_mapping(&ModuleKey::root());
        let android_canon = root_map.get("android_external").unwrap().as_str();
        let xcode_canon = root_map.get("local_config_xcode").unwrap().as_str();

        // `android_external_repository` has `local = True`: recognized as local, excluded from
        // vendoring, kept out of `repo_contents_cache`, and refetched when fetching is enabled.
        assert!(rt1.is_local_repo(android_canon));
        assert!(!rt1.is_configure_repo(android_canon).unwrap());
        let adir1 = rt1.fetch_repo(android_canon).unwrap();
        assert!(adir1.join("BUILD").exists());
        let android_spec = rt1.repo_spec_for(android_canon).unwrap().unwrap();
        let android_fp = rt1.predeclared_input_hash(android_canon, &android_spec);
        assert!(
            crate::repo_contents_cache::RepoContentsCache::new(&rcc)
                .get_candidate_repos(&android_fp)
                .is_empty(),
            "local = True repo must not be moved into repo_contents_cache"
        );
        #[cfg(unix)]
        assert!(!adir1.is_symlink());
        std::fs::write(adir1.join(".sentinel"), "1").unwrap();

        let rt2 = Rt::new(opts, res);
        let adir2 = rt2.fetch_repo(android_canon).unwrap();
        assert!(
            !adir2.join(".sentinel").exists(),
            "android_external_repository (local = True) must refetch when fetching is enabled"
        );

        // `xcode_autoconf` in `@bazel_tools//tools/osx:xcode_configure.bzl` sets `configure = True`:
        // `is_configure_repo` and `is_local_repo` (vendoring exclusion) must both return true.
        assert!(
            rt2.is_configure_repo(xcode_canon).unwrap(),
            "configure = True rule in @bazel_tools must be recognized by is_configure_repo"
        );
        assert!(
            rt2.is_local_repo(xcode_canon),
            "configure = True rule in @bazel_tools must be excluded from vendoring"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn http_file_non_ascii_downloaded_path_and_sha512_remote_file_integrity() {
        let base = tmpdir("http_file_non_ascii");
        let ws = base.join("ws");
        let payload = base.join("blob.bin");
        std::fs::write(&payload, b"non-ascii-ok").unwrap();
        let sha512_sri = format!(
            "sha512-{}",
            crate::hash::base64_encode(&crate::hash::sha512(b"non-ascii-ok"))
        );

        let mut tar_bytes = Vec::new();
        {
            let mut hdr = vec![0u8; 512];
            hdr[..5].copy_from_slice(b"a.txt");
            hdr[100..108].copy_from_slice(b"0000644\0");
            hdr[108..116].copy_from_slice(b"0000000\0");
            hdr[116..124].copy_from_slice(b"0000000\0");
            hdr[124..136].copy_from_slice(b"00000000002\0");
            hdr[136..148].copy_from_slice(b"00000000000\0");
            hdr[148..156].copy_from_slice(b"        ");
            hdr[156] = b'0';
            hdr[257..263].copy_from_slice(b"ustar\0");
            hdr[263..265].copy_from_slice(b"00");
            let sum: u32 = hdr.iter().map(|&b| b as u32).sum();
            hdr[148..156].copy_from_slice(format!("{:06o}\0 ", sum).as_bytes());
            tar_bytes.extend_from_slice(&hdr);
            tar_bytes.extend_from_slice(b"hi");
            tar_bytes.extend(vec![0u8; 510]);
            tar_bytes.extend(vec![0u8; 1024]);
        }
        let archive_path = base.join("pkg.tar");
        std::fs::write(&archive_path, &tar_bytes).unwrap();
        let archive_sha512 = format!(
            "sha512-{}",
            crate::hash::base64_encode(&crate::hash::sha512(&tar_bytes))
        );

        write(
            &ws,
            "MODULE.bazel",
            &format!(
                concat!(
                    "module(name='root')\n",
                    "http_file = use_repo_rule('@bazel_tools//tools/build_defs/repo:http.bzl', 'http_file')\n",
                    "http_archive = use_repo_rule('@bazel_tools//tools/build_defs/repo:http.bzl', 'http_archive')\n",
                    "http_file(\n",
                    "    name = 'umlaut_file',\n",
                    "    urls = ['{blob_url}'],\n",
                    "    integrity = '{blob_sri}',\n",
                    "    downloaded_file_path = 'fübar.bin',\n",
                    ")\n",
                    "http_archive(\n",
                    "    name = 'sha512_arch',\n",
                    "    urls = ['{arch_url}'],\n",
                    "    integrity = '{arch_sri}',\n",
                    "    build_file_content = 'exports_files([\"extra.bin\"])\\n',\n",
                    "    remote_file_urls = {{'extra.bin': ['{blob_url}']}},\n",
                    "    remote_file_integrity = {{'extra.bin': '{blob_sri}'}},\n",
                    ")\n",
                ),
                blob_url = file_url(&payload),
                blob_sri = sha512_sri,
                arch_url = file_url(&archive_path),
                arch_sri = archive_sha512,
            ),
        );
        let mut o = ResolveOptions::new(&ws);
        o.check_direct_dependencies = CheckMode::Off;
        let res = Arc::new(resolve(&o).unwrap_or_else(|e| panic!("{e}")));
        let rcc = base.join("rcc");
        let opts = ExternalOptions {
            output_base: base.join("ob"),
            workspace_root: ws.clone(),
            repo_contents_cache: Some(rcc.clone()),
            ..Default::default()
        };
        let rt = Rt::new(opts, res.clone());
        let root_map = res.repo_mapping(&ModuleKey::root());

        let hf_canon = root_map.get("umlaut_file").unwrap().as_str();
        let hf_dir = rt.fetch_repo(hf_canon).unwrap();
        let build_text = std::fs::read_to_string(hf_dir.join("file/BUILD")).unwrap();
        assert!(
            build_text.contains("\"fübar.bin\""),
            "http_file BUILD must contain unescaped UTF-8 filename, got: {build_text}"
        );
        assert!(!build_text.contains("\\u{"), "{build_text}");
        assert_eq!(
            std::fs::read(hf_dir.join("file/fübar.bin")).unwrap(),
            b"non-ascii-ok"
        );

        // `sha512_arch` has matching SHA-512 `remote_file_integrity`, so `http_archive` returns
        // `reproducible = True` and stores the fetched repo in `repo_contents_cache` (symlinked on
        // Unix, copied back on Windows).
        let ha_canon = root_map.get("sha512_arch").unwrap().as_str();
        let ha_dir = rt.fetch_repo(ha_canon).unwrap();
        assert_eq!(
            std::fs::read(ha_dir.join("extra.bin")).unwrap(),
            b"non-ascii-ok"
        );
        let ha_spec = rt.repo_spec_for(ha_canon).unwrap().unwrap();
        let ha_fp = rt.predeclared_input_hash(ha_canon, &ha_spec);
        let candidates =
            crate::repo_contents_cache::RepoContentsCache::new(&rcc).get_candidate_repos(&ha_fp);
        assert_eq!(
            candidates.len(),
            1,
            "http_archive with matching SHA-512 remote_file_integrity must stay reproducible and enter repo_contents_cache"
        );
        assert_eq!(
            std::fs::read(candidates[0].contents_dir.join("extra.bin")).unwrap(),
            b"non-ascii-ok"
        );
        #[cfg(unix)]
        assert!(ha_dir.is_symlink());

        // Remove source files and fetch into a fresh output_base with downloads disabled to prove
        // the repo_contents_cache entry satisfies subsequent fetches cross-platform.
        std::fs::remove_file(&payload).unwrap();
        std::fs::remove_file(&archive_path).unwrap();
        let rt_cached = Rt::new(
            ExternalOptions {
                output_base: base.join("ob2"),
                workspace_root: ws,
                repo_contents_cache: Some(rcc),
                disable_download: true,
                ..Default::default()
            },
            res,
        );
        let ha_cached_dir = rt_cached.fetch_repo(ha_canon).unwrap();
        assert_eq!(
            std::fs::read(ha_cached_dir.join("extra.bin")).unwrap(),
            b"non-ascii-ok"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn windows_starlark_path_normalization_uses_slashes_and_uppercase_drive() {
        use crate::rt::normalize_windows_starlark_path;
        assert_eq!(
            normalize_windows_starlark_path(r"c:\Users\alice\repo\file.txt"),
            "C:/Users/alice/repo/file.txt"
        );
        assert_eq!(
            normalize_windows_starlark_path(r"\\?\d:\bazel\external\foo\"),
            "D:/bazel/external/foo"
        );
        assert_eq!(normalize_windows_starlark_path(r"e:\"), "E:/");
        assert_eq!(
            normalize_windows_starlark_path(r"\\?\UNC\server\share\dir\a.txt"),
            "//server/share/dir/a.txt"
        );
    }

    #[test]
    fn original_name_override_repo_must_exist_and_label_dict_attrs() {
        let base = tmpdir("fetch_parity_ext");
        let ws = base.join("ws");
        write(&ws, "BUILD", "");
        write(
            &ws,
            "defs.bzl",
            r#"
__do_not_use_fail_with_incompatible_use_cc_configure_from_rules_cc()

def _r_impl(rctx):
    keys = [str(k) for k in rctx.attr.lksd.keys()]
    vals = [str(x) for x in rctx.attr.lld.get("grp", [])]
    rctx.file("BUILD", "")
    rctx.file("info.txt", "name=" + rctx.name + "\norig=" + rctx.original_name + "\nlksd=" + ",".join(keys) + "\nlld=" + ",".join(vals) + "\n")

my_repo = repository_rule(
    implementation = _r_impl,
    attrs = {
        "lksd": attr.label_keyed_string_dict(),
        "lld": attr.label_list_dict(),
    },
)

def _ext_impl(mctx):
    my_repo(
        name = "bar",
        lksd = {Label("//:t1"): "v1"},
        lld = {"grp": ["//:t2"]},
    )

ext = module_extension(implementation = _ext_impl)
"#,
        );
        write(
            &ws,
            "MODULE.bazel",
            "module(name='root')\next = use_extension('//:defs.bzl', 'ext')\nuse_repo(ext, 'bar')\n",
        );
        let mut o = ResolveOptions::new(&ws);
        o.check_direct_dependencies = CheckMode::Off;
        let res = Arc::new(resolve(&o).unwrap_or_else(|e| panic!("{e}")));
        let rt = Rt::new(
            ExternalOptions {
                output_base: base.join("ob"),
                workspace_root: ws.clone(),
                client_env: std::env::vars().collect(),
                ..Default::default()
            },
            res,
        );
        let repo_dir = rt.fetch_repo("+ext+bar").unwrap();
        let info = std::fs::read_to_string(repo_dir.join("info.txt")).unwrap();
        assert!(info.contains("name=+ext+bar\n"), "{info}");
        assert!(info.contains("orig=bar\n"), "{info}");
        assert!(info.contains("lksd=@@//:t1\n"), "{info}");
        assert!(info.contains("lld=@@//:t2\n"), "{info}");

        // `override_repo` targeting a repository not generated by the extension must fail.
        let ws_bad = base.join("ws_bad_override");
        write(&ws_bad, "BUILD", "");
        write(
            &ws_bad,
            "defs.bzl",
            &std::fs::read_to_string(ws.join("defs.bzl")).unwrap(),
        );
        write(&ws_bad, "other/MODULE.bazel", "module(name='other')\n");
        write(
            &ws_bad,
            "MODULE.bazel",
            "module(name='root')\nlocal_repository = use_repo_rule('@bazel_tools//tools/build_defs/repo:local.bzl', 'local_repository')\nlocal_repository(name='local_rep', path='other')\next = use_extension('//:defs.bzl', 'ext')\noverride_repo(ext, nonexistent='local_rep')\nuse_repo(ext, 'bar')\n",
        );
        let mut o_bad = ResolveOptions::new(&ws_bad);
        o_bad.check_direct_dependencies = CheckMode::Off;
        let res_bad = Arc::new(resolve(&o_bad).unwrap());
        let rt_bad = Rt::new(
            ExternalOptions {
                output_base: base.join("ob_bad"),
                workspace_root: ws_bad,
                client_env: std::env::vars().collect(),
                ..Default::default()
            },
            res_bad,
        );
        let err = rt_bad.fetch_repo("+ext+bar").unwrap_err();
        assert!(
            err.contains("does not generate repository \"nonexistent\", yet it is overridden via override_repo()"),
            "{err}"
        );
        assert!(err.contains("Use inject_repo() instead"), "{err}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn archive_override_applies_canonical_bazel_tools_and_root_patches_and_preserves_original_name()
    {
        crate::resolve::HERMETIC_BAZEL_TOOLS.with(|h| h.set(true));
        let base = tmpdir("bazel_tools_patch_and_orig_name");
        let tgz = make_tgz(
            &base,
            "dep",
            &[
                ("MODULE.bazel", "module(name = 'dep', version = '1.0')\n"),
                ("BUILD", ""),
                ("msg.txt", "step0\n"),
            ],
        );

        // Provide an overridden `@@bazel_tools` repo carrying `tools/build_defs/repo/*.bzl` plus `//tools:dep.patch`.
        let bt_override = base.join("custom_bazel_tools");
        for (rel, bytes) in zel_bazel_tools::files() {
            if *rel == "MODULE.bazel" || rel.starts_with("tools/build_defs/repo/") {
                let dst = bt_override.join(rel);
                std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
                std::fs::write(dst, bytes).unwrap();
            }
        }
        write(
            &bt_override,
            "tools/dep.patch",
            "--- a/msg.txt\n+++ b/msg.txt\n@@ -1 +1 @@\n-step0\n+step1\n",
        );

        let ws = base.join("ws");
        write(&ws, "BUILD", "");
        write(
            &ws,
            "root.patch",
            "--- a/msg.txt\n+++ b/msg.txt\n@@ -1 +1 @@\n-step1\n+step2\n",
        );
        write(
            &ws,
            "inspect.bzl",
            r#"
def _inspect_impl(rctx):
    rctx.file("BUILD", "")
    rctx.file("names.txt", "name=" + rctx.name + "\norig=" + rctx.original_name + "\n")

inspect_repo = repository_rule(implementation = _inspect_impl)
"#,
        );
        write(
            &ws,
            "MODULE.bazel",
            &format!(
                r#"
module(name = "root", repo_name = "my_main")
bazel_dep(name = "dep", version = "1.0", repo_name = "dep_alias")
archive_override(
    module_name = "dep",
    urls = ["{url}"],
    strip_prefix = "dep-1.0",
    patch_strip = 1,
    patches = [
        "@@bazel_tools//tools:dep.patch",
        "@my_main//:root.patch",
    ],
)
inspect_repo = use_repo_rule("//:inspect.bzl", "inspect_repo")
inspect_repo(name = "via_use_repo_rule")
"#,
                url = file_url(&tgz),
            ),
        );

        let ob = base.join("ob");
        let args = vec![
            "--registry=file:///nonexistent-registry".to_string(),
            "--check_direct_dependencies=off".to_string(),
            format!(
                "--override_repository=bazel_tools={}",
                bt_override.display()
            ),
            "--all".to_string(),
        ];
        let out = crate::commands::run_bzlmod_command("fetch", &args, &ws, &ob);
        assert_eq!(out.exit_code, 0, "{}", out.stderr);

        assert_eq!(
            std::fs::read_to_string(ob.join("external/dep+/msg.txt")).unwrap(),
            "step2\n"
        );

        // `use_repo_rule` repos (`InnateRunnableExtension`, `BazelDepGraphFunction.java:186-215`)
        // have `rctx.name == "+<rule_name>+<name>"` and `rctx.original_name == "<name>"`.
        let names =
            std::fs::read_to_string(ob.join("external/+inspect_repo+via_use_repo_rule/names.txt"))
                .unwrap();
        assert_eq!(
            names,
            "name=+inspect_repo+via_use_repo_rule\norig=via_use_repo_rule\n"
        );

        // Module repos (`canonical_repo_names`) have `original_name == canonical` (`dep+`), matching
        // `RepoDefinitionFunction.java:148,198` and `StarlarkRepositoryContext.java:141-143`.
        let e = setup(
            "orig_name_module",
            "module(name='root')\nbazel_dep(name='dep', version='1.0', repo_name='dep_alias')\n",
            &[(
                "dep",
                "1.0",
                &[("MODULE.bazel", "module(name='dep', version='1.0')\n")],
            )],
        );
        assert_eq!(e.rt.original_name_for("dep+", "dep_alias"), "dep+");
        let _ = std::fs::remove_dir_all(e.ws.parent().unwrap());

        let _ = std::fs::remove_dir_all(&base);
    }
}
