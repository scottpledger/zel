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

//! `.bazelrc` file discovery, tokenizer, import resolution, and inheritance.
//! Mirrors `src/main/cpp/rc_file.cc` and `src/main/cpp/option_processor.cc`.

use crate::error::OptionsParsingError;
use crate::flag_tables::COMMAND_INHERITANCE;
use crate::priority::{OptionPriority, PriorityCategory};
use crate::startup::StartupOptions;
use std::collections::{BTreeMap, HashSet};
use zel_util::Os;
use zel_vfs::{FileSystem, PathFragment};

const MAX_IMPORT_DEPTH: usize = 100;

/// Buffered `BAZEL_LOG(INFO)` lines of the C++ client (`file:line`, message); the launcher
/// flushes them in front of a fatal error (`BAZEL_DIE`), so they are collected here.
static CLIENT_LOG: std::sync::Mutex<Vec<(&'static str, String)>> = std::sync::Mutex::new(Vec::new());

fn client_log(location: &'static str, msg: String) {
    CLIENT_LOG.lock().unwrap_or_else(|e| e.into_inner()).push((location, msg));
}

/// Drains the buffered client INFO log (`("src/main/cpp/rc_file.cc:94", "Parsing the RcFile X")`, ...).
pub fn take_client_log() -> Vec<(&'static str, String)> {
    std::mem::take(&mut *CLIENT_LOG.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Formats an I/O error like the C++ client does (`(error: 2): No such file or directory`).
fn errno_text(e: &dyn std::fmt::Display) -> String {
    let s = e.to_string();
    match s.rsplit_once(" (os error ") {
        Some((msg, rest)) => format!("(error: {}): {msg}", rest.trim_end_matches(')')),
        None => s,
    }
}

/// Parsed `.bazelrc` configuration containing command and config entries.
#[derive(Debug, Clone, Default)]
pub struct BazelrcConfig {
    /// Maps section key (e.g. `"startup"`, `"common"`, `"build"`, `"build:opt"`) to list of `(option, source_path)`.
    pub sections: BTreeMap<String, Vec<(String, String)>>,
    /// Canonical paths of all loaded rc files in order.
    pub loaded_rc_files: Vec<PathFragment>,
}

impl BazelrcConfig {
    /// Expands command options for a given `command` (including `always`, `common`, `<command>`, and `--config=<name>`).
    pub fn expand_command_args(
        &self,
        command: &str,
        cli_args: &[String],
    ) -> Result<Vec<String>, OptionsParsingError> {
        let (expanded, _warnings) = crate::config_expander::expand_command_args(self, command, cli_args)?;
        Ok(expanded)
    }

    /// Expands command options returning both expanded arguments and Bazel-matching rc info diagnostics.
    pub fn expand_command_args_with_diagnostics(
        &self,
        command: &str,
        cli_args: &[String],
    ) -> Result<(Vec<String>, Vec<String>), OptionsParsingError> {
        let (expanded, warnings) = crate::config_expander::expand_command_args(self, command, cli_args)?;
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
                            let ignored = crate::parser::split_unsupported_common_args(command, &args).1;
                            if !ignored.is_empty() {
                                note.push_str(&format!("\n  Ignored as unsupported by '{command}': {}", ignored.join(" ")));
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
        let pf = PathFragment::create(source_path);
        let mut import_stack = vec![pf.clone()];
        visited.insert(pf);
        self.parse_str_internal(
            content,
            source_path,
            Some(workspace_dir),
            fs,
            "7.0.0",
            &mut import_stack,
            visited,
            0,
        )
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
        let pf = PathFragment::create(source_path);
        let mut import_stack = vec![pf.clone()];
        let mut visited = HashSet::new();
        visited.insert(pf);
        self.parse_str_internal(
            content,
            source_path,
            workspace_dir,
            fs,
            build_label,
            &mut import_stack,
            &mut visited,
            0,
        )
    }

    /// Reads and parses an RC file from filesystem.
    pub fn parse_file(
        &mut self,
        rc_path: &PathFragment,
        workspace_dir: Option<&PathFragment>,
        fs: &dyn FileSystem,
        build_label: &str,
    ) -> Result<(), OptionsParsingError> {
        let mut import_stack = vec![rc_path.clone()];
        let mut visited = HashSet::new();
        visited.insert(rc_path.clone());
        self.parse_file_internal(
            rc_path,
            workspace_dir,
            fs,
            build_label,
            &mut import_stack,
            &mut visited,
            0,
        )
    }

    fn parse_file_internal(
        &mut self,
        rc_path: &PathFragment,
        workspace_dir: Option<&PathFragment>,
        fs: &dyn FileSystem,
        build_label: &str,
        import_stack: &mut Vec<PathFragment>,
        visited: &mut HashSet<PathFragment>,
        depth: usize,
    ) -> Result<(), OptionsParsingError> {
        client_log("src/main/cpp/rc_file.cc:94", format!("Parsing the RcFile {rc_path}"));
        let bytes = fs.read_file(rc_path).map_err(|e| {
            OptionsParsingError::new(format!(
                "Unexpected error reading config file '{rc_path}': {}",
                errno_text(&e)
            ))
        })?;
        let content = String::from_utf8_lossy(&bytes);
        if !self.loaded_rc_files.contains(rc_path) {
            self.loaded_rc_files.push(rc_path.clone());
        }
        // `rc_file.cc`: option sources (shown by `--announce_rc`) are the *canonical* file names
        // (`canonicalize_path(filename)`); error texts keep the name as given.
        let canonical = std::fs::canonicalize(rc_path.path_string())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| rc_path.path_string().to_string());
        self.parse_str_internal(
            &content,
            &canonical,
            workspace_dir,
            fs,
            build_label,
            import_stack,
            visited,
            depth,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn parse_str_internal(
        &mut self,
        content: &str,
        source_path: &str,
        workspace_dir: Option<&PathFragment>,
        fs: &dyn FileSystem,
        build_label: &str,
        import_stack: &mut Vec<PathFragment>,
        visited: &mut HashSet<PathFragment>,
        depth: usize,
    ) -> Result<(), OptionsParsingError> {
        if depth > MAX_IMPORT_DEPTH {
            return Err(OptionsParsingError::new(format!(
                "Maximum import depth exceeded parsing config file '{source_path}'"
            )));
        }

        let source_pf = PathFragment::create(source_path);
        let source_dir = source_pf.parent_directory();

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
            if command == "import" || command == "try-import" || command == "try-import-if-bazel-version" {
                let is_try = command.starts_with("try-import");
                let is_version_conditioned = command == "try-import-if-bazel-version";

                if (is_version_conditioned && tokens.len() != 3)
                    || (!is_version_conditioned && tokens.len() != 2)
                {
                    return Err(OptionsParsingError::new(format!(
                        "Invalid import declaration in config file '{source_path}': '{full_line}'"
                    )));
                }

                if is_version_conditioned {
                    let condition = &tokens[1];
                    let matches = match eval_bazel_version_condition(condition, build_label) {
                        Ok(m) => m,
                        Err(err_msg) => {
                            return Err(OptionsParsingError::new(format!(
                                "Invalid version condition in config file '{source_path}': '{full_line}'. {err_msg}"
                            )));
                        }
                    };
                    if !matches {
                        // Condition did not match; skip import
                        continue;
                    }
                }

                let import_target_str = if is_version_conditioned {
                    &tokens[2]
                } else {
                    &tokens[1]
                };

                let resolved_path = if import_target_str.starts_with("%workspace%") {
                    let Some(ws) = workspace_dir else {
                        if !is_try {
                            return Err(OptionsParsingError::new(format!(
                                "Nonexistent path in import declaration in config file '{source_path}': '{full_line}'. This is because no workspace was found and %workspace%-relative imports are only supported when running from within a workspace."
                            )));
                        }
                        continue;
                    };
                    let rel = import_target_str.trim_start_matches("%workspace%").trim_start_matches('/');
                    ws.get_relative_str(rel)
                } else {
                    let target_pf = PathFragment::create(import_target_str);
                    if target_pf.is_absolute() {
                        target_pf
                    } else if let Some(ref dir) = source_dir {
                        dir.get_relative(&target_pf)
                    } else if let Some(ws) = workspace_dir {
                        ws.get_relative(&target_pf)
                    } else {
                        target_pf
                    }
                };

                // Check for import cycle
                if import_stack.contains(&resolved_path) {
                    let mut loop_str = String::from("Import loop detected:\n");
                    for p in import_stack.iter() {
                        loop_str.push_str(&format!("  {}\n", p.path_string()));
                    }
                    loop_str.push_str(&format!("  {}\n", resolved_path.path_string()));
                    return Err(OptionsParsingError::new(loop_str));
                }

                if visited.contains(&resolved_path) {
                    continue;
                }

                if !fs.exists(&resolved_path, true) {
                    if is_try {
                        // rc_file.cc: a `%workspace%` import that cannot be resolved is skipped
                        // before reading (line 205); a plain path is read first (line 94) and the
                        // unreadable file skipped afterwards (line 234).
                        let loc = if import_target_str.starts_with("%workspace%") {
                            "src/main/cpp/rc_file.cc:205"
                        } else {
                            client_log("src/main/cpp/rc_file.cc:94", format!("Parsing the RcFile {import_target_str}"));
                            "src/main/cpp/rc_file.cc:234"
                        };
                        client_log(
                            loc,
                            format!(
                                "Skipped optional import of {import_target_str}, the specified rc file either does not exist or is not readable."
                            ),
                        );
                        continue;
                    }
                    if import_target_str.starts_with("%workspace%") {
                        return Err(OptionsParsingError::new(format!(
                            "Nonexistent path in import declaration in config file '{source_path}': '{full_line}' (are you in your source checkout/WORKSPACE?)"
                        )));
                    }
                    // A plain path is only found missing when it is read (RcFile::ParseFile).
                    client_log("src/main/cpp/rc_file.cc:94", format!("Parsing the RcFile {resolved_path}"));
                    return Err(OptionsParsingError::new(format!(
                        "Unexpected error reading config file '{resolved_path}': (error: 2): No such file or directory"
                    )));
                }

                import_stack.push(resolved_path.clone());
                visited.insert(resolved_path.clone());

                let sub_res = self.parse_file_internal(
                    &resolved_path,
                    workspace_dir,
                    fs,
                    build_label,
                    import_stack,
                    visited,
                    depth + 1,
                );

                import_stack.pop();

                if let Err(e) = sub_res {
                    if is_try && e.message.contains("Unexpected error reading config file") {
                        continue;
                    }
                    return Err(e);
                }
                continue;
            }

const VALID_COMMANDS: &[&str] = &[
    "always",
    "common",
    "startup",
    "build",
    "test",
    "run",
    "clean",
    "info",
    "query",
    "cquery",
    "aquery",
    "coverage",
    "version",
    "help",
    "shutdown",
    "canonicalize-flags",
    "config",
    "dump",
    "print_action",
    "mobile-install",
    "license",
    "mod",
    "fetch",
    "vendor",
    "sync",
];

            // Normal section line: command [option ...]
            let cmd_base = match command.split_once(':') {
                Some((base, _)) => base,
                None => command.as_str(),
            };
            if !VALID_COMMANDS.contains(&cmd_base) {
                eprintln!(
                    "WARNING: while reading option defaults file '{source_path}':\n  invalid command name '{command}'."
                );
                continue;
            }

            // Check that --bazelrc is NOT specified in RC file!
            for tok in &tokens[1..] {
                if tok == "--bazelrc" || tok.starts_with("--bazelrc=") {
                    return Err(OptionsParsingError::new(
                        "Can't specify --bazelrc in the RC file.",
                    ));
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

/// Discovers and loads rc files in full parity with Bazel's discovery order:
/// 1. `/etc/bazel.bazelrc` (system rc)
/// 2. `%workspace%/.bazelrc` and `.zelrc` (workspace rc)
/// 3. `$HOME/.bazelrc` and `.zelrc` (home rc)
/// 4. `$BAZELRC` env var
/// 5. `--bazelrc` CLI flags
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
        return Ok(config);
    }

    let mut potential_rc_files: Vec<(PathFragment, bool)> = Vec::new(); // (path, is_explicit)

    // 1. System rc (unless --nosystem_rc)
    if startup.system_rc {
        let sys_rc = match Os::current() {
            Os::Windows => PathFragment::create("C:/ProgramData/bazel.bazelrc"),
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
        if let Some(h) = home_dir {
            let home_pf = PathFragment::create(h);
            potential_rc_files.push((home_pf.get_relative_str(".bazelrc"), false));
        }
    }

    // 4. Env var BAZELRC (split by comma)
    if let Ok(bazelrc_env) = std::env::var("BAZELRC") {
        for part in bazelrc_env.split(',') {
            let trimmed = part.trim();
            if !trimmed.is_empty() {
                let pf = PathFragment::create(trimmed);
                let full_pf = if pf.is_absolute() {
                    pf
                } else {
                    cwd.get_relative(&pf)
                };
                potential_rc_files.push((full_pf, true));
            }
        }
    }

    // 5. Command-line provided --bazelrc (can be repeated)
    for custom_rc in &startup.bazelrc_files {
        if custom_rc == "/dev/null" {
            continue;
        }
        let pf = PathFragment::create(custom_rc);
        let full_pf = if pf.is_absolute() {
            pf
        } else {
            cwd.get_relative(&pf)
        };
        potential_rc_files.push((full_pf, true));
    }

    client_log(
        "src/main/cpp/option_processor.cc:437",
        format!(
            "Looking for the following rc files: {}",
            potential_rc_files.iter().map(|(p, _)| p.path_string().to_string()).collect::<Vec<_>>().join(",")
        ),
    );

    // Deduplicate and load
    let mut visited = HashSet::new();
    for (rc_path, is_explicit) in potential_rc_files {
        if visited.contains(&rc_path) {
            continue;
        }
        visited.insert(rc_path.clone());

        if !fs.exists(&rc_path, true) {
            if is_explicit {
                return Err(OptionsParsingError::new(format!(
                    "Unable to read .bazelrc file '{}'.",
                    rc_path.path_string()
                )));
            }
            continue;
        }

        config.parse_file(&rc_path, workspace_dir, fs, build_label)?;
    }

    // Apply any startup options found in rc files to startup at RcFile priority
    if let Some(startup_opts) = config.sections.get("startup") {
        let mut priority = OptionPriority::lowest_at_category(PriorityCategory::RcFile);
        for (flag, src) in startup_opts {
            if let Err(e) = startup.apply_flag_with_priority(flag, src, priority.clone()) {
                let prod = if startup.product_name.is_empty() { "bazel" } else { &startup.product_name };
                let msg = &e.message;
                if msg.starts_with("Invalid argument to") {
                    return Err(OptionsParsingError::new(format!("FATAL: {msg}")));
                }
                return Err(OptionsParsingError::new(format!(
                    "FATAL: Unknown startup option: '{flag}'.\n  For more info, run '{prod} help startup_options'."
                )));
            }
            priority = priority.next();
        }
    }

    Ok(config)
}

/// Tokenizes an RC file line respecting quotes, backslashes, and # comments.
pub fn tokenize_rc_line(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '#' && !in_single && !in_double {
            // Rest of line is comment
            break;
        }
        if c == '\\' && !in_single {
            if let Some(next_c) = chars.next() {
                current.push(next_c);
            }
            continue;
        }
        if c == '\'' && !in_double {
            in_single = !in_single;
            continue;
        }
        if c == '"' && !in_single {
            in_double = !in_double;
            continue;
        }
        if c.is_whitespace() && !in_single && !in_double {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(c);
    }
    if !current.is_empty() {
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
        let Some((name, inherits, _, _)) = COMMAND_INHERITANCE.iter().find(|c| c.0 == command) else {
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

/// Evaluates a Bazel version condition (e.g. `>=7.0.0`, `<8.0.0`, `==7.1.0`).
fn eval_bazel_version_condition(condition: &str, current_version: &str) -> Result<bool, String> {
    let mut op_len = 0;
    if condition.starts_with("<=") || condition.starts_with(">=") || condition.starts_with("==") || condition.starts_with("!=") {
        op_len = 2;
    } else if condition.starts_with('<') || condition.starts_with('>') || condition.starts_with('~') {
        op_len = 1;
    }

    if op_len == 0 {
        return Err(format!(
            "Condition '{condition}'. A valid condition is one of the comparison operators ('<', '<=', '>', '>=', '==', '!=', '~') followed by a semantic version."
        ));
    }

    let op = &condition[..op_len];
    let ver_str = condition[op_len..].trim();

    let target_ver = parse_semver(ver_str).ok_or_else(|| {
        format!("Condition '{condition}'. Invalid target semantic version '{ver_str}'.")
    })?;

    // If current_version is development or unparseable, default true
    let curr_ver = match parse_semver(current_version) {
        Some(v) => v,
        None => return Ok(true),
    };

    let ord = curr_ver.cmp(&target_ver);
    match op {
        "<" => Ok(ord == std::cmp::Ordering::Less),
        "<=" => Ok(ord != std::cmp::Ordering::Greater),
        ">" => Ok(ord == std::cmp::Ordering::Greater),
        ">=" => Ok(ord != std::cmp::Ordering::Less),
        "==" => Ok(ord == std::cmp::Ordering::Equal),
        "!=" => Ok(ord != std::cmp::Ordering::Equal),
        "~" => {
            // ~7.1.0 means >= 7.1.0 and < 8.0.0
            Ok(curr_ver.0 == target_ver.0 && curr_ver >= target_ver)
        }
        _ => Err(format!("Unsupported operator '{op}'")),
    }
}

fn parse_semver(s: &str) -> Option<(u32, u32, u32)> {
    let clean = s.trim().trim_start_matches('v').split('-').next().unwrap_or("");
    let parts: Vec<&str> = clean.split('.').collect();
    if parts.is_empty() {
        return None;
    }
    let major = parts[0].parse::<u32>().ok()?;
    let minor = if parts.len() > 1 {
        parts[1].parse::<u32>().ok()?
    } else {
        0
    };
    let patch = if parts.len() > 2 {
        parts[2].parse::<u32>().ok()?
    } else {
        0
    };
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_vfs::InMemoryFileSystem;

    #[test]
    fn test_rc_tokenize() {
        let line = "build --copt=\"-O3 -Wall\" --jobs 16 # this is a comment";
        let tokens = tokenize_rc_line(line);
        assert_eq!(tokens, vec!["build", "--copt=-O3 -Wall", "--jobs", "16"]);
    }

    #[test]
    fn test_rc_relative_import() {
        let fs = InMemoryFileSystem::default();
        let ws = PathFragment::create("/workspace");
        fs.create_directory_and_parents(&ws).unwrap();
        let sub = ws.get_relative_str("sub");
        fs.create_directory_and_parents(&sub).unwrap();

        fs.write_file(
            &sub.get_relative_str("base.bazelrc"),
            b"build -c opt\n",
        )
        .unwrap();

        fs.write_file(
            &sub.get_relative_str("nested.bazelrc"),
            b"import base.bazelrc\nbuild --jobs=32\n",
        )
        .unwrap();

        let mut config = BazelrcConfig::default();
        config
            .parse_file(&sub.get_relative_str("nested.bazelrc"), Some(&ws), &fs, "7.0.0")
            .unwrap();

        let opts = config.get_inherited_command_options("build");
        let flags: Vec<String> = opts.into_iter().map(|(o, _)| o).collect();
        assert_eq!(flags, vec!["-c", "opt", "--jobs=32"]);
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
}
