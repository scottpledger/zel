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

//! Schema-driven options parser matching Bazel's `OptionsParserImpl.java`.

use crate::converters::{convert_flag_alias, convert_option};
use crate::definition::{OptionDefinition, OptionValueDescription};
use crate::error::OptionsParsingError;
use crate::flag_tables::{
    get_command_abbrev_map, get_command_allowed_classes, get_command_options_map,
    is_command_allowed, ALL_COMMAND_OPTIONS,
};
use crate::invocation_policy::{
    InvocationPolicy, PolicyOperation, ReplacementValue, SetValueBehavior,
};
use crate::priority::{OptionPriority, PriorityCategory};
use crate::spellcheck::did_you_mean;
use crate::types::{OptionType, OptionValue};
use std::collections::{BTreeMap, HashSet};
use zel_vfs::PathFragment;

/// A skipped option token (e.g. Starlark flag `--//...` or `--@...`) recorded with priority
/// and explicitness metadata (`OptionsParserImpl.skippedOptions`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedArg {
    pub raw_arg: String,
    pub priority: OptionPriority,
    pub is_explicit: bool,
    pub expanded_from: Option<&'static str>,
}

/// A syntactically recognized Starlark build-setting occurrence on the command line / rc file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StarlarkOccurrence {
    /// Original argument token (e.g. `--//pkg:flag`, `--no//pkg:flag`, `--//pkg:flag=val`, or `--alias`).
    pub raw_arg: String,
    /// Target name after `--` (or `--no` when no `=`): e.g. `//pkg:flag` or `no//pkg:flag`.
    pub flag_name: String,
    /// Inline value when `=` was present (`None` for bare `--//pkg:flag` / `--no//pkg:flag`).
    pub value: Option<String>,
    /// True when `=` was absent and `--no` prefix was used.
    pub negated: bool,
    /// Next token if it does not start with `-` (used for `Did you mean <flag>=<val>?`).
    pub next_non_flag_token: Option<String>,
}

/// Returns true iff `command` runs analysis (`Command.BuildPhase.analyzes()`).
pub fn command_analyzes(command: &str) -> bool {
    matches!(
        command,
        "build"
            | "test"
            | "coverage"
            | "run"
            | "cquery"
            | "aquery"
            | "fetch"
            | "vendor"
            | "print_action"
            | "mobile-install"
    )
}

/// Parsed command options matching Bazel's command option models.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCommandOptions {
    // Legacy fields for backward compatibility with zel-server
    pub compilation_mode: String,
    pub jobs: usize,
    pub keep_going: bool,
    pub subcommands: bool,
    pub verbose_failures: bool,
    pub expunge: bool,
    pub expunge_async: bool,
    pub query_output_format: String,
    pub show_make_env: bool,
    pub disk_cache: Option<PathFragment>,
    pub profile: Option<PathFragment>,
    pub build_event_json_file: Option<PathFragment>,
    pub test_output: String,
    pub test_filter: Option<String>,
    pub spawn_strategy: String,
    pub user_defines: BTreeMap<String, String>,
    pub canonical_flags: Vec<String>,
    pub targets: Vec<String>,
    pub passthrough_args: Vec<String>,

    // Starlark and schema-driven enhancements
    pub starlark_options: BTreeMap<String, String>,
    /// Every `--//pkg:flag=value` occurrence in command-line order (list-typed flags accumulate them).
    pub starlark_occurrences: Vec<(String, String)>,
    /// Raw Starlark flag occurrences with exact `value: Option<String>` and `negated` preserved for `StarlarkOptionsParser`.
    pub raw_starlark_args: Vec<StarlarkOccurrence>,
    pub flag_aliases: BTreeMap<String, String>,
    pub parsed_options: BTreeMap<String, OptionValueDescription>,
    /// `Option '<old>' is deprecated: Use --<name> instead` for options given by their `oldName`.
    pub old_name_warnings: Vec<String>,
    /// Parser warnings (expansion value ignored, expansion override, implicit-default, etc.).
    pub warnings: Vec<String>,
    /// Pre-`--` residue tokens in encounter order (`OptionsParserImplResult.preDoubleDashResidue`).
    pub pre_double_dash_residue: Vec<String>,
    /// Post-`--` residue tokens in encounter order (`OptionsParserImplResult.postDoubleDashResidue`).
    pub post_double_dash_residue: Vec<String>,
    /// Skipped Starlark option tokens in priority order (`OptionsParserImpl.skippedOptions`).
    pub skipped_args: Vec<SkippedArg>,
    /// All `--flag_alias=name=target` mappings (including native flag targets) for shorthand swap.
    command_line_aliases: BTreeMap<String, String>,
}

/// `--jobs` default (`auto` = `HOST_CPUS`, capped at `MAX_JOBS`); kept as a plain number for the
/// legacy `jobs` field. The authoritative evaluation of the raw flag string is `zel-exec`'s.
fn default_jobs() -> usize {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    cpus.clamp(1, zel_util::resource_converter::MAX_JOBS as usize)
}

impl Default for ParsedCommandOptions {
    fn default() -> Self {
        Self {
            compilation_mode: "fastbuild".to_string(),
            jobs: default_jobs(),
            keep_going: false,
            subcommands: false,
            verbose_failures: false,
            expunge: false,
            expunge_async: false,
            query_output_format: "label".to_string(),
            show_make_env: false,
            disk_cache: None,
            profile: None,
            build_event_json_file: None,
            test_output: "summary".to_string(),
            test_filter: None,
            spawn_strategy: "local".to_string(),
            user_defines: BTreeMap::new(),
            canonical_flags: Vec::new(),
            targets: Vec::new(),
            passthrough_args: Vec::new(),
            starlark_options: BTreeMap::new(),
            starlark_occurrences: Vec::new(),
            raw_starlark_args: Vec::new(),
            flag_aliases: BTreeMap::new(),
            parsed_options: BTreeMap::new(),
            old_name_warnings: Vec::new(),
            warnings: Vec::new(),
            pre_double_dash_residue: Vec::new(),
            post_double_dash_residue: Vec::new(),
            skipped_args: Vec::new(),
            command_line_aliases: BTreeMap::new(),
        }
    }
}

impl ParsedCommandOptions {
    /// Extracts `--client_env=NAME=VALUE` and `--client_env NAME=VALUE` (before `--`) into a map
    /// where later occurrences overwrite earlier ones (`ClientOptions.clientEnv` + `CommandEnvironment`).
    pub fn extract_client_env(args: &[String]) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            if a == "--" {
                break;
            }
            if let Some(rest) = a.strip_prefix("--client_env=") {
                if let Ok((k, v)) = crate::converters::convert_assignment(rest) {
                    map.insert(k, v);
                }
                i += 1;
            } else if a == "--client_env" && i + 1 < args.len() {
                if let Ok((k, v)) = crate::converters::convert_assignment(&args[i + 1]) {
                    map.insert(k, v);
                }
                i += 2;
            } else {
                i += 1;
            }
        }
        map
    }

    /// Returns the `--client_env` map recorded in `parsed_options`, last assignment winning.
    pub fn client_env_map(&self) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        if let Some(desc) = self.parsed_options.get("client_env") {
            if let OptionValue::List(items) = &desc.value {
                for item in items {
                    if let OptionValue::Entry(k, v) = item {
                        if let Some(val) = v.as_str() {
                            map.insert(k.clone(), val.to_string());
                        }
                    }
                }
            }
        }
        map
    }

    /// Default parse without command context (defaults to "build").
    pub fn parse(args: &[String]) -> Result<Self, OptionsParsingError> {
        Self::parse_for_command("build", args)
    }

    /// Parses command-line arguments for a specific command (e.g. "build", "test", "run", "query").
    pub fn parse_for_command(command: &str, args: &[String]) -> Result<Self, OptionsParsingError> {
        Self::parse_internal(command, args, PriorityCategory::CommandLine, true, true)
    }

    /// Parses command-line arguments without `editOptions` or invocation policy (for `canonicalize-flags`).
    pub fn parse_for_command_no_edit(
        command: &str,
        args: &[String],
    ) -> Result<Self, OptionsParsingError> {
        Self::parse_internal(command, args, PriorityCategory::CommandLine, false, false)
    }

    /// Parses options with a specific initial priority category.
    pub fn parse_with_priority(
        command: &str,
        args: &[String],
        priority: PriorityCategory,
    ) -> Result<Self, OptionsParsingError> {
        Self::parse_internal(command, args, priority, true, true)
    }

    /// `OptionsParser.getResidue`: pre-`--` followed by post-`--` residue.
    pub fn get_residue(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(
            self.pre_double_dash_residue.len() + self.post_double_dash_residue.len(),
        );
        out.extend(self.pre_double_dash_residue.iter().cloned());
        out.extend(self.post_double_dash_residue.iter().cloned());
        out
    }

    /// `OptionsParser.getPreDoubleDashResidue`: returns full residue when post-`--` is empty,
    /// otherwise filters out elements contained in `post_double_dash_residue`.
    pub fn get_pre_double_dash_residue(&self) -> Vec<String> {
        if self.post_double_dash_residue.is_empty() {
            return self.get_residue();
        }
        self.get_residue()
            .into_iter()
            .filter(|item| !self.post_double_dash_residue.contains(item))
            .collect()
    }

    pub fn get_post_double_dash_residue(&self) -> &[String] {
        &self.post_double_dash_residue
    }

    /// `OptionsParser.getExplicitStarlarkOptions`: intersects explicit skipped option keys
    /// (`raw_arg[2..]` split on `=`) with parsed `starlark_options`.
    pub fn get_explicit_starlark_options(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for skipped in &self.skipped_args {
            if !skipped.is_explicit {
                continue;
            }
            if let Some(rest) = skipped.raw_arg.strip_prefix("--") {
                let key = rest.split('=').next().unwrap_or("");
                if let Some(val) = self.starlark_options.get(key) {
                    out.insert(key.to_string(), val.clone());
                }
            }
        }
        out
    }

    pub fn get_warnings(&self) -> &[String] {
        &self.warnings
    }

    fn parse_internal(
        command: &str,
        args: &[String],
        initial_priority: PriorityCategory,
        run_edit_options: bool,
        run_invocation_policy: bool,
    ) -> Result<Self, OptionsParsingError> {
        let mut opts = Self::default();
        let priority = OptionPriority::lowest_at_category(initial_priority);
        let (pre, post) = opts.parse_into(command, args, priority, "command line", None, None)?;
        opts.pre_double_dash_residue = pre;
        opts.post_double_dash_residue = post;

        // Populate legacy `targets` and `passthrough_args` for server compatibility.
        opts.targets
            .extend(opts.pre_double_dash_residue.iter().cloned());
        if !opts.post_double_dash_residue.is_empty() {
            if command == "run" {
                if opts.targets.is_empty() {
                    opts.targets.push(opts.post_double_dash_residue[0].clone());
                    opts.passthrough_args
                        .extend_from_slice(&opts.post_double_dash_residue[1..]);
                } else {
                    opts.passthrough_args
                        .extend_from_slice(&opts.post_double_dash_residue);
                }
            } else {
                opts.targets
                    .extend_from_slice(&opts.post_double_dash_residue);
            }
        }

        if run_edit_options {
            opts.apply_edit_options(command)?;
        }

        // Apply invocation policy if --invocation_policy was set. `canonicalize-flags` declares
        // `--invocation_policy` as its own option (`CanonicalizeCommand.Options`) and only applies it to
        // the flags it canonicalizes, never to its own options.
        if run_invocation_policy {
            if let Some(policy_val) = opts
                .get_str("invocation_policy")
                .filter(|_| command != "canonicalize-flags")
                .map(str::to_string)
            {
                if !policy_val.is_empty() {
                    if let Ok(policy) = InvocationPolicy::parse(&policy_val) {
                        opts.apply_invocation_policy(&policy, command)?;
                    }
                }
            }
        }

        Ok(opts)
    }

    fn parse_edit_args(
        &mut self,
        command: &str,
        args: &[String],
        category: PriorityCategory,
    ) -> Result<(), OptionsParsingError> {
        let _ = self.parse_into(
            command,
            args,
            OptionPriority::lowest_at_category(category),
            "command line",
            None,
            None,
        )?;
        Ok(())
    }

    fn apply_edit_options(&mut self, command: &str) -> Result<(), OptionsParsingError> {
        // TestCommand.editOptions (inherited by test, coverage, cquery)
        if matches!(command, "test" | "coverage" | "cquery")
            && self.test_output.eq_ignore_ascii_case("streamed")
        {
            self.parse_edit_args(
                command,
                &[
                    "--test_sharding_strategy=disabled".to_string(),
                    "--test_strategy=exclusive".to_string(),
                ],
                PriorityCategory::SoftwareRequirement,
            )?;
        }
        if command == "coverage" {
            self.parse_edit_args(
                command,
                &["--collect_code_coverage".to_string()],
                PriorityCategory::SoftwareRequirement,
            )?;
            self.parse_edit_args(
                command,
                &["--test_timeout=300,600,1200,3600".to_string()],
                PriorityCategory::ComputedDefault,
            )?;
        } else if command == "cquery" {
            if self.get_str("transitions").is_some_and(|t| t != "none") {
                self.parse_edit_args(
                    command,
                    &["--output=transitions".to_string()],
                    PriorityCategory::ComputedDefault,
                )?;
            }
            self.parse_edit_args(
                command,
                &["--nobuild".to_string(), "--build_manual_tests".to_string()],
                PriorityCategory::ComputedDefault,
            )?;
            self.parse_edit_args(
                command,
                &[
                    "--noexpand_test_suites".to_string(),
                    "--nobuild_tests_only".to_string(),
                ],
                PriorityCategory::SoftwareRequirement,
            )?;
            if let Some(scf) = self
                .get_str("show_config_fragments")
                .filter(|v| *v != "off")
                .map(str::to_string)
            {
                self.parse_edit_args(
                    command,
                    &[format!("--include_config_fragments_provider={scf}")],
                    PriorityCategory::ComputedDefault,
                )?;
            }
        } else if matches!(command, "aquery" | "fetch" | "vendor") {
            self.parse_edit_args(
                command,
                &["--nobuild".to_string()],
                PriorityCategory::ComputedDefault,
            )?;
        } else if command == "mobile-install" {
            let aspect = self
                .get_str("mobile_install_aspect")
                .unwrap_or("@rules_android//mobile_install:mi.bzl")
                .to_string();
            self.parse_edit_args(
                command,
                &[
                    format!("--aspects={aspect}%MIASPECT"),
                    "--output_groups=mobile_install_INTERNAL_".to_string(),
                    "--output_groups=mobile_install_launcher_INTERNAL_".to_string(),
                ],
                PriorityCategory::CommandLine,
            )?;
        }
        Ok(())
    }

    /// `OptionsParserImpl.swapShorthandAlias`
    fn swap_shorthand_alias(
        &mut self,
        arg: &str,
        cmd_map: &std::collections::HashMap<&'static str, &'static OptionDefinition>,
    ) -> String {
        if !arg.starts_with("--") {
            return arg.to_string();
        }
        let body = &arg[2..];
        let eq = body.find('=');
        let actual_arg = match eq {
            Some(pos) => &body[..pos],
            None => body,
        };
        if let Some(target) = self.command_line_aliases.get(actual_arg) {
            return match eq {
                Some(pos) => format!("--{target}{}", &body[pos..]),
                None => format!("--{target}"),
            };
        }
        if let Some(name_without_no) = actual_arg.strip_prefix("no") {
            if self.command_line_aliases.contains_key(name_without_no) {
                if let Some(def) = cmd_map.get(name_without_no) {
                    if let Some(dw) = def.deprecation_warning {
                        self.warnings
                            .push(format!("Option '{}' is deprecated: {}", def.name, dw));
                    } else {
                        self.warnings.push(format!(
                            "Flag --no{} is deprecated. Use --{}=false instead.",
                            name_without_no, name_without_no
                        ));
                    }
                }
            }
        }
        arg.to_string()
    }

    fn parse_into(
        &mut self,
        command: &str,
        args: &[String],
        mut priority: OptionPriority,
        source: &str,
        implicit_dependent: Option<&'static str>,
        expanded_from: Option<&'static str>,
    ) -> Result<(Vec<String>, Vec<String>), OptionsParsingError> {
        let cmd_map = get_command_options_map();
        let abbrev_map = get_command_abbrev_map();
        let allowed_classes = get_command_allowed_classes(command);
        let mut pre_residue = Vec::new();
        let mut post_residue = Vec::new();
        let analyzes = command_analyzes(command);
        let mut i = 0;

        while i < args.len() {
            let raw_arg = &args[i];

            // 1. Non-flag argument is a residue / target / query argument
            if !raw_arg.starts_with('-') {
                pre_residue.push(raw_arg.clone());
                i += 1;
                continue;
            }

            if (raw_arg.starts_with("-//") || raw_arg.starts_with("-@"))
                && !raw_arg.starts_with("--")
            {
                return Err(OptionsParsingError::new(format!(
                    "Invalid options syntax: {raw_arg}\nNote: Negative target patterns can only appear after the end of options marker ('--'). Flags corresponding to Starlark-defined build settings always start with '--', not '-'."
                )));
            }

            // Shorthand alias swap (`OptionsParserImpl.swapShorthandAlias`)
            let swapped = self.swap_shorthand_alias(raw_arg, &cmd_map);
            let arg = swapped.as_str();

            // 2. Double dash `--` ends options parsing
            if arg == "--" {
                post_residue.extend_from_slice(&args[i + 1..]);
                break;
            }

            // 3. Starlark flags (`STARLARK_SKIPPED_PREFIXES`: `--//`, `--@`, `--no//`, `--no@`)
            // Only recognized on commands that analyze (`BlazeOptionHandler.parseStarlarkOptions`).
            if analyzes
                && (arg.starts_with("--//")
                    || arg.starts_with("--@")
                    || arg.starts_with("--no//")
                    || arg.starts_with("--no@"))
            {
                self.skipped_args.push(SkippedArg {
                    raw_arg: arg.to_string(),
                    priority: priority.clone(),
                    is_explicit: implicit_dependent.is_none() && expanded_from.is_none(),
                    expanded_from,
                });
                let next_non_flag = args.get(i + 1).filter(|s| !s.starts_with('-')).cloned();
                if let Some((k, v)) = arg[2..].split_once('=') {
                    // Bazel StarlarkOptionsParser only strips `no` when there is NO `=`.
                    self.starlark_options.insert(k.to_string(), v.to_string());
                    self.starlark_occurrences
                        .push((k.to_string(), v.to_string()));
                    self.canonical_flags.push(format!("--{k}={v}"));
                    self.raw_starlark_args.push(StarlarkOccurrence {
                        raw_arg: raw_arg.clone(),
                        flag_name: k.to_string(),
                        value: Some(v.to_string()),
                        negated: false,
                        next_non_flag_token: next_non_flag,
                    });
                } else if arg.starts_with("--no//") || arg.starts_with("--no@") {
                    let stripped = &arg[4..];
                    self.starlark_options
                        .insert(stripped.to_string(), "false".to_string());
                    self.starlark_occurrences
                        .push((stripped.to_string(), "false".to_string()));
                    self.canonical_flags.push(format!("--{stripped}=false"));
                    self.raw_starlark_args.push(StarlarkOccurrence {
                        raw_arg: raw_arg.clone(),
                        flag_name: stripped.to_string(),
                        value: None,
                        negated: true,
                        next_non_flag_token: next_non_flag,
                    });
                } else {
                    let stripped = &arg[2..];
                    self.starlark_options
                        .insert(stripped.to_string(), "true".to_string());
                    self.starlark_occurrences
                        .push((stripped.to_string(), "true".to_string()));
                    self.canonical_flags.push(format!("--{stripped}=true"));
                    self.raw_starlark_args.push(StarlarkOccurrence {
                        raw_arg: raw_arg.clone(),
                        flag_name: stripped.to_string(),
                        value: None,
                        negated: false,
                        next_non_flag_token: next_non_flag,
                    });
                }
                priority = priority.next();
                i += 1;
                continue;
            }

            // `OptionsParserImpl.identifyOptionAndPossibleArgument`: only `-x`, `-x-` and
            // `--long[=value]` are valid shapes.
            check_arg_syntax(arg)?;

            // 4. Identify OptionDefinition (long flag, boolean `no` prefix, or abbrev)
            let (lookup_name, inline_value, is_negated_bool, _is_abbrev) =
                resolve_arg_shape(arg, &cmd_map, &abbrev_map, &self.flag_aliases);

            // 5. Check if it's a flag alias (e.g. `--myalias=val`, `--myalias`, or `--nomyalias`)
            if analyzes {
                if let Some(target_label) = self.flag_aliases.get(&lookup_name).cloned() {
                    let next_non_flag = args.get(i + 1).filter(|s| !s.starts_with('-')).cloned();
                    let val_str = if is_negated_bool {
                        "false"
                    } else if let Some(ref v) = inline_value {
                        v.as_str()
                    } else {
                        "true"
                    };
                    self.skipped_args.push(SkippedArg {
                        raw_arg: format!("--{target_label}={val_str}"),
                        priority: priority.clone(),
                        is_explicit: implicit_dependent.is_none() && expanded_from.is_none(),
                        expanded_from,
                    });
                    self.starlark_options
                        .insert(target_label.clone(), val_str.to_string());
                    self.starlark_occurrences
                        .push((target_label.clone(), val_str.to_string()));
                    self.canonical_flags
                        .push(format!("--{target_label}={val_str}"));
                    self.raw_starlark_args.push(StarlarkOccurrence {
                        raw_arg: raw_arg.clone(),
                        flag_name: target_label,
                        value: if is_negated_bool {
                            None
                        } else {
                            inline_value.clone()
                        },
                        negated: is_negated_bool,
                        next_non_flag_token: next_non_flag,
                    });
                    priority = priority.next();
                    i += 1;
                    continue;
                }
            }

            // `--no<name>` on a known non-boolean option, or `--no<name>=value`.
            if !cmd_map.contains_key(lookup_name.as_str()) && arg.starts_with("--") {
                let name_end = arg.find('=').unwrap_or(arg.len());
                if let Some(rest) = arg[2..name_end].strip_prefix("no") {
                    if let Some(d) = cmd_map.get(rest) {
                        if !d.uses_boolean_syntax() {
                            return Err(OptionsParsingError::with_arg(
                                format!("Illegal use of 'no' prefix on non-boolean option: {arg}"),
                                arg,
                            ));
                        }
                        if name_end != arg.len() {
                            return Err(OptionsParsingError::with_arg(
                                format!("Unexpected value after boolean option: {arg}"),
                                arg,
                            ));
                        }
                    }
                }
            }

            let Some(def) = cmd_map.get(lookup_name.as_str()) else {
                let mut storage = Vec::new();
                let valid_names =
                    get_valid_option_names(command, allowed_classes.as_ref(), &mut storage);
                let suggestion = if arg.starts_with("--") {
                    did_you_mean(arg, valid_names)
                } else {
                    String::new()
                };
                return Err(OptionsParsingError::unrecognized_option(arg, &suggestion));
            };

            let mut def = *def;
            if !def.commands.is_empty() && !is_command_allowed(command, def) {
                if let Some(matching) = ALL_COMMAND_OPTIONS
                    .iter()
                    .find(|d| d.name == lookup_name && is_command_allowed(command, d))
                {
                    def = matching;
                }
            }

            if def.old_name == Some(lookup_name.as_str()) && def.name != lookup_name.as_str() {
                self.old_name_warnings.push(format!(
                    "Option '{}' is deprecated: Use --{} instead",
                    lookup_name, def.name
                ));
            }

            // Check if option is allowed on the current command
            if !def.commands.is_empty() && !is_command_allowed(command, def) {
                let mut storage = Vec::new();
                let valid_names =
                    get_valid_option_names(command, allowed_classes.as_ref(), &mut storage);
                let suggestion = if arg.starts_with("--") {
                    did_you_mean(arg, valid_names)
                } else {
                    String::new()
                };
                return Err(OptionsParsingError::unrecognized_option(arg, &suggestion));
            }

            // 5. Handle flag value extraction
            let (raw_value, consumed) = if is_negated_bool {
                ("false".to_string(), 1)
            } else if let Some(ref v) = inline_value {
                (v.clone(), 1)
            } else if def.uses_boolean_syntax() {
                ("true".to_string(), 1)
            } else if def.option_type == OptionType::Void {
                ("".to_string(), 1)
            } else {
                if i + 1 >= args.len() {
                    return Err(OptionsParsingError::new(format!(
                        "Expected value after {arg}"
                    )));
                }
                (args[i + 1].clone(), 2)
            };
            let command_line_form = if consumed == 2 {
                format!("{arg} {raw_value}")
            } else {
                arg.to_string()
            };

            // 6. Expansion options: no value/source/canonical form of their own; expand at child priority.
            if let Some(expansion_args) = def.expansion {
                if !expansion_args.is_empty() {
                    if let Some(ref v) = inline_value {
                        if !is_negated_bool && !v.is_empty() {
                            self.warnings.push(format!(
                                "option '--{}' is an expansion option. It does not accept values, and does not change its expansion based on the value provided. Value '{}' will be ignored.",
                                def.name, v
                            ));
                        }
                    }
                    let child_p = priority.get_child_priority()?;
                    let exp_source = if source.is_empty() {
                        format!("expanded from option '--{}'", def.name)
                    } else {
                        format!("expanded from option '--{}' (source {source})", def.name)
                    };
                    let exp_vec: Vec<String> =
                        expansion_args.iter().map(|s| (*s).to_string()).collect();
                    let (sub_pre, sub_post) = self.parse_into(
                        command,
                        &exp_vec,
                        child_p,
                        &exp_source,
                        None,
                        Some(def.name),
                    )?;
                    if !sub_pre.is_empty() || !sub_post.is_empty() {
                        let mut rem = sub_pre;
                        rem.extend(sub_post);
                        return Err(OptionsParsingError::new(format!(
                            "Unparsed options remain after processing {raw_value}: {}",
                            rem.join(" ")
                        )));
                    }
                    priority = priority.next();
                    i += consumed;
                    continue;
                }
            }

            // 7. Convert and apply value
            // `VoidConverter`: `--flag=value` is rejected unless the value is "null". Expansion flags never
            // invoke their converter (`Converters.VoidConverter` javadoc), so any `=value` is accepted there.
            if def.option_type == OptionType::Void && !def.is_expansion() {
                if let Some(ref v) = inline_value {
                    if !is_negated_bool && v != "null" {
                        return Err(OptionsParsingError::new(format!(
                            "While parsing option {command_line_form}: '{v}' unexpected"
                        )));
                    }
                }
            }
            let converted = convert_option(&raw_value, def).map_err(|e| {
                OptionsParsingError::new(format!(
                    "While parsing option {command_line_form}: {}",
                    e.message
                ))
            })?;

            // Register flag alias mappings (used by `swap_shorthand_alias`).
            if def.name == "flag_alias" && implicit_dependent.is_none() {
                if let Some((alias, target)) = raw_value.split_once('=') {
                    self.command_line_aliases
                        .insert(alias.to_string(), target.to_string());
                }
                if let Ok((alias, label)) = convert_flag_alias(&raw_value) {
                    self.flag_aliases.insert(alias, label);
                }
            }

            if def.name == "define" {
                match converted {
                    OptionValue::Entry(ref k, ref v) => {
                        if let Some(v) = v.as_str() {
                            self.user_defines.insert(k.clone(), v.to_string());
                        }
                    }
                    OptionValue::Map(ref m) => {
                        for (k, v) in m {
                            self.user_defines.insert(k.clone(), v.clone());
                        }
                    }
                    _ => {}
                }
            }

            // Record into canonical flags unless this instance is an implicit requirement.
            if implicit_dependent.is_none() {
                if def.option_type == OptionType::Boolean {
                    let b_val = if is_negated_bool {
                        false
                    } else {
                        converted.as_bool().unwrap_or(true)
                    };
                    self.canonical_flags.push(format!(
                        "--{}={}",
                        def.name,
                        if b_val { "1" } else { "0" }
                    ));
                } else if def.option_type != OptionType::Void {
                    let canonical_value = if is_negated_bool {
                        "0".to_string()
                    } else if inline_value.is_none() && def.uses_boolean_syntax() {
                        "1".to_string()
                    } else {
                        raw_value.clone()
                    };
                    self.canonical_flags
                        .push(format!("--{}={canonical_value}", def.name));
                }
            }

            // Store into parsed_options respecting OptionPriority!
            let mut should_update_legacy = false;
            if let Some(existing) = self.parsed_options.get_mut(def.name) {
                if def.allow_multiple {
                    existing.raw_values.push(raw_value.clone());
                    append_instance(&mut existing.value, converted.clone());
                    should_update_legacy = true;
                } else if priority >= existing.priority {
                    if existing.value != converted {
                        let same_cat = priority.category == existing.priority.category;
                        if let (Some(imp), Some(prev_imp)) =
                            (implicit_dependent, existing.implicit_dependent)
                        {
                            if imp != prev_imp {
                                self.warnings.push(format!(
                                    "option '--{}' is implicitly defined by both option '--{prev_imp}' and option '--{imp}'",
                                    def.name
                                ));
                            }
                        } else if let Some(imp) = implicit_dependent {
                            if same_cat {
                                self.warnings.push(format!(
                                    "option '--{}' is implicitly defined by option '--{imp}'; the implicitly set value overrides the previous one",
                                    def.name
                                ));
                            }
                        } else if let Some(prev_imp) = existing.implicit_dependent {
                            self.warnings.push(format!(
                                "A new value for option '--{}' overrides a previous implicit setting of that option by option '--{prev_imp}'",
                                def.name
                            ));
                        } else if same_cat
                            && priority.category == PriorityCategory::CommandLine
                            && existing.expanded_from.is_none()
                            && expanded_from.is_some()
                        {
                            let ef = expanded_from.unwrap();
                            let prev_clf = existing.command_line_form.as_deref().unwrap_or("");
                            self.warnings.push(format!(
                                "option '--{ef}' was expanded and now overrides the explicit option {prev_clf} with {command_line_form}"
                            ));
                        }
                    }
                    existing.raw_values = vec![raw_value.clone()];
                    existing.value = converted.clone();
                    existing.priority = priority.clone();
                    existing.source = source.to_string();
                    existing.expanded_from = expanded_from;
                    existing.implicit_dependent = implicit_dependent;
                    existing.command_line_form = Some(command_line_form.clone());
                    should_update_legacy = true;
                }
            } else {
                let stored = if def.allow_multiple {
                    let mut list = OptionValue::List(Vec::new());
                    append_instance(&mut list, converted.clone());
                    list
                } else {
                    converted.clone()
                };
                self.parsed_options.insert(
                    def.name.to_string(),
                    OptionValueDescription::new(
                        def,
                        vec![raw_value.clone()],
                        stored,
                        priority.clone(),
                        source.to_string(),
                    )
                    .with_provenance(
                        expanded_from,
                        implicit_dependent,
                        Some(command_line_form.clone()),
                    ),
                );
                should_update_legacy = true;
            }

            if should_update_legacy {
                self.update_legacy_field(def.name, &converted, &raw_value);
            }

            // 8. Implicit requirements run AFTER the parent is stored, at child priority.
            if def.has_implicit_requirements() {
                if let Some(default_raw) = def.default_value {
                    if let Ok(default_val) = convert_option(default_raw, def) {
                        if converted == default_val {
                            self.warnings.push(format!(
                                "{command_line_form} sets option '--{}' to its default value. Since this option has implicit requirements that are ignored when it is set to its default value, this has no effect. Specifically, this options expands to {{{}}}.",
                                def.name,
                                def.implicit_requirements.join(" ")
                            ));
                        }
                    }
                }
                let child_p = priority.get_child_priority()?;
                let req_source = if source.is_empty() {
                    format!("implicit requirement of option '--{}'", def.name)
                } else {
                    format!(
                        "implicit requirement of option '--{}' (source {source})",
                        def.name
                    )
                };
                let req_vec: Vec<String> = def
                    .implicit_requirements
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect();
                let (sub_pre, sub_post) = self.parse_into(
                    command,
                    &req_vec,
                    child_p,
                    &req_source,
                    Some(def.name),
                    None,
                )?;
                if !sub_pre.is_empty() || !sub_post.is_empty() {
                    let mut rem = sub_pre;
                    rem.extend(sub_post);
                    return Err(OptionsParsingError::new(format!(
                        "Unparsed options remain after processing {raw_value}: {}",
                        rem.join(" ")
                    )));
                }
            }

            priority = priority.next();
            i += consumed;
        }

        Ok((pre_residue, post_residue))
    }

    /// Enforces an invocation policy onto these parsed options.
    pub fn apply_invocation_policy(
        &mut self,
        policy: &InvocationPolicy,
        command: &str,
    ) -> Result<(), OptionsParsingError> {
        let cmd_map = get_command_options_map();

        for flag_policy in &policy.flag_policies {
            if !InvocationPolicy::applies_to_command(&flag_policy.commands, command) {
                continue;
            }

            let Some(def) = cmd_map.get(flag_policy.flag_name.as_str()) else {
                // Silently ignore policies for flags unknown on this command
                continue;
            };

            let Some(ref op) = flag_policy.operation else {
                continue;
            };

            match op {
                PolicyOperation::SetValue(ref sv) => {
                    let should_apply = match sv.behavior {
                        SetValueBehavior::AllowOverrides => {
                            // Only set if not already set by user
                            !self.parsed_options.contains_key(def.name)
                        }
                        SetValueBehavior::Append => true,
                        SetValueBehavior::FinalValueIgnoreOverrides => true,
                        SetValueBehavior::FinalValueThrowOnOverride => {
                            if self.parsed_options.contains_key(def.name) {
                                let msg = flag_policy
                                    .custom_error_message
                                    .clone()
                                    .unwrap_or_else(|| {
                                        format!(
                                            "Flag '{}' is set by invocation policy and cannot be overridden",
                                            def.name
                                        )
                                    });
                                return Err(OptionsParsingError::new(msg));
                            }
                            true
                        }
                        SetValueBehavior::Undefined => true,
                    };

                    if should_apply {
                        for val_str in &sv.flag_values {
                            let converted = convert_option(val_str, def)?;
                            self.update_legacy_field(def.name, &converted, val_str);
                            let priority = OptionPriority::lowest_at_category(
                                PriorityCategory::InvocationPolicy,
                            );
                            self.parsed_options.insert(
                                def.name.to_string(),
                                OptionValueDescription::new(
                                    def,
                                    vec![val_str.clone()],
                                    converted,
                                    priority,
                                    "invocation policy".to_string(),
                                ),
                            );
                        }
                    }
                }
                PolicyOperation::UseDefault(_) => {
                    self.parsed_options.remove(def.name);
                }
                PolicyOperation::DisallowValues(ref dv) => {
                    self.apply_filter_policy(
                        def,
                        false,
                        &dv.disallowed_values,
                        dv.replacement.as_ref(),
                    )?;
                }
                PolicyOperation::AllowValues(ref av) => {
                    self.apply_filter_policy(
                        def,
                        true,
                        &av.allowed_values,
                        av.replacement.as_ref(),
                    )?;
                }
            }
        }

        Ok(())
    }

    /// `InvocationPolicyEnforcer.FilterValueOperation.apply` (allow_values / disallow_values).
    fn apply_filter_policy(
        &mut self,
        def: &'static OptionDefinition,
        is_allow: bool,
        policy_values: &[String],
        replacement: Option<&ReplacementValue>,
    ) -> Result<(), OptionsParsingError> {
        let policy_type = if is_allow { "Allow" } else { "Disallow" };
        let display_flag = format!("option '--{}'", def.name);
        let shown_policy_values = format!("[{}]", policy_values.join(", "));
        let mut converted_policy: Vec<String> = Vec::new();
        for v in policy_values {
            match convert_option(v, def)? {
                OptionValue::List(items) if def.allow_multiple => {
                    converted_policy.extend(items.iter().map(|i| i.to_string_repr()))
                }
                other => converted_policy.push(other.to_string_repr()),
            }
        }
        let allowed = |value: &str| converted_policy.iter().any(|c| c == value) == is_allow;
        let use_default = matches!(replacement, Some(ReplacementValue::UseDefault));
        let new_value = match replacement {
            Some(ReplacementValue::NewValue(v)) => Some(v.as_str()),
            _ => None,
        };
        let default_repr = def
            .default_value
            .and_then(|d| convert_option(d, def).ok())
            .map(|v| v.to_string_repr());
        if !def.allow_multiple {
            if let Some(d) = &default_repr {
                if !allowed(d) && use_default {
                    return Err(OptionsParsingError::new(format!(
                        "{policy_type}Values policy disallows the default value '{d}' for {display_flag} but also specifies to use the default value"
                    )));
                }
            }
        }
        let user_value = self.parsed_options.get(def.name).map(|d| d.value.clone());
        match user_value {
            None => {
                let Some(d) = default_repr else { return Ok(()) };
                if def.allow_multiple || allowed(&d) {
                    return Ok(());
                }
                match new_value {
                    Some(nv) => self.set_policy_value(def, nv)?,
                    None => {
                        return Err(OptionsParsingError::new(format!(
                            "Default flag value '{d}' for {display_flag} is not allowed by invocation policy, but the policy does not provide a new value. {policy_type}ed values are: {shown_policy_values}"
                        )))
                    }
                }
            }
            Some(OptionValue::List(items)) if def.allow_multiple => {
                for item in &items {
                    let repr = item.to_string_repr();
                    if !allowed(&repr) {
                        if use_default {
                            self.clear_policy_value(def);
                        } else {
                            return Err(OptionsParsingError::new(format!(
                                "Flag value '{repr}' for {display_flag} is not allowed by invocation policy. {policy_type}ed values are: {shown_policy_values}"
                            )));
                        }
                    }
                }
            }
            Some(value) => {
                let repr = value.to_string_repr();
                if !allowed(&repr) {
                    if let Some(nv) = new_value {
                        self.set_policy_value(def, nv)?;
                    } else if use_default {
                        self.clear_policy_value(def);
                    } else {
                        return Err(OptionsParsingError::new(format!(
                            "Flag value '{repr}' for {display_flag} is not allowed by invocation policy and the policy does not specify a new value. {policy_type}ed values are: {shown_policy_values}"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn clear_policy_value(&mut self, def: &OptionDefinition) {
        self.parsed_options.remove(def.name);
        let prefix = format!("--{}=", def.name);
        self.canonical_flags.retain(|f| !f.starts_with(&prefix));
    }

    /// `parser.clearValue` + `setOptionValueAtSpecificPriorityWithoutExpansion` for an invocation policy.
    fn set_policy_value(
        &mut self,
        def: &'static OptionDefinition,
        raw: &str,
    ) -> Result<(), OptionsParsingError> {
        let converted = convert_option(raw, def)?;
        self.clear_policy_value(def);
        self.update_legacy_field(def.name, &converted, raw);
        let canonical = if def.option_type == OptionType::Boolean {
            if converted.as_bool().unwrap_or(true) {
                "1".to_string()
            } else {
                "0".to_string()
            }
        } else {
            raw.to_string()
        };
        self.canonical_flags
            .push(format!("--{}={canonical}", def.name));
        let stored = if def.allow_multiple {
            OptionValue::List(vec![converted])
        } else {
            converted
        };
        self.parsed_options.insert(
            def.name.to_string(),
            OptionValueDescription::new(
                def,
                vec![raw.to_string()],
                stored,
                OptionPriority::lowest_at_category(PriorityCategory::InvocationPolicy),
                "invocation policy".to_string(),
            ),
        );
        Ok(())
    }

    fn update_legacy_field(&mut self, name: &str, val: &OptionValue, raw_val: &str) {
        match name {
            "compilation_mode" => {
                if let Some(s) = val.as_str() {
                    self.compilation_mode = s.to_string();
                }
            }
            "jobs" => {
                if let Some(i) = val.as_int() {
                    self.jobs = i.max(1) as usize;
                }
            }
            "keep_going" => {
                if let Some(b) = val.as_bool() {
                    self.keep_going = b;
                }
            }
            "subcommands" => {
                // `ShowSubcommands`: `true` and `pretty_print` both announce subcommands.
                if let Some(b) = val.as_bool() {
                    self.subcommands = b;
                } else if let Some(s) = val.as_str() {
                    self.subcommands = s != "false";
                }
            }
            "verbose_failures" => {
                if let Some(b) = val.as_bool() {
                    self.verbose_failures = b;
                }
            }
            "expunge" => {
                if let Some(b) = val.as_bool() {
                    self.expunge = b;
                }
            }
            "expunge_async" => {
                if let Some(b) = val.as_bool() {
                    self.expunge_async = b;
                }
            }
            "output" => {
                if let Some(s) = val.as_str() {
                    self.query_output_format = s.to_string();
                }
            }
            "show_make_env" => {
                if let Some(b) = val.as_bool() {
                    self.show_make_env = b;
                }
            }
            "disk_cache" => {
                if !raw_val.is_empty() {
                    self.disk_cache = Some(PathFragment::create(raw_val));
                }
            }
            "profile" => {
                if !raw_val.is_empty() {
                    self.profile = Some(PathFragment::create(raw_val));
                }
            }
            "build_event_json_file" => {
                if !raw_val.is_empty() {
                    self.build_event_json_file = Some(PathFragment::create(raw_val));
                }
            }
            "test_output" => {
                if let Some(s) = val.as_str() {
                    self.test_output = s.to_string();
                }
            }
            "test_filter" => {
                self.test_filter = Some(raw_val.to_string());
            }
            "spawn_strategy" => {
                if let Some(s) = val.as_str() {
                    self.spawn_strategy = s.to_string();
                }
            }
            _ => {}
        }
    }

    pub fn get_bool(&self, name: &str) -> Option<bool> {
        self.parsed_options
            .get(name)
            .and_then(|desc| desc.value.as_bool())
    }

    pub fn get_str(&self, name: &str) -> Option<&str> {
        self.parsed_options
            .get(name)
            .and_then(|desc| desc.value.as_str())
    }

    pub fn get_int(&self, name: &str) -> Option<i32> {
        self.parsed_options
            .get(name)
            .and_then(|desc| desc.value.as_int())
    }
}

/// Appends one occurrence of an `allowMultiple` option to its accumulated value. List-valued
/// converters (`--features=a,b`) contribute their elements, others contribute one element.
fn append_instance(acc: &mut OptionValue, instance: OptionValue) {
    if let OptionValue::List(list) = acc {
        match instance {
            OptionValue::List(items) => list.extend(items),
            other => list.push(other),
        }
    }
}

/// `OptionsParserImpl.identifyOptionAndPossibleArgument` syntax checks that precede the lookup.
fn check_arg_syntax(arg: &str) -> Result<(), OptionsParsingError> {
    let len = arg.chars().count();
    let ok = if (len == 2 && arg.starts_with('-'))
        || (len == 3 && arg.starts_with('-') && arg.ends_with('-'))
    {
        true
    } else if let Some(body) = arg.strip_prefix("--") {
        let name = body.split('=').next().unwrap_or("");
        !name.trim().is_empty()
    } else {
        false
    };
    // `OptionsParserImpl`: an invalid option that looks like an absolute negative target pattern or
    // a mistyped Starlark flag gets a hint (checked before the generic syntax error).
    if (arg.starts_with("-//") || arg.starts_with("-@")) && !arg.starts_with("--") {
        // Thrown without an option argument (`new OptionsParsingException(message)`): no `<arg> :: ` prefix.
        return Err(OptionsParsingError::new(format!(
            "Invalid options syntax: {arg}\nNote: Negative target patterns can only appear after the end of options marker ('--'). Flags corresponding to Starlark-defined build settings always start with '--', not '-'."
        )));
    }
    if ok || !arg.starts_with('-') {
        Ok(())
    } else {
        Err(OptionsParsingError::with_failed_arg(
            arg,
            format!("Invalid options syntax: {arg}"),
        ))
    }
}

/// Resolves an argument token into `(lookup_name, inline_val, is_negated_bool, is_abbrev)`.
fn resolve_arg_shape(
    arg: &str,
    cmd_map: &std::collections::HashMap<&'static str, &'static OptionDefinition>,
    abbrev_map: &std::collections::HashMap<char, &'static OptionDefinition>,
    aliases: &BTreeMap<String, String>,
) -> (String, Option<String>, bool, bool) {
    if let Some((k, v)) = arg.strip_prefix("--").and_then(|s| s.split_once('=')) {
        return (k.to_string(), Some(v.to_string()), false, false);
    }

    if let Some(naked) = arg.strip_prefix("--") {
        if cmd_map.contains_key(naked) || aliases.contains_key(naked) {
            return (naked.to_string(), None, false, false);
        }
        if let Some(no_naked) = naked.strip_prefix("no") {
            if let Some(def) = cmd_map.get(no_naked) {
                if def.uses_boolean_syntax() {
                    return (no_naked.to_string(), Some("false".to_string()), true, false);
                }
            }
            if aliases.contains_key(no_naked) {
                return (no_naked.to_string(), Some("false".to_string()), true, false);
            }
        }
        return (naked.to_string(), None, false, false);
    }

    // Single dash abbreviation: e.g. `-c`, `-k`, `-k-`
    if let Some(rest) = arg.strip_prefix('-') {
        if rest.len() == 1 {
            let ch = rest.chars().next().unwrap();
            if let Some(def) = abbrev_map.get(&ch) {
                return (def.name.to_string(), None, false, true);
            }
        } else if rest.len() == 2 && rest.ends_with('-') {
            let ch = rest.chars().next().unwrap();
            if let Some(def) = abbrev_map.get(&ch) {
                if def.uses_boolean_syntax() {
                    return (def.name.to_string(), Some("false".to_string()), true, true);
                }
            }
        }
    }

    (arg.to_string(), None, false, false)
}

/// `OptionsParser.parseWithSourceFunction(..., fallbackData)` for the `common` rc pseudo-command: options that
/// exist for *some* command but are not part of `command`'s option set are skipped instead of being an error
/// (together with the separate value token of value-taking options). Everything else — including options no
/// command knows about — is kept so that it is diagnosed by the regular parse. Returns `(kept, ignored)`.
pub fn split_unsupported_common_args(command: &str, args: &[String]) -> (Vec<String>, Vec<String>) {
    let all = get_command_options_map();
    let abbrevs = get_command_abbrev_map();
    let no_aliases = BTreeMap::new();
    let mut kept = Vec::new();
    let mut ignored = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            kept.extend_from_slice(&args[i..]);
            break;
        }
        if !arg.starts_with('-')
            || arg.starts_with("--//")
            || arg.starts_with("--@")
            || arg.starts_with("--no//")
            || arg.starts_with("--no@")
        {
            kept.push(arg.clone());
            i += 1;
            continue;
        }
        let (name, inline, _negated, _abbrev) = resolve_arg_shape(arg, &all, &abbrevs, &no_aliases);
        let cmd_def = ALL_COMMAND_OPTIONS.iter().find(|d| {
            (d.name == name || d.old_name == Some(name.as_str())) && is_command_allowed(command, d)
        });
        match (cmd_def, all.get(name.as_str())) {
            (Some(def), _) => {
                kept.push(arg.clone());
                i += 1;
                if inline.is_none() && def.requires_value() {
                    if let Some(v) = args.get(i) {
                        kept.push(v.clone());
                        i += 1;
                    }
                }
            }
            (None, Some(fallback_def)) => {
                ignored.push(arg.clone());
                i += 1;
                if inline.is_none() && fallback_def.requires_value() {
                    if let Some(v) = args.get(i) {
                        ignored.push(v.clone());
                        i += 1;
                    }
                }
            }
            (None, None) => {
                kept.push(arg.clone());
                i += 1;
            }
        }
    }
    (kept, ignored)
}

fn get_valid_option_names<'a>(
    command: &str,
    _allowed_classes: Option<&HashSet<&'static str>>,
    storage: &'a mut Vec<String>,
) -> Vec<&'a str> {
    for def in ALL_COMMAND_OPTIONS {
        if !def.commands.is_empty() && !is_command_allowed(command, def) {
            continue;
        }
        storage.push(format!("--{}", def.name));
        if def.uses_boolean_syntax() {
            storage.push(format!("--no{}", def.name));
        }
    }
    storage.iter().map(|s| s.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unknown_flag_fails() {
        let args = vec!["--this_flag_does_not_exist_at_all".to_string()];
        let err = ParsedCommandOptions::parse_for_command("build", &args).unwrap_err();
        assert!(err
            .message
            .contains("Unrecognized option: --this_flag_does_not_exist_at_all"));
        assert_eq!(
            err.format_bazel_error(),
            "ERROR: --this_flag_does_not_exist_at_all :: Unrecognized option: --this_flag_does_not_exist_at_all"
        );
    }

    #[test]
    fn test_unknown_flag_suggestion_matches_bazel() {
        // Bazel: SpellChecker.didYouMean(arg, ...) with the full argument.
        let args = vec!["--keep_goin".to_string()];
        let err = ParsedCommandOptions::parse_for_command("build", &args).unwrap_err();
        assert_eq!(
            err.format_bazel_error(),
            "ERROR: --keep_goin :: Unrecognized option: --keep_goin (did you mean '--keep_going'?)"
        );
        // With `=value` appended the edit distance exceeds the cutoff, so Bazel gives no hint.
        let args = vec!["--keep_goin=true".to_string()];
        let err = ParsedCommandOptions::parse_for_command("build", &args).unwrap_err();
        assert_eq!(
            err.format_bazel_error(),
            "ERROR: --keep_goin=true :: Unrecognized option: --keep_goin=true"
        );
    }

    #[test]
    fn test_residue_and_target_loss_fix() {
        let args = vec!["--".to_string(), "//foo:bar".to_string()];
        let parsed = ParsedCommandOptions::parse_for_command("build", &args).unwrap();
        assert_eq!(parsed.targets, vec!["//foo:bar"]);
    }

    #[test]
    fn test_boolean_flag_variants() {
        let args = vec!["--keep_going=1".to_string()];
        let parsed = ParsedCommandOptions::parse_for_command("build", &args).unwrap();
        assert!(parsed.keep_going);

        let args2 = vec!["--keep_going=false".to_string()];
        let parsed2 = ParsedCommandOptions::parse_for_command("build", &args2).unwrap();
        assert!(!parsed2.keep_going);

        let args3 = vec!["-k".to_string()];
        let parsed3 = ParsedCommandOptions::parse_for_command("build", &args3).unwrap();
        assert!(parsed3.keep_going);

        let args4 = vec!["-k-".to_string()];
        let parsed4 = ParsedCommandOptions::parse_for_command("build", &args4).unwrap();
        assert!(!parsed4.keep_going);
    }

    #[test]
    fn test_starlark_flag_parsing() {
        let args = vec![
            "--//tools:flag=custom".to_string(),
            "--@myrepo//cc:opt".to_string(),
        ];
        let parsed = ParsedCommandOptions::parse_for_command("build", &args).unwrap();
        assert_eq!(
            parsed.starlark_options.get("//tools:flag").unwrap(),
            "custom"
        );
        assert_eq!(
            parsed.starlark_options.get("@myrepo//cc:opt").unwrap(),
            "true"
        );
    }

    #[test]
    fn test_flag_alias() {
        let args = vec![
            "--flag_alias=myflag=//pkg:target".to_string(),
            "--myflag=hello".to_string(),
        ];
        let parsed = ParsedCommandOptions::parse_for_command("build", &args).unwrap();
        assert_eq!(
            parsed.starlark_options.get("//pkg:target").unwrap(),
            "hello"
        );
    }

    #[test]
    fn test_expansion_priority_and_no_self_value() {
        // `--remote_download_minimal` expands to `--remote_download_outputs=minimal`.
        // Explicit `--remote_download_outputs=all` AFTER it wins; BEFORE it is overridden with a warning.
        let p1 = ParsedCommandOptions::parse_for_command(
            "build",
            &[
                "--remote_download_outputs=all".to_string(),
                "--remote_download_minimal".to_string(),
            ],
        )
        .unwrap();
        assert_eq!(p1.get_str("remote_download_outputs"), Some("minimal"));
        assert!(!p1.parsed_options.contains_key("remote_download_minimal"));
        assert!(!p1
            .canonical_flags
            .iter()
            .any(|f| f.contains("remote_download_minimal")));
        assert!(p1
            .warnings
            .iter()
            .any(|w| w.contains("was expanded and now overrides")));

        let p2 = ParsedCommandOptions::parse_for_command(
            "build",
            &[
                "--remote_download_minimal".to_string(),
                "--remote_download_outputs=all".to_string(),
            ],
        )
        .unwrap();
        assert_eq!(p2.get_str("remote_download_outputs"), Some("all"));
    }

    #[test]
    fn test_expansion_with_value_warns_and_ignores() {
        let p = ParsedCommandOptions::parse_for_command(
            "build",
            &["--remote_download_minimal=ignored_val".to_string()],
        )
        .unwrap();
        assert_eq!(p.get_str("remote_download_outputs"), Some("minimal"));
        assert!(
            p.warnings
                .iter()
                .any(|w| w.contains("is an expansion option") && w.contains("ignored_val")),
            "{:?}",
            p.warnings
        );
    }

    #[test]
    fn test_explicit_starlark_options_and_residue() {
        let p = ParsedCommandOptions::parse_for_command(
            "build",
            &[
                "--//a:x=1".to_string(),
                "--no//a:y".to_string(),
                "a".to_string(),
                "b".to_string(),
                "--".to_string(),
                "b".to_string(),
                "c".to_string(),
            ],
        )
        .unwrap();
        let exp = p.get_explicit_starlark_options();
        assert_eq!(exp.get("//a:x").map(String::as_str), Some("1"));
        assert!(!exp.contains_key("//a:y"));
        assert_eq!(p.get_pre_double_dash_residue(), vec!["a"]);
        assert_eq!(p.get_post_double_dash_residue(), &["b", "c"]);
    }

    #[test]
    fn test_empty_test_filter_clears_earlier_filter_to_empty_some() {
        let p = ParsedCommandOptions::parse_for_command(
            "test",
            &[
                "--test_filter=Foo".to_string(),
                "--test_filter=".to_string(),
            ],
        )
        .unwrap();
        assert_eq!(p.test_filter.as_deref(), Some(""));
    }
}
