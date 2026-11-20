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

//! `.bazelrc` file discovery, tokenizer, import resolution, semantic version conditions, and inheritance.
//! Mirrors `src/main/cpp/rc_file.cc`, `src/main/cpp/sem_ver.cc`, and `src/main/cpp/option_processor.cc`.

use crate::error::OptionsParsingError;
use crate::flag_tables::{get_startup_options_map, COMMAND_INHERITANCE};
use crate::priority::{OptionPriority, PriorityCategory};
use crate::startup::{absolute_path_from_flag, parse_startup_flag_token, StartupOptions};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::io;
use zel_util::Os;
use zel_vfs::{FileSystem, PathFragment};

/// Buffered `BAZEL_LOG(INFO)` lines of the C++ client (`file:line`, message); the launcher
/// flushes them in front of a fatal error (`BAZEL_DIE`), so they are collected here.
static CLIENT_LOG: std::sync::Mutex<Vec<(&'static str, String)>> =
    std::sync::Mutex::new(Vec::new());

fn client_log(location: &'static str, msg: String) {
    CLIENT_LOG
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((location, msg));
}

/// Drains the buffered client INFO log (`("src/main/cpp/rc_file.cc:94", "Parsing the RcFile X")`, ...).
pub fn take_client_log() -> Vec<(&'static str, String)> {
    std::mem::take(&mut *CLIENT_LOG.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Formats an I/O error like the C++ client does (`(error: 2): No such file or directory`).
fn errno_text(e: &io::Error) -> String {
    let s = e.to_string();
    if let Some((msg, rest)) = s.rsplit_once(" (os error ") {
        return format!("(error: {}): {msg}", rest.trim_end_matches(')'));
    }
    if e.kind() == io::ErrorKind::NotFound {
        return "(error: 2): No such file or directory".to_string();
    }
    s
}

/// Distinguishes a direct unreadable target file (`ParseError::UNREADABLE_FILE`, ignored by `try-import`)
/// from malformed syntax, import loops, or failed nested mandatory `import`s.
enum RcLoadError {
    UnreadableTarget(OptionsParsingError),
    ParseFailure(OptionsParsingError),
}

impl RcLoadError {
    fn into_inner(self) -> OptionsParsingError {
        match self {
            Self::UnreadableTarget(e) | Self::ParseFailure(e) => e,
        }
    }
}

/// Parsed `.bazelrc` configuration containing command and config entries.
#[derive(Debug, Clone, Default)]
pub struct BazelrcConfig {
    /// Maps section key (e.g. `"startup"`, `"common"`, `"build"`, `"build:opt"`) to list of `(option, source_path)`.
    pub sections: BTreeMap<String, Vec<(String, String)>>,
    /// Distinct loaded rc file paths in first-load order.
    pub loaded_rc_files: Vec<PathFragment>,
    /// Canonical paths of every rc file load (including duplicate imports).
    pub canonical_rc_paths: Vec<PathFragment>,
    /// Non-fatal warnings collected during rc discovery and loading (e.g. duplicate rc files).
    pub warnings: Vec<String>,
}

impl BazelrcConfig {
    /// Expands command options for a given `command` (including `always`, `common`, `<command>`, and `--config=<name>`).
    pub fn expand_command_args(
        &self,
        command: &str,
        cli_args: &[String],
    ) -> Result<Vec<String>, OptionsParsingError> {
        let (expanded, _warnings) =
            crate::config_expander::expand_command_args(self, command, cli_args)?;
        Ok(expanded)
    }

    /// Expands command options returning both expanded arguments and Bazel-matching rc info diagnostics.
    pub fn expand_command_args_with_diagnostics(
        &self,
        command: &str,
        cli_args: &[String],
    ) -> Result<(Vec<String>, Vec<String>), OptionsParsingError> {
        let (expanded, warnings) =
            crate::config_expander::expand_command_args(self, command, cli_args)?;
        let mut notes = Vec::new();
        let commands_to_parse = get_command_ancestry(command);
        for cmd in &commands_to_parse {
            if let Some(opts) = self.sections.get(*cmd) {
                if !opts.is_empty() {
                    let inherited = if *cmd == command { "" } else { "Inherited " };
                    let mut by_file: BTreeMap<String, Vec<String>> = BTreeMap::new();
                    for (opt, src) in opts {
                        by_file.entry(src.clone()).or_default().push(opt.clone());
                    }
                    for (src, args) in by_file {
                        let mut note = format!(
                            "INFO: Reading rc options for '{command}' from {src}:\n  {inherited}'{cmd}' options: {}",
                            args.join(" ")
                        );
                        if *cmd == "common" {
                            let ignored =
                                crate::parser::split_unsupported_common_args(command, &args).1;
                            if !ignored.is_empty() {
                                note.push_str(&format!(
                                    "\n  Ignored as unsupported by '{command}': {}",
                                    ignored.join(" ")
                                ));
                            }
                        }
                        notes.push(note);
                    }
                }
            }
        }
        notes.extend(warnings);
        Ok((expanded, notes))
    }

    /// Parses an RC file from string content given its source path and workspace directory.
    pub fn parse_str(
        &mut self,
        content: &str,
        source_path: &str,
        workspace_dir: &PathFragment,
        fs: &dyn FileSystem,
        visited: &mut HashSet<PathFragment>,
    ) -> Result<(), OptionsParsingError> {
        visited.insert(PathFragment::create(source_path));
        let mut import_stack = vec![source_path.to_string()];
        self.parse_str_internal(
            content,
            source_path,
            Some(workspace_dir),
            Some(workspace_dir),
            fs,
            "7.0.0",
            &mut import_stack,
        )
        .map_err(RcLoadError::into_inner)
    }

    /// Parses an RC file with explicit build label.
    pub fn parse_str_with_label(
        &mut self,
        content: &str,
        source_path: &str,
        workspace_dir: Option<&PathFragment>,
        fs: &dyn FileSystem,
        build_label: &str,
    ) -> Result<(), OptionsParsingError> {
        let mut import_stack = vec![source_path.to_string()];
        self.parse_str_internal(
            content,
            source_path,
            workspace_dir,
            workspace_dir,
            fs,
            build_label,
            &mut import_stack,
        )
        .map_err(RcLoadError::into_inner)
    }

    /// Reads and parses an RC file from filesystem.
    pub fn parse_file(
        &mut self,
        rc_path: &PathFragment,
        workspace_dir: Option<&PathFragment>,
        fs: &dyn FileSystem,
        build_label: &str,
    ) -> Result<(), OptionsParsingError> {
        self.parse_file_with_cwd(rc_path, workspace_dir, workspace_dir, fs, build_label)
    }

    /// Reads and parses an RC file from filesystem resolving relative non-`%workspace%/` imports against `cwd`.
    pub fn parse_file_with_cwd(
        &mut self,
        rc_path: &PathFragment,
        workspace_dir: Option<&PathFragment>,
        cwd: Option<&PathFragment>,
        fs: &dyn FileSystem,
        build_label: &str,
    ) -> Result<(), OptionsParsingError> {
        let mut import_stack = vec![rc_path.path_string().to_string()];
        self.parse_file_internal(
            rc_path,
            rc_path.path_string(),
            workspace_dir,
            cwd,
            fs,
            build_label,
            &mut import_stack,
        )
        .map_err(RcLoadError::into_inner)
    }

    #[allow(clippy::too_many_arguments)]
    fn parse_file_internal(
        &mut self,
        rc_path: &PathFragment,
        display_path: &str,
        workspace_dir: Option<&PathFragment>,
        cwd: Option<&PathFragment>,
        fs: &dyn FileSystem,
        build_label: &str,
        import_stack: &mut Vec<String>,
    ) -> Result<(), RcLoadError> {
        client_log(
            "src/main/cpp/rc_file.cc:94",
            format!("Parsing the RcFile {display_path}"),
        );
        let bytes = fs.read_file(rc_path).map_err(|e| {
            RcLoadError::UnreadableTarget(OptionsParsingError::new(format!(
                "Unexpected error reading config file '{display_path}': {}",
                errno_text(&e)
            )))
        })?;
        let content = String::from_utf8_lossy(&bytes);
        if !self.loaded_rc_files.contains(rc_path) {
            self.loaded_rc_files.push(rc_path.clone());
        }
        let canonical = canonicalize_rc_path(fs, rc_path);
        self.canonical_rc_paths
            .push(PathFragment::create(&canonical));
        self.parse_str_internal(
            &content,
            &canonical,
            workspace_dir,
            cwd,
            fs,
            build_label,
            import_stack,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn parse_str_internal(
        &mut self,
        content: &str,
        source_path: &str,
        workspace_dir: Option<&PathFragment>,
        cwd: Option<&PathFragment>,
        fs: &dyn FileSystem,
        build_label: &str,
        import_stack: &mut Vec<String>,
    ) -> Result<(), RcLoadError> {
        // `rc_file.cc`: "A '\\' at the end of a line continues the line" - both `\\\r\n` and `\\\n` are deleted
        // from the whole file before it is split on '\n' and each line stripped.
        let content = content.replace("\\\r\n", "").replace("\\\n", "");
        for raw_line in content.split('\n') {
            let full_line = raw_line.trim().to_string();
            let tokens = tokenize_rc_line(&full_line);
            if tokens.is_empty() {
                continue;
            }

            let command = &tokens[0];

            // Directives: import, try-import, try-import-if-bazel-version
            if command == "import"
                || command == "try-import"
                || command == "try-import-if-bazel-version"
            {
                let is_try = command.starts_with("try-import");
                let is_version_conditioned = command == "try-import-if-bazel-version";

                if (is_version_conditioned && tokens.len() != 3)
                    || (!is_version_conditioned && tokens.len() != 2)
                {
                    return Err(RcLoadError::ParseFailure(OptionsParsingError::new(
                        format!(
                            "Invalid import declaration in config file '{source_path}': '{full_line}'"
                        ),
                    )));
                }

                if is_version_conditioned {
                    let condition = &tokens[1];
                    let matches = match eval_bazel_version_condition(condition, build_label) {
                        Ok(m) => m,
                        Err(err_msg) => {
                            return Err(RcLoadError::ParseFailure(OptionsParsingError::new(
                                format!(
                                    "Invalid version condition in config file '{source_path}': '{full_line}'. {err_msg}"
                                ),
                            )));
                        }
                    };
                    if !matches {
                        continue;
                    }
                }

                let import_target_str = if is_version_conditioned {
                    &tokens[2]
                } else {
                    &tokens[1]
                };

                let (resolved_path, stack_entry) = if let Some(rel) =
                    import_target_str.strip_prefix("%workspace%/")
                {
                    let Some(ws) = workspace_dir else {
                        if !is_try {
                            return Err(RcLoadError::ParseFailure(OptionsParsingError::new(format!(
                                "Nonexistent path in import declaration in config file '{source_path}': '{full_line}'. This is because no workspace was found and %workspace%-relative imports are only supported when running from within a workspace."
                            ))));
                        }
                        client_log(
                            "src/main/cpp/rc_file.cc:205",
                            format!(
                                "Skipped optional import of {import_target_str}, the specified rc file either does not exist or is not readable."
                            ),
                        );
                        continue;
                    };
                    let p = ws.get_relative_str(rel.trim_start_matches('/'));
                    if !fs.exists(&p, true) {
                        if is_try {
                            client_log(
                                "src/main/cpp/rc_file.cc:205",
                                format!(
                                    "Skipped optional import of {import_target_str}, the specified rc file either does not exist or is not readable."
                                ),
                            );
                            continue;
                        }
                        return Err(RcLoadError::ParseFailure(OptionsParsingError::new(format!(
                            "Nonexistent path in import declaration in config file '{source_path}': '{full_line}' (are you in your source checkout/WORKSPACE?)"
                        ))));
                    }
                    let entry = p.path_string().to_string();
                    (p, entry)
                } else {
                    // Non-`%workspace%/` imports are opened literally as written (`rc_file.cc`),
                    // so relative paths resolve against `cwd` rather than the importing file's directory.
                    let target_pf = PathFragment::create(import_target_str);
                    let fs_pf = if target_pf.is_absolute() {
                        target_pf
                    } else if let Some(c) = cwd {
                        c.get_relative(&target_pf)
                    } else if let Some(ws) = workspace_dir {
                        ws.get_relative(&target_pf)
                    } else if let Ok(cur) = std::env::current_dir() {
                        PathFragment::create(&cur.to_string_lossy()).get_relative(&target_pf)
                    } else {
                        target_pf
                    };
                    (fs_pf, import_target_str.to_string())
                };

                // Import cycles are exact string matches on the path pushed onto `import_stack`.
                if import_stack.iter().any(|p| p == &stack_entry) {
                    let mut loop_str = String::from("Import loop detected:\n");
                    for p in import_stack.iter() {
                        loop_str.push_str(&format!("  {p}\n"));
                    }
                    loop_str.push_str(&format!("  {stack_entry}\n"));
                    return Err(RcLoadError::ParseFailure(OptionsParsingError::new(
                        loop_str,
                    )));
                }

                import_stack.push(stack_entry.clone());
                let sub_res = self.parse_file_internal(
                    &resolved_path,
                    &stack_entry,
                    workspace_dir,
                    cwd,
                    fs,
                    build_label,
                    import_stack,
                );
                import_stack.pop();

                match sub_res {
                    Ok(()) => {}
                    Err(RcLoadError::UnreadableTarget(e)) => {
                        if is_try {
                            client_log(
                                "src/main/cpp/rc_file.cc:234",
                                format!(
                                    "Skipped optional import of {import_target_str}, the specified rc file either does not exist or is not readable."
                                ),
                            );
                            continue;
                        }
                        return Err(RcLoadError::ParseFailure(e));
                    }
                    Err(RcLoadError::ParseFailure(e)) => {
                        return Err(RcLoadError::ParseFailure(e));
                    }
                }
                continue;
            }

            // Normal section line: command [option ...]
            // `rc_file.cc` has no command whitelist; any non-directive first token is stored as a section.
            let cmd_base = match command.split_once(':') {
                Some((base, _)) => base,
                None => command.as_str(),
            };

            if cmd_base == "startup" {
                for tok in &tokens[1..] {
                    let flag_only = tok.split('=').next().unwrap_or(tok.as_str());
                    if flag_only == "--bazelrc" {
                        return Err(RcLoadError::ParseFailure(OptionsParsingError::new(
                            "Can't specify --bazelrc in the RC file.",
                        )));
                    }
                    if matches!(
                        flag_only,
                        "--system_rc"
                            | "--nosystem_rc"
                            | "--workspace_rc"
                            | "--noworkspace_rc"
                            | "--home_rc"
                            | "--nohome_rc"
                            | "--ignore_all_rc_files"
                            | "--noignore_all_rc_files"
                    ) {
                        return Err(RcLoadError::ParseFailure(OptionsParsingError::new(
                            format!("Can't specify {flag_only} in the RC file."),
                        )));
                    }
                }
            }

            let entry = self.sections.entry(command.clone()).or_default();
            for tok in &tokens[1..] {
                entry.push((tok.clone(), source_path.to_string()));
            }
        }
        Ok(())
    }

    /// Returns all options for the given command, following command inheritance:
    /// `always` -> `common` -> `build` -> `test`, etc.
    pub fn get_inherited_command_options(&self, command: &str) -> Vec<(String, String)> {
        let mut res = Vec::new();
        let hierarchy = get_command_ancestry(command);
        for cmd in hierarchy {
            if let Some(opts) = self.sections.get(cmd) {
                res.extend(opts.iter().cloned());
            }
        }
        res
    }
}

fn canonicalize_rc_path(fs: &dyn FileSystem, rc_path: &PathFragment) -> String {
    if let Ok(p) = std::fs::canonicalize(rc_path.path_string()) {
        let s = p.to_string_lossy();
        #[cfg(windows)]
        let s = zel_vfs::strip_windows_nt_prefix(&s);
        return s.into_owned();
    }
    if let Ok(resolved) = zel_vfs::resolve_symlink_chain(fs, rc_path) {
        return resolved.real_path.path_string().to_string();
    }
    rc_path.path_string().to_string()
}

fn expand_windows_percent_vars(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find('%') {
            let var = &after[..end];
            if !var.is_empty() {
                if let Ok(val) = std::env::var(var) {
                    out.push_str(&val);
                    rest = &after[end + 1..];
                    continue;
                }
            }
            out.push('%');
            out.push_str(var);
            out.push('%');
            rest = &after[end + 1..];
        } else {
            out.push('%');
            out.push_str(after);
            return out;
        }
    }
    out.push_str(rest);
    out
}

/// Discovers and loads rc files in full parity with Bazel's discovery order:
/// 1. `/etc/bazel.bazelrc` or `%ProgramData%/bazel.bazelrc` (system rc)
/// 2. `%workspace%/.bazelrc` (workspace rc)
/// 3. `$HOME/.bazelrc` (home rc)
/// 4. `$BAZELRC` env var
/// 5. `--bazelrc` CLI flags (`/dev/null` stops subsequent `--bazelrc` entries)
pub fn discover_and_load_rc_files(
    startup: &mut StartupOptions,
    workspace_dir: Option<&PathFragment>,
    home_dir: Option<&str>,
    cwd: &PathFragment,
    fs: &dyn FileSystem,
    build_label: &str,
) -> Result<BazelrcConfig, OptionsParsingError> {
    let mut config = BazelrcConfig::default();
    if startup.ignore_all_rc_files {
        if !startup.bazelrc_files.is_empty() {
            eprintln!("WARNING: Value of --bazelrc is ignored, since --ignore_all_rc_files is on.");
        }
        if startup.home_rc && startup.parsed_options.contains_key("home_rc") {
            eprintln!("WARNING: Explicit value of --home_rc is ignored, since --ignore_all_rc_files is on.");
        }
        if startup.system_rc && startup.parsed_options.contains_key("system_rc") {
            eprintln!("WARNING: Explicit value of --system_rc is ignored, since --ignore_all_rc_files is on.");
        }
        if startup.workspace_rc && startup.parsed_options.contains_key("workspace_rc") {
            eprintln!("WARNING: Explicit value of --workspace_rc is ignored, since --ignore_all_rc_files is on.");
        }
        startup.absolutize_paths(cwd, home_dir);
        return Ok(config);
    }

    let mut potential_rc_files: Vec<(PathFragment, bool)> = Vec::new(); // (path, is_explicit)

    // 1. System rc (unless --nosystem_rc)
    if startup.system_rc {
        let sys_rc = match Os::current() {
            Os::Windows => {
                let expanded = expand_windows_percent_vars("%ProgramData%/bazel.bazelrc");
                let path = if expanded.contains("%ProgramData%") {
                    "C:/ProgramData/bazel.bazelrc".to_string()
                } else {
                    expanded
                };
                PathFragment::create(&path)
            }
            _ => PathFragment::create("/etc/bazel.bazelrc"),
        };
        potential_rc_files.push((sys_rc, false));
    }

    // 2. Workspace rc: %workspace%/.bazelrc (unless --noworkspace_rc)
    if startup.workspace_rc {
        if let Some(ws) = workspace_dir {
            potential_rc_files.push((ws.get_relative_str(".bazelrc"), false));
        }
    }

    // 3. User rc: $HOME/.bazelrc (unless --nohome_rc)
    if startup.home_rc {
        if let Some(h) = home_dir.filter(|s| !s.is_empty()) {
            let home_pf = PathFragment::create(h);
            potential_rc_files.push((home_pf.get_relative_str(".bazelrc"), false));
        }
    }

    // 4. Env var BAZELRC (split by comma, stopped by /dev/null)
    if let Ok(bazelrc_env) = std::env::var("BAZELRC") {
        if !bazelrc_env.is_empty() {
            for part in bazelrc_env.split(',') {
                if part == "/dev/null" {
                    break;
                }
                let full_pf = absolute_path_from_flag(part, cwd, home_dir);
                potential_rc_files.push((full_pf, true));
            }
        }
    }

    // 5. Command-line provided --bazelrc (can be repeated; `/dev/null` stops further `--bazelrc`s)
    for custom_rc in &startup.bazelrc_files {
        if custom_rc == "/dev/null" {
            break;
        }
        let full_pf = absolute_path_from_flag(custom_rc, cwd, home_dir);
        potential_rc_files.push((full_pf, true));
    }

    client_log(
        "src/main/cpp/option_processor.cc:437",
        format!(
            "Looking for the following rc files: {}",
            potential_rc_files
                .iter()
                .map(|(p, _)| p.path_string().to_string())
                .collect::<Vec<_>>()
                .join(",")
        ),
    );

    // Deduplicate top-level candidate queue by canonical path (`DedupeBlazercPaths`) and load.
    let mut top_level_seen = HashSet::new();
    for (rc_path, is_explicit) in potential_rc_files {
        if !fs.exists(&rc_path, true) {
            if is_explicit {
                return Err(OptionsParsingError::new(format!(
                    "Error: Unable to read .bazelrc file '{}'.",
                    rc_path.path_string()
                )));
            }
            continue;
        }

        let canonical_key = PathFragment::create(&canonicalize_rc_path(fs, &rc_path));
        if !top_level_seen.insert(canonical_key) {
            continue;
        }

        config.parse_file_with_cwd(&rc_path, workspace_dir, Some(cwd), fs, build_label)?;
    }

    // `OptionProcessor::WarnAboutDuplicateRcFiles`: warn if a canonical rc path was loaded multiple times
    // (for example, a diamond import or importing a standard rc file that was already discovered).
    let mut seen_canon = HashSet::new();
    let mut warned_canon = HashSet::new();
    for canon in &config.canonical_rc_paths {
        if !seen_canon.insert(canon.clone()) && warned_canon.insert(canon.clone()) {
            config.warnings.push(format!(
                "WARNING: Duplicate rc file: {}",
                canon.path_string()
            ));
        }
    }

    // Apply startup options from `startup` and `startup:<os>` at RcFile priority, keeping rc-tagged
    // startup options ahead of CLI-tagged startup options in `startup.raw_tagged_options`.
    let startup_sections = [
        "startup".to_string(),
        format!("startup:{}", crate::config_expander::get_platform_name()),
    ];
    let cli_tagged = std::mem::take(&mut startup.raw_tagged_options);
    let startup_map = get_startup_options_map();
    let mut priority = OptionPriority::lowest_at_category(PriorityCategory::RcFile);

    let finish_raw_tagged = |startup: &mut StartupOptions, cli: Vec<_>| {
        startup.raw_tagged_options.extend(cli);
    };

    for sec_name in &startup_sections {
        let Some(startup_opts) = config.sections.get(sec_name) else {
            continue;
        };
        let mut idx = 0;
        while idx < startup_opts.len() {
            let (flag, src) = &startup_opts[idx];
            if let Some((f, _)) = flag.split_once('=') {
                let naked = f.strip_prefix("--").unwrap_or(f);
                let is_nullary = |n: &str| startup_map.get(n).is_some_and(|d| !d.requires_value());
                let nullary_negated = naked.strip_prefix("no").is_some_and(|rest| {
                    startup_map
                        .get(rest)
                        .is_some_and(|d| d.uses_boolean_syntax())
                });
                if f.starts_with("--")
                    && naked != "block_for_lock"
                    && (is_nullary(naked) || nullary_negated)
                {
                    finish_raw_tagged(startup, cli_tagged);
                    return Err(OptionsParsingError::new(format!(
                        "FATAL: In argument '{flag}': option '{f}' does not take a value."
                    )));
                }
            }

            let (flag_name, inline_val) = parse_startup_flag_token(flag);
            let combined_buf;
            let effective_flag = if inline_val.is_none()
                && startup_map
                    .get(flag_name.as_str())
                    .is_some_and(|d| d.requires_value())
            {
                if idx + 1 < startup_opts.len() && startup_opts[idx + 1].1 == *src {
                    combined_buf = format!("{flag}={}", startup_opts[idx + 1].0);
                    idx += 1;
                    combined_buf.as_str()
                } else {
                    let prod = startup.lowercase_product_name();
                    finish_raw_tagged(startup, cli_tagged);
                    return Err(OptionsParsingError::new(format!(
                        "FATAL: {}",
                        OptionsParsingError::startup_option_missing_value(flag, &prod).message
                    )));
                }
            } else {
                flag.as_str()
            };

            if let Err(e) = startup.apply_flag_with_priority(effective_flag, src, priority.clone())
            {
                let prod = startup.lowercase_product_name();
                let msg = &e.message;
                finish_raw_tagged(startup, cli_tagged);
                if msg.starts_with("Invalid argument to")
                    || msg.starts_with("The startup flag --invocation_policy")
                {
                    return Err(OptionsParsingError::new(format!("FATAL: {msg}")));
                }
                return Err(OptionsParsingError::new(format!(
                    "FATAL: Unknown startup option: '{flag}'.\n  For more info, run '{prod} help startup_options'."
                )));
            }
            priority = priority.next();
            idx += 1;
        }
    }

    finish_raw_tagged(startup, cli_tagged);
    startup.absolutize_paths(cwd, home_dir);

    Ok(config)
}

/// Partitions `--rc_source` and `--default_override` arguments out of `cmd_line`, validates every
/// `--default_override` (both `--default_override=val` and `--default_override val`), records any
/// resulting command/config overrides in `rc_config`, and returns the remaining arguments
/// (`BlazeOptionHandler.partitionCommandLineArgs` + `parseRcOptions`).
pub fn partition_rc_sources_and_apply(
    rc_config: &mut BazelrcConfig,
    cmd_line: &[String],
) -> Result<Vec<String>, OptionsParsingError> {
    const OVERRIDE_MSG: &str =
        "option overrides must be in form rcfile:command=option, where rcfile is a nonzero integer";
    let mut remaining = Vec::with_capacity(cmd_line.len());
    let mut rc_sources: Vec<String> = Vec::new();
    let mut overrides: Vec<(usize, String, String)> = Vec::new();

    let parse_override = |clf: &str,
                          raw: &str|
     -> Result<(usize, String, String), OptionsParsingError> {
        let colon = raw.find(':');
        let assign = raw.find('=');
        let Some(colon) = colon else {
            return Err(OptionsParsingError::new(format!(
                "While parsing option {clf}: {OVERRIDE_MSG}"
            )));
        };
        let Some(assign) = assign.filter(|a| *a > colon + 1) else {
            return Err(OptionsParsingError::new(format!(
                "While parsing option {clf}: {OVERRIDE_MSG}"
            )));
        };
        let Some(idx) = crate::converters::java_integer_value_of(&raw[..colon]).filter(|n| *n >= 0)
        else {
            return Err(OptionsParsingError::new(format!(
                "While parsing option {clf}: {OVERRIDE_MSG}"
            )));
        };
        Ok((
            idx as usize,
            raw[colon + 1..assign].to_string(),
            raw[assign + 1..].to_string(),
        ))
    };

    let mut i = 0;
    while i < cmd_line.len() {
        let arg = &cmd_line[i];
        if let Some(src) = arg.strip_prefix("--rc_source=") {
            rc_sources.push(src.to_string());
            i += 1;
        } else if arg == "--rc_source" {
            if i + 1 >= cmd_line.len() {
                return Err(OptionsParsingError::new("Expected value after --rc_source"));
            }
            rc_sources.push(cmd_line[i + 1].clone());
            i += 2;
        } else if let Some(val) = arg.strip_prefix("--default_override=") {
            overrides.push(parse_override(arg, val)?);
            i += 1;
        } else if arg == "--default_override" {
            if i + 1 >= cmd_line.len() {
                return Err(OptionsParsingError::new(
                    "Expected value after --default_override",
                ));
            }
            let val = &cmd_line[i + 1];
            let clf = format!("--default_override {val}");
            overrides.push(parse_override(&clf, val)?);
            i += 2;
        } else {
            remaining.push(arg.clone());
            i += 1;
        }
    }

    for (rc_idx, section, opt) in overrides {
        let src = if rc_sources.is_empty() && rc_idx == 0 {
            "client".to_string()
        } else if let Some(s) = rc_sources.get(rc_idx) {
            s.clone()
        } else {
            return Err(OptionsParsingError::new(format!(
                "Consistency error in options: --default_override index {rc_idx} out of bounds"
            )));
        };
        if !opt.is_empty() {
            if (opt == "--flag_alias" || opt.starts_with("--flag_alias="))
                && !matches!(section.as_str(), "build" | "common" | "always")
            {
                return Err(OptionsParsingError::new(format!(
                    "{src}: \"{section} {opt}\" disallowed. --flag_alias only supports these commands: build, common, always"
                )));
            }
            rc_config
                .sections
                .entry(section)
                .or_default()
                .push((opt, src));
        }
    }

    Ok(remaining)
}

/// Tokenizes an RC file line respecting quotes, backslashes (including inside single quotes), and `#` comments.
/// Mirrors `blaze_util::SplitQuotedStringUsing` as invoked by `rc_file.cc`.
pub fn tokenize_rc_line(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut has_token = false;
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '#' && !in_single && !in_double {
            break;
        }
        if c == '\\' {
            if let Some(next_c) = chars.next() {
                has_token = true;
                current.push(next_c);
            }
            continue;
        }
        if c == '\'' && !in_double {
            has_token = true;
            in_single = !in_single;
            continue;
        }
        if c == '"' && !in_single {
            has_token = true;
            in_double = !in_double;
            continue;
        }
        if matches!(c, ' ' | '\t' | '\n' | '\r') && !in_single && !in_double {
            if has_token {
                tokens.push(std::mem::take(&mut current));
                has_token = false;
            }
            continue;
        }
        has_token = true;
        current.push(c);
    }
    if has_token {
        tokens.push(current);
    }
    tokens
}

/// Returns the ordered list of rc pseudo-commands/commands an invocation inherits from
/// (`BlazeOptionHandler.getCommandNamesToParse`): `always`, `common`, then — depth first over
/// `@Command(inheritsOptionsFrom)` — the bases followed by the command itself.
/// E.g. for `test`: `["always", "common", "build", "test"]`.
pub fn get_command_ancestry(command: &str) -> Vec<&'static str> {
    fn visit(command: &str, acc: &mut Vec<&'static str>) {
        let Some((name, inherits, _, _)) = COMMAND_INHERITANCE.iter().find(|c| c.0 == command)
        else {
            return;
        };
        for base in inherits.iter() {
            visit(base, acc);
        }
        acc.push(name);
    }
    let mut res = vec!["always", "common"];
    visit(command, &mut res);
    res
}

/// Semantic version matching Bazel C++ `SemVer` (`src/main/cpp/sem_ver.h`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemVer {
    pub major: i32,
    pub minor: i32,
    pub patch: i32,
    pub prerelease: Vec<String>,
}

impl SemVer {
    pub fn new(major: i32, minor: i32, patch: i32) -> Self {
        Self {
            major,
            minor,
            patch,
            prerelease: Vec::new(),
        }
    }

    fn parse_component(part: &str) -> Option<i32> {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if part.len() > 1 && part.starts_with('0') {
            return None;
        }
        part.parse::<i32>().ok().filter(|&v| v >= 0)
    }

    fn valid_ident_chars(id: &str) -> bool {
        !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    }

    fn parse_prerelease_and_build(rest: &str) -> Option<Vec<String>> {
        let before_build = match rest.split_once('+') {
            Some((pre, build)) => {
                if build.is_empty() || !build.split('.').all(Self::valid_ident_chars) {
                    return None;
                }
                pre
            }
            None => rest,
        };
        if before_build.is_empty() {
            return Some(Vec::new());
        }
        let pre = before_build.strip_prefix('-')?;
        if pre.is_empty() {
            return None;
        }
        let mut idents = Vec::new();
        for id in pre.split('.') {
            if !Self::valid_ident_chars(id) {
                return None;
            }
            if id.len() > 1 && id.starts_with('0') && id.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            idents.push(id.to_string());
        }
        Some(idents)
    }

    /// Parses a full `major.minor.patch[-prerelease][+build]` semantic version.
    pub fn parse(s: &str) -> Option<Self> {
        if s.is_empty() {
            return None;
        }
        let split_pos = s.find(['-', '+']).unwrap_or(s.len());
        let (core, rest) = s.split_at(split_pos);
        let parts: Vec<&str> = core.split('.').collect();
        if parts.len() != 3 {
            return None;
        }
        let major = Self::parse_component(parts[0])?;
        let minor = Self::parse_component(parts[1])?;
        let patch = Self::parse_component(parts[2])?;
        let prerelease = Self::parse_prerelease_and_build(rest)?;
        Some(Self {
            major,
            minor,
            patch,
            prerelease,
        })
    }

    /// Parses a tilde range version (`M`, `M.N`, or `M.N.P`, with optional prerelease/build),
    /// returning `(inclusive_lower_bound, exclusive_upper_bound)`.
    pub fn parse_tilde(s: &str) -> Option<(Self, Self)> {
        if s.is_empty() {
            return None;
        }
        let split_pos = s.find(['-', '+']).unwrap_or(s.len());
        let (core, rest) = s.split_at(split_pos);
        let parts: Vec<&str> = core.split('.').collect();
        if parts.is_empty() || parts.len() > 3 {
            return None;
        }
        let major = Self::parse_component(parts[0])?;
        let minor = if parts.len() >= 2 {
            Self::parse_component(parts[1])?
        } else {
            0
        };
        let patch = if parts.len() == 3 {
            Self::parse_component(parts[2])?
        } else {
            0
        };
        let prerelease = Self::parse_prerelease_and_build(rest)?;
        let lower = Self {
            major,
            minor,
            patch,
            prerelease,
        };
        let upper = if parts.len() == 1 {
            lower.next_major_version()
        } else {
            lower.next_minor_version()
        };
        Some((lower, upper))
    }

    pub fn next_major_version(&self) -> Self {
        Self {
            major: self.major.saturating_add(1),
            minor: 0,
            patch: 0,
            prerelease: Vec::new(),
        }
    }

    pub fn next_minor_version(&self) -> Self {
        Self {
            major: self.major,
            minor: self.minor.saturating_add(1),
            patch: 0,
            prerelease: Vec::new(),
        }
    }
}

impl PartialOrd for SemVer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SemVer {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch)) {
            Ordering::Equal => {}
            non_eq => return non_eq,
        }
        match (self.prerelease.is_empty(), other.prerelease.is_empty()) {
            (true, true) => Ordering::Equal,
            (false, true) => Ordering::Less,
            (true, false) => Ordering::Greater,
            (false, false) => {
                for (a, b) in self.prerelease.iter().zip(other.prerelease.iter()) {
                    let a_num = a.bytes().all(|c| c.is_ascii_digit());
                    let b_num = b.bytes().all(|c| c.is_ascii_digit());
                    let ord = match (a_num, b_num) {
                        (true, true) => a.len().cmp(&b.len()).then_with(|| a.cmp(b)),
                        (true, false) => Ordering::Less,
                        (false, true) => Ordering::Greater,
                        (false, false) => a.cmp(b),
                    };
                    if ord != Ordering::Equal {
                        return ord;
                    }
                }
                self.prerelease.len().cmp(&other.prerelease.len())
            }
        }
    }
}

/// Evaluates a Bazel version condition (`<`, `<=`, `>`, `>=`, `==`, `!=`, `~`) matching `rc_file.cc`.
pub fn eval_bazel_version_condition(
    condition: &str,
    current_version: &str,
) -> Result<bool, String> {
    let op_len = if condition.starts_with("<=")
        || condition.starts_with(">=")
        || condition.starts_with("==")
        || condition.starts_with("!=")
    {
        2
    } else if condition.starts_with('<') || condition.starts_with('>') || condition.starts_with('~')
    {
        1
    } else {
        return Err(format!(
            "Condition '{condition}'. A valid condition is one of the comparison operators ('<', '<=', '>', '>=', '==', '!=', '~') followed by a semantic version."
        ));
    };

    let op = &condition[..op_len];
    let ver_str = &condition[op_len..];

    if op == "~" {
        let (lower, upper) = SemVer::parse_tilde(ver_str)
            .ok_or_else(|| format!("Could not parse the tilde range version '{ver_str}'"))?;
        let Some(curr_ver) = SemVer::parse(current_version) else {
            return Ok(false);
        };
        return Ok(curr_ver >= lower && curr_ver < upper);
    }

    let target_ver = SemVer::parse(ver_str).ok_or_else(|| {
        format!("Could not parse version '{ver_str}' as a valid semantic version")
    })?;

    // If current_version is unparseable (e.g. development "no_version"), skip conditional import.
    let Some(curr_ver) = SemVer::parse(current_version) else {
        return Ok(false);
    };

    let ord = curr_ver.cmp(&target_ver);
    match op {
        "<" => Ok(ord == Ordering::Less),
        "<=" => Ok(ord != Ordering::Greater),
        ">" => Ok(ord == Ordering::Greater),
        ">=" => Ok(ord != Ordering::Less),
        "==" => Ok(ord == Ordering::Equal),
        "!=" => Ok(ord != Ordering::Equal),
        _ => Err(format!("Unsupported operator '{op}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_vfs::InMemoryFileSystem;

    #[test]
    fn test_rc_tokenize_and_single_quote_backslash() {
        let line = "build --copt=\"-O3 -Wall\" --jobs 16 # this is a comment";
        let tokens = tokenize_rc_line(line);
        assert_eq!(tokens, vec!["build", "--copt=-O3 -Wall", "--jobs", "16"]);

        // `rc_file.cc`: backslash escapes even inside single quotes (`'foo\'bar'` -> `foo'bar`).
        let single_escaped = tokenize_rc_line("build '--copt=foo\\'bar' ''");
        assert_eq!(single_escaped, vec!["build", "--copt=foo'bar", ""]);
    }

    #[test]
    fn test_rc_relative_import_resolves_against_cwd_not_importing_dir() {
        let fs = InMemoryFileSystem::default();
        let ws = PathFragment::create("/workspace");
        let cwd = PathFragment::create("/workspace/cwd_dir");
        let sub = ws.get_relative_str("sub");
        fs.create_directory_and_parents(&cwd).unwrap();
        fs.create_directory_and_parents(&sub).unwrap();

        fs.write_file(&cwd.get_relative_str("base.bazelrc"), b"build -c opt\n")
            .unwrap();
        fs.write_file(&sub.get_relative_str("base.bazelrc"), b"build -c dbg\n")
            .unwrap();
        fs.write_file(
            &sub.get_relative_str("nested.bazelrc"),
            b"import base.bazelrc\nbuild --jobs=32\n",
        )
        .unwrap();

        let mut config = BazelrcConfig::default();
        config
            .parse_file_with_cwd(
                &sub.get_relative_str("nested.bazelrc"),
                Some(&ws),
                Some(&cwd),
                &fs,
                "7.0.0",
            )
            .unwrap();

        let opts = config.get_inherited_command_options("build");
        let flags: Vec<String> = opts.into_iter().map(|(o, _)| o).collect();
        assert_eq!(flags, vec!["-c", "opt", "--jobs=32"]);
    }

    #[test]
    fn test_rc_diamond_imports_apply_twice_and_warn_duplicate() {
        let fs = InMemoryFileSystem::default();
        let ws = PathFragment::create("/workspace");
        fs.create_directory_and_parents(&ws).unwrap();

        fs.write_file(
            &ws.get_relative_str(".bazelrc"),
            b"import %workspace%/a.rc\nimport %workspace%/b.rc\n",
        )
        .unwrap();
        fs.write_file(
            &ws.get_relative_str("a.rc"),
            b"import %workspace%/shared.rc\nbuild --copt=-DA\n",
        )
        .unwrap();
        fs.write_file(
            &ws.get_relative_str("b.rc"),
            b"import %workspace%/shared.rc\nbuild --copt=-DB\n",
        )
        .unwrap();
        fs.write_file(
            &ws.get_relative_str("shared.rc"),
            b"build --copt=-DSHARED\n",
        )
        .unwrap();

        let mut startup = StartupOptions {
            system_rc: false,
            home_rc: false,
            ..Default::default()
        };
        let cfg =
            discover_and_load_rc_files(&mut startup, Some(&ws), None, &ws, &fs, "9.3.2").unwrap();
        let flags: Vec<String> = cfg
            .get_inherited_command_options("build")
            .into_iter()
            .map(|(o, _)| o)
            .collect();
        assert_eq!(
            flags,
            vec![
                "--copt=-DSHARED",
                "--copt=-DA",
                "--copt=-DSHARED",
                "--copt=-DB"
            ]
        );
        assert!(
            cfg.warnings
                .iter()
                .any(|w| w.contains("Duplicate rc file") && w.contains("shared.rc")),
            "{:?}",
            cfg.warnings
        );
    }

    #[test]
    fn test_rc_try_import_only_ignores_unreadable_direct_target() {
        let fs = InMemoryFileSystem::default();
        let ws = PathFragment::create("/workspace");
        fs.create_directory_and_parents(&ws).unwrap();

        // Readable try-import with malformed import inside must fail.
        fs.write_file(&ws.get_relative_str("bad_syntax.rc"), b"import a b c\n")
            .unwrap();
        fs.write_file(
            &ws.get_relative_str("root1.rc"),
            b"try-import %workspace%/bad_syntax.rc\n",
        )
        .unwrap();
        let mut cfg1 = BazelrcConfig::default();
        let err1 = cfg1
            .parse_file(&ws.get_relative_str("root1.rc"), Some(&ws), &fs, "9.3.2")
            .unwrap_err();
        assert!(
            err1.message.contains("Invalid import declaration"),
            "{}",
            err1.message
        );

        // Readable try-import containing a mandatory `import` of a missing plain path must ALSO fail.
        fs.write_file(
            &ws.get_relative_str("nested_missing.rc"),
            b"import /nonexistent/mandatory.rc\n",
        )
        .unwrap();
        fs.write_file(
            &ws.get_relative_str("root2.rc"),
            b"try-import %workspace%/nested_missing.rc\n",
        )
        .unwrap();
        let mut cfg2 = BazelrcConfig::default();
        let err2 = cfg2
            .parse_file(&ws.get_relative_str("root2.rc"), Some(&ws), &fs, "9.3.2")
            .unwrap_err();
        assert!(
            err2.message
                .contains("Unexpected error reading config file"),
            "{}",
            err2.message
        );
    }

    #[test]
    fn test_rc_arbitrary_command_names_accepted_without_warning() {
        let fs = InMemoryFileSystem::default();
        let ws = PathFragment::create("/workspace");
        fs.create_directory_and_parents(&ws).unwrap();
        fs.write_file(
            &ws.get_relative_str(".bazelrc"),
            b"custom_starlark_cmd --foo=bar\nbuild --jobs=4\n",
        )
        .unwrap();

        let mut cfg = BazelrcConfig::default();
        cfg.parse_file(&ws.get_relative_str(".bazelrc"), Some(&ws), &fs, "9.3.2")
            .unwrap();
        assert!(cfg.sections.contains_key("custom_starlark_cmd"));
    }

    #[test]
    fn test_semver_and_tilde_conditions() {
        // SemVer parsing and pre-release/build metadata rules
        assert!(SemVer::parse("7.0.0+build.1").is_some());
        assert!(SemVer::parse("01.0.0").is_none());
        assert!(SemVer::parse("v7.0.0").is_none());
        assert!(SemVer::parse("7.0").is_none());
        assert_eq!(
            SemVer::parse("7.0.0+20260101").unwrap(),
            SemVer::parse("7.0.0+other").unwrap()
        );
        assert!(SemVer::parse("8.0.0-alpha.2").unwrap() < SemVer::parse("8.0.0-alpha.10").unwrap());
        assert!(SemVer::parse("8.0.0-rc.1").unwrap() < SemVer::parse("8.0.0").unwrap());

        // Tilde: `~8` is [8.0.0, 9.0.0); `~8.1` and `~8.1.4` are bounded by next minor 8.2.0
        assert!(eval_bazel_version_condition("~8", "8.9.2").unwrap());
        assert!(!eval_bazel_version_condition("~8", "9.0.0").unwrap());
        assert!(eval_bazel_version_condition("~8.1", "8.1.9").unwrap());
        assert!(!eval_bazel_version_condition("~8.1", "8.2.0").unwrap());
        assert!(eval_bazel_version_condition("~8.1.4", "8.1.5").unwrap());
        assert!(!eval_bazel_version_condition("~8.1.4", "8.2.0").unwrap());
        assert!(!eval_bazel_version_condition("~8.1.4", "8.1.3").unwrap());

        // Unparseable running build_label skips the directive (returns false)
        assert!(!eval_bazel_version_condition(">=7.0.0", "no_version").unwrap());
        assert!(!eval_bazel_version_condition("~9.3", "").unwrap());
    }

    #[test]
    fn test_rc_import_loop() {
        let fs = InMemoryFileSystem::default();
        let ws = PathFragment::create("/workspace");
        fs.create_directory_and_parents(&ws).unwrap();

        fs.write_file(
            &ws.get_relative_str("a.bazelrc"),
            b"import %workspace%/b.bazelrc\n",
        )
        .unwrap();

        fs.write_file(
            &ws.get_relative_str("b.bazelrc"),
            b"import %workspace%/a.bazelrc\n",
        )
        .unwrap();

        let mut config = BazelrcConfig::default();
        let err = config
            .parse_file(&ws.get_relative_str("a.bazelrc"), Some(&ws), &fs, "7.0.0")
            .unwrap_err();

        assert!(err.message.contains("Import loop detected:"));
    }

    #[test]
    fn test_rc_disallow_bazelrc_flag() {
        let fs = InMemoryFileSystem::default();
        let ws = PathFragment::create("/workspace");
        fs.create_directory_and_parents(&ws).unwrap();

        fs.write_file(
            &ws.get_relative_str(".bazelrc"),
            b"startup --bazelrc=/dev/null\n",
        )
        .unwrap();

        let mut config = BazelrcConfig::default();
        let err = config
            .parse_file(&ws.get_relative_str(".bazelrc"), Some(&ws), &fs, "7.0.0")
            .unwrap_err();

        assert_eq!(err.message, "Can't specify --bazelrc in the RC file.");
    }

    #[test]
    fn test_rc_disallow_rc_control_flags() {
        let fs = InMemoryFileSystem::default();
        let ws = PathFragment::create("/workspace");
        fs.create_directory_and_parents(&ws).unwrap();
        for flag in [
            "--system_rc",
            "--nosystem_rc",
            "--workspace_rc",
            "--home_rc",
            "--nohome_rc",
            "--ignore_all_rc_files",
            "--noignore_all_rc_files",
        ] {
            fs.write_file(
                &ws.get_relative_str(".bazelrc"),
                format!("startup {flag}\n").as_bytes(),
            )
            .unwrap();
            let mut config = BazelrcConfig::default();
            let err = config
                .parse_file(&ws.get_relative_str(".bazelrc"), Some(&ws), &fs, "7.0.0")
                .unwrap_err();
            assert!(
                err.message.contains("Can't specify"),
                "{flag}: {}",
                err.message
            );
        }
    }

    #[test]
    fn test_rc_dev_null_stops_later_bazelrc_and_tilde_expands_home() {
        let fs = InMemoryFileSystem::default();
        let ws = PathFragment::create("/workspace");
        let home = PathFragment::create("/home/alice");
        fs.create_directory_and_parents(&ws).unwrap();
        fs.create_directory_and_parents(&home).unwrap();
        fs.write_file(&home.get_relative_str("my.rc"), b"build --jobs=7\n")
            .unwrap();
        fs.write_file(&ws.get_relative_str("later.rc"), b"build --jobs=99\n")
            .unwrap();

        let mut startup = StartupOptions {
            system_rc: false,
            workspace_rc: false,
            home_rc: false,
            bazelrc_files: vec![
                "~/my.rc".to_string(),
                "/dev/null".to_string(),
                "/workspace/later.rc".to_string(),
            ],
            ..Default::default()
        };

        let cfg = discover_and_load_rc_files(
            &mut startup,
            Some(&ws),
            Some("/home/alice"),
            &ws,
            &fs,
            "7.0.0",
        )
        .unwrap();
        let opts = cfg.get_inherited_command_options("build");
        let flags: Vec<String> = opts.into_iter().map(|(o, _)| o).collect();
        assert_eq!(flags, vec!["--jobs=7"]);
    }

    #[test]
    fn test_rc_windows_path_escaping_in_imports() {
        // Per `blaze_util::Tokenize` (`src/main/cpp/util/strings.cc`), `\` is an escape character
        // in `.bazelrc` on all platforms: Windows drive paths must use `/` or escaped `\\`.
        assert_eq!(
            tokenize_rc_line(r"import C:/Users/runner/ws/missing.rc"),
            vec!["import", "C:/Users/runner/ws/missing.rc"]
        );
        assert_eq!(
            tokenize_rc_line(r"import C:\\Users\\runner\\ws\\missing.rc"),
            vec!["import", r"C:\Users\runner\ws\missing.rc"]
        );
        assert_eq!(
            tokenize_rc_line(r"import C:\Users\runner\ws\missing.rc"),
            vec!["import", "C:Usersrunnerwsmissing.rc"]
        );

        let fs = InMemoryFileSystem::default();
        let ws = PathFragment::create("C:/ws");
        let configs = ws.get_relative_str("configs");
        fs.create_directory_and_parents(&configs).unwrap();
        fs.write_file(
            &configs.get_relative_str("slash.bazelrc"),
            b"build --jobs=4\n",
        )
        .unwrap();
        fs.write_file(
            &configs.get_relative_str("escaped.bazelrc"),
            b"build --jobs=8\n",
        )
        .unwrap();
        fs.write_file(
            &ws.get_relative_str(".bazelrc"),
            b"import C:/ws/configs/slash.bazelrc\nimport C:\\\\ws\\\\configs\\\\escaped.bazelrc\n",
        )
        .unwrap();

        let mut config = BazelrcConfig::default();
        config
            .parse_file(&ws.get_relative_str(".bazelrc"), Some(&ws), &fs, "9.3.2")
            .unwrap();
        let flags: Vec<String> = config
            .get_inherited_command_options("build")
            .into_iter()
            .map(|(o, _)| o)
            .collect();
        assert_eq!(flags, vec!["--jobs=4", "--jobs=8"]);
    }
}
