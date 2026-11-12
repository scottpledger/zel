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
    pub flag_aliases: BTreeMap<String, String>,
    pub parsed_options: BTreeMap<String, OptionValueDescription>,
    /// `Option '<old>' is deprecated: Use --<name> instead` for options given by their `oldName`.
    pub old_name_warnings: Vec<String>,
}

/// `--jobs` default (`auto` = `HOST_CPUS`, capped at `MAX_JOBS`); kept as a plain number for the
/// legacy `jobs` field. The authoritative evaluation of the raw flag string is `zel-exec`'s.
fn default_jobs() -> usize {
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
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
            flag_aliases: BTreeMap::new(),
            parsed_options: BTreeMap::new(),
            old_name_warnings: Vec::new(),
        }
    }
}

impl ParsedCommandOptions {
    /// Default parse without command context (defaults to "build").
    pub fn parse(args: &[String]) -> Result<Self, OptionsParsingError> {
        Self::parse_for_command("build", args)
    }

    /// Parses command-line arguments for a specific command (e.g. "build", "test", "run", "query").
    pub fn parse_for_command(
        command: &str,
        args: &[String],
    ) -> Result<Self, OptionsParsingError> {
        Self::parse_internal(command, args, PriorityCategory::CommandLine)
    }

    /// Parses options with a specific initial priority category.
    pub fn parse_with_priority(
        command: &str,
        args: &[String],
        priority: PriorityCategory,
    ) -> Result<Self, OptionsParsingError> {
        Self::parse_internal(command, args, priority)
    }

    fn parse_internal(
        command: &str,
        args: &[String],
        initial_priority: PriorityCategory,
    ) -> Result<Self, OptionsParsingError> {
        let mut opts = Self::default();
        let cmd_map = get_command_options_map();
        let abbrev_map = get_command_abbrev_map();
        let allowed_classes = get_command_allowed_classes(command);

        let mut priority = OptionPriority::lowest_at_category(initial_priority);
        let mut i = 0;

        while i < args.len() {
            let arg = &args[i];

            // 1. Double dash `--` ends options parsing
            if arg == "--" {
                let residue = &args[i + 1..];
                if command == "run" {
                    if opts.targets.is_empty() && !residue.is_empty() {
                        opts.targets.push(residue[0].clone());
                        opts.passthrough_args.extend_from_slice(&residue[1..]);
                    } else {
                        opts.passthrough_args.extend_from_slice(residue);
                    }
                } else {
                    // For build, test, query, etc.: all post-`--` tokens are targets/queries!
                    opts.targets.extend_from_slice(residue);
                }
                break;
            }

            // 2. Non-flag argument is a target / query argument
            if !arg.starts_with('-') {
                opts.targets.push(arg.clone());
                i += 1;
                continue;
            }

            // 3. Starlark flags: `--//pkg:flag`, `--@repo//pkg:flag`, `--no//...`, `--no@...`
            if arg.starts_with("--//") || arg.starts_with("--@") {
                let stripped = &arg[2..];
                if let Some((k, v)) = stripped.split_once('=') {
                    opts.starlark_options.insert(k.to_string(), v.to_string());
                    opts.starlark_occurrences.push((k.to_string(), v.to_string()));
                    opts.canonical_flags.push(format!("--{k}={v}"));
                } else {
                    opts.starlark_options.insert(stripped.to_string(), "true".to_string());
                    opts.starlark_occurrences.push((stripped.to_string(), "true".to_string()));
                    opts.canonical_flags.push(format!("--{stripped}=true"));
                }
                i += 1;
                continue;
            }
            if arg.starts_with("--no//") || arg.starts_with("--no@") {
                let stripped = &arg[4..];
                opts.starlark_options.insert(stripped.to_string(), "false".to_string());
                opts.starlark_occurrences.push((stripped.to_string(), "false".to_string()));
                opts.canonical_flags.push(format!("--{stripped}=false"));
                i += 1;
                continue;
            }

            // `OptionsParserImpl.identifyOptionAndPossibleArgument`: only `-x`, `-x-` and
            // `--long[=value]` are valid shapes.
            check_arg_syntax(arg)?;

            // 4. Identify OptionDefinition (long flag, boolean `no` prefix, or abbrev)
            let (lookup_name, inline_value, is_negated_bool, _is_abbrev) =
                resolve_arg_shape(arg, &cmd_map, &abbrev_map, &opts.flag_aliases);

            // 5. Check if it's a flag alias (e.g. `--myalias=val` or `--myalias`)
            if let Some(target_label) = opts.flag_aliases.get(&lookup_name).cloned() {
                let val_str = if is_negated_bool {
                    "false"
                } else if let Some(ref v) = inline_value {
                    v.as_str()
                } else {
                    "true"
                };
                opts.starlark_options.insert(target_label.clone(), val_str.to_string());
                opts.starlark_occurrences.push((target_label.clone(), val_str.to_string()));
                opts.canonical_flags.push(format!("--{target_label}={val_str}"));
                i += 1;
                continue;
            }

            // `--no<name>` on a known non-boolean option, or `--no<name>=value`.
            if cmd_map.get(lookup_name.as_str()).is_none() && arg.starts_with("--") {
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
                let valid_names = get_valid_option_names(command, allowed_classes.as_ref(), &mut storage);
                // Bazel (OptionsParserImpl.java:804) passes the *whole* argument (including any
                // `=value`) to SpellChecker.didYouMean, so we do the same.
                let flag_token = arg;
                let suggestion = if arg.starts_with("--") {
                    did_you_mean(flag_token, valid_names)
                } else {
                    String::new()
                };
                return Err(OptionsParsingError::unrecognized_option(arg, &suggestion));
            };

            let mut def = *def;
            if !def.commands.is_empty() && !is_command_allowed(command, &def) {
                if let Some(matching) = ALL_COMMAND_OPTIONS.iter().find(|d| d.name == lookup_name && is_command_allowed(command, d)) {
                    def = matching;
                }
            }

            if def.old_name == Some(lookup_name.as_str()) && def.name != lookup_name.as_str() {
                opts.old_name_warnings.push(format!("Option '{}' is deprecated: Use --{} instead", lookup_name, def.name));
            }

            // Check if option is allowed on the current command
            if !def.commands.is_empty() && !is_command_allowed(command, &def) {
                let mut storage = Vec::new();
                let valid_names = get_valid_option_names(command, allowed_classes.as_ref(), &mut storage);
                // Bazel (OptionsParserImpl.java:804) passes the *whole* argument (including any
                // `=value`) to SpellChecker.didYouMean, so we do the same.
                let flag_token = arg;
                let suggestion = if arg.starts_with("--") {
                    did_you_mean(flag_token, valid_names)
                } else {
                    String::new()
                };
                return Err(OptionsParsingError::unrecognized_option(arg, &suggestion));
            }

            // 6. Handle flag value extraction
            let (raw_value, consumed) = if is_negated_bool {
                ("false".to_string(), 1)
            } else if let Some(ref v) = inline_value {
                (v.clone(), 1)
            } else if def.uses_boolean_syntax() {
                ("true".to_string(), 1)
            } else if def.option_type == OptionType::Void {
                ("".to_string(), 1)
            } else {
                // Takes next arg as value
                if i + 1 >= args.len() {
                    return Err(OptionsParsingError::new(format!("Expected value after {arg}")));
                }
                (args[i + 1].clone(), 2)
            };
            // `ParsedOptionDescription.commandLineForm`: the arg, plus the separate value arg.
            let command_line_form = if consumed == 2 { format!("{arg} {raw_value}") } else { arg.clone() };

            // 7. Handle expansion options
            if let Some(expansion_args) = def.expansion {
                let mut child_p = priority.get_child_priority().unwrap_or_else(|_| priority.clone());
                for exp_arg in expansion_args {
                    let sub_args = vec![exp_arg.to_string()];
                    let sub_opts = Self::parse_internal(command, &sub_args, child_p.category)?;
                    opts.merge(sub_opts);
                    child_p = child_p.next();
                }
            }

            // 8. Handle implicit requirements
            for req in def.implicit_requirements {
                let sub_args = vec![req.to_string()];
                let sub_opts = Self::parse_internal(command, &sub_args, PriorityCategory::ComputedDefault)?;
                opts.merge(sub_opts);
            }

            // 9. Convert and apply value
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
                // `ParsedOptionDescription.getConvertedValue` adds the option context.
                OptionsParsingError::new(format!("While parsing option {command_line_form}: {}", e.message))
            })?;

            // Check for special flag alias registrations: `--flag_alias=name=label`
            if def.name == "flag_alias" {
                if let Ok((alias, label)) = convert_flag_alias(&raw_value) {
                    opts.flag_aliases.insert(alias, label);
                }
            }

            // Check for defines: `--define key=val`
            if def.name == "define" {
                match converted {
                    OptionValue::Entry(ref k, ref v) => {
                        if let Some(v) = v.as_str() {
                            opts.user_defines.insert(k.clone(), v.to_string());
                        }
                    }
                    OptionValue::Map(ref m) => {
                        for (k, v) in m {
                            opts.user_defines.insert(k.clone(), v.clone());
                        }
                    }
                    _ => {}
                }
            }

            // Update legacy fields
            opts.update_legacy_field(def.name, &converted, &raw_value);

            // Record into canonical flags (`ParsedOptionDescription.getDeprecatedCanonicalForm`):
            // plain `boolean` options use `--flag=1` / `--flag=0`;
            // everything else (TriState, BoolOrEnum, ...) is `--flag=<unconverted value>`.
            if def.option_type == OptionType::Boolean {
                let b_val = if is_negated_bool {
                    false
                } else {
                    converted.as_bool().unwrap_or(true)
                };
                opts.canonical_flags.push(format!("--{}={}", def.name, if b_val { "1" } else { "0" }));
            } else if def.option_type != OptionType::Void {
                let canonical_value = if is_negated_bool {
                    "0".to_string()
                } else if inline_value.is_none() && def.uses_boolean_syntax() {
                    "1".to_string()
                } else {
                    raw_value.clone()
                };
                opts.canonical_flags.push(format!("--{}={canonical_value}", def.name));
            }

            // Store into parsed options map with priority
            if let Some(existing) = opts.parsed_options.get_mut(def.name) {
                if def.allow_multiple {
                    existing.raw_values.push(raw_value);
                    append_instance(&mut existing.value, converted);
                } else if priority >= existing.priority {
                    existing.raw_values = vec![raw_value];
                    existing.value = converted;
                    existing.priority = priority.clone();
                    existing.source = "command line".to_string();
                }
            } else {
                let converted = if def.allow_multiple {
                    let mut list = OptionValue::List(Vec::new());
                    append_instance(&mut list, converted);
                    list
                } else {
                    converted
                };
                opts.parsed_options.insert(
                    def.name.to_string(),
                    OptionValueDescription::new(
                        def,
                        vec![raw_value],
                        converted,
                        priority.clone(),
                        "command line".to_string(),
                    ),
                );
            }

            priority = priority.next();
            i += consumed;
        }

        // Apply invocation policy if --invocation_policy was set. `canonicalize-flags` declares
        // `--invocation_policy` as its own option (`CanonicalizeCommand.Options`) and only applies it to
        // the flags it canonicalizes, never to its own options.
        if let Some(policy_val) = opts.get_str("invocation_policy").filter(|_| command != "canonicalize-flags") {
            if !policy_val.is_empty() {
                if let Ok(policy) = InvocationPolicy::parse(policy_val) {
                    opts.apply_invocation_policy(&policy, command)?;
                }
            }
        }

        Ok(opts)
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
                            let priority = OptionPriority::lowest_at_category(PriorityCategory::InvocationPolicy);
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
                    self.apply_filter_policy(def, false, &dv.disallowed_values, dv.replacement.as_ref())?;
                }
                PolicyOperation::AllowValues(ref av) => {
                    self.apply_filter_policy(def, true, &av.allowed_values, av.replacement.as_ref())?;
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
    fn set_policy_value(&mut self, def: &'static OptionDefinition, raw: &str) -> Result<(), OptionsParsingError> {
        let converted = convert_option(raw, def)?;
        self.clear_policy_value(def);
        self.update_legacy_field(def.name, &converted, raw);
        let canonical = if def.option_type == OptionType::Boolean {
            if converted.as_bool().unwrap_or(true) { "1".to_string() } else { "0".to_string() }
        } else {
            raw.to_string()
        };
        self.canonical_flags.push(format!("--{}={canonical}", def.name));
        let stored = if def.allow_multiple { OptionValue::List(vec![converted]) } else { converted };
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

    fn merge(&mut self, other: ParsedCommandOptions) {
        self.targets.extend(other.targets);
        self.passthrough_args.extend(other.passthrough_args);
        self.canonical_flags.extend(other.canonical_flags);
        self.starlark_options.extend(other.starlark_options);
        self.starlark_occurrences.extend(other.starlark_occurrences);
        self.flag_aliases.extend(other.flag_aliases);
        self.user_defines.extend(other.user_defines);

        for (k, v) in other.parsed_options {
            self.parsed_options.insert(k, v);
        }
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
                if !raw_val.is_empty() {
                    self.test_filter = Some(raw_val.to_string());
                }
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
        self.parsed_options.get(name).and_then(|desc| desc.value.as_bool())
    }

    pub fn get_str(&self, name: &str) -> Option<&str> {
        self.parsed_options.get(name).and_then(|desc| desc.value.as_str())
    }

    pub fn get_int(&self, name: &str) -> Option<i32> {
        self.parsed_options.get(name).and_then(|desc| desc.value.as_int())
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
    let ok = if len == 2 && arg.starts_with('-') {
        true
    } else if len == 3 && arg.starts_with('-') && arg.ends_with('-') {
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
        Err(OptionsParsingError::with_failed_arg(arg, format!("Invalid options syntax: {arg}")))
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
    let supported = |name: &str| {
        ALL_COMMAND_OPTIONS.iter().any(|d| (d.name == name || d.old_name == Some(name)) && is_command_allowed(command, d))
    };
    let mut kept = Vec::new();
    let mut ignored = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            kept.extend_from_slice(&args[i..]);
            break;
        }
        if !arg.starts_with('-') || arg.starts_with("--//") || arg.starts_with("--@") || arg.starts_with("--no//") || arg.starts_with("--no@") {
            kept.push(arg.clone());
            i += 1;
            continue;
        }
        let (name, inline, _negated, _abbrev) = resolve_arg_shape(arg, &all, &abbrevs, &no_aliases);
        match all.get(name.as_str()) {
            Some(def) if !supported(&name) => {
                ignored.push(arg.clone());
                i += 1;
                if inline.is_none() && def.requires_value() {
                    if let Some(v) = args.get(i) {
                        ignored.push(v.clone());
                        i += 1;
                    }
                }
            }
            _ => {
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
        assert!(err.message.contains("Unrecognized option: --this_flag_does_not_exist_at_all"));
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
        let args = vec!["--//tools:flag=custom".to_string(), "--@myrepo//cc:opt".to_string()];
        let parsed = ParsedCommandOptions::parse_for_command("build", &args).unwrap();
        assert_eq!(parsed.starlark_options.get("//tools:flag").unwrap(), "custom");
        assert_eq!(parsed.starlark_options.get("@myrepo//cc:opt").unwrap(), "true");
    }

    #[test]
    fn test_flag_alias() {
        let args = vec![
            "--flag_alias=myflag=//pkg:target".to_string(),
            "--myflag=hello".to_string(),
        ];
        let parsed = ParsedCommandOptions::parse_for_command("build", &args).unwrap();
        assert_eq!(parsed.starlark_options.get("//pkg:target").unwrap(), "hello");
    }
}
