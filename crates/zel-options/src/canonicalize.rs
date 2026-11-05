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

//! Flag canonicalization matching Bazel's `canonicalize-flags` command.

use crate::error::OptionsParsingError;
use crate::flag_tables::get_command_options_map;
use crate::invocation_policy::InvocationPolicy;
use crate::parser::ParsedCommandOptions;
use std::collections::HashSet;

/// Options specific to the `canonicalize-flags` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalizeCommandOptions {
    pub for_command: String,
    pub invocation_policy: String,
    pub canonicalize_policy: bool,
    pub include_default_values: bool,
}

impl Default for CanonicalizeCommandOptions {
    fn default() -> Self {
        Self {
            for_command: "build".to_string(),
            invocation_policy: String::new(),
            canonicalize_policy: false,
            include_default_values: true,
        }
    }
}

/// Canonicalizes command options for a given target command.
pub fn canonicalize_flags(
    raw_args: &[String],
) -> Result<Vec<String>, OptionsParsingError> {
    let (before_dash, after_dash) = match raw_args.iter().position(|a| a == "--") {
        Some(idx) => (&raw_args[..idx], &raw_args[idx + 1..]),
        None => (raw_args, &[][..]),
    };

    let mut cmd_opts = CanonicalizeCommandOptions::default();
    let parsed_cmd = ParsedCommandOptions::parse_for_command("canonicalize-flags", before_dash)?;
    if let Some(c) = parsed_cmd.get_str("for_command") {
        cmd_opts.for_command = c.to_string();
    }
    if let Some(p) = parsed_cmd.get_str("invocation_policy") {
        cmd_opts.invocation_policy = p.to_string();
    }
    if let Some(cp) = parsed_cmd.get_bool("canonicalize_policy") {
        cmd_opts.canonicalize_policy = cp;
    }
    if let Some(idv) = parsed_cmd.get_bool("experimental_include_default_values") {
        cmd_opts.include_default_values = idv;
    }

    if !cmd_opts.invocation_policy.is_empty() {
        if let Err(_) = InvocationPolicy::parse(&cmd_opts.invocation_policy) {
            return Err(OptionsParsingError::new(format!(
                "Malformed value of --invocation_policy: {}",
                cmd_opts.invocation_policy
            )));
        }
    }

    if after_dash.is_empty() {
        return Ok(Vec::new());
    }

    let mut parsed = match ParsedCommandOptions::parse_for_command(
        &cmd_opts.for_command,
        after_dash,
    ) {
        Ok(p) => p,
        Err(mut e) => {
            e.failed_arg = None;
            return Err(e);
        }
    };

    if !parsed.targets.is_empty() {
        return Err(OptionsParsingError::new(format!(
            "Unrecognized arguments: {}",
            parsed.targets.join(" ")
        )));
    }

    if !cmd_opts.invocation_policy.is_empty() {
        let policy = match InvocationPolicy::parse(&cmd_opts.invocation_policy) {
            Ok(p) => p,
            Err(_) => {
                return Err(OptionsParsingError::new(format!(
                    "Malformed value of --invocation_policy: {}",
                    cmd_opts.invocation_policy
                )));
            }
        };
        parsed.apply_invocation_policy(&policy, &cmd_opts.for_command)?;
    }

    let cmd_map = get_command_options_map();

    // Preserve encounter order, retaining only the winning (last) instance for single-value flags
    let mut seen_single = HashSet::new();
    let mut effective_canonical = Vec::new();
    for flag in parsed.canonical_flags.into_iter().rev() {
        let flag_name = flag.trim_start_matches('-').split('=').next().unwrap_or("");
        let allow_multiple = cmd_map.get(flag_name).map(|d| d.allow_multiple).unwrap_or(false);
        if !allow_multiple {
            if seen_single.contains(flag_name) {
                continue;
            }
            seen_single.insert(flag_name.to_string());
        }
        effective_canonical.push(flag);
    }
    effective_canonical.reverse();

    Ok(effective_canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canonicalize_ordering_and_dedup() {
        let args = vec![
            "--".to_string(),
            "-k".to_string(),
            "-j".to_string(),
            "4".to_string(),
            "-c".to_string(),
            "opt".to_string(),
            "--keep_going".to_string(),
        ];
        let flags = canonicalize_flags(&args).unwrap();
        assert_eq!(flags, vec!["--jobs=4", "--compilation_mode=opt", "--keep_going=1"]);
    }
}
