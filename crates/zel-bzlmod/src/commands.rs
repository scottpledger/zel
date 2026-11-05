//! Entry points used by the server for `mod`, `fetch` and `vendor`.

use crate::model::*;
use crate::modcommand::{run_mod, ExtensionInfoProvider, ModOptions, RepoSpecProvider};
use crate::resolve::ExtensionId;
use crate::rt::Rt;
use crate::session::{run_fetch, run_vendor, BzlmodConfig, CmdOutput, Session};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use zel_options::parser::ParsedCommandOptions;

struct RtProvider {
    /// Extension evaluation failures seen while inspecting (`BazelModuleInspectorValue.errors`).
    errors: std::sync::Mutex<Vec<String>>,
    rt: Arc<Rt>,
}

impl RepoSpecProvider for RtProvider {
    fn repo_spec(&self, canonical: &str) -> Option<RepoSpec> {
        self.rt.repo_spec_for(canonical).ok().flatten()
    }
    fn external_directory(&self) -> String {
        self.rt.external_dir().display().to_string()
    }
    fn repo_override_path(&self, canonical: &str) -> Option<String> {
        // `--override_repository` makes the repo an override (`RepoDefinitionValue.RepoOverride`).
        if let Some(p) = self.rt.override_for(canonical) {
            return Some(p.display().to_string());
        }
        if canonical == "bazel_tools" {
            if let Some(ib) = crate::session::install_base() {
                return Some(ib.join("embedded_tools").display().to_string());
            }
            return Some(self.rt.repo_dir("bazel_tools").display().to_string());
        }
        None
    }
}

impl ExtensionInfoProvider for RtProvider {
    fn generated_repos(&self, id: &ExtensionId) -> Option<Vec<String>> {
        match self.rt.eval_extension(id) {
            Ok(r) => Some(r.generated.into_iter().map(|(n, _)| n).collect()),
            Err(e) => {
                self.errors.lock().unwrap().push(e);
                None
            }
        }
    }
}

fn err_out(code: i32, msg: &str) -> CmdOutput {
    let stderr = if msg.starts_with("ERROR:") || msg.starts_with("INFO:") {
        format!("{msg}\n")
    } else {
        format!("ERROR: {msg}\n")
    };
    CmdOutput { exit_code: code, stdout: String::new(), stderr }
}

/// Runs `mod`, `fetch` or `vendor`. `args` are the command-line arguments after the command name.
pub fn run_bzlmod_command(command: &str, args: &[String], workspace: &Path, output_base: &Path) -> CmdOutput {
    run_bzlmod_command_with_env(command, args, workspace, output_base, std::env::vars().collect())
}

/// [`run_bzlmod_command`] with the command's client environment (`--client_env`).
pub fn run_bzlmod_command_with_env(
    command: &str,
    args: &[String],
    workspace: &Path,
    output_base: &Path,
    env: BTreeMap<String, String>,
) -> CmdOutput {
    let parsed = match ParsedCommandOptions::parse_for_command(command, args) {
        Ok(p) => p,
        Err(e) => return CmdOutput { exit_code: 2, stdout: String::new(), stderr: format!("{}\n", e.format_bazel_error()) },
    };
    let cfg = match BzlmodConfig::from_parsed(&parsed, workspace, output_base, env) {
        Ok(c) => c,
        Err(e) => {
            let code = if e.contains("Parsing command line flag --allow_yanked_versions") { 37 } else { 2 };
            return err_out(code, &e);
        }
    };
    match command {
        "fetch" => run_fetch(cfg, &parsed),
        "vendor" => run_vendor(cfg, &parsed),
        "mod" => run_mod_command(cfg, &parsed),
        other => err_out(2, &format!("Command '{other}' not found. Try 'bazel help'.")),
    }
}

fn run_mod_command(cfg: BzlmodConfig, parsed: &ParsedCommandOptions) -> CmdOutput {
    let mod_opts = match ModOptions::from_parsed(parsed) {
        Ok(o) => o,
        Err(e) => return err_out(2, &e.message),
    };
    let sub = parsed.targets.first().cloned().unwrap_or_default();
    let rest: Vec<String> = parsed.targets.iter().skip(1).cloned().collect();
    // Option/argument validation happens before the module graph is evaluated.
    if let Err(e) = crate::modcommand::validate_early(&sub, &rest, &mod_opts) {
        return err_out(e.exit_code(), &e.message);
    }
    if sub.eq_ignore_ascii_case("dump_repo_mapping") {
        if let Err((code, m)) = validate_dump_args(&rest) {
            return err_out(code, &m);
        }
    }
    let session = match Session::open(cfg) {
        Ok(s) => s,
        Err(e) => {
            let mut s = String::new();
            for w in &e.warnings {
                s.push_str(&if w.starts_with("ERROR:") { format!("{w}\n") } else { format!("WARNING: {w}\n") });
            }
            if e.message.starts_with("ERROR:") || e.message.starts_with("INFO:") {
                s.push_str(&format!("{}\n", e.message));
            } else {
                s.push_str(&format!("ERROR: {}\n", e.message));
            }
            return CmdOutput { exit_code: e.exit_code, stdout: String::new(), stderr: s };
        }
    };
    let mut stderr = String::new();
    for w in &session.warnings {
        stderr.push_str(&format!("WARNING: {w}\n"));
    }
    let lower = sub.to_ascii_lowercase();
    let mut inspector_errors: Vec<String> = Vec::new();
    let out = if lower == "dump_repo_mapping" {
        dump_repo_mapping(&session, &rest, &mod_opts)
    } else if lower == "tidy" {
        if !rest.is_empty() {
            Err((2, "the 'tidy' command doesn't take extra arguments. Type 'bazel help mod' for syntax and help.".to_string()))
        } else {
            crate::tidy::run_tidy(&session)
        }
    } else {
        let provider = RtProvider { errors: Default::default(), rt: session.rt.clone() };
        let r = run_mod(&session.resolution, &sub, &rest, &mod_opts, &provider, &provider).map_err(|e| (e.exit_code(), e.message));
        inspector_errors = std::mem::take(&mut *provider.errors.lock().unwrap());
        r
    };
    for l in session.rt.take_log() {
        stderr.push_str(&format!("{l}\n"));
    }
    let _ = session.finish();
    match out {
        Ok(s) if inspector_errors.is_empty() => CmdOutput { exit_code: 0, stdout: s, stderr },
        Ok(s) => {
            // `ModCommand`: the results are printed, then the failed extensions are reported.
            let mut seen = std::collections::HashSet::new();
            inspector_errors.retain(|e| seen.insert(e.clone()));
            for e in &inspector_errors {
                stderr.push_str(&format!("{}\n", if e.starts_with("ERROR:") { e.clone() } else { format!("ERROR: {e}") }));
            }
            let n = inspector_errors.len();
            stderr.push_str(&format!("ERROR: Results may be incomplete as {n} extension{} failed.\n", if n == 1 { "" } else { "s" }));
            CmdOutput { exit_code: 2, stdout: s, stderr }
        }
        Err((code, msg)) => {
            stderr.push_str(&format!("ERROR: {msg}\n"));
            CmdOutput { exit_code: code, stdout: String::new(), stderr }
        }
    }
}

fn validate_dump_args(args: &[String]) -> Result<(), (i32, String)> {
    if args.is_empty() {
        return Err((2, "No repository name(s) specified. Type 'bazel help mod' for syntax and help.".into()));
    }
    // `RepositoryName.create` of each argument (validated before anything is evaluated).
    for a in args {
        let bad = if a == "." || a == ".." {
            Some(format!("invalid repository name '{a}': repo names are not allowed to be '{a}'"))
        } else if !a.is_empty() && a != "_builtins" && !a.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+')) {
            Some(format!("invalid repository name '{a}': repo names may contain only A-Z, a-z, 0-9, '-', '_', '.' and '+'"))
        } else {
            None
        };
        if let Some(m) = bad {
            return Err((2, format!("{m}. Type 'bazel help mod' for syntax and help.")));
        }
    }
    Ok(())
}

fn dump_repo_mapping(session: &Session, args: &[String], _opts: &ModOptions) -> Result<String, (i32, String)> {
    if args.is_empty() {
        return Err((2, "No repository name(s) specified. Type 'bazel help mod' for syntax and help.".into()));
    }
    validate_dump_args(args)?;
    let res = &session.resolution;
    let mut missing = vec![];
    let mut out = String::new();
    for a in args {
        let mapping: Option<Vec<(String, String)>> = if a.is_empty() {
            Some(res.repo_mapping_ordered(&ModuleKey::root()))
        } else if let Some(k) = res.canonical_repo_names.get(a) {
            Some(res.repo_mapping_ordered(k))
        } else {
            // Only canonical names of repos that exist are accepted (module repos are handled above):
            // a repo generated by a module extension, i.e. `<unique name>+<generated name>`.
            let m = session.rt.repo_mapping_of(a);
            let known = res.extension_ids().into_iter().any(|id| {
                let Some(unique) = res.extension_unique_names.get(&id) else { return false };
                a.strip_prefix(&format!("{unique}+"))
                    .is_some_and(|rest| session.rt.eval_extension(&id).map(|r| r.generated.iter().any(|(n, _)| n == rest)).unwrap_or(false))
            });
            if known {
                let mut v: Vec<(String, String)> = m.into_iter().collect();
                v.sort();
                Some(v)
            } else {
                None
            }
        };
        match mapping {
            Some(m) => {
                let j = crate::json::Json::Obj(m.into_iter().map(|(k, v)| (k, crate::json::Json::Str(v))).collect());
                out.push_str(&j.to_compact());
                out.push('\n');
            }
            None => missing.push(a.clone()),
        }
    }
    if !missing.is_empty() {
        return Err((2, format!("Repositories not found: {}. Type 'bazel help mod' for syntax and help.", missing.join(", "))));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::{base64_encode, sha256};
    use crate::registry::testutil::*;
    use std::path::PathBuf;

    struct Fx {
        ws: PathBuf,
        ob: PathBuf,
        reg: String,
        base: PathBuf,
    }

    fn fixture(name: &str, root: &str) -> Fx {
        // No network in unit tests: bazel_tools' real extension repos (downloads) are dropped.
        crate::resolve::HERMETIC_BAZEL_TOOLS.with(|h| h.set(true));
        let base = tmpdir(name);
        let reg_dir = base.join("registry");
        let mk = |n: &str, v: &str, deps: &str, files: &[(&str, &str)]| {
            let src = base.join(format!("{n}-{v}-src"));
            write(&src, &format!("{n}-{v}/MODULE.bazel"), &format!("module(name='{n}', version='{v}')\n{deps}"));
            for (p, c) in files {
                write(&src, &format!("{n}-{v}/{p}"), c);
            }
            let tgz = base.join(format!("{n}-{v}.tar.gz"));
            let st = std::process::Command::new("tar").args(["czf", tgz.to_str().unwrap(), "-C", src.to_str().unwrap(), &format!("{n}-{v}")]).status().unwrap();
            assert!(st.success());
            let integrity = format!("sha256-{}", base64_encode(&sha256(&std::fs::read(&tgz).unwrap())));
            add_module(
                &reg_dir,
                n,
                v,
                &format!("module(name='{n}', version='{v}')\n{deps}"),
                &format!(r#"{{"url":"file://{}","integrity":"{integrity}","strip_prefix":"{n}-{v}"}}"#, tgz.display()),
            );
        };
        mk("a", "1.0", "bazel_dep(name='c', version='1.0')\n", &[("BUILD.bazel", "# a\n")]);
        mk("b", "1.0", "bazel_dep(name='c', version='1.1')\n", &[]);
        mk("c", "1.0", "", &[]);
        mk("c", "1.1", "", &[("data.txt", "c11")]);
        let ws = base.join("ws");
        write(&ws, "MODULE.bazel", root);
        Fx { ws, ob: base.join("ob"), reg: format!("file://{}", reg_dir.display()), base }
    }

    fn run(f: &Fx, cmd: &str, args: &[&str]) -> CmdOutput {
        let mut a: Vec<String> = vec![format!("--registry={}", f.reg), "--check_direct_dependencies=off".into()];
        a.extend(args.iter().map(|s| s.to_string()));
        run_bzlmod_command(cmd, &a, &f.ws, &f.ob)
    }

    const ROOT: &str = "module(name='root', version='1.0')\nbazel_dep(name='a', version='1.0')\nbazel_dep(name='b', version='1.0')\n";

    #[test]
    fn mod_graph_and_deps_end_to_end() {
        let f = fixture("cmd1", ROOT);
        let out = run(&f, "mod", &["graph"]);
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        assert_eq!(out.stdout, "<root> (root@1.0)\n├───a@1.0 \n│   └───c@1.1 \n└───b@1.0 \n    └───c@1.1 (*) \n\n");
        let out = run(&f, "mod", &["deps"]);
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        assert_eq!(out.stdout, "<root> (root@1.0)\n\n");
        let out = run(&f, "mod", &["deps", "<root>"]);
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        assert!(out.stdout.contains("a@1.0") && out.stdout.contains("b@1.0"), "{}", out.stdout);
        let out = run(&f, "mod", &["path", "c"]);
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        assert!(out.stdout.contains("c@1.1"), "{}", out.stdout);
        let out = run(&f, "mod", &["bogus"]);
        assert_ne!(out.exit_code, 0);
        let out = run(&f, "mod", &["dump_repo_mapping", ""]);
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        assert_eq!(out.stdout, "{\"\":\"\",\"root\":\"\",\"a\":\"a+\",\"b\":\"b+\",\"bazel_tools\":\"bazel_tools\"}\n");
    }

    #[test]
    fn fetch_all_repo_and_lockfile_modes() {
        let f = fixture("cmd2", ROOT);
        let out = run(&f, "fetch", &["--all"]);
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        assert!(out.stderr.contains("All external dependencies fetched successfully."), "{}", out.stderr);
        assert_eq!(std::fs::read_to_string(f.ob.join("external/c+/data.txt")).unwrap(), "c11");
        assert!(f.ob.join("external/a+/BUILD.bazel").exists());
        // file:// registry hashes are never written to the lockfile (BazelLockFileModule filters `file:`),
        // so a graph without extensions writes no lockfile at all.
        if let Ok(lock) = std::fs::read_to_string(f.ws.join("MODULE.bazel.lock")) {
            assert!(lock.contains("\"lockFileVersion\": 28"));
            assert!(!lock.contains("file://"), "{lock}");
        }
        // error mode succeeds for local file:// registries without requiring recorded registry hashes
        let out = run(&f, "fetch", &["--repo=@a", "--lockfile_mode=error"]);
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        assert!(out.stderr.contains("All requested repos fetched successfully."));
        // and updating files in file:// registries succeeds without checksum failure
        write(&f.base.join("registry"), "modules/c/1.1/source.json", r#"{"type":"local_path","path":"/tmp/c"}"#);
        let out = run(&f, "fetch", &["--repo=@a", "--lockfile_mode=update"]);
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        // off mode writes nothing.
        let _ = std::fs::remove_file(f.ws.join("MODULE.bazel.lock"));
        let out = run(&f, "mod", &["graph", "--lockfile_mode=off"]);
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        assert!(!f.ws.join("MODULE.bazel.lock").exists());
        // conflicting fetch options
        let out = run(&f, "fetch", &["--all", "--repo=@a"]);
        assert_eq!(out.exit_code, 2);
        assert!(out.stderr.contains("Only one fetch option can be provided for fetch command"));
    }

    #[test]
    fn fetch_and_vendor_failures_exit_2_like_head() {
        // HEAD (fac655f4d2): `QUERY_EVALUATION_ERROR` = 2 for every failure after option validation.
        let f = fixture("cmd_exit2", ROOT);
        let out = run(&f, "fetch", &["--repo=@nonexistent"]);
        assert_eq!(out.exit_code, 2, "{}", out.stderr);
        let out = run(&f, "fetch", &["--repo=@bad!name"]);
        assert_eq!(out.exit_code, 2, "{}", out.stderr);
        assert!(out.stderr.contains("Invalid repo name:"), "{}", out.stderr);
        let vd = f.base.join("vendor_exit2");
        let out = run(&f, "vendor", &[&format!("--vendor_dir={}", vd.display()), "--repo=@nonexistent"]);
        assert_eq!(out.exit_code, 2, "{}", out.stderr);
        // `--repo` with target patterns is `InterruptedFailureDetails` (8) in `vendor`.
        let out = run(&f, "vendor", &[&format!("--vendor_dir={}", vd.display()), "--repo=@a", "//:x"]);
        assert_eq!(out.exit_code, 8, "{}", out.stderr);
        let out = run(&f, "vendor", &[&format!("--vendor_dir={}", vd.display()), "--nofetch"]);
        assert_eq!(out.exit_code, 2, "{}", out.stderr);
        assert!(out.stderr.contains("You cannot run the vendor command with --nofetch"), "{}", out.stderr);
    }

    #[test]
    fn fetch_of_missing_target_fails_like_a_nobuild_build() {
        let f = fixture("cmd_fetch_target", ROOT);
        write(&f.ws, "BUILD.bazel", "filegroup(name = 'a')\n");
        let out = run(&f, "fetch", &["//:nonexist"]);
        assert_eq!(out.exit_code, 2, "{}", out.stderr);
        assert!(out.stderr.contains("WARNING: Target pattern parsing failed."), "{}", out.stderr);
        assert!(out.stderr.contains("ERROR: Skipping '//:nonexist': no such target '//:nonexist'"), "{}", out.stderr);
        assert!(out.stderr.contains("ERROR: Build did NOT complete successfully"), "{}", out.stderr);
        let out = run(&f, "fetch", &["//nopkg:x"]);
        assert_eq!(out.exit_code, 2, "{}", out.stderr);
        assert!(out.stderr.contains("ERROR: Fetching some target dependencies failed with errors: BUILD file not found"), "{}", out.stderr);
    }

    #[test]
    fn include_failure_is_bad_module_and_missing_override_dir_is_internal() {
        // `include()` of a missing file: BAD_MODULE (48) with the main-mapping prefix; the override dir that is not
        // a repo at all fails the repository fetch (37).
        let f = fixture("cmd_include", "module(name='root')\ninclude('//:nonexist.MODULE.bazel')\n");
        let out = run(&f, "fetch", &["--all"]);
        assert_eq!(out.exit_code, 48, "{}", out.stderr);
        assert!(out.stderr.contains("ERROR: Error computing the main repository mapping: error reading '//:nonexist.MODULE.bazel' included at"), "{}", out.stderr);
        let empty = f.base.join("emptydir");
        std::fs::create_dir_all(&empty).unwrap();
        let f = fixture("cmd_ovr_empty", &format!("module(name='root')\nbazel_dep(name='ma')\nlocal_path_override(module_name='ma', path='{}')\n", empty.display()));
        let out = run(&f, "fetch", &["--all"]);
        assert_eq!(out.exit_code, 37, "{}", out.stderr);
        assert!(out.stderr.contains("fetching ma+: java.io.IOException: No MODULE.bazel, REPO.bazel, or WORKSPACE file found in"), "{}", out.stderr);
        assert!(out.stderr.contains("error during computation of main repo mapping: No MODULE.bazel, REPO.bazel, or WORKSPACE file found in"), "{}", out.stderr);
    }

    #[test]
    fn vendor_copies_repos() {
        let f = fixture("cmd3", ROOT);
        let vd = f.base.join("vendor");
        let out = run(&f, "vendor", &[&format!("--vendor_dir={}", vd.display())]);
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        assert_eq!(std::fs::read_to_string(vd.join("c+/data.txt")).unwrap(), "c11");
        assert!(vd.join("@c+.marker").exists());
        assert!(vd.join("VENDOR.bazel").exists());
        assert!(vd.join("bazel-external").is_symlink());
        let out = run(&f, "vendor", &[]);
        assert_eq!(out.exit_code, 2);
        assert!(out.stderr.contains("You cannot run the vendor command without specifying --vendor_dir"));
    }

    #[test]
    fn vendor_bazel_ignore_and_pin() {
        let f = fixture("cmd_vendor_ignore_pin", ROOT);
        let vd = f.base.join("vendor");
        std::fs::create_dir_all(&vd).unwrap();
        std::fs::write(
            vd.join("VENDOR.bazel"),
            "ignore('@@c+')\n",
        ).unwrap();
        let out = run(&f, "vendor", &[&format!("--vendor_dir={}", vd.display())]);
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        // c+ was ignored, so it should not be vendored
        assert!(!vd.join("c+").exists());

        // Pinning test
        std::fs::create_dir_all(vd.join("c+")).unwrap();
        std::fs::write(vd.join("c+/data.txt"), "manually_pinned_content").unwrap();
        std::fs::write(
            vd.join("VENDOR.bazel"),
            "pin('@@c+')\n",
        ).unwrap();
        let out = run(&f, "vendor", &[&format!("--vendor_dir={}", vd.display())]);
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        // c+ was pinned, so vendor must not overwrite manually modified content
        assert_eq!(std::fs::read_to_string(vd.join("c+/data.txt")).unwrap(), "manually_pinned_content");
    }

    #[test]
    fn check_direct_dependencies_flag_and_unknown_registry_module() {
        let f = fixture("cmd4", "module(name='root')\nbazel_dep(name='a', version='1.0')\nbazel_dep(name='b', version='1.0')\nbazel_dep(name='c', version='1.0')\n");
        let mut a = vec![format!("--registry={}", f.reg), "--check_direct_dependencies=error".to_string(), "graph".into()];
        let out = run_bzlmod_command("mod", &a, &f.ws, &f.ob);
        assert_eq!(out.exit_code, 37, "{}", out.stderr);
        assert!(out.stderr.contains("For repository 'c', the root module requires module version c@1.0, but got c@1.1"), "{}", out.stderr);
        a[1] = "--check_direct_dependencies=warning".into();
        let out = run_bzlmod_command("mod", &a, &f.ws, &f.ob);
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        assert!(out.stderr.contains("WARNING: For repository 'c'"));
        write(&f.ws, "MODULE.bazel", "module(name='root')\nbazel_dep(name='zzz', version='1.0')\n");
        let out = run_bzlmod_command("mod", &a, &f.ws, &f.ob);
        assert_eq!(out.exit_code, 37);
        assert!(out.stderr.contains("module zzz@1.0 not found in registries"), "{}", out.stderr);
    }

    #[test]
    fn archive_and_git_overrides_and_rctx_download() {
        crate::resolve::HERMETIC_BAZEL_TOOLS.with(|h| h.set(true));
        let base = tmpdir("cmd5");
        // archive-overridden module `arch`
        let src = base.join("arch-src");
        write(&src, "arch-1.0/MODULE.bazel", "module(name='arch', version='1.0')\nbazel_dep(name='gitmod', version='1.0')\n");
        write(&src, "arch-1.0/BUILD.bazel", "# arch\n");
        write(&src, "arch-1.0/tool.txt", "hello\n");
        let tgz = base.join("arch.tar.gz");
        assert!(std::process::Command::new("tar").args(["czf", tgz.to_str().unwrap(), "-C", src.to_str().unwrap(), "arch-1.0"]).status().unwrap().success());
        let sha = crate::hash::sha256_hex(&std::fs::read(&tgz).unwrap());
        // patch file in the workspace
        let ws = base.join("ws");
        write(&ws, "p.patch", "--- a/tool.txt\n+++ b/tool.txt\n@@ -1 +1 @@\n-hello\n+patched\n");
        // git repo for `gitmod`
        let g = base.join("gitrepo");
        write(&g, "MODULE.bazel", "module(name='gitmod', version='1.0')\n");
        write(&g, "BUILD.bazel", "# git\n");
        let git = |args: &[&str]| {
            let st = std::process::Command::new("git").args(args).current_dir(&g).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").status().unwrap();
            assert!(st.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        let commit = String::from_utf8(std::process::Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&g).output().unwrap().stdout).unwrap().trim().to_string();
        write(
            &ws,
            "MODULE.bazel",
            &format!(
                "module(name='root')\nbazel_dep(name='arch', version='1.0')\nbazel_dep(name='gitmod', version='1.0')\narchive_override(module_name='arch', urls=['file://{}'], integrity='sha256-{}', strip_prefix='arch-1.0', patches=['//:p.patch'], patch_strip=1)\ngit_override(module_name='gitmod', remote='file://{}', commit='{commit}')\n",
                tgz.display(),
                crate::hash::base64_encode(&crate::hash::from_hex(&sha).unwrap()),
                g.display()
            ),
        );
        let ob = base.join("ob");
        let a = vec!["--registry=file:///nonexistent-registry".to_string(), "--check_direct_dependencies=off".into(), "--all".to_string()];
        let out = run_bzlmod_command("fetch", &a, &ws, &ob);
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        assert_eq!(std::fs::read_to_string(ob.join("external/arch+/tool.txt")).unwrap(), "patched\n");
        assert_eq!(std::fs::read_to_string(ob.join("external/gitmod+/BUILD.bazel")).unwrap(), "# git\n");
        assert!(!ob.join("external/gitmod+/.git").exists());
        // `mod graph` sees both as `_` versions.
        let out = run_bzlmod_command("mod", &["--registry=file:///nonexistent-registry".to_string(), "--check_direct_dependencies=off".into(), "graph".into()], &ws, &ob);
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        assert!(out.stdout.contains("arch@_") && out.stdout.contains("gitmod@_"), "{}", out.stdout);
    }

    #[test]
    fn extension_facts_and_facts_version_reach_lockfile() {
        crate::resolve::HERMETIC_BAZEL_TOOLS.with(|h| h.set(true));
        let base = tmpdir("facts_test");
        let ws = base.join("ws");
        let ob = base.join("ob");
        write(
            &ws,
            "ext.bzl",
            r#"
def _impl(ctx):
    return ctx.extension_metadata(facts = {"k": "v", "n": 1})
ext = module_extension(implementation = _impl, facts_version = 1)
"#,
        );
        write(
            &ws,
            "MODULE.bazel",
            r#"
module(name = "root")
my_ext = use_extension("//:ext.bzl", "ext")
"#,
        );

        let a = vec![
            "--registry=file:///nonexistent-registry".to_string(),
            "--check_direct_dependencies=off".into(),
            "--all".to_string(),
        ];
        let out = run_bzlmod_command("fetch", &a, &ws, &ob);
        assert_eq!(out.exit_code, 0, "{}", out.stderr);

        let lock_path = ws.join("MODULE.bazel.lock");
        assert!(lock_path.exists());
        let lock_text = std::fs::read_to_string(&lock_path).unwrap();
        assert!(
            lock_text.contains("\"facts\"") && lock_text.contains("\"k\": \"v\""),
            "MODULE.bazel.lock must contain persisted facts: {lock_text}"
        );
        assert!(
            lock_text.contains("\"factsVersions\"") && lock_text.contains("\"//:ext.bzl%ext\": 1"),
            "MODULE.bazel.lock must contain factsVersions: {lock_text}"
        );
    }

    #[test]
    fn repo_contents_cache_caching_and_gc() {
        crate::resolve::HERMETIC_BAZEL_TOOLS.with(|h| h.set(true));
        let base = tmpdir("rcc_test");
        let ws = base.join("ws");
        let ob = base.join("ob");
        let rcc = base.join("rcc");

        // Create archive
        let src = base.join("archive-src");
        write(&src, "mod-1.0/MODULE.bazel", "module(name='cmod', version='1.0')\n");
        write(&src, "mod-1.0/BUILD.bazel", "# cmod\n");
        write(&src, "mod-1.0/file.txt", "content_from_archive\n");
        let tgz = base.join("mod.tar.gz");
        assert!(std::process::Command::new("tar").args(["czf", tgz.to_str().unwrap(), "-C", src.to_str().unwrap(), "mod-1.0"]).status().unwrap().success());
        let sha = crate::hash::sha256_hex(&std::fs::read(&tgz).unwrap());
        let integrity = crate::hash::base64_encode(&crate::hash::from_hex(&sha).unwrap());

        write(
            &ws,
            "MODULE.bazel",
            &format!(
                "module(name='root')\nbazel_dep(name='cmod', version='1.0')\narchive_override(module_name='cmod', urls=['file://{}'], integrity='sha256-{}', strip_prefix='mod-1.0')\n",
                tgz.display(),
                integrity
            ),
        );

        let a = vec![
            "--registry=file:///nonexistent-registry".to_string(),
            "--check_direct_dependencies=off".into(),
            format!("--repo_contents_cache={}", rcc.display()),
            "--repo=@@cmod+".to_string(),
        ];
        let out = run_bzlmod_command("fetch", &a, &ws, &ob);
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        assert_eq!(std::fs::read_to_string(ob.join("external/cmod+/file.txt")).unwrap(), "content_from_archive\n");

        // Verify rcc contains the cached repo
        assert!(rcc.exists(), "rcc dir must exist");
        let cache = crate::repo_contents_cache::RepoContentsCache::new(&rcc);
        let entries: Vec<_> = std::fs::read_dir(&rcc).unwrap().filter_map(|e| e.ok()).collect();
        assert!(!entries.is_empty(), "rcc must contain entries");

        // Delete external/ and re-fetch: must hit cache
        let _ = std::fs::remove_dir_all(ob.join("external"));
        assert!(!ob.join("external/cmod+").exists());

        let out2 = run_bzlmod_command("fetch", &a, &ws, &ob);
        assert_eq!(out2.exit_code, 0, "{}", out2.stderr);
        assert_eq!(std::fs::read_to_string(ob.join("external/cmod+/file.txt")).unwrap(), "content_from_archive\n");

        // Test GC
        cache.run_gc(std::time::Duration::from_secs(3600)).unwrap();
    }

    #[test]
    fn test_download_block_false() {
        crate::resolve::HERMETIC_BAZEL_TOOLS.with(|h| h.set(true));
        let base = tmpdir("dl_block_false");
        let ws = base.join("ws");
        let ob = base.join("ob");
        let src_file = base.join("source.txt");
        write(&base, "source.txt", "download_block_false_content\n");

        write(
            &ws,
            "rule.bzl",
            &format!(
                r#"
def _impl(ctx):
    token = ctx.download(url = ["file://{}"], output = "out.txt", block = False)
    res = token.wait()
    ctx.file("BUILD", "")
dl_repo = repository_rule(implementation = _impl)
"#,
                src_file.display()
            ),
        );
        write(
            &ws,
            "MODULE.bazel",
            r#"
module(name = "root")
r = use_repo_rule("//:rule.bzl", "dl_repo")
r(name = "my_dl")
"#,
        );

        let a = vec![
            "--registry=file:///nonexistent-registry".to_string(),
            "--check_direct_dependencies=off".into(),
            "--repo=@my_dl".to_string(),
        ];
        let out = run_bzlmod_command("fetch", &a, &ws, &ob);
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        let ext_entries: Vec<_> = std::fs::read_dir(ob.join("external")).unwrap().map(|e| e.unwrap().file_name()).collect();
        eprintln!("ob/external entries: {:?}", ext_entries);
        assert_eq!(
            std::fs::read_to_string(ob.join("external/+dl_repo+my_dl/out.txt")).unwrap(),
            "download_block_false_content\n"
        );
    }

    #[test]
    fn test_git_override_options_e2e() {
        crate::resolve::HERMETIC_BAZEL_TOOLS.with(|h| h.set(true));
        let base = tmpdir("git_opts_e2e");
        let ws = base.join("ws");
        let ob = base.join("ob");

        // Submodule repo
        let sub = base.join("subrepo");
        write(&sub, "sub.txt", "submodule_content\n");
        let git_run = |dir: &Path, args: &[&str]| {
            let st = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .status()
                .unwrap();
            assert!(st.success(), "git {args:?}");
        };
        git_run(&sub, &["init", "-q"]);
        git_run(&sub, &["add", "."]);
        git_run(&sub, &["commit", "-q", "-m", "init sub"]);

        // Main git repo with strip_prefix directory and submodule
        let g = base.join("mainrepo");
        let prefix_dir = g.join("nested/inner");
        write(&prefix_dir, "MODULE.bazel", "module(name='gitmod', version='1.0')\n");
        write(&prefix_dir, "BUILD.bazel", "# gitmod\n");
        write(&prefix_dir, "file.txt", "before_patch\n");
        git_run(&g, &["init", "-q"]);
        git_run(&g, &["add", "."]);
        git_run(&g, &["commit", "-q", "-m", "init main"]);

        let commit = String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&g)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        write(&ws, "p.patch", "--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-before_patch\n+after_patch\n");
        write(
            &ws,
            "MODULE.bazel",
            &format!(
                r#"
module(name = "root")
bazel_dep(name = "gitmod", version = "1.0")
git_override(
    module_name = "gitmod",
    remote = "file://{}",
    commit = "{commit}",
    strip_prefix = "nested/inner",
    patches = ["//:p.patch"],
    patch_strip = 1,
)
"#,
                g.display()
            ),
        );

        let a = vec![
            "--registry=file:///nonexistent-registry".to_string(),
            "--check_direct_dependencies=off".into(),
            "--repo=@@gitmod+".to_string(),
        ];
        let out = run_bzlmod_command("fetch", &a, &ws, &ob);
        assert_eq!(out.exit_code, 0, "{}", out.stderr);
        assert_eq!(
            std::fs::read_to_string(ob.join("external/gitmod+/file.txt")).unwrap(),
            "after_patch\n"
        );
        assert!(ob.join("external/gitmod+/BUILD.bazel").exists());
    }

    #[test]
    fn test_fetch_targets_closure_e2e() {
        crate::resolve::HERMETIC_BAZEL_TOOLS.with(|h| h.set(true));
        let base = tmpdir("fetch_closure");
        let ws = base.join("ws");
        let ob = base.join("ob");

        // External repo dep
        let ext = base.join("ext_src");
        write(&ext, "MODULE.bazel", "module(name = 'dep_mod', version = '1.0')\n");
        write(
            &ext,
            "BUILD.bazel",
            r#"
filegroup(
    name = "dep_file",
    srcs = ["lib.cc"],
    visibility = ["//visibility:public"],
)
"#,
        );
        write(&ext, "lib.cc", "int f() { return 42; }\n");

        write(
            &ws,
            "MODULE.bazel",
            &format!(
                r#"
module(name = "root")
bazel_dep(name = "dep_mod", version = "1.0")
local_path_override(module_name = "dep_mod", path = "{}")
"#,
                ext.display()
            ),
        );
        write(
            &ws,
            "pkg/BUILD.bazel",
            r#"
filegroup(
    name = "app",
    srcs = ["@dep_mod//:dep_file"],
)
"#,
        );
        write(&ws, "pkg/main.cc", "int main() {}\n");

        let a = vec![
            "--registry=file:///nonexistent-registry".to_string(),
            "--check_direct_dependencies=off".into(),
            "//pkg:app".to_string(),
        ];
        let out = run_bzlmod_command("fetch", &a, &ws, &ob);
        let ext_entries: Vec<_> = if ob.join("external").exists() {
            std::fs::read_dir(ob.join("external")).unwrap().map(|e| e.unwrap().file_name()).collect()
        } else {
            vec![]
        };
        eprintln!("test_fetch_targets_closure_e2e ob/external entries: {:?}", ext_entries);
        assert_eq!(out.exit_code, 0, "fetch //pkg:app should succeed: {}", out.stderr);
        assert!(
            ob.join("external/dep_mod+/lib.cc").exists(),
            "external repo dep_mod+ must be fetched"
        );
    }

    #[test]
    fn test_downloader_urlencode_rewrite_e2e() {
        use crate::download::UrlRewriter;
        let temp = std::env::temp_dir().join(format!("urlencode_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();

        // Create downloader config with rewrite using urlencode
        let cfg_file = temp.join("downloader.cfg");
        std::fs::write(
            &cfg_file,
            "rewrite repo.example.com/(.*) mirrors.example.com/${urlencode($1)}\n",
        ).unwrap();

        let rewriter = UrlRewriter::parse_file(&cfg_file).unwrap();
        let input_urls = vec!["https://repo.example.com/foo bar+baz.tar.gz".to_string()];
        let rewritten = rewriter.amend(&input_urls);
        assert_eq!(rewritten.len(), 1);
        assert_eq!(rewritten[0], "https://mirrors.example.com/foo%20bar%2Bbaz.tar.gz");
    }
}
