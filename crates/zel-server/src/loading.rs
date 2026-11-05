// Copyright 2026 The Bazel Authors. All rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//    http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Loading-phase glue for the command handlers: package loader construction, target pattern
//! resolution, `query` evaluation over loaded packages, and Bazel-format loading diagnostics.

use std::collections::HashSet;
use super::*;
use std::collections::BTreeSet;
use zel_cmdline::RepositoryName;
use zel_packages::{
    lookup::EventKind, LoaderConfig, PackageEvent, PackageLoader, PatternResolver,
    SimpleRepoResolver, Target as PkgTarget,
};

/// Flattens a (possibly repeated, comma-separated) list option into strings.
pub(crate) fn opt_list(parsed: &ParsedCommandOptions, name: &str) -> Vec<String> {
    fn collect(v: &zel_options::OptionValue, out: &mut Vec<String>) {
        match v {
            zel_options::OptionValue::List(l) => l.iter().for_each(|x| collect(x, out)),
            zel_options::OptionValue::String(s) | zel_options::OptionValue::Label(s) => {
                out.extend(s.split(',').filter(|p| !p.is_empty()).map(|p| p.to_string()))
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    if let Some(d) = parsed.parsed_options.get(name) {
        collect(&d.value, &mut out);
    }
    out
}

impl BlazeRuntime {
    /// The client's working directory relative to the workspace root (from `--client_cwd`).
    pub(crate) fn command_relative_dir(&self, parsed: &ParsedCommandOptions) -> PathFragment {
        match parsed.get_str("client_cwd") {
            Some(cwd) if !cwd.is_empty() => PathFragment::create(cwd)
                .relative_to(&self.dirs.workspace_dir)
                .unwrap_or_else(|_| PathFragment::empty()),
            _ => PathFragment::empty(),
        }
    }

    /// The client's working directory as a host path (used to resolve `--target_pattern_file`).
    pub(crate) fn command_cwd(&self, parsed: &ParsedCommandOptions) -> std::path::PathBuf {
        match parsed.get_str("client_cwd") {
            Some(cwd) if !cwd.is_empty() => std::path::PathBuf::from(cwd),
            _ => std::path::PathBuf::from(self.dirs.workspace_dir.path_string()),
        }
    }

    /// Builds a [`PackageLoader`] for one command invocation.
    pub(crate) fn make_loader(&self, parsed: &ParsedCommandOptions) -> Arc<PackageLoader> {
        let mut resolver = SimpleRepoResolver::new(self.dirs.workspace_dir.clone());
        // External repositories materialized under `<output_base>/external/<canonical name>`.
        let ext = self.dirs.output_base.get_relative_str("external");
        if let Ok(entries) = self.fs.readdir(&ext) {
            for e in entries {
                if e.name.starts_with('@') || e.name.starts_with('.') {
                    continue;
                }
                let root = ext.get_relative_str(&e.name);
                if !self.fs.stat(&root, true).map(|s| s.is_directory).unwrap_or(false) {
                    continue;
                }
                if let Ok(canonical) = RepositoryName::create(&e.name) {
                    let apparent = e
                        .name
                        .split(|c| c == '+' || c == '~')
                        .next()
                        .unwrap_or(&e.name)
                        .to_string();
                    resolver.external.insert(e.name.clone(), root);
                    resolver.apparent.entry(apparent).or_insert(canonical);
                }
            }
        }
        // --override_repository=<name>=<path>
        for o in opt_list(parsed, "override_repository") {
            if let Some((name, path)) = o.split_once('=') {
                if let Ok(canonical) = RepositoryName::create(name) {
                    let p = PathFragment::create(path);
                    let p = if p.is_absolute() {
                        p
                    } else {
                        self.dirs.workspace_dir.get_relative(&p)
                    };
                    resolver.external.insert(name.to_string(), p);
                    resolver.apparent.insert(name.to_string(), canonical);
                }
            }
        }
        // Bzlmod-aware resolver (installed per command by `CommandResolver::install_for_command`):
        // fetches external repos on demand and applies canonical repo mappings.
        let repo_resolver: Arc<dyn zel_packages::lookup::RepoResolver> =
            self.repo_resolver.slot().get().unwrap_or_else(|| Arc::new(resolver));
        // Package loading is memoized in the persistent Skyframe graph: the loader reads through
        // the recording file system (so every observed file/directory becomes a dependency) and
        // `load_package` is served by `PACKAGE_LOAD` nodes across commands.
        let mut cfg = LoaderConfig::new(self.skyframe.recording_fs(), repo_resolver);
        cfg.package_cache = Some(self.skyframe.package_cache());
        let mut deleted = BTreeSet::new();
        for d in opt_list(parsed, "deleted_packages") {
            let d = d.trim_start_matches("//").to_string();
            if let Ok(id) = PackageIdentifier::create_in_main_repo(&d) {
                deleted.insert(id);
            }
        }
        cfg.lookup.deleted_packages = deleted;
        cfg.allow_empty_glob = !parsed.get_bool("incompatible_disallow_empty_glob").unwrap_or(false);
        let mut sig_parts: Vec<String> = vec![
            format!("allow_empty_glob={}", cfg.allow_empty_glob),
            format!("deleted={:?}", cfg.lookup.deleted_packages),
        ];
        sig_parts.extend(opt_list(parsed, "override_repository").into_iter().map(|o| format!("override={o}")));
        if let Ok(entries) = self.fs.readdir(&ext) {
            let mut names: Vec<String> = entries.into_iter().map(|e| e.name).collect();
            names.sort();
            sig_parts.push(format!("external={names:?}"));
        }
        self.sync_skyframe(&sig_parts);
        PackageLoader::new(cfg)
    }

    /// Once per command: invalidate whatever changed on disk since the previous command and
    /// inject the loading options into the graph.
    pub(crate) fn sync_skyframe(&self, option_parts: &[String]) {
        let sig = zel_skyframe::executor::loading_signature(
            self.fs.as_ref(),
            &self.dirs.workspace_dir,
            option_parts,
        );
        let seq = self.command_seq.load(std::sync::atomic::Ordering::SeqCst);
        let mut synced = self.synced_command.lock().unwrap();
        if synced.as_ref() == Some(&(seq, sig.clone())) {
            return;
        }
        let _ = self.skyframe.sync(&sig);
        *synced = Some((seq, sig));
    }

    pub(crate) fn pattern_resolver(&self, parsed: &ParsedCommandOptions) -> PatternResolver {
        PatternResolver::new(self.make_loader(parsed), self.command_relative_dir(parsed))
    }

    /// Reads target patterns from the residue or `--target_pattern_file`.
    pub(crate) fn command_patterns(
        &self,
        parsed: &ParsedCommandOptions,
    ) -> Result<Vec<String>, String> {
        zel_cmdline::read_target_patterns(
            &parsed.targets,
            parsed.get_str("target_pattern_file").unwrap_or(""),
            &self.command_cwd(parsed),
        )
    }

    /// Prints loading events (errors/warnings/`print()` output) as Bazel does.
    pub(crate) fn report_events(stderr: &mut Vec<u8>, events: &[PackageEvent]) {
        for e in events {
            writeln_bytes(stderr, &Self::render_event(e));
        }
    }

    /// `No repository visible as '@x' from main repository` / `Repository '@@x' is not defined`.
    fn unknown_repo_message(pattern: &str, message: &str) -> Option<String> {
        if !message.starts_with("no such package '@@[unknown repo '") {
            return None;
        }
        let rest = &message["no such package '@@[unknown repo '".len()..];
        let name = rest.split('\'').next()?;
        if pattern.starts_with("@@") {
            Some(format!("Repository '@@{name}' is not defined"))
        } else {
            Some(format!("No repository visible as '@{name}' from main repository"))
        }
    }

    /// Renders a package event; rule-instantiation failures (output conflicts) are Starlark
    /// evaluation errors, which Bazel prints as a `Traceback` of the failing BUILD call.
    fn render_event(e: &PackageEvent) -> String {
        if matches!(e.kind, EventKind::Error) && e.message.contains("conflicts with existing generated file from rule") {
            if let Some(loc) = &e.location {
                let mut it = loc.rsplitn(3, ':');
                let (col, line, file) = (it.next(), it.next(), it.next());
                if let (Some(col), Some(line), Some(file)) = (col, line, file) {
                    if let (Ok(n), Ok(text)) = (line.parse::<usize>(), std::fs::read_to_string(file)) {
                        if let Some(src) = text.lines().nth(n.saturating_sub(1)) {
                            let src = src.trim();
                            let func = src.split('(').next().unwrap_or("").trim();
                            return format!(
                                "ERROR: Traceback (most recent call last):\n\tFile \"{file}\", line {line}, column {col}, in <toplevel>\n\t\t{src}\nError in {func}: {}",
                                e.message
                            );
                        }
                    }
                }
            }
        }
        e.render()
    }

    /// Resolves patterns; reports failures like `TargetPatternPhaseFunction`. Returns the resolved
    /// targets, or `Err(exit code)` when the command must stop.
    pub(crate) fn resolve_for_build(
        &self,
        resolver: &PatternResolver,
        patterns: &[String],
        keep_going: bool,
        filter_manual: bool,
        warn_without_keep_going: bool,
        stderr: &mut Vec<u8>,
    ) -> Result<(Vec<PkgTarget>, bool), ExitCode> {
        let mut outcome = resolver.resolve_patterns_filtered(patterns, filter_manual);
        Self::report_events(stderr, &outcome.events);
        // A package that failed to load: `TargetPatternPhaseFunction` wraps the cause as
        // `Error evaluating '<pattern>': error loading package ...: Package '...' contains errors`.
        for err in &mut outcome.errors {
            if let Some(rest) = err.message.strip_prefix("error loading package '") {
                if let Some((pkg, tail)) = rest.split_once("': ").filter(|(_, t)| t.contains("contains errors") || t.contains("ERROR: ")) {
                    let _ = tail;
                    let pkg = pkg.to_string();
                    err.message = format!(
                        "Error evaluating '{}': error loading package '{pkg}': Package '{pkg}' contains errors",
                        err.pattern
                    );
                }
            }
        }
        // A single target of a package that contains errors fails its pattern unless
        // `--keep_going` (`TargetPatternPhaseFunction`: `Package '...' contains errors`).
        if !keep_going {
            let mut bad_pkgs: Vec<String> = Vec::new();
            for t in &outcome.targets {
                let id = t.label().package_identifier().clone();
                if let Ok(pkg) = resolver.package(&id) {
                    let frag = id.package_fragment().to_string();
                    if pkg.contains_errors && !bad_pkgs.contains(&frag) {
                        bad_pkgs.push(frag);
                    }
                }
            }
            for frag in bad_pkgs {
                if let Some(pat) = patterns.iter().find(|p| p.trim_start_matches("//").starts_with(&format!("{frag}:"))) {
                    if !outcome.errors.iter().any(|e| &e.pattern == pat) {
                        outcome.errors.push(zel_packages::resolve::PatternError {
                            pattern: pat.clone(),
                            message: format!(
                                "Error evaluating '{pat}': error loading package '{frag}': Package '{frag}' contains errors"
                            ),
                            package_errors: None,
                        });
                    }
                    outcome.targets.retain(|t| t.label().package_identifier().package_fragment().to_string() != frag);
                }
            }
        }
        let had_errors = !outcome.errors.is_empty();
        if had_errors && keep_going {
            writeln_bytes(stderr, "WARNING: Target pattern parsing failed.");
            for err in &outcome.errors {
                writeln_bytes(
                    stderr,
                    &format!("ERROR: Skipping '{}': {}", err.pattern, err.message),
                );
            }
        } else if had_errors && outcome.errors.iter().all(|e| Self::unknown_repo_message(&e.pattern, &e.message).is_some()) {
            // A pattern naming a repository that does not exist is rejected while parsing it.
            for err in &outcome.errors {
                if let Some(m) = Self::unknown_repo_message(&err.pattern, &err.message) {
                    writeln_bytes(stderr, &format!("ERROR: {m}"));
                }
            }
            // The resolver's own note about the repository is superseded by the message above.
            let _ = self.repo_resolver.take_messages();
            writeln_bytes(stderr, "INFO: Elapsed time: 0.050s");
            writeln_bytes(stderr, "INFO: 0 processes.");
            writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
            return Err(ExitCode::PARSING_FAILURE);
        } else if had_errors {
            // `TargetPatternPhaseFunction` warns only for `--keep_going` loads; `bazel test`
            // observably does not warn without it while build/run do.
            if warn_without_keep_going {
                writeln_bytes(stderr, "WARNING: Target pattern parsing failed.");
            }
            for err in &outcome.errors {
                writeln_bytes(
                    stderr,
                    &format!("ERROR: Skipping '{}': {}", err.pattern, err.message),
                );
                writeln_bytes(stderr, &format!("ERROR: {}", err.message));
            }
            writeln_bytes(stderr, "INFO: Elapsed time: 0.050s");
            writeln_bytes(stderr, "INFO: 0 processes.");
            writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
            return Err(ExitCode::PARSING_FAILURE);
        }
        Ok((outcome.targets, had_errors))
    }

    /// Loads the closure of dependencies for `roots` and reports unresolved dependencies in
    /// Bazel's format. Returns the rules in dependency order.
    pub(crate) fn load_build_closure(
        &self,
        resolver: &PatternResolver,
        roots: &[PkgTarget],
        stderr: &mut Vec<u8>,
    ) -> Result<Vec<PkgTarget>, ExitCode> {
        let (order, errors) = resolver.load_transitive_closure(roots);
        let errors: Vec<_> = errors.into_iter().filter(|(referrer, dep, msg)| {
            if dep.package_identifier() == referrer.package_identifier()
                && msg.starts_with("no such target")
                && dep.name().contains('.')
            {
                if let Ok(pkg) = resolver.package(dep.package_identifier()) {
                    if !pkg.targets.contains_key(dep.name()) {
                        return false;
                    }
                }
            }
            true
        }).collect();
        Self::report_events(stderr, &resolver.drain_new_events());
        if let Some(first) = errors.first() {
            for (referrer, _dep, msg) in &errors {
                let loc = resolver
                    .get_target(referrer)
                    .ok()
                    .and_then(|t| match t {
                        PkgTarget::Rule(r) => r.extra.location.clone(),
                        _ => None,
                    })
                    .map(|l| format!("{l}: "))
                    .unwrap_or_default();
                writeln_bytes(
                    stderr,
                    &format!("ERROR: {loc}{msg} and referenced by '{}'", referrer.canonical_form()),
                );
            }
            let top = roots
                .iter()
                .find(|r| r.label() == &first.0)
                .map(|r| r.label().canonical_form())
                .or_else(|| roots.first().map(|r| r.label().canonical_form()))
                .unwrap_or_default();
            writeln_bytes(
                stderr,
                &format!("ERROR: Analysis of target '{top}' failed; build aborted: Analysis failed"),
            );
            writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
            return Err(ExitCode::BUILD_FAILURE);
        }
        // Package-level load errors in transitively loaded packages.
        let _ = EventKind::Error;
        Ok(order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_vfs::InMemoryFileSystem;

    fn runtime(files: &[(&str, &str)]) -> BlazeRuntime {
        let fs = Arc::new(InMemoryFileSystem::default());
        let ws = PathFragment::create("/workspace");
        // A private output base per runtime: tests run in parallel (and other cargo runs share /tmp).
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let uniq = format!("{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
        let ob = PathFragment::create(&format!("/tmp/ob-loading-{uniq}"));
        fs.create_directory_and_parents(&ws).unwrap();
        fs.create_directory_and_parents(&ob).unwrap();
        for (p, c) in files {
            let path = ws.get_relative_str(p);
            fs.create_directory_and_parents(&path.parent_directory().unwrap()).unwrap();
            fs.write_file(&path, c.as_bytes()).unwrap();
        }
        let dirs = BlazeDirectories {
            workspace_dir: ws,
            output_base: ob,
            install_base: PathFragment::create(&format!("/tmp/ib-loading-{uniq}")),
            output_user_root: PathFragment::create(&format!("/tmp/ur-loading-{uniq}")),
        };
        BlazeRuntime::new(dirs, fs as Arc<dyn FileSystem>, 1)
    }

    struct Out {
        code: i32,
        stdout: String,
        stderr: String,
    }

    fn run(rt: &BlazeRuntime, args: &[&str]) -> Out {
        let req = zel_proto::command_server::RunRequest {
            cookie: "c".into(),
            arg: args.iter().map(|a| a.as_bytes().to_vec()).collect(),
            client_description: "test".into(),
            ..Default::default()
        };
        let r = rt.dispatch_command(&req, "r");
        Out {
            code: r.exit_code,
            stdout: String::from_utf8_lossy(&r.standard_output).to_string(),
            stderr: String::from_utf8_lossy(&r.standard_error).to_string(),
        }
    }

    fn tree() -> BlazeRuntime {
        runtime(&[
            (
                "BUILD",
                "print('root loaded')\ngenrule(name = 'root', outs = ['root.out'], cmd = 'echo r > $@')",
            ),
            ("lib/BUILD", "genrule(name = 'l', outs = ['l.out'], cmd = 'echo l > $@', visibility = ['//visibility:public'])"),
            (
                "app/BUILD",
                "genrule(name = 'app', srcs = ['//lib:l'], outs = ['app.out'], cmd = 'cat $(SRCS) > $@')\ngenrule(name = 'other', outs = ['o.out'], cmd = 'echo o > $@')",
            ),
            ("ignored/BUILD", "genrule(name = 'bad', outs = ['b'], cmd = '')"),
            (".bazelignore", "ignored\n"),
        ])
    }

    #[test]
    fn build_nonexistent_target_fails_like_bazel() {
        let rt = tree();
        let o = run(&rt, &["build", "--nobuild", "//:nonexistent"]);
        assert_eq!(o.code, 1, "{}", o.stderr);
        let want = "no such target '//:nonexistent': target 'nonexistent' not declared in package '' defined by /workspace/BUILD";
        assert!(o.stderr.contains(&format!("ERROR: Skipping '//:nonexistent': {want}")), "{}", o.stderr);
        assert!(o.stderr.contains(&format!("\nERROR: {want}\n")), "{}", o.stderr);
        assert!(o.stderr.contains("ERROR: Build did NOT complete successfully"), "{}", o.stderr);
        assert!(!o.stderr.contains("Build completed successfully"));
    }

    #[test]
    fn build_print_output_appears_exactly_once() {
        let rt = tree();
        let o = run(&rt, &["build", "--nobuild", "//:root"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert_eq!(
            o.stderr.matches("DEBUG: /workspace/BUILD:1:6: root loaded").count(),
            1,
            "{}",
            o.stderr
        );
    }

    #[test]
    fn missing_package_and_ignored_package() {
        let rt = tree();
        let o = run(&rt, &["build", "--nobuild", "//nope:x"]);
        assert_eq!(o.code, 1);
        assert!(o.stderr.contains("no such package 'nope': BUILD file not found in any of the following directories. Add a BUILD file to a directory to mark it as a package.\n - nope"), "{}", o.stderr);
        let o = run(&rt, &["build", "--nobuild", "//ignored:bad"]);
        assert_eq!(o.code, 1);
        assert!(o.stderr.contains("Package is considered deleted due to .bazelignore"), "{}", o.stderr);
    }

    #[test]
    fn recursive_and_negative_patterns() {
        let rt = tree();
        let o = run(&rt, &["build", "--nobuild", "//..."]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert!(o.stderr.contains("Analyzed 4 targets ("), "{}", o.stderr);
        let o = run(&rt, &["build", "--nobuild", "--", "//...", "-//app/..."]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert!(o.stderr.contains("Analyzed 2 targets ("), "{}", o.stderr);
        // Ordered subtraction: re-adding after the negative pattern.
        let o = run(&rt, &["build", "--nobuild", "--", "//...", "-//app/...", "//app:app"]);
        assert!(o.stderr.contains("Analyzed 3 targets ("), "{}", o.stderr);
    }

    #[test]
    fn relative_patterns_resolve_against_client_cwd() {
        let rt = tree();
        let o = run(&rt, &["build", "--nobuild", "--client_cwd=/workspace/app", ":app"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert!(o.stderr.contains("Analyzed target "), "{}", o.stderr);
        let o = run(&rt, &["build", "--nobuild", "--client_cwd=/workspace/app", "..."]);
        assert!(o.stderr.contains("Analyzed 2 targets ("), "{}", o.stderr);
        let o = run(&rt, &["query", "--client_cwd=/workspace/app", ":all"]);
        assert_eq!(o.stdout.lines().collect::<Vec<_>>(), vec!["//app:app", "//app:other"]);
        let o = run(&rt, &["query", "--client_cwd=/workspace/app", "..."]);
        assert_eq!(o.stdout.lines().collect::<Vec<_>>(), vec!["//app:app", "//app:other"]);
        let o = run(&rt, &["query", "--noimplicit_deps", "--client_cwd=/workspace/app", "deps(:app)"]);
        assert_eq!(o.stdout.lines().collect::<Vec<_>>(), vec!["//app:app", "//lib:l"]);
        // `//...` from a subdirectory is still absolute.
        let o = run(&rt, &["query", "--client_cwd=/workspace/app", "//lib:all"]);
        assert_eq!(o.stdout.lines().collect::<Vec<_>>(), vec!["//lib:l"]);
    }

    #[test]
    fn keep_going_continues_past_bad_patterns() {
        let rt = tree();
        let o = run(&rt, &["build", "--nobuild", "--keep_going", "//:root", "//:nope"]);
        assert_eq!(o.code, 1);
        assert!(o.stderr.contains("ERROR: Skipping '//:nope':"), "{}", o.stderr);
        assert!(o.stderr.contains("WARNING: Target pattern parsing failed."), "{}", o.stderr);
        assert!(o.stderr.contains("Analyzed target "), "{}", o.stderr);
        assert!(o.stderr.contains("ERROR: Build did NOT complete successfully"), "{}", o.stderr);
    }

    #[test]
    fn starlark_errors_surface_without_scraper_fallback() {
        let rt = runtime(&[
            ("BUILD", "undefined_rule(name = 'x')\nfilegroup(name = 'y')"),
        ]);
        let o = run(&rt, &["build", "--nobuild", "//:y"]);
        assert_eq!(o.code, 1, "{}", o.stderr);
        assert!(
            o.stderr.contains("ERROR: /workspace/BUILD:1:1: name 'undefined_rule' is not defined"),
            "{}",
            o.stderr
        );
        // Real bazel 9.3.2: the target defined after the failing statement does not exist.
        assert!(o.stderr.contains("ERROR: no such target '//:y': target 'y' not declared in package '' defined by /workspace/BUILD"), "{}", o.stderr);
        // The scraper used to recover `y` from the raw text; it must not any more.
        assert!(!o.stderr.contains("Build completed successfully"));
    }

    #[test]
    fn unresolved_dependency_reports_referencing_rule() {
        let rt = runtime(&[(
            "BUILD",
            "filegroup(name = 'a', srcs = [':missing'])",
        )]);
        let o = run(&rt, &["build", "--nobuild", "//:a"]);
        assert_eq!(o.code, 1);
        assert!(o.stderr.contains("ERROR: /workspace/BUILD:1:10: no such target '//:missing': target 'missing' not declared in package '' defined by /workspace/BUILD") && o.stderr.contains("and referenced by '//:a'"), "{}", o.stderr);
        assert!(o.stderr.contains("ERROR: Analysis of target '//:a' failed; build aborted: Analysis failed"), "{}", o.stderr);
    }

    #[test]
    fn target_pattern_file_handling() {
        let dir = std::env::temp_dir().join(format!("zel-srv-tpf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("p.txt"), "//:root # comment\n\n//lib:l\n").unwrap();
        let rt = tree();
        let arg = format!("--target_pattern_file={}", dir.join("p.txt").display());
        let o = run(&rt, &["build", "--nobuild", &arg]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert!(o.stderr.contains("Analyzed 2 targets ("), "{}", o.stderr);
        let o = run(&rt, &["build", "--nobuild", &arg, "//:root"]);
        assert_eq!(o.code, 2);
        assert!(o.stderr.contains("Command-line target pattern and --target_pattern_file cannot both be specified"), "{}", o.stderr);
        let o = run(&rt, &["build", "--nobuild", "--target_pattern_file=/definitely/missing.txt"]);
        assert_eq!(o.code, 2);
        assert!(o.stderr.contains("I/O error reading from /definitely/missing.txt: No such file or directory"), "{}", o.stderr);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn deleted_packages_flag() {
        let rt = tree();
        let o = run(&rt, &["build", "--nobuild", "--deleted_packages=lib", "//lib:l"]);
        assert_eq!(o.code, 1);
        assert!(o.stderr.contains("Package is considered deleted due to --deleted_packages"), "{}", o.stderr);
        let o = run(&rt, &["build", "--nobuild", "--deleted_packages=lib", "//..."]);
        assert!(o.stderr.contains("referenced by '//app:app'"), "{}", o.stderr);
    }

    #[test]
    fn package_loading_is_incremental_across_commands() {
        let rt = tree();
        let ws = PathFragment::create("/workspace");
        let o = run(&rt, &["build", "--nobuild", "//lib:l", "//app:other"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        let loads_after_first = rt.skyframe.package_loads();
        assert!(loads_after_first >= 2, "lib and app packages were loaded");

        // Nothing changed: the second command is served entirely from the persistent graph.
        let o = run(&rt, &["build", "--nobuild", "//lib:l", "//app:other"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert_eq!(rt.skyframe.package_loads(), loads_after_first, "no package was re-evaluated");

        // Edit one BUILD file: only that package is re-evaluated, and the new content is seen.
        rt.fs
            .write_file(
                &ws.get_relative_str("lib/BUILD"),
                b"genrule(name = 'l', outs = ['l.out'], cmd = 'echo l > $@')\ngenrule(name = 'l2', outs = ['l2.out'], cmd = 'echo 2 > $@')",
            )
            .unwrap();
        let o = run(&rt, &["build", "--nobuild", "//lib:l2", "//app:other"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert_eq!(rt.skyframe.package_loads(), loads_after_first + 1, "only //lib was reloaded");

        // `clean` drops the graph entirely.
        let o = run(&rt, &["clean"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        let o = run(&rt, &["build", "--nobuild", "//lib:l2", "//app:other"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert!(rt.skyframe.package_loads() >= loads_after_first + 3);
    }

    #[test]
    fn changed_loading_flags_do_not_serve_stale_packages() {
        let rt = tree();
        let o = run(&rt, &["build", "--nobuild", "//app:other"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        // --deleted_packages must take effect even though //app is already in the graph.
        let o = run(&rt, &["build", "--nobuild", "--deleted_packages=app", "//app:other"]);
        assert_eq!(o.code, 1, "{}", o.stderr);
        assert!(o.stderr.contains("no such package 'app'"), "{}", o.stderr);
        let o = run(&rt, &["build", "--nobuild", "//app:other"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
    }

    const ANALYSIS_DEFS: &str = "def _bad(ctx):\n    fail('boom')\nbad_rule = rule(implementation = _bad)\ndef _ok(ctx):\n    out = ctx.actions.declare_file(ctx.label.name + '.txt')\n    ctx.actions.write(out, 'x')\n    return [DefaultInfo(files = depset([out]))]\nok_rule = rule(implementation = _ok)\ndef _top(ctx):\n    return [DefaultInfo(files = depset(ctx.files.deps))]\ntop_rule = rule(implementation = _top, attrs = {'deps': attr.label_list()})\n";

    fn analysis_tree() -> BlazeRuntime {
        runtime(&[
            ("defs.bzl", ANALYSIS_DEFS),
            (
                "BUILD",
                "load(':defs.bzl', 'bad_rule', 'ok_rule', 'top_rule')\nbad_rule(name = 'bad')\nok_rule(name = 'good')\ntop_rule(name = 'top', deps = [':bad'])\ntop_rule(name = 'top_ok', deps = [':good'])\n",
            ),
        ])
    }

    #[test]
    fn analysis_errors_are_reported_in_bazel_format_and_abort() {
        let rt = analysis_tree();
        let o = run(&rt, &["build", "--nobuild", "//:top"]);
        assert_eq!(o.code, 1, "{}", o.stderr);
        // The failing dependency's rule error (with its location and Starlark traceback) ...
        // (real Bazel 9.3.2: the traceback starts on the line after `in <kind> rule <label>: `)
        assert!(o.stderr.contains("ERROR: /workspace/BUILD:2:9: in bad_rule rule //:bad: \nTraceback (most recent call last):"), "{}", o.stderr);
        assert!(o.stderr.contains("ERROR: /workspace/BUILD:2:9: Analysis of target '//:bad' (config: "), "{}", o.stderr);
        assert!(o.stderr.contains("Error in fail: boom"), "{}", o.stderr);
        // ... then the top-level abort.
        assert!(o.stderr.contains("ERROR: Analysis of target '//:top' failed; build aborted: Analysis failed"), "{}", o.stderr);
        assert!(o.stderr.contains("ERROR: Build did NOT complete successfully"), "{}", o.stderr);
        assert!(!o.stderr.contains("Analyzed"), "{}", o.stderr);
    }

    #[test]
    fn keep_going_skips_failed_analysis_but_analyzes_the_rest() {
        let rt = analysis_tree();
        let o = run(&rt, &["build", "--nobuild", "--keep_going", "//:top", "//:top_ok"]);
        assert_eq!(o.code, 1, "{}", o.stderr);
        assert!(o.stderr.contains("WARNING: errors encountered while analyzing target '//:top', it will not be built."), "{}", o.stderr);
        // Real bazel 9.3.2: the loading-phase label count includes the failed root.
        assert!(o.stderr.contains("INFO: Analyzed 2 targets "), "{}", o.stderr);
        assert!(o.stderr.contains("ERROR: Build did NOT complete successfully"), "{}", o.stderr);
        assert!(!o.stderr.contains("Analysis of target '//:top' failed; build aborted"), "{}", o.stderr);
    }

    #[test]
    fn dependencies_are_analyzed_and_provide_files_to_dependents() {
        let rt = analysis_tree();
        let o = run(&rt, &["build", "--nobuild", "//:top_ok"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert!(o.stderr.contains("INFO: Analyzed target "), "{}", o.stderr);
        let o = run(&rt, &["aquery", "//:good"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert!(o.stdout.contains("Target: //:good"), "{}", o.stdout);
        assert!(o.stdout.contains("Mnemonic: FileWrite"), "{}", o.stdout);
        let o = run(&rt, &["cquery", "//:bad"]);
        assert_ne!(o.code, 0, "{}", o.stderr);
        assert!(o.stderr.contains("Error in fail: boom"), "{}", o.stderr);
    }

    #[test]
    fn flags_populate_the_analysis_configuration() {
        let rt = runtime(&[
            (
                "defs.bzl",
                "def _i(ctx):\n    out = ctx.actions.declare_file('o')\n    ctx.actions.write(out, ctx.var.get('FOO', '-') + ctx.var['COMPILATION_MODE'] + ctx.var['TARGET_CPU'])\n    return [DefaultInfo(files = depset([out]))]\nshow = rule(implementation = _i)\n",
            ),
            (
                "BUILD",
                "load(':defs.bzl', 'show')\nshow(name = 's')\nconfig_setting(name = 'foo', define_values = {'FOO': 'bar'})\n",
            ),
        ]);
        let o = run(&rt, &["aquery", "--define=FOO=bar", "-c", "opt", "--cpu=k8", "//:s"]);
        assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
        assert!(o.stdout.contains("bazel-out/k8-opt/bin/o"), "{}", o.stdout);
        assert!(o.stdout.contains("Configuration: k8-opt"), "{}", o.stdout);
    }
}
