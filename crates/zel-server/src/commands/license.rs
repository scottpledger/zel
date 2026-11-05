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

//! `license` (`LicenseCommand`) and `canonicalize-flags` (`CanonicalizeCommand`).

use super::COMMANDS;
use crate::{writeln_bytes, BlazeRuntime};
use zel_options::invocation_policy::{
    AllowValues, DisallowValues, FlagPolicy, InvocationPolicy, PolicyOperation, ReplacementValue,
    SetValueBehavior,
};
use zel_util::ExitCode;

const LICENSE_TEXT: &str = include_str!("../../resources/LICENSE");

fn quote(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            _ => o.push(c),
        }
    }
    o.push('"');
    o
}

fn single_flag_policy_text(f: &FlagPolicy) -> String {
    let mut o = format!("flag_name: {}\n", quote(&f.flag_name));
    for c in &f.commands {
        o.push_str(&format!("commands: {}\n", quote(c)));
    }
    match &f.operation {
        Some(PolicyOperation::SetValue(s)) => {
            o.push_str("set_value {\n");
            for v in &s.flag_values {
                o.push_str(&format!("  flag_value: {}\n", quote(v)));
            }
            match s.behavior {
                SetValueBehavior::Undefined if s.behavior_set => o.push_str("  behavior: UNDEFINED\n"),
                SetValueBehavior::Undefined => {}
                SetValueBehavior::AllowOverrides => o.push_str("  behavior: ALLOW_OVERRIDES\n"),
                SetValueBehavior::Append => o.push_str("  behavior: APPEND\n"),
                SetValueBehavior::FinalValueIgnoreOverrides => {
                    o.push_str("  behavior: FINAL_VALUE_IGNORE_OVERRIDES\n")
                }
                SetValueBehavior::FinalValueThrowOnOverride => {
                    o.push_str("  behavior: FINAL_VALUE_THROW_ON_OVERRIDE\n")
                }
            }
            o.push('}');
        }
        _ => {}
    }
    o
}

/// Protobuf text format of an `InvocationPolicy` (`InvocationPolicy.toString()`).
fn policy_text(p: &InvocationPolicy) -> String {
    let mut o = String::new();
    for f in &p.flag_policies {
        o.push_str("flag_policies {\n");
        o.push_str(&format!("  flag_name: {}\n", quote(&f.flag_name)));
        for c in &f.commands {
            o.push_str(&format!("  commands: {}\n", quote(c)));
        }
        match &f.operation {
            Some(PolicyOperation::SetValue(s)) => {
                o.push_str("  set_value {\n");
                for v in &s.flag_values {
                    o.push_str(&format!("    flag_value: {}\n", quote(v)));
                }
                let b = match s.behavior {
                    SetValueBehavior::Undefined if s.behavior_set => Some("UNDEFINED"),
                    SetValueBehavior::Undefined => None,
                    SetValueBehavior::AllowOverrides => Some("ALLOW_OVERRIDES"),
                    SetValueBehavior::Append => Some("APPEND"),
                    SetValueBehavior::FinalValueIgnoreOverrides => Some("FINAL_VALUE_IGNORE_OVERRIDES"),
                    SetValueBehavior::FinalValueThrowOnOverride => Some("FINAL_VALUE_THROW_ON_OVERRIDE"),
                };
                if let Some(b) = b {
                    o.push_str(&format!("    behavior: {b}\n"));
                }
                o.push_str("  }\n");
            }
            Some(PolicyOperation::UseDefault(_)) => o.push_str("  use_default {\n  }\n"),
            Some(PolicyOperation::DisallowValues(DisallowValues { disallowed_values, replacement })) => {
                o.push_str("  disallow_values {\n");
                for v in disallowed_values {
                    o.push_str(&format!("    disallowed_values: {}\n", quote(v)));
                }
                push_replacement(&mut o, replacement, "new_value", "use_default");
                o.push_str("  }\n");
            }
            Some(PolicyOperation::AllowValues(AllowValues { allowed_values, replacement })) => {
                o.push_str("  allow_values {\n");
                for v in allowed_values {
                    o.push_str(&format!("    allowed_values: {}\n", quote(v)));
                }
                push_replacement(&mut o, replacement, "new_value", "use_default");
                o.push_str("  }\n");
            }
            None => {}
        }
        if let Some(m) = &f.custom_error_message {
            o.push_str(&format!("  custom_error_message: {}\n", quote(m)));
        }
        o.push_str("}\n");
    }
    o
}

fn push_replacement(o: &mut String, r: &Option<ReplacementValue>, nv: &str, ud: &str) {
    match r {
        Some(ReplacementValue::NewValue(v)) => o.push_str(&format!("    {nv}: {}\n", quote(v))),
        Some(ReplacementValue::UseDefault) => o.push_str(&format!("    {ud} {{\n    }}\n")),
        None => {}
    }
}

/// `InvocationPolicyEnforcer.getEffectiveInvocationPolicy`: policies applicable to `command` on flags
/// the command knows, one per flag (the last value, at the position of the first occurrence).
fn policy_for_command(p: &InvocationPolicy, command: &str) -> InvocationPolicy {
    let defs = super::help::command_options(command);
    let mut flag_policies: Vec<FlagPolicy> = Vec::new();
    for f in p.flag_policies.iter() {
        if !InvocationPolicy::applies_to_command(&f.commands, command) {
            continue;
        }
        if f.flag_name != "config" && !defs.iter().any(|d| d.name == f.flag_name) {
            continue;
        }
        match flag_policies.iter_mut().find(|e| e.flag_name == f.flag_name) {
            Some(e) => *e = f.clone(),
            None => flag_policies.push(f.clone()),
        }
    }
    InvocationPolicy { flag_policies, config_policies: p.config_policies.clone() }
}

fn walk_license_files(dir: &std::path::Path, file_names: &[&str], stdout: &mut Vec<u8>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut sorted_entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        sorted_entries.sort_by_key(|e| e.file_name());
        for entry in sorted_entries {
            let path = entry.path();
            if path.is_dir() {
                walk_license_files(&path, file_names, stdout);
            } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if file_names.contains(&name) {
                    writeln_bytes(stdout, &format!("{}:\n", path.display()));
                    if let Ok(content) = std::fs::read(&path) {
                        stdout.extend_from_slice(&content);
                        writeln_bytes(stdout, "\n");
                    }
                }
            }
        }
    }
}

impl BlazeRuntime {
    /// The validation half of `InvocationPolicyEnforcer.getEffectivePolicies` / `applySetValueOperation`
    /// for the flag policies applicable to `command`; the first violation is returned as the
    /// message of the `OptionsParsingException` Bazel would throw.
    fn validate_flag_policies(&self, p: &InvocationPolicy, command: &str) -> Result<(), String> {
        // Only the last policy per flag is effective.
        let mut effective: Vec<&FlagPolicy> = Vec::new();
        for fp in &p.flag_policies {
            effective.retain(|e| e.flag_name != fp.flag_name);
            effective.push(fp);
        }
        let defs = super::help::command_options(command);
        for fp in effective {
            if fp.flag_name == "config" {
                return Err(format!(
                    "Invocation policy is applied after --config expansion, changing config values now would have no effect and is disallowed to prevent confusion. To add a --config to the command line, use config_policies instead. Please remove the following policy : {}",
                    single_flag_policy_text(fp).trim_end()
                ));
            }
            // Flags this command does not know are ignored (forward compatibility).
            let Some(def) = defs.iter().find(|d| d.name == fp.flag_name) else { continue };
            let Some(op) = &fp.operation else {
                return Err(format!("Flag policy for flag '{}' does not have an operation", fp.flag_name));
            };
            if let PolicyOperation::SetValue(sv) = op {
                if sv.flag_values.is_empty() {
                    return Err(format!(
                        "SetValue operation from invocation policy for option '--{}' does not have a value",
                        def.name
                    ));
                }
                if sv.flag_values.len() > 1 && !def.allow_multiple {
                    return Err(format!(
                        "SetValue operation from invocation policy sets multiple values for option '--{}' which does not allow multiple values",
                        def.name
                    ));
                }
                if sv.behavior == SetValueBehavior::Undefined {
                    return Err(format!(
                        "SetValue operation from invocation policy for has an undefined behavior: {}",
                        single_flag_policy_text(fp).trim_end()
                    ));
                }
            }
        }
        Ok(())
    }

    /// Applies the SetValue / UseDefault operations of the effective flag policies to the option
    /// arguments after `--` (`InvocationPolicyEnforcer.applySetValueOperation` /
    /// `applyUseDefaultOperation`): ALLOW_OVERRIDES keeps a user-set flag, FINAL_VALUE_IGNORE_OVERRIDES
    /// replaces it, APPEND adds values, FINAL_VALUE_THROW_ON_OVERRIDE rejects a user-set flag.
    /// Allow/Disallow-values filtering is not applied.
    fn enforce_flag_policies(&self, p: &InvocationPolicy, command: &str, args: &[String]) -> Result<Vec<String>, String> {
        let defs = super::help::command_options(command);
        let Some(sep) = args.iter().position(|a| a == "--") else { return Ok(args.to_vec()) };
        let mut head: Vec<String> = args[..=sep].to_vec();
        let mut opts: Vec<String> = args[sep + 1..].to_vec();
        let mut effective: Vec<&FlagPolicy> = Vec::new();
        for fp in &p.flag_policies {
            effective.retain(|e| e.flag_name != fp.flag_name);
            effective.push(fp);
        }
        for fp in effective {
            let Some(def) = defs.iter().find(|d| d.name == fp.flag_name) else { continue };
            let names_flag = |a: &str| {
                let n = a.trim_start_matches('-');
                let n = n.split('=').next().unwrap_or(n);
                (a.starts_with("--") && (n == def.name || (def.uses_boolean_syntax() && n.strip_prefix("no") == Some(def.name))))
                    || (!a.starts_with("--") && a.starts_with('-') && def.abbrev.is_some_and(|c| n.chars().next() == Some(c)))
            };
            let user_set = opts.iter().any(|a| names_flag(a));
            let mut remove = |opts: &mut Vec<String>| {
                let mut i = 0;
                while i < opts.len() {
                    if names_flag(&opts[i]) {
                        // `--flag value` / `-c value` two-token form.
                        let two = !opts[i].contains('=') && def.requires_value() && i + 1 < opts.len();
                        opts.drain(i..i + if two { 2 } else { 1 });
                    } else {
                        i += 1;
                    }
                }
            };
            match &fp.operation {
                Some(PolicyOperation::SetValue(sv)) => {
                    match sv.behavior {
                        SetValueBehavior::AllowOverrides if user_set => continue,
                        SetValueBehavior::FinalValueIgnoreOverrides => remove(&mut opts),
                        SetValueBehavior::FinalValueThrowOnOverride if user_set => {
                            return Err(format!(
                                "User set a value for option '--{}' which is not permitted by the invocation policy. This flag value will always be overridden to [{}]. {}",
                                def.name,
                                sv.flag_values.join(", "),
                                fp.custom_error_message.clone().unwrap_or_default()
                            ));
                        }
                        _ => {}
                    }
                    for v in &sv.flag_values {
                        opts.push(format!("--{}={}", def.name, v));
                    }
                }
                Some(PolicyOperation::UseDefault(_)) => remove(&mut opts),
                _ => {}
            }
        }
        head.append(&mut opts);
        Ok(head)
    }

    /// `LicenseCommand.exec`.
    pub(crate) fn cmd_license(&self, args: &[String], stdout: &mut Vec<u8>, stderr: &mut Vec<u8>) -> ExitCode {
        let _parsed = match zel_options::ParsedCommandOptions::parse_for_command("license", args) {
            Ok(p) => p,
            Err(e) => {
                writeln_bytes(stderr, &e.format_bazel_error());
                return ExitCode::COMMAND_LINE_ERROR;
            }
        };

        writeln_bytes(stdout, "Licenses of all components included in this binary:\n");
        writeln_bytes(stdout, LICENSE_TEXT);

        const JAVA_LICENSE_FILES: &[&str] = &[
            "ASSEMBLY_EXCEPTION",
            "DISCLAIMER",
            "LICENSE",
            "THIRD_PARTY_README",
        ];
        let bundled_jdk = self.dirs.install_base.get_relative_str("embedded_tools/jdk");
        let bundled_jre = self.dirs.install_base.get_relative_str("embedded_tools/jre");
        for (dir_name, bundled_dir) in [("JDK", bundled_jdk), ("JRE", bundled_jre)] {
            let p = std::path::Path::new(bundled_dir.path_string());
            if p.exists() {
                writeln_bytes(
                    stdout,
                    &format!(
                        "\nThis binary comes with a bundled {dir_name}, which contains the following license files:\n"
                    ),
                );
                walk_license_files(p, JAVA_LICENSE_FILES, stdout);
            }
        }

        ExitCode::SUCCESS
    }

    /// `CanonicalizeCommand.exec`.
    pub(crate) fn cmd_canonicalize_flags(
        &self,
        args: &[String],
        stdout: &mut Vec<u8>,
        stderr: &mut Vec<u8>,
    ) -> ExitCode {
        // `BlazeOptionHandler.partitionCommandLineArgs`: every `--rc_source[=]` / `--default_override[=]`
        // (also after `--`) is consumed as an rc source before option parsing.
        // The consumed rc sources are still converted (`OptionOverrideConverter` for `--default_override`).
        for a in args.iter().filter(|a| a.starts_with("--rc_source=") || a.starts_with("--default_override=")) {
            if let Err(e) = zel_options::ParsedCommandOptions::parse_for_command("canonicalize-flags", std::slice::from_ref(a)) {
                writeln_bytes(stderr, &e.format_bazel_error());
                return ExitCode::COMMAND_LINE_ERROR;
            }
        }
        let args_owned = partition_rc_sources(args);
        let args: &[String] = &args_owned;
        // `--for_command` is validated against the command map first.
        let mut for_command = "build".to_string();
        let mut policy_arg = String::new();
        let mut canonicalize_policy = false;
        for a in args.iter().take_while(|a| *a != "--") {
            if let Some(v) = a.strip_prefix("--for_command=") {
                for_command = v.to_string();
            } else if let Some(v) = a.strip_prefix("--invocation_policy=") {
                policy_arg = v.to_string();
            } else if a == "--canonicalize_policy" || a == "--canonicalize_policy=true" {
                canonicalize_policy = true;
            } else if a == "--nocanonicalize_policy" || a == "--canonicalize_policy=false" {
                canonicalize_policy = false;
            }
        }
        const BAZEL_COMMAND_ORDER: &[&str] = &[
            "mobile-install", "fetch", "mod", "vendor", "build", "canonicalize-flags", "clean",
            "coverage", "dump", "help", "info", "print_action", "query", "run", "shutdown",
            "test", "version", "aquery", "cquery", "config", "license",
        ];
        if !BAZEL_COMMAND_ORDER.contains(&for_command.as_str()) {
            writeln_bytes(
                stderr,
                &format!(
                    "ERROR: Not a valid command: '{for_command}' (should be one of {})",
                    BAZEL_COMMAND_ORDER.join(", ")
                ),
            );
            return ExitCode::COMMAND_LINE_ERROR;
        }
        if !policy_arg.is_empty() {
            if let Err(_) = InvocationPolicy::parse(&policy_arg) {
                writeln_bytes(stderr, &format!("ERROR: Malformed value of --invocation_policy: {policy_arg}"));
                return ExitCode::COMMAND_LINE_ERROR;
            }
        }
        // `InvocationPolicyEnforcer.enforce` runs for every canonicalization: structurally invalid
        // policies of the effective policy set (last one per flag, applicable to the command)
        // are errors even when the flag itself is not touched.
        if !policy_arg.is_empty() {
            if let Ok(p) = InvocationPolicy::parse(&policy_arg) {
                if let Err(msg) = self.validate_flag_policies(&policy_for_command(&p, &for_command), &for_command) {
                    writeln_bytes(stderr, &format!("ERROR: {msg}"));
                    return ExitCode::COMMAND_LINE_ERROR;
                }
            }
        }
        if canonicalize_policy {
            return match InvocationPolicy::parse(&policy_arg) {
                Ok(p) => {
                    for fp in &p.flag_policies {
                        if let Some(PolicyOperation::SetValue(ref sv)) = fp.operation {
                            if sv.behavior == SetValueBehavior::Undefined {
                                writeln_bytes(
                                    stderr,
                                    &format!(
                                        "ERROR: SetValue operation from invocation policy for has an undefined behavior: {}",
                                        single_flag_policy_text(fp)
                                    ),
                                );
                                return ExitCode::COMMAND_LINE_ERROR;
                            }
                        }
                    }
                    writeln_bytes(stdout, &policy_text(&policy_for_command(&p, &for_command)));
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    writeln_bytes(stderr, &format!("ERROR: {}", e.message));
                    ExitCode::COMMAND_LINE_ERROR
                }
            };
        }
        if let Some(pos) = args.iter().position(|a| a == "--") {
            for a in &args[pos + 1..] {
                // Only `--//pkg:flag` / `--@repo//pkg:flag` are Starlark flags (loaded as targets); a
                // bare `//pkg:target` is residue, reported by the option parser.
                if !a.starts_with("--") {
                    continue;
                }
                let stripped = a.trim_start_matches('-');
                if stripped.starts_with("//") || stripped.starts_with('@') {
                    let opt_name = stripped.split('=').next().unwrap_or(stripped);
                    let (pkg, _name) = if let Some(rel) = opt_name.strip_prefix("//") {
                        rel.split_once(':').unwrap_or((rel, ""))
                    } else if let Some((_, rest)) = opt_name.split_once("//") {
                        rest.split_once(':').unwrap_or((rest, ""))
                    } else {
                        ("", "")
                    };
                    let ws = std::path::Path::new(self.dirs.workspace_dir.path_string());
                    let pkg_dir = ws.join(pkg);
                    if !pkg_dir.join("BUILD").exists() && !pkg_dir.join("BUILD.bazel").exists() {
                        writeln_bytes(stderr, "WARNING: Target pattern parsing failed.");
                        writeln_bytes(
                            stderr,
                            &format!(
                                "ERROR: Skipping '{opt_name}': no such package '{pkg}': BUILD file not found in any of the following directories. Add a BUILD file to a directory to mark it as a package.\n - {pkg}"
                            ),
                        );
                        writeln_bytes(
                            stderr,
                            &format!(
                                "ERROR: Error loading option {opt_name}: no such package '{pkg}': BUILD file not found in any of the following directories. Add a BUILD file to a directory to mark it as a package.\n - {pkg}"
                            ),
                        );
                        return ExitCode::COMMAND_LINE_ERROR;
                    }
                }
            }
        }
        let enforced_args;
        let args = if policy_arg.is_empty() {
            args
        } else {
            match InvocationPolicy::parse(&policy_arg) {
                Ok(p) => match self.enforce_flag_policies(&policy_for_command(&p, &for_command), &for_command, args) {
                    Ok(a) => {
                        enforced_args = a;
                        &enforced_args[..]
                    }
                    Err(msg) => {
                        writeln_bytes(stderr, &format!("ERROR: {msg}"));
                        return ExitCode::COMMAND_LINE_ERROR;
                    }
                },
                Err(_) => args,
            }
        };
        match zel_options::canonicalize_flags(args) {
            Ok(flags) => {
                for flag in &flags {
                    writeln_bytes(stdout, flag);
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                writeln_bytes(stderr, &e.format_bazel_error());
                ExitCode::COMMAND_LINE_ERROR
            }
        }
    }
}

/// `BlazeOptionHandler.partitionCommandLineArgs`: the command line without `--rc_source[=]` and
/// `--default_override[=]` (and the separate value following the bare forms).
pub(crate) fn partition_rc_sources(cmd_line: &[String]) -> Vec<String> {
    let mut remaining = Vec::with_capacity(cmd_line.len());
    let mut it = cmd_line.iter();
    while let Some(option) = it.next() {
        if option.starts_with("--rc_source=") || option.starts_with("--default_override=") {
            continue;
        }
        if option == "--rc_source" || option == "--default_override" {
            it.next();
            continue;
        }
        remaining.push(option.clone());
    }
    remaining
}
