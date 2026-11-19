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

//! Startup option definitions and command line splitting matching Bazel's C++ client.
//! Mirrors `src/main/cpp/startup_options.cc`, `bazel_startup_options.cc`, and `option_processor.cc`.

use crate::converters::convert_option;
use crate::definition::OptionValueDescription;
use crate::error::OptionsParsingError;
use crate::flag_tables::get_startup_options_map;
use crate::priority::{OptionPriority, PriorityCategory};
use crate::types::{OptionType, OptionValue};
use std::collections::BTreeMap;
use zel_proto::command_server::StartupOption as ProtoStartupOption;
use zel_util::{md5, Os};
use zel_vfs::PathFragment;

/// Parsed startup options affecting client and server lifecycle.
#[derive(Debug, Clone, PartialEq)]
pub struct StartupOptions {
    pub output_base: Option<PathFragment>,
    pub output_user_root: Option<PathFragment>,
    pub install_base: Option<PathFragment>,
    pub server_jvm_out: Option<PathFragment>,
    pub failure_detail_out: Option<PathFragment>,
    pub server_javabase: Option<PathFragment>,
    pub batch: bool,
    pub block_for_lock: bool,
    pub quiet: bool,
    pub max_idle_secs: u64,
    pub bazelrc_files: Vec<String>,
    pub ignore_all_rc_files: bool,
    pub system_rc: bool,
    pub workspace_rc: bool,
    pub home_rc: bool,
    pub host_jvm_args: Vec<String>,
    pub invocation_policy: String,
    pub product_name: String,
    pub raw_tagged_options: Vec<ProtoStartupOption>,
    pub parsed_options: BTreeMap<String, OptionValueDescription>,
    /// First command-line startup option whose *value* failed to convert. The C++ client only
    /// converts values in `ParseStartupOptions`, i.e. after the rc files were read, so the error
    /// is reported by the launcher at that point.
    pub deferred_error: Option<OptionsParsingError>,
}

impl Default for StartupOptions {
    fn default() -> Self {
        Self {
            output_base: None,
            output_user_root: None,
            install_base: None,
            server_jvm_out: None,
            failure_detail_out: None,
            server_javabase: None,
            batch: false,
            block_for_lock: true,
            quiet: false,
            max_idle_secs: 3 * 3600,
            bazelrc_files: Vec::new(),
            ignore_all_rc_files: false,
            system_rc: true,
            workspace_rc: true,
            home_rc: true,
            host_jvm_args: Vec::new(),
            invocation_policy: String::new(),
            product_name: "bazel".to_string(),
            raw_tagged_options: Vec::new(),
            parsed_options: BTreeMap::new(),
            deferred_error: None,
        }
    }
}

/// Converts a flag path value into an absolute `PathFragment` matching `blaze::AbsolutePathFromFlag`
/// (`src/main/cpp/blaze_util.cc`):
/// - `""` resolves to `cwd`.
/// - A leading `~` joins `home` with `&value[1..]` (`blaze_util::JoinPath(GetHomeDir(), value.substr(1))`).
/// - Relative paths are joined onto `cwd`.
pub fn absolute_path_from_flag(
    value: &str,
    cwd: &PathFragment,
    home: Option<&str>,
) -> PathFragment {
    if value.is_empty() {
        return cwd.clone();
    }
    if let Some(rest) = value.strip_prefix('~') {
        let h = home.unwrap_or("");
        let joined = if h.is_empty() {
            rest.to_string()
        } else if rest.is_empty() {
            h.to_string()
        } else {
            format!(
                "{}/{}",
                h.trim_end_matches(['/', '\\']),
                rest.trim_start_matches(['/', '\\'])
            )
        };
        let pf = PathFragment::create(&joined);
        if pf.is_absolute() || h.is_empty() && joined.is_empty() {
            return pf;
        }
        return cwd.get_relative(&pf);
    }
    let pf = PathFragment::create(value);
    if pf.is_absolute() {
        pf
    } else {
        cwd.get_relative(&pf)
    }
}

impl StartupOptions {
    fn expand_startup_path(raw: &str) -> PathFragment {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()
            .filter(|h| !h.is_empty());
        let expanded = if raw == "~" {
            home.unwrap_or_else(|| raw.to_string())
        } else if let Some(rest) = raw.strip_prefix("~/") {
            match home {
                Some(h) => format!("{}/{rest}", h.trim_end_matches(['/', '\\'])),
                None => raw.to_string(),
            }
        } else {
            raw.to_string()
        };
        let pf = PathFragment::create(&expanded);
        if pf.is_absolute() || expanded.is_empty() {
            pf
        } else if let Ok(cwd) = std::env::current_dir() {
            PathFragment::create(&cwd.to_string_lossy()).get_relative(&pf)
        } else {
            pf
        }
    }

    /// Lowercased product name (`StartupOptions::GetLowercaseProductName`), defaulting to `"bazel"`.
    pub fn lowercase_product_name(&self) -> String {
        if self.product_name.is_empty() {
            "bazel".to_string()
        } else {
            self.product_name.to_ascii_lowercase()
        }
    }

    /// Resolves all path-valued startup flags (`output_base`, `output_user_root`, `install_base`,
    /// `server_jvm_out`, `failure_detail_out`, `server_javabase`) via [`absolute_path_from_flag`].
    pub fn absolutize_paths(&mut self, cwd: &PathFragment, home: Option<&str>) {
        let resolve_opt = |opts: &BTreeMap<String, OptionValueDescription>, name: &str| {
            opts.get(name)
                .and_then(|d| d.raw_values.last())
                .map(|raw| absolute_path_from_flag(raw, cwd, home))
        };
        if let Some(p) = resolve_opt(&self.parsed_options, "output_base") {
            self.output_base = Some(p);
        } else if let Some(ref ob) = self.output_base {
            self.output_base = Some(absolute_path_from_flag(ob.path_string(), cwd, home));
        }
        if let Some(p) = resolve_opt(&self.parsed_options, "output_user_root") {
            self.output_user_root = Some(p);
        } else if let Some(ref our) = self.output_user_root {
            self.output_user_root = Some(absolute_path_from_flag(our.path_string(), cwd, home));
        }
        if let Some(p) = resolve_opt(&self.parsed_options, "install_base") {
            self.install_base = Some(p);
        } else if let Some(ref ib) = self.install_base {
            self.install_base = Some(absolute_path_from_flag(ib.path_string(), cwd, home));
        }
        if let Some(p) = resolve_opt(&self.parsed_options, "server_jvm_out") {
            self.server_jvm_out = Some(p);
        }
        if let Some(p) = resolve_opt(&self.parsed_options, "failure_detail_out") {
            self.failure_detail_out = Some(p);
        }
        if let Some(p) = resolve_opt(&self.parsed_options, "server_javabase") {
            self.server_javabase = Some(p);
        }
    }

    /// Resolves output user root checking environment variables (TEST_TMPDIR, XDG_CACHE_HOME).
    pub fn resolve_output_user_root(&self, user: &str, home: Option<&str>) -> PathFragment {
        let xdg = std::env::var("XDG_CACHE_HOME").ok();
        let test_tmpdir = std::env::var("TEST_TMPDIR").ok();
        self.resolve_output_user_root_full(user, home, xdg.as_deref(), test_tmpdir.as_deref())
    }

    /// Resolves output user root according to OS and explicit environment overrides:
    /// - `TEST_TMPDIR`: `<TEST_TMPDIR>/_<product>_{user}`
    /// - macOS: `$XDG_CACHE_HOME/<product>/_<product>_{user}` or `$HOME/Library/Caches/<product>/_<product>_{user}` (or `/var/tmp/_<product>_{user}`)
    /// - Linux/BSD: `$XDG_CACHE_HOME/<product>/_<product>_{user}` or `$HOME/.cache/<product>/_<product>_{user}` (or `/tmp/_<product>_{user}`)
    /// - Windows: `<HOME_OR_USERPROFILE>/_<product>_{user}`
    pub fn resolve_output_user_root_full(
        &self,
        user: &str,
        home: Option<&str>,
        xdg_cache_home: Option<&str>,
        test_tmpdir: Option<&str>,
    ) -> PathFragment {
        if let Some(ref root) = self.output_user_root {
            return root.clone();
        }

        let prod = self.lowercase_product_name();

        if let Some(tmp) = test_tmpdir {
            if !tmp.is_empty() {
                return PathFragment::create(&format!("{tmp}/_{prod}_{user}"));
            }
        }

        match Os::current() {
            Os::Darwin => {
                if let Some(xdg) = xdg_cache_home {
                    if !xdg.is_empty() {
                        return PathFragment::create(&format!("{xdg}/{prod}/_{prod}_{user}"));
                    }
                }
                if let Some(h) = home.filter(|s| !s.is_empty()) {
                    PathFragment::create(&format!("{h}/Library/Caches/{prod}/_{prod}_{user}"))
                } else {
                    PathFragment::create(&format!("/var/tmp/_{prod}_{user}"))
                }
            }
            Os::Windows => {
                let win_home = home
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .or_else(|| std::env::var("USERPROFILE").ok().filter(|s| !s.is_empty()));
                if let Some(h) = win_home {
                    PathFragment::create(&format!("{h}/_{prod}_{user}"))
                } else {
                    PathFragment::create(&format!("C:/temp/_{prod}_{user}"))
                }
            }
            _ => {
                if let Some(xdg) = xdg_cache_home {
                    if !xdg.is_empty() {
                        return PathFragment::create(&format!("{xdg}/{prod}/_{prod}_{user}"));
                    }
                }
                if let Some(h) = home.filter(|s| !s.is_empty()) {
                    PathFragment::create(&format!("{h}/.cache/{prod}/_{prod}_{user}"))
                } else {
                    PathFragment::create(&format!("/tmp/_{prod}_{user}"))
                }
            }
        }
    }

    /// Computes output base (`<output_user_root>/<md5(workspace_dir)>`) unless overridden.
    pub fn resolve_output_base(
        &self,
        workspace_dir: &PathFragment,
        user: &str,
        home: Option<&str>,
    ) -> PathFragment {
        if let Some(ref ob) = self.output_base {
            return ob.clone();
        }
        let user_root = self.resolve_output_user_root(user, home);
        let ws_hash = md5::hex_digest(workspace_dir.path_string().as_bytes());
        user_root.get_relative_str(&ws_hash)
    }

    /// Computes install base (`<output_user_root>/install/<install_md5>`) unless overridden.
    pub fn resolve_install_base(
        &self,
        install_md5: &str,
        user: &str,
        home: Option<&str>,
    ) -> PathFragment {
        if let Some(ref ib) = self.install_base {
            return ib.clone();
        }
        let user_root = self.resolve_output_user_root(user, home);
        user_root
            .get_relative_str("install")
            .get_relative_str(install_md5)
    }

    /// Applies a startup flag with default priority (CommandLine).
    pub fn apply_flag(&mut self, flag: &str, source: &str) -> Result<(), OptionsParsingError> {
        self.apply_flag_with_priority(
            flag,
            source,
            OptionPriority::lowest_at_category(PriorityCategory::CommandLine),
        )
    }

    /// Applies a startup flag with explicit OptionPriority.
    pub fn apply_flag_with_priority(
        &mut self,
        flag: &str,
        source: &str,
        priority: OptionPriority,
    ) -> Result<(), OptionsParsingError> {
        self.raw_tagged_options.push(ProtoStartupOption {
            option: flag.as_bytes().to_vec(),
            source: source.as_bytes().to_vec(),
        });

        let prod = self.lowercase_product_name();
        let startup_map = get_startup_options_map();
        let (flag_name, flag_val) = parse_startup_flag_token(flag);

        let Some(def) = startup_map.get(flag_name.as_str()) else {
            return Err(OptionsParsingError::unknown_startup_option(flag, &prod));
        };

        // Convert value according to schema
        let val_str = match &flag_val {
            Some(v) => v.as_str(),
            None => {
                if def.uses_boolean_syntax() {
                    "true"
                } else if def.option_type == OptionType::Void {
                    ""
                } else {
                    return Err(OptionsParsingError::startup_option_missing_value(
                        flag, &prod,
                    ));
                }
            }
        };

        if def.name == "invocation_policy" && self.parsed_options.contains_key("invocation_policy")
        {
            return Err(OptionsParsingError::new(
                "The startup flag --invocation_policy cannot be specified multiple times.",
            ));
        }

        // `StartupOptions::ProcessArg` range checks (`startup_options.cc`) with their exact messages; they use
        // `blaze_util::safe_strto32` (base 10) rather than `Integer.decode`.
        if let Some(msg) = startup_range_error(def.name, val_str) {
            return Err(OptionsParsingError::new(msg));
        }
        // `--digest_function` is a plain string for the C++ client (`StartupOptions::ProcessArg`); the server
        // validates it with `DigestFunctionConverter`.
        let converted = match if def.name == "digest_function" {
            Ok(OptionValue::String(val_str.to_string()))
        } else {
            convert_option(val_str, def)
        } {
            Ok(v) => v,
            Err(_) => {
                return Err(OptionsParsingError::new(format!(
                    "Invalid argument to --{}: '{val_str}'.",
                    def.name
                )));
            }
        };

        let should_override = self
            .parsed_options
            .get(def.name)
            .is_none_or(|existing| priority >= existing.priority);

        // Update known typed fields when priority permits (or always for allow_multiple list flags).
        match def.name {
            "bazelrc" => {
                if let Some(list) = converted.as_list() {
                    for item in list {
                        if let Some(s) = item.as_str() {
                            self.bazelrc_files.push(s.to_string());
                        }
                    }
                } else if let Some(s) = converted.as_str() {
                    self.bazelrc_files.push(s.to_string());
                }
            }
            "host_jvm_args" => {
                if let Some(list) = converted.as_list() {
                    for item in list {
                        if let Some(s) = item.as_str() {
                            self.host_jvm_args.push(s.to_string());
                        }
                    }
                } else if let Some(s) = converted.as_str() {
                    self.host_jvm_args.push(s.to_string());
                }
            }
            _ if should_override => match def.name {
                "batch" => self.batch = converted.as_bool().unwrap_or(true),
                "block_for_lock" => self.block_for_lock = converted.as_bool().unwrap_or(true),
                "quiet" => self.quiet = converted.as_bool().unwrap_or(true),
                "ignore_all_rc_files" => {
                    self.ignore_all_rc_files = converted.as_bool().unwrap_or(true)
                }
                "system_rc" => self.system_rc = converted.as_bool().unwrap_or(true),
                "workspace_rc" => self.workspace_rc = converted.as_bool().unwrap_or(true),
                "home_rc" => self.home_rc = converted.as_bool().unwrap_or(true),
                "output_base" => {
                    if let Some(s) = converted.as_str() {
                        self.output_base = Some(Self::expand_startup_path(s));
                    }
                }
                "output_user_root" => {
                    if let Some(s) = converted.as_str() {
                        self.output_user_root = Some(Self::expand_startup_path(s));
                    }
                }
                "install_base" => {
                    if let Some(s) = converted.as_str() {
                        self.install_base = Some(Self::expand_startup_path(s));
                    }
                }
                "server_jvm_out" => {
                    if let Some(s) = converted.as_str() {
                        self.server_jvm_out = Some(Self::expand_startup_path(s));
                    }
                }
                "failure_detail_out" => {
                    if let Some(s) = converted.as_str() {
                        self.failure_detail_out = Some(Self::expand_startup_path(s));
                    }
                }
                "server_javabase" => {
                    if let Some(s) = converted.as_str() {
                        self.server_javabase = Some(Self::expand_startup_path(s));
                    }
                }
                "max_idle_secs" => {
                    if let Some(i) = converted.as_long() {
                        self.max_idle_secs = i as u64;
                    }
                }
                "product_name" => {
                    if let Some(s) = converted.as_str() {
                        self.product_name = s.to_string();
                    }
                }
                "invocation_policy" => {
                    if let Some(s) = converted.as_str() {
                        self.invocation_policy = s.to_string();
                    }
                }
                _ => {}
            },
            _ => {}
        }

        // Store into parsed options map
        if let Some(existing) = self.parsed_options.get_mut(def.name) {
            if def.allow_multiple {
                existing.raw_values.push(val_str.to_string());
                if let OptionValue::List(ref mut list) = existing.value {
                    list.push(converted);
                }
            } else if priority >= existing.priority {
                existing.raw_values = vec![val_str.to_string()];
                existing.value = converted;
                existing.priority = priority;
                existing.source = source.to_string();
            }
        } else {
            self.parsed_options.insert(
                def.name.to_string(),
                OptionValueDescription::new(
                    def,
                    vec![val_str.to_string()],
                    converted,
                    priority,
                    source.to_string(),
                ),
            );
        }

        Ok(())
    }
}

/// Helper to parse a startup flag token into `(canonical_flag_name, value)`.
/// Handles `--flag`, `--noflag`, `--flag=val`.
pub(crate) fn parse_startup_flag_token(token: &str) -> (String, Option<String>) {
    let startup_map = get_startup_options_map();

    if let Some((k, v)) = token.strip_prefix("--").and_then(|s| s.split_once('=')) {
        return (k.to_string(), Some(v.to_string()));
    }

    if let Some(naked) = token.strip_prefix("--") {
        if startup_map.contains_key(naked) {
            return (naked.to_string(), None);
        }
        if let Some(no_naked) = naked.strip_prefix("no") {
            if let Some(def) = startup_map.get(no_naked) {
                if def.uses_boolean_syntax() {
                    return (no_naked.to_string(), Some("false".to_string()));
                }
            }
        }
        return (naked.to_string(), None);
    }

    (token.to_string(), None)
}

/// Splits CLI arguments into `(StartupOptions, command, command_args)`.
/// Exactly matches `OptionProcessor::SplitCommandLine` in Bazel C++.
pub fn split_command_line(
    args: &[String],
) -> Result<(StartupOptions, String, Vec<String>), OptionsParsingError> {
    let mut startup = StartupOptions::default();
    let startup_map = get_startup_options_map();
    let mut i = 0;
    let mut priority = OptionPriority::lowest_at_category(PriorityCategory::CommandLine);

    while i < args.len() {
        let arg = &args[i];

        if !arg.starts_with('-') || arg == "-" {
            if arg.is_empty() {
                let prod = startup.lowercase_product_name();
                return Err(OptionsParsingError::new(format!(
                    "Command cannot be the empty string.\n  For more info, run '{prod} help'."
                )));
            }
            let command = arg.clone();
            let command_args = args[i + 1..].to_vec();
            return Ok((startup, command, command_args));
        }

        if arg == "--help" || arg == "-help" || arg == "-h" {
            return Ok((startup, "help".to_string(), args[i + 1..].to_vec()));
        }

        if arg == "--version" {
            if i == 0 && args.len() == 1 {
                return Ok((
                    startup,
                    "version".to_string(),
                    vec!["--startup_version".to_string()],
                ));
            } else {
                let prod = startup.lowercase_product_name();
                return Err(OptionsParsingError::unknown_startup_option(arg, &prod));
            }
        }

        // StartupOptions::MaybeCheckValidNullary: `--batch=1` / `--nobatch=x` (a nullary flag
        // with an inline value; `--block_for_lock` is exempt) is rejected while splitting.
        if let Some((f, _)) = arg.split_once('=') {
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
                return Err(OptionsParsingError::new(format!(
                    "In argument '{arg}': option '{f}' does not take a value."
                )));
            }
        }

        let (flag_name, inline_val) = parse_startup_flag_token(arg);

        let Some(def) = startup_map.get(flag_name.as_str()) else {
            let prod = startup.lowercase_product_name();
            return Err(OptionsParsingError::unknown_startup_option(arg, &prod));
        };

        let deferrable = |msg: &str| {
            msg.starts_with("Invalid argument to")
                || msg.starts_with("The startup flag --invocation_policy")
        };

        if inline_val.is_some() || !def.requires_value() {
            if let Err(e) = startup.apply_flag_with_priority(arg, "command line", priority.clone())
            {
                if !deferrable(&e.message) {
                    return Err(e);
                }
                startup.deferred_error.get_or_insert(e);
            }
            priority = priority.next();
            i += 1;
        } else {
            // Unary option with next arg value: e.g. `--output_base /tmp/ob`
            if i + 1 >= args.len() {
                let prod = startup.lowercase_product_name();
                return Err(OptionsParsingError::startup_option_missing_value(
                    arg, &prod,
                ));
            }
            let combined = format!("{}={}", arg, args[i + 1]);
            if let Err(e) =
                startup.apply_flag_with_priority(&combined, "command line", priority.clone())
            {
                if !deferrable(&e.message) {
                    return Err(e);
                }
                startup.deferred_error.get_or_insert(e);
            }
            priority = priority.next();
            i += 2;
        }
    }

    // No command provided: defaults to "help"
    Ok((startup, String::new(), Vec::new()))
}

/// `blaze_util::safe_strto32`: optional leading whitespace and sign, decimal digits, nothing after, fits `i32`.
fn safe_strto32(value: &str) -> Option<i32> {
    let v = value.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let digits = v.strip_prefix(['+', '-']).unwrap_or(v);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    v.parse::<i32>().ok()
}

/// Range checks of `StartupOptions::ProcessArg` that have a dedicated message.
fn startup_range_error(name: &str, value: &str) -> Option<String> {
    let n = safe_strto32(value);
    match name {
        "io_nice_level" if n.is_none_or(|n| n > 7) => {
            Some(format!("Invalid argument to --io_nice_level: '{value}'. Must not exceed 7."))
        }
        "max_idle_secs" if n.is_none_or(|n| n < 0) => Some(format!("Invalid argument to --max_idle_secs: '{value}'.")),
        "connect_timeout_secs" if n.is_none_or(|n| !(1..=3600).contains(&n)) => Some(format!(
            "Invalid argument to --connect_timeout_secs: '{value}'.\nMust be an integer between 1 and 3600.\n"
        )),
        "local_startup_timeout_secs" if n.is_none_or(|n| n < 1) => Some(format!(
            "Invalid argument to --local_startup_timeout_secs: '{value}'.\nMust be a positive integer.\n"
        )),
        "command_port" if n.is_none_or(|n| !(0..=65535).contains(&n)) => Some(format!(
            "Invalid argument to --command_port: '{value}'.\nMust be a valid port number or 0.\n"
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_startup_and_command() {
        let args = vec![
            "--output_base=/tmp/ob".to_string(),
            "--batch".to_string(),
            "--connect_timeout_secs".to_string(),
            "45".to_string(),
            "build".to_string(),
            "-c".to_string(),
            "opt".to_string(),
            "//foo:bar".to_string(),
        ];
        let (startup, cmd, cmd_args) = split_command_line(&args).unwrap();
        assert_eq!(startup.output_base, Some(PathFragment::create("/tmp/ob")));
        assert!(startup.batch);
        assert_eq!(cmd, "build");
        assert_eq!(cmd_args, vec!["-c", "opt", "//foo:bar"]);
    }

    #[test]
    fn test_unknown_startup_flag() {
        let args = vec!["--unknown_startup_xyz".to_string(), "build".to_string()];
        let err = split_command_line(&args).unwrap_err();
        assert!(err
            .message
            .contains("Unknown startup option: '--unknown_startup_xyz'"));
    }
}
