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

//! `help` (`HelpCommand`, `BlazeCommandUtils.expandHelpTopic`, `OptionsUsage`, `HtmlUtils`):
//! every topic, with option documentation generated from the option definitions.

use super::{expand_product, info, product_name, COMMANDS};
use crate::{build_label, writeln_bytes, BlazeRuntime};
use std::collections::HashSet;
use zel_options::{
    OptionDefinition, OptionDocumentationCategory as Cat, OptionEffectTag, OptionMetadataTag, OptionType,
    ParsedCommandOptions, ALL_COMMAND_OPTIONS, ALL_STARTUP_OPTIONS,
};
use zel_util::ExitCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpVerbosity {
    Short,
    Medium,
    Long,
}

const DOCUMENTATION_ORDER: &[Cat] = &[
    Cat::BazelClientOptions,
    Cat::ExecutionStrategy,
    Cat::Toolchain,
    Cat::OutputSelection,
    Cat::OutputParameters,
    Cat::InputStrictness,
    Cat::Signing,
    Cat::StarlarkSemantics,
    Cat::Testing,
    Cat::Query,
    Cat::ModCommand,
    Cat::Bzlmod,
    Cat::BuildTimeOptimization,
    Cat::Logging,
    Cat::GenericInputs,
    Cat::Remote,
    Cat::Uncategorized,
];

fn category_description(c: Cat) -> &'static str {
    match c {
        Cat::Uncategorized => "Miscellaneous options, not otherwise categorized.",
        Cat::Undocumented => "This feature should not be documented, as it is not meant for general use",
        Cat::BazelClientOptions => "Options that appear before the command and are parsed by the client",
        Cat::Logging => "Options that affect the verbosity, format or location of logging",
        Cat::ExecutionStrategy => "Options that control build execution",
        Cat::BuildTimeOptimization => "Options that trigger optimizations of the build time",
        Cat::OutputSelection => "Options that control the output of the command",
        Cat::OutputParameters => {
            "Options that let the user configure the intended output, affecting its value, as opposed to its existence"
        }
        Cat::InputStrictness => {
            "Options that affect how strictly Bazel enforces valid build inputs (rule definitions,  flag combinations, etc.)"
        }
        Cat::Signing => "Options that affect the signing outputs of a build",
        Cat::StarlarkSemantics => {
            "This option affects semantics of the Starlark language or the build API accessible to BUILD files, .bzl files, or WORKSPACE files."
        }
        Cat::Testing => "Options that govern the behavior of the test environment or test runner",
        Cat::Toolchain => "Options that configure the toolchain used for action execution",
        Cat::Query => "Options relating to query output and semantics",
        Cat::ModCommand => "Options relating to the output and semantics of the `mod` subcommand",
        Cat::Bzlmod => "Options relating to Bzlmod output and semantics",
        Cat::GenericInputs => {
            "Options specifying or altering a generic input to a Bazel command that does not fall into other categories."
        }
        Cat::Remote => "Remote caching and execution options",
    }
}

fn effect_tag_description(t: OptionEffectTag) -> String {
    let p = product_name();
    match t {
        OptionEffectTag::Unknown => "This option has unknown, or undocumented, effect.".into(),
        OptionEffectTag::NoOp => "This option has literally no effect.".into(),
        OptionEffectTag::LosesIncrementalState => "Changing the value of this option can cause significant loss of incremental state, which slows builds. State could be lost due to a server restart or to invalidation of a large part of the dependency graph.".into(),
        OptionEffectTag::ChangesInputs => format!("This option actively changes the inputs that {p} considers for the build, such as filesystem restrictions, repository versions, or other options."),
        OptionEffectTag::AffectsOutputs => format!("This option affects {p}'s outputs. This tag is intentionally broad, can include transitive affects, and does not specify the type of output it affects."),
        OptionEffectTag::BuildFileSemantics => "This option affects the semantics of BUILD or .bzl files.".into(),
        OptionEffectTag::BazelInternalConfiguration => format!("This option affects settings of {p}-internal machinery. This tag does not, on its own, mean that build artifacts are affected."),
        OptionEffectTag::LoadingAndAnalysis => "This option affects the loading and analysis of dependencies, and the building of the dependency graph.".into(),
        OptionEffectTag::Execution => "This option affects the execution phase, such as sandboxing or remote execution related options.".into(),
        OptionEffectTag::HostMachineResourceOptimizations => "This option triggers an optimization that may be machine specific and is not guaranteed to work on all machines. The optimization could include a tradeoff with other aspects of performance, such as memory or cpu cost.".into(),
        OptionEffectTag::EagernessToExit => format!("This option changes how eagerly {p} will exit from a failure, where a choice between continuing despite the failure and ending the invocation exists."),
        OptionEffectTag::BazelMonitoring => format!("This option is used to monitor {p}'s behavior and performance."),
        OptionEffectTag::TerminalOutput => format!("This option affects {p}'s terminal output."),
        OptionEffectTag::ActionCommandLines => "This option changes the command line arguments of one or more build actions.".into(),
        OptionEffectTag::TestRunner => "This option changes the testrunner environment of the build.".into(),
    }
}

fn metadata_tag_description(t: OptionMetadataTag) -> &'static str {
    match t {
        OptionMetadataTag::Experimental => "This option triggers an experimental feature with no guarantees of functionality.",
        OptionMetadataTag::IncompatibleChange => "This option triggers a breaking change. Use this option to test your migration readiness or get early access to the new feature",
        OptionMetadataTag::Deprecated => "This option is deprecated. It might be that the feature it affects is deprecated, or that another method of supplying the information is preferred.",
        OptionMetadataTag::Hidden => "This option should not be used by a user, and should not be logged.",
        OptionMetadataTag::Internal => "This option isn't even a option, and should not be logged.",
        OptionMetadataTag::NonConfigurable => "This option cannot be changed in a transition or be used in a select() statement.",
        OptionMetadataTag::FullyRedactedInLogs => "This option may contain sensitive credentials and should be redacted in logs.",
    }
}

const ALL_EFFECT_TAGS: &[OptionEffectTag] = &[
    OptionEffectTag::Unknown,
    OptionEffectTag::NoOp,
    OptionEffectTag::LosesIncrementalState,
    OptionEffectTag::ChangesInputs,
    OptionEffectTag::AffectsOutputs,
    OptionEffectTag::BuildFileSemantics,
    OptionEffectTag::BazelInternalConfiguration,
    OptionEffectTag::LoadingAndAnalysis,
    OptionEffectTag::Execution,
    OptionEffectTag::HostMachineResourceOptimizations,
    OptionEffectTag::EagernessToExit,
    OptionEffectTag::BazelMonitoring,
    OptionEffectTag::TerminalOutput,
    OptionEffectTag::ActionCommandLines,
    OptionEffectTag::TestRunner,
];

const ALL_METADATA_TAGS: &[OptionMetadataTag] = &[
    OptionMetadataTag::Experimental,
    OptionMetadataTag::IncompatibleChange,
    OptionMetadataTag::Deprecated,
    OptionMetadataTag::Hidden,
    OptionMetadataTag::Internal,
    OptionMetadataTag::NonConfigurable,
    OptionMetadataTag::FullyRedactedInLogs,
];

const PATH_OPTIONS: &[&str] = &[
    "output_base",
    "output_user_root",
    "install_base",
    "bazelrc",
    "server_jvm_out",
    "failure_detail_out",
    "workspace_directory",
    "profile",
    "build_event_json_file",
    "build_event_text_file",
    "build_event_binary_file",
    "disk_cache",
    "repository_cache",
    "repo_contents_cache",
    "distdir",
    "vendor_dir",
    "symlink_prefix",
];

/// `Converter.getTypeDescription()` of the converter behind an option's schema type.
pub fn type_description(d: &OptionDefinition) -> String {
    if let Some(td) = d.type_description {
        return td.to_string();
    }
    match &d.option_type {
        OptionType::Boolean => "a boolean".into(),
        OptionType::TriState => "a tri-state (auto, yes, no)".into(),
        OptionType::Int => "an integer".into(),
        OptionType::Long => "a long integer".into(),
        OptionType::Double => "a double".into(),
        OptionType::String => {
            if PATH_OPTIONS.contains(&d.name) {
                "a path".into()
            } else if d.name == "digest_function" {
                "hash function".into()
            } else {
                "a string".into()
            }
        }
        OptionType::Duration => "An immutable length of time.".into(),
        OptionType::ByteSize => "a size in bytes, optionally followed by a K, M, G or T multiplier".into(),
        OptionType::Label => "a build target label".into(),
        OptionType::ListString => {
            if PATH_OPTIONS.contains(&d.name) {
                "a path".into()
            } else if d.allow_multiple {
                "a string".into()
            } else {
                "comma-separated list of options".into()
            }
        }
        OptionType::ListLabel => {
            if d.allow_multiple {
                "a build target label".into()
            } else {
                "comma-separated list of labels".into()
            }
        }
        OptionType::Map => "a 'name=value' assignment".into(),
        OptionType::Void => String::new(),
        OptionType::Enum(spec) => join_english(spec.values),
        OptionType::Resource(_) => "an integer, or a keyword (\"auto\", \"HOST_CPUS\", \"HOST_RAM\"), optionally followed by an operation ([-|*]<float>) eg. \"auto\", \"HOST_CPUS*.5\"".into(),
        OptionType::ResourceAssignment => "a named float, 'name=value', where value is either a float or HOST_CPUS or HOST_RAM, optionally followed by an operation ([-|*]<float>)".into(),
        OptionType::Custom(name) => match *name {
            "AssignmentConverter" => "a 'name=value' assignment".into(),
            "PathFragmentConverter" | "WorkspacePathFragmentConverter" | "AbsolutePathFragmentConverter" => "a path".into(),
            "LabelToStringListConverter" => "a '<Label>=value[,value]' assignment".into(),
            "RegexFilterAssignmentConverter" => "a '<RegexFilter>=value[,value]' assignment".into(),
            "EnvVarConverter" | "EnvVar.Converter" => "a 'name[=value]' assignment with an optional value part or the special syntax '=name' to unset a variable".into(),
            "MemoryModeConverter" => "memory mode".into(),
            "HashFunctionConverter" => "hash function".into(),
            "CommaSeparatedOptionSetConverter" => "comma-separated set of options".into(),
            "CommaSeparatedNonEmptyOptionListConverter" => "comma-separated list of options".into(),
            "ColonSeparatedOptionListConverter" => "colon-separated list of options".into(),
            "PositiveIntegerConverter" => "a positive integer".into(),
            "NonNegativeIntegerConverter" => "a non-negative integer".into(),
            "PercentageConverter" => "an integer percentage".into(),
            "RemoteDurationConverter" => "An immutable length of time.".into(),
            "StringToStringListConverter" => "a '[name=]value1[,..,valueN]' assignment".into(),
            "MnemonicBooleanConverter" | "SlimProfileConverter" => "a boolean".into(),
            "LogLevelConverter" => "0 <= an integer <= 6".into(),
            "MultiResourceConverter" | "TestResourcesConverter" => "a named float, 'name=value'".into(),
            "MaxStdoutErrBytesConverter" => "a size in bytes, optionally followed by a K, M, G or T multiplier".into(),
            "TestTimeoutConverter" => "a single integer or comma-separated list of 4 integers".into(),
            "ProgressReportIntervalConverter" => "an integer".into(),
            "FlagAliasConverter" => "a 'name=value' flag alias".into(),
            other => other.trim_end_matches("Converter").to_string(),
        },
    }
}

/// `Converters.joinEnglishList`.
fn join_english(values: &[&str]) -> String {
    match values.len() {
        0 => String::new(),
        1 => values[0].to_string(),
        2 => format!("{} or {}", values[0], values[1]),
        _ => format!("{}, or {}", values[..values.len() - 1].join(", "), values[values.len() - 1]),
    }
}

fn join_english_and(values: &[String]) -> String {
    match values.len() {
        0 => String::new(),
        1 => values[0].clone(),
        2 => format!("{} and {}", values[0], values[1]),
        _ => format!("{}, and {}", values[..values.len() - 1].join(", "), values[values.len() - 1]),
    }
}

fn break_opportunities(p: &[char]) -> Vec<usize> {
    let mut ends = Vec::new();
    let mut i = 0;
    while i < p.len() {
        let c = p[i];
        if c == ' ' {
            while i < p.len() && p[i] == ' ' {
                i += 1;
            }
            ends.push(i);
            continue;
        }
        if (c == '-' && i > 0 && p[i - 1].is_alphanumeric() && p.get(i + 1).map(|n| n.is_alphanumeric()).unwrap_or(false))
            || (c == '/' && p.get(i + 1).map(|n| n.is_alphabetic()).unwrap_or(false))
        {
            ends.push(i + 1);
        }
        i += 1;
    }
    if ends.last() != Some(&p.len()) {
        ends.push(p.len());
    }
    ends
}

/// `OptionsUsage.paragraphFill`.
pub fn paragraph_fill(input: &str, indent: usize, width: usize) -> String {
    let ind = " ".repeat(indent);
    let mut out = String::new();
    let mut sep = "";
    for para in input.split('\n') {
        let chars: Vec<char> = para.chars().collect();
        out.push_str(sep);
        out.push_str(&ind);
        let mut cursor = indent;
        let mut start = 0;
        for end in break_opportunities(&chars) {
            let word: String = chars[start..end].iter().collect();
            let wl = word.chars().count();
            if wl + cursor > width {
                while out.ends_with(' ') || out.ends_with('\t') {
                    out.pop();
                }
                out.push('\n');
                out.push_str(&ind);
                cursor = indent;
            }
            out.push_str(&word);
            cursor += wl;
            start = end;
        }
        while out.ends_with(' ') || out.ends_with('\t') {
            out.pop();
        }
        sep = "\n";
    }
    out
}

/// `OptionsUsage.getUsage(option, usage, verbosity, data, includeTags)`.
fn option_usage(d: &OptionDefinition, usage: &mut String, v: HelpVerbosity, include_tags: bool) {
    let flag = if d.uses_boolean_syntax() { format!("[no]{}", d.name) } else { d.name.to_string() };
    usage.push_str("  --");
    usage.push_str(&flag);
    if v == HelpVerbosity::Short {
        usage.push('\n');
        return;
    }
    if let Some(a) = d.abbrev {
        usage.push_str(&format!(" [-{a}]"));
    }
    let td = type_description(d);
    if !td.is_empty() {
        usage.push_str(&format!(" ({td}; "));
        if d.allow_multiple {
            usage.push_str("may be used multiple times");
        } else {
            match d.default_value {
                None => usage.push_str("default: see description"),
                Some(dv) => usage.push_str(&format!("default: \"{dv}\"")),
            }
        }
        usage.push(')');
    }
    usage.push('\n');
    if v == HelpVerbosity::Medium {
        return;
    }
    if !d.help.is_empty() {
        usage.push_str(&paragraph_fill(d.help, 4, 80));
        usage.push('\n');
    }
    if let Some(exp) = d.expansion {
        if !exp.is_empty() {
            let mut m = String::from("Expands to: ");
            for e in exp {
                m.push_str(e);
                m.push(' ');
            }
            usage.push_str(&paragraph_fill(&m, 6, 80));
            usage.push('\n');
        }
    }
    if d.has_implicit_requirements() {
        let mut m = String::from("Using this option will also add: ");
        for r in d.implicit_requirements {
            m.push_str(r);
            m.push(' ');
        }
        usage.push_str(&paragraph_fill(&m, 6, 80));
        usage.push('\n');
    }
    if !include_tags {
        return;
    }
    let mut tags: Vec<String> = d
        .effect_tags
        .iter()
        .filter(|t| **t != OptionEffectTag::Unknown)
        .map(|t| t.as_str().to_ascii_lowercase())
        .collect();
    tags.extend(
        d.metadata_tags
            .iter()
            .filter(|t| **t != OptionMetadataTag::Hidden && **t != OptionMetadataTag::Internal)
            .map(|t| t.as_str().to_ascii_lowercase()),
    );
    if !tags.is_empty() {
        usage.push_str(&paragraph_fill(&format!("Tags: {}", tags.join(", ")), 6, 80));
        usage.push('\n');
    }
}

/// Documented options by category in display order, sorted by name (`getOptionsSortedByCategory`).
fn options_by_category<'a>(defs: &[&'a OptionDefinition]) -> Vec<(Cat, Vec<&'a OptionDefinition>)> {
    let mut out = Vec::new();
    for cat in DOCUMENTATION_ORDER {
        let mut v: Vec<&OptionDefinition> =
            defs.iter().copied().filter(|d| d.documentation_category == *cat).collect();
        v.sort_by(|a, b| a.name.cmp(b.name));
        out.push((*cat, v));
    }
    out
}

/// `OptionsParser.describeOptions`.
pub fn describe_options(defs: &[&OptionDefinition], v: HelpVerbosity) -> String {
    let mut desc = String::new();
    for (cat, list) in options_by_category(defs) {
        if list.is_empty() {
            continue;
        }
        desc.push('\n');
        desc.push_str(category_description(cat));
        desc.push_str(":\n");
        for d in list {
            option_usage(d, &mut desc, v, true);
        }
    }
    desc.trim().to_string()
}

/// `BlazeCommandUtils.expandHelpTopic`.
pub fn expand_help_topic(topic: &str, template: &str, defs: &[&OptionDefinition], v: HelpVerbosity) -> String {
    let p = product_name();
    let options = describe_options(defs, v).replace("%{product}", p);
    let mut s = template
        .replace("%{product}", p)
        .replace("%{command}", topic)
        .replace("%{options}", &options)
        .trim()
        .to_string();
    s.push_str("\n\n");
    if v == HelpVerbosity::Medium {
        s.push_str("(Use 'help --long' for full details or --short to just enumerate options.)\n");
    }
    s
}

const R_BUILD: &str = include_str!("../../resources/help/build.txt");

fn command_template(name: &str) -> &'static str {
    match name {
        "aquery" => include_str!("../../resources/help/aquery.txt"),
        "build" => R_BUILD,
        "canonicalize-flags" => include_str!("../../resources/help/canonicalize.txt"),
        "clean" => include_str!("../../resources/help/clean.txt"),
        "config" => include_str!("../../resources/help/config.txt"),
        "coverage" => include_str!("../../resources/help/coverage.txt"),
        "cquery" => include_str!("../../resources/help/cquery.txt"),
        "dump" => "Usage: %{product} dump <options>\nDumps the internal state of the %{product} server process.  This command is provided as an aid to debugging, not as a stable interface, so users should not try to parse the output; instead, use 'query' or 'info' for this purpose.\n\n%{options}",
        "fetch" => include_str!("../../resources/help/fetch.txt"),
        "help" => include_str!("../../resources/help/help.txt"),
        "info" => include_str!("../../resources/help/info.txt"),
        "license" => "Prints the license of this software.\n\n%{options}",
        "mobile-install" => include_str!("../../resources/help/mobile-install.txt"),
        "mod" => include_str!("../../resources/help/mod.txt"),
        "print_action" => include_str!("../../resources/help/print_action.txt"),
        "query" => include_str!("../../resources/help/query.txt"),
        "run" => include_str!("../../resources/help/run.txt"),
        "shutdown" => "This command shuts down the memory resident %{product} server process.\n\n%{options}",
        "test" => include_str!("../../resources/help/test.txt"),
        "vendor" => include_str!("../../resources/help/vendor.txt"),
        "version" => include_str!("../../resources/help/version.txt"),
        _ => "%{options}",
    }
}

/// Every option of a command (`BlazeCommandUtils.getOptions` -> the options of all of the command's options
/// classes), including undocumented ones, sorted by name (`OptionsParser.visitOptions`).
pub fn command_all_options(command: &str) -> Vec<&'static OptionDefinition> {
    let mut v: Vec<&'static OptionDefinition> =
        ALL_COMMAND_OPTIONS.iter().filter(|d| d.commands.contains(&command)).collect();
    v.sort_by(|a, b| a.name.cmp(b.name));
    v
}

/// The documented options of a command (what `help <command>` / `OptionsParser.describeOptions` describe).
pub fn command_options(command: &str) -> Vec<&'static OptionDefinition> {
    command_all_options(command).into_iter().filter(|d| d.documentation_category != Cat::Undocumented).collect()
}

/// `BlazeCommandUtils.getUsage` for a command.
pub fn command_usage(command: &str, v: HelpVerbosity) -> String {
    let defs: Vec<&OptionDefinition> = if command == "startup_options" {
        ALL_STARTUP_OPTIONS.iter().filter(|d| d.documentation_category != Cat::Undocumented).collect()
    } else {
        command_options(command)
    };
    let tmpl = if command == "startup_options" {
        include_str!("../../resources/help/startup_options.txt")
    } else {
        command_template(command)
    };
    expand_help_topic(command, tmpl, &defs, v)
}

fn version_line() -> String {
    format!("{:>80}\n", format!("[{} {}]", product_name(), crate::release_name()))
}

fn generic_help() -> String {
    let p = product_name();
    let mut s = format!("Usage: {p} <command> <options> ...\n\nAvailable commands:\n");
    for c in COMMANDS {
        if c.hidden {
            continue;
        }
        s.push_str(&format!("  {:<19} {}\n", c.name, expand_product(c.short_description)));
    }
    s.push_str(&format!(
        "\nGetting more help:\n  {p} help <command>\n                   Prints help and options for <command>.\n  {p} help startup_options\n                   Options for the JVM hosting {p}.\n  {p} help target-syntax\n                   Explains the syntax for specifying targets.\n  {p} help info-keys\n                   Displays a list of keys used by the info command.\n"
    ));
    s
}

/// `OptionsUsage.getCompletion`: keyed on the Java *field type* of the option (`List` options and other
/// types give a bare `=`); Zel recovers the field type from the converter (`enum_values` for enum types).
fn completion_line(d: &OptionDefinition, out: &mut String) {
    out.push_str(&format!("--{}", d.name));
    let conv = match &d.option_type {
        OptionType::Custom(c) => *c,
        _ => "",
    };
    if d.allow_multiple && !matches!(d.option_type, OptionType::Boolean | OptionType::TriState | OptionType::Void) {
        out.push_str("=\n");
        return;
    }
    match &d.option_type {
        OptionType::Boolean => out.push_str(&format!("\n--no{}\n", d.name)),
        OptionType::TriState => out.push_str(&format!("={{auto,yes,no}}\n--no{}\n", d.name)),
        OptionType::Enum(spec) => out.push_str(&format!("={{{}}}\n", spec.values.join(","))),
        OptionType::Label => out.push_str("=label\n"),
        OptionType::Void => out.push('\n'),
        OptionType::Custom(_) if !d.enum_values.is_empty() => {
            out.push_str(&format!("={{{}}}\n", d.enum_values.join(",").to_lowercase()))
        }
        OptionType::Custom(_)
            if matches!(
                conv,
                "LabelConverter" | "EmptyToNullLabelConverter" | "LibcTopLabelConverter" | "HostPlatformConverter"
            ) =>
        {
            out.push_str("=label\n")
        }
        OptionType::Custom(_) if conv.contains("PathFragment") || matches!(conv, "DiskCacheConverter" | "ExecutionLogFileConverter") => {
            out.push_str("=path\n")
        }
        _ => out.push_str("=\n"),
    }
}

fn completion_options(defs: &[&OptionDefinition]) -> String {
    let mut sorted: Vec<&OptionDefinition> =
        defs.iter().copied().filter(|d| d.documentation_category != Cat::Undocumented).collect();
    sorted.sort_by(|a, b| a.name.cmp(b.name));
    let mut out = String::new();
    for d in sorted {
        completion_line(d, &mut out);
    }
    out
}

fn upper_underscore(name: &str) -> String {
    name.replace('-', "_").to_ascii_uppercase()
}

fn completion_variables() -> String {
    let mut out = String::new();
    // HelpCommand.getSortedCommands: every registered command (hidden ones such as `config`
    // included), sorted by name.
    let mut sorted_commands: Vec<&super::CommandSpec> = COMMANDS.iter().collect();
    sorted_commands.sort_by_key(|c| c.name);
    let cmd_names: Vec<&str> = sorted_commands.iter().map(|c| c.name).collect();
    out.push_str(&format!("BAZEL_COMMAND_LIST=\"{}\"\n", cmd_names.join(" ")));

    out.push_str("BAZEL_INFO_KEYS=\"\n");
    for name in info::hardwired_info_item_names() {
        out.push_str(&name);
        out.push('\n');
    }
    out.push_str("\"\n");

    out.push_str("BAZEL_STARTUP_OPTIONS=\"\n");
    let startup_defs: Vec<&OptionDefinition> = ALL_STARTUP_OPTIONS.iter().filter(|d| !d.is_internal()).collect();
    out.push_str(&completion_options(&startup_defs));
    out.push_str("\"\n");

    for c in &sorted_commands {
        let var_name = upper_underscore(c.name);
        if !c.completion.is_empty() {
            out.push_str(&format!("BAZEL_COMMAND_{var_name}_ARGUMENT=\"{}\"\n", c.completion));
        }
        out.push_str(&format!("BAZEL_COMMAND_{var_name}_FLAGS=\"\n"));
        let defs = command_options(c.name);
        out.push_str(&completion_options(&defs));
        out.push_str("\"\n");
    }
    out
}

// ------------------------------------------------------------------- flags-as-proto / html

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for ch in data.chunks(3) {
        let b = [ch[0], *ch.get(1).unwrap_or(&0), *ch.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if ch.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if ch.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

/// `String.hashCode()` (UTF-16 code units, wrapping `int` arithmetic).
fn java_string_hash(s: &str) -> i32 {
    let mut h: i32 = 0;
    for u in s.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(u as i32);
    }
    h
}

/// Iteration order of a `java.util.HashMap<String, _>` after inserting `keys` (all distinct) in order:
/// buckets by `(h ^ h >>> 16) & (capacity - 1)` with the table doubled whenever size exceeds 0.75 * capacity.
/// Within a bucket the entries come out in *reverse* insertion order: this was derived from (and reproduces
/// exactly, for all 1083 flags) the order of the real Bazel 9.3.x `help flags-as-proto` output.
fn java_hashmap_iteration_order(keys: &[&str]) -> Vec<usize> {
    let mut capacity: usize = 16;
    let mut size = 0usize;
    for _ in keys {
        size += 1;
        if size > capacity * 3 / 4 {
            capacity *= 2;
        }
    }
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); capacity];
    for (i, k) in keys.iter().enumerate() {
        let h = java_string_hash(k) as u32;
        let spread = h ^ (h >> 16);
        buckets[(spread as usize) & (capacity - 1)].push(i);
    }
    buckets.into_iter().flat_map(|b| b.into_iter().rev()).collect()
}

/// `HelpCommand.emitFlagsAsProtoHelp`: every non-internal option of the startup options and of every command's
/// options (sorted by command name, then option name), merged by option name — the first visit (startup first)
/// creates the `FlagInfo`, each visit appends the command — serialized as a `FlagCollection` in `HashMap` order.
fn flags_as_proto() -> String {
    let mut visited: Vec<(&'static OptionDefinition, Vec<&'static str>)> = Vec::new();
    let mut index: std::collections::HashMap<&'static str, usize> = std::collections::HashMap::new();
    let mut visit = |d: &'static OptionDefinition, command: &'static str| {
        if d.is_internal() {
            return;
        }
        let i = *index.entry(d.name).or_insert_with(|| {
            visited.push((d, Vec::new()));
            visited.len() - 1
        });
        visited[i].1.push(command);
    };
    let mut startup: Vec<&'static OptionDefinition> = ALL_STARTUP_OPTIONS.iter().collect();
    startup.sort_by(|a, b| a.name.cmp(b.name));
    for d in startup {
        visit(d, "startup");
    }
    let mut commands: Vec<&'static str> = COMMANDS.iter().map(|c| c.name).collect();
    commands.sort_unstable();
    for c in commands {
        for d in command_all_options(c) {
            visit(d, c);
        }
    }

    let names: Vec<&str> = visited.iter().map(|(d, _)| d.name).collect();
    let mut collection = Vec::new();
    for i in java_hashmap_iteration_order(&names) {
        let (d, cmds) = &visited[i];
        let mut f = Vec::new();
        use zel_proto::wire::{encode_bytes_always as bytes, encode_tag, WIRE_TYPE_VARINT};
        let boolean = |n: u32, v: bool, out: &mut Vec<u8>| {
            encode_tag(n, WIRE_TYPE_VARINT, out);
            out.push(v as u8);
        };
        bytes(1, d.name.as_bytes(), &mut f);
        boolean(2, d.uses_boolean_syntax(), &mut f);
        bytes(3, d.help.as_bytes(), &mut f);
        for c in cmds {
            bytes(4, c.as_bytes(), &mut f);
        }
        if let Some(a) = d.abbrev {
            bytes(5, a.to_string().as_bytes(), &mut f);
        }
        boolean(6, d.allow_multiple, &mut f);
        for t in d.effect_tags {
            bytes(7, t.as_str().as_bytes(), &mut f);
        }
        for t in d.metadata_tags {
            bytes(8, t.as_str().as_bytes(), &mut f);
        }
        bytes(9, d.documentation_category.as_str().as_bytes(), &mut f);
        boolean(10, d.requires_value(), &mut f);
        if let Some(old) = d.old_name {
            bytes(11, old.as_bytes(), &mut f);
        }
        if let Some(dep) = d.deprecation_warning {
            bytes(12, dep.as_bytes(), &mut f);
        }
        if let Some(dv) = d.default_value {
            bytes(13, dv.as_bytes(), &mut f);
        }
        if let Some(exp) = d.expansion {
            for e in exp {
                bytes(14, e.as_bytes(), &mut f);
            }
        }
        if let Some(tc) = d.type_converter {
            bytes(15, tc.as_bytes(), &mut f);
        }
        for v in d.enum_values {
            bytes(16, v.as_bytes(), &mut f);
        }
        bytes_into(&mut collection, &f);
    }
    base64(&collection)
}

/// `FlagCollection.flag_infos` (field 1).
fn bytes_into(collection: &mut Vec<u8>, flag: &[u8]) {
    zel_proto::wire::encode_bytes_always(1, flag, collection);
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// commonmark-java `Escaping.escapeHtml`: `&`, `<`, `>` and `"` only (not `'`, unlike Guava's `HtmlEscapers`).
fn commonmark_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn option_usage_html(d: &OptionDefinition, usage: &mut String, command: Option<&str>) {
    let plain = d.name;
    let flag = if d.uses_boolean_syntax() { format!("[no]{plain}") } else { plain.to_string() };
    let anchor = match command {
        Some(c) => format!("{c}-flag--{plain}"),
        None => format!("flag--{plain}"),
    };
    usage.push_str(&format!("<dt id=\"{anchor}\"><code"));
    if command.is_some() {
        usage.push_str(&format!(" id=\"{plain}\""));
    }
    usage.push_str(&format!("><a href=\"#{anchor}\">--{flag}</a>"));
    let td = type_description(d);
    if !d.requires_value() {
    } else if !d.value_help.is_empty() {
        usage.push_str(&format!("={}", html_escape(d.value_help)));
    } else if !td.is_empty() {
        usage.push_str(&format!("=&lt;{}&gt;", html_escape(&td)));
    }
    usage.push_str("</code>");
    if let Some(a) = d.abbrev {
        usage.push_str(&format!(" [<code>-{a}</code>]"));
    }
    if d.allow_multiple {
        usage.push_str(" multiple uses are accumulated");
    } else if d.option_type == OptionType::Void {
    } else {
        match d.default_value {
            None => usage.push_str(" default: see description"),
            Some(dv) => usage.push_str(&format!(" default: \"{}\"", html_escape(dv))),
        }
    }
    usage.push_str("</dt>\n<dd>\n");
    if !d.help.is_empty() {
        usage.push_str(&super::markdown::render_to_html(d.help));
        usage.push('\n');
    }
    if let Some(exp) = d.expansion {
        if !exp.is_empty() {
            usage.push_str("<p>Expands to:\n");
            for e in exp {
                let name = html_escape(e);
                let name = name.split('=').next().unwrap_or("");
                usage.push_str(&format!(
                    "<br/>&nbsp;&nbsp;<code><a href=\"#flag{name}\">{}</a></code>\n",
                    html_escape(e)
                ));
            }
            usage.push_str("</p>");
        }
    }
    let mut tags: Vec<String> = d
        .effect_tags
        .iter()
        .filter(|t| **t != OptionEffectTag::Unknown)
        .map(|t| {
            format!(
                "<a href=\"#effect_tag_{}\"><code>{}</code></a>",
                t.as_str(),
                t.as_str().to_ascii_lowercase()
            )
        })
        .collect();
    tags.extend(
        d.metadata_tags
            .iter()
            .filter(|t| **t != OptionMetadataTag::Hidden && **t != OptionMetadataTag::Internal)
            .map(|t| {
                format!(
                    "<a href=\"#metadata_tag_{}\"><code>{}</code></a>",
                    t.as_str(),
                    t.as_str().to_ascii_lowercase()
                )
            }),
    );
    if !tags.is_empty() {
        usage.push_str(&format!("<p>Tags:\n{}\n</p>", tags.join(", ")));
    }
    usage.push_str("</dd>\n");
}

fn options_html(defs: &[&OptionDefinition], command: Option<&str>) -> String {
    let mut desc = String::new();
    for (cat, list) in options_by_category(defs) {
        let list: Vec<&&OptionDefinition> =
            list.iter().filter(|d| !d.effect_tags.contains(&OptionEffectTag::NoOp)).collect();
        if list.is_empty() {
            continue;
        }
        desc.push_str(&format!("<dl>{}:\n", html_escape(category_description(cat))));
        for d in list {
            option_usage_html(d, &mut desc, command);
        }
        desc.push_str("</dl>\n");
    }
    desc.replace("%{product}", product_name())
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// `@Command(inheritsOptionsFrom)` of `cmd` (from the generated `COMMAND_INHERITANCE` table).
fn get_command_inherits(cmd: &str) -> Vec<&'static str> {
    zel_options::COMMAND_INHERITANCE
        .iter()
        .find(|c| c.0 == cmd)
        .map(|c| c.1.to_vec())
        .unwrap_or_default()
}

/// `HelpCommand.HtmlEmitter.emit`: the HTML reference of all commands and options, generated
/// from the command table and the option classes of each command (`COMMAND_OPTION_CLASSES`:
/// the classes of `@Command(options)` plus the module-contributed ones, *not* inherited ones).
fn everything_as_html() -> String {
    use zel_options::{COMMAND_INHERITANCE, COMMAND_OPTION_CLASSES, COMMON_OPTION_CLASSES, FRAGMENT_OPTION_CLASSES};
    let mut result = String::new();
    result.push_str("<h2>Commands</h2>\n<table>\n");
    // `getSortedCommands`: every registered command, hidden ones included, in name order.
    let mut sorted_commands: Vec<&super::CommandSpec> = COMMANDS.iter().collect();
    sorted_commands.sort_by_key(|c| c.name);
    for c in sorted_commands.iter().filter(|c| !c.hidden) {
        let short_desc = expand_product(c.short_description);
        result.push_str("<tr>\n");
        result.push_str(&format!("  <td><a href=\"#{}\"><code>{}</code></a></td>\n", c.name, c.name));
        result.push_str(&format!("  <td>{}</td>\n", html_escape(&short_desc)));
        result.push_str("</tr>\n");
    }
    result.push_str("</table>\n\n");

    result.push_str("<h2>Startup Options</h2>\n");
    let startup_defs: Vec<&OptionDefinition> = ALL_STARTUP_OPTIONS.iter().filter(|d| !d.is_internal()).collect();
    result.push_str(&options_html(&startup_defs, Some("startup_options")));
    result.push('\n');

    result.push_str("<h2><a name=\"common_options\">Options Common to all Commands</a></h2>\n");
    let common_defs: Vec<&OptionDefinition> = ALL_COMMAND_OPTIONS
        .iter()
        .filter(|d| COMMON_OPTION_CLASSES.contains(&d.java_class) && !d.is_internal())
        .collect();
    result.push_str(&options_html(&common_defs, Some("common_options")));
    result.push('\n');

    for c in &sorted_commands {
        let cap_name = capitalize(c.name);
        result.push_str(&format!("<h2><a name=\"{}\">{} Options</a></h2>\n", c.name, cap_name));
        if c.hidden {
            continue;
        }
        let inherits = get_command_inherits(c.name);
        if !inherits.is_empty() {
            let links: Vec<String> = inherits.iter().map(|n| format!("<a href=\"#{n}\">{n}</a>")).collect();
            result.push_str(&format!("<p>Inherits all options from {}.</p>\n\n", join_english_and(&links)));
        }
        let direct: &[&str] = COMMAND_OPTION_CLASSES.iter().find(|e| e.0 == c.name).map(|e| e.1).unwrap_or(&[]);
        let cmd_defs: Vec<&OptionDefinition> = ALL_COMMAND_OPTIONS
            .iter()
            .filter(|d| direct.contains(&d.java_class) && !d.is_internal())
            .collect();
        result.push_str(&options_html(&cmd_defs, Some(c.name)));
        result.push('\n');

        // The configuration (fragment) options follow, without a command-specific anchor and
        // minus the names already listed above.
        if COMMAND_INHERITANCE.iter().find(|e| e.0 == c.name).is_some_and(|e| e.2) {
            let listed: HashSet<&str> = options_by_category(&cmd_defs)
                .iter()
                .flat_map(|(_, l)| l.iter().map(|d| d.name))
                .collect();
            let cfg_defs: Vec<&OptionDefinition> = ALL_COMMAND_OPTIONS
                .iter()
                .filter(|d| {
                    (direct.contains(&d.java_class) || FRAGMENT_OPTION_CLASSES.contains(&d.java_class))
                        && !d.is_internal()
                        && !listed.contains(d.name)
                })
                .collect();
            result.push_str(&options_html(&cfg_defs, None));
            result.push('\n');
        }
    }

    result.push_str("<h3>Option Effect Tags</h3>\n<table>\n");
    for tag in ALL_EFFECT_TAGS {
        let desc = effect_tag_description(*tag);
        result.push_str("<tr>\n");
        result.push_str(&format!("<td id=\"effect_tag_{}\"><code>{}</code></td>\n", tag.as_str(), tag.as_str().to_ascii_lowercase()));
        result.push_str(&format!("<td>{}</td>\n", html_escape(&desc)));
        result.push_str("</tr>\n");
    }
    result.push_str("</table>\n");

    result.push_str("<h3>Option Metadata Tags</h3>\n<table>\n");
    for tag in ALL_METADATA_TAGS {
        if *tag == OptionMetadataTag::Hidden || *tag == OptionMetadataTag::Internal {
            continue;
        }
        let desc = metadata_tag_description(*tag);
        result.push_str("<tr>\n");
        result.push_str(&format!("<td id=\"metadata_tag_{}\"><code>{}</code></td>\n", tag.as_str(), tag.as_str().to_ascii_lowercase()));
        result.push_str(&format!("<td>{}</td>\n", html_escape(desc)));
        result.push_str("</tr>\n");
    }
    result.push_str("</table>\n");

    result.replace("%{product}", product_name())
}

impl BlazeRuntime {
    /// `HelpCommand.exec`.
    pub(crate) fn cmd_help(&self, args: &[String], stdout: &mut Vec<u8>, stderr: &mut Vec<u8>) -> ExitCode {
        let parsed = match ParsedCommandOptions::parse_for_command("help", args) {
            Ok(p) => p,
            Err(e) => {
                writeln_bytes(stderr, &e.format_bazel_error());
                return ExitCode::COMMAND_LINE_ERROR;
            }
        };
        // `--long`/`-l`/`--short` expand to `--help_verbosity`; the last one wins.
        let mut verbosity = HelpVerbosity::Medium;
        for a in args.iter().take_while(|a| *a != "--") {
            match a.as_str() {
                "--long" | "-l" => verbosity = HelpVerbosity::Long,
                "--short" => verbosity = HelpVerbosity::Short,
                other => {
                    if let Some(v) = other.strip_prefix("--help_verbosity=") {
                        verbosity = match v {
                            "long" => HelpVerbosity::Long,
                            "short" => HelpVerbosity::Short,
                            _ => HelpVerbosity::Medium,
                        };
                    }
                }
            }
        }
        let residue = &parsed.targets;
        let put = |out: &mut Vec<u8>, s: &str| out.extend_from_slice(s.as_bytes());
        if residue.is_empty() {
            put(stdout, &version_line());
            put(stdout, &generic_help());
            return ExitCode::SUCCESS;
        }
        if residue[0] == "completion" {
            if residue.len() > 2 {
                writeln_bytes(stderr, "ERROR: The completion command takes at most one argument");
                return ExitCode::COMMAND_LINE_ERROR;
            }
            match residue.get(1).map(String::as_str) {
                Some("bash") => {
                    writeln_bytes(stdout, include_str!("../../resources/help/bazel-complete-header.bash"));
                    put(stdout, &completion_variables());
                    writeln_bytes(stdout, include_str!("../../resources/help/bazel-complete-template.bash"));
                }
                None => put(stdout, &completion_variables()),
                Some(other) => {
                    writeln_bytes(
                        stderr,
                        &format!("ERROR: The completion command only supports 'bash' as an argument, got '{other}'"),
                    );
                    return ExitCode::COMMAND_LINE_ERROR;
                }
            }
            return ExitCode::SUCCESS;
        }
        if residue.len() != 1 {
            writeln_bytes(stderr, "ERROR: You must specify exactly one command");
            return ExitCode::COMMAND_LINE_ERROR;
        }
        let subject = residue[0].as_str();
        match subject {
            "startup_options" => {
                put(stdout, &version_line());
                put(
                    stdout,
                    &command_usage("startup_options", verbosity),
                );
                return ExitCode::SUCCESS;
            }
            "target-syntax" => {
                put(stdout, &version_line());
                put(
                    stdout,
                    &expand_help_topic(
                        "target-syntax",
                        include_str!("../../resources/help/target-syntax.txt"),
                        &[],
                        HelpVerbosity::Medium,
                    ),
                );
                return ExitCode::SUCCESS;
            }
            "info-keys" => {
                for (k, d) in info::info_key_descriptions() {
                    put(stdout, &format!("{k:<23} {d}\n"));
                }
                return ExitCode::SUCCESS;
            }
            "flags-as-proto" => {
                put(stdout, &flags_as_proto());
                return ExitCode::SUCCESS;
            }
            "everything-as-html" => {
                put(stdout, &everything_as_html());
                return ExitCode::SUCCESS;
            }
            _ => {}
        }
        if !COMMANDS.iter().any(|c| c.name == subject) {
            writeln_bytes(stderr, &format!("ERROR: '{subject}' is not a known command"));
            return ExitCode::COMMAND_LINE_ERROR;
        }
        put(stdout, &version_line());
        put(stdout, &command_usage(subject, verbosity));
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_proto::dynamic::Msg;

    fn b64_decode(s: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let (mut acc, mut bits) = (0u32, 0);
        for c in s.bytes() {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => continue,
            };
            acc = (acc << 6) | v as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
                acc &= (1 << bits) - 1;
            }
        }
        out
    }

    fn flags() -> Vec<Msg> {
        let coll = Msg::decode("bazel_flags.FlagCollection", &b64_decode(&flags_as_proto())).unwrap();
        coll.get_msgs("flag_infos").into_iter().cloned().collect()
    }

    fn commands(f: &Msg) -> Vec<String> {
        f.values("commands")
            .iter()
            .map(|v| match v {
                zel_proto::dynamic::Value::Str(s) => s.clone(),
                _ => panic!(),
            })
            .collect()
    }

    #[test]
    fn flags_as_proto_contains_every_non_internal_option_once_with_exact_commands() {
        let flags = flags();
        let mut names: Vec<&str> = flags.iter().map(|f| f.get_str("name")).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(n, names.len(), "flag names must be unique");
        let expected: HashSet<&str> = ALL_COMMAND_OPTIONS
            .iter()
            .chain(ALL_STARTUP_OPTIONS.iter())
            .filter(|d| !d.is_internal())
            .map(|d| d.name)
            .collect();
        assert_eq!(names.len(), expected.len());
        // undocumented options are included (HelpCommand visits every option)
        assert!(names.contains(&"enable_bzlmod") && names.contains(&"client_env") && names.contains(&"write_command_log"));
        // no ghost flags
        assert!(!names.contains(&"help"));
        // the same-named startup/command option is one flag, `startup` first (startup options are visited first)
        let wcl = flags.iter().find(|f| f.get_str("name") == "write_command_log").unwrap();
        let cmds = commands(wcl);
        assert_eq!(cmds[0], "startup");
        assert!(cmds.contains(&"build".to_string()) && cmds.contains(&"query".to_string()));
        // query/run/mobile-install flags are attributed to their own commands only
        let find = |n: &str| commands(flags.iter().find(|f| f.get_str("name") == n).unwrap());
        assert_eq!(find("order_output"), ["query"]);
        assert_eq!(find("run"), ["run"]);
        assert_eq!(find("start"), ["mobile-install"]);
        assert_eq!(find("expunge"), ["clean"]);
        assert!(find("jobs").contains(&"info".to_string()) && find("jobs").contains(&"clean".to_string()));
        assert!(!find("jobs").contains(&"query".to_string()));
        // commands are sorted by name after `startup`
        for f in &flags {
            let c = commands(f);
            let rest: Vec<&String> = c.iter().filter(|c| *c != "startup").collect();
            assert!(rest.windows(2).all(|w| w[0] < w[1]), "{}: {c:?}", f.get_str("name"));
        }
    }

    #[test]
    fn flags_as_proto_serializes_explicit_presence_fields() {
        let flags = flags();
        let jobs = flags.iter().find(|f| f.get_str("name") == "jobs").unwrap();
        assert!(jobs.has("has_negative_flag") && jobs.has("allows_multiple") && jobs.has("requires_value"));
        assert!(!jobs.get_bool("has_negative_flag") && jobs.get_bool("requires_value"));
        assert_eq!(jobs.get_str("abbreviation"), "j");
        assert_eq!(jobs.get_str("default_value"), "auto");
        assert_eq!(jobs.get_str("documentation_category"), "EXECUTION_STRATEGY");
        let cm = flags.iter().find(|f| f.get_str("name") == "compilation_mode").unwrap();
        assert_eq!(cm.get_str("abbreviation"), "c");
        assert!(cm.has("type_converter") && cm.get_str("type_converter").is_empty(), "nested `Converter` class -> \"\"");
        // undocumented flags keep their category; special-null defaults carry no default_value
        let ebz = flags.iter().find(|f| f.get_str("name") == "enable_bzlmod").unwrap();
        assert_eq!(ebz.get_str("documentation_category"), "UNDOCUMENTED");
        assert_eq!(ebz.get_str("old_name"), "experimental_enable_bzlmod");
        let long = flags.iter().find(|f| f.get_str("name") == "long").unwrap();
        assert!(!long.has("default_value"));
        assert_eq!(long.values("option_expansions").len(), 1);
        assert!(!long.get_bool("requires_value"));
    }

    #[test]
    fn flags_as_proto_follows_java_hashmap_iteration_order() {
        let flags = flags();
        let n = flags.len();
        let mut cap = 16usize;
        for size in 1..=n {
            if size > cap * 3 / 4 {
                cap *= 2;
            }
        }
        let bucket = |name: &str| {
            let h = java_string_hash(name) as u32;
            ((h ^ (h >> 16)) as usize) & (cap - 1)
        };
        let buckets: Vec<usize> = flags.iter().map(|f| bucket(f.get_str("name"))).collect();
        assert!(buckets.windows(2).all(|w| w[0] <= w[1]), "bucket order");
        // within a bucket: reverse of the visiting order (startup, then commands by name, options by name)
        assert_eq!(java_string_hash("a"), 97);
        assert_eq!(java_string_hash("ab"), 97 * 31 + 98);
        let order = java_hashmap_iteration_order(&["x", "y", "z"]);
        assert_eq!(order.len(), 3);
    }

    #[test]
    fn help_command_lists_exactly_the_options_of_the_command() {
        let build: HashSet<&str> = command_all_options("build").iter().map(|d| d.name).collect();
        for leaked in ["order_output", "run", "start", "script_path", "expunge", "keep_going_dummy"] {
            assert!(!build.contains(leaked), "{leaked} must not be a build option");
        }
        for owned in ["write_command_log", "jobs", "compilation_mode", "client_env"] {
            assert!(build.contains(owned), "{owned} missing from build");
        }
        let usage = command_usage("build", HelpVerbosity::Medium);
        assert!(usage.contains("--[no]write_command_log"));
        assert!(!usage.contains("--order_output"));
        assert!(!usage.contains("Options relating to query output and semantics"));
        // every command shows the client options block (RepositoryOptions et al. are common options)
        for c in ["clean", "info", "dump", "version", "query", "build"] {
            assert!(
                command_usage(c, HelpVerbosity::Medium).contains("Options that appear before the command and are parsed by the client:"),
                "{c}"
            );
        }
    }
}

#[cfg(test)]
mod everything_as_html_tests {
    #[test]
    fn lists_per_command_options_including_superclass_options() {
        let html = super::everything_as_html();
        // R7-1: per-command option documentation (declaring classes are matched by binary name).
        assert!(html.matches("<dt").count() > 1500, "only {} <dt>", html.matches("<dt").count());
        for anchor in ["build-flag--jobs", "query-flag--universe_scope", "aquery-flag--include_aspects", "cquery-flag--output"] {
            assert!(html.contains(&format!("<dt id=\"{anchor}\"")), "{anchor} missing");
        }
        // commonmark-java does not escape apostrophes, Markdown renders `_x_` as emphasis.
        assert!(html.contains("use 'batch' CPU scheduling"));
        assert!(html.contains("${OUTPUT_ROOT}/<em>blaze</em>${USER}") || html.contains("${OUTPUT_ROOT}/_blaze_${USER}"));
    }
}
