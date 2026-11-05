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

//! Encapsulates logic for performing `--config` option expansion matching Bazel's `ConfigExpander.java`.

use crate::error::OptionsParsingError;
use crate::parser::split_unsupported_common_args;
use crate::rc::{get_command_ancestry, BazelrcConfig};
use zel_util::Os;

/// Returns host platform identifier matching Bazel's `ConfigExpander.getPlatformName()`.
pub fn get_platform_name() -> &'static str {
    match Os::current() {
        Os::Darwin => "macos",
        Os::Linux => "linux",
        Os::Windows => "windows",
        Os::FreeBsd => "freebsd",
        Os::OpenBsd => "openbsd",
        _ => "unknown",
    }
}

/// Expands all RC options, platform-specific configs, and `--config` references.
pub fn expand_command_args(
    rc_config: &BazelrcConfig,
    command: &str,
    cli_args: &[String],
) -> Result<(Vec<String>, Vec<String>), OptionsParsingError> {
    let mut warnings = Vec::new();
    let commands_to_parse = get_command_ancestry(command);

    // 1. Collect base args from RC files for commands in ancestry
    let mut base_args = Vec::new();
    for cmd in &commands_to_parse {
        if let Some(opts) = rc_config.sections.get(*cmd) {
            let toks: Vec<String> = opts.iter().map(|(o, _)| o.clone()).collect();
            // `common` options that only other commands know are ignored (BlazeOptionHandler.parseRcOptions).
            let toks = if *cmd == "common" { split_unsupported_common_args(command, &toks).0 } else { toks };
            base_args.extend(toks);
        }
    }

    // 2. Append CLI args
    base_args.extend(cli_args.iter().cloned());

    // 3. Check for --enable_platform_specific_config in base_args
    let platform = get_platform_name();
    let platform_enabled = base_args.iter().any(|a| {
        a == "--enable_platform_specific_config"
            || a == "--enable_platform_specific_config=true"
            || a == "--enable_platform_specific_config=1"
    });

    if platform_enabled {
        // If corresponding config definition exists in any of the commands to parse
        let has_platform_config = commands_to_parse.iter().any(|cmd| {
            let key = format!("{cmd}:{platform}");
            rc_config.sections.contains_key(&key)
        });
        if has_platform_config {
            base_args.push(format!("--config={platform}"));
        }
    }

    // 4. Expand --config recursively
    let mut expanded = Vec::new();
    let mut longest_chain = Vec::new();
    let mut config_counts = std::collections::BTreeMap::new();
    let mut found_notes: Vec<String> = Vec::new();
    expand_configs_recursive(
        rc_config,
        command,
        &commands_to_parse,
        &base_args,
        &mut Vec::new(),
        &mut longest_chain,
        &mut config_counts,
        &mut found_notes,
        &mut expanded,
    )?;

    let dups: Vec<String> = config_counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(cfg, _)| cfg)
        .collect();
    if !dups.is_empty() {
        warnings.push(format!(
            "WARNING: The following configs were expanded more than once: [{}]. For repeatable flags, repeats are counted twice and may lead to unexpected behavior.",
            dups.join(", ")
        ));
    }

    if longest_chain.len() >= 10 {
        warnings.push(format!(
            "There is a recursive chain of configs {} configs long: {:?}. This seems excessive, and might be hiding errors.",
            longest_chain.len(),
            longest_chain
        ));
    }

    // `ConfigExpander.getExpansion` announces every applicable definition (consumed by
    // `--announce_rc`), one note per rc file chunk.
    warnings.splice(0..0, found_notes);
    Ok((expanded, warnings))
}

fn expand_configs_recursive(
    rc_config: &BazelrcConfig,
    command: &str,
    commands_to_parse: &[&'static str],
    args: &[String],
    config_ancestor_set: &mut Vec<String>,
    longest_chain: &mut Vec<String>,
    config_counts: &mut std::collections::BTreeMap<String, usize>,
    found_notes: &mut Vec<String>,
    out: &mut Vec<String>,
) -> Result<(), OptionsParsingError> {
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            // Residue separator; don't expand any options after --
            out.extend_from_slice(&args[i..]);
            break;
        }

        let config_name = if let Some(c) = arg.strip_prefix("--config=") {
            Some((c.to_string(), 1))
        } else if arg == "--config" && i + 1 < args.len() {
            Some((args[i + 1].clone(), 2))
        } else {
            None
        };

        if let Some((cfg, consumed)) = config_name {
            if config_ancestor_set.contains(&cfg) {
                return Err(OptionsParsingError::new(format!(
                    "Config expansion has a cycle: config value {cfg} expands to itself, see inheritance chain [{}]",
                    config_ancestor_set.join(", ")
                )));
            }

            *config_counts.entry(cfg.clone()).or_default() += 1;

            let mut found_definition = false;
            let mut cfg_opts = Vec::new();

            for cmd in commands_to_parse {
                let key = format!("{cmd}:{cfg}");
                if let Some(opts) = rc_config.sections.get(&key) {
                    found_definition = true;
                    let mut chunk_src: Option<&str> = None;
                    let mut chunk: Vec<&str> = Vec::new();
                    let kept_common: Option<Vec<String>> = if *cmd == "common" {
                        let toks: Vec<String> = opts.iter().map(|(o, _)| o.clone()).collect();
                        Some(split_unsupported_common_args(command, &toks).0)
                    } else {
                        None
                    };
                    let opts: Vec<&(String, String)> = match &kept_common {
                        Some(k) => {
                            let mut keep = k.clone();
                            opts.iter().filter(|(o, _)| {
                                if let Some(p) = keep.iter().position(|x| x == o) { keep.remove(p); true } else { false }
                            }).collect()
                        }
                        None => opts.iter().collect(),
                    };
                    for (opt, src) in opts {
                        if chunk_src.is_some_and(|c| c != src.as_str()) {
                            found_notes.push(format!(
                                "INFO: Found applicable config definition {key} in file {}: {}",
                                chunk_src.unwrap_or_default(),
                                chunk.join(" ")
                            ));
                            chunk.clear();
                        }
                        chunk_src = Some(src.as_str());
                        chunk.push(opt.as_str());
                        cfg_opts.push(opt.clone());
                    }
                    if let Some(c) = chunk_src {
                        found_notes.push(format!(
                            "INFO: Found applicable config definition {key} in file {c}: {}",
                            chunk.join(" ")
                        ));
                    }
                }
            }

            if !found_definition {
                return Err(OptionsParsingError::new(format!(
                    "Config value '{cfg}' is not defined in any .rc file"
                )));
            }

            config_ancestor_set.push(cfg);
            if config_ancestor_set.len() > longest_chain.len() {
                *longest_chain = config_ancestor_set.clone();
            }

            expand_configs_recursive(
                rc_config,
                command,
                commands_to_parse,
                &cfg_opts,
                config_ancestor_set,
                longest_chain,
                config_counts,
                found_notes,
                out,
            )?;

            config_ancestor_set.pop();
            i += consumed;
        } else {
            out.push(arg.clone());
            i += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_expansion_diamond() {
        let mut config = BazelrcConfig::default();
        config.sections.insert(
            "build:base".to_string(),
            vec![("-c".to_string(), "rc".to_string()), ("opt".to_string(), "rc".to_string())],
        );
        config.sections.insert(
            "build:a".to_string(),
            vec![("--config=base".to_string(), "rc".to_string()), ("--jobs=16".to_string(), "rc".to_string())],
        );
        config.sections.insert(
            "build:b".to_string(),
            vec![("--config=base".to_string(), "rc".to_string()), ("--verbose_failures".to_string(), "rc".to_string())],
        );
        config.sections.insert(
            "build:top".to_string(),
            vec![("--config=a".to_string(), "rc".to_string()), ("--config=b".to_string(), "rc".to_string())],
        );

        let (expanded, _) = expand_command_args(&config, "build", &["--config=top".to_string()]).unwrap();
        assert_eq!(expanded, vec!["-c", "opt", "--jobs=16", "-c", "opt", "--verbose_failures"]);
    }

    #[test]
    fn test_config_expansion_cycle() {
        let mut config = BazelrcConfig::default();
        config.sections.insert(
            "build:foo".to_string(),
            vec![("--config=bar".to_string(), "rc".to_string())],
        );
        config.sections.insert(
            "build:bar".to_string(),
            vec![("--config=foo".to_string(), "rc".to_string())],
        );

        let err = expand_command_args(&config, "build", &["--config=foo".to_string()]).unwrap_err();
        assert!(err.message.contains("Config expansion has a cycle: config value foo expands to itself"));
    }
}
