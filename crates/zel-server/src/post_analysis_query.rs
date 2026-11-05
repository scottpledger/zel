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

//! `cquery` and `aquery` command glue (`CqueryCommand`, `AqueryCommand`).

use super::loading::opt_list;
use super::query_cmd::exit_from_code;
use super::*;
use zel_query::post_analysis::{run_post_analysis, PostOpts};
use zel_rules::TargetResolver;

impl BlazeRuntime {
    pub(crate) fn cmd_post_analysis_query(
        &self,
        cmd_kind: &str,
        parsed: &ParsedCommandOptions,
        expr: &str,
        stdout: &mut Vec<u8>,
        stderr: &mut Vec<u8>,
    ) -> ExitCode {
        let aquery = cmd_kind == "aquery";
        for w in &parsed.old_name_warnings {
            writeln_bytes(stderr, &format!("WARNING: {w}"));
        }
        if parsed.get_bool("nodep_deps") == Some(false) {
            writeln_bytes(stderr, "ERROR: Error doing post analysis query: The following filter(s) are not currently supported by configured query: [NO_NODEP_DEPS, INCLUDE_ASPECTS]");
            writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
            return ExitCode::BUILD_FAILURE;
        }
        let resolver = self.pattern_resolver(parsed);
        let config = match self.build_config_key(parsed, resolver.loader()).map(|mut c| {
            // `CqueryCommand`: `--show_config_fragments=X` implicitly sets
            // `--include_config_fragments_provider=X` (COMMAND_LINE priority).
            if !aquery {
                if let Some(v) = parsed.get_str("show_config_fragments").filter(|v| *v != "off") {
                    if let Some((_, opt)) = zel_analysis::build_options::find_option("include_config_fragments_provider") {
                        let raws = vec![v.to_string()];
                        let canon = |raw: &str| Ok::<String, String>(raw.to_string());
                        if let Ok(value) = zel_analysis::build_options::convert_raws(opt, &raws, &canon, zel_analysis::build_options::auto_host_cpu()) {
                            c.native.insert("include_config_fragments_provider".to_string(), zel_analysis::NativeSetting { raw: v.to_string(), value });
                        }
                    }
                }
            }
            c
        }) {
            Ok(c) => c,
            Err(e) => {
                writeln_bytes(stderr, &format!("ERROR: {e}"));
                writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                return ExitCode::COMMAND_LINE_ERROR;
            }
        };
        if let Err(code) = self.validate_top_level_aspects(parsed, resolver.loader(), expr, stderr) {
            return code;
        }
        if let Err(code) = self.check_analysis_cache(parsed, &config, stderr) {
            writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
            return code;
        }
        let acfg = match self.analysis_config(parsed, resolver.loader()) {
            Ok(a) => a,
            Err(e) => {
                writeln_bytes(stderr, &format!("ERROR: {e}"));
                writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                return ExitCode::BUILD_FAILURE;
            }
        };
        let qopts = self.query_opts(parsed);
        let default_output = if aquery { "text" } else { "label" };
        let fopts = self.query_format_opts(parsed, &resolver, default_output);
        let cwd = self.command_cwd(parsed);
        let flag = |name: &str, default: bool| parsed.get_bool(name).unwrap_or(default);
        let mut popts = PostOpts {
            aquery,
            output: fopts.output.clone(),
            transitions: parsed.get_str("transitions").unwrap_or("none").to_string(),
            show_config_fragments: parsed.get_str("show_config_fragments").unwrap_or("off").to_string(),
            proto_include_configurations: flag("proto:include_configurations", true),
            starlark_expr: parsed.get_str("starlark:expr").unwrap_or("").to_string(),
            starlark_file: parsed.get_str("starlark:file").unwrap_or("").to_string(),
            include_commandline: flag("include_commandline", true),
            include_artifacts: flag("include_artifacts", true),
            include_param_files: flag("include_param_files", false),
            include_file_write_contents: flag("include_file_write_contents", false),
            include_pruned_inputs: flag("include_pruned_inputs", true),
            skyframe_state: flag("skyframe_state", false),
            prune_unused_actions: flag("prune_unused_actions", false),
            output_groups: crate::loading::opt_list(parsed, "output_groups"),
            line_terminator: fopts.line_terminator.clone(),
            ..Default::default()
        };
        let tr = if popts.skyframe_state {
            if let Some(r) = self.last_resolver.lock().unwrap().clone() {
                r
            } else {
                TargetResolver::new(resolver.loader().clone(), acfg)
            }
        } else {
            TargetResolver::new(resolver.loader().clone(), acfg)
        };
        if !aquery && parsed.get_str("output").is_none() && popts.transitions != "none" {
            popts.output = "transitions".into();
        }
        if !popts.starlark_file.is_empty() {
            if !popts.starlark_expr.is_empty() {
                writeln_bytes(stderr, "ERROR: Error doing post analysis query: You must not specify both --starlark:expr and --starlark:file");
                writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                return ExitCode::BUILD_FAILURE;
            }
            let file_path = cwd.join(&popts.starlark_file);
            match std::fs::read_to_string(&file_path) {
                Ok(s) => popts.starlark_file_source = s,
                Err(_) => {
                    writeln_bytes(stderr, &format!("ERROR: Error doing post analysis query: invalid --starlark:file: failed to read {}", file_path.display()));
                    writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
                    return ExitCode::BUILD_FAILURE;
                }
            }
        }
        if aquery && expr.trim().is_empty() && !popts.skyframe_state {
            return ExitCode::SUCCESS;
        }
        if aquery && popts.skyframe_state {
            let mut pats = Vec::new();
            if let Ok(e) = zel_query::parse::parse(expr, &zel_query::parse::all_functions()) {
                e.collect_target_patterns(&mut pats);
            }
            let top = if qopts.universe_scope.is_empty() { pats } else { qopts.universe_scope.clone() };
            if !top.is_empty() {
                writeln_bytes(
                    stderr,
                    &format!(
                        "ERROR: Error while parsing '{}': Specifying build target(s) [{}] with --skyframe_state is currently not supported.",
                        zel_query::parse::truncate(expr),
                        top.join(", ")
                    ),
                );
                return ExitCode::COMMAND_LINE_ERROR;
            }
            if popts.output != "proto" && popts.output != "textproto" && popts.output != "jsonproto" {
                writeln_bytes(stderr, &format!("ERROR: --skyframe_state must be used with --output=proto|textproto|jsonproto. Invalid aquery output format: {}", popts.output));
                return ExitCode::COMMAND_LINE_ERROR;
            }
        }
        let _ = opt_list;
        let mut popts = popts;
        {
            // `--aspects`: validated above; loaded again here to be applied to the top-level targets.
            let rel = self.command_relative_dir(parsed);
            for spec in crate::loading::opt_list(parsed, "aspects") {
                if let Some((file, name)) = spec.rsplit_once('%') {
                    if let Ok(label) = Self::canonical_option_label(resolver.loader(), file, &rel) {
                        if let Ok(v) = resolver.loader().load_bzl_export(&label, name) {
                            popts.top_aspects.push(v);
                        }
                    }
                }
            }
            for kv in crate::loading::opt_list(parsed, "aspects_parameters") {
                if let Some((k, v)) = kv.split_once('=') {
                    popts.aspect_params.insert(k.to_string(), v.to_string());
                }
            }
        }
        let outcome = run_post_analysis(&resolver, tr, config, expr, &qopts, &fopts, &popts);
        let output_file = parsed.get_str("output_file").filter(|s| !s.is_empty());
        if let Some(out_path) = output_file {
            let file_path = cwd.join(out_path);
            if let Err(code) = crate::query_cmd::write_query_output_file(&file_path, &outcome.stdout, stderr) {
                return code;
            }
        } else {
            stdout.extend_from_slice(&outcome.stdout);
        }
        for l in &outcome.stderr {
            writeln_bytes(stderr, l);
        }
        exit_from_code(outcome.exit_code)
    }

    /// `--aspects=//pkg:file.bzl%name`: a top-level aspect that cannot be loaded fails the whole
    /// command (`AspectCollection` / `BuildView.getTopLevelAspects`): `Aspect 'x' is unknown` for a
    /// spec without `%`, otherwise the load failure reported twice (as event and as the analysis
    /// failure of the first top-level target).
    fn validate_top_level_aspects(
        &self,
        parsed: &ParsedCommandOptions,
        loader: &Arc<zel_packages::PackageLoader>,
        expr: &str,
        stderr: &mut Vec<u8>,
    ) -> Result<(), ExitCode> {
        let specs = crate::loading::opt_list(parsed, "aspects");
        if specs.is_empty() {
            return Ok(());
        }
        let rel = self.command_relative_dir(parsed);
        let fail = |stderr: &mut Vec<u8>, lines: Vec<String>| {
            for l in lines {
                writeln_bytes(stderr, &l);
            }
            writeln_bytes(stderr, "ERROR: Build did NOT complete successfully");
            ExitCode::BUILD_FAILURE
        };
        for spec in &specs {
            let Some((file, name)) = spec.rsplit_once('%') else {
                return Err(fail(stderr, vec![format!("ERROR: Aspect '{spec}' is unknown")]));
            };
            let problem = match Self::canonical_option_label(loader, file, &rel) {
                Err(e) => Some(e),
                Ok(label) => match loader.load_bzl_exports(&label) {
                    Err(e) => Some(e),
                    Ok(exports) => match exports.get(name) {
                        Some(v) if zel_packages::aspect::aspect_of(v).is_some() => None,
                        Some(_) => Some(format!("{name} is not an aspect")),
                        None => Some(format!("{name} is not exported from {}", label.canonical_form())),
                    },
                },
            };
            if let Some(msg) = problem {
                let mut pats = Vec::new();
                if let Ok(e) = zel_query::parse::parse(expr, &zel_query::parse::all_functions()) {
                    e.collect_target_patterns(&mut pats);
                }
                let target = pats.first().cloned().unwrap_or_default();
                let params = crate::loading::opt_list(parsed, "aspects_parameters").join(", ");
                return Err(fail(
                    stderr,
                    vec![
                        format!("ERROR: {msg}"),
                        format!(
                            "ERROR: Analysis of aspects '[{}] with parameters {{{params}}} on {target}' failed; build aborted: {msg}",
                            specs.join(", ")
                        ),
                    ],
                ));
            }
        }
        Ok(())
    }
}
