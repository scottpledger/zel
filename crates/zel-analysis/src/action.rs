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

//! Typed action model produced by analysis (`Action`, `SpawnAction`, `FileWriteAction`,
//! `SymlinkAction`, `TemplateExpansionAction`, `ParameterFileWriteAction`) plus the
//! [`ActionGraph`] that performs action-conflict detection (`ActionConflictException`).
//!
//! The execution phase consumes either this model directly or the legacy flat
//! [`ActionAnalysisMetadata`] through [`Action::lower`].

use crate::{ActionAnalysisMetadata, Artifact};
use std::collections::BTreeMap;
use zel_cmdline::Label;
use zel_util::Fingerprint;
use zel_vfs::PathFragment;

/// Resource request of a spawn (`resource_set` callback result, `ResourceSet.java`).
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceSet {
    pub memory_mb: f64,
    pub cpu: f64,
    pub local_test_count: i32,
}

// Resource values are finite numbers from Starlark; treat equality as total.
impl Eq for ResourceSet {}

impl Default for ResourceSet {
    /// `ResourceSet.createWithRamCpu(250, 1)` — the Bazel default for spawn actions.
    fn default() -> Self {
        Self {
            memory_mb: 250.0,
            cpu: 1.0,
            local_test_count: 0,
        }
    }
}

/// Parameter-file formats (`ParameterFileType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamFileFormat {
    /// One shell-quoted argument per whitespace-separated token, on one line per arg (`SHELL_QUOTED`).
    Shell,
    /// One raw argument per line (`UNQUOTED`).
    Multiline,
    /// One raw argument per line, flag-style: grouped `--flag=value` args, spilling only `--` args.
    FlagPerLine,
    /// `ParameterFileType.GCC_QUOTED` (`GccParamFileEscaper`).
    GccQuoted,
    /// `ParameterFileType.WINDOWS` (`WindowsParamFileEscaper`).
    Windows,
}

/// `Args.use_param_file` / `set_param_file_format` settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamFileInfo {
    /// Format string for the param-file flag; `%s` is replaced by the param file path
    /// (default `@%s`).
    pub flag_format: String,
    pub format: ParamFileFormat,
    /// Always use a param file even when the command line is short.
    pub use_always: bool,
    /// Number of leading arguments (the executable and fixed flags) left on the command line.
    pub leading_args: usize,
    /// Slice ranges `(start, end)` in `CommandLineArgs.args` recorded by each `Args.add*` call,
    /// used by `ParamFileFormat::FlagPerLine` to join `flag` and `rest` as `flag=rest`.
    pub flag_groups: Vec<(usize, usize)>,
    /// Optional explicit parameter file path (e.g. `<output>.params` for `CppCompileAction`).
    pub explicit_path: Option<String>,
}

impl Default for ParamFileInfo {
    fn default() -> Self {
        Self {
            flag_format: "@%s".to_string(),
            format: ParamFileFormat::Shell,
            use_always: false,
            leading_args: 0,
            flag_groups: Vec::new(),
            explicit_path: None,
        }
    }
}

impl ParamFileInfo {
    /// Applies `flag_per_line` grouping (`first=rest`) to `args[lead..]` using `self.flag_groups`.
    pub fn format_flag_groups(&self, lead: usize, args: &[String]) -> Vec<String> {
        if self.format != ParamFileFormat::FlagPerLine || self.flag_groups.is_empty() {
            return args[lead..].to_vec();
        }
        let mut out = Vec::new();
        let mut cursor = lead;
        for &(start, end) in &self.flag_groups {
            if end <= lead || start >= args.len() {
                continue;
            }
            let s = start.max(lead);
            let e = end.min(args.len());
            while cursor < s {
                out.push(args[cursor].clone());
                cursor += 1;
            }
            if e - s >= 2 && !args[s..e].iter().any(|item| item.contains("\u{1}ZEL_TREE")) {
                if args[s].is_empty() {
                    out.push(args[s + 1..e].join(" "));
                } else {
                    out.push(format!("{}={}", args[s], args[s + 1..e].join(" ")));
                }
            } else {
                out.extend(args[s..e].iter().cloned());
            }
            cursor = e;
        }
        while cursor < args.len() {
            out.push(args[cursor].clone());
            cursor += 1;
        }
        out
    }
}

/// One fully materialised `ctx.actions.args()` object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLineArgs {
    /// Expanded arguments in order (artifacts have been rendered as exec paths).
    pub args: Vec<String>,
    /// Present if `use_param_file()` was called on the Args object.
    pub param_file: Option<ParamFileInfo>,
}

/// Quoting / limits used when deciding whether a param file is needed. Bazel derives the limit
/// from the platform (`CommandLineLimits`); on unix-like hosts the safe value is 128 KiB.
pub const DEFAULT_PARAM_FILE_THRESHOLD: usize = 128 * 1024;

/// Shell-quotes one argument for [`ParamFileFormat::Shell`] (`ShellEscaper.escapeString`).
pub fn shell_escape(s: &str) -> String {
    // `ShellEscaper.escape`: safe chars `@%-_+:,./` + ASCII alphanumerics, plus `~` unless leading.
    let safe = |c: char| c.is_ascii_alphanumeric() || "@%-_+:,./".contains(c);
    if !s.is_empty()
        && (s.chars().all(safe) || (!s.starts_with('~') && s.chars().all(|c| safe(c) || c == '~')))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Quotes one argument for [`ParamFileFormat::GccQuoted`] (`GccParamFileEscaper.escapeString`).
pub fn gcc_escape(s: &str) -> String {
    zel_util::param_escaper::gcc_escape_param(s)
}

/// Quotes one argument for [`ParamFileFormat::Windows`] (`WindowsParamFileEscaper.escapeString`).
pub fn windows_param_escape(s: &str) -> String {
    zel_util::param_escaper::windows_escape_param(s)
}

/// The contents of a `FileWriteAction`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileContent {
    /// `ctx.actions.write(output, "text")`
    Literal(String),
    /// Binary payload (e.g. `genquery(compressed_output = True)`).
    Bytes(Vec<u8>),
    /// `ctx.actions.write(output, args)`: arguments rendered into a param file.
    Args {
        args: Vec<String>,
        format: ParamFileFormat,
        flag_groups: Vec<(usize, usize)>,
    },
}

impl FileContent {
    /// Bytes written to disk when no execution-time tree expansion is needed.
    pub fn render(&self) -> Vec<u8> {
        match self {
            FileContent::Literal(s) => s.as_bytes().to_vec(),
            FileContent::Bytes(b) => b.clone(),
            FileContent::Args {
                args,
                format,
                flag_groups,
            } => {
                let pf = ParamFileInfo {
                    format: *format,
                    flag_groups: flag_groups.clone(),
                    ..Default::default()
                };
                let effective = pf.format_flag_groups(0, args);
                render_param_file(&effective, *format).into_bytes()
            }
        }
    }
}

/// Escapes one argument for [`ParamFileFormat::GccQuoted`] (`GccParamFileEscaper.escapeString`).
pub fn gcc_escape(s: &str) -> String {
    zel_util::param_escaper::gcc_escape_param(s)
}

/// Escapes one argument for [`ParamFileFormat::Windows`] (`WindowsParamFileEscaper.escapeString`).
pub fn windows_escape(s: &str) -> String {
    zel_util::param_escaper::windows_escape_param(s)
}

/// Renders a parameter file body (`ParameterFile.writeContent`).
pub fn render_param_file(args: &[String], format: ParamFileFormat) -> String {
    let mut s = String::new();
    for a in args {
        match format {
            ParamFileFormat::Shell => s.push_str(&shell_escape(a)),
            ParamFileFormat::GccQuoted => s.push_str(&zel_util::param_escaper::gcc_escape_param(a)),
            ParamFileFormat::Windows => {
                s.push_str(&zel_util::param_escaper::windows_escape_param(a))
            }
            ParamFileFormat::Multiline | ParamFileFormat::FlagPerLine => s.push_str(a),
        }
        s.push('\n');
    }
    s
}

/// Kind-specific payload of an [`Action`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionKind {
    /// `ctx.actions.run` / `run_shell` (`SpawnAction`).
    Spawn {
        /// Executable (path string; for `run`, the exec path or program name; for `run_shell`,
        /// the shell).
        executable: String,
        /// Arguments following the executable: `ctx.actions.run(arguments=...)` and
        /// `run_shell(command=...)`'s `-c <command>`, in order.
        arguments: Vec<CommandLineArgs>,
        /// `run_shell`'s command string (already part of `arguments`); kept for aquery.
        shell_command: Option<String>,
    },
    /// `ctx.actions.write` (`FileWriteAction` / `ParameterFileWriteAction`).
    FileWrite {
        content: FileContent,
        is_executable: bool,
    },
    /// `ctx.actions.symlink` (`SymlinkAction`): output is a symlink to `target_file` (an
    /// artifact input) or, for `target_path`, an unresolved symlink.
    Symlink {
        target_file: Option<Artifact>,
        target_path: Option<String>,
        is_executable: bool,
    },
    /// `ctx.actions.expand_template` (`TemplateExpansionAction`).
    TemplateExpand {
        template: Artifact,
        /// In dict order (`ImmutableMap` insertion order).
        substitutions: Vec<(String, String)>,
        is_executable: bool,
        /// `computed_substitutions`, applied after plain ones.
        computed_substitutions: Vec<(String, String)>,
    },
    /// `ctx.actions.transform_version_file` / `ctx.actions.transform_info_file`
    /// (`BuildInfoFileWriteAction`).
    BuildInfoTranslate {
        template: Artifact,
        status_file: Artifact,
        is_volatile: bool,
        translator: crate::BuildInfoTranslator,
    },
}

/// Target type for `SymlinkAction` (`SymlinkAction.TargetType`) / `UnresolvedSymlinkAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SymlinkTargetType {
    #[default]
    Unspecified,
    File,
    Directory,
    Executable,
    Fileset,
    Other,
}

/// A registered action (`ctx.actions.*`). All the common fields live on the struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub owner: Label,
    /// Rule class of the owner (`ActionOwner.getTargetKind`).
    pub owner_kind: String,
    /// Configuration checksum string of the owner (`ActionOwner.getConfigurationChecksum`).
    pub owner_config: String,
    pub mnemonic: String,
    /// `progress_message` (already `%{label}`/`%{input}`/`%{output}`-substituted).
    pub progress_message: Option<String>,
    pub env: BTreeMap<String, String>,
    pub execution_requirements: BTreeMap<String, String>,
    /// All inputs, including tools (ordered, de-duplicated by exec path).
    pub inputs: Vec<Artifact>,
    /// Subset of the inputs that are tools (`tools=`/`executable=` of `run`).
    pub tools: Vec<Artifact>,
    pub outputs: Vec<Artifact>,
    pub resource_set: ResourceSet,
    pub use_default_shell_env: bool,
    /// The complete environment of the action (`SpawnAction.createActionEnvironment`) once the
    /// configuration is known (`apply_configuration_env`); `None` means just `env`.
    pub action_env: Option<zel_util::action_env::ActionEnvironment>,
    /// `toolchain=` / `exec_group=` selecting the execution platform.
    pub exec_group: Option<String>,
    /// Execution platform label for this action (`ActionOwner.getExecutionPlatform`).
    pub exec_platform: Option<String>,
    /// Execution properties from the execution platform / rule (`ActionOwner.getExecProperties`).
    pub exec_properties: BTreeMap<String, String>,
    /// Symlink target type (`SymlinkAction.TargetType` / `UnresolvedSymlinkAction`).
    pub symlink_target_type: SymlinkTargetType,
    /// `use_exec_root_for_source` for `actions.symlink`.
    pub use_exec_root_for_source: bool,
    /// True for `actions.do_nothing` (`PseudoAction`): recorded in analysis, no-op on execution.
    pub is_pseudo: bool,
    /// Runfiles-tree inputs (`input_manifests`): executable -> runfiles manifest artifact.
    pub input_manifests: Vec<Artifact>,
    /// Set for `JavaCompileAction`s that run with a reduced classpath.
    pub java_compile: Option<crate::JavaCompileSpec>,
    /// Inputs of `Action.computeKey` that are not otherwise part of this model (nested sets).
    pub key_info: Option<std::sync::Arc<ActionKeyInfo>>,
    pub kind: ActionKind,
}

/// `Package.Metadata` as `RepoMappingManifestAction` fingerprints it: repository name and the
/// fingerprint of the repository mapping of the package's repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkgMeta {
    pub package: String,
    pub repo: String,
    pub mapping: Vec<(String, String)>,
}

/// Extra `computeKey` inputs of specific action types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionKeyInfo {
    RepoMapping {
        packages: crate::nested_set::NSet<PkgMeta>,
        /// `(exec path, owner repository name)` of every runfiles artifact.
        runfiles_owner_repos: crate::nested_set::NSet<(String, String)>,
        has_runfiles_symlinks: bool,
        /// `(path, first path segment)` of every root symlink.
        root_symlink_segments: crate::nested_set::NSet<(String, String)>,
        workspace_name: String,
        compact: bool,
    },
    /// `TestRunnerAction.computeKey` (`getKey` inputs that the synthesized spawn does not carry).
    TestRunner(Box<TestRunnerKey>),
}

/// The inputs of `TestRunnerAction.computeKey`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TestRunnerKey {
    pub args: Vec<String>,
    pub test_filter: String,
    pub fail_fast: bool,
    pub run_under: String,
    pub coverage_env: BTreeMap<String, String>,
    /// `(fixed, inherited)` of the rule's `RunEnvironmentInfo` (extra test environment).
    pub extra_env: (BTreeMap<String, String>, Vec<String>),
    pub action_env: (BTreeMap<String, String>, Vec<String>),
    pub test_env: (BTreeMap<String, String>, Vec<String>),
    pub size: String,
    pub timeout_category: String,
    /// `java.time.Duration.toString()` of the effective timeout.
    pub timeout: String,
    pub tags: Vec<String>,
    pub remotable: bool,
    pub shard_num: i32,
    pub total_shards: i32,
    pub run_number: i32,
    pub total_runs: i32,
    pub coverage: bool,
    pub zip_undeclared: bool,
}

/// Propagates execution-relevant `tags` of `rule` onto `exec_info`
/// (`TargetUtils.getExecutionInfo` when `--incompatible_allow_tags_propagation` is enabled).
pub fn propagate_rule_tags(
    rule: &zel_packages::RuleInstance,
    exec_info: &mut BTreeMap<String, String>,
) {
    let Some(zel_packages::attr::AttrValue::StrList(tags)) = rule.attr_or_default("tags") else {
        return;
    };
    for t in tags {
        if t.starts_with("requires-")
            || t.starts_with("cpu:")
            || t.starts_with("resources:")
            || t.starts_with("gpu:")
            || matches!(
                t.as_str(),
                "no-sandbox"
                    | "no-cache"
                    | "no-remote"
                    | "no-remote-cache"
                    | "no-remote-exec"
                    | "no-remote-imageless-invocation"
                    | "no-local"
                    | "local"
                    | "block-network"
                    | "requires-network"
                    | "supports-workers"
                    | "supports-multiplex-workers"
                    | "supports-worker-cancellation"
                    | "supports-graceful-termination"
                    | "supports-path-mapping"
            )
        {
            exec_info.entry(t).or_default();
        }
    }
}

/// Applies `--modify_execution_info` regex modifiers (`ExecutionRequirementsModifier`) to `reqs`.
pub fn apply_modify_execution_info_to_map(
    cfg: &crate::BuildConfigurationKey,
    mnemonic: &str,
    reqs: &mut BTreeMap<String, String>,
) {
    let bo = cfg.build_options();
    let entries: Vec<String> = match bo.get("modify_execution_info") {
        Some(crate::build_options::ConfigValue::List(vs)) => vs.clone(),
        Some(crate::build_options::ConfigValue::Scalar(s)) if !s.is_empty() => vec![s.clone()],
        _ => cfg
            .options
            .get("modify_execution_info")
            .filter(|s| !s.is_empty())
            .cloned()
            .into_iter()
            .collect(),
    };
    if entries.is_empty() {
        return;
    }
    let additive = cfg.bool_option("incompatible_modify_execution_info_additive", true);
    let active_entries: &[String] = if additive {
        entries.as_slice()
    } else {
        &entries[entries.len() - 1..]
    };
    for entry in active_entries {
        let raw = entry
            .strip_prefix("ExecutionInfoModifier{option=")
            .and_then(|s| s.split_once(", expressions=[").map(|(opt, _)| opt))
            .unwrap_or(entry.as_str());
        let mut current_matches = false;
        for part in raw.split(',').filter(|s| !s.is_empty()) {
            let bytes = part.as_bytes();
            let mut eq_idx: Option<usize> = None;
            for i in (1..bytes.len().saturating_sub(2)).rev() {
                if bytes[i] == b'='
                    && (bytes[i + 1] == b'+' || bytes[i + 1] == b'-')
                    && i + 2 < bytes.len()
                {
                    eq_idx = Some(i);
                    break;
                }
            }
            if let Some(i) = eq_idx {
                let pat = &part[..i];
                let spec_item = &part[i + 1..];
                current_matches =
                    zel_util::java_regex::Regex::new(pat).is_ok_and(|re| re.full_match(mnemonic));
                if current_matches {
                    apply_exec_info_spec_item(spec_item, reqs);
                }
            } else if let Some((pat, spec_item)) = part.split_once('=') {
                current_matches =
                    zel_util::java_regex::Regex::new(pat).is_ok_and(|re| re.full_match(mnemonic));
                if current_matches {
                    apply_exec_info_spec_item(spec_item, reqs);
                }
            } else if current_matches {
                apply_exec_info_spec_item(part, reqs);
            }
        }
    }
}

fn apply_exec_info_spec_item(item: &str, reqs: &mut BTreeMap<String, String>) {
    if let Some(rem) = item.strip_prefix('-') {
        reqs.remove(rem);
    } else {
        let body = item.strip_prefix('+').unwrap_or(item);
        match body.split_once(':') {
            Some((k, v)) => {
                reqs.insert(k.to_string(), v.to_string());
            }
            None => {
                reqs.insert(body.to_string(), String::new());
            }
        }
    }
}

type LoweredParamFile = (usize, String, Vec<u8>, crate::ParamFileWriteSpec);

impl Action {
    /// Starts an action with defaults for everything except `owner`, `mnemonic`, and `kind`.
    pub fn new(owner: Label, mnemonic: &str, kind: ActionKind) -> Self {
        Self {
            owner,
            owner_kind: String::new(),
            owner_config: String::new(),
            mnemonic: mnemonic.to_string(),
            progress_message: None,
            env: BTreeMap::new(),
            execution_requirements: BTreeMap::new(),
            inputs: Vec::new(),
            tools: Vec::new(),
            outputs: Vec::new(),
            resource_set: ResourceSet::default(),
            use_default_shell_env: false,
            action_env: None,
            exec_group: None,
            exec_platform: None,
            exec_properties: BTreeMap::new(),
            symlink_target_type: SymlinkTargetType::Unspecified,
            use_exec_root_for_source: false,
            is_pseudo: false,
            input_manifests: Vec::new(),
            java_compile: None,
            key_info: None,
            kind,
        }
    }

    /// Applies `--modify_execution_info` regex rules (`ExecutionRequirementsModifier`) to `reqs`.
    pub fn apply_modify_execution_info(&mut self, cfg: &crate::BuildConfigurationKey) {
        apply_modify_execution_info_to_map(cfg, &self.mnemonic, &mut self.execution_requirements);
    }

    /// Computes the action's environment from the configuration of its owner
    /// (`SpawnAction.createActionEnvironment`): with `use_default_shell_env` the configuration's
    /// action environment (`--action_env`, or `--host_action_env` in exec configurations, plus the
    /// strict/default `PATH`) with `env` laid over it, otherwise only `env`.
    pub fn apply_configuration_env(&mut self, cfg: &crate::BuildConfigurationKey) {
        if !matches!(self.kind, ActionKind::Spawn { .. }) {
            return;
        }
        self.action_env = Some(zel_util::action_env::create_spawn_action_environment(
            &cfg.action_environment(),
            self.use_default_shell_env,
            &self.env,
        ));
        self.apply_modify_execution_info(cfg);
    }

    pub fn primary_output(&self) -> Option<&Artifact> {
        self.outputs.first()
    }

    pub fn primary_input(&self) -> Option<&Artifact> {
        self.inputs.first()
    }

    /// Whether this action may be shared (`AbstractAction.isShareable`).
    pub fn is_shareable(&self) -> bool {
        true
    }

    /// Whether two such actions generating the same output may coexist
    /// (`Actions.canBeShared`): same mnemonic, same key, equal inputs/outputs ignoring owner.
    pub fn can_be_shared(&self, other: &Action) -> bool {
        self.is_shareable()
            && other.is_shareable()
            && self.mnemonic == other.mnemonic
            && self.key() == other.key()
            && same_artifacts_without_owner(&self.inputs, &other.inputs)
            && same_artifacts_without_owner(&self.outputs, &other.outputs)
    }

    /// `Action.getKey`: fingerprint over everything affecting the action's behaviour.
    pub fn key(&self) -> String {
        let mut fp = Fingerprint::new();
        if self.is_pseudo {
            fp.add_str("PseudoAction")
                .add_str(&self.mnemonic)
                .add_str(&format!("empty action {}", self.owner.canonical_form()));
            for inp in &self.inputs {
                fp.add_str(inp.exec_path.path_string());
            }
            return fp.hex_digest_and_reset();
        }
        fp.add_str(&self.mnemonic);
        match &self.kind {
            ActionKind::Spawn {
                executable,
                arguments,
                shell_command,
            } => {
                fp.add_str("Spawn").add_str(executable);
                for a in arguments {
                    match &a.param_file {
                        Some(p) if p.format == ParamFileFormat::FlagPerLine => {
                            let lead = p.leading_args.min(a.args.len());
                            fp.add_strings(a.args[..lead].iter());
                            let grouped = p.format_flag_groups(lead, &a.args);
                            fp.add_strings(grouped.iter());
                        }
                        _ => {
                            fp.add_strings(a.args.iter());
                        }
                    }
                    if let Some(p) = &a.param_file {
                        let fmt_tag = match p.format {
                            ParamFileFormat::Shell => "SHELL_QUOTED",
                            ParamFileFormat::GccQuoted => "GCC_QUOTED",
                            ParamFileFormat::Windows => "WINDOWS",
                            ParamFileFormat::Multiline => "UNQUOTED",
                            ParamFileFormat::FlagPerLine => "FLAG_PER_LINE",
                        };
                        fp.add_str(&p.flag_format)
                            .add_str(fmt_tag)
                            .add_bool(p.use_always);
                        if p.format == ParamFileFormat::FlagPerLine && !p.flag_groups.is_empty() {
                            fp.add_i32(p.flag_groups.len() as i32);
                            for &(s, e) in &p.flag_groups {
                                fp.add_i32(s as i32).add_i32(e as i32);
                            }
                        }
                    }
                }
                fp.add_str(shell_command.as_deref().unwrap_or(""));
                if let Some(ep) = &self.exec_platform {
                    fp.add_str(ep);
                }
                for (k, v) in &self.exec_properties {
                    fp.add_str(k).add_str(v);
                }
            }
            ActionKind::FileWrite {
                content,
                is_executable,
            } => {
                fp.add_str("332877c7-ca9f-4731-b387-54f620408522")
                    .add_bool(*is_executable)
                    .add_bytes(&content.render());
                if let FileContent::Args {
                    format,
                    flag_groups,
                    ..
                } = content
                {
                    if *format == ParamFileFormat::FlagPerLine && !flag_groups.is_empty() {
                        fp.add_i32(flag_groups.len() as i32);
                        for &(s, e) in flag_groups {
                            fp.add_i32(s as i32).add_i32(e as i32);
                        }
                    }
                }
            }
            ActionKind::Symlink {
                target_file,
                target_path,
                is_executable,
            } => {
                let tt = match self.symlink_target_type {
                    SymlinkTargetType::Unspecified => "UNSPECIFIED",
                    SymlinkTargetType::File => "FILE",
                    SymlinkTargetType::Directory => "DIRECTORY",
                    SymlinkTargetType::Executable => "EXECUTABLE",
                    SymlinkTargetType::Fileset => "FILESET",
                    SymlinkTargetType::Other => "OTHER",
                };
                fp.add_str("Symlink")
                    .add_str(tt)
                    .add_bool(self.use_exec_root_for_source)
                    .add_str(
                        target_file
                            .as_ref()
                            .map(|a| a.exec_path.path_string())
                            .unwrap_or(""),
                    )
                    .add_str(target_path.as_deref().unwrap_or(""))
                    .add_bool(*is_executable);
            }
            ActionKind::TemplateExpand {
                template,
                substitutions,
                is_executable,
                computed_substitutions,
            } => {
                fp.add_str("TemplateExpand")
                    .add_str(template.exec_path.path_string())
                    .add_bool(*is_executable);
                for (k, v) in substitutions {
                    fp.add_str(k).add_str(v);
                }
                for (k, v) in computed_substitutions {
                    fp.add_str(k).add_str(v);
                }
            }
            ActionKind::BuildInfoTranslate {
                template,
                status_file,
                is_volatile,
                translator,
            } => {
                fp.add_str("TranslateBuildInfo")
                    .add_str(template.exec_path.path_string())
                    .add_str(status_file.exec_path.path_string())
                    .add_bool(*is_volatile)
                    .add_str(&translator.key);
            }
        }
        for (k, v) in &self.env {
            fp.add_str(k).add_str(v);
        }
        if let Some(e) = &self.action_env {
            // `ActionEnvironment.addTo`: fixed values, inherited names and unset names.
            e.add_to(&mut fp);
        }
        for (k, v) in &self.execution_requirements {
            fp.add_str(k).add_str(v);
        }
        fp.add_bool(self.use_default_shell_env);
        for o in &self.outputs {
            fp.add_str(o.exec_path.path_string());
        }
        if let Some(info) = &self.key_info {
            match info.as_ref() {
                ActionKeyInfo::RepoMapping {
                    packages,
                    runfiles_owner_repos,
                    has_runfiles_symlinks,
                    root_symlink_segments,
                    workspace_name,
                    compact,
                } => {
                    fp.add_str("RepoMapping")
                        .add_str(workspace_name)
                        .add_bool(*has_runfiles_symlinks)
                        .add_bool(*compact);
                    for p in packages.to_vec() {
                        fp.add_str(&p.package).add_str(&p.repo);
                        for (k, v) in p.mapping {
                            fp.add_str(&k).add_str(&v);
                        }
                    }
                    for (ep, r) in runfiles_owner_repos.to_vec() {
                        fp.add_str(&ep).add_str(&r);
                    }
                    for (p, seg) in root_symlink_segments.to_vec() {
                        fp.add_str(&p).add_str(&seg);
                    }
                }
                ActionKeyInfo::TestRunner(tr) => {
                    fp.add_str("TestRunner")
                        .add_strings(tr.args.iter())
                        .add_str(&tr.test_filter)
                        .add_bool(tr.fail_fast)
                        .add_str(&tr.run_under)
                        .add_str(&tr.size)
                        .add_str(&tr.timeout_category)
                        .add_str(&tr.timeout)
                        .add_strings(tr.tags.iter())
                        .add_bool(tr.remotable)
                        .add_i32(tr.shard_num)
                        .add_i32(tr.total_shards)
                        .add_i32(tr.run_number)
                        .add_i32(tr.total_runs)
                        .add_bool(tr.coverage)
                        .add_bool(tr.zip_undeclared);
                    for (k, v) in &tr.coverage_env {
                        fp.add_str(k).add_str(v);
                    }
                    for (k, v) in &tr.extra_env.0 {
                        fp.add_str(k).add_str(v);
                    }
                    fp.add_strings(tr.extra_env.1.iter());
                    for (k, v) in &tr.action_env.0 {
                        fp.add_str(k).add_str(v);
                    }
                    fp.add_strings(tr.action_env.1.iter());
                    for (k, v) in &tr.test_env.0 {
                        fp.add_str(k).add_str(v);
                    }
                    fp.add_strings(tr.test_env.1.iter());
                }
            }
        }
        fp.hex_digest_and_reset()
    }

    /// Full command line (`executable` followed by arguments), with param files folded in
    /// according to `ParamFileInfo`; the second element lists `(param file exec path, body)`
    /// pairs that must be written before the spawn runs.
    pub fn command_line(&self) -> (Vec<String>, Vec<(String, Vec<u8>)>) {
        let (argv, params) = self.command_line_with_specs();
        (
            argv,
            params
                .into_iter()
                .map(|(_, path, body, _)| (path, body))
                .collect(),
        )
    }

    fn command_line_with_specs(&self) -> (Vec<String>, Vec<LoweredParamFile>) {
        let mut argv = Vec::new();
        let mut params = Vec::new();
        let ActionKind::Spawn {
            executable,
            arguments,
            ..
        } = &self.kind
        else {
            return (argv, params);
        };
        argv.push(executable.clone());
        let primary = self
            .outputs
            .first()
            .map(|o| o.exec_path.path_string().to_string())
            .unwrap_or_default();
        let mut spilled_idx = 0usize;
        for (i, a) in arguments.iter().enumerate() {
            match &a.param_file {
                Some(pf) => {
                    let lead = pf.leading_args.min(a.args.len());
                    let grouped = pf.format_flag_groups(lead, &a.args);
                    let total: usize = a.args[..lead]
                        .iter()
                        .chain(grouped.iter())
                        .map(|s| s.encode_utf16().count() + 1)
                        .sum();
                    if pf.use_always || total > DEFAULT_PARAM_FILE_THRESHOLD {
                        argv.extend(a.args[..lead].iter().cloned());
                        let path = match &pf.explicit_path {
                            Some(ep) => ep.clone(),
                            None => {
                                let p = format!("{primary}-{spilled_idx}.params");
                                spilled_idx += 1;
                                p
                            }
                        };
                        let formatted = crate::args::format_arg(&pf.flag_format, &path)
                            .unwrap_or_else(|_| pf.flag_format.replace("%s", &path));
                        argv.push(formatted);
                        if pf.format == ParamFileFormat::FlagPerLine {
                            // `CommandLines.expand` + `ParameterFile.{flagsOnly,nonFlags}`:
                            // `--`-prefixed lines go to the param file and non-`--` lines stay on
                            // `argv` in order. Slices containing deferred tree-artifact markers are
                            // routed to both `ParamFileWriteSpec` (`flags_only: true`) and a
                            // complementary `nonFlags` wrapper on `argv` so post-expansion strings
                            // are partitioned 1-for-1 at execution time without dropping values.
                            if !pf.flag_groups.is_empty() {
                                let mut raw_flag_args: Vec<String> = Vec::new();
                                let mut spec_groups: Vec<(usize, usize)> = Vec::new();
                                let mut static_flags: Vec<String> = Vec::new();
                                let mut cursor = lead;
                                let mut push_slice = |s: usize, e: usize| {
                                    let slice = &a.args[s..e];
                                    if slice.is_empty() {
                                        return;
                                    }
                                    if slice.iter().any(|x| x.contains("\u{1}ZEL_TREE")) {
                                        let ns = raw_flag_args.len();
                                        raw_flag_args.extend_from_slice(slice);
                                        spec_groups.push((ns, raw_flag_args.len()));
                                        argv.push(crate::args::TREE_NONFLAGS_GROUP_START.into());
                                        argv.extend_from_slice(slice);
                                        argv.push(crate::args::TREE_NONFLAGS_END.into());
                                    } else {
                                        let mut collapsed = Vec::with_capacity(1);
                                        crate::args::collapse_flag_per_line_group(
                                            slice.to_vec(),
                                            &mut collapsed,
                                        );
                                        for line in collapsed {
                                            if line.starts_with("--") {
                                                let ns = raw_flag_args.len();
                                                raw_flag_args.push(line.clone());
                                                spec_groups.push((ns, ns + 1));
                                                static_flags.push(line);
                                            } else {
                                                argv.push(line);
                                            }
                                        }
                                    }
                                };
                                for &(start, end) in &pf.flag_groups {
                                    if end <= lead || start >= a.args.len() {
                                        continue;
                                    }
                                    let s = start.max(lead);
                                    let e = end.min(a.args.len());
                                    while cursor < s {
                                        push_slice(cursor, cursor + 1);
                                        cursor += 1;
                                    }
                                    push_slice(s, e);
                                    cursor = e;
                                }
                                while cursor < a.args.len() {
                                    push_slice(cursor, cursor + 1);
                                    cursor += 1;
                                }
                                let body = render_param_file(&static_flags, pf.format).into_bytes();
                                params.push((
                                    i,
                                    path,
                                    body,
                                    crate::ParamFileWriteSpec {
                                        args: raw_flag_args,
                                        format: pf.format,
                                        flag_groups: spec_groups,
                                        flags_only: true,
                                    },
                                ));
                            } else {
                                let tail = &a.args[lead..];
                                let mut raw_flag_args = Vec::new();
                                let mut static_flags = Vec::new();
                                let mut idx = 0usize;
                                while idx < tail.len() {
                                    let item = &tail[idx];
                                    if item.starts_with(crate::args::TREE_HEAD_MARKER)
                                        || item.starts_with(crate::TREE_MARKER)
                                    {
                                        let seq_start = idx;
                                        if item.starts_with(crate::args::TREE_HEAD_MARKER) {
                                            idx += 1;
                                        }
                                        while idx < tail.len()
                                            && tail[idx].starts_with(crate::TREE_MARKER)
                                        {
                                            idx += 1;
                                        }
                                        if idx < tail.len()
                                            && tail[idx].starts_with(crate::args::TREE_TAIL_MARKER)
                                        {
                                            idx += 1;
                                        }
                                        let seq = &tail[seq_start..idx];
                                        raw_flag_args.extend_from_slice(seq);
                                        argv.push(crate::args::TREE_NONFLAGS_START.into());
                                        argv.extend_from_slice(seq);
                                        argv.push(crate::args::TREE_NONFLAGS_END.into());
                                    } else if let Some(rest) =
                                        item.strip_prefix(crate::args::TREE_TAIL_MARKER)
                                    {
                                        if rest.starts_with("--") {
                                            raw_flag_args.push(rest.to_string());
                                            static_flags.push(rest.to_string());
                                        } else {
                                            argv.push(rest.to_string());
                                        }
                                        idx += 1;
                                    } else {
                                        if item.starts_with("--") {
                                            raw_flag_args.push(item.clone());
                                            static_flags.push(item.clone());
                                        } else {
                                            argv.push(item.clone());
                                        }
                                        idx += 1;
                                    }
                                }
                                let body = render_param_file(&static_flags, pf.format).into_bytes();
                                params.push((
                                    i,
                                    path,
                                    body,
                                    crate::ParamFileWriteSpec {
                                        args: raw_flag_args,
                                        format: pf.format,
                                        flag_groups: Vec::new(),
                                        flags_only: true,
                                    },
                                ));
                            }
                        } else {
                            let raw_spilled = a.args[lead..].to_vec();
                            let body = render_param_file(&grouped, pf.format).into_bytes();
                            params.push((
                                i,
                                path,
                                body,
                                crate::ParamFileWriteSpec {
                                    args: raw_spilled,
                                    format: pf.format,
                                    flag_groups: Vec::new(),
                                    flags_only: false,
                                },
                            ));
                        }
                    } else {
                        argv.extend(a.args.iter().cloned());
                    }
                }
                None => argv.extend(a.args.iter().cloned()),
            }
        }
        (argv, params)
    }

    /// `CommandLines.allArguments`: the executable and every argument, *including* the ones that
    /// go into param files (what `aquery` reports as the command line).
    pub fn all_arguments(&self) -> Vec<String> {
        let ActionKind::Spawn {
            executable,
            arguments,
            ..
        } = &self.kind
        else {
            return Vec::new();
        };
        let mut argv = vec![executable.clone()];
        for a in arguments {
            argv.extend(a.args.iter().cloned());
        }
        argv
    }

    /// Lowers to the flat model consumed by the existing executor. Param files become extra
    /// `FileWriteAction`s listed *before* the action that uses them (and as its inputs).
    pub fn lower(&self) -> Vec<ActionAnalysisMetadata> {
        let mut out = Vec::new();
        if self.is_pseudo {
            return out;
        }
        match &self.kind {
            ActionKind::Spawn { .. } => {
                let (argv, params) = self.command_line_with_specs();
                let mut inputs = self.inputs.clone();
                let tree_inputs: Vec<Artifact> = self
                    .inputs
                    .iter()
                    .filter(|a| a.is_tree_artifact)
                    .cloned()
                    .collect();
                let mut java_compile = self.java_compile.clone();
                for (arg_idx, path, body, spec) in params {
                    // The classpath param file of a reduced-classpath Java compile is written by
                    // the executor (its content depends on `.jdeps` files read at execution time).
                    if let Some(jc) = java_compile.as_mut() {
                        if arg_idx == jc.classpath_arg_index {
                            jc.classpath_params_path = path;
                            continue;
                        }
                    }
                    let art = Artifact {
                        is_source: false,
                        is_tree_artifact: false,
                        root: crate::ArtifactRoot::Bin,
                        is_unresolved_symlink: false,
                        is_constant_metadata: false,
                        exec_path: PathFragment::create(&path),
                        root_relative_path: PathFragment::create(
                            path.strip_prefix(&format!(
                                "{}/",
                                self.outputs
                                    .first()
                                    .map(|o| o.root_path())
                                    .unwrap_or_default()
                            ))
                            .unwrap_or(&path),
                        ),
                        owner_label: self.owner.clone(),
                    };
                    let param_inputs = if spec.has_tree_artifacts() {
                        tree_inputs.clone()
                    } else {
                        Vec::new()
                    };
                    out.push(ActionAnalysisMetadata::FileWriteAction {
                        owner: self.owner.clone(),
                        output: art.clone(),
                        content: body,
                        is_executable: false,
                        inputs: param_inputs,
                        param_file_spec: Some(spec),
                    });
                    inputs.push(art);
                }
                let mut execution_info = self.execution_requirements.clone();
                for (k, v) in &self.exec_properties {
                    execution_info
                        .entry(format!("exec-property:{k}"))
                        .or_insert_with(|| v.clone());
                }
                out.push(ActionAnalysisMetadata::SpawnAction {
                    owner: self.owner.clone(),
                    mnemonic: self.mnemonic.clone(),
                    progress_message: self.progress_message.clone().unwrap_or_else(|| {
                        if self.mnemonic == "Genrule" {
                            format!("Executing genrule {}", self.owner)
                        } else {
                            format!("Running {} for {}", self.mnemonic, self.owner)
                        }
                    }),
                    argv,
                    env: self
                        .action_env
                        .as_ref()
                        .map(|e| e.fixed_env().clone())
                        .unwrap_or_else(|| self.env.clone()),
                    inherited_env: self
                        .action_env
                        .as_ref()
                        .map(|e| e.inherited_env().clone())
                        .unwrap_or_default(),
                    unset_env: self
                        .action_env
                        .as_ref()
                        .map(|e| e.unset_env().clone())
                        .unwrap_or_default(),
                    inputs,
                    outputs: self.outputs.clone(),
                    execution_info,
                    tools: self.tools.clone(),
                    java_compile: java_compile.map(Box::new),
                });
            }
            ActionKind::FileWrite {
                content,
                is_executable,
            } => {
                if let Some(output) = self.outputs.first() {
                    let param_file_spec = match content {
                        FileContent::Args {
                            args,
                            format,
                            flag_groups,
                        } => Some(crate::ParamFileWriteSpec {
                            args: args.clone(),
                            format: *format,
                            flag_groups: flag_groups.clone(),
                            flags_only: false,
                        }),
                        FileContent::Literal(_) | FileContent::Bytes(_) => None,
                    };
                    out.push(ActionAnalysisMetadata::FileWriteAction {
                        owner: self.owner.clone(),
                        output: output.clone(),
                        content: content.render(),
                        is_executable: *is_executable,
                        inputs: self.inputs.clone(),
                        param_file_spec,
                    });
                }
            }
            ActionKind::Symlink {
                target_file,
                target_path,
                is_executable,
            } => {
                if let Some(output) = self.outputs.first() {
                    let input_path = match (target_file, target_path) {
                        (Some(f), _) => f.exec_path.clone(),
                        (None, Some(p)) => PathFragment::create(p),
                        (None, None) => PathFragment::empty(),
                    };
                    out.push(ActionAnalysisMetadata::SymlinkAction {
                        owner: self.owner.clone(),
                        input_path,
                        output: output.clone(),
                        is_executable: *is_executable,
                    });
                }
            }
            ActionKind::TemplateExpand {
                template,
                substitutions,
                is_executable,
                computed_substitutions,
            } => {
                if let Some(output) = self.outputs.first() {
                    let mut subs: Vec<(String, String)> = substitutions.clone();
                    subs.extend(computed_substitutions.iter().cloned());
                    out.push(ActionAnalysisMetadata::TemplateExpansionAction {
                        owner: self.owner.clone(),
                        template: template.clone(),
                        output: output.clone(),
                        substitutions: subs,
                        is_executable: *is_executable,
                    });
                }
            }
            ActionKind::BuildInfoTranslate {
                template,
                status_file,
                is_volatile,
                translator,
            } => {
                if let Some(output) = self.outputs.first() {
                    out.push(ActionAnalysisMetadata::BuildInfoTranslateAction {
                        owner: self.owner.clone(),
                        template: template.clone(),
                        status_file: status_file.clone(),
                        output: output.clone(),
                        is_volatile: *is_volatile,
                        translator: translator.clone(),
                    });
                }
            }
        }
        out
    }
}

fn same_artifacts_without_owner(a: &[Artifact], b: &[Artifact]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.equals_without_owner(y))
}

/// `Actions.escapedPath`: escapes `_`, `/`, `\`, `:`, `@` for use as a filename.
pub fn escaped_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            '_' => out.push_str("_U"),
            '/' => out.push_str("_S"),
            '\\' => out.push_str("_B"),
            ':' => out.push_str("_C"),
            '@' => out.push_str("_A"),
            _ => out.push(c),
        }
    }
    out
}

/// `Actions.isRunfilesArtifactPair`: exempts a `.runfiles` tree paired with `<tree>/MANIFEST`.
pub fn is_runfiles_artifact_pair(outer: &Artifact, inner: &Artifact) -> bool {
    outer.is_runfiles_tree()
        && inner.exec_path.path_string() == format!("{}/MANIFEST", outer.exec_path.path_string())
}

/// Error from [`ActionGraph::register`] (`ActionConflictException`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionConflict {
    pub artifact: String,
    pub is_prefix_conflict: bool,
    pub message: String,
}

impl std::fmt::Display for ActionConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ActionConflict {}

/// Map of output exec path → generating action; detects conflicting and shadowed actions.
#[derive(Debug, Default)]
pub struct ActionGraph {
    generating: BTreeMap<String, Action>,
    artifacts: BTreeMap<String, Artifact>,
}

impl ActionGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn size(&self) -> usize {
        self.generating.len()
    }

    pub fn actions(&self) -> impl Iterator<Item = &Action> {
        self.generating.values()
    }

    pub fn generating_action(&self, exec_path: &str) -> Option<&Action> {
        self.generating.get(exec_path)
    }

    /// `ActionGraph.getGeneratingAction(Artifact)`: returns `None` for source artifacts.
    pub fn generating_action_by_artifact(&self, artifact: &Artifact) -> Option<&Action> {
        if artifact.is_source {
            return None;
        }
        self.generating.get(artifact.exec_path.path_string())
    }

    /// Registers `action`, mirroring `MapBasedActionGraph.registerAction` plus the
    /// `ArtifactPrefixConflict` check and `Actions.canBeShared` sharing rule.
    pub fn register(&mut self, action: &Action) -> Result<(), ActionConflict> {
        for (idx, out) in action.outputs.iter().enumerate() {
            if out.is_source {
                return Err(ActionConflict {
                    artifact: out.exec_path.path_string().to_string(),
                    is_prefix_conflict: false,
                    message: format!("Source in outputs: {}", out.exec_path),
                });
            }
            let p = out.exec_path.path_string().to_string();
            if let Some(prev) = self.generating.get(&p) {
                if prev.can_be_shared(action) {
                    continue;
                }
                return Err(ActionConflict {
                    artifact: p.clone(),
                    is_prefix_conflict: false,
                    message: format!(
                        "file '{}' is generated by these conflicting actions:\n{}",
                        p,
                        debug_suffix(prev, action)
                    ),
                });
            }
            // Check intra-action prefix conflicts among this action's own outputs.
            for other_out in action.outputs.iter().skip(idx + 1) {
                let q = other_out.exec_path.path_string().to_string();
                let (short, long, short_a, long_a) = if p.len() < q.len() {
                    (&p, &q, out, other_out)
                } else {
                    (&q, &p, other_out, out)
                };
                if long.len() > short.len()
                    && long.starts_with(short.as_str())
                    && long.as_bytes()[short.len()] == b'/'
                    && !is_runfiles_artifact_pair(short_a, long_a)
                {
                    return Err(ActionConflict {
                        artifact: short.clone(),
                        is_prefix_conflict: true,
                        message: format!(
                            "One of the output paths '{}' (belonging to {}) and '{}' (belonging to {}) is a prefix of the other. These actions cannot be simultaneously present; please rename one of the output files or build just one of them",
                            short, action.owner, long, action.owner
                        ),
                    });
                }
            }
            // Prefix conflict against outputs already in the graph (exempting runfiles-manifest).
            for (other, prev) in &self.generating {
                let (short, long, first_a, second_a, short_art, long_art) = if p.len() < other.len()
                {
                    (
                        &p,
                        other,
                        action,
                        prev,
                        Some(out),
                        self.artifacts.get(other),
                    )
                } else {
                    (
                        other,
                        &p,
                        prev,
                        action,
                        self.artifacts.get(other),
                        Some(out),
                    )
                };
                if long.len() > short.len()
                    && long.starts_with(short.as_str())
                    && long.as_bytes()[short.len()] == b'/'
                {
                    if let (Some(sa), Some(la)) = (short_art, long_art) {
                        if is_runfiles_artifact_pair(sa, la) {
                            continue;
                        }
                    }
                    return Err(ActionConflict {
                        artifact: p.clone(),
                        is_prefix_conflict: true,
                        message: format!(
                            "One of the output paths '{}' (belonging to {}) and '{}' (belonging to {}) is a prefix of the other. These actions cannot be simultaneously present; please rename one of the output files or build just one of them",
                            short, first_a.owner, long, second_a.owner
                        ),
                    });
                }
            }
        }
        for out in &action.outputs {
            let key = out.exec_path.path_string().to_string();
            self.generating
                .entry(key.clone())
                .or_insert_with(|| action.clone());
            self.artifacts.entry(key).or_insert_with(|| out.clone());
        }
        Ok(())
    }

    /// `Actions.findArtifactPrefixConflicts`: sorts all registered outputs using
    /// `PathFragment::hierarchical_cmp`, reports outer strict-prefix pairs (skipping inner
    /// pairs when `x << y << z`), and exempts `<tree>.runfiles` + `<tree>.runfiles/MANIFEST`.
    pub fn find_artifact_prefix_conflicts(&self) -> Vec<ActionConflict> {
        let mut arts: Vec<&Artifact> = self.artifacts.values().collect();
        arts.sort_by(|a, b| a.exec_path.hierarchical_cmp(&b.exec_path));
        let mut conflicts = Vec::new();
        let mut idx = 0usize;
        while idx + 1 < arts.len() {
            let art_i = arts[idx];
            let mut j = idx + 1;
            let mut advanced = false;
            while j < arts.len() {
                let art_j = arts[j];
                if art_j.exec_path.path_string().len() > art_i.exec_path.path_string().len()
                    && art_j.exec_path.starts_with(&art_i.exec_path)
                {
                    if !is_runfiles_artifact_pair(art_i, art_j) {
                        let a_i = &self.generating[art_i.exec_path.path_string()];
                        let a_j = &self.generating[art_j.exec_path.path_string()];
                        conflicts.push(ActionConflict {
                            artifact: art_i.exec_path.path_string().to_string(),
                            is_prefix_conflict: true,
                            message: format!(
                                "One of the output paths '{}' (belonging to {}) and '{}' (belonging to {}) is a prefix of the other. These actions cannot be simultaneously present; please rename one of the output files or build just one of them",
                                art_i.exec_path, a_i.owner, art_j.exec_path, a_j.owner
                            ),
                        });
                    }
                    j += 1;
                    advanced = true;
                } else {
                    break;
                }
            }
            idx = if advanced { j } else { idx + 1 };
        }
        conflicts
    }
}

fn detail(sb: &mut String, key: &str, a: &str, b: &str) {
    sb.push_str(key);
    sb.push_str(": ");
    sb.push_str(a);
    if a != b {
        sb.push_str(", ");
        sb.push_str(b);
    }
    sb.push('\n');
}

fn non_empty_or_null(s: &str) -> &str {
    if s.is_empty() {
        "(null)"
    } else {
        s
    }
}

fn diff_artifact_lists(sb: &mut String, label: &str, a: &[Artifact], b: &[Artifact]) {
    let mut diffs = Vec::new();
    let max_len = a.len().max(b.len());
    for i in 0..max_len {
        let left = a.get(i);
        let right = b.get(i);
        let equal = match (left, right) {
            (Some(l), Some(r)) => l.equals_without_owner(r),
            _ => false,
        };
        if !equal {
            diffs.push(format!(
                "  {}: {} vs {}",
                i,
                left.map(|x| x.exec_path.path_string())
                    .unwrap_or("(absent)"),
                right
                    .map(|x| x.exec_path.path_string())
                    .unwrap_or("(absent)"),
            ));
            if diffs.len() >= 5 {
                break;
            }
        }
    }
    if !diffs.is_empty() {
        sb.push_str(label);
        sb.push_str(" differ:\n");
        for d in diffs {
            sb.push_str(&d);
            sb.push('\n');
        }
    }
}

/// `ActionConflictException.debugSuffix` (matching Bazel's conflict diagnostics).
fn debug_suffix(a: &Action, b: &Action) -> String {
    let mut sb = String::new();
    detail(
        &mut sb,
        "Label",
        &a.owner.canonical_form(),
        &b.owner.canonical_form(),
    );
    detail(
        &mut sb,
        "RuleClass",
        non_empty_or_null(&a.owner_kind),
        non_empty_or_null(&b.owner_kind),
    );
    detail(
        &mut sb,
        "Configuration",
        non_empty_or_null(&a.owner_config),
        non_empty_or_null(&b.owner_config),
    );
    detail(&mut sb, "Mnemonic", &a.mnemonic, &b.mnemonic);
    detail(&mut sb, "Action key", &a.key(), &b.key());
    detail(
        &mut sb,
        "Progress message",
        a.progress_message.as_deref().unwrap_or("(null)"),
        b.progress_message.as_deref().unwrap_or("(null)"),
    );
    let pi = |x: &Action| {
        x.primary_input()
            .map(|i| i.to_string())
            .unwrap_or("(null)".into())
    };
    let pi_a = pi(a);
    let pi_b = pi(b);
    detail(&mut sb, "PrimaryInput", &pi_a, &pi_b);
    let po = |x: &Action| {
        x.primary_output()
            .map(|i| i.to_string())
            .unwrap_or("(null)".into())
    };
    detail(&mut sb, "PrimaryOutput", &po(a), &po(b));
    if pi_a == pi_b {
        diff_artifact_lists(&mut sb, "MandatoryInputs", &a.inputs, &b.inputs);
        diff_artifact_lists(&mut sb, "Outputs", &a.outputs, &b.outputs);
    }
    sb
}

#[cfg(test)]
mod tests {
    use super::*;

    fn art(p: &str) -> Artifact {
        Artifact::derived(
            "k8-fastbuild",
            PathFragment::create(p),
            Label::parse_canonical("//a:b").unwrap(),
        )
    }

    fn write(owner: &str, out: &str, text: &str) -> Action {
        let mut a = Action::new(
            Label::parse_canonical(owner).unwrap(),
            "FileWrite",
            ActionKind::FileWrite {
                content: FileContent::Literal(text.into()),
                is_executable: false,
            },
        );
        a.outputs.push(art(out));
        a
    }

    #[test]
    fn identical_actions_are_shared_different_conflict() {
        let mut g = ActionGraph::new();
        g.register(&write("//a:b", "a/x", "1")).unwrap();
        g.register(&write("//a:c", "a/x", "1")).unwrap();
        let e = g.register(&write("//a:c", "a/x", "2")).unwrap_err();
        assert!(e.message.starts_with(
            "file 'bazel-out/k8-fastbuild/bin/a/x' is generated by these conflicting actions:\nLabel: //a:b, //a:c\n"
        ), "{}", e.message);
        assert!(e.message.contains("Mnemonic: FileWrite\n"));
        assert!(!e.is_prefix_conflict);
    }

    #[test]
    fn action_keys_include_param_format_exec_props_and_symlink_type() {
        let mut s1 = Action::new(
            Label::parse_canonical("//a:s").unwrap(),
            "Tool",
            ActionKind::Spawn {
                executable: "/bin/true".into(),
                arguments: vec![CommandLineArgs {
                    args: vec!["--flag".into()],
                    param_file: Some(ParamFileInfo {
                        flag_format: "100%%-%s".into(),
                        format: ParamFileFormat::Shell,
                        use_always: true,
                        leading_args: 0,
                        flag_groups: Vec::new(),
                        explicit_path: None,
                    }),
                }],
                shell_command: None,
            },
        );
        s1.outputs.push(art("a/out"));
        let mut s2 = s1.clone();
        if let ActionKind::Spawn { arguments, .. } = &mut s2.kind {
            arguments[0].param_file.as_mut().unwrap().format = ParamFileFormat::Multiline;
        }
        assert_ne!(s1.key(), s2.key());
        let (argv, _) = s1.command_line();
        assert_eq!(argv[1], "100%-bazel-out/k8-fastbuild/bin/a/out-0.params");

        let mut sym1 = Action::new(
            Label::parse_canonical("//a:l").unwrap(),
            "Symlink",
            ActionKind::Symlink {
                target_file: Some(art("a/in")),
                target_path: None,
                is_executable: false,
            },
        );
        sym1.symlink_target_type = SymlinkTargetType::File;
        let mut sym2 = sym1.clone();
        sym2.use_exec_root_for_source = true;
        assert_ne!(sym1.key(), sym2.key());

        let mut pseudo = Action::new(
            Label::parse_canonical("//a:p").unwrap(),
            "Nothing",
            ActionKind::Spawn {
                executable: String::new(),
                arguments: vec![],
                shell_command: None,
            },
        );
        pseudo.is_pseudo = true;
        assert!(pseudo.lower().is_empty());
    }

    #[test]
    fn prefix_conflict() {
        let mut g = ActionGraph::new();
        g.register(&write("//a:b", "a/x", "1")).unwrap();
        let e = g.register(&write("//a:c", "a/x/y", "1")).unwrap_err();
        assert!(e.is_prefix_conflict);
        assert!(e.message.contains("is a prefix of the other"));
    }

    #[test]
    fn param_file_used_when_always() {
        let mut a = Action::new(
            Label::parse_canonical("//a:b").unwrap(),
            "Compile",
            ActionKind::Spawn {
                executable: "/bin/tool".into(),
                arguments: vec![CommandLineArgs {
                    args: vec!["--x".into(), "a b".into()],
                    param_file: Some(ParamFileInfo {
                        use_always: true,
                        leading_args: 0,
                        ..Default::default()
                    }),
                }],
                shell_command: None,
            },
        );
        a.outputs.push(art("a/out"));
        let (argv, params) = a.command_line();
        assert_eq!(argv[0], "/bin/tool");
        assert_eq!(argv[1], "@bazel-out/k8-fastbuild/bin/a/out-0.params");
        assert_eq!(
            String::from_utf8(params[0].1.clone()).unwrap(),
            "--x\n'a b'\n"
        );
        let lowered = a.lower();
        assert_eq!(lowered.len(), 2);
    }

    #[test]
    fn escaped_path_matches_bazel_actions() {
        assert_eq!(escaped_path("a/b_c:d\\e@f"), "a_Sb_Uc_Cd_Be_Af");
    }

    #[test]
    fn sharing_checks_artifact_root_and_key_info() {
        let a1 = write("//a:b", "a/x", "1");
        let mut a2 = Action::new(
            Label::parse_canonical("//a:c").unwrap(),
            "FileWrite",
            ActionKind::FileWrite {
                content: FileContent::Literal("1".into()),
                is_executable: false,
            },
        );
        a2.outputs.push(Artifact::derived_in_root(
            "k8-fastbuild",
            crate::ArtifactRoot::Genfiles,
            PathFragment::create("a/x"),
            Label::parse_canonical("//a:c").unwrap(),
        ));
        assert!(
            !a1.can_be_shared(&a2),
            "different ArtifactRoot must not share"
        );

        let mut t1 = write("//a:b", "a/y", "1");
        let mut t2 = write("//a:c", "a/y", "1");
        t1.key_info = Some(std::sync::Arc::new(ActionKeyInfo::TestRunner(Box::new(
            TestRunnerKey {
                test_filter: "A".into(),
                ..Default::default()
            },
        ))));
        t2.key_info = Some(std::sync::Arc::new(ActionKeyInfo::TestRunner(Box::new(
            TestRunnerKey {
                test_filter: "B".into(),
                ..Default::default()
            },
        ))));
        assert_ne!(t1.key(), t2.key());
        assert!(!t1.can_be_shared(&t2));
    }

    #[test]
    fn runfiles_manifest_exempted_from_prefix_conflict_and_intra_action_prefix_caught() {
        let mut g = ActionGraph::new();
        let mut tree_act = Action::new(
            Label::parse_canonical("//a:b").unwrap(),
            "RunfilesTree",
            ActionKind::FileWrite {
                content: FileContent::Literal(String::new()),
                is_executable: false,
            },
        );
        let mut tree_art = art("a/bin.runfiles");
        tree_art.is_tree_artifact = true;
        tree_act.outputs.push(tree_art);
        g.register(&tree_act).unwrap();

        let manifest_act = write("//a:b", "a/bin.runfiles/MANIFEST", "m");
        g.register(&manifest_act).unwrap();
        assert!(g.find_artifact_prefix_conflicts().is_empty());

        // Intra-action prefix conflict
        let mut bad = write("//a:bad", "a/dir", "1");
        bad.outputs.push(art("a/dir/file"));
        let err = g.register(&bad).unwrap_err();
        assert!(err.is_prefix_conflict);
    }

    #[test]
    fn source_output_rejected_and_null_owner_fields_formatted() {
        let mut g = ActionGraph::new();
        let mut src_out = Action::new(
            Label::parse_canonical("//a:b").unwrap(),
            "FileWrite",
            ActionKind::FileWrite {
                content: FileContent::Literal("x".into()),
                is_executable: false,
            },
        );
        src_out.outputs.push(Artifact::source(
            PathFragment::create("a/src.txt"),
            Label::parse_canonical("//a:b").unwrap(),
        ));
        assert!(g.register(&src_out).is_err());

        g.register(&write("//a:b", "a/z", "1")).unwrap();
        let err = g.register(&write("//a:c", "a/z", "2")).unwrap_err();
        assert!(
            err.message.contains("RuleClass: (null)\n"),
            "{}",
            err.message
        );
        assert!(
            err.message.contains("Configuration: (null)\n"),
            "{}",
            err.message
        );
    }

    #[test]
    fn gcc_quoted_windows_flag_per_line_and_spilled_index_numbering() {
        let gcc = render_param_file(&["".into(), "a b'c".into()], ParamFileFormat::GccQuoted);
        assert_eq!(gcc, "''\na\\ b\\'c\n");
        let win = render_param_file(
            &["plain".into(), "a \"b\"\\".into()],
            ParamFileFormat::Windows,
        );
        assert_eq!(win, "plain\n\"a \\\"b\\\"\\\\\"\n");

        let mut act = Action::new(
            Label::parse_canonical("//a:b").unwrap(),
            "PackageTar",
            ActionKind::Spawn {
                executable: "/bin/tar_tool".into(),
                arguments: vec![
                    CommandLineArgs {
                        args: vec!["--inline-first".into()],
                        param_file: None,
                    },
                    CommandLineArgs {
                        args: vec![
                            "--output".into(),
                            "out.tar".into(),
                            "pos_input".into(),
                            "--mode".into(),
                            "0644".into(),
                        ],
                        param_file: Some(ParamFileInfo {
                            flag_format: "@%s".into(),
                            format: ParamFileFormat::FlagPerLine,
                            use_always: true,
                            leading_args: 0,
                            flag_groups: vec![(0, 2), (2, 3), (3, 5)],
                            explicit_path: None,
                        }),
                    },
                ],
                shell_command: None,
            },
        );
        act.outputs.push(art("a/out.tar"));
        let (argv, params) = act.command_line();
        assert_eq!(
            argv,
            vec![
                "/bin/tar_tool",
                "--inline-first",
                "@bazel-out/k8-fastbuild/bin/a/out.tar-0.params",
                "pos_input",
            ]
        );
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].0, "bazel-out/k8-fastbuild/bin/a/out.tar-0.params");
        assert_eq!(
            String::from_utf8(params[0].1.clone()).unwrap(),
            "--output=out.tar\n--mode=0644\n"
        );

        let mut tmpl = Action::new(
            Label::parse_canonical("//a:t").unwrap(),
            "TemplateExpand",
            ActionKind::TemplateExpand {
                template: art("a/in.tpl"),
                substitutions: vec![("{Z}".into(), "{A}".into()), ("{A}".into(), "done".into())],
                is_executable: false,
                computed_substitutions: vec![("{C}".into(), "last".into())],
            },
        );
        tmpl.outputs.push(art("a/out.txt"));
        let lowered = tmpl.lower();
        match &lowered[0] {
            ActionAnalysisMetadata::TemplateExpansionAction { substitutions, .. } => {
                assert_eq!(
                    substitutions,
                    &vec![
                        ("{Z}".into(), "{A}".into()),
                        ("{A}".into(), "done".into()),
                        ("{C}".into(), "last".into()),
                    ]
                );
            }
            other => panic!("unexpected lowered action: {other:?}"),
        }
    }

    #[test]
    fn flag_per_line_vs_multiline_changes_action_key_and_prevents_false_sharing() {
        let mk_spawn = |owner: &str, format: ParamFileFormat, groups: Vec<(usize, usize)>| {
            let mut act = Action::new(
                Label::parse_canonical(owner).unwrap(),
                "PackageTar",
                ActionKind::Spawn {
                    executable: "/bin/tar_tool".into(),
                    arguments: vec![CommandLineArgs {
                        args: vec!["--output".into(), "out.tar".into(), "pos_input".into()],
                        param_file: Some(ParamFileInfo {
                            flag_format: "@%s".into(),
                            format,
                            use_always: true,
                            leading_args: 0,
                            flag_groups: groups,
                            explicit_path: None,
                        }),
                    }],
                    shell_command: None,
                },
            );
            act.outputs.push(art("a/shared.tar"));
            act
        };

        let multi = mk_spawn("//a:m", ParamFileFormat::Multiline, vec![(0, 2), (2, 3)]);
        let flag_grouped = mk_spawn("//a:f", ParamFileFormat::FlagPerLine, vec![(0, 2), (2, 3)]);
        let flag_ungrouped = mk_spawn(
            "//a:u",
            ParamFileFormat::FlagPerLine,
            vec![(0, 1), (1, 2), (2, 3)],
        );

        assert_ne!(multi.key(), flag_grouped.key());
        assert_ne!(multi.key(), flag_ungrouped.key());
        assert_ne!(flag_grouped.key(), flag_ungrouped.key());
        assert!(!multi.can_be_shared(&flag_grouped));
        assert!(!multi.can_be_shared(&flag_ungrouped));

        let mut g = ActionGraph::new();
        g.register(&multi).unwrap();
        assert!(g.register(&flag_grouped).is_err());
    }

    #[test]
    fn spawn_action_lowering_propagates_exec_properties() {
        let mut act = Action::new(
            Label::parse_canonical("//a:b").unwrap(),
            "Spawn",
            ActionKind::Spawn {
                executable: "tool".to_string(),
                arguments: Vec::new(),
                shell_command: None,
            },
        );
        act.outputs.push(art("a/out"));
        act.exec_properties
            .insert("OSFamily".to_string(), "linux".to_string());
        act.execution_requirements
            .insert("no-sandbox".to_string(), String::new());
        let lowered = act.lower();
        let Some(ActionAnalysisMetadata::SpawnAction { execution_info, .. }) = lowered.first()
        else {
            panic!("expected SpawnAction");
        };
        assert_eq!(
            execution_info
                .get("exec-property:OSFamily")
                .map(String::as_str),
            Some("linux")
        );
        assert!(execution_info.contains_key("no-sandbox"));
    }

    #[test]
    fn param_file_flag_per_line_groups_distinguish_action_keys_and_prevent_sharing() {
        let make_spawn = |groups: Vec<(usize, usize)>| {
            let mut a = Action::new(
                Label::parse_canonical("//a:b").unwrap(),
                "Compile",
                ActionKind::Spawn {
                    executable: "/bin/tool".into(),
                    arguments: vec![CommandLineArgs {
                        args: vec!["--flag".into(), "v".into()],
                        param_file: Some(ParamFileInfo {
                            flag_format: "@%s".into(),
                            use_always: true,
                            format: ParamFileFormat::FlagPerLine,
                            leading_args: 0,
                            flag_groups: groups,
                            explicit_path: None,
                        }),
                    }],
                    shell_command: None,
                },
            );
            a.outputs.push(art("a/out"));
            a
        };

        // `args.add("--flag", "v")` -> `--flag=v` in param file.
        let paired = make_spawn(vec![(0, 2)]);
        // `args.add("--flag"); args.add("v")` -> `--flag` in param file, `v` on argv.
        let split = make_spawn(vec![(0, 1), (1, 2)]);

        let (argv_paired, params_paired) = paired.command_line();
        let (argv_split, params_split) = split.command_line();
        assert_eq!(params_paired[0].1, b"--flag=v\n");
        assert_eq!(params_split[0].1, b"--flag\n");
        assert_ne!(argv_paired, argv_split);

        assert_ne!(paired.key(), split.key());
        assert!(!paired.can_be_shared(&split));

        let lowered_paired = paired.lower();
        let lowered_split = split.lower();
        assert_ne!(
            lowered_paired[0].action_key(),
            lowered_split[0].action_key()
        );
    }

    #[test]
    fn flag_only_param_files_partition_tree_artifacts_without_dropping_values() {
        let mut tree_before = art("a/tree_before");
        tree_before.is_tree_artifact = true;
        let mut tree_fmt = art("a/tree_fmt");
        tree_fmt.is_tree_artifact = true;
        let mut tree_pos = art("a/tree_pos");
        tree_pos.is_tree_artifact = true;
        let mut tree_short = art("a/tree_short");
        tree_short.is_tree_artifact = true;

        let list_dir = |dir: &str| -> Result<Vec<String>, String> {
            match dir {
                "bazel-out/k8-fastbuild/bin/a/tree_before" => Ok(vec![
                    "bazel-out/k8-fastbuild/bin/a/tree_before/one.txt".into(),
                    "bazel-out/k8-fastbuild/bin/a/tree_before/two.txt".into(),
                ]),
                "bazel-out/k8-fastbuild/bin/a/tree_fmt" => Ok(vec![
                    "bazel-out/k8-fastbuild/bin/a/tree_fmt/alpha.txt".into(),
                    "bazel-out/k8-fastbuild/bin/a/tree_fmt/beta.txt".into(),
                ]),
                "bazel-out/k8-fastbuild/bin/a/tree_pos" => Ok(vec![
                    "bazel-out/k8-fastbuild/bin/a/tree_pos/first.txt".into(),
                    "bazel-out/k8-fastbuild/bin/a/tree_pos/second.txt".into(),
                ]),
                "bazel-out/k8-fastbuild/bin/a/tree_short" => {
                    Ok(vec!["bazel-out/k8-fastbuild/bin/a/tree_short/inc.h".into()])
                }
                other => Err(format!("unexpected tree dir: {other}")),
            }
        };

        // 1. Ungrouped `flagsOnly` (`CommandLines.java:130-141` + `ParameterFile.java:103-124`):
        //    `--` tokens go to the param file and non-`--` tokens stay on `argv` in order.
        let ungrouped_args = vec![
            "--static=ok".to_string(),
            "leading_pos".to_string(),
            format!(
                "{}\u{1}--input\u{1}{}",
                crate::TREE_MARKER,
                tree_before.exec_path.path_string()
            ),
            format!(
                "{}--input=%s\u{1}\u{1}{}",
                crate::TREE_MARKER,
                tree_fmt.exec_path.path_string()
            ),
            format!(
                "{}\u{1}\u{1}{}",
                crate::TREE_MARKER,
                tree_pos.exec_path.path_string()
            ),
            format!(
                "{}\u{1}-I\u{1}{}",
                crate::TREE_MARKER,
                tree_short.exec_path.path_string()
            ),
            "-single".to_string(),
        ];
        let mut ungrouped = Action::new(
            Label::parse_canonical("//a:ungrouped").unwrap(),
            "RunTool",
            ActionKind::Spawn {
                executable: "/bin/tool".into(),
                arguments: vec![CommandLineArgs {
                    args: ungrouped_args,
                    param_file: Some(ParamFileInfo {
                        flag_format: "@%s".into(),
                        use_always: true,
                        format: ParamFileFormat::FlagPerLine,
                        leading_args: 0,
                        flag_groups: Vec::new(),
                        explicit_path: None,
                    }),
                }],
                shell_command: None,
            },
        );
        ungrouped.inputs = vec![
            tree_before.clone(),
            tree_fmt.clone(),
            tree_pos.clone(),
            tree_short.clone(),
        ];
        ungrouped.outputs.push(art("a/ungrouped.out"));
        let lowered_ungrouped = ungrouped.lower();
        let crate::ActionAnalysisMetadata::FileWriteAction {
            param_file_spec: Some(ref ungrouped_spec),
            ..
        } = lowered_ungrouped[0]
        else {
            panic!("expected FileWriteAction with param_file_spec");
        };
        let crate::ActionAnalysisMetadata::SpawnAction {
            argv: ref ungrouped_argv,
            ..
        } = lowered_ungrouped[1]
        else {
            panic!("expected SpawnAction");
        };
        let ungrouped_params =
            String::from_utf8(ungrouped_spec.render_expanded(&list_dir).unwrap()).unwrap();
        let ungrouped_expanded_argv =
            crate::expand_tree_artifact_args(ungrouped_argv, &list_dir).unwrap();
        assert_eq!(
            ungrouped_params,
            concat!(
                "--static=ok\n",
                "--input\n",
                "--input\n",
                "--input=bazel-out/k8-fastbuild/bin/a/tree_fmt/alpha.txt\n",
                "--input=bazel-out/k8-fastbuild/bin/a/tree_fmt/beta.txt\n",
            )
        );
        assert_eq!(
            ungrouped_expanded_argv,
            vec![
                "/bin/tool",
                "@bazel-out/k8-fastbuild/bin/a/ungrouped.out-0.params",
                "leading_pos",
                "bazel-out/k8-fastbuild/bin/a/tree_before/one.txt",
                "bazel-out/k8-fastbuild/bin/a/tree_before/two.txt",
                "bazel-out/k8-fastbuild/bin/a/tree_pos/first.txt",
                "bazel-out/k8-fastbuild/bin/a/tree_pos/second.txt",
                "-I",
                "bazel-out/k8-fastbuild/bin/a/tree_short/inc.h",
                "-single",
            ]
        );

        // 2. Grouped `StarlarkCustomCommandLineWithIndexes` (`flag_groups` per `Args.add*` call):
        //    each `add_all` collapses via `GroupedPreprocessedArgs`, then partitions on `--`.
        let grouped_args = vec![
            format!(
                "{}\u{1}--input\u{1}{}",
                crate::TREE_MARKER,
                tree_before.exec_path.path_string()
            ),
            format!(
                "{}--input=%s\u{1}\u{1}{}",
                crate::TREE_MARKER,
                tree_fmt.exec_path.path_string()
            ),
            format!(
                "{}\u{1}\u{1}{}",
                crate::TREE_MARKER,
                tree_pos.exec_path.path_string()
            ),
            crate::args::TREE_HEAD_MARKER.to_string(),
            format!(
                "{}--input=%s\u{1}\u{1}{}",
                crate::TREE_MARKER,
                tree_fmt.exec_path.path_string()
            ),
        ];
        let mut grouped = Action::new(
            Label::parse_canonical("//a:grouped").unwrap(),
            "RunTool",
            ActionKind::Spawn {
                executable: "/bin/tool".into(),
                arguments: vec![CommandLineArgs {
                    args: grouped_args,
                    param_file: Some(ParamFileInfo {
                        flag_format: "@%s".into(),
                        use_always: true,
                        format: ParamFileFormat::FlagPerLine,
                        leading_args: 0,
                        flag_groups: vec![(0, 1), (1, 2), (2, 3), (3, 5)],
                        explicit_path: None,
                    }),
                }],
                shell_command: None,
            },
        );
        grouped.inputs = vec![tree_before, tree_fmt, tree_pos];
        grouped.outputs.push(art("a/grouped.out"));
        let lowered_grouped = grouped.lower();
        let crate::ActionAnalysisMetadata::FileWriteAction {
            param_file_spec: Some(ref grouped_spec),
            ..
        } = lowered_grouped[0]
        else {
            panic!("expected FileWriteAction with param_file_spec");
        };
        let crate::ActionAnalysisMetadata::SpawnAction {
            argv: ref grouped_argv,
            ..
        } = lowered_grouped[1]
        else {
            panic!("expected SpawnAction");
        };
        let grouped_params =
            String::from_utf8(grouped_spec.render_expanded(&list_dir).unwrap()).unwrap();
        let grouped_expanded_argv =
            crate::expand_tree_artifact_args(grouped_argv, &list_dir).unwrap();
        assert_eq!(
            grouped_params,
            concat!(
                "--input=bazel-out/k8-fastbuild/bin/a/tree_before/one.txt --input bazel-out/k8-fastbuild/bin/a/tree_before/two.txt\n",
                "--input=bazel-out/k8-fastbuild/bin/a/tree_fmt/alpha.txt=--input=bazel-out/k8-fastbuild/bin/a/tree_fmt/beta.txt\n",
                "--input=bazel-out/k8-fastbuild/bin/a/tree_fmt/alpha.txt --input=bazel-out/k8-fastbuild/bin/a/tree_fmt/beta.txt\n",
            )
        );
        assert_eq!(
            grouped_expanded_argv,
            vec![
                "/bin/tool",
                "@bazel-out/k8-fastbuild/bin/a/grouped.out-0.params",
                "bazel-out/k8-fastbuild/bin/a/tree_pos/first.txt=bazel-out/k8-fastbuild/bin/a/tree_pos/second.txt",
            ]
        );
    }
}
