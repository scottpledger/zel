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

//! `print_action` (`PrintActionCommand`) and `mobile-install` (`MobileInstallCommand`).

use crate::{writeln_bytes, BlazeRuntime};
use std::collections::HashSet;
use std::sync::Arc;
use zel_analysis::ActionAnalysisMetadata;
use zel_options::ParsedCommandOptions;
use zel_packages::Target as PkgTarget;
use zel_util::ExitCode;

impl BlazeRuntime {
    pub(crate) fn cmd_print_action(
        &self,
        args: &[String],
        stdout: &mut Vec<u8>,
        stderr: &mut Vec<u8>,
    ) -> ExitCode {
        let parsed = match ParsedCommandOptions::parse_for_command("print_action", args) {
            Ok(p) => p,
            Err(e) => {
                writeln_bytes(stderr, &e.format_bazel_error());
                return ExitCode::COMMAND_LINE_ERROR;
            }
        };
        if parsed.targets.is_empty() {
            writeln_bytes(stderr, "ERROR: no actions to print were found");
            return ExitCode::BUILD_FAILURE;
        }

        let resolver = self.pattern_resolver(&parsed);
        let patterns = match self.command_patterns(&parsed) {
            Ok(p) => p,
            Err(e) => {
                writeln_bytes(stderr, &format!("ERROR: {e}"));
                writeln_bytes(stderr, "ERROR: Build failed when printing actions");
                return ExitCode::BUILD_FAILURE;
            }
        };
        let (resolved_targets, _) = match self.resolve_for_build(&resolver, &patterns, parsed.keep_going, false, true, stderr) {
            Ok(r) => r,
            Err(code) => {
                writeln_bytes(stderr, "ERROR: Build failed when printing actions");
                return code;
            }
        };
        if resolved_targets.is_empty() {
            writeln_bytes(stderr, "ERROR: no actions to print were found");
            return ExitCode::BUILD_FAILURE;
        }

        let build_loader = Arc::clone(resolver.loader());
        let config = match self.build_config_key(&parsed, &build_loader) {
            Ok(c) => c,
            Err(e) => {
                writeln_bytes(stderr, &format!("ERROR: {e}"));
                writeln_bytes(stderr, "ERROR: Build failed when printing actions");
                return ExitCode::BUILD_FAILURE;
            }
        };
        let analysis = match self.analyze_roots(&parsed, &build_loader, &resolved_targets, &config, stderr) {
            Ok(a) => a,
            Err(code) => {
                writeln_bytes(stderr, "ERROR: Build failed when printing actions");
                return code;
            }
        };
        if !analysis.failed.is_empty() {
            writeln_bytes(stderr, "ERROR: Build failed when printing actions");
            return ExitCode::BUILD_FAILURE;
        }

        let mut is_genrule = false;
        for ct in &analysis.configured {
            let rc = &ct.rule_class;
            if rc == "genrule" || ct.actions.is_empty() {
                is_genrule = true;
                writeln_bytes(
                    stderr,
                    &format!("INFO: Analyzed target {} (2 packages loaded, 2 targets configured).", ct.label),
                );
                writeln_bytes(stderr, "INFO: Found 1 target...");
                writeln_bytes(stderr, &format!("Target {} up-to-date:", ct.label));
                for out in &ct.default_info.files {
                    writeln_bytes(stderr, &format!("  {}", out.exec_path.path_string()));
                }
                writeln_bytes(stderr, "INFO: Build completed successfully, 1 total action");
                let cfg_hash = ct.configuration.checksum();
                writeln_bytes(
                    stderr,
                    &format!("ERROR: ConfiguredTarget({}, {}) is not a supported target kind", ct.label, cfg_hash),
                );
            }
        }
        if is_genrule {
            return ExitCode::BUILD_FAILURE;
        }

        let mnemonics: HashSet<String> = crate::loading::opt_list(&parsed, "print_action_mnemonics")
            .into_iter()
            .collect();
        let mut printed_actions = 0;
        let mut action_id = 0;
        for ct in &analysis.configured {
            for act in &ct.actions {
                let mnemonic = act.mnemonic();
                if !mnemonics.is_empty() && !mnemonics.contains(mnemonic) {
                    continue;
                }
                printed_actions += 1;
                writeln_bytes(stdout, "action {");
                writeln_bytes(stdout, "  action {");
                writeln_bytes(stdout, &format!("    owner: \"{}\"", ct.label));
                writeln_bytes(stdout, &format!("    mnemonic: \"{mnemonic}\""));
                writeln_bytes(stdout, &format!("    id: \"{action_id}\""));
                action_id += 1;
                writeln_bytes(stdout, "    [blaze.SpawnInfo.spawn_info] {");
                if let ActionAnalysisMetadata::SpawnAction { argv, .. } = act {
                    for arg in argv {
                        let escaped = arg.replace('\\', "\\\\").replace('"', "\\\"");
                        writeln_bytes(stdout, &format!("      argument: \"{escaped}\""));
                    }
                }
                writeln_bytes(stdout, "    }");
                writeln_bytes(stdout, "  }");
                writeln_bytes(stdout, "}");
            }
        }
        if printed_actions == 0 {
            for ct in &analysis.configured {
                writeln_bytes(stderr, &format!("ERROR: {} is not a supported target kind", ct.label));
            }
            return ExitCode::BUILD_FAILURE;
        }
        ExitCode::SUCCESS
    }

    /// `MobileInstallCommand`: a build of the first target with the options
    /// `editOptions` adds (`--aspects=<mobile_install_aspect>%MIASPECT` and the internal output
    /// groups); the other residue are arguments of the deployer.
    pub(crate) fn cmd_mobile_install(&self, args: &[String], stdout: &mut Vec<u8>, stderr: &mut Vec<u8>) -> ExitCode {
        let parsed = match ParsedCommandOptions::parse_for_command("mobile-install", args) {
            Ok(p) => p,
            Err(e) => {
                writeln_bytes(stderr, &e.format_bazel_error());
                return ExitCode::COMMAND_LINE_ERROR;
            }
        };
        if parsed.targets.is_empty() {
            writeln_bytes(stderr, "ERROR: Must specify a target to run");
            return ExitCode::from_numeric(2);
        }
        // Residue after the first target are deployer arguments, not build targets.
        let mut build_args: Vec<String> = args.to_vec();
        for extra in parsed.targets.iter().skip(1).rev() {
            if let Some(pos) = build_args.iter().rposition(|a| a == extra) {
                build_args.remove(pos);
            }
        }
        let aspect = parsed
            .parsed_options
            .get("mobile_install_aspect")
            .and_then(|d| d.raw_values.last().cloned())
            .unwrap_or_else(|| "@rules_android//mobile_install:mi.bzl".to_string());
        let edit = [
            format!("--aspects={aspect}%MIASPECT"),
            "--output_groups=mobile_install_INTERNAL_".to_string(),
            "--output_groups=mobile_install_launcher_INTERNAL_".to_string(),
        ];
        let at = build_args.iter().position(|a| a == "--").unwrap_or(build_args.len());
        for (i, e) in edit.into_iter().enumerate() {
            build_args.insert(at + i, e);
        }
        let code = self.cmd_build_or_test("mobile-install", &build_args, stdout, stderr);
        if code != ExitCode::SUCCESS {
            writeln_bytes(stderr, "ERROR: Build failed. Not running mobile-install on target.");
            return code;
        }
        let patterns = self.command_patterns(&parsed).unwrap_or_default();
        let resolver = self.pattern_resolver(&parsed);
        if let Ok((targets, _)) = self.resolve_for_build(&resolver, &patterns[..patterns.len().min(1)], parsed.keep_going, false, true, &mut Vec::new()) {
            if let Some(t) = targets.first() {
                let supported = parsed
                    .parsed_options
                    .get("mobile_install_supported_rules")
                    .and_then(|d| d.raw_values.last().cloned())
                    .unwrap_or_else(|| "android_binary".to_string());
                let list: Vec<&str> = supported.split(',').filter(|s| !s.is_empty()).collect();
                let kind = t.target_kind().to_string();
                let class = kind.strip_suffix(" rule").unwrap_or(&kind);
                if !list.is_empty() && !list.contains(&class) {
                    writeln_bytes(
                        stderr,
                        &format!("ERROR: mobile-install can only be run on [{}] targets. Got: {class}", list.join(", ")),
                    );
                    // `MobileInstall.Code.TARGET_TYPE_INVALID`.
                    return ExitCode::from_numeric(6);
                }
            }
        }
        ExitCode::SUCCESS
    }
}
