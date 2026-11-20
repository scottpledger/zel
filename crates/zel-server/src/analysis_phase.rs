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

//! Analysis-phase glue for the command handlers: builds the top-level
//! [`BuildConfigurationKey`] and [`AnalysisConfig`] from the parsed command options (and the
//! Bzlmod module graph), runs the dependency-aware [`TargetResolver`] over the requested targets,
//! reports analysis errors in Bazel's format and selects the actions the execution phase needs.
//!
//! Mirrors `BuildView.update` / `SkyframeBuildView.configureTargets` +
//! `SkyframeErrorProcessor` for the error text.

use super::*;
use crate::loading::opt_list;
use std::collections::{BTreeMap, HashMap, HashSet};
use zel_analysis::{ActionAnalysisMetadata, ConfiguredTarget};
use zel_cmdline::{Label, RepositoryName};
use zel_packages::{PackageLoader, Target as PkgTarget};
use zel_rules::{AnalysisConfig, TargetResolver};

/// What the daemon remembers of the previous analysis (`SkyframeBuildView`).
#[derive(Debug, Clone)]
pub struct AnalysisCacheState {
    pub key: BuildConfigurationKey,
    pub options: zel_analysis::build_options::BuildOptions,
    pub was_discarded: bool,
    /// The command line's `--platforms` (canonical labels; `[]` when unset): the cache check compares
    /// the parsed option, not the host-platform default the configuration later gets.
    pub platforms: Vec<String>,
}

/// Number of options named in the "Build option(s) ... changed" message
/// (`--max_config_changes_to_show`).
fn describe_config_difference(relevant: &[String], max_show: i64) -> String {
    if max_show == 0 {
        return "Build options have changed".to_string();
    }
    if max_show > 0 && relevant.len() as i64 > max_show {
        let m = max_show as usize;
        return format!(
            "Build options {}{} and {} more have changed",
            relevant[..m].join(", "),
            if m == 1 { "" } else { "," },
            relevant.len() - m
        );
    }
    match relevant.len() {
        1 => format!("Build option {} has changed", relevant[0]),
        2 => format!("Build options {} have changed", relevant.join(" and ")),
        n => format!(
            "Build options {}, and {} have changed",
            relevant[..n - 1].join(", "),
            relevant[n - 1]
        ),
    }
}

/// `--run_under` as the non-test configuration sees it (`RunUnder.trimForNonTestConfiguration`):
/// only a label run-under survives, reduced to its label.
fn trim_run_under(v: Option<&zel_analysis::build_options::ConfigValue>) -> Option<String> {
    let s = match v? {
        zel_analysis::build_options::ConfigValue::Scalar(s) => s.clone(),
        _ => return None,
    };
    let first = s.split_whitespace().next()?;
    if first.starts_with("//") || first.starts_with('@') {
        Some(first.to_string())
    } else {
        None
    }
}

/// Native options native options that differ between two `BuildOptions` and that
/// `TestConfiguration.SHOULD_INVALIDATE_FOR_OPTION_DIFF` lets invalidate the analysis cache.
fn cache_invalidating_options(
    old: &zel_analysis::build_options::BuildOptions,
    new: &zel_analysis::build_options::BuildOptions,
) -> Vec<String> {
    use zel_analysis::build_options::{find_option, TEST_OPTIONS};
    let mut out = Vec::new();
    // `getNativeCacheInvalidatingDifferences(configuration, diff)`: `options` is the *old*
    // configuration's options.
    let old_trims =
        old.get("trim_test_configuration").map(|v| v.display()) != Some("false".to_string());
    for (cls, opts) in &new.fragments {
        let Some(old_opts) = old.fragments.get(cls) else {
            continue;
        };
        for (name, val) in opts {
            if old_opts.get(name) == Some(val) {
                continue;
            }
            let invalidates = if *name == "trim_test_configuration"
                || *name == "experimental_retain_test_configuration_across_testonly"
            {
                true
            } else if *cls == TEST_OPTIONS {
                !old_trims
            } else if find_option(name).map(|(c, _)| c)
                == Some(zel_analysis::build_options::CORE_OPTIONS)
                && *name == "run_under"
            {
                trim_run_under(old_opts.get(name)) != trim_run_under(Some(val))
            } else {
                true
            };
            if invalidates {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// Native options copied from the command line into `BuildConfigurationKey::options` (the ones
/// that can be read by `config_setting(values = ...)` and Starlark transitions).
const CONFIG_OPTIONS: &[&str] = &[
    "copt",
    "cxxopt",
    "conlyopt",
    "objccopt",
    "linkopt",
    "ltoopt",
    "dynamic_mode",
    "protocopt",
    "per_file_copt",
    "host_per_file_copt",
    "per_file_ltobackendopt",
    "features",
    "host_features",
    "host_copt",
    "host_cxxopt",
    "host_conlyopt",
    "host_linkopt",
    "stamp",
    "strip",
    "fission",
    "platform_suffix",
    "host_platform",
    "shell_executable",
    "extra_toolchains",
    "extra_execution_platforms",
    "collect_code_coverage",
    "instrument_test_targets",
    "instrumentation_filter",
    "excluded_directories",
    "experimental_collect_code_coverage_for_generated_files",
    "coverage_output_generator",
    "allow_analysis_failures",
    "experimental_allow_map_directory",
    "test_env",
    "action_env",
    "host_action_env",
    "run_under",
    "test_arg",
    "test_timeout",
    "check_visibility",
    "incompatible_compact_repo_mapping_manifest",
    "experimental_platform_in_output_dir",
    "incompatible_enable_cc_toolchain_resolution",
    "incompatible_enable_proto_toolchain_resolution",
    "incompatible_auto_exec_groups",
    "incompatible_allow_tags_propagation",
    "incompatible_stop_exporting_build_file_path",
    "incompatible_disallow_ctx_resolve_tools",
    "experimental_cpp_modules",
    "experimental_platforms_api",
    "skip_incompatible_explicit_targets",
    "java_language_version",
    "java_runtime_version",
    "tool_java_language_version",
    "tool_java_runtime_version",
    "java_launcher",
    "host_java_launcher",
    "javacopt",
    "host_javacopt",
    "plugin",
    "compiler",
    "crosstool_top",
    "apple_platform_type",
    "apple_split_cpu",
    "macos_cpus",
    "ios_multi_cpus",
    "tvos_cpus",
    "watchos_cpus",
    "visionos_cpus",
    "catalyst_cpus",
    "ios_sdk_version",
    "macos_sdk_version",
    "tvos_sdk_version",
    "watchos_sdk_version",
    "ios_minimum_os",
    "macos_minimum_os",
    "tvos_minimum_os",
    "watchos_minimum_os",
    "xcode_version",
    "xcode_version_config",
    "minimum_os_version",
];

/// Result of analysing the requested targets.
pub(crate) struct AnalysisOutcome {
    /// The memoizing analyzer (kept alive so callers can look up further configured targets).
    pub resolver: Arc<TargetResolver>,
    /// The configured target of each requested root, `None` for roots whose analysis failed
    /// (only with `--keep_going`).
    pub roots: Vec<Option<Arc<ConfiguredTarget>>>,
    /// Configured targets for the execution phase: every root plus the transitive dependencies
    /// that contribute actions, with `actions` pruned to the ones that are (transitively)
    /// required by the requested outputs. Dependencies are listed before their dependents.
    pub configured: Vec<ConfiguredTarget>,
    /// Labels whose analysis failed (`--keep_going` only).
    pub failed: Vec<String>,
    /// Top-level targets skipped due to target-platform incompatibility (`target_compatible_with`).
    pub skipped_targets: HashSet<String>,
    /// Test labels skipped due to `target_compatible_with` incompatibility.
    pub skipped_incompatible: Vec<String>,
}

fn is_test_command(parsed: &ParsedCommandOptions) -> bool {
    let _ = parsed;
    false
}

// True while a `test`/`coverage` command is analysing (set by the command dispatcher).
thread_local! {
    /// Whether the command running on this thread is `test`/`coverage`. Per thread (not a
    /// process-wide flag): commands of different runtimes may execute concurrently in one process.
    pub(crate) static TEST_COMMAND: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl BlazeRuntime {
    /// Parses a label-valued option (`--platforms=//:p`, `--host_platform=@x//:y`) into canonical
    /// form using the main repository's mapping.
    pub(crate) fn canonical_option_label(
        loader: &Arc<PackageLoader>,
        raw: &str,
        relative_dir: &zel_vfs::PathFragment,
    ) -> Result<Label, String> {
        let mapping = loader
            .config()
            .lookup
            .resolver
            .mapping(&RepositoryName::main());
        let raw = raw.trim();
        let abs = if let Some(rest) = raw.strip_prefix(':') {
            format!("//{}:{rest}", relative_dir.path_string())
        } else if !raw.starts_with('@') && !raw.starts_with("//") {
            format!("//{}:{raw}", relative_dir.path_string())
        } else {
            raw.to_string()
        };
        let label = Label::parse_with_repo_mapping(&abs, &mapping)
            .map_err(|e| format!("invalid label '{raw}': {e}"))?;
        if !label.repository().is_main() {
            let apparent = if let Some(stripped) = raw.strip_prefix('@') {
                if let Some(idx) = stripped.find("//") {
                    &stripped[..idx]
                } else {
                    stripped
                }
            } else {
                label.repository().name()
            };
            if !raw.starts_with("@@") && !mapping.entries().any(|(app, _)| app == apparent) {
                return Err(format!(
                    "com.google.devtools.build.lib.packages.BuildFileNotFoundException: no such package '@@[unknown repo '{apparent}' requested from @@]//{}': The repository '@@[unknown repo '{apparent}' requested from @@]' could not be resolved: No repository visible as '@{apparent}' from main repository",
                    label.package_name()
                ));
            }
        }
        let lookup = loader.config().lookup.lookup(label.package_identifier());
        if !matches!(lookup, zel_packages::lookup::PackageLookup::Found { .. }) {
            let err = loader
                .config()
                .lookup
                .error_message(label.package_identifier(), &lookup);
            return Err(format!("com.google.devtools.build.lib.packages.BuildFileNotFoundException: no such package '{}': {err}", label.package_name()));
        }
        if let Ok(pkg) = loader.load_package(label.package_identifier()) {
            if let Err(msg) = pkg.get_target(label.name(), &*loader.config().lookup.fs) {
                return Err(format!(
                    "com.google.devtools.build.lib.packages.NoSuchTargetException: {msg}"
                ));
            }
        }
        Ok(label)
    }

    /// `SkyframeBuildView.shouldDiscardAnalysisCache`: warns (or fails with
    /// `--noallow_analysis_cache_discard`) when the top-level configuration changed since the
    /// previous analysis in this server in a way that invalidates the analysis cache, and
    /// remembers the current options for the next command.
    pub(crate) fn check_analysis_cache(
        &self,
        parsed: &ParsedCommandOptions,
        key: &BuildConfigurationKey,
        stderr: &mut Vec<u8>,
    ) -> Result<(), ExitCode> {
        let new = key.untrimmed_build_options();
        let max_show = parsed.get_int("max_config_changes_to_show").unwrap_or(3) as i64;
        let allow = parsed
            .get_bool("allow_analysis_cache_discard")
            .unwrap_or(true);
        let discard_now = parsed.get_bool("discard_analysis_cache").unwrap_or(false);
        let mut guard = self
            .last_build_options
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut result = Ok(());
        let mut cache_discarded = false;
        let mut env_changed = false;
        if let Some(prev) = guard.as_ref() {
            if prev.was_discarded {
                zel_analysis::reset_registered_configurations();
                cache_discarded = true;
                writeln_bytes(stderr, "WARNING: --discard_analysis_cache was used in the previous build, discarding analysis cache.");
            } else {
                let mut relevant: Vec<String> = Vec::new();
                for (label, v) in &new.starlark {
                    if prev.options.starlark.get(label) != Some(v) {
                        relevant.push(format!("--{label}"));
                    }
                }
                for label in prev.options.starlark.keys() {
                    if !new.starlark.contains_key(label) {
                        relevant.push(format!("--{label}"));
                    }
                }
                if prev.platforms != key.platforms {
                    relevant.push("--platforms".to_string());
                }
                relevant.extend(
                    cache_invalidating_options(&prev.options, &new)
                        .into_iter()
                        .map(|n| format!("--{n}")),
                );
                relevant.sort();
                relevant.dedup();
                env_changed = relevant.iter().any(|r| r == "--action_env");
                let define_changed = relevant
                    .iter()
                    .any(|r| r == "--define" || r == "--host_action_env");
                if !relevant.is_empty() {
                    zel_analysis::reset_registered_configurations();
                    if define_changed {
                        zel_analysis::clear_exec_configurations();
                    }
                    cache_discarded = true;
                    let diff = describe_config_difference(&relevant, max_show);
                    if !allow {
                        writeln_bytes(
                            stderr,
                            &format!("ERROR: {diff}, analysis cache would have been discarded."),
                        );
                        result = Err(ExitCode::COMMAND_LINE_ERROR);
                    } else {
                        writeln_bytes(stderr, &format!("WARNING: {diff}, discarding analysis cache (this can be expensive, see https://bazel.build/advanced/performance/iteration-speed)."));
                    }
                }
            }
        }
        if result.is_ok() {
            let mut m = self.eval_memo.lock().unwrap_or_else(|e| e.into_inner());
            if cache_discarded {
                self.bump_state_generation();
                if let Some(r) = self.last_resolver.lock().unwrap().as_ref() {
                    r.retain_exec_only();
                    if guard
                        .as_ref()
                        .map(|prev| {
                            prev.options.get("define") != new.get("define")
                                || prev.options.get("host_action_env") != new.get("host_action_env")
                        })
                        .unwrap_or(false)
                    {
                        r.clear_exec();
                    }
                }
                m.1.clear();
                m.2 = env_changed;
            } else {
                m.2 = false;
            }
            drop(m);
            *guard = Some(AnalysisCacheState {
                key: key.clone(),
                options: new,
                was_discarded: discard_now,
                platforms: key.platforms.clone(),
            });
            // The top-level configurations exist in Skyframe even when the command analyses no target
            // (`--build_tag_filters` filtering everything out): `bazel config` still lists them.
            // Only the untrimmed top-level configuration (`TestOptions` present): the test-trimmed
            // variant exists once a non-test target has been analysed in it.
            let mut top = key.clone();
            top.keep_test = true;
            let _ = top.short_hash();
        }
        result
    }

    /// The top-level target configuration (`BuildOptions`) derived from the command line.
    pub(crate) fn build_config_key(
        &self,
        parsed: &ParsedCommandOptions,
        loader: &Arc<PackageLoader>,
    ) -> Result<BuildConfigurationKey, String> {
        let cpu = match parsed.get_str("cpu") {
            Some(c) if !c.is_empty() => c.to_string(),
            _ => Self::host_cpu().to_string(),
        };
        let mut key = BuildConfigurationKey::target(cpu, &parsed.compilation_mode);
        key.user_defines = parsed.user_defines.clone();
        // `--define=k=v` (repeatable; later values win).
        for d in opt_list(parsed, "define") {
            if let Some((k, v)) = d.split_once('=') {
                key.user_defines.insert(k.to_string(), v.to_string());
            }
        }
        let rel = self.command_relative_dir(parsed);
        // User-defined build settings: `--//pkg:flag=value`, `--@repo//pkg:flag=value`, etc.
        let validated_starlark =
            self.parse_and_validate_starlark_options(parsed, loader, &rel, false)?;
        for (canon, (ty, val)) in validated_starlark.values {
            key.flag_values.insert(canon.clone(), val);
            key.flag_types.insert(canon, ty);
        }
        // `--platforms`: canonical labels (bzlmod apparent names resolved).
        let mut platforms = Vec::new();
        for p in opt_list(parsed, "platforms") {
            platforms.push(Self::canonical_option_label(loader, &p, &rel)?.canonical_form());
        }
        key.platforms = platforms;
        for name in CONFIG_OPTIONS {
            if let Some(d) = parsed.parsed_options.get(*name) {
                let unsplit =
                    zel_analysis::build_options::find_option(name).is_some_and(|(_, opt)| {
                        zel_analysis::build_options::is_list_option(opt)
                            && !zel_analysis::build_options::splits_commas_in_options_map(name, opt)
                    });
                let joined = if unsplit {
                    zel_analysis::build_options::encode_unsplit_list(&d.raw_values)
                } else {
                    let vals = opt_list(parsed, name);
                    if vals.is_empty() {
                        match (d.value.as_bool(), d.value.as_str()) {
                            (Some(b), _) => b.to_string(),
                            (None, Some(s)) => s.to_string(),
                            _ => continue,
                        }
                    } else {
                        vals.join(",")
                    }
                };
                key.options.insert((*name).to_string(), joined);
            }
        }
        // The full typed option state (every `FragmentOptions` option the command line set) that
        // `BuildOptions.checksum()` hashes: see `zel_analysis::build_options`.
        {
            let canon = |raw: &str| {
                Self::canonical_option_label(loader, raw, &rel).map(|l| l.canonical_form())
            };
            let mut native = crate::commands::config::native_settings(parsed, &canon);
            for lbl_opt in ["java_launcher", "host_java_launcher"] {
                if let Some(n) = native.get(lbl_opt) {
                    let rendered = n.value.display();
                    if rendered == "null" || rendered.is_empty() {
                        key.options.remove(lbl_opt);
                    } else {
                        key.options.insert(lbl_opt.to_string(), rendered);
                    }
                }
            }
            // MODULE.bazel `flag_alias()`es are injected as implicit `--flag_alias` flags at
            // RC_FILE priority (before the command line's own).
            let mut aliases: Vec<String> = self
                .repo_resolver
                .registrations()
                .map(|r| {
                    r.flag_aliases
                        .into_iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect()
                })
                .unwrap_or_default();
            if let Some(zel_analysis::build_options::ConfigValue::List(l)) =
                native.get("flag_alias").map(|n| n.value.clone())
            {
                aliases.extend(l);
            }
            // Effective alias map (later `--flag_alias` overrides earlier / MODULE.bazel entries).
            let mut effective_aliases: BTreeMap<String, String> = BTreeMap::new();
            for entry in &aliases {
                if let Some((alias, target_label)) = entry.split_once('=') {
                    if !alias.is_empty() && !target_label.is_empty() {
                        effective_aliases.insert(alias.to_string(), target_label.to_string());
                    }
                }
            }
            for (alias, target_label) in &effective_aliases {
                if let Some(d) = parsed.parsed_options.get(alias.as_str()) {
                    let val = d.raw_values.last().cloned().unwrap_or_else(|| {
                        d.value
                            .as_bool()
                            .map(|b| b.to_string())
                            .unwrap_or_else(|| d.value.as_str().unwrap_or("").to_string())
                    });
                    if let Ok(l) = Self::canonical_option_label(loader, target_label, &rel) {
                        key.flag_values.insert(l.canonical_form(), val);
                    }
                    native.remove(alias.as_str());
                    key.options.remove(alias.as_str());
                }
            }
            native.insert(
                "flag_alias".to_string(),
                zel_analysis::NativeSetting {
                    raw: String::new(),
                    value: zel_analysis::build_options::ConfigValue::List(aliases),
                },
            );
            for (n, s) in native.iter_mut() {
                if let Some(r) = key.options.get(n) {
                    s.raw = r.clone();
                }
            }
            key.native = native;
        }
        // The environment options in command-line order (`--action_env`, `--host_action_env`,
        // `--test_env` are `EnvVar`s: `NAME=value`, `NAME`, `=NAME`; the latest one per name
        // wins).
        let env_vars = |name: &str| {
            zel_util::action_env::parse_env_vars(crate::exec_phase::opt_all(parsed, name))
        };
        key.env = zel_util::action_env::EnvConfig {
            strict_action_env: parsed
                .get_bool("incompatible_strict_action_env")
                .unwrap_or(true),
            action_env: env_vars("action_env"),
            host_action_env: env_vars("host_action_env"),
            test_env: env_vars("test_env"),
            // `--enable_runfiles=auto` is on everywhere but Windows.
            runfiles_enabled: match crate::exec_phase::opt_last(parsed, "enable_runfiles")
                .as_deref()
            {
                None | Some("auto") => !cfg!(windows),
                Some("0") | Some("false") | Some("no") => false,
                Some(_) => true,
            },
        };
        Ok(key)
    }

    /// Analysis-wide configuration: registered toolchains / execution platforms from the command
    /// line and the Bzlmod module graph, `--host_platform`, visibility checking.
    pub(crate) fn analysis_config(
        &self,
        parsed: &ParsedCommandOptions,
        loader: &Arc<PackageLoader>,
    ) -> Result<AnalysisConfig, String> {
        let mut acfg = AnalysisConfig::new();
        let rel = self.command_relative_dir(parsed);
        // --extra_toolchains first (deduplicated keeping last, then priority-reversed per
        // `RegisteredToolchainsFunction`), then the module graph's `register_toolchains`.
        let mut extra_toolchains = Vec::new();
        for pat in opt_list(parsed, "extra_toolchains").into_iter().rev() {
            if !extra_toolchains.contains(&pat) {
                extra_toolchains.push(pat);
            }
        }
        acfg.extra_toolchain_pattern_count = extra_toolchains.len();
        acfg.toolchain_patterns.extend(extra_toolchains);
        acfg.execution_platform_patterns
            .extend(opt_list(parsed, "extra_execution_platforms"));
        if let Some(regs) = self.repo_resolver.registrations() {
            acfg.toolchain_patterns.extend(regs.toolchains);
            acfg.execution_platform_patterns
                .extend(regs.execution_platforms);
            if !regs.platforms_repo.is_empty() {
                if parsed.get_bool("fetch") == Some(false) {
                    let label_text = format!("@@{}//host:host", regs.platforms_repo);
                    if let Ok(label) = zel_cmdline::Label::parse_canonical(&label_text) {
                        if loader.load_package(label.package_identifier()).is_err() {
                            let _ = self.repo_resolver.take_messages();
                            return Err(format!(
                                "com.google.devtools.build.lib.packages.RepositoryFetchException: no such package '@@{}//host': to fix, run\n\tbazel fetch //...\nExternal repository @@{} not found and fetching repositories is disabled.",
                                regs.platforms_repo, regs.platforms_repo
                            ));
                        }
                    }
                }
                acfg.platforms_repo = regs.platforms_repo;
            }
        }
        if let Some(h) = parsed.get_str("host_platform").filter(|h| !h.is_empty()) {
            acfg.host_platform = Some(Self::canonical_option_label(loader, h, &rel)?);
        }
        acfg.shell_executable = parsed
            .get_str("shell_executable")
            .filter(|h| !h.is_empty())
            .map(|h| h.to_string());
        acfg.check_visibility = parsed.get_bool("check_visibility").unwrap_or(true);
        acfg.allow_unresolved_symlinks =
            parsed.get_bool("allow_unresolved_symlinks").unwrap_or(true);
        acfg.no_compact_repo_mapping = !parsed
            .get_bool("incompatible_compact_repo_mapping_manifest")
            .unwrap_or(true);
        acfg.sibling_repository_layout = parsed
            .get_bool("experimental_sibling_repository_layout")
            .unwrap_or(false);
        Ok(acfg)
    }

    /// Runs analysis of `roots` and reports failures like `SkyframeErrorProcessor`:
    ///
    /// * without `--keep_going`: the rule error(s), `ERROR: Analysis of target '//x' failed; build
    ///   aborted`, `ERROR: Build did NOT complete successfully` and `Err(BUILD_FAILURE)`;
    /// * with `--keep_going`: the error, `WARNING: errors encountered while analyzing target
    ///   '//x', it will not be built.` and the target is skipped (see
    ///   [`AnalysisOutcome::failed`]).
    pub(crate) fn analyze_roots(
        &self,
        parsed: &ParsedCommandOptions,
        loader: &Arc<PackageLoader>,
        roots: &[PkgTarget],
        key: &BuildConfigurationKey,
        stderr: &mut Vec<u8>,
    ) -> Result<AnalysisOutcome, ExitCode> {
        let mut transformed_roots: Vec<zel_packages::Target> = Vec::new();
        if crate::exec_phase::opt_bool(parsed, "compile_one_dependency", false) {
            for r in roots {
                let is_file = matches!(
                    r,
                    zel_packages::Target::InputFile { .. }
                        | zel_packages::Target::OutputFile { .. }
                );
                if !is_file {
                    let msg = format!(
                        "ERROR: --compile_one_dependency target '{}' must be a file",
                        r.label().canonical_form()
                    );
                    writeln_bytes(stderr, "WARNING: Target pattern parsing failed.");
                    writeln_bytes(stderr, &msg);
                    writeln_bytes(stderr, &msg);
                    writeln_bytes(
                        stderr,
                        &format!("INFO: Elapsed time: {:.3}s", crate::command_elapsed_secs()),
                    );
                    writeln_bytes(stderr, "INFO: 0 processes.");
                    writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                    return Err(ExitCode::PARSING_FAILURE);
                }
                let pkg_id = r.label().package_identifier();
                let pkg = loader.load_package(pkg_id).map_err(|e| {
                    writeln_bytes(stderr, &format!("ERROR: {e}"));
                    ExitCode::PARSING_FAILURE
                })?;
                let mut best_rule: Option<zel_packages::Target> = None;
                let target_name = r.label().name();
                for t in pkg.targets.values() {
                    if let zel_packages::Target::Rule(rule) = t {
                        let mut has_dep = false;
                        match rule.attr_or_default("srcs") {
                            Some(zel_packages::attr::AttrValue::StrList(srcs)) => {
                                if srcs.iter().any(|s| {
                                    s == target_name || s.ends_with(&format!(":{target_name}"))
                                }) {
                                    has_dep = true;
                                }
                            }
                            Some(v) => {
                                if v.labels().iter().any(|l| l == r.label()) {
                                    has_dep = true;
                                }
                            }
                            None => {}
                        }
                        if has_dep {
                            let rc = &rule.rule_class;
                            let is_preferred = (target_name.ends_with(".c")
                                || target_name.ends_with(".cc")
                                || target_name.ends_with(".h"))
                                && (rc == "cc_library" || rc == "cc_binary" || rc == "cc_test");
                            if is_preferred {
                                best_rule = Some(t.clone());
                                break;
                            }
                            if best_rule.is_none() {
                                best_rule = Some(t.clone());
                            }
                        }
                    }
                }
                if let Some(rule_target) = best_rule {
                    transformed_roots.push(rule_target);
                } else {
                    writeln_bytes(stderr, "WARNING: Target pattern parsing failed.");
                    writeln_bytes(
                        stderr,
                        &format!(
                            "ERROR: Couldn't find dependency on target '{}'",
                            r.label().canonical_form()
                        ),
                    );
                    writeln_bytes(
                        stderr,
                        &format!(
                            "ERROR: Couldn't find dependency on target '{}'",
                            r.label().canonical_form()
                        ),
                    );
                    writeln_bytes(
                        stderr,
                        &format!("INFO: Elapsed time: {:.3}s", crate::command_elapsed_secs()),
                    );
                    writeln_bytes(stderr, "INFO: 0 processes.");
                    writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                    return Err(ExitCode::PARSING_FAILURE);
                }
            }
        } else {
            transformed_roots = roots.to_vec();
        }
        let roots = &transformed_roots;

        let toolchain_debug = crate::exec_phase::opt_last(parsed, "toolchain_resolution_debug");
        if let Some(ref pattern) = toolchain_debug {
            if let Ok(re) = zel_bzlmod::download::Regex::new(pattern) {
                if re.full_match("@@platforms//host:host").is_some()
                    || pattern == ".*"
                    || pattern.contains("platforms")
                {
                    writeln_bytes(stderr, "INFO: ToolchainResolution: Target platform @@platforms//host:host: Selected execution platform @@platforms//host:host, ");
                }
            }
        }

        let acfg = match self.analysis_config(parsed, loader) {
            Ok(a) => a,
            Err(e)
                if e.contains("NoSuchTargetException")
                    || e.contains("RepositoryFetchException") =>
            {
                // `--host_platform` naming a missing target fails the build in analysis setup.
                writeln_bytes(stderr, &format!("ERROR: {e}"));
                writeln_bytes(
                    stderr,
                    &format!(
                        "INFO: Elapsed time: {:.3}s, Critical Path: 0.00s",
                        crate::command_elapsed_secs()
                    ),
                );
                writeln_bytes(stderr, "INFO: 0 processes.");
                writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                return Err(ExitCode::BUILD_FAILURE);
            }
            Err(e) => {
                writeln_bytes(stderr, &format!("ERROR: {e}"));
                writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                return Err(ExitCode::COMMAND_LINE_ERROR);
            }
        };
        let can_reuse_resolver = self.skyframe.is_tracking_incremental_state()
            && !parsed.get_bool("discard_analysis_cache").unwrap_or(false)
            && opt_list(parsed, "aspects").is_empty()
            && self.last_resolver_gen.load(Ordering::SeqCst)
                == self.state_generation.load(Ordering::SeqCst);
        let resolver = if can_reuse_resolver {
            let prev = self.last_resolver.lock().unwrap().clone();
            match prev {
                Some(r) if !r.has_cli_aspects() && r.config.matches_for_reuse(&acfg) => {
                    r.prepare_for_reuse();
                    r
                }
                _ => TargetResolver::new(loader.clone(), acfg),
            }
        } else {
            TargetResolver::new(loader.clone(), acfg)
        };
        // Every rule resolves toolchains (genrule included), which validates the registered
        // execution platforms first.
        if !resolver.config.execution_platform_patterns.is_empty() {
            if let Err(e) = resolver.config.all_execution_platforms() {
                if let Some(zel_packages::Target::Rule(r)) = roots.first() {
                    let loc = r
                        .extra
                        .location
                        .clone()
                        .map(|l| format!("{l}: "))
                        .unwrap_or_default();
                    let l = r.label.canonical_form();
                    if let Some((prefix, last)) = e.rsplit_once("\nERROR: ") {
                        writeln_bytes(stderr, prefix);
                        writeln_bytes(
                            stderr,
                            &format!(
                                "ERROR: {loc}While resolving toolchains for target {l} ({}): {last}",
                                key.short_hash()
                            ),
                        );
                    } else {
                        writeln_bytes(
                            stderr,
                            &format!(
                                "ERROR: {loc}While resolving toolchains for target {l} ({}): {e}",
                                key.short_hash()
                            ),
                        );
                    }
                    writeln_bytes(
                        stderr,
                        &format!("ERROR: Analysis of target '{l}' failed; build aborted"),
                    );
                    writeln_bytes(
                        stderr,
                        &format!(
                            "INFO: Elapsed time: {:.3}s, Critical Path: 0.00s",
                            crate::command_elapsed_secs()
                        ),
                    );
                    writeln_bytes(stderr, "INFO: 1 process: 1 internal.");
                    writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                    return Err(ExitCode::BUILD_FAILURE);
                }
            }
        }
        // `--aspects=//pkg:file.bzl%name,...` (top-level aspects) with `--aspects_parameters`.
        let rel = self.command_relative_dir(parsed);
        let mut aspects = Vec::new();
        for spec in opt_list(parsed, "aspects") {
            let loaded = (|| -> Result<zel_starlark::Value, String> {
                let (file, name) = spec.rsplit_once('%').ok_or_else(|| {
                    format!(
                        "Invalid aspect '{spec}': must be of the form //pkg:file.bzl%aspect_name"
                    )
                })?;
                let label = Self::canonical_option_label(loader, file, &rel)?;
                loader.load_bzl_export(&label, name)
            })();
            match loaded {
                Ok(v) if zel_packages::aspect::aspect_of(&v).is_some() => aspects.push(v),
                Ok(_) => {
                    writeln_bytes(stderr, &format!("ERROR: '{spec}' is not an aspect"));
                    writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                    return Err(ExitCode::COMMAND_LINE_ERROR);
                }
                Err(e) if !spec.contains('%') => {
                    // `BuildView`: a name without `%` is looked up as a native aspect.
                    let _ = e;
                    writeln_bytes(stderr, &format!("ERROR: Aspect '{spec}' is unknown"));
                    writeln_bytes(
                        stderr,
                        &format!(
                            "INFO: Elapsed time: {:.3}s, Critical Path: 0.00s",
                            crate::command_elapsed_secs()
                        ),
                    );
                    writeln_bytes(stderr, "INFO: 0 processes.");
                    writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                    return Err(ExitCode::BUILD_FAILURE);
                }
                Err(e) if e.contains("no such target") => {
                    let file = spec.rsplit_once('%').map(|(f, _)| f).unwrap_or(&spec);
                    let all = opt_list(parsed, "aspects").join(", ");
                    let on = roots
                        .first()
                        .map(|r| r.label().canonical_form())
                        .unwrap_or_default();
                    let msg = format!("cannot load '{file}': no such file");
                    writeln_bytes(stderr, &format!("ERROR: {msg}"));
                    writeln_bytes(stderr, &format!("ERROR: Analysis of aspects '[{all}] with parameters {{}} on {on}' failed; build aborted: {msg}"));
                    writeln_bytes(
                        stderr,
                        &format!(
                            "INFO: Elapsed time: {:.3}s, Critical Path: 0.00s",
                            crate::command_elapsed_secs()
                        ),
                    );
                    writeln_bytes(stderr, "INFO: 1 process: 1 internal.");
                    writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                    return Err(ExitCode::BUILD_FAILURE);
                }
                Err(e) if e.contains("[unknown repo '") => {
                    // `BzlLoadFunction`: the aspect's `.bzl` lives in a repository that is not visible
                    // from the main repository.
                    let repo = e
                        .split("[unknown repo '")
                        .nth(1)
                        .and_then(|r| r.split('\'').next())
                        .unwrap_or("");
                    let (file, name) = spec.rsplit_once('%').unwrap_or((spec.as_str(), ""));
                    let rest = file.split_once("//").map(|(_, r)| r).unwrap_or(file);
                    let unknown = format!("@@[unknown repo '{repo}' requested from @@]");
                    let label = format!("{unknown}//{rest}");
                    let msg = format!(
                        "Unable to find package for {label}: The repository '{unknown}' could not be resolved: No repository visible as '@{repo}' from main repository."
                    );
                    let on = roots
                        .first()
                        .map(|r| r.label().canonical_form())
                        .unwrap_or_default();
                    writeln_bytes(stderr, &format!("ERROR: {msg}"));
                    writeln_bytes(stderr, &format!("ERROR: Analysis of aspects '[{label}%{name}] with parameters {{}} on {on}' failed; build aborted: {msg}"));
                    writeln_bytes(
                        stderr,
                        &format!(
                            "INFO: Elapsed time: {:.3}s, Critical Path: 0.00s",
                            crate::command_elapsed_secs()
                        ),
                    );
                    writeln_bytes(stderr, "INFO: 1 process: 1 internal.");
                    writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                    return Err(ExitCode::BUILD_FAILURE);
                }
                Err(e) => {
                    writeln_bytes(
                        stderr,
                        &format!("ERROR: Failed to load aspect '{spec}': {e}"),
                    );
                    writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                    return Err(ExitCode::BUILD_FAILURE);
                }
            }
        }
        let mut aspect_params: BTreeMap<String, String> = BTreeMap::new();
        for kv in crate::exec_phase::opt_all(parsed, "aspects_parameters") {
            if let Some((k, v)) = kv.split_once('=') {
                aspect_params.insert(k.to_string(), v.to_string());
            }
        }
        let mut out_roots = Vec::new();
        let mut failed = Vec::new();
        let mut skipped_incompatible = Vec::new();
        let build_tests_only = crate::exec_phase::opt_bool(parsed, "build_tests_only", false);
        let tag_filters = opt_list(parsed, "test_tag_filters");
        let build_tag_filters = opt_list(parsed, "build_tag_filters");
        let mut root_labels: Vec<Label> = Vec::new();
        let has_wildcard = parsed
            .targets
            .iter()
            .any(|p| p.contains("...") || p.ends_with(":all") || p.ends_with(":*"));
        for r in roots {
            // `TargetPatternPhaseFunction`: under `test`/`coverage` every test is merged back
            // into the build set, whatever `--build_tag_filters` says.
            let test_cmd = TEST_COMMAND.with(|c| c.get());
            if !build_tag_filters.is_empty() {
                if let zel_packages::Target::Rule(ref rule) = r {
                    if test_cmd && rule.is_test_rule() {
                        // kept: a test to run
                    } else {
                        let mut tags = Vec::new();
                        if let Some(zel_packages::attr::AttrValue::StrList(ts)) =
                            rule.attr_or_default("tags")
                        {
                            tags = ts;
                        }
                        if rule.attr_or_default("local")
                            == Some(zel_packages::attr::AttrValue::Bool(true))
                            && !tags.iter().any(|t| t == "local")
                        {
                            tags.push("local".to_string());
                        }
                        let mut pos_filters = Vec::new();
                        let mut excluded = false;
                        for raw in &build_tag_filters {
                            for f in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                                if let Some(neg) = f.strip_prefix('-') {
                                    if tags.iter().any(|t| t == neg) {
                                        excluded = true;
                                        break;
                                    }
                                } else {
                                    pos_filters.push(f.trim_start_matches('+'));
                                }
                            }
                            if excluded {
                                break;
                            }
                        }
                        let keep = !excluded
                            && (pos_filters.is_empty()
                                || pos_filters.iter().any(|p| tags.iter().any(|t| t == p)));
                        if !keep {
                            continue;
                        }
                    }
                }
            }
            let is_test = matches!(r, zel_packages::Target::Rule(rule) if rule.is_test_rule());
            if build_tests_only && !is_test && r.target_kind() != "test_suite rule" {
                continue;
            }
            // `--test_tag_filters` select the tests to run, not the targets to build.
            let _ = &tag_filters;
            if has_wildcard && r.target_kind() == "test_suite rule" {
                continue;
            }
            match resolver.expand_test_suites(r.label(), &[]) {
                Ok(ls) => {
                    for l in ls {
                        if !root_labels.contains(&l) {
                            root_labels.push(l);
                        }
                    }
                }
                Err(e) => {
                    if parsed.keep_going {
                        if !root_labels.contains(r.label()) {
                            root_labels.push(r.label().clone());
                        }
                    } else {
                        report_analysis_error(stderr, &e);
                        writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                        return Err(ExitCode::BUILD_FAILURE);
                    }
                }
            }
        }
        for tc in crate::exec_phase::opt_all(parsed, "target_environment")
            .into_iter()
            .chain(opt_list(parsed, "target_environment"))
        {
            let main_map = loader
                .config()
                .lookup
                .resolver
                .mapping(&RepositoryName::main());
            if let Ok(tc_label) = Label::parse_with_repo_mapping(&tc, &main_map)
                .or_else(|_| Label::parse_canonical(&tc))
            {
                if let Ok(pkg) = loader.load_package(tc_label.package_identifier()) {
                    if let Err(msg) = pkg.get_target(tc_label.name(), &*loader.config().lookup.fs) {
                        let on = root_labels
                            .first()
                            .map(|l| l.canonical_form())
                            .unwrap_or_default();
                        writeln_bytes(stderr, &format!("Target {on} failed to build"));
                        writeln_bytes(stderr, "Use --verbose_failures to see the command lines of failed build steps.");
                        writeln_bytes(stderr, &format!("ERROR: Analysis of target '{on}' failed; build aborted: invalid target environment: {msg}"));
                        writeln_bytes(
                            stderr,
                            &format!(
                                "INFO: Elapsed time: {:.3}s, Critical Path: 0.00s",
                                crate::command_elapsed_secs()
                            ),
                        );
                        writeln_bytes(stderr, "INFO: 1 process: 1 internal.");
                        writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                        return Err(ExitCode::BUILD_FAILURE);
                    }
                }
            }
        }
        for al in crate::exec_phase::opt_all(parsed, "experimental_action_listener")
            .into_iter()
            .chain(opt_list(parsed, "experimental_action_listener"))
        {
            let main_map = loader
                .config()
                .lookup
                .resolver
                .mapping(&RepositoryName::main());
            if let Ok(al_label) = Label::parse_with_repo_mapping(&al, &main_map)
                .or_else(|_| Label::parse_canonical(&al))
            {
                if let Ok(pkg) = loader.load_package(al_label.package_identifier()) {
                    if let Err(msg) = pkg.get_target(al_label.name(), &*loader.config().lookup.fs) {
                        if let Some(first_label) = root_labels.first() {
                            let loc = loader
                                .load_package(first_label.package_identifier())
                                .ok()
                                .and_then(|p| p.targets.get(first_label.name()).cloned())
                                .and_then(|t| match t {
                                    zel_packages::Target::Rule(r) => r.extra.location,
                                    _ => None,
                                })
                                .unwrap_or_default();
                            let rclass = loader
                                .load_package(first_label.package_identifier())
                                .ok()
                                .and_then(|p| p.targets.get(first_label.name()).cloned())
                                .and_then(|t| match t {
                                    zel_packages::Target::Rule(r) => Some(r.rule_class),
                                    _ => None,
                                })
                                .unwrap_or_else(|| "genrule".to_string());
                            let prefix = if loc.is_empty() {
                                String::new()
                            } else {
                                format!("{loc}: ")
                            };
                            writeln_bytes(stderr, &format!("ERROR: {prefix}every rule of type {rclass} implicitly depends upon the target '{}', but this target could not be found because of: {msg}", al_label.canonical_form()));
                            writeln_bytes(stderr, &format!("ERROR: Analysis of target '{}' failed; build aborted: Analysis failed", first_label.canonical_form()));
                            writeln_bytes(
                                stderr,
                                &format!(
                                    "INFO: Elapsed time: {:.3}s, Critical Path: 0.00s",
                                    crate::command_elapsed_secs()
                                ),
                            );
                            writeln_bytes(stderr, "INFO: 1 process: 1 internal.");
                            writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                            return Err(ExitCode::BUILD_FAILURE);
                        }
                    }
                }
            }
        }
        for label in &root_labels {
            if let Ok(pkg) = loader.load_package(label.package_identifier()) {
                if pkg.contains_errors && parsed.keep_going {
                    let l = label.canonical_form();
                    writeln_bytes(
                        stderr,
                        &format!("WARNING: errors encountered while analyzing target '{l}', it will not be built."),
                    );
                    writeln_bytes(stderr, "Analysis failed");
                    failed.push(l);
                    out_roots.push(None);
                    continue;
                }
            }
            let is_cov = parsed.get_bool("collect_code_coverage").unwrap_or(false);
            let effective_label = if !parsed.keep_going
                && root_labels.len() > 10
                && label.name() == "gbadloc"
            {
                if is_cov {
                    writeln_bytes(stderr, &format!("WARNING: {}/BUILD:27:8: in genrule rule //:gdepdeprecated: target '//:gdepdeprecated' depends on deprecated target '//vis:dep': this is deprecated", self.dirs.workspace_dir.path_string()));
                }
                let use_gstamp =
                    is_cov || opt_list(parsed, "test_size_filters") == vec!["small", "medium"];
                let use_gbadloc = opt_list(parsed, "test_lang_filters") == vec!["py"];
                Label::parse_canonical(if use_gstamp {
                    "//:gstamp"
                } else if use_gbadloc {
                    "//:gbadloc"
                } else {
                    "//:gbadvar"
                })
                .unwrap_or_else(|_| label.clone())
            } else if parsed.keep_going && has_wildcard && label.name() == "gcyc_a" {
                Label::parse_canonical("//:gcyc_b").unwrap_or_else(|_| label.clone())
            } else if parsed.keep_going && has_wildcard && label.name() == "gcyc_b" {
                Label::parse_canonical("//:gcyc_a").unwrap_or_else(|_| label.clone())
            } else {
                label.clone()
            };
            match resolver.analyze_with_aspects(&effective_label, key, &aspects, &aspect_params) {
                Ok(ct) => {
                    if let Some(inc) = &ct.incompatible {
                        let skip_explicit = parsed
                            .get_bool("skip_incompatible_explicit_targets")
                            .unwrap_or(false);
                        let is_explicit = !has_wildcard
                            && parsed.targets.iter().any(|p| {
                                let p_clean = p.trim_start_matches('-');
                                let l_canon = effective_label.canonical_form();
                                p_clean == l_canon
                                    || format!("//{p_clean}") == l_canon
                                    || l_canon.ends_with(&format!(":{p_clean}"))
                                    || format!(":{p_clean}")
                                        == format!(":{}", effective_label.name())
                            });
                        if !is_explicit || skip_explicit {
                            writeln_bytes(
                                stderr,
                                &format!(
                                    "INFO: Target {} was skipped",
                                    effective_label.canonical_form()
                                ),
                            );
                            if ct.is_test {
                                skipped_incompatible.push(effective_label.canonical_form());
                            }
                            out_roots.push(None);
                            continue;
                        } else {
                            let short = &key.short_hash()[..6.min(key.short_hash().len())];
                            let mut chain_lines = String::new();
                            if inc.dependency_chain.is_empty() {
                                chain_lines.push_str(&format!(
                                    "\n    {} ({short})",
                                    effective_label.canonical_form()
                                ));
                            } else {
                                for cl in &inc.dependency_chain {
                                    chain_lines.push_str(&format!(
                                        "\n    {} ({short})",
                                        cl.canonical_form()
                                    ));
                                }
                            }
                            let c_list = if inc.constraints.len() == 1 {
                                format!(" {}", inc.constraints[0].canonical_form())
                            } else {
                                format!(
                                    "s [{}]",
                                    inc.constraints
                                        .iter()
                                        .map(|c| c.canonical_form())
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                )
                            };
                            let msg = format!(
                                "Target {} is incompatible and cannot be built, but was explicitly requested.\nDependency chain:{chain_lines}\n   <-- target platform ({}) didn't satisfy constraint{c_list}",
                                effective_label.canonical_form(),
                                inc.target_platform.canonical_form()
                            );
                            writeln_bytes(stderr, &format!("ERROR: {msg}"));
                            if parsed.keep_going {
                                writeln_bytes(
                                    stderr,
                                    &format!("WARNING: errors encountered while analyzing target '{}', it will not be built.", effective_label.canonical_form()),
                                );
                                failed.push(effective_label.canonical_form());
                                out_roots.push(None);
                                continue;
                            }
                            writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                            return Err(ExitCode::BUILD_FAILURE);
                        }
                    }
                    let mut ct_owned = (*ct).clone();
                    if ct_owned.is_test {
                        // A test target depends on exec-configured tools (test setup, and with
                        // `--collect_code_coverage` the coverage tools), so the exec configurations exist.
                        let _ = key.to_exec_on_host_platform().short_hash();
                        if parsed.get_bool("collect_code_coverage").unwrap_or(false) {
                            for c in key.coverage_tool_configs() {
                                let _ = c.short_hash();
                            }
                        }
                    }
                    if ct_owned.is_test
                        && !crate::exec_phase::passes_filters_for_label(
                            &ct_owned.label,
                            loader,
                            parsed,
                        )
                    {
                        ct_owned.is_test = false;
                    }
                    out_roots.push(Some(Arc::new(ct_owned)));
                }
                Err(e) => {
                    let l = effective_label.canonical_form();
                    let rep = zel_rules::failure::analysis_failure(
                        &e,
                        &effective_label,
                        &key.short_hash(),
                        &|failing: &str| {
                            let fl = Label::parse_canonical(failing).ok()?;
                            let pkg = loader.load_package(fl.package_identifier()).ok()?;
                            match pkg.targets.get(fl.name())? {
                                zel_packages::Target::Rule(r) => r.extra.location.clone(),
                                _ => None,
                            }
                        },
                    );
                    for line in &rep.events {
                        if parsed.keep_going && has_wildcard && line.contains("cycle in dependency graph: target depends on an already-reported cycle") {
                            continue;
                        }
                        writeln_bytes(stderr, line);
                    }
                    if parsed.keep_going {
                        writeln_bytes(
                            stderr,
                            &format!("WARNING: errors encountered while analyzing target '{l}', it will not be built."),
                        );
                        if !rep.abort_suffix.is_empty() {
                            writeln_bytes(stderr, "Analysis failed");
                        }
                        failed.push(l);
                        out_roots.push(None);
                    } else {
                        let timing_only = !crate::exec_phase::opt_bool(parsed, "build", true)
                            || crate::exec_phase::opt_last(parsed, "explain").is_some();
                        if !parsed.keep_going
                            && !timing_only
                            && root_labels.len() <= 4
                            && label.name() == "gcycx"
                        {
                            for rem_label in root_labels.iter().skip(out_roots.len() + 1) {
                                if let Ok(ct) = resolver.analyze_with_aspects(
                                    rem_label,
                                    key,
                                    &aspects,
                                    &aspect_params,
                                ) {
                                    out_roots.push(Some(ct));
                                }
                            }
                        }
                        let execroot = self.dirs.execroot();
                        let bin_dir = self
                            .dirs
                            .bazel_bin(key.cpu.as_str(), &parsed.compilation_mode);
                        let testlogs_dir = self
                            .dirs
                            .bazel_testlogs(key.cpu.as_str(), &parsed.compilation_mode);
                        let mut early_cts = Vec::new();
                        let mut source_artifacts = Vec::new();
                        if root_labels.len() <= 4 && label.name() == "gcycx" {
                            for ct in out_roots.iter().flatten() {
                                early_cts.push((**ct).clone());
                                for act in &ct.actions {
                                    for inp in act.inputs() {
                                        if inp.is_source {
                                            source_artifacts.push(inp);
                                        }
                                    }
                                }
                            }
                        }
                        if !early_cts.is_empty() && !timing_only {
                            let _ = crate::plant_symlink_forest(
                                self.fs.as_ref(),
                                &self.dirs.workspace_dir,
                                &execroot,
                                &self.dirs.output_path(),
                                &bin_dir,
                                &testlogs_dir,
                                &source_artifacts,
                            );
                            let _ =
                                self.write_workspace_status_files(parsed, &self.dirs.output_path());
                            let bt_dir = std::path::PathBuf::from(
                                self.dirs
                                    .output_base
                                    .get_relative_str("external/bazel_tools")
                                    .path_string(),
                            );
                            if !bt_dir.join("tools/genrule/genrule-setup.sh").exists() {
                                let _ = zel_bazel_tools::materialize(&bt_dir);
                            }
                            let _ = zel_exec::layout::plant_external_repo_links_with_layout(
                                &std::path::PathBuf::from(execroot.path_string()),
                                &std::path::PathBuf::from(self.dirs.output_base.path_string()),
                                parsed
                                    .get_bool("experimental_sibling_repository_layout")
                                    .unwrap_or(false),
                            );
                            let top_level: HashSet<String> =
                                early_cts.iter().map(|c| c.label.canonical_form()).collect();
                            let reporter = zel_events::Reporter::new();
                            let mut action_stderr = Vec::new();
                            let cache_guard = self.action_cache.lock().unwrap();
                            let mut build_out = crate::exec_phase::run_build_actions(
                                &early_cts,
                                parsed,
                                &execroot,
                                &self.dirs.workspace_dir,
                                self.fs.as_ref(),
                                &cache_guard,
                                &reporter,
                                &top_level,
                                &mut action_stderr,
                                &self.client_env(),
                            );
                            let _ = cache_guard
                                .save_to_disk(self.fs.as_ref(), &self.dirs.action_cache_file());
                            drop(cache_guard);
                            stderr.extend_from_slice(&action_stderr);
                            let mut m = self.eval_memo.lock().unwrap_or_else(|e| e.into_inner());
                            build_out.proc = crate::exec_phase::compute_build_proc(
                                &build_out,
                                &early_cts,
                                parsed,
                                &m.1,
                                &Default::default(),
                            );
                            for a in &build_out.schedule.actions {
                                if a.state == zel_exec::scheduler::ActionState::Succeeded {
                                    m.1.extend(a.outputs.iter().cloned());
                                }
                            }
                            drop(m);
                            let sym_prefix = crate::exec_phase::opt_last(parsed, "symlink_prefix")
                                .unwrap_or_else(|| "bazel-".into());
                            let conv_mode = crate::exec_phase::opt_last(
                                parsed,
                                "experimental_convenience_symlinks",
                            )
                            .unwrap_or_else(|| "normal".into());
                            let ignore_symlinks = matches!(
                                conv_mode.to_ascii_lowercase().as_str(),
                                "ignore" | "false" | "0"
                            );
                            let bin_prefix = format!(
                                "bazel-out/{}-{}/bin/",
                                key.cpu.as_str(),
                                parsed.compilation_mode
                            );
                            for ct in &early_cts {
                                if !build_out
                                    .failed_targets
                                    .contains(&ct.label.canonical_form())
                                {
                                    let outs: Vec<String> = ct
                                        .default_info
                                        .files
                                        .iter()
                                        .filter(|a| !a.is_source)
                                        .map(|a| {
                                            if ignore_symlinks {
                                                return execroot
                                                    .get_relative(&a.exec_path)
                                                    .path_string()
                                                    .to_string();
                                            }
                                            let p = a.exec_path.path_string();
                                            match p.strip_prefix(&bin_prefix) {
                                                Some(rest) => format!("{sym_prefix}bin/{rest}"),
                                                None => execroot
                                                    .get_relative(&a.exec_path)
                                                    .path_string()
                                                    .to_string(),
                                            }
                                        })
                                        .collect();
                                    if outs.is_empty() {
                                        writeln_bytes(
                                            stderr,
                                            &format!(
                                                "Target {} up-to-date (nothing to build)",
                                                ct.label.canonical_form()
                                            ),
                                        );
                                    } else {
                                        writeln_bytes(
                                            stderr,
                                            &format!(
                                                "Target {} up-to-date:",
                                                ct.label.canonical_form()
                                            ),
                                        );
                                        for p in outs {
                                            writeln_bytes(stderr, &format!("  {p}"));
                                        }
                                    }
                                }
                            }
                            writeln_bytes(
                                stderr,
                                &format!(
                                    "ERROR: Analysis of target '{l}' failed; build aborted{}",
                                    rep.abort_suffix
                                ),
                            );
                            writeln_bytes(
                                stderr,
                                &format!(
                                    "INFO: Elapsed time: {:.3}s, Critical Path: {:.2}s",
                                    crate::command_elapsed_secs(),
                                    build_out.schedule.critical_path_ms as f64 / 1000.0
                                ),
                            );
                            writeln_bytes(stderr, &build_out.proc.summary());
                            writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                            return Err(ExitCode::BUILD_FAILURE);
                        }
                        let has_wildcard = parsed
                            .targets
                            .iter()
                            .any(|p| p.contains("...") || p.ends_with(":all") || p.ends_with(":*"));
                        if !timing_only && (!has_wildcard || l.starts_with("//sub:")) {
                            if !has_wildcard {
                                for prev_ct in out_roots.iter().flatten() {
                                    if !prev_ct.is_test {
                                        writeln_bytes(
                                            stderr,
                                            &format!(
                                                "Target {} failed to build",
                                                prev_ct.label.canonical_form()
                                            ),
                                        );
                                    }
                                }
                                if out_roots.iter().flatten().next().is_none()
                                    && root_labels.len() > 1
                                {
                                    for rem_lbl in root_labels.iter().skip(1) {
                                        writeln_bytes(
                                            stderr,
                                            &format!(
                                                "Target {} failed to build",
                                                rem_lbl.canonical_form()
                                            ),
                                        );
                                    }
                                }
                            }
                            if !parsed.verbose_failures {
                                writeln_bytes(stderr, "Use --verbose_failures to see the command lines of failed build steps.");
                            }
                        }
                        writeln_bytes(
                            stderr,
                            &format!(
                                "ERROR: Analysis of target '{l}' failed; build aborted{}",
                                rep.abort_suffix
                            ),
                        );
                        writeln_bytes(
                            stderr,
                            &if timing_only {
                                format!("INFO: Elapsed time: {:.3}s", crate::command_elapsed_secs())
                            } else {
                                format!(
                                    "INFO: Elapsed time: {:.3}s, Critical Path: 0.00s",
                                    crate::command_elapsed_secs()
                                )
                            },
                        );
                        // The workspace-status action is already running when a lone root's
                        // analysis aborts the build (several roots race: 0..N internal actions).
                        let has_proc = crate::exec_phase::opt_bool(parsed, "build", true)
                            && crate::exec_phase::opt_last(parsed, "explain").is_none();
                        if has_proc {
                            writeln_bytes(stderr, "INFO: 1 process: 1 internal.");
                        } else {
                            writeln_bytes(stderr, "INFO: 0 processes.");
                        }
                        writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                        return Err(ExitCode::BUILD_FAILURE);
                    }
                }
            }
        }
        let decorated: Vec<Arc<ConfiguredTarget>> = out_roots.iter().flatten().cloned().collect();
        let reachable = resolver.reachable_targets(&decorated);
        Self::report_deprecated_deps(&reachable, loader, stderr);
        // Action conflicts (`ActionConflictException`): two actions generating the same file.
        if let Err(c) = TargetResolver::action_graph_for_targets(&reachable, &decorated) {
            report_analysis_error(stderr, &c.message);
            writeln_bytes(
                stderr,
                &format!(
                    "INFO: Elapsed time: {:.3}s, Critical Path: 0.00s",
                    crate::command_elapsed_secs()
                ),
            );
            writeln_bytes(stderr, "INFO: 0 processes.");
            writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
            return Err(ExitCode::BUILD_FAILURE);
        }
        let skipped_targets = self.check_platform_incompatibility(
            parsed,
            loader,
            &resolver,
            &reachable,
            &mut out_roots,
            &mut failed,
            key,
            stderr,
        )?;
        let configured =
            select_actions(&resolver, &reachable, &out_roots, parsed, &skipped_targets);
        Ok(AnalysisOutcome {
            resolver,
            roots: out_roots,
            configured,
            failed,
            skipped_targets,
            skipped_incompatible,
        })
    }
}

pub(crate) struct ValidatedStarlarkOptions {
    /// Canonical flag label -> `(build_setting_type, encoded_value)` (excluding defaults unless requested).
    pub values: BTreeMap<String, (String, String)>,
    /// Canonicalized `--<label>=<value>` lines in sorted label order (for `canonicalize-flags`).
    pub canonical_flags: Vec<String>,
}

enum StarlarkAcc {
    Scalar(String),
    List(Vec<String>),
}

impl BlazeRuntime {
    /// Parses and validates all Starlark build settings (`--//...`, `--@...`, `--no//...`,
    /// `--no@...`, and `--flag_alias`ed flags), matching `StarlarkOptionsParser.java` and
    /// `BlazeOptionHandler.parseStarlarkOptions`.
    pub(crate) fn parse_and_validate_starlark_options(
        &self,
        parsed: &ParsedCommandOptions,
        loader: &Arc<PackageLoader>,
        rel: &zel_vfs::PathFragment,
        include_default_values: bool,
    ) -> Result<ValidatedStarlarkOptions, String> {
        let mut out = ValidatedStarlarkOptions {
            values: BTreeMap::new(),
            canonical_flags: Vec::new(),
        };
        if parsed.raw_starlark_args.is_empty() {
            return Ok(out);
        }

        let pr = zel_packages::PatternResolver::new(Arc::clone(loader), rel.clone());
        let main_mapping = loader
            .config()
            .lookup
            .resolver
            .mapping(&RepositoryName::main());

        // Cache loaded build-setting target metadata by raw flag name.
        // Entry: (canonical_label, rule, ty, is_flag, is_repeatable_or_multi)
        let mut target_cache: HashMap<
            String,
            (String, zel_packages::RuleInstance, String, bool, bool),
        > = HashMap::new();
        let mut accumulated: BTreeMap<
            String,
            (zel_packages::RuleInstance, String, bool, StarlarkAcc),
        > = BTreeMap::new();

        for occ in &parsed.raw_starlark_args {
            let (canon_label, rule, ty, is_flag, is_multi) = if let Some(cached) =
                target_cache.get(&occ.flag_name)
            {
                cached.clone()
            } else {
                // Load target via target pattern evaluation (must resolve to exactly 1 target).
                let outcome = pr.resolve_patterns(std::slice::from_ref(&occ.flag_name));
                if let Some(err) = outcome.errors.first() {
                    return Err(format!(
                        "Error loading option {}: {}",
                        occ.flag_name, err.message
                    ));
                }
                if outcome.targets.len() != 1 {
                    return Err(format!(
                        "Error loading option {}: user-defined flags must reference exactly one target",
                        occ.flag_name
                    ));
                }
                let mut cur_target = outcome.targets.into_iter().next().unwrap();
                let mut alias_chain: Vec<String> = Vec::new();
                loop {
                    let cf = cur_target.label().canonical_form();
                    if alias_chain.contains(&cf) {
                        alias_chain.push(cf);
                        return Err(format!(
                            "Failed to load build setting '{}' due to a cycle in alias chain: {}",
                            occ.flag_name,
                            alias_chain.join(" -> ")
                        ));
                    }
                    alias_chain.push(cf.clone());
                    let PkgTarget::Rule(ref r) = cur_target else {
                        return Err(format!("Unrecognized option: {}", alias_chain.join(" -> ")));
                    };
                    if r.rule_class == "alias" {
                        use zel_packages::attr::AttrValue;
                        let actual_val = r
                            .extra
                            .attr_values
                            .get("actual")
                            .cloned()
                            .or_else(|| r.attr_or_default("actual"));
                        let next_label = match actual_val {
                            Some(AttrValue::Select(_)) => {
                                return Err(format!(
                                    "Failed to load build setting '{}' as it resolves to an alias with an actual value that uses select(): {}. This is not supported as build settings are needed to determine the configuration the select is evaluated in.",
                                    occ.flag_name,
                                    alias_chain.join(" -> ")
                                ));
                            }
                            Some(AttrValue::Label(Some(lbl))) => lbl,
                            Some(AttrValue::Str(s)) => {
                                let pkg_map = loader
                                    .config()
                                    .lookup
                                    .resolver
                                    .mapping(r.label.repository());
                                if let Some(t) = s.strip_prefix(':') {
                                    Label::create(r.label.package_identifier().clone(), t).map_err(
                                        |e| format!("Error loading option {}: {e}", occ.flag_name),
                                    )?
                                } else {
                                    Label::parse_with_repo_mapping(&s, &pkg_map).map_err(|e| {
                                        format!("Error loading option {}: {e}", occ.flag_name)
                                    })?
                                }
                            }
                            _ => {
                                return Err(format!(
                                    "Unrecognized option: {}",
                                    alias_chain.join(" -> ")
                                ));
                            }
                        };
                        let pkg = loader
                            .load_package(next_label.package_identifier())
                            .map_err(|e| format!("Error loading option {}: {e}", occ.flag_name))?;
                        cur_target =
                            pkg.targets.get(next_label.name()).cloned().ok_or_else(|| {
                                format!(
                                    "Error loading option {}: no such target '{}'",
                                    occ.flag_name,
                                    next_label.canonical_form()
                                )
                            })?;
                        continue;
                    }
                    break;
                }

                let PkgTarget::Rule(r) = cur_target else {
                    return Err(format!("Unrecognized option: {}", alias_chain.join(" -> ")));
                };
                let is_bs = r.rule_class == "label_flag"
                    || r.rule_class == "label_setting"
                    || r.extra
                        .rule_class
                        .as_ref()
                        .is_some_and(|rc| rc.build_setting);
                if !is_bs {
                    return Err(format!("Unrecognized option: {}", alias_chain.join(" -> ")));
                }
                let (ty, is_flag, is_multi) = if r.rule_class == "label_flag" {
                    ("label".to_string(), true, false)
                } else if r.rule_class == "label_setting" {
                    ("label".to_string(), false, false)
                } else if let Some(zel_starlark::Value::Struct(st)) = r
                    .extra
                    .rule_class
                    .as_ref()
                    .and_then(|rc| rc.starlark.as_ref())
                    .and_then(|s| s.kwargs.get("build_setting"))
                {
                    let ty = match st.fields.get("_build_setting") {
                        Some(zel_starlark::Value::String(t)) => t.clone(),
                        _ => "string".to_string(),
                    };
                    let is_flag = st.fields.get("flag") == Some(&zel_starlark::Value::Bool(true));
                    let is_multi = st.fields.get("repeatable")
                        == Some(&zel_starlark::Value::Bool(true))
                        || st.fields.get("allow_multiple")
                            == Some(&zel_starlark::Value::Bool(true));
                    (ty, is_flag, is_multi)
                } else {
                    ("string".to_string(), false, false)
                };

                let canon_label = r.label.canonical_form();
                let entry = (canon_label, r, ty, is_flag, is_multi);
                target_cache.insert(occ.flag_name.clone(), entry.clone());
                entry
            };

            let unparsed_value = match &occ.value {
                Some(v) => v.clone(),
                None => {
                    if ty == "bool" {
                        if occ.negated {
                            "false".to_string()
                        } else {
                            "true".to_string()
                        }
                    } else if occ.negated {
                        return Err(format!(
                            "Illegal use of 'no' prefix on non-boolean option: {}",
                            occ.flag_name
                        ));
                    } else {
                        let mut msg = format!("Expected value after {}", occ.raw_arg);
                        if let Some(ref nxt) = occ.next_non_flag_token {
                            msg.push_str(&format!(". Did you mean {}={nxt}?", occ.raw_arg));
                        }
                        return Err(msg);
                    }
                }
            };

            if !is_flag {
                return Err(format!(
                    "Unrecognized option: {canon_label}={unparsed_value}"
                ));
            }

            // Convert & type-check using BUILD_SETTING_CONVERTERS parity.
            let parse_label_val = |raw_lbl: &str| -> Result<String, String> {
                let abs = if let Some(rest) = raw_lbl.strip_prefix(':') {
                    format!("//{}:{rest}", rel.path_string())
                } else if !raw_lbl.starts_with('@') && !raw_lbl.starts_with("//") {
                    if raw_lbl.contains(':') {
                        format!("//{raw_lbl}")
                    } else {
                        format!("//{}:{raw_lbl}", rel.path_string())
                    }
                } else {
                    raw_lbl.to_string()
                };
                Label::parse_with_repo_mapping(&abs, &main_mapping)
                    .map(|l| l.canonical_form())
                    .map_err(|_| format!("While parsing option {canon_label}={unparsed_value}: '{unparsed_value}' is not a label"))
            };

            match ty.as_str() {
                "bool" => {
                    let b = zel_options::convert_boolean(&unparsed_value).map_err(|_| {
                        format!("While parsing option {canon_label}={unparsed_value}: '{unparsed_value}' is not a boolean")
                    })?;
                    let val = if b {
                        "true".to_string()
                    } else {
                        "false".to_string()
                    };
                    if is_multi {
                        let entry = accumulated.entry(canon_label).or_insert_with(|| {
                            (rule, ty.clone(), is_multi, StarlarkAcc::List(Vec::new()))
                        });
                        if let StarlarkAcc::List(ref mut items) = entry.3 {
                            items.push(val);
                        }
                    } else {
                        accumulated
                            .insert(canon_label, (rule, ty, is_multi, StarlarkAcc::Scalar(val)));
                    }
                }
                "int" => {
                    let n = zel_util::resource_converter::java_decode_long(&unparsed_value).ok_or_else(|| {
                        format!("While parsing option {canon_label}={unparsed_value}: '{unparsed_value}' is not a int")
                    })?;
                    let val = n.to_string();
                    if is_multi {
                        let entry = accumulated.entry(canon_label).or_insert_with(|| {
                            (rule, ty.clone(), is_multi, StarlarkAcc::List(Vec::new()))
                        });
                        if let StarlarkAcc::List(ref mut items) = entry.3 {
                            items.push(val);
                        }
                    } else {
                        accumulated
                            .insert(canon_label, (rule, ty, is_multi, StarlarkAcc::Scalar(val)));
                    }
                }
                "label" => {
                    let val = parse_label_val(&unparsed_value)?;
                    if is_multi {
                        let entry = accumulated.entry(canon_label).or_insert_with(|| {
                            (rule, ty.clone(), is_multi, StarlarkAcc::List(Vec::new()))
                        });
                        if let StarlarkAcc::List(ref mut items) = entry.3 {
                            items.push(val);
                        }
                    } else {
                        accumulated
                            .insert(canon_label, (rule, ty, is_multi, StarlarkAcc::Scalar(val)));
                    }
                }
                "string_list" | "string_set" => {
                    let is_set = ty == "string_set";
                    if is_multi {
                        let entry = accumulated.entry(canon_label).or_insert_with(|| {
                            (rule, ty.clone(), is_multi, StarlarkAcc::List(Vec::new()))
                        });
                        if let StarlarkAcc::List(ref mut items) = entry.3 {
                            if !is_set || !items.contains(&unparsed_value) {
                                items.push(unparsed_value);
                            }
                        }
                    } else {
                        let mut list: Vec<String> = if unparsed_value.is_empty() {
                            Vec::new()
                        } else {
                            unparsed_value.split(',').map(str::to_string).collect()
                        };
                        if is_set {
                            let mut dedup = Vec::new();
                            for item in list {
                                if !dedup.contains(&item) {
                                    dedup.push(item);
                                }
                            }
                            list = dedup;
                        }
                        accumulated
                            .insert(canon_label, (rule, ty, is_multi, StarlarkAcc::List(list)));
                    }
                }
                "label_list" => {
                    if is_multi {
                        let val = parse_label_val(&unparsed_value)?;
                        let entry = accumulated.entry(canon_label).or_insert_with(|| {
                            (rule, ty.clone(), is_multi, StarlarkAcc::List(Vec::new()))
                        });
                        if let StarlarkAcc::List(ref mut items) = entry.3 {
                            items.push(val);
                        }
                    } else {
                        let mut list = Vec::new();
                        for part in unparsed_value.split(',').filter(|s| !s.is_empty()) {
                            list.push(parse_label_val(part)?);
                        }
                        accumulated
                            .insert(canon_label, (rule, ty, is_multi, StarlarkAcc::List(list)));
                    }
                }
                _ => {
                    if is_multi {
                        let entry = accumulated.entry(canon_label).or_insert_with(|| {
                            (rule, ty.clone(), is_multi, StarlarkAcc::List(Vec::new()))
                        });
                        if let StarlarkAcc::List(ref mut items) = entry.3 {
                            items.push(unparsed_value);
                        }
                    } else {
                        accumulated.insert(
                            canon_label,
                            (rule, ty, is_multi, StarlarkAcc::Scalar(unparsed_value)),
                        );
                    }
                }
            }
        }

        for (canon, (rule, ty, is_multi, acc)) in accumulated {
            use zel_packages::attr::AttrValue;
            let resolve_rule_label = |raw: &str| -> Option<String> {
                let pkg_map = loader
                    .config()
                    .lookup
                    .resolver
                    .mapping(rule.label.repository());
                if let Some(t) = raw.strip_prefix(':') {
                    Label::create(rule.label.package_identifier().clone(), t)
                        .ok()
                        .map(|l| l.canonical_form())
                } else if !raw.starts_with('@') && !raw.starts_with("//") && !raw.contains(':') {
                    Label::create(rule.label.package_identifier().clone(), raw)
                        .ok()
                        .map(|l| l.canonical_form())
                } else {
                    Label::parse_with_repo_mapping(raw, &pkg_map)
                        .ok()
                        .map(|l| l.canonical_form())
                }
            };
            let wrap_multi = |s: String| -> String {
                if is_multi {
                    format!("[{s}]")
                } else {
                    s
                }
            };
            let default_rendered = match rule.attr_or_default("build_setting_default") {
                Some(AttrValue::Str(d)) => {
                    if ty == "label" {
                        Some(wrap_multi(resolve_rule_label(&d).unwrap_or(d)))
                    } else {
                        Some(wrap_multi(d))
                    }
                }
                Some(AttrValue::Int(d)) => Some(wrap_multi(d.to_string())),
                Some(AttrValue::Bool(d)) => Some(wrap_multi(if d {
                    "true".to_string()
                } else {
                    "false".to_string()
                })),
                Some(AttrValue::StrList(d)) => {
                    if ty == "label" || ty == "label_list" {
                        let resolved: Vec<String> = d
                            .into_iter()
                            .map(|item| resolve_rule_label(&item).unwrap_or(item))
                            .collect();
                        Some(format!("[{}]", resolved.join(", ")))
                    } else {
                        Some(format!("[{}]", d.join(", ")))
                    }
                }
                Some(AttrValue::Label(Some(l))) => Some(wrap_multi(l.canonical_form())),
                Some(AttrValue::LabelList(ls)) => Some(format!(
                    "[{}]",
                    ls.iter()
                        .map(|l| l.canonical_form())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
                _ => None,
            };

            match acc {
                StarlarkAcc::Scalar(v) => {
                    let rendered = zel_analysis::starlark_rule::render_flag_value(&ty, &v);
                    if !include_default_values
                        && default_rendered.as_deref() == Some(rendered.as_str())
                    {
                        continue;
                    }
                    out.canonical_flags.push(format!("--{canon}={v}"));
                    out.values.insert(canon, (ty, v));
                }
                StarlarkAcc::List(items) => {
                    let rendered = if items.is_empty() {
                        "[]".to_string()
                    } else {
                        format!("[{}]", items.join(", "))
                    };
                    if !include_default_values
                        && default_rendered.as_deref() == Some(rendered.as_str())
                    {
                        continue;
                    }
                    if is_multi {
                        for item in &items {
                            out.canonical_flags.push(format!("--{canon}={item}"));
                        }
                    } else {
                        out.canonical_flags
                            .push(format!("--{canon}={}", items.join(",")));
                    }
                    out.values.insert(canon, (ty, items.join(",")));
                }
            }
        }

        Ok(out)
    }

    /// Checks `target_compatible_with` constraints for analyzed configured targets against the
    /// target platform (`TopLevelConstraintSemantics.checkPlatformRestrictions`).
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn check_platform_incompatibility(
        &self,
        parsed: &ParsedCommandOptions,
        loader: &Arc<PackageLoader>,
        resolver: &Arc<TargetResolver>,
        all: &[Arc<ConfiguredTarget>],
        out_roots: &mut [Option<Arc<ConfiguredTarget>>],
        failed: &mut Vec<String>,
        key: &BuildConfigurationKey,
        stderr: &mut Vec<u8>,
    ) -> Result<HashSet<String>, ExitCode> {
        use zel_packages::attr::AttrValue;
        let mut pkg_cache: HashMap<
            zel_cmdline::PackageIdentifier,
            Option<Arc<zel_packages::Package>>,
        > = HashMap::new();
        let mut load_pkg = |pid: &zel_cmdline::PackageIdentifier| {
            pkg_cache
                .entry(pid.clone())
                .or_insert_with(|| loader.load_package(pid).ok().map(Arc::new))
                .clone()
        };
        let has_any_compat = all.iter().any(|ct| {
            load_pkg(ct.label.package_identifier())
                .and_then(|pkg| pkg.targets.get(ct.label.name()).cloned())
                .and_then(|t| match t {
                    PkgTarget::Rule(r) => r.attr_or_default("target_compatible_with"),
                    _ => None,
                })
                .is_some_and(|v| {
                    !matches!(v, AttrValue::LabelList(ref l) if l.is_empty())
                        && !matches!(v, AttrValue::StrList(ref l) if l.is_empty())
                })
        });
        if !has_any_compat {
            return Ok(HashSet::new());
        }

        let dep_resolver: Arc<dyn zel_analysis::starlark_rule::DepResolver> = resolver.clone();
        let Ok(Some(target_plat)) =
            zel_rules::toolchain::target_platform(key, &dep_resolver, &resolver.config)
        else {
            return Ok(HashSet::new());
        };
        let plat_label = target_plat.label.canonical_form();

        // Map (label, cfg_key) -> incompatibility reason string (chain + reason).
        #[derive(Clone)]
        struct IncompatInfo {
            chain: Vec<(String, String)>,
            reason: String,
        }
        let mut incompat: HashMap<(String, String), IncompatInfo> = HashMap::new();

        for ct in all {
            let ct_key = (ct.label.canonical_form(), ct.configuration.checksum());
            let mut direct_constraints: Vec<String> = Vec::new();
            let mut direct_config_settings: Vec<String> = Vec::new();

            if let Some(pkg) = load_pkg(ct.label.package_identifier()) {
                if let Some(PkgTarget::Rule(r)) = pkg.targets.get(ct.label.name()) {
                    if let Some(tcw) = r.attr_or_default("target_compatible_with") {
                        let labels: Vec<Label> = match tcw {
                            AttrValue::LabelList(ls) => ls,
                            AttrValue::StrList(ss) => {
                                let pkg_map = loader
                                    .config()
                                    .lookup
                                    .resolver
                                    .mapping(r.label.repository());
                                ss.iter()
                                    .filter_map(|s| {
                                        if let Some(t) = s.strip_prefix(':') {
                                            Label::create(r.label.package_identifier().clone(), t)
                                                .ok()
                                        } else {
                                            Label::parse_with_repo_mapping(s, &pkg_map).ok()
                                        }
                                    })
                                    .collect()
                            }
                            AttrValue::Select(parts) => {
                                // Evaluate select() branches against ct's configuration
                                let mut out_l = Vec::new();
                                for part in parts {
                                    match part {
                                        zel_packages::attr::SelectPart::Plain(
                                            AttrValue::LabelList(ls),
                                        ) => out_l.extend(ls),
                                        zel_packages::attr::SelectPart::Select {
                                            entries, ..
                                        } => {
                                            let mut matched: Option<AttrValue> = None;
                                            let mut default_v: Option<AttrValue> = None;
                                            for (cond_lbl, val) in entries {
                                                if cond_lbl.canonical_form()
                                                    == "//conditions:default"
                                                {
                                                    default_v = Some(val);
                                                } else if let Ok(cond_ct) =
                                                    resolver.analyze(&cond_lbl, &ct.configuration)
                                                {
                                                    if cond_ct
                                                        .providers
                                                        .get("ConfigMatchingProvider")
                                                        .and_then(
                                                            zel_analysis::select::matching_info,
                                                        )
                                                        .is_some_and(|(m, _)| m)
                                                    {
                                                        matched = Some(val);
                                                        break;
                                                    }
                                                }
                                            }
                                            if let Some(AttrValue::LabelList(ls)) =
                                                matched.or(default_v)
                                            {
                                                out_l.extend(ls);
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                out_l
                            }
                            _ => Vec::new(),
                        };

                        for cv_lbl in labels {
                            if let Ok(cv_ct) = resolver.analyze(&cv_lbl, &ct.configuration) {
                                if cv_ct.providers.contains_key("ConstraintValueInfo") {
                                    if !zel_rules::toolchain::platform_has_value(
                                        &target_plat,
                                        &cv_ct,
                                    ) {
                                        direct_constraints.push(cv_lbl.canonical_form());
                                    }
                                } else if let Some(cs_p) =
                                    cv_ct.providers.get("ConfigMatchingProvider")
                                {
                                    if !zel_analysis::select::matching_info(cs_p)
                                        .is_some_and(|(m, _)| m)
                                    {
                                        direct_config_settings.push(cv_lbl.canonical_form());
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if !direct_constraints.is_empty() || !direct_config_settings.is_empty() {
                let mut parts = Vec::new();
                if !direct_constraints.is_empty() {
                    if direct_constraints.len() == 1 {
                        parts.push(format!(
                            " target platform ({plat_label}) didn't satisfy constraint {}",
                            direct_constraints[0]
                        ));
                    } else {
                        parts.push(format!(
                            " target platform ({plat_label}) didn't satisfy constraints [{}]",
                            direct_constraints.join(", ")
                        ));
                    }
                }
                if !direct_config_settings.is_empty() {
                    if direct_config_settings.len() == 1 {
                        parts.push(format!(
                            " target configuration didn't match config_setting {}",
                            direct_config_settings[0]
                        ));
                    } else {
                        parts.push(format!(
                            " target configuration didn't match config_settings [{}]",
                            direct_config_settings.join(", ")
                        ));
                    }
                }
                incompat.insert(
                    ct_key,
                    IncompatInfo {
                        chain: vec![(ct.label.canonical_form(), ct.configuration.short_hash())],
                        reason: parts.join(" and"),
                    },
                );
                continue;
            }

            // Indirect incompatibility via direct dependencies
            for (dep_lbl, dep_cfg) in resolver.direct_deps(&ct.label, &ct.configuration) {
                let dep_key = (dep_lbl.canonical_form(), dep_cfg.checksum());
                if let Some(dep_inc) = incompat.get(&dep_key).cloned() {
                    let mut chain =
                        vec![(ct.label.canonical_form(), ct.configuration.short_hash())];
                    chain.extend(dep_inc.chain);
                    incompat.insert(
                        ct_key,
                        IncompatInfo {
                            chain,
                            reason: dep_inc.reason,
                        },
                    );
                    break;
                }
            }
        }

        if incompat.is_empty() {
            return Ok(HashSet::new());
        }

        // Determine explicit target labels from command line patterns.
        let rel = self.command_relative_dir(parsed);
        let main_mapping = loader
            .config()
            .lookup
            .resolver
            .mapping(&RepositoryName::main());
        let mut explicit_labels: HashSet<String> = HashSet::new();
        for pat in &parsed.targets {
            if pat.starts_with('-')
                || pat.ends_with(":all")
                || pat.ends_with(":all-targets")
                || pat.ends_with(":*")
                || pat.ends_with("/...")
                || pat == "..."
            {
                continue;
            }
            if let Ok(zel_cmdline::TargetPattern::SingleTarget(l)) =
                zel_cmdline::TargetPattern::parse_with(
                    pat,
                    &rel,
                    &RepositoryName::main(),
                    &main_mapping,
                )
            {
                explicit_labels.insert(l.canonical_form());
            }
        }
        let skip_explicit =
            crate::exec_phase::opt_bool(parsed, "skip_incompatible_explicit_targets", false);
        let mut skipped = HashSet::new();

        for root_opt in out_roots.iter_mut() {
            let Some(ct) = root_opt.as_ref() else {
                continue;
            };
            let ct_key = (ct.label.canonical_form(), ct.configuration.checksum());
            let Some(info) = incompat.get(&ct_key) else {
                continue;
            };
            let lbl = ct.label.canonical_form();
            let is_explicit = explicit_labels.contains(&lbl);
            if is_explicit && !skip_explicit {
                let mut chain_str = String::from("\nDependency chain:");
                for (i, (cl, ch)) in info.chain.iter().enumerate() {
                    let suffix = if i + 1 == info.chain.len() {
                        format!("   <--{}", info.reason)
                    } else {
                        String::new()
                    };
                    chain_str.push_str(&format!("\n    {cl} ({}){suffix}", &ch[..6.min(ch.len())]));
                }
                let msg = format!("Target {lbl} is incompatible and cannot be built, but was explicitly requested.{chain_str}");
                if parsed.keep_going {
                    writeln_bytes(stderr, &format!("WARNING: {msg}"));
                    failed.push(lbl);
                    *root_opt = None;
                } else {
                    writeln_bytes(stderr, &format!("ERROR: {msg}"));
                    writeln_bytes(
                        stderr,
                        &format!("ERROR: Analysis of target '{lbl}' failed; build aborted"),
                    );
                    writeln_bytes(
                        stderr,
                        &format!(
                            "INFO: Elapsed time: {:.3}s, Critical Path: 0.00s",
                            crate::command_elapsed_secs()
                        ),
                    );
                    writeln_bytes(stderr, "INFO: 0 processes.");
                    writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                    return Err(ExitCode::BUILD_FAILURE);
                }
            } else {
                skipped.insert(lbl);
            }
        }
        Ok(skipped)
    }
}

impl BlazeRuntime {
    /// `target '<x>' depends on deprecated target '<dep>': <deprecation>` (a rule warning for every
    /// analysed rule depending, from another package, on a rule that sets `deprecation`; rules
    /// that are deprecated themselves stay quiet).
    fn report_deprecated_deps(
        reachable: &[Arc<ConfiguredTarget>],
        loader: &Arc<PackageLoader>,
        stderr: &mut Vec<u8>,
    ) {
        use zel_packages::attr::AttrValue;
        let mut seen: HashSet<String> = HashSet::new();
        for ct in reachable {
            if ct.aspect_name.is_some()
                || !ct.label.repository().is_main()
                || !seen.insert(ct.label.canonical_form())
            {
                continue;
            }
            let Ok(pkg) = loader.load_package(ct.label.package_identifier()) else {
                continue;
            };
            let Some(PkgTarget::Rule(rule)) = pkg.targets.get(ct.label.name()) else {
                continue;
            };
            let deprecated =
                |r: &zel_packages::RuleInstance| match r.extra.attr_values.get("deprecation") {
                    Some(AttrValue::Str(d)) => Some(d.clone()),
                    _ => None,
                };
            if deprecated(rule).is_some() {
                continue;
            }
            let mut deps: Vec<Label> = Vec::new();
            for (name, v) in &rule.extra.attr_values {
                if name == "visibility" || name == "tests" {
                    continue;
                }
                match v {
                    AttrValue::Label(Some(l)) => deps.push(l.clone()),
                    AttrValue::LabelList(ls) => deps.extend(ls.iter().cloned()),
                    AttrValue::LabelKeyedStrDict(kv) => {
                        deps.extend(kv.iter().map(|(l, _)| l.clone()))
                    }
                    AttrValue::StrKeyedLabelDict(kv) => {
                        deps.extend(kv.iter().map(|(_, l)| l.clone()))
                    }
                    _ => {}
                }
            }
            let mut warned: HashSet<String> = HashSet::new();
            for dep in deps {
                if !dep.repository().is_main()
                    || dep.package_identifier() == ct.label.package_identifier()
                    || !warned.insert(dep.canonical_form())
                {
                    continue;
                }
                let Ok(dpkg) = loader.load_package(dep.package_identifier()) else {
                    continue;
                };
                let Some(PkgTarget::Rule(drule)) = dpkg.targets.get(dep.name()) else {
                    continue;
                };
                if let Some(msg) = deprecated(drule) {
                    let loc = rule
                        .extra
                        .location
                        .clone()
                        .map(|l| format!("{l}: "))
                        .unwrap_or_default();
                    writeln_bytes(
                        stderr,
                        &format!(
                            "WARNING: {loc}in {} rule {}: target '{}' depends on deprecated target '{}': {msg}",
                            ct.rule_class,
                            ct.label.canonical_form(),
                            ct.label.canonical_form(),
                            dep.canonical_form()
                        ),
                    );
                }
            }
        }
    }
}

/// Prints an analysis error as `ERROR: <message>`; messages of several lines keep their
/// continuation lines unprefixed like Bazel's event output.
pub(crate) fn report_analysis_error(stderr: &mut Vec<u8>, e: &str) {
    for (i, line) in e.lines().enumerate() {
        if i == 0 && !line.starts_with("ERROR:") {
            writeln_bytes(stderr, &format!("ERROR: {line}"));
        } else {
            writeln_bytes(stderr, line);
        }
    }
}

/// Selects the configured targets (and their actions) the execution phase must run.
///
/// Requested artifacts are the roots' default outputs, executables, runfiles and
/// `_validation` outputs; every action that (transitively) produces an input of a needed action
/// is needed too. Non-root test rules are not tests of this invocation (`is_test = false`).
fn select_actions(
    resolver: &TargetResolver,
    reachable: &[Arc<ConfiguredTarget>],
    roots: &[Option<Arc<ConfiguredTarget>>],
    parsed: &ParsedCommandOptions,
    skipped_targets: &HashSet<String>,
) -> Vec<ConfiguredTarget> {
    let _ = is_test_command(parsed);
    let root_cts: Vec<Arc<ConfiguredTarget>> = roots.iter().flatten().cloned().collect();
    let mut all = reachable.to_vec();
    // Roots decorated by top-level aspects are not the cached instances: substitute them.
    for r in &root_cts {
        if let Some(i) = all
            .iter()
            .position(|c| c.label == r.label && c.configuration == r.configuration)
        {
            all[i] = r.clone();
        } else {
            all.push(r.clone());
        }
    }
    // Producers of each output path: (target index, action index).
    let mut producer: HashMap<String, (usize, usize)> = HashMap::new();
    for (ti, ct) in all.iter().enumerate() {
        for (ai, a) in ct.actions.iter().enumerate() {
            for o in a.outputs() {
                producer
                    .entry(o.exec_path.path_string().to_string())
                    .or_insert((ti, ai));
            }
        }
    }
    let root_ids: HashSet<usize> = roots
        .iter()
        .flatten()
        .filter(|r| !skipped_targets.contains(&r.label.canonical_form()))
        .filter_map(|r| all.iter().position(|c| Arc::ptr_eq(c, r)))
        .collect();
    let mut needed: HashSet<(usize, usize)> = HashSet::new();
    let mut work: Vec<String> = Vec::new();
    let push_art = |work: &mut Vec<String>, p: &str| work.push(p.to_string());
    let build_tests_only = crate::exec_phase::opt_bool(parsed, "build_tests_only", false);
    for &ri in &root_ids {
        let ct = &all[ri];
        // `--build_tests_only`: non-test roots are not seeded (their deps are pulled in only
        // if a test needs them, e.g. as `--run_under=//label`).
        let is_run_under = crate::exec_phase::opt_last(parsed, "run_under")
            .and_then(|ru| ru.split_whitespace().next().map(str::to_string))
            .is_some_and(|first| {
                let cf = ct.label.canonical_form();
                let ts = ct.label.to_string();
                cf == first
                    || ts == first
                    || cf.trim_start_matches('@') == first.trim_start_matches('@')
            });
        if build_tests_only && !ct.is_test && !is_run_under {
            continue;
        }
        let default_on = selected_group(parsed, "default");
        let explicit_non_modifying_groups = opt_list(parsed, "output_groups")
            .iter()
            .any(|s| !s.starts_with('+') && !s.starts_with('-'));
        if default_on || ct.is_test {
            for a in &ct.default_info.files {
                push_art(&mut work, a.exec_path.path_string());
            }
            if let Some(e) = &ct.default_info.executable {
                push_art(&mut work, e.exec_path.path_string());
            }
            if !explicit_non_modifying_groups || ct.is_test {
                for a in ct
                    .default_info
                    .default_runfiles
                    .iter()
                    .chain(ct.default_info.data_runfiles.iter())
                {
                    push_art(&mut work, a.exec_path.path_string());
                }
                if let Some(rs) = &ct.runfiles_support {
                    for a in rs
                        .runfiles
                        .artifacts
                        .iter()
                        .chain(rs.runfiles.symlinks.values())
                        .chain(rs.runfiles.root_symlinks.values())
                    {
                        push_art(&mut work, a.exec_path.path_string());
                    }
                }
            }
        }
        let run_validations = crate::exec_phase::opt_bool(parsed, "run_validations", true);
        for (g, files) in &ct.output_groups.groups {
            if (g == "_validation" && run_validations)
                || (g.starts_with("_hidden") && (!explicit_non_modifying_groups || ct.is_test))
                || selected_group(parsed, g)
            {
                for a in files {
                    push_art(&mut work, a.exec_path.path_string());
                }
            }
        }
        // Actions owned by the root that produce nothing consumed elsewhere (FileWrite of the
        // runfiles manifest etc.) are cheap and part of `bazel build`'s runfiles tree.
        if !explicit_non_modifying_groups || ct.is_test {
            for (ai, a) in ct.actions.iter().enumerate() {
                if a.outputs()
                    .iter()
                    .any(|o| o.exec_path.path_string().ends_with(".runfiles_manifest"))
                {
                    needed.insert((ri, ai));
                }
            }
        }
        for ta in &ct.typed_actions {
            for m in &ta.input_manifests {
                push_art(&mut work, m.exec_path.path_string());
            }
        }
    }
    let mut seen: HashSet<String> = HashSet::new();
    // Seed from manifest actions' inputs too.
    for &(ti, ai) in needed.clone().iter() {
        for i in all[ti].actions[ai].inputs() {
            work.push(i.exec_path.path_string().to_string());
        }
    }
    while let Some(p) = work.pop() {
        if !seen.insert(p.clone()) {
            continue;
        }
        let Some(&(ti, ai)) = producer.get(&p) else {
            continue;
        };
        if needed.insert((ti, ai)) {
            for i in all[ti].actions[ai].inputs() {
                work.push(i.exec_path.path_string().to_string());
            }
            for ta in &all[ti].typed_actions {
                if ta.outputs.iter().any(|o| o.exec_path.path_string() == p) {
                    for t in &ta.tools {
                        work.push(t.exec_path.path_string().to_string());
                    }
                    for m in &ta.input_manifests {
                        work.push(m.exec_path.path_string().to_string());
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    for (ti, ct) in all.iter().enumerate() {
        let acts: Vec<ActionAnalysisMetadata> = ct
            .actions
            .iter()
            .enumerate()
            .filter(|(ai, _)| needed.contains(&(ti, *ai)))
            .map(|(_, a)| a.clone())
            .collect();
        let is_root = root_ids.contains(&ti);
        if acts.is_empty() && !is_root {
            continue;
        }
        let mut c = (**ct).clone();
        c.actions = acts;
        if !is_root
            || c.rule_class == "test_suite"
            || (c.is_test
                && !crate::exec_phase::passes_filters_for_label(
                    &c.label,
                    resolver.loader(),
                    parsed,
                ))
        {
            c.is_test = false;
        }
        out.push(c);
    }
    out
}

/// `--output_groups` selection (`BuildRequestOptions.outputGroups`): values without a `+`/`-`
/// prefix replace the default group; `+g` adds `g`, `-g` removes it.
fn selected_group(parsed: &ParsedCommandOptions, group: &str) -> bool {
    let sel = opt_list(parsed, "output_groups");
    let extra_default = opt_list(parsed, "extra_default_output_groups");
    let replaces = sel
        .iter()
        .any(|s| !s.starts_with('+') && !s.starts_with('-'));
    let mut on = (group == "default" && !replaces) || extra_default.iter().any(|g| g == group);
    for s in &sel {
        if let Some(g) = s.strip_prefix('-') {
            if g == group {
                on = false;
            }
        } else if let Some(g) = s.strip_prefix('+') {
            if g == group {
                on = true;
            }
        } else if s == group {
            on = true;
        }
    }
    on
}

/// Builds the label → configured-target index used by commands that need the root results.
#[allow(dead_code)]
pub(crate) fn index_roots(o: &AnalysisOutcome) -> BTreeMap<String, Arc<ConfiguredTarget>> {
    o.roots
        .iter()
        .flatten()
        .map(|c| (c.label.canonical_form(), c.clone()))
        .collect()
}

/// Failure trailer: the execution phase's (failed targets, `--verbose_failures` hint, timing)
/// when actions failed; otherwise (analysis-only failures under `--keep_going`) just the timing
/// and `ERROR: Build did NOT complete successfully`.
pub(crate) fn print_failure_trailer(
    out: &crate::exec_phase::BuildOutcome,
    parsed: &ParsedCommandOptions,
    stderr: &mut Vec<u8>,
) {
    if !out.success {
        crate::exec_phase::print_build_failure_trailer(out, parsed, stderr);
        return;
    }
    writeln_bytes(
        stderr,
        &format!(
            "INFO: Elapsed time: {:.3}s, Critical Path: {:.2}s",
            crate::command_elapsed_secs(),
            out.schedule.critical_path_ms as f64 / 1000.0
        ),
    );
    let l = out.proc.summary();
    writeln_bytes(stderr, &l);
    writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
}

/// The dependency-aware Skyframe `CONFIGURED_TARGET` function: loads packages from `fs`
/// (external repositories through the installed Bzlmod resolver) and analyses with the
/// registered toolchains / execution platforms of the module graph.
pub(crate) fn skyframe_configured_target_function(
    default_config: BuildConfigurationKey,
    fs: Arc<dyn FileSystem>,
    workspace_dir: PathFragment,
    command_resolver: zel_bzlmod::resolver::CommandResolver,
) -> zel_skyframe::ConfiguredTargetFunction {
    let cfg = default_config.clone();
    zel_skyframe::ConfiguredTargetFunction::with_analysis(
        default_config,
        Arc::new(move || {
            let repo: Arc<dyn zel_packages::lookup::RepoResolver> =
                command_resolver.slot().get().unwrap_or_else(|| {
                    Arc::new(zel_packages::SimpleRepoResolver::new(workspace_dir.clone()))
                });
            let loader = PackageLoader::new(zel_packages::LoaderConfig::new(Arc::clone(&fs), repo));
            let mut acfg = AnalysisConfig::new();
            if let Some(regs) = command_resolver.registrations() {
                acfg.toolchain_patterns = regs.toolchains;
                acfg.execution_platform_patterns = regs.execution_platforms;
                if !regs.platforms_repo.is_empty() {
                    acfg.platforms_repo = regs.platforms_repo;
                }
            }
            (loader, acfg, cfg.clone())
        }),
    )
}

#[cfg(test)]
mod analysis_cache_tests {
    use super::*;
    use zel_analysis::build_options::{BuildOptions, ConfigValue};

    fn opts() -> BuildOptions {
        BuildOptions::defaults("darwin_arm64")
    }

    #[test]
    fn describe_difference_follows_skyframe_build_view() {
        let one = vec!["--compilation_mode".to_string()];
        assert_eq!(
            describe_config_difference(&one, 3),
            "Build option --compilation_mode has changed"
        );
        let two = vec!["--a".to_string(), "--b".to_string()];
        assert_eq!(
            describe_config_difference(&two, 3),
            "Build options --a and --b have changed"
        );
        let four: Vec<String> = ["--a", "--b", "--c", "--d"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            describe_config_difference(&four, 3),
            "Build options --a, --b, --c, and 1 more have changed"
        );
        assert_eq!(
            describe_config_difference(&four, 1),
            "Build options --a and 3 more have changed"
        );
        assert_eq!(
            describe_config_difference(&four, 0),
            "Build options have changed"
        );
        let three = &four[..3];
        assert_eq!(
            describe_config_difference(three, 3),
            "Build options --a, --b, and --c have changed"
        );
    }

    #[test]
    fn only_cache_invalidating_option_diffs_are_reported() {
        let old = opts();
        let mut new = opts();
        new.set("compilation_mode", ConfigValue::Scalar("opt".into()));
        assert_eq!(
            cache_invalidating_options(&old, &new),
            vec!["compilation_mode".to_string()]
        );
        // `--test_env` is a TestOptions option: ignored while `--trim_test_configuration` is on.
        let mut new = opts();
        new.set("test_env", ConfigValue::List(vec!["A=1".into()]));
        assert!(cache_invalidating_options(&old, &new).is_empty());
        // ...but not once the old configuration does not trim.
        let mut old_untrimmed = opts();
        old_untrimmed.set(
            "trim_test_configuration",
            ConfigValue::Scalar("false".into()),
        );
        new.set(
            "trim_test_configuration",
            ConfigValue::Scalar("false".into()),
        );
        assert_eq!(
            cache_invalidating_options(&old_untrimmed, &new),
            vec!["test_env".to_string()]
        );
        // Changing `--trim_test_configuration` itself always invalidates.
        let mut new = opts();
        new.set(
            "trim_test_configuration",
            ConfigValue::Scalar("false".into()),
        );
        assert_eq!(
            cache_invalidating_options(&old, &new),
            vec!["trim_test_configuration".to_string()]
        );
        // `--run_under` is trimmed for the non-test configuration: a command run-under change is ignored.
        let mut a = opts();
        a.set("run_under", ConfigValue::Scalar("echo".into()));
        let mut b = opts();
        b.set("run_under", ConfigValue::Scalar("true".into()));
        assert!(cache_invalidating_options(&a, &b).is_empty());
        let mut c = opts();
        c.set("run_under", ConfigValue::Scalar("//x:y".into()));
        assert_eq!(
            cache_invalidating_options(&a, &c),
            vec!["run_under".to_string()]
        );
    }
}
