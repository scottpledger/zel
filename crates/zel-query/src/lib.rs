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

//! Bazel Query, Cquery, and Aquery: language parser, evaluation engine, environments and
//! output formatters. Mirrors `src/main/java/com/google/devtools/build/lib/query2`.

pub mod action_key;
pub mod aquery;
pub mod engine;
pub mod format;
pub mod java_order;
pub mod parse;
pub mod pb;
pub mod post_analysis;
pub mod regex;
pub mod starlark_out;
pub mod target_env;

#[cfg(test)]
mod tests;

use engine::{Engine, QueryEnv};
use format::FormatOpts;
use target_env::{QueryOpts, TargetEnv};
use zel_packages::PatternResolver;

/// Exit codes used by the query commands.
pub mod exit {
    pub const SUCCESS: i32 = 0;
    pub const COMMAND_LINE_ERROR: i32 = 2;
    pub const PARTIAL_ANALYSIS_FAILURE: i32 = 3;
    pub const ANALYSIS_FAILURE: i32 = 7;
    pub const BUILD_FAILURE_CODE: i32 = 1;
}

/// Everything a finished query command produces.
#[derive(Debug, Default)]
pub struct QueryOutcome {
    pub stdout: Vec<u8>,
    /// Lines for stderr (without trailing newline), already prefixed (`ERROR: `, `INFO: `, ...).
    pub stderr: Vec<String>,
    pub exit_code: i32,
}

/// `OutputFormatters.formatterNames`.
pub fn formatter_names() -> String {
    format::FORMATTERS.join(", ")
}

/// Runs a `query` command.
pub fn run_query(
    resolver: &PatternResolver,
    expr_str: &str,
    opts: &QueryOpts,
    fmt: &FormatOpts,
) -> QueryOutcome {
    let mut outcome = QueryOutcome::default();
    let mut opts = opts.clone();
    let streamed = format::is_streamed(&fmt.output)
        && matches!(
            fmt.order,
            format::OrderOutput::No | format::OrderOutput::Auto
        );
    opts.sky = streamed && (!opts.universe_scope.is_empty() || opts.infer_universe_scope);
    // `QueryEnvironmentBasedCommand`: YES requires a streaming formatter and ordering.
    if opts.graphless_query == Some(true) && !streamed {
        outcome.stderr.push(format!(
            "ERROR: --experimental_graphless_query requires --order_output=no or --order_output=auto and an --output option that supports streaming; valid values are: {}",
            format::FORMATTERS.iter().filter(|f| format::is_streamed(f)).cloned().collect::<Vec<_>>().join(", ")
        ));
        outcome.exit_code = exit::COMMAND_LINE_ERROR;
        return outcome;
    }
    let graphless = streamed && !opts.sky && opts.graphless_query != Some(false);
    let opts = &opts;
    let mut env = TargetEnv::new(resolver, opts.clone());
    env.graphless = graphless;
    env.output_format = fmt.output.clone();
    env.order_output = Some(fmt.order);
    let functions = env.functions();
    let expr = match parse::parse(expr_str, &functions) {
        Ok(e) => e,
        Err(e) => {
            outcome.stderr.push(format!(
                "ERROR: Error while parsing '{}': {}",
                parse::truncate(expr_str),
                e.0
            ));
            outcome.exit_code = exit::COMMAND_LINE_ERROR;
            return outcome;
        }
    };
    if !format::FORMATTERS.contains(&fmt.output.as_str()) {
        outcome.stderr.push(format!(
            "ERROR: Invalid output format '{}'. Valid values are: {}",
            fmt.output,
            formatter_names()
        ));
        outcome.exit_code = exit::COMMAND_LINE_ERROR;
        return outcome;
    }
    if fmt.output == "location" {
        let mut names = std::collections::BTreeSet::new();
        expr.function_names(&mut names);
        if names.contains("buildfiles") || names.contains("loadfiles") {
            outcome.stderr.push(
                "ERROR: Query expressions involving 'buildfiles' or 'loadfiles' cannot be used with --output=location"
                    .to_string(),
            );
            outcome.exit_code = exit::COMMAND_LINE_ERROR;
            return outcome;
        }
    }
    if fmt.order != format::OrderOutput::Auto
        && fmt.order != format::OrderOutput::Full
        && fmt.order != format::OrderOutput::Deps
    {
        // `no` is accepted by all formatters that stream; non-streaming formatters ignore it.
    }
    env.toplevel = expr.to_truncated_string();
    let mut patterns = Vec::new();
    expr.collect_target_patterns(&mut patterns);
    let patterns_for_preload = patterns.clone();
    let universe: Vec<String> = if opts.universe_scope.is_empty() {
        patterns
    } else {
        opts.universe_scope.clone()
    };

    let eval_result;
    let mut diag_errors: Vec<String> = Vec::new();
    let mut had_errors = false;
    let missing_graph: Vec<String>;
    {
        let mut engine = Engine::new(&mut env, opts.keep_going, universe);
        engine.diag.graphless = graphless;
        let preload = engine
            .env
            .preload_patterns(&patterns_for_preload, &mut engine.diag);
        eval_result = match preload {
            Ok(()) => {
                if opts.sky && !opts.universe_scope.is_empty() {
                    match engine.universe(&expr) {
                        Ok(_) => engine.eval(&expr),
                        Err(e) => Err(e),
                    }
                } else {
                    engine.eval(&expr)
                }
            }
            Err(e) => Err(e),
        };
        diag_errors.append(&mut engine.diag.errors);
        had_errors |= engine.diag.had_error;
        missing_graph = std::mem::take(&mut engine.diag.missing_graph);
    }
    if !missing_graph.is_empty() {
        outcome.stderr.push(format!(
            "WARNING: Targets were missing from graph: [{}]",
            missing_graph.join(", ")
        ));
    }
    // Error events of loaded packages (`ErrorSensingEventHandler`) make the evaluation fail
    // (`--nokeep_going`) or partial (`--keep_going`).
    let mut event_errors = false;
    for ev in env.take_events() {
        event_errors |= ev.kind == zel_packages::lookup::EventKind::Error;
        outcome.stderr.push(ev.render());
    }
    had_errors |= event_errors;
    match eval_result {
        Err(e) => {
            for l in diag_errors {
                outcome.stderr.push(format!("ERROR: {l}"));
            }
            outcome.stderr.push(format!("ERROR: {}", e.describe(&expr)));
            outcome.exit_code = exit::ANALYSIS_FAILURE;
            return outcome;
        }
        Ok(_) if event_errors && !opts.keep_going => {
            for l in diag_errors {
                outcome.stderr.push(format!("ERROR: {l}"));
            }
            outcome.stderr.push(format!(
                "ERROR: Evaluation of query \"{}\" failed",
                expr.to_truncated_string()
            ));
            outcome.exit_code = exit::ANALYSIS_FAILURE;
            return outcome;
        }
        Ok(set) => {
            for e in diag_errors {
                outcome.stderr.push(format!("ERROR: {e}"));
            }
            let nodes = set.to_vec();
            match format::format_targets(&env, &nodes, fmt, expr.is_somepath()) {
                Ok(bytes) => outcome.stdout = bytes,
                Err(e) => {
                    outcome.stderr.push(format!("ERROR: {e}"));
                    outcome.exit_code = exit::ANALYSIS_FAILURE;
                    return outcome;
                }
            }
            // The `--keep_going` warning is emitted by `evaluateQueryInternal`; the command
            // reports `Empty results` afterwards.
            if had_errors {
                outcome.stderr.push(
                    "WARNING: --keep_going specified, ignoring errors. Results may be inaccurate"
                        .to_string(),
                );
                outcome.exit_code = exit::PARTIAL_ANALYSIS_FAILURE;
            }
            if nodes.is_empty() {
                outcome.stderr.push("INFO: Empty results".to_string());
            }
        }
    }
    outcome
}

/// The `genquery` rule's `GenQuery.executeQuery`: evaluates `expression` over everything the loader
/// can see and prints it with the `--output` formatter of `opts` (default `label`). Options follow
/// `GenQuery.java`: `--keep_going`, `--universe_scope`, `--order_*` and `--experimental_graphless_query`
/// The `genquery` rule's `GenQuery.executeQuery`: evaluates `expression` over the `deps(scope)`
/// universe (`GenQueryPackageProvider`) and prints it with the `--output` formatter of `opts`
/// (default `label`). Options follow `GenQuery.java`: `--keep_going`, `--universe_scope`,
/// `--order_*` and `--experimental_graphless_query` are rejected, locations are relative,
/// results are ordered, and `nodep_deps` defaults to off.
pub fn run_genquery(
    loader: &std::sync::Arc<zel_packages::PackageLoader>,
    expression: &str,
    scope: &[String],
    strict: bool,
    opts: &[String],
) -> Result<Vec<u8>, String> {
    fn parse_bool_opt(raw: &str, val: &str) -> Result<bool, String> {
        match val {
            "true" | "1" | "yes" => Ok(true),
            "false" | "0" | "no" => Ok(false),
            _ => Err(format!(
                "error while parsing query options: '{raw}': Not a valid boolean: '{val}'"
            )),
        }
    }

    let mut qopts = QueryOpts::default();
    let mut fmt = FormatOpts::default();
    qopts.nodep_deps = false;
    fmt.nodep_deps = false;
    fmt.relative_locations = true;
    fmt.order = format::OrderOutput::Full;
    for o in opts {
        let o = o.as_str();
        match o {
            "--keep_going" | "--nokeep_going" => {
                return Err("option --keep_going is not allowed".to_string())
            }
            _ if o.starts_with("--keep_going=") => {
                return Err("option --keep_going is not allowed".to_string())
            }
            "--implicit_deps" => qopts.implicit_deps = true,
            "--noimplicit_deps" => qopts.implicit_deps = false,
            _ if o.starts_with("--implicit_deps=") => {
                qopts.implicit_deps = parse_bool_opt(o, &o["--implicit_deps=".len()..])?;
            }
            "--tool_deps" | "--host_deps" => qopts.tool_deps = true,
            "--notool_deps" | "--nohost_deps" => qopts.tool_deps = false,
            _ if o.starts_with("--tool_deps=") => {
                qopts.tool_deps = parse_bool_opt(o, &o["--tool_deps=".len()..])?;
            }
            _ if o.starts_with("--host_deps=") => {
                qopts.tool_deps = parse_bool_opt(o, &o["--host_deps=".len()..])?;
            }
            "--nodep_deps" => qopts.nodep_deps = true,
            "--nonodep_deps" => qopts.nodep_deps = false,
            _ if o.starts_with("--nodep_deps=") => {
                qopts.nodep_deps = parse_bool_opt(o, &o["--nodep_deps=".len()..])?;
            }
            "--consistent_labels" => fmt.printer.consistent = true,
            "--noconsistent_labels" => fmt.printer.consistent = false,
            _ if o.starts_with("--consistent_labels=") => {
                fmt.printer.consistent = parse_bool_opt(o, &o["--consistent_labels=".len()..])?;
            }
            "--strict_test_suite" => qopts.strict_test_suite = true,
            "--nostrict_test_suite" => qopts.strict_test_suite = false,
            _ if o.starts_with("--strict_test_suite=") => {
                qopts.strict_test_suite = parse_bool_opt(o, &o["--strict_test_suite=".len()..])?;
            }
            "--output:display_full_kind" => fmt.display_full_kind = true,
            "--nooutput:display_full_kind" => fmt.display_full_kind = false,
            "--incompatible_package_group_includes_double_slash" => {
                fmt.package_group_includes_double_slash = true
            }
            "--noincompatible_package_group_includes_double_slash" => {
                fmt.package_group_includes_double_slash = false
            }
            "--incompatible_package_group_build_output" => fmt.package_group_build_output = true,
            "--noincompatible_package_group_build_output" => fmt.package_group_build_output = false,
            "--line_terminator_null" => fmt.line_terminator = "\0".into(),
            "--noline_terminator_null" => fmt.line_terminator = "\n".into(),
            "--proto:locations" => fmt.proto_locations = true,
            "--noproto:locations" => fmt.proto_locations = false,
            "--proto:default_values" => fmt.proto_default_values = true,
            "--noproto:default_values" => fmt.proto_default_values = false,
            "--proto:flatten_selects" => fmt.proto_flatten_selects = true,
            "--noproto:flatten_selects" => fmt.proto_flatten_selects = false,
            "--proto:rule_inputs_and_outputs" => fmt.proto_rule_inputs_and_outputs = true,
            "--noproto:rule_inputs_and_outputs" => fmt.proto_rule_inputs_and_outputs = false,
            "--proto:instantiation_stack" => fmt.proto_instantiation_stack = true,
            "--noproto:instantiation_stack" => fmt.proto_instantiation_stack = false,
            "--proto:definition_stack" => fmt.proto_definition_stack = true,
            "--noproto:definition_stack" => fmt.proto_definition_stack = false,
            "--proto:include_attribute_source_aspects" => {
                fmt.proto_include_attribute_source_aspects = true
            }
            "--noproto:include_attribute_source_aspects" => {
                fmt.proto_include_attribute_source_aspects = false
            }
            "--proto:rule_classes" => fmt.proto_rule_classes = true,
            "--noproto:rule_classes" => fmt.proto_rule_classes = false,
            "--proto:include_synthetic_attribute_hash" => {
                fmt.proto_include_synthetic_attribute_hash = true
            }
            "--noproto:include_synthetic_attribute_hash" => {
                fmt.proto_include_synthetic_attribute_hash = false
            }
            "--proto:include_starlark_rule_env" => fmt.proto_include_starlark_rule_env = true,
            "--noproto:include_starlark_rule_env" => fmt.proto_include_starlark_rule_env = false,
            "--xml:line_numbers" => fmt.xml_line_numbers = true,
            "--noxml:line_numbers" => fmt.xml_line_numbers = false,
            "--xml:default_values" => fmt.xml_default_values = true,
            "--noxml:default_values" => fmt.xml_default_values = false,
            "--graph:factored" => fmt.graph_factored = true,
            "--nograph:factored" => fmt.graph_factored = false,
            _ if o.starts_with("--graph:factored=") => {
                fmt.graph_factored = parse_bool_opt(o, &o["--graph:factored=".len()..])?;
            }
            "--relative_locations" | "--norelative_locations" => {}
            _ if o.starts_with("--output=") => fmt.output = o["--output=".len()..].to_string(),
            _ if o.starts_with("--aspect_deps=") => {
                let m = o["--aspect_deps=".len()..].to_string();
                qopts.aspect_deps = m.clone();
                fmt.aspect_deps = m;
            }
            _ if o.starts_with("--graph:node_limit=") => {
                if let Ok(v) = o["--graph:node_limit=".len()..].parse::<i32>() {
                    fmt.graph_node_limit = v;
                }
            }
            _ if o.starts_with("--graph:conditional_edges_limit=") => {
                if let Ok(v) = o["--graph:conditional_edges_limit=".len()..].parse::<i32>() {
                    fmt.graph_conditional_edges_limit = v;
                }
            }
            _ if o.starts_with("--universe_scope") => {
                return Err("option --universe_scope is not allowed".to_string())
            }
            _ if o.starts_with("--order_output") => {
                return Err("option --order_output is not allowed".to_string())
            }
            _ if o.contains("order_results") => {
                return Err(format!(
                    "option --{} is not allowed",
                    o.trim_start_matches("--")
                ))
            }
            _ if o.contains("graphless_query") => {
                return Err("option --experimental_graphless_query is not allowed".to_string())
            }
            _ => {
                return Err(format!(
                    "error while parsing query options: Unrecognized option: {o}"
                ))
            }
        }
    }
    if !format::FORMATTERS.contains(&fmt.output.as_str()) {
        return Err(format!(
            "Invalid output format '{}'. Valid values are: {}",
            fmt.output,
            formatter_names()
        ));
    }
    fmt.implicit_deps = qopts.implicit_deps;
    fmt.tool_deps = qopts.tool_deps;
    fmt.nodep_deps = qopts.nodep_deps;
    let graphless = format::is_streamed(&fmt.output);
    fmt.order = if graphless {
        format::OrderOutput::Auto
    } else {
        format::OrderOutput::Full
    };

    let resolver = PatternResolver::new(loader.clone(), zel_vfs::PathFragment::empty());
    let scope_probe = TargetEnv::new(&resolver, QueryOpts::default());
    let genquery_scope = scope_probe.compute_genquery_scope(scope, strict)?;

    let mut env = TargetEnv::new(&resolver, qopts);
    env.graphless = graphless;
    env.output_format = fmt.output.clone();
    env.order_output = Some(fmt.order);
    env.genquery_scope = Some(genquery_scope);

    let funcs = env.functions();
    let expr =
        parse::parse(expression, &funcs).map_err(|e| format!("query syntax error: {}", e.0))?;
    if fmt.output == "location" {
        let mut names = std::collections::BTreeSet::new();
        expr.function_names(&mut names);
        if names.contains("buildfiles") || names.contains("loadfiles") {
            return Err(
                "query failed: Query expressions involving 'buildfiles' or 'loadfiles' cannot be used with --output=location"
                    .to_string(),
            );
        }
    }

    env.toplevel = expr.to_truncated_string();
    let mut patterns = Vec::new();
    expr.collect_target_patterns(&mut patterns);
    let patterns_for_preload = patterns.clone();
    let eval_result = {
        let mut engine = Engine::new(&mut env, false, patterns);
        engine.diag.graphless = graphless;
        match engine
            .env
            .preload_patterns(&patterns_for_preload, &mut engine.diag)
        {
            Ok(()) => engine.eval(&expr),
            Err(e) => Err(e),
        }
    };
    let events = env.take_events();
    let event_errors = events
        .iter()
        .any(|ev| ev.kind == zel_packages::lookup::EventKind::Error);
    match eval_result {
        Err(e) => Err(format!("query failed: {}", e.describe(&expr))),
        Ok(_) if event_errors => Err(format!(
            "query failed: Evaluation of query \"{}\" failed",
            expr.to_truncated_string()
        )),
        Ok(set) => {
            let nodes = set.to_vec();
            format::install_printer(&fmt.printer);
            format::format_targets(&env, &nodes, &fmt, expr.is_somepath())
        }
    }
}
