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

//! The `query` / `cquery` / `aquery` commands (`QueryCommand`, `CqueryCommand`,
//! `AqueryCommand`, `QueryEnvironmentBasedCommand`, `QueryOptionHelper`).

use super::loading::opt_list;
use super::*;
use zel_packages::PatternResolver;
use zel_query::format::{FormatOpts, LabelPrinter, OrderOutput};
use zel_query::target_env::QueryOpts;

/// Reads a boolean option with a default.
fn flag(parsed: &ParsedCommandOptions, name: &str, default: bool) -> bool {
    parsed.get_bool(name).unwrap_or(default)
}

fn int_flag(parsed: &ParsedCommandOptions, name: &str, default: i32) -> i32 {
    parsed.get_int(name).unwrap_or(default)
}

/// `QueryOptionHelper.readQuery`: joins the residue with spaces, or reads `--query_file`.
pub(crate) fn read_query(parsed: &ParsedCommandOptions, cwd: &std::path::Path, cmd_kind: &str, allow_empty: bool) -> Result<String, String> {
    let query_file = parsed.get_str("query_file").unwrap_or("");
    if !parsed.targets.is_empty() {
        if !query_file.is_empty() {
            return Err("Command-line query and --query_file cannot both be specified".to_string());
        }
        return Ok(parsed.targets.join(" "));
    }
    if !query_file.is_empty() {
        let path = cwd.join(query_file);
        return match std::fs::read(&path) {
            // The file is read as ISO-8859-1 by Bazel.
            Ok(bytes) => Ok(bytes.iter().map(|&b| b as char).collect()),
            Err(_) => Err(format!("I/O error reading from {}", path.display())),
        };
    }
    let _ = cmd_kind;
    if allow_empty {
        return Ok(String::new());
    }
    Err("missing query expression. Type 'bazel help query' for syntax and help".to_string())
}

impl BlazeRuntime {
    /// Option values shared by all three flavours.
    pub(crate) fn query_opts(&self, parsed: &ParsedCommandOptions) -> QueryOpts {
        QueryOpts {
            keep_going: parsed.keep_going,
            implicit_deps: flag(parsed, "implicit_deps", true),
            tool_deps: if let Some(b) = parsed.get_bool("host_deps") {
                b
            } else {
                flag(parsed, "tool_deps", true)
            },
            include_aspects: flag(parsed, "include_aspects", true),
            strict_test_suite: flag(parsed, "strict_test_suite", false),
            universe_scope: opt_list(parsed, "universe_scope"),
            infer_universe_scope: flag(parsed, "infer_universe_scope", false),
            package_group_includes_double_slash: flag(parsed, "incompatible_package_group_includes_double_slash", true),
            nodep_deps: flag(parsed, "nodep_deps", true),
            aspect_deps: parsed.get_str("aspect_deps").unwrap_or("conservative").to_string(),
            sky: false,
            graphless_query: match parsed.parsed_options.get("experimental_graphless_query").map(|d| &d.value) {
                Some(zel_options::OptionValue::TriState(zel_options::TriState::Yes)) => Some(true),
                Some(zel_options::OptionValue::TriState(zel_options::TriState::No)) => Some(false),
                _ => None,
            },
        }
    }

    pub(crate) fn query_format_opts(&self, parsed: &ParsedCommandOptions, resolver: &PatternResolver, default_output: &str) -> FormatOpts {
        let output = match parsed.parsed_options.get("output") {
            Some(d) => d.value.as_str().unwrap_or(default_output).to_string(),
            None => default_output.to_string(),
        };
        let order = match parsed.get_str("order_output").and_then(OrderOutput::parse) {
            Some(o) => o,
            None => match parsed.get_bool("order_results") {
                Some(true) => OrderOutput::Auto,
                Some(false) => OrderOutput::No,
                None => OrderOutput::Auto,
            },
        };
        let mut printer = LabelPrinter { consistent: flag(parsed, "consistent_labels", false), ..Default::default() };
        for (apparent, canonical) in resolver.main_mapping.entries() {
            printer.apparent.entry(canonical.name().to_string()).or_insert_with(|| apparent.clone());
        }
        // Every repository mapping contains `bazel_tools`.
        printer.apparent.entry("bazel_tools".to_string()).or_insert_with(|| "bazel_tools".to_string());
        let attrs = {
            let l = opt_list(parsed, "proto:output_rule_attrs");
            if l.is_empty() && !parsed.parsed_options.contains_key("proto:output_rule_attrs") {
                vec!["all".to_string()]
            } else {
                l
            }
        };
        FormatOpts {
            output,
            printer,
            order,
            line_terminator: if flag(parsed, "line_terminator_null", false) { "\0".into() } else { "\n".into() },
            relative_locations: flag(parsed, "relative_locations", false),
            display_full_kind: flag(parsed, "output:display_full_kind", false),
            implicit_deps: flag(parsed, "implicit_deps", true),
            tool_deps: if let Some(b) = parsed.get_bool("host_deps") {
                b
            } else {
                flag(parsed, "tool_deps", true)
            },
            package_group_includes_double_slash: flag(parsed, "incompatible_package_group_includes_double_slash", true),
            package_group_build_output: flag(parsed, "incompatible_package_group_build_output", false),
            proto_locations: flag(parsed, "proto:locations", true),
            proto_default_values: flag(parsed, "proto:default_values", true),
            proto_flatten_selects: flag(parsed, "proto:flatten_selects", true),
            proto_output_rule_attrs: attrs,
            proto_rule_inputs_and_outputs: flag(parsed, "proto:rule_inputs_and_outputs", true),
            proto_instantiation_stack: flag(parsed, "proto:instantiation_stack", false),
            proto_definition_stack: flag(parsed, "proto:definition_stack", false),
            proto_include_attribute_source_aspects: flag(parsed, "proto:include_attribute_source_aspects", false),
            proto_rule_classes: flag(parsed, "proto:rule_classes", false),
            proto_include_synthetic_attribute_hash: flag(parsed, "proto:include_synthetic_attribute_hash", false),
            proto_include_starlark_rule_env: flag(parsed, "proto:include_starlark_rule_env", false),
            nodep_deps: flag(parsed, "nodep_deps", true),
            include_aspects: flag(parsed, "include_aspects", true),
            xml_line_numbers: flag(parsed, "xml:line_numbers", true),
            xml_default_values: flag(parsed, "xml:default_values", false),
            graph_node_limit: int_flag(parsed, "graph:node_limit", 512),
            graph_factored: flag(parsed, "graph:factored", true),
            graph_conditional_edges_limit: int_flag(parsed, "graph:conditional_edges_limit", 4),
            ..FormatOpts::default()
        }
    }

    /// `query`/`cquery`/`aquery` entry point.
    pub(crate) fn cmd_query(
        &self,
        cmd_kind: &str,
        args: &[String],
        stdout: &mut Vec<u8>,
        stderr: &mut Vec<u8>,
    ) -> ExitCode {
        let parsed = match ParsedCommandOptions::parse_for_command(cmd_kind, args) {
            Ok(p) => p,
            Err(e) => {
                writeln_bytes(stderr, &e.format_bazel_error());
                return ExitCode::COMMAND_LINE_ERROR;
            }
        };
        let cwd = self.command_cwd(&parsed);
        let expr_str = match read_query(&parsed, &cwd, cmd_kind, cmd_kind == "aquery" && parsed.get_bool("skyframe_state").unwrap_or(false)) {
            Ok(q) => q,
            Err(_) => {
                // `QueryEnvironmentBasedCommand` returns `BlazeCommandResult.failureDetail(..)`
                // for `QueryException`s of `QueryOptionHelper.readQuery` without reporting them.
                return ExitCode::COMMAND_LINE_ERROR;
            }
        };
        match cmd_kind {
            "query" => {
                emit_deprecation_warnings(cmd_kind, args, stderr);
                let resolver = self.pattern_resolver(&parsed);
                let opts = self.query_opts(&parsed);
                let fmt = self.query_format_opts(&parsed, &resolver, "label");
                let outcome = zel_query::run_query(&resolver, &expr_str, &opts, &fmt);
                let output_file = parsed.get_str("output_file").filter(|s| !s.is_empty());
                if let Some(out_path) = output_file {
                    // `QueryRuntimeHelper.FileQueryRuntimeHelper.create`: the file is opened (not
                    // created in missing directories) before the query is evaluated.
                    let file_path = cwd.join(out_path);
                    if let Err(code) = write_query_output_file(&file_path, &outcome.stdout, stderr) {
                        return code;
                    }
                } else if parsed.get_bool("experimental_ui_debug_all_events").unwrap_or(false) {
                    writeln_bytes(stdout, "INFO <no location>: Transports announced:");
                    let text = String::from_utf8_lossy(&outcome.stdout);
                    stdout.extend_from_slice(format!("STDOUT <no location>: {text}\n").as_bytes());
                } else {
                    stdout.extend_from_slice(&outcome.stdout);
                }
                for l in &outcome.stderr {
                    writeln_bytes(stderr, l);
                }
                exit_from_code(outcome.exit_code)
            }
            _ => self.cmd_post_analysis_query(cmd_kind, &parsed, &expr_str, stdout, stderr),
        }
    }
}

/// Writes `--output_file` like `QueryRuntimeHelper.FileQueryRuntimeHelper`: failing to open the
/// file is `ERROR: Could not open query output file <path>` with exit code 36
/// (`QUERY_OUTPUT_WRITE_FAILURE`, a local environmental error).
pub(crate) fn write_query_output_file(
    path: &std::path::Path,
    data: &[u8],
    stderr: &mut Vec<u8>,
) -> Result<(), ExitCode> {
    use std::io::Write;
    match std::fs::File::create(path) {
        Ok(mut f) => {
            if f.write_all(data).is_err() {
                writeln_bytes(stderr, &format!("ERROR: Could not close query output file {}", path.display()));
                return Err(ExitCode::LOCAL_ENVIRONMENTAL_ERROR);
            }
            Ok(())
        }
        Err(_) => {
            writeln_bytes(stderr, &format!("ERROR: Could not open query output file {}", path.display()));
            Err(ExitCode::LOCAL_ENVIRONMENTAL_ERROR)
        }
    }
}

/// `@Option(deprecationWarning = ...)` texts extracted from the HEAD sources (the generated flag
/// tables carry none).
const DEPRECATION_WARNINGS: &[(&str, &str)] = &[
    ("announce", "This option is now deprecated and is a no-op"),
    ("bes_best_effort", "BES best effort upload has been removed. The flag has no more functionality attached to it and will be removed in a future release."),
    ("build_python_zip", "The '--no' prefix is no longer supported for this flag. Please use --build_python_zip=false instead."),
    ("check_fileset_dependencies_recursively", "This flag is a no-op and fileset dependencies are always checked to ensure correctness of builds."),
    ("experimental_sibling_repository_layout", "This flag is a no-op. Non-main repositories are always planted under $output_base/execution_root/_main/external."),
    ("experimental_skyframe_native_filesets", "This flag is a no-op and skyframe-native-filesets is always true."),
    ("experimental_skyframe_prepare_analysis", "This flag is a no-op and will be deleted in a future release."),
    ("experimental_spawn_scheduler", "--experimental_spawn_scheduler is deprecated. Using dynamic execution for everything is rarely a good idea (see https://bazel.build/remote/dynamic). If you really want to enable dynamic execution globally, pass `--internal_spawn_scheduler --spawn_strategy=dynamic`."),
    ("fatal_event_bus_exceptions", "Will be enabled by default and removed soon"),
    ("incompatible_override_toolchain_transition", "This is now always set, please remove this flag."),
    ("noorder_results", "Please use --order_output=no or --order_output=auto instead of this flag"),
    ("order_results", "Please use --order_output=auto or --order_output=no instead of this flag"),
    ("propeller_optimize_absolute_cc_profile", "Deprecated. Use --propeller_optimize instead."),
    ("propeller_optimize_absolute_ld_profile", "Deprecated. Use --propeller_optimize instead."),
    ("use_target_platform_for_tests", "This flag is a no-op. Tests select an execution platform matching all constraints of the target platform by default. Make sure that all test target platforms are registered as execution platforms."),
];

/// `OptionsParserImpl` deprecation warnings: `Option 'x' is deprecated: <warning>` for options
/// with a `deprecationWarning`, and `... Use --<name> instead` for options set by their `oldName`.
pub(crate) fn emit_deprecation_warnings(command: &str, args: &[String], stderr: &mut Vec<u8>) {
    use zel_options::ALL_COMMAND_OPTIONS;
    for a in args {
        if a == "--" {
            break;
        }
        let Some(body) = a.strip_prefix("--") else { continue };
        let name = body.split('=').next().unwrap_or("");
        let mut candidates = vec![name.to_string()];
        if let Some(n) = name.strip_prefix("no") {
            candidates.push(n.to_string());
        }
        'cand: for cand in &candidates {
            if cand == "test_result_expiration" {
                writeln_bytes(stderr, "WARNING: Option 'test_result_expiration' is deprecated");
                break 'cand;
            }
            if let Some((_, w)) = DEPRECATION_WARNINGS.iter().find(|(k, _)| k == cand) {
                if ALL_COMMAND_OPTIONS.iter().any(|d| d.name == cand.as_str() && d.commands.contains(&command)) {
                    writeln_bytes(stderr, &format!("WARNING: Option '{cand}' is deprecated: {w}"));
                    break 'cand;
                }
            }
            for d in ALL_COMMAND_OPTIONS.iter() {
                if !d.commands.contains(&command) {
                    continue;
                }
                if d.name == cand {
                    if let Some(w) = d.deprecation_warning {
                        writeln_bytes(stderr, &format!("WARNING: Option '{}' is deprecated: {w}", d.name));
                    }
                    break 'cand;
                }
                if d.old_name == Some(cand.as_str()) {
                    writeln_bytes(stderr, &format!("WARNING: Option '{cand}' is deprecated: Use --{} instead", d.name));
                    break 'cand;
                }
            }
        }
    }
}

pub(crate) fn exit_from_code(code: i32) -> ExitCode {
    match code {
        0 => ExitCode::SUCCESS,
        1 => ExitCode::BUILD_FAILURE,
        2 => ExitCode::COMMAND_LINE_ERROR,
        3 => ExitCode::PARTIAL_ANALYSIS_FAILURE,
        7 => ExitCode::ANALYSIS_FAILURE,
        other => ExitCode::new(other, "QUERY_FAILURE", false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_vfs::InMemoryFileSystem;

    fn runtime(files: &[(&str, &str)]) -> BlazeRuntime {
        let fs = Arc::new(InMemoryFileSystem::default());
        let ws = PathFragment::create("/workspace");
        // A private output base per runtime: tests run in parallel (and other cargo runs share /tmp).
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let uniq = format!("{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
        let ob = PathFragment::create(&format!("/tmp/ob-query-cmd-{uniq}"));
        fs.create_directory_and_parents(&ws).unwrap();
        fs.create_directory_and_parents(&ob).unwrap();
        for (p, c) in files {
            let path = if p.starts_with('/') { PathFragment::create(p) } else { ws.get_relative_str(p) };
            fs.create_directory_and_parents(&path.parent_directory().unwrap()).unwrap();
            fs.write_file(&path, c.as_bytes()).unwrap();
        }
        let dirs = BlazeDirectories {
            workspace_dir: ws,
            output_base: ob,
            install_base: PathFragment::create(&format!("/tmp/ib-query-cmd-{uniq}")),
            output_user_root: PathFragment::create(&format!("/tmp/ur-query-cmd-{uniq}")),
        };
        BlazeRuntime::new(dirs, fs as Arc<dyn FileSystem>, 1)
    }

    struct Out {
        code: i32,
        stdout: Vec<u8>,
        stderr: String,
    }

    impl Out {
        fn out(&self) -> String {
            String::from_utf8_lossy(&self.stdout).to_string()
        }
        fn lines(&self) -> Vec<String> {
            self.out().lines().map(|s| s.to_string()).collect()
        }
    }

    fn run(rt: &BlazeRuntime, args: &[&str]) -> Out {
        let req = zel_proto::command_server::RunRequest {
            cookie: "c".into(),
            arg: args.iter().map(|a| a.as_bytes().to_vec()).collect(),
            client_description: "test".into(),
            ..Default::default()
        };
        let r = rt.dispatch_command(&req, "r");
        Out { code: r.exit_code, stdout: r.standard_output, stderr: String::from_utf8_lossy(&r.standard_error).to_string() }
    }

    fn tree() -> BlazeRuntime {
        runtime(&[
            (
                "lib/BUILD",
                "genrule(name = 'l', srcs = ['data.txt'], outs = ['l.out'], cmd = 'cat $(SRCS) > $@', visibility = ['//visibility:public'])\n",
            ),
            ("lib/data.txt", "hi"),
            (
                "app/BUILD",
                "genrule(name = 'app', srcs = ['//lib:l', 'in.txt'], outs = ['app.out'], cmd = 'cat $(SRCS) > $@')\n\
                 filegroup(name = 'fg', srcs = [':app'])\n",
            ),
            ("app/in.txt", "x"),
        ])
    }

    #[test]
    fn query_joins_residue_words_with_spaces() {
        let rt = tree();
        let o = run(&rt, &["query", "//lib:l", "+", "//app:app"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert_eq!(o.lines(), vec!["//app:app", "//lib:l"]);
    }

    #[test]
    fn query_syntax_error_and_missing_expression() {
        let rt = tree();
        let o = run(&rt, &["query", "deps("]);
        assert_eq!(o.code, 2);
        assert!(o.stderr.contains("ERROR: Error while parsing 'deps(': premature end of input"), "{}", o.stderr);
        let o = run(&rt, &["query"]);
        // `QueryOptionHelper.readQuery` failures are not reported (usage text only).
        assert_eq!(o.code, 2);
        let o = run(&rt, &["query", "--output=bogus", "//lib:l"]);
        assert_eq!(o.code, 2);
        assert!(o.stderr.contains("Invalid output format 'bogus'. Valid values are: label, label_kind, build, minrank, maxrank, package, location, graph, xml, proto, streamed_jsonproto, streamed_proto"), "{}", o.stderr);
    }

    #[test]
    fn query_cross_package_and_no_false_literal_on_empty() {
        let rt = tree();
        // The in-memory workspace has no `@bazel_tools` repository (genrule's implicit dep).
        let o = run(&rt, &["query", "--noimplicit_deps", "deps(//app:fg)"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert_eq!(o.lines(), vec!["//app:app", "//app:fg", "//app:in.txt", "//lib:data.txt", "//lib:l"]);
        let o = run(&rt, &["query", "//app:all - //app:all"]);
        assert_eq!(o.code, 0);
        assert!(o.stdout.is_empty());
        assert!(o.stderr.contains("INFO: Empty results"), "{}", o.stderr);
    }

    #[test]
    fn query_build_xml_proto_outputs() {
        let rt = tree();
        let o = run(&rt, &["query", "--output=build", "//lib:l"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        let want = "# /workspace/lib/BUILD:1:8\ngenrule(\n  name = \"l\",\n  visibility = [\"//visibility:public\"],\n  srcs = [\"//lib:data.txt\"],\n  outs = [\"//lib:l.out\"],\n  cmd = \"cat $(SRCS) > $@\",\n)\n# Rule l instantiated at (most recent call last):\n#   /workspace/lib/BUILD:1:8 in <toplevel>\n\n";
        assert_eq!(o.out(), want);
        let o = run(&rt, &["query", "--output=xml", "//lib:l"]);
        assert!(o.out().starts_with("<?xml version=\"1.1\" encoding=\"UTF-8\" standalone=\"no\"?>\n<query version=\"2\">\n    <rule class=\"genrule\" location=\"/workspace/lib/BUILD:1:8\" name=\"//lib:l\">") || o.out().starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?><query version=\"2\">\n    <rule class=\"genrule\" location=\"/workspace/lib/BUILD:1:8\" name=\"//lib:l\">"), "{}", o.out());
        assert!(o.out().contains("<list name=\"srcs\">\n            <label value=\"//lib:data.txt\"/>"), "{}", o.out());
        assert!(o.out().contains("<rule-input name=\"//lib:data.txt\"/>"), "{}", o.out());
        assert!(o.out().contains("<rule-output name=\"//lib:l.out\"/>"), "{}", o.out());
        let o = run(&rt, &["query", "--output=proto", "//lib:l"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        // QueryResult { target { type: RULE rule { name: ... } } }
        assert_eq!(o.stdout[0], 0x0a);
        assert!(String::from_utf8_lossy(&o.stdout).contains("//lib:l"));
        let o = run(&rt, &["query", "--output=streamed_jsonproto", "//lib:l"]);
        assert!(o.out().starts_with("{\"type\":\"RULE\",\"rule\":{\"name\":\"//lib:l\",\"ruleClass\":\"genrule\",\"location\":\"/workspace/lib/BUILD:1:8\""), "{}", o.out());
    }

    #[test]
    fn cquery_prints_label_and_configuration() {
        let rt = tree();
        let o = run(&rt, &["cquery", "//app:app"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        let l = o.lines();
        assert_eq!(l.len(), 1, "{l:?}");
        assert!(l[0].starts_with("//app:app ("), "{l:?}");
        assert!(l[0].ends_with(')') && !l[0].contains("fastbuild"), "{l:?}");
        let o = run(&rt, &["cquery", "--output=label_kind", "//app:all"]);
        assert!(o.lines()[0].starts_with("genrule rule //app:app ("), "{:?}", o.lines());
        assert!(o.lines()[1].starts_with("filegroup rule //app:fg ("), "{:?}", o.lines());
        let o = run(&rt, &["cquery", "deps(//app:app)"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        let ls = o.lines();
        assert!(ls.iter().any(|l| l.starts_with("//lib:l (")), "{ls:?}");
        assert!(ls.iter().any(|l| l == "//app:in.txt (null)"), "{ls:?}");
        let o = run(&rt, &["cquery", "--output=files", "//lib:l"]);
        assert!(o.out().trim().ends_with("/bin/lib/l.out"), "{}", o.out());
    }

    #[test]
    fn aquery_text_commands_summary_and_proto() {
        let rt = tree();
        let o = run(&rt, &["aquery", "//lib:l"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        let out = o.out();
        assert!(out.starts_with("action 'Executing genrule //lib:l'\n  Mnemonic: Genrule\n  Target: //lib:l\n  Configuration: "), "{out}");
        assert!(out.contains("  ActionKey: "), "{out}");
        assert!(out.contains("  Command Line: (exec "), "{out}");
        assert!(out.contains("\n# Configuration: "), "{out}");
        let o = run(&rt, &["aquery", "--output=summary", "//lib:l"]);
        assert!(o.out().starts_with("1 total action.\n\nMnemonics:\n  Genrule: 1\n"), "{}", o.out());
        let o = run(&rt, &["aquery", "--output=commands", "//lib:l"]);
        assert!(o.out().contains("cat lib/data.txt > "), "{}", o.out());
        let o = run(&rt, &["aquery", "--output=jsonproto", "//lib:l"]);
        assert!(o.out().contains("\"mnemonic\": \"Genrule\""), "{}", o.out());
        let o = run(&rt, &["aquery", "mnemonic('Nope', //lib:l)"]);
        assert_eq!(o.out(), "");
    }

    #[test]
    fn cquery_starlark_config_transitions_and_graph() {
        let rt = tree();
        let o = run(&rt, &["cquery", "--output=starlark", "--starlark:expr=target.label.name + ':' + str(len(providers(target)) > 0)", "//app:app"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert_eq!(o.lines(), vec!["app:True"]);
        let o = run(&rt, &["cquery", "--output=starlark", "--starlark:expr=1", "//app:app"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert_eq!(o.lines(), vec!["1"]);
        let o = run(&rt, &["cquery", "--output=starlark", "//app:app"]);
        // HEAD `StarlarkOutputFormatterCallback`: the default expression is `str(target.label)`.
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert_eq!(o.lines(), vec!["@@//app:app"], "{}", o.stderr);
        let o = run(&rt, &["cquery", "config(//lib:data.txt, null)"]);
        assert_eq!(o.lines(), vec!["//lib:data.txt (null)"], "{}", o.stderr);
        let o = run(&rt, &["cquery", "--output=transitions", "//app:app"]);
        assert_eq!(o.code, 0, "{}", o.stderr); // reported after `Found`, exit code stays 0
        assert!(o.stderr.contains("Instead of using --output=transitions, set the --transitions flag explicitly to 'lite' or 'full'"), "{}", o.stderr);
        let o = run(&rt, &["cquery", "--output=transitions", "--transitions=lite", "//app:app"]);
        let l = o.lines();
        assert!(l[0].starts_with("NoTransition -> //app:app ("), "{l:?}");
        assert!(l.iter().any(|x| x.starts_with("  srcs#//lib:l#NoTransition -> ")), "{l:?}");
        let o = run(&rt, &["cquery", "--output=graph", "--nograph:factored", "deps(//app:app)"]);
        assert!(o.out().starts_with("digraph mygraph {\n  node [shape=box];\n  \"//app:app ("), "{}", o.out());
        assert!(o.out().contains("\"//app:in.txt (null)\""), "{}", o.out());
        let o = run(&rt, &["cquery", "--output=streamed_proto", "//app:app"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert!(!o.stdout.is_empty());
        let o = run(&rt, &["cquery", "--output=textproto", "//app:app"]);
        assert!(o.out().starts_with("results {\n  target {\n    type: RULE\n    rule {\n      name: \"//app:app\""), "{}", o.out());
        assert!(o.out().contains("configurations {\n  id: 1\n"), "{}", o.out());
        // Setting --transitions=full implies --output=transitions automatically
        let o = run(&rt, &["cquery", "--transitions=full", "//app:app"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert!(o.out().starts_with("NoTransition -> //app:app ("), "{}", o.out());
        let o = run(&rt, &["cquery", "--show_config_fragments=direct", "//app:app"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert!(o.out().contains("[BazelRuleClassProvider$StrictActionEnvConfiguration, CoreOptions, PlatformConfiguration, PlatformOptions, ShellConfiguration]"), "{}", o.out());
    }

    #[test]
    fn query_output_file_redirects_stdout() {
        let rt = tree();
        let tmp = std::env::temp_dir();
        let tmp_file = tmp.join("zel_query_test_out.txt");
        let o = run(&rt, &["query", &format!("--output_file={}", tmp_file.display()), "//lib:l"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert!(o.stdout.is_empty(), "stdout must be empty when --output_file is set");
        let contents = std::fs::read_to_string(&tmp_file).unwrap();
        assert_eq!(contents.trim(), "//lib:l");
        let _ = std::fs::remove_file(&tmp_file);

        let tmp_cquery = tmp.join("zel_cquery_test_out.txt");
        let o = run(&rt, &["cquery", &format!("--output_file={}", tmp_cquery.display()), "//lib:l"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert!(o.stdout.is_empty(), "stdout must be empty when --output_file is set");
        let contents = std::fs::read_to_string(&tmp_cquery).unwrap();
        assert!(contents.starts_with("//lib:l ("));
        let _ = std::fs::remove_file(&tmp_cquery);

        let tmp_aquery = tmp.join("zel_aquery_test_out.txt");
        let o = run(&rt, &["aquery", &format!("--output_file={}", tmp_aquery.display()), "//lib:l"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert!(o.stdout.is_empty(), "stdout must be empty when --output_file is set");
        let contents = std::fs::read_to_string(&tmp_aquery).unwrap();
        assert!(contents.contains("action 'Executing genrule //lib:l'"));
        let _ = std::fs::remove_file(&tmp_aquery);
    }

    #[test]
    fn aquery_filters_and_summary_and_flags() {
        let rt = tree();
        let o = run(&rt, &["aquery", "inputs('.*data.txt', deps(//app:app))"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert_eq!(o.out().matches("Mnemonic:").count(), 1, "{}", o.out());
        assert!(o.out().contains("Target: //lib:l"), "{}", o.out());
        let o = run(&rt, &["aquery", "outputs('.*app.out', deps(//app:app))"]);
        assert!(o.out().contains("Target: //app:app") && !o.out().contains("Target: //lib:l"), "{}", o.out());
        let o = run(&rt, &["aquery", "--noinclude_artifacts", "//lib:l"]);
        assert!(!o.out().contains("Inputs:") && !o.out().contains("Outputs:"), "{}", o.out());
        let o = run(&rt, &["aquery", "--noinclude_commandline", "//lib:l"]);
        assert!(!o.out().contains("Command Line:"), "{}", o.out());
        let o = run(&rt, &["aquery", "--output=summary", "deps(//app:app)"]);
        assert!(o.out().starts_with("2 total actions.\n\nMnemonics:\n  Genrule: 2\n"), "{}", o.out());
        let o = run(&rt, &["aquery", "--output=nope", "//lib:l"]);
        assert_eq!(o.code, 2);
        assert!(o.stderr.contains("Invalid output format 'nope'. Valid values are: proto, streamed_proto, textproto, jsonproto, text, commands, summary"), "{}", o.stderr);
        let o = run(&rt, &["aquery", "--skyframe_state", "//lib:l"]);
        assert_eq!(o.code, 2);
        assert!(o.stderr.contains("Specifying build target(s) [//lib:l] with --skyframe_state is currently not supported."), "{}", o.stderr);
        let o = run(&rt, &["aquery", "--skyframe_state", "--output=proto"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        let b = run(&rt, &["build", "--nobuild", "//lib:l"]);
        assert_eq!(b.code, 0, "{}", b.stderr);
        let o = run(&rt, &["aquery", "--skyframe_state", "--output=textproto"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert!(o.out().contains("mnemonic: \"Genrule\""), "skyframe_state must dump actions from last build: {}", o.out());
        let o = run(&rt, &["aquery", "--skyframe_state", "--output=text"]);
        assert_eq!(o.code, 2);
        assert!(o.stderr.contains("--skyframe_state must be used with --output=proto|textproto|jsonproto."));
        let o = run(&rt, &["aquery", "--output=proto", "//lib:l"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert_eq!(o.stdout[0], 0x0a, "ActionGraphContainer starts with artifacts (field 1)");
    }
    #[test]
    fn round9_found_count_graphless_and_formatter_errors() {
        let rt = tree();
        // `Found N target(s)` counts the configured top-level targets once (not once per evaluation).
        let o = run(&rt, &["cquery", "deps(//app:app)"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert!(o.stderr.contains("INFO: Found 1 target..."), "{}", o.stderr);
        let o = run(&rt, &["cquery", "rdeps(//app:app, //lib:l)"]);
        assert!(o.stderr.contains("INFO: Found 2 targets..."), "{}", o.stderr); // two distinct patterns
        // aquery: unknown formatter is reported after `Found`, exit 2.
        let o = run(&rt, &["aquery", "--output=starlark", "//app:app"]);
        assert_eq!(o.code, 2, "{}", o.stderr);
        let found = o.stderr.find("INFO: Found 1 target...").expect(&o.stderr);
        let bad = o.stderr.find("ERROR: Invalid output format 'starlark'. Valid values are: proto, streamed_proto, textproto, jsonproto, text, commands, summary").expect(&o.stderr);
        assert!(found < bad, "{}", o.stderr);
        // `--experimental_graphless_query=yes` needs a streaming formatter and order.
        let o = run(&rt, &["query", "--experimental_graphless_query=true", "--order_output=full", "//lib:l"]);
        assert_eq!(o.code, 2, "{}", o.stderr);
        assert!(o.stderr.contains("ERROR: --experimental_graphless_query requires --order_output=no or --order_output=auto and an --output option that supports streaming; valid values are: label, label_kind, build, package, location, xml, proto, streamed_jsonproto, streamed_proto"), "{}", o.stderr);
        let o = run(&rt, &["query", "--experimental_graphless_query=true", "--output=graph", "//lib:l"]);
        assert_eq!(o.code, 2, "{}", o.stderr);
        let o = run(&rt, &["query", "--experimental_graphless_query=true", "//lib:l"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
    }

    #[test]
    fn round9_cquery_proto_has_deprecated_configuration_checksum() {
        let rt = tree();
        let o = run(&rt, &["cquery", "--output=textproto", "//app:app"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        let out = o.out();
        // ConfiguredTarget { target, configuration { checksum }, configuration_id }
        assert!(out.contains("configuration {\n    checksum: \""), "{out}");
        let o = run(&rt, &["cquery", "--noproto:include_configurations", "--output=textproto", "//app:app"]);
        assert!(!o.out().contains("checksum"), "{}", o.out());
    }

    #[test]
    fn round9_aquery_filters_use_full_match() {
        let rt = tree();
        let o = run(&rt, &["aquery", "mnemonic('Genru', //app:app)"]);
        assert_eq!(o.out(), "", "partial regex must not match (Pattern.matches)");
        let o = run(&rt, &["aquery", "mnemonic('Genrule', //app:app)"]);
        assert!(o.out().contains("Mnemonic: Genrule"), "{}", o.out());
        let o = run(&rt, &["aquery", "inputs('data.txt', deps(//app:app))"]);
        assert_eq!(o.out(), "", "{}", o.out());
        let o = run(&rt, &["aquery", "inputs('.*data.txt', deps(//app:app))"]);
        assert!(o.out().contains("Target: //lib:l"), "{}", o.out());
    }
}

#[cfg(test)]
mod output_file_tests {
    use super::*;

    #[test]
    fn unopenable_output_file_is_exit_36_without_creating_directories() {
        let mut err = Vec::new();
        let r = write_query_output_file(std::path::Path::new("/nonexistent-zel-dir/x"), b"data", &mut err);
        assert_eq!(r, Err(ExitCode::LOCAL_ENVIRONMENTAL_ERROR));
        assert_eq!(String::from_utf8_lossy(&err), "ERROR: Could not open query output file /nonexistent-zel-dir/x\n");
        assert_eq!(ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code(), 36);
        assert!(!std::path::Path::new("/nonexistent-zel-dir").exists());
        let dir = std::env::temp_dir().join(format!("zel-q6a-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ok = dir.join("out.txt");
        let mut err = Vec::new();
        assert!(write_query_output_file(&ok, b"hello", &mut err).is_ok());
        assert_eq!(std::fs::read(&ok).unwrap(), b"hello");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
