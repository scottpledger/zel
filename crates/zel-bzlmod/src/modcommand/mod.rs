//! Bazel's `mod` command: queries over the resolved Bzlmod dependency graph.
//!
//! Mirrors `bazel/bzlmod/modcommand/*` (`ModExecutor`, `ModOptions`, `ModuleArg`, `ExtensionArg`
//! and the output formatters) and the logic of `bazel/commands/ModCommand`.

pub mod args;
pub mod executor;
pub mod graphviz;
pub mod inspector;
pub mod json;
pub mod repo;
pub mod text;

use crate::model::{ModuleKey, RepoSpec};
use crate::resolve::{ExtensionId, Resolution};
use args::{list_string, ExtensionArg, ModuleArg, RepoMapping};
use executor::ModExecutor;
use inspector::{sorted_unique_ext_ids, Inspector, MaybeCompleteExtSet};
use repo::RepoDefinition;
use std::fmt;

/// How much detail about extension usages to include (`--extension_info`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionShow {
    Hidden,
    Usages,
    Repos,
    All,
}

/// Character set used to draw the tree (`--charset`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charset {
    Utf8,
    Ascii,
}

/// The format of the result (`--output`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Json,
    Graph,
    StreamedProto,
    StreamedJsonProto,
}

impl fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            OutputFormat::Text => "text",
            OutputFormat::Json => "json",
            OutputFormat::Graph => "graph",
            OutputFormat::StreamedProto => "streamed_proto",
            OutputFormat::StreamedJsonProto => "streamed_jsonproto",
        })
    }
}

/// Category of a [`ModError`], mirroring `FailureDetails.ModCommand.Code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModErrorKind {
    /// `MOD_COMMAND_UNKNOWN` (exit code 37): bad/missing subcommand.
    UnknownSubcommand,
    /// `MISSING_ARGUMENTS` (exit code 2).
    MissingArguments,
    /// `TOO_MANY_ARGUMENTS` (exit code 2).
    TooManyArguments,
    /// `INVALID_ARGUMENTS` (exit code 2).
    InvalidArguments,
    /// An option value that the options parser rejects (`While parsing option --x=y: ...`): reported
    /// before the command runs, without the `Type 'bazel help mod'` suffix (exit code 2).
    OptionParsing,
    /// The subcommand is not implemented by this module (handled by the caller).
    Unsupported,
}

/// A `mod` command failure. `message` is the complete user-facing message (without `ERROR: `).
#[derive(Debug, Clone, PartialEq)]
pub struct ModError {
    pub message: String,
    pub kind: ModErrorKind,
}

impl ModError {
    /// A message that has not been decorated with the "Type 'bazel help mod'..." suffix yet.
    pub(crate) fn raw(kind: ModErrorKind, message: impl Into<String>) -> ModError {
        ModError { message: message.into(), kind }
    }

    /// Process exit code of the failure (`COMMAND_LINE_ERROR` is 2).
    pub fn exit_code(&self) -> i32 {
        match self.kind {
            ModErrorKind::UnknownSubcommand => 37,
            _ => 2,
        }
    }

    /// `reportAndCreateFailureResult`: argument errors get a help suffix.
    fn finalize(self) -> ModError {
        match self.kind {
            ModErrorKind::MissingArguments | ModErrorKind::TooManyArguments | ModErrorKind::InvalidArguments => {
                let dot = if self.message.ends_with('.') { "" } else { "." };
                ModError {
                    message: format!("{}{} Type 'bazel help mod' for syntax and help.", self.message, dot),
                    kind: self.kind,
                }
            }
            _ => self,
        }
    }
}

impl fmt::Display for ModError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ModError {}

/// Options of the `mod` command (`ModOptions`). `Default` yields the flag defaults.
#[derive(Debug, Clone)]
pub struct ModOptions {
    /// `--depth`; values below 1 mean "unset" (subcommand-specific default applies).
    pub depth: i32,
    pub cycles: bool,
    pub include_unused: bool,
    pub include_builtin: bool,
    pub extension_info: ExtensionShow,
    /// `--extension_filter`: `None` if unset, `Some(vec![])` for the empty list (all extensions).
    pub extension_filter: Option<Vec<String>>,
    /// `--base_module`.
    pub base_module: String,
    /// `--extension_usages`.
    pub extension_usages: Vec<String>,
    pub charset: Charset,
    pub verbose: bool,
    pub output: OutputFormat,
    /// `--from`.
    pub from: Vec<String>,
    pub all_repos: bool,
    pub all_visible_repos: bool,
}

impl Default for ModOptions {
    fn default() -> Self {
        ModOptions {
            depth: -1,
            cycles: true,
            include_unused: false,
            include_builtin: false,
            extension_info: ExtensionShow::Hidden,
            extension_filter: None,
            base_module: "<root>".to_string(),
            extension_usages: vec![],
            charset: Charset::Utf8,
            verbose: false,
            output: OutputFormat::Text,
            from: vec!["<root>".to_string()],
            all_repos: false,
            all_visible_repos: false,
        }
    }
}

/// `Converters.joinEnglishList`: `a, b or c`.
fn join_english_list(values: &[&str]) -> String {
    let mut out = String::new();
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            out.push_str(if i + 1 < values.len() { ", " } else { " or " });
        }
        out.push_str(v);
    }
    out
}

fn enum_error(name: &str, desc: &str, input: &str, values: &[&str]) -> ModError {
    ModError::raw(
        ModErrorKind::OptionParsing,
        format!("While parsing option --{name}={input}: Not a valid {desc}: '{input}' (should be {})", join_english_list(values)),
    )
}

impl ModOptions {
    /// Reads the `mod` flags from parsed command options.
    pub fn from_parsed(p: &zel_options::ParsedCommandOptions) -> Result<ModOptions, ModError> {
        let mut o = ModOptions::default();
        let list = |name: &str| -> Option<Vec<String>> {
            let desc = p.parsed_options.get(name)?;
            let mut raw: Vec<String> = match desc.value.as_list() {
                Some(items) => items.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
                None => desc.value.as_str().map(|s| vec![s.to_string()]).unwrap_or_default(),
            };
            if raw.is_empty() && !desc.raw_values.is_empty() {
                raw = desc.raw_values.clone();
            }
            // Each element may itself be comma-separated; empty list elements are dropped here
            // (empty-element errors are reported when the list is parsed).
            Some(if raw.iter().all(|r| r.is_empty()) {
                vec![]
            } else {
                raw.iter().flat_map(|r| r.split(',').map(String::from).collect::<Vec<_>>()).collect()
            })
        };
        if let Some(v) = list("from") {
            o.from = v;
        }
        if let Some(v) = list("extension_usages") {
            o.extension_usages = v;
        }
        if p.parsed_options.contains_key("extension_filter") {
            o.extension_filter = list("extension_filter").or(Some(vec![]));
        }
        if let Some(b) = p.get_bool("verbose") {
            o.verbose = b;
        }
        if let Some(b) = p.get_bool("include_unused") {
            o.include_unused = b;
        }
        if let Some(b) = p.get_bool("cycles") {
            o.cycles = b;
        }
        if let Some(b) = p.get_bool("include_builtin") {
            o.include_builtin = b;
        }
        if let Some(b) = p.get_bool("all_repos") {
            o.all_repos = b;
        }
        if let Some(b) = p.get_bool("all_visible_repos") {
            o.all_visible_repos = b;
        }
        if let Some(d) = p.get_int("depth") {
            o.depth = d;
        }
        if let Some(s) = p.get_str("base_module") {
            o.base_module = s.to_string();
        }
        if let Some(s) = p.get_str("extension_info") {
            o.extension_info = match s.to_ascii_lowercase().as_str() {
                "hidden" => ExtensionShow::Hidden,
                "usages" => ExtensionShow::Usages,
                "repos" => ExtensionShow::Repos,
                "all" => ExtensionShow::All,
                _ => return Err(enum_error("extension_info", "extension show", s, &["hidden", "usages", "repos", "all"])),
            };
        }
        if let Some(s) = p.get_str("charset") {
            o.charset = match s.to_ascii_lowercase().as_str() {
                "utf8" => Charset::Utf8,
                "ascii" => Charset::Ascii,
                _ => return Err(enum_error("charset", "output charset", s, &["utf8", "ascii"])),
            };
        }
        if let Some(s) = p.get_str("output") {
            o.output = match s.to_ascii_lowercase().as_str() {
                "text" => OutputFormat::Text,
                "json" => OutputFormat::Json,
                "graph" => OutputFormat::Graph,
                "streamed_proto" => OutputFormat::StreamedProto,
                "streamed_jsonproto" => OutputFormat::StreamedJsonProto,
                _ => {
                    return Err(enum_error(
                        "output",
                        "output format",
                        s,
                        &["text", "json", "graph", "streamed_proto", "streamed_jsonproto"],
                    ))
                }
            };
        }
        Ok(o)
    }
}

/// Supplies repo definitions that are not part of the module graph.
pub trait RepoSpecProvider {
    /// The definition of the (extension-generated) repo with the given canonical name.
    fn repo_spec(&self, canonical_repo_name: &str) -> Option<RepoSpec>;

    /// The output base's `external` directory (used for the `# Located at:` line).
    fn external_directory(&self) -> String {
        String::new()
    }

    /// Path of a builtin or overridden repo (e.g. `@bazel_tools`), which has no repo spec.
    fn repo_override_path(&self, _canonical_repo_name: &str) -> Option<String> {
        None
    }
}

/// Supplies the repos generated by module extensions.
pub trait ExtensionInfoProvider {
    /// Internal names of the repos generated by the extension; `None` if unknown (or failed).
    fn generated_repos(&self, id: &ExtensionId) -> Option<Vec<String>>;
}

/// A [`RepoSpecProvider`] that knows nothing beyond the module graph.
pub struct NoRepoSpecs;

impl RepoSpecProvider for NoRepoSpecs {
    fn repo_spec(&self, _canonical_repo_name: &str) -> Option<RepoSpec> {
        None
    }
}

/// An [`ExtensionInfoProvider`] that reports no generated repos.
pub struct NoExtensionInfo;

impl ExtensionInfoProvider for NoExtensionInfo {
    fn generated_repos(&self, _id: &ExtensionId) -> Option<Vec<String>> {
        None
    }
}

/// The subcommands in `ModSubcommand` order.
const SUBCOMMANDS: [&str; 9] =
    ["deps", "graph", "all_paths", "path", "explain", "show_repo", "show_extension", "dump_repo_mapping", "tidy"];

fn is_graph_subcommand(s: &str) -> bool {
    matches!(s, "deps" | "graph" | "all_paths" | "path" | "explain")
}

fn bad_args(msg: impl Into<String>) -> ModError {
    ModError::raw(ModErrorKind::InvalidArguments, msg)
}

fn validate_args(subcommand: &str, opts: &ModOptions, args: &[String]) -> Result<(), ModError> {
    match subcommand {
        "show_repo" => match opts.output {
            OutputFormat::Text | OutputFormat::StreamedJsonProto | OutputFormat::StreamedProto => {}
            o => {
                return Err(bad_args(format!(
                    "Invalid --output '{o}' for the 'show_repo' subcommand. Only 'text', 'streamed_jsonproto', and 'streamed_proto' are supported."
                )))
            }
        },
        "show_extension" => {
            if opts.output != OutputFormat::Text {
                return Err(bad_args(format!(
                    "Invalid --output '{}' for the 'show_extension' subcommand. Only 'text' is supported.",
                    opts.output
                )));
            }
        }
        s if is_graph_subcommand(s) => match opts.output {
            OutputFormat::Text | OutputFormat::Json | OutputFormat::Graph => {}
            o => {
                return Err(bad_args(format!(
                    "Invalid --output '{o}' for the '{s}' subcommand. Only 'text', 'json', and 'graph' are supported."
                )))
            }
        },
        _ => {}
    }

    if subcommand == "show_repo" {
        let selected = [opts.all_repos, opts.all_visible_repos, !args.is_empty()].iter().filter(|b| **b).count();
        if selected > 1 {
            return Err(ModError::raw(
                ModErrorKind::TooManyArguments,
                "the 'show_repo' command requires exactly one of --all_repos, --all_visible_repos, or a list of repo arguments",
            ));
        }
    } else {
        if opts.all_repos {
            return Err(bad_args(format!("the '{subcommand}' command doesn't take the --all_repos option")));
        }
        if opts.all_visible_repos {
            return Err(bad_args(format!("the '{subcommand}' command doesn't take the --all_visible_repos option")));
        }
    }
    Ok(())
}

/// Parses an option list value, reporting failures like the options parser does.
fn parse_option_list<T>(
    flag: &str,
    values: &[String],
    parse: impl Fn(&str) -> Result<Vec<T>, String>,
) -> Result<Vec<T>, ModError> {
    if values.is_empty() {
        return Ok(vec![]);
    }
    let joined = values.join(",");
    parse(&joined).map_err(|e| ModError::raw(ModErrorKind::OptionParsing, format!("While parsing option --{flag}={joined}: {e}")))
}

fn module_arg_list_to_keys(
    list: &[ModuleArg],
    insp: &Inspector,
    base_deps: &[(String, ModuleKey)],
    base_unused: &[(String, ModuleKey)],
    include_unused: bool,
) -> Result<Vec<ModuleKey>, ModError> {
    let mut out: Vec<ModuleKey> = vec![];
    for a in list {
        for k in a.resolve_to_module_keys(insp, base_deps, base_unused, include_unused, true)? {
            if !out.contains(&k) {
                out.push(k);
            }
        }
    }
    Ok(out)
}

/// Looks up what a canonical repo name refers to.
fn repo_definition(
    res: &Resolution,
    insp: &Inspector,
    canonical: &str,
    provider: &dyn RepoSpecProvider,
    ext_info: &dyn ExtensionInfoProvider,
) -> Option<RepoDefinition> {
    if canonical.is_empty() {
        return None;
    }
    if let Some(path) = provider.repo_override_path(canonical) {
        return Some(RepoDefinition::RepoOverride { path });
    }
    if let Some(m) = res.module_by_canonical(canonical) {
        if let Some(spec) = &m.repo_spec {
            return Some(RepoDefinition::Found { name: canonical.to_string(), original_name: None, spec: spec.clone() });
        }
    }
    // Extension-generated repo: `<unique name>+<internal name>`.
    let mut original_name = None;
    for (id, unique) in &insp.extension_unique_names {
        if let Some(rest) = canonical.strip_prefix(&format!("{unique}+")) {
            if ext_info.generated_repos(id).map(|r| r.iter().any(|x| x == rest)).unwrap_or(false) {
                original_name = Some(rest.to_string());
                break;
            }
        }
    }
    let spec = provider.repo_spec(canonical)?;
    Some(RepoDefinition::Found { name: canonical.to_string(), original_name, spec })
}

/// `getReposToShow`: (display key, canonical repo name) pairs.
fn repos_to_show(
    opts: &ModOptions,
    insp: &Inspector,
    res: &Resolution,
    base_mapping: &RepoMapping,
    args: &[String],
) -> Result<Vec<(String, String)>, ModError> {
    let mut out: Vec<(String, String)> = vec![];
    fn put(out: &mut Vec<(String, String)>, k: String, v: String) {
        if let Some(e) = out.iter_mut().find(|(n, _)| *n == k) {
            e.1 = v;
        } else {
            out.push((k, v));
        }
    }
    if opts.all_repos {
        for (_, canonical) in &insp.key_to_canonical {
            if canonical.is_empty() {
                continue; // The main repo can't be inspected.
            }
            put(&mut out, format!("@@{canonical}"), canonical.clone());
        }
        for id in insp.extension_ids() {
            let Some(unique) = res.extension_unique_names.get(&id) else { continue };
            for (i, repos) in &insp.extension_repos {
                if *i != id {
                    continue;
                }
                for internal in repos {
                    let name = format!("{unique}+{internal}");
                    put(&mut out, format!("@@{name}"), name);
                }
            }
        }
    } else if opts.all_visible_repos {
        for (apparent, canonical) in &base_mapping.ordered {
            if canonical.is_empty() {
                continue;
            }
            put(&mut out, format!("@{apparent}"), canonical.clone());
        }
    } else {
        for arg in args {
            let resolved = ModuleArg::parse(arg)
                .map_err(bad_args)
                .and_then(|m| m.resolve_to_repo_names(insp, base_mapping))
                .map_err(|e| {
                    bad_args(format!(
                        "In repo argument {arg}: {} (Note that unused modules cannot be used here)",
                        e.message
                    ))
                })?;
            for (k, v) in resolved {
                put(&mut out, k, v);
            }
        }
    }
    Ok(out)
}

/// Runs a `mod` subcommand and returns what it prints to stdout.
///
/// `subcommand` is the first residue element and `args` the rest. `tidy` and
/// `dump_repo_mapping` are not handled here and return [`ModErrorKind::Unsupported`].
pub fn run_mod(
    resolution: &Resolution,
    subcommand: &str,
    args: &[String],
    opts: &ModOptions,
    repo_provider: &dyn RepoSpecProvider,
    ext_info: &dyn ExtensionInfoProvider,
) -> Result<String, ModError> {
    run_mod_inner(resolution, subcommand, args, opts, repo_provider, ext_info).map_err(ModError::finalize)
}

/// The checks `ModCommand` does before it evaluates anything: option values, subcommand, `validateArgs`.
pub fn validate_early(subcommand_str: &str, args: &[String], opts: &ModOptions) -> Result<(), ModError> {
    validate_early_inner(subcommand_str, args, opts).map(|_| ()).map_err(ModError::finalize)
}

#[allow(clippy::type_complexity)]
fn validate_early_inner(
    subcommand_str: &str,
    args: &[String],
    opts: &ModOptions,
) -> Result<(String, Vec<ModuleArg>, Vec<ModuleArg>, ModuleArg, Option<Vec<ExtensionArg>>), ModError> {
    let values = format!("({})", SUBCOMMANDS.join(", "));
    if subcommand_str.is_empty() {
        return Err(ModError::raw(
            ModErrorKind::UnknownSubcommand,
            format!("No subcommand specified, choose one of : {values}."),
        ));
    }
    let subcommand = subcommand_str.to_ascii_lowercase();
    if !SUBCOMMANDS.contains(&subcommand.as_str()) {
        return Err(ModError::raw(
            ModErrorKind::UnknownSubcommand,
            format!("Invalid subcommand, choose one from : {values}."),
        ));
    }

    // Option values given as strings are parsed up front, like the options parser would.
    let from_args = parse_option_list("from", &opts.from, ModuleArg::parse_list)?;
    let usage_args = parse_option_list("extension_usages", &opts.extension_usages, ModuleArg::parse_list)?;
    let base_module_arg = ModuleArg::parse(&opts.base_module)
        .map_err(|e| ModError::raw(ModErrorKind::OptionParsing, format!("While parsing option --base_module={}: {e}", opts.base_module)))?;
    let filter_args: Option<Vec<ExtensionArg>> = match &opts.extension_filter {
        None => None,
        Some(v) => Some(parse_option_list("extension_filter", v, ExtensionArg::parse_list)?),
    };

    validate_args(&subcommand, opts, args)?;

    Ok((subcommand, from_args, usage_args, base_module_arg, filter_args))
}

fn run_mod_inner(
    res: &Resolution,
    subcommand_str: &str,
    args: &[String],
    opts: &ModOptions,
    repo_provider: &dyn RepoSpecProvider,
    ext_info: &dyn ExtensionInfoProvider,
) -> Result<String, ModError> {
    let (subcommand, from_args, usage_args, base_module_arg, filter_args) = validate_early_inner(subcommand_str, args, opts)?;
    let subcommand = subcommand.as_str();

    if subcommand == "tidy" || subcommand == "dump_repo_mapping" {
        return Err(ModError::raw(ModErrorKind::Unsupported, format!("{subcommand} not handled here")));
    }

    // Build the inspector.
    let extension_repos: Vec<(ExtensionId, Vec<String>)> = res
        .extension_ids()
        .into_iter()
        .filter_map(|id| {
            let repos = ext_info.generated_repos(&id)?;
            Some((id, repos))
        })
        .collect();
    let insp = Inspector::from_resolution(res, extension_repos);

    // Extract and check the --base_module argument first to use it when parsing the other args.
    let root_module = insp
        .dep_graph
        .get(&ModuleKey::root())
        .ok_or_else(|| bad_args("the root module is missing from the dependency graph"))?;
    let base_module_key: ModuleKey = {
        let r = (|| -> Result<ModuleKey, ModError> {
            let keys = base_module_arg.resolve_to_module_keys(
                &insp,
                &root_module.deps,
                &root_module.unused_deps,
                false,
                false,
            )?;
            if keys.len() > 1 {
                return Err(bad_args(format!(
                    "The --base_module option can only specify exactly one module version, choose one of: {}.",
                    keys.iter().map(|k| k.to_string()).collect::<Vec<_>>().join(", ")
                )));
            }
            Ok(keys.into_iter().next().unwrap())
        })();
        r.map_err(|e| {
            bad_args(format!(
                "In --base_module {} option: {} (Note that unused modules cannot be used here)",
                base_module_arg, e.message
            ))
        })?
    };
    let base_module = insp.dep_graph.get(&base_module_key).expect("base module exists");
    let base_deps = base_module.deps.clone();
    let base_unused = base_module.unused_deps.clone();

    let mut args_as_modules: Vec<ModuleKey> = vec![];
    let mut args_as_extensions: Vec<ExtensionId> = vec![];
    let mut args_as_repos: Vec<(String, String)> = vec![];
    match subcommand {
        "graph" => {
            if !args.is_empty() {
                return Err(ModError::raw(
                    ModErrorKind::TooManyArguments,
                    "the 'graph' command doesn't take extra arguments",
                ));
            }
        }
        "show_repo" => {
            let base_mapping = RepoMapping {
                entries: res.repo_mapping(&base_module_key),
                ordered: res.repo_mapping_ordered(&base_module_key),
                context_repo: insp.canonical_name(&base_module_key).unwrap_or("").to_string(),
            };
            args_as_repos = repos_to_show(opts, &insp, res, &base_mapping, args)?;
        }
        "show_extension" => {
            for arg in args {
                let id = ExtensionArg::parse(arg)
                    .map_err(bad_args)
                    .and_then(|e| e.resolve_to_extension_id(&insp, &base_deps, &base_unused))
                    .map_err(|e| bad_args(format!("In extension argument {arg}: {}", e.message)))?;
                args_as_extensions.push(id);
            }
            args_as_extensions = sorted_unique_ext_ids(args_as_extensions);
        }
        _ => {
            for arg in args {
                let keys = ModuleArg::parse(arg)
                    .map_err(bad_args)
                    .and_then(|m| {
                        m.resolve_to_module_keys(&insp, &base_deps, &base_unused, opts.include_unused, true)
                    })
                    .map_err(|e| bad_args(format!("In module argument {arg}: {}", e.message)))?;
                for k in keys {
                    if !args_as_modules.contains(&k) {
                        args_as_modules.push(k);
                    }
                }
            }
        }
    }

    // Extract and check the --from and --extension_usages arguments.
    let from_keys = module_arg_list_to_keys(&from_args, &insp, &base_deps, &base_unused, opts.include_unused)
        .map_err(|e| bad_args(format!("In --from {} option: {}", list_string(&from_args), e.message)))?;
    let usage_keys = module_arg_list_to_keys(&usage_args, &insp, &base_deps, &base_unused, opts.include_unused)
        .map_err(|e| {
            bad_args(format!(
                "In --extension_usages {} option: {} (Note that unused modules cannot be used here)",
                list_string(&usage_args),
                e.message
            ))
        })?;

    // Extract and check the --extension_filter argument.
    let mut filter_extensions: Option<MaybeCompleteExtSet> = None;
    if is_graph_subcommand(subcommand) {
        if let Some(fa) = &filter_args {
            if fa.is_empty() {
                filter_extensions = Some(MaybeCompleteExtSet::Complete);
            } else {
                let mut ids = vec![];
                for a in fa {
                    ids.push(a.resolve_to_extension_id(&insp, &base_deps, &base_unused).map_err(|e| {
                        bad_args(format!("In --extension_filter {} option: {}", list_string(fa), e.message))
                    })?);
                }
                filter_extensions = Some(MaybeCompleteExtSet::Set(sorted_unique_ext_ids(ids)));
            }
        }
    }

    // Resolve the repo definitions to show.
    let mut definitions: Vec<(String, RepoDefinition)> = vec![];
    if subcommand == "show_repo" {
        let explicit = !opts.all_repos && !opts.all_visible_repos;
        for (key, canonical) in &args_as_repos {
            match repo_definition(res, &insp, canonical, repo_provider, ext_info) {
                Some(d) => definitions.push((key.clone(), d)),
                None if explicit => return Err(bad_args(format!("In repo argument {key}: no such repo"))),
                None => {}
            }
        }
    }

    // Workaround to allow different default value for DEPS and EXPLAIN.
    let mut effective = opts.clone();
    if effective.depth < 1 {
        effective.depth = match subcommand {
            "explain" => 1,
            "deps" => 2,
            _ => i32::MAX,
        };
    }

    let mut executor = ModExecutor::new(&insp, filter_extensions, &effective);
    match subcommand {
        "graph" => executor.graph(&from_keys),
        "deps" => executor.graph(&args_as_modules),
        "path" => executor.path(&from_keys, &args_as_modules),
        "all_paths" | "explain" => executor.all_paths(&from_keys, &args_as_modules),
        "show_repo" => {
            let ext_dir = repo_provider.external_directory();
            match opts.output {
                OutputFormat::StreamedJsonProto => {
                    let def_refs: Vec<(&str, &repo::RepoDefinition)> =
                        definitions.iter().map(|(k, d)| (k.as_str(), d)).collect();
                    return Ok(repo::print_streamed_jsonproto(&def_refs, &ext_dir));
                }
                OutputFormat::StreamedProto => {
                    let def_refs: Vec<(&str, &repo::RepoDefinition)> =
                        definitions.iter().map(|(k, d)| (k.as_str(), d)).collect();
                    let bytes = repo::print_streamed_proto(&def_refs, &ext_dir);
                    let s: String = bytes.iter().map(|&b| b as char).collect();
                    return Ok(format!("{}{s}", crate::session::RAW_BYTES_MARK));
                }
                _ => {
                    let mut out = String::new();
                    for (key, def) in &definitions {
                        out.push_str(&repo::print_starlark(key, def, &ext_dir));
                    }
                    return Ok(out);
                }
            }
        }
        "show_extension" => executor.show_extension(&args_as_extensions, &usage_keys)?,
        _ => unreachable!("unexpected subcommand: {subcommand}"),
    }
    Ok(executor.take_output())
}

#[cfg(test)]
mod tests;
