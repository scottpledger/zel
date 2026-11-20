//! `RepoResolver` backed by Bzlmod: external repositories are fetched on demand into
//! `<output_base>/external/<canonical>` and labels are resolved through repo mappings.

use crate::rt::Rt;
use std::sync::{Arc, Mutex};
use zel_cmdline::{RepositoryMapping, RepositoryName};
use zel_packages::lookup::RepoResolver;
use zel_vfs::PathFragment;

pub struct BzlmodRepoResolver {
    rt: Arc<Rt>,
    /// Fetch errors encountered while resolving (reported by the caller).
    pub errors: Mutex<Vec<String>>,
}

impl BzlmodRepoResolver {
    pub fn new(rt: Arc<Rt>) -> Self {
        Self {
            rt,
            errors: Mutex::new(vec![]),
        }
    }
}

impl RepoResolver for BzlmodRepoResolver {
    fn repo_root(&self, repo: &RepositoryName) -> Option<PathFragment> {
        if !repo.is_visible() {
            return None;
        }
        if repo.is_main() || repo.name() == "_main" {
            return Some(PathFragment::create(
                &self.rt.opts.workspace_root.display().to_string(),
            ));
        }
        match self.rt.fetch_repo(repo.name()) {
            Ok(dir) => Some(PathFragment::create(&dir.display().to_string())),
            Err(e) => {
                self.errors.lock().unwrap().push(e.to_string());
                None
            }
        }
    }

    fn mapping(&self, owner: &RepositoryName) -> RepositoryMapping {
        let entries: Vec<(String, RepositoryName)> = self
            .rt
            .repo_mapping_ordered_of(owner.name())
            .into_iter()
            .map(|(a, c)| (a, RepositoryName::create_unvalidated(c)))
            .collect();
        RepositoryMapping::new_ordered(entries, owner.clone())
    }

    fn autoload_module_repo(&self, module_name: &str, repo_name: &str) -> Option<RepositoryName> {
        autoload_module_repo_for_resolution(self, &self.rt.resolution, module_name, repo_name)
    }
}

enum AutoloadModuleLookup {
    Found(RepositoryName),
    TooOld,
    NotFound,
}

fn autoload_module_repo_for_resolution<R: RepoResolver>(
    resolver: &R,
    res: &crate::resolve::Resolution,
    module_name: &str,
    repo_name: &str,
) -> Option<RepositoryName> {
    match resolve_autoload_module_repo(res, module_name, repo_name) {
        AutoloadModuleLookup::Found(repo) => Some(repo),
        // Module exists in the Bzlmod dep graph but is below the minimum version required for
        // autoloading; do not fall back to the main repo mapping (which would return that same
        // too-old repo and fail autoloading instead of installing missing-module stubs).
        AutoloadModuleLookup::TooOld => None,
        AutoloadModuleLookup::NotFound => {
            let main_map = resolver.mapping(&RepositoryName::main());
            for name in [repo_name, module_name] {
                let r = main_map.get(name);
                if r.is_visible() && resolver.repo_root(&r).is_some() {
                    return Some(r);
                }
            }
            None
        }
    }
}

fn resolve_autoload_module_repo(
    res: &crate::resolve::Resolution,
    module_name: &str,
    repo_name: &str,
) -> AutoloadModuleLookup {
    let best = res
        .canonical_repo_names
        .iter()
        .filter(|(_, mk)| !mk.is_root() && (mk.name == module_name || mk.name == repo_name))
        .max_by(|(_, a), (_, b)| a.version.cmp(&b.version));
    let Some((canon, mk)) = best else {
        return AutoloadModuleLookup::NotFound;
    };
    let min_ver = match module_name {
        "protobuf" => Some("29.0-rc1"),
        "rules_android" => Some("0.6.0-rc1"),
        _ => None,
    };
    if let Some(min_s) = min_ver {
        if let Ok(min_v) = crate::version::Version::parse(min_s) {
            if !mk.version.is_empty() && mk.version <= min_v {
                return AutoloadModuleLookup::TooOld;
            }
        }
    }
    AutoloadModuleLookup::Found(RepositoryName::create_unvalidated(canon.as_str()))
}

/// Resolver that opens the Bzlmod session lazily, on the first lookup of a non-main repository,
/// so commands that never touch external repos never resolve modules or hit registries.
pub struct LazyBzlmodResolver {
    cfg: crate::session::BzlmodConfig,
    session: Mutex<Option<Result<Arc<crate::session::Session>, String>>>,
    errors: Mutex<Vec<String>>,
    fail_code: Mutex<Option<i32>>,
}

impl LazyBzlmodResolver {
    pub fn new(cfg: crate::session::BzlmodConfig) -> Self {
        Self {
            cfg,
            session: Mutex::new(None),
            errors: Mutex::new(vec![]),
            fail_code: Mutex::new(None),
        }
    }

    /// Eagerly resolves the module graph (Bazel computes the main repository mapping before
    /// loading anything). Returns the failure exit code if resolution failed; without a
    /// `MODULE.bazel` this is a no-op.
    pub fn open_now(&self) -> Result<(), i32> {
        if !self.cfg.workspace.join("MODULE.bazel").exists() {
            return Ok(());
        }
        let _ = self.session();
        match *self.fail_code.lock().unwrap() {
            Some(c) => Err(c),
            None => Ok(()),
        }
    }

    fn session(&self) -> Option<Arc<crate::session::Session>> {
        let mut g = self.session.lock().unwrap();
        if g.is_none() {
            *g = Some(match crate::session::Session::open(self.cfg.clone()) {
                Ok(s) => {
                    let mut e = self.errors.lock().unwrap();
                    for w in &s.warnings {
                        e.push(format!("WARNING: {w}"));
                    }
                    Ok(Arc::new(s))
                }
                Err(e) => {
                    *self.fail_code.lock().unwrap() = Some(e.exit_code);
                    let mut es = self.errors.lock().unwrap();
                    for w in &e.warnings {
                        es.push(if w.starts_with("ERROR:") {
                            w.clone()
                        } else {
                            format!("WARNING: {w}")
                        });
                    }
                    es.push(format!("ERROR: {}", e.message));
                    Err(e.message)
                }
            });
        }
        g.as_ref().and_then(|r| r.as_ref().ok().cloned())
    }

    /// Messages (warnings, fetch progress, errors) produced so far; drains the buffer.
    pub fn take_messages(&self) -> Vec<String> {
        let mut out: Vec<String> = std::mem::take(&mut *self.errors.lock().unwrap());
        if let Some(Ok(s)) = self.session.lock().unwrap().as_ref() {
            out.extend(s.rt.take_log());
        }
        out
    }

    /// Writes `MODULE.bazel.lock` if a session was opened.
    pub fn finish(&self) {
        if let Some(Ok(s)) = self.session.lock().unwrap().as_ref() {
            if let Err(e) = s.finish() {
                self.errors.lock().unwrap().push(format!("WARNING: {e}"));
            }
        }
    }
}

impl RepoResolver for LazyBzlmodResolver {
    fn repo_root(&self, repo: &RepositoryName) -> Option<PathFragment> {
        if !repo.is_visible() {
            return None;
        }
        if repo.is_main() || repo.name() == "_main" {
            return Some(PathFragment::create(
                &self.cfg.workspace.display().to_string(),
            ));
        }
        let s = self.session()?;
        match s.rt.fetch_repo(repo.name()) {
            Ok(dir) => Some(PathFragment::create(&dir.display().to_string())),
            Err(e) => {
                self.errors
                    .lock()
                    .unwrap()
                    .push(if e.starts_with("INFO: ") {
                        e
                    } else {
                        format!("ERROR: {e}")
                    });
                None
            }
        }
    }

    fn mapping(&self, owner: &RepositoryName) -> RepositoryMapping {
        // Only consult Bzlmod when a MODULE.bazel exists, to keep plain workspaces untouched.
        if !self.cfg.workspace.join("MODULE.bazel").exists() {
            return RepositoryMapping::always_fallback();
        }
        let Some(s) = self.session() else {
            return RepositoryMapping::always_fallback();
        };
        let entries: Vec<(String, RepositoryName)> =
            s.rt.repo_mapping_ordered_of(owner.name())
                .into_iter()
                .map(|(a, c)| (a, RepositoryName::create_unvalidated(c)))
                .collect();
        RepositoryMapping::new_ordered(entries, owner.clone())
    }

    fn autoload_module_repo(&self, module_name: &str, repo_name: &str) -> Option<RepositoryName> {
        if !self.cfg.workspace.join("MODULE.bazel").exists() {
            return None;
        }
        let s = self.session()?;
        autoload_module_repo_for_resolution(self, &s.rt.resolution, module_name, repo_name)
    }
}

/// Per-runtime owner of the external repository resolver of the running command: the slot read
/// by package loading plus the concrete [`LazyBzlmodResolver`] (for registrations / lockfile).
#[derive(Clone, Default)]
pub struct CommandResolver {
    slot: zel_packages::lookup::RepoResolverSlot,
    installed: Arc<Mutex<Option<Arc<LazyBzlmodResolver>>>>,
}

impl CommandResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// The slot to hand to the Skyframe executor.
    pub fn slot(&self) -> zel_packages::lookup::RepoResolverSlot {
        self.slot.clone()
    }

    /// Installs a [`LazyBzlmodResolver`] as the external repository resolver for `command`
    /// (parsing the command's options for registries, overrides, lockfile mode, ...).
    /// Parse failures leave the resolver uninstalled (the command reports its own option errors).
    pub fn install_for_command(
        &self,
        command: &str,
        args: &[String],
        workspace: &std::path::Path,
        output_base: &std::path::Path,
    ) {
        self.install_for_command_with_env(
            command,
            args,
            workspace,
            output_base,
            std::env::vars().collect(),
        )
    }

    /// Like [`install_for_command`](Self::install_for_command) with the command's client
    /// environment (`--client_env`), from which the repository environment is derived.
    pub fn install_for_command_with_env(
        &self,
        command: &str,
        args: &[String],
        workspace: &std::path::Path,
        output_base: &std::path::Path,
        client_env: std::collections::BTreeMap<String, String>,
    ) {
        let installed = (|| {
            let parsed =
                zel_options::ParsedCommandOptions::parse_for_command(command, args).ok()?;
            let mut cfg = crate::session::BzlmodConfig::from_parsed(
                &parsed,
                workspace,
                output_base,
                client_env,
            )
            .ok()?;
            // Build/query/... compute the main repository mapping first: registry I/O failures
            // there are EXTERNAL_DEPS errors (32), not the internal error `mod` reports.
            cfg.main_mapping_errors = true;
            Some(Arc::new(LazyBzlmodResolver::new(cfg)))
        })();
        match installed {
            Some(r) => {
                self.slot.set(Some(r.clone()));
                *self.installed.lock().unwrap() = Some(r);
            }
            None => {
                self.slot.set(None);
                *self.installed.lock().unwrap() = None;
            }
        }
    }

    /// Registrations of the module graph of the installed resolver (`None` without MODULE.bazel).
    pub fn registrations(&self) -> Option<Registrations> {
        let r = self.installed.lock().unwrap().clone()?;
        r.registrations()
    }

    /// Eagerly opens the module graph of the installed resolver; `Err(exit_code)` on failure
    /// (messages are then available through [`take_messages`](Self::take_messages)).
    pub fn open_now(&self) -> Result<(), i32> {
        let r = self.installed.lock().unwrap().clone();
        match r {
            Some(r) => r.open_now(),
            None => Ok(()),
        }
    }

    /// Whether the installed resolver has encountered any errors so far.
    pub fn has_errors(&self) -> bool {
        let g = self.installed.lock().unwrap();
        if let Some(r) = g.as_ref() {
            !r.errors.lock().unwrap().is_empty()
        } else {
            false
        }
    }

    /// Returns and drains pending messages from the installed resolver.
    pub fn take_messages(&self) -> Vec<String> {
        let g = self.installed.lock().unwrap();
        if let Some(r) = g.as_ref() {
            r.take_messages()
        } else {
            vec![]
        }
    }

    /// Finishes the installed resolver (writes the lockfile) and returns pending messages for stderr.
    pub fn finish(&self) -> Vec<String> {
        let r = self.installed.lock().unwrap().take();
        self.slot.set(None);
        match r {
            Some(r) => {
                r.finish();
                r.take_messages()
            }
            None => vec![],
        }
    }
}

/// Toolchain / execution-platform registrations of the module graph (canonical target patterns
/// in dependency-graph order, root module first) plus the canonical name of `@platforms`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Registrations {
    pub toolchains: Vec<String>,
    pub execution_platforms: Vec<String>,
    pub platforms_repo: String,
    /// `MODULE.bazel` `flag_alias()` entries as `(alias, canonical label)` (`getFlagAliases`).
    pub flag_aliases: Vec<(String, String)>,
}

impl LazyBzlmodResolver {
    /// Opens the session (if needed) and returns the module graph's registrations.
    pub fn registrations(&self) -> Option<Registrations> {
        if !self.cfg.workspace.join("MODULE.bazel").exists() {
            return None;
        }
        let s = self.session()?;
        let platforms_repo =
            s.rt.repo_mapping_of("bazel_tools")
                .get("platforms")
                .cloned()
                .or_else(|| s.rt.repo_mapping_of("").get("platforms").cloned())
                .unwrap_or_else(|| "platforms".to_string());
        Some(Registrations {
            toolchains: s.resolution.registered_toolchains(),
            execution_platforms: s.resolution.registered_execution_platforms(),
            platforms_repo,
            flag_aliases: s.resolution.flag_aliases(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::{base64_encode, sha256};
    use crate::registry::testutil::*;
    use crate::session::{BzlmodConfig, Session};
    use zel_cmdline::PackageIdentifier;
    use zel_packages::loader::{LoaderConfig, PackageLoader};
    use zel_vfs::{DigestHashFunction, LocalFileSystem};

    #[test]
    fn loads_external_package_through_bzlmod() {
        let base = tmpdir("resolver1");
        let src = base.join("a-src");
        write(
            &src,
            "a-1.0/MODULE.bazel",
            "module(name='a', version='1.0')\n",
        );
        write(
            &src,
            "a-1.0/BUILD.bazel",
            "filegroup(name = 'lib', srcs = ['x.txt'], visibility = ['//visibility:public'])\n",
        );
        write(&src, "a-1.0/x.txt", "x");
        write(
            &src,
            "a-1.0/defs.bzl",
            "def mk(name):\n    native.filegroup(name = name, srcs = [])\n",
        );
        let tgz = base.join("a.tar.gz");
        assert!(std::process::Command::new("tar")
            .args([
                "czf",
                tgz.to_str().unwrap(),
                "-C",
                src.to_str().unwrap(),
                "a-1.0"
            ])
            .status()
            .unwrap()
            .success());
        let integrity = format!(
            "sha256-{}",
            base64_encode(&sha256(&std::fs::read(&tgz).unwrap()))
        );
        let reg = base.join("registry");
        add_module(
            &reg,
            "a",
            "1.0",
            "module(name='a', version='1.0')\n",
            &format!(
                r#"{{"url":"{}","integrity":"{integrity}","strip_prefix":"a-1.0"}}"#,
                crate::file_url_for_path(&tgz)
            ),
        );
        let ws = base.join("ws");
        write(
            &ws,
            "MODULE.bazel",
            "module(name='root')\nbazel_dep(name='a', version='1.0', repo_name='my_a')\n",
        );
        write(&ws, "BUILD.bazel", "load('@my_a//:defs.bzl', 'mk')\nmk(name = 'made')\nfilegroup(name = 'g', srcs = ['@my_a//:lib'])\n");
        let parsed = zel_options::ParsedCommandOptions::parse_for_command(
            "build",
            &[format!("--registry={}", crate::file_url_for_path(&reg))],
        )
        .unwrap();
        let cfg =
            BzlmodConfig::from_parsed(&parsed, &ws, &base.join("ob"), std::env::vars().collect())
                .unwrap();
        let session = Session::open(cfg).unwrap_or_else(|e| panic!("{}", e.message));
        let resolver = Arc::new(BzlmodRepoResolver::new(session.rt.clone()));
        let fs = Arc::new(LocalFileSystem::new(DigestHashFunction::Sha256));
        let loader = PackageLoader::new(LoaderConfig::new(fs, resolver.clone()));
        let pkg = loader
            .load_package(&PackageIdentifier::create_in_main_repo("").unwrap())
            .unwrap_or_else(|e| {
                panic!(
                    "{e:?}; resolver errors: {:?}",
                    resolver.errors.lock().unwrap()
                )
            });
        assert!(!pkg.contains_errors, "{:?}", pkg.events);
        assert!(pkg.targets.contains_key("made"));
        let g = pkg.rules().find(|r| r.label.name() == "g").unwrap();
        assert!(format!("{:?}", g.srcs).contains("a+"), "{:?}", g.srcs);
        // Loading a package that lives in the external repo itself.
        let ext_pkg = loader
            .load_package(
                &PackageIdentifier::create(
                    RepositoryName::create_unvalidated("a+"),
                    PathFragment::empty(),
                )
                .unwrap(),
            )
            .unwrap_or_else(|e| {
                panic!(
                    "{e:?}; resolver errors: {:?}",
                    resolver.errors.lock().unwrap()
                )
            });
        assert!(ext_pkg.targets.contains_key("lib"));
        assert!(resolver.errors.lock().unwrap().is_empty());
    }

    #[test]
    fn concurrent_scoped_output_user_root_does_not_leak_into_external_package_load() {
        let base = tmpdir("resolver_isolated_user_root");
        let src = base.join("a-src");
        write(
            &src,
            "a-1.0/MODULE.bazel",
            "module(name='a', version='1.0')\n",
        );
        write(
            &src,
            "a-1.0/BUILD.bazel",
            "filegroup(name = 'lib', srcs = ['x.txt'], visibility = ['//visibility:public'])\n",
        );
        write(&src, "a-1.0/x.txt", "x");
        write(
            &src,
            "a-1.0/defs.bzl",
            "def mk(name):\n    native.filegroup(name = name, srcs = [])\n",
        );
        let tgz = base.join("a.tar.gz");
        assert!(std::process::Command::new("tar")
            .args([
                "czf",
                tgz.to_str().unwrap(),
                "-C",
                src.to_str().unwrap(),
                "a-1.0"
            ])
            .status()
            .unwrap()
            .success());
        let integrity = format!(
            "sha256-{}",
            base64_encode(&sha256(&std::fs::read(&tgz).unwrap()))
        );
        let reg = base.join("registry");
        add_module(
            &reg,
            "a",
            "1.0",
            "module(name='a', version='1.0')\n",
            &format!(
                r#"{{"url":"{}","integrity":"{integrity}","strip_prefix":"a-1.0"}}"#,
                crate::file_url_for_path(&tgz)
            ),
        );

        let barrier = Arc::new(std::sync::Barrier::new(5));
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut handles = Vec::new();

        for _ in 0..4 {
            let barrier = Arc::clone(&barrier);
            let done = Arc::clone(&done);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                while !done.load(std::sync::atomic::Ordering::Acquire) {
                    let _guard = crate::session::scoped_output_user_root(Some(
                        std::path::Path::new("/the/user/root"),
                    ));
                    assert_eq!(
                        crate::session::default_repository_cache(std::path::Path::new(
                            "/x/ob/hash"
                        )),
                        std::path::PathBuf::from("/the/user/root/cache/repos/v1")
                    );
                    std::thread::yield_now();
                }
            }));
        }

        barrier.wait();
        for iter in 0..4 {
            let ws = base.join(format!("ws-{iter}"));
            let ob = base.join(format!("ob-{iter}"));
            write(
                &ws,
                "MODULE.bazel",
                "module(name='root')\nbazel_dep(name='a', version='1.0', repo_name='my_a')\n",
            );
            write(
                &ws,
                "BUILD.bazel",
                "load('@my_a//:defs.bzl', 'mk')\nmk(name = 'made')\nfilegroup(name = 'g', srcs = ['@my_a//:lib'])\n",
            );
            let parsed = zel_options::ParsedCommandOptions::parse_for_command(
                "build",
                &[format!("--registry={}", crate::file_url_for_path(&reg))],
            )
            .unwrap();
            let cfg =
                BzlmodConfig::from_parsed(&parsed, &ws, &ob, std::env::vars().collect()).unwrap();
            assert_eq!(
                cfg.repository_cache.as_deref(),
                Some(base.join("cache/repos/v1").as_path())
            );
            assert_eq!(
                cfg.repo_contents_cache.as_deref(),
                Some(base.join("cache/repos/v1/contents").as_path())
            );
            let session = Session::open(cfg).unwrap_or_else(|e| panic!("{}", e.message));
            let resolver = Arc::new(BzlmodRepoResolver::new(session.rt.clone()));
            let fs = Arc::new(LocalFileSystem::new(DigestHashFunction::Sha256));
            let loader = PackageLoader::new(LoaderConfig::new(fs, resolver.clone()));
            let pkg = loader
                .load_package(&PackageIdentifier::create_in_main_repo("").unwrap())
                .unwrap_or_else(|e| {
                    panic!(
                        "{e:?}; resolver errors: {:?}",
                        resolver.errors.lock().unwrap()
                    )
                });
            assert!(!pkg.contains_errors, "{:?}", pkg.events);
            assert!(pkg.targets.contains_key("made"));
            assert!(resolver.errors.lock().unwrap().is_empty());
        }
        done.store(true, std::sync::atomic::Ordering::Release);
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn registrations_expose_module_toolchains_and_platforms_repo() {
        let base = tmpdir("regs1");
        let reg = base.join("registry");
        let ws = base.join("ws");
        write(
            &ws,
            "MODULE.bazel",
            "module(name='root')\nregister_toolchains('//tc:all', '//other:t')\nregister_execution_platforms('//plat:p')\n",
        );
        let parsed = zel_options::ParsedCommandOptions::parse_for_command(
            "build",
            &[format!("--registry={}", crate::file_url_for_path(&reg))],
        )
        .unwrap();
        let cfg =
            BzlmodConfig::from_parsed(&parsed, &ws, &base.join("ob"), std::env::vars().collect())
                .unwrap();
        let r = LazyBzlmodResolver::new(cfg);
        let regs = r.registrations().expect("registrations");
        // The root module's registrations come first; bazel_tools registers its own after them.
        assert_eq!(
            &regs.toolchains[..2],
            ["@@//tc:all".to_string(), "@@//other:t".to_string()]
        );
        assert_eq!(regs.execution_platforms, vec!["@@//plat:p".to_string()]);
        assert_eq!(regs.platforms_repo, "platforms");
    }

    #[test]
    fn autoload_module_repo_rejects_too_old_version_without_mapping_fallback() {
        let base = tmpdir("autoload-minver");
        let reg = base.join("registry");
        let old_pb_src = base.join("old_pb_src");
        let new_pb_src = base.join("new_pb_src");
        let old_android_src = base.join("old_android_src");
        let new_android_src = base.join("new_android_src");

        for (src, mod_name, ver) in [
            (&old_pb_src, "protobuf", "28.0"),
            (&new_pb_src, "protobuf", "29.0"),
            (&old_android_src, "rules_android", "0.6.0-rc1"),
            (&new_android_src, "rules_android", "0.6.0"),
        ] {
            write(
                src,
                "MODULE.bazel",
                &format!("module(name = '{mod_name}', version = '{ver}')\n"),
            );
            write(src, "BUILD.bazel", "");
        }

        let reg_add = |name: &str, versions: &[(&str, &std::path::Path)]| {
            let v_json: Vec<String> = versions.iter().map(|(v, _)| format!("\"{v}\"")).collect();
            write(
                &reg,
                &format!("modules/{name}/metadata.json"),
                &format!(
                    "{{\"versions\": [{}], \"yanked_versions\": {{}}}}",
                    v_json.join(", ")
                ),
            );
            for (v, src) in versions {
                write(
                    &reg,
                    &format!("modules/{name}/{v}/MODULE.bazel"),
                    &format!("module(name = '{name}', version = '{v}')\n"),
                );
                let source_json = crate::json::Json::Obj(vec![
                    ("type".to_string(), crate::json::Json::str("local_path")),
                    (
                        "path".to_string(),
                        crate::json::Json::str(src.to_string_lossy().replace('\\', "/")),
                    ),
                ])
                .to_compact();
                write(
                    &reg,
                    &format!("modules/{name}/{v}/source.json"),
                    &source_json,
                );
            }
        };
        write(&reg, "bazel_registry.json", "{}");
        reg_add("protobuf", &[("28.0", &old_pb_src), ("29.0", &new_pb_src)]);
        reg_add(
            "rules_android",
            &[("0.6.0-rc1", &old_android_src), ("0.6.0", &new_android_src)],
        );

        let open_resolvers = |label: &str, module_bazel: &str| {
            let ws = base.join(format!("ws_{label}"));
            let ob = base.join(format!("ob_{label}"));
            write(&ws, "MODULE.bazel", module_bazel);
            write(&ws, "BUILD.bazel", "");
            let parsed = zel_options::ParsedCommandOptions::parse_for_command(
                "build",
                &[format!("--registry={}", crate::file_url_for_path(&reg))],
            )
            .unwrap();
            let cfg =
                BzlmodConfig::from_parsed(&parsed, &ws, &ob, std::env::vars().collect()).unwrap();
            let session = Session::open(cfg.clone()).unwrap_or_else(|e| panic!("{}", e.message));
            let eager = BzlmodRepoResolver::new(session.rt.clone());
            let lazy = LazyBzlmodResolver::new(cfg);
            (eager, lazy)
        };

        // Case 1: protobuf@28.0 (<= 29.0-rc1) and rules_android@0.6.0-rc1 (<= 0.6.0-rc1) are both
        // present in the Bzlmod graph AND visible in the main repo mapping; both resolvers must
        // return None (never falling back to main repo mapping).
        let (eager_old, lazy_old) = open_resolvers(
            "old",
            "module(name = 'root')\nbazel_dep(name = 'protobuf', version = '28.0', repo_name = 'com_google_protobuf')\nbazel_dep(name = 'rules_android', version = '0.6.0-rc1')\n",
        );
        // Sanity check: the main repo mapping DOES see com_google_protobuf and its root exists.
        let mapped_pb = eager_old
            .mapping(&RepositoryName::main())
            .get("com_google_protobuf");
        assert!(mapped_pb.is_visible());
        assert!(eager_old.repo_root(&mapped_pb).is_some());

        assert_eq!(
            eager_old.autoload_module_repo("protobuf", "com_google_protobuf"),
            None
        );
        assert_eq!(
            lazy_old.autoload_module_repo("protobuf", "com_google_protobuf"),
            None
        );
        assert_eq!(
            eager_old.autoload_module_repo("rules_android", "rules_android"),
            None
        );
        assert_eq!(
            lazy_old.autoload_module_repo("rules_android", "rules_android"),
            None
        );

        // Case 2: protobuf@29.0 (> 29.0-rc1) and rules_android@0.6.0 (> 0.6.0-rc1) resolve cleanly.
        let (eager_new, lazy_new) = open_resolvers(
            "new",
            "module(name = 'root')\nbazel_dep(name = 'protobuf', version = '29.0', repo_name = 'com_google_protobuf')\nbazel_dep(name = 'rules_android', version = '0.6.0')\n",
        );
        assert!(eager_new
            .autoload_module_repo("protobuf", "com_google_protobuf")
            .is_some());
        assert!(lazy_new
            .autoload_module_repo("protobuf", "com_google_protobuf")
            .is_some());
        assert!(eager_new
            .autoload_module_repo("rules_android", "rules_android")
            .is_some());
        assert!(lazy_new
            .autoload_module_repo("rules_android", "rules_android")
            .is_some());
    }
}
