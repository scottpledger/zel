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
        Self { memory_mb: 250.0, cpu: 1.0, local_test_count: 0 }
    }
}

/// Parameter-file formats (`ParameterFileType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamFileFormat {
    /// One shell-quoted argument per whitespace-separated token, on one line per arg.
    Shell,
    /// One raw argument per line.
    Multiline,
    /// One raw argument per line, flag-style: `--flag=value` args are kept intact.
    FlagPerLine,
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
}

impl Default for ParamFileInfo {
    fn default() -> Self {
        Self {
            flag_format: "@%s".to_string(),
            format: ParamFileFormat::Shell,
            use_always: false,
            leading_args: 0,
        }
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
    if !s.is_empty() && (s.chars().all(safe) || (!s.starts_with('~') && s.chars().all(|c| safe(c) || c == '~'))) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// The contents of a `FileWriteAction`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileContent {
    /// `ctx.actions.write(output, "text")`
    Literal(String),
    /// `ctx.actions.write(output, args)`: arguments rendered into a param file.
    Args { args: Vec<String>, format: ParamFileFormat },
}

impl FileContent {
    /// Bytes written to disk.
    pub fn render(&self) -> Vec<u8> {
        match self {
            FileContent::Literal(s) => s.as_bytes().to_vec(),
            FileContent::Args { args, format } => render_param_file(args, *format).into_bytes(),
        }
    }
}

/// Renders a parameter file body (`ParameterFile.writeContent`).
pub fn render_param_file(args: &[String], format: ParamFileFormat) -> String {
    let mut s = String::new();
    for a in args {
        match format {
            ParamFileFormat::Shell => s.push_str(&shell_escape(a)),
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
    FileWrite { content: FileContent, is_executable: bool },
    /// `ctx.actions.symlink` (`SymlinkAction`): output is a symlink to `target_file` (an
    /// artifact input) or, for `target_path`, an unresolved symlink.
    Symlink { target_file: Option<Artifact>, target_path: Option<String>, is_executable: bool },
    /// `ctx.actions.expand_template` (`TemplateExpansionAction`).
    TemplateExpand {
        template: Artifact,
        /// In dict order (`ImmutableMap` insertion order).
        substitutions: Vec<(String, String)>,
        is_executable: bool,
        /// `computed_substitutions`, applied after plain ones.
        computed_substitutions: Vec<(String, String)>,
    },
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
            input_manifests: Vec::new(),
            java_compile: None,
            key_info: None,
            kind,
        }
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
    }

    pub fn primary_output(&self) -> Option<&Artifact> {
        self.outputs.first()
    }

    pub fn primary_input(&self) -> Option<&Artifact> {
        self.inputs.first()
    }

    /// Whether two such actions generating the same output may coexist
    /// (`Actions.canBeShared`): same mnemonic, same key, equal inputs/outputs ignoring owner.
    pub fn can_be_shared(&self, other: &Action) -> bool {
        self.mnemonic == other.mnemonic
            && self.key() == other.key()
            && same_paths(&self.inputs, &other.inputs)
            && same_paths(&self.outputs, &other.outputs)
    }

    /// `Action.getKey`: fingerprint over everything affecting the action's behaviour.
    pub fn key(&self) -> String {
        let mut fp = Fingerprint::new();
        fp.add_str(&self.mnemonic);
        match &self.kind {
            ActionKind::Spawn { executable, arguments, shell_command } => {
                fp.add_str("Spawn").add_str(executable);
                for a in arguments {
                    fp.add_strings(a.args.iter());
                    if let Some(p) = &a.param_file {
                        fp.add_str(&p.flag_format).add_bool(p.use_always);
                    }
                }
                fp.add_str(shell_command.as_deref().unwrap_or(""));
            }
            ActionKind::FileWrite { content, is_executable } => {
                fp.add_str("FileWrite").add_bytes(&content.render()).add_bool(*is_executable);
            }
            ActionKind::Symlink { target_file, target_path, is_executable } => {
                fp.add_str("Symlink")
                    .add_str(target_file.as_ref().map(|a| a.exec_path.path_string()).unwrap_or(""))
                    .add_str(target_path.as_deref().unwrap_or(""))
                    .add_bool(*is_executable);
            }
            ActionKind::TemplateExpand { template, substitutions, is_executable, computed_substitutions } => {
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
        fp.hex_digest_and_reset()
    }

    /// Full command line (`executable` followed by arguments), with param files folded in
    /// according to `ParamFileInfo`; the second element lists `(param file exec path, body)`
    /// pairs that must be written before the spawn runs.
    pub fn command_line(&self) -> (Vec<String>, Vec<(String, Vec<u8>)>) {
        let mut argv = Vec::new();
        let mut params = Vec::new();
        let ActionKind::Spawn { executable, arguments, .. } = &self.kind else {
            return (argv, params);
        };
        argv.push(executable.clone());
        let primary = self
            .outputs
            .first()
            .map(|o| o.exec_path.path_string().to_string())
            .unwrap_or_default();
        for (i, a) in arguments.iter().enumerate() {
            match &a.param_file {
                Some(pf) => {
                    let total: usize = a.args.iter().map(|s| s.len() + 1).sum();
                    if pf.use_always || total > DEFAULT_PARAM_FILE_THRESHOLD {
                        let lead = pf.leading_args.min(a.args.len());
                        argv.extend(a.args[..lead].iter().cloned());
                        let path = format!("{primary}-{i}.params");
                        argv.push(pf.flag_format.replace("%s", &path));
                        params.push((path, render_param_file(&a.args[lead..], pf.format).into_bytes()));
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
        let ActionKind::Spawn { executable, arguments, .. } = &self.kind else {
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
        match &self.kind {
            ActionKind::Spawn { .. } => {
                let (argv, params) = self.command_line();
                let mut inputs = self.inputs.clone();
                let mut java_compile = self.java_compile.clone();
                let primary_path = self.outputs.first().map(|o| o.exec_path.path_string().to_string()).unwrap_or_default();
                for (path, body) in params {
                    // The classpath param file of a reduced-classpath Java compile is written by
                    // the executor (its content depends on `.jdeps` files read at execution time).
                    if let Some(jc) = java_compile.as_mut() {
                        if path == format!("{primary_path}-{}.params", jc.classpath_arg_index) {
                            jc.classpath_params_path = path;
                            continue;
                        }
                    }
                    let art = Artifact {
                        is_source: false,
                        is_tree_artifact: false,
                        root: crate::ArtifactRoot::Bin,
                        is_unresolved_symlink: false,
                        exec_path: PathFragment::create(&path),
                        root_relative_path: PathFragment::create(
                            path.strip_prefix(&format!("{}/", self.outputs.first().map(|o| o.root_path()).unwrap_or_default()))
                                .unwrap_or(&path),
                        ),
                        owner_label: self.owner.clone(),
                    };
                    out.push(ActionAnalysisMetadata::FileWriteAction {
                        owner: self.owner.clone(),
                        output: art.clone(),
                        content: body,
                        is_executable: false,
                    });
                    inputs.push(art);
                }
                out.push(ActionAnalysisMetadata::SpawnAction {
                    owner: self.owner.clone(),
                    mnemonic: self.mnemonic.clone(),
                    progress_message: self
                        .progress_message
                        .clone()
                        .unwrap_or_else(|| {
                            if self.mnemonic == "Genrule" {
                                format!("Executing genrule {}", self.owner)
                            } else {
                                format!("Running {} for {}", self.mnemonic, self.owner)
                            }
                        }),
                    argv,
                    env: self.action_env.as_ref().map(|e| e.fixed_env().clone()).unwrap_or_else(|| self.env.clone()),
                    inherited_env: self.action_env.as_ref().map(|e| e.inherited_env().clone()).unwrap_or_default(),
                    unset_env: self.action_env.as_ref().map(|e| e.unset_env().clone()).unwrap_or_default(),
                    inputs,
                    outputs: self.outputs.clone(),
                    execution_info: self.execution_requirements.clone(),
                    java_compile: java_compile.map(Box::new),
                });
            }
            ActionKind::FileWrite { content, is_executable } => {
                out.push(ActionAnalysisMetadata::FileWriteAction {
                    owner: self.owner.clone(),
                    output: self.outputs[0].clone(),
                    content: content.render(),
                    is_executable: *is_executable,
                });
            }
            ActionKind::Symlink { target_file, target_path, is_executable } => {
                let input_path = match (target_file, target_path) {
                    (Some(f), _) => f.exec_path.clone(),
                    (None, Some(p)) => PathFragment::create(p),
                    (None, None) => PathFragment::empty(),
                };
                out.push(ActionAnalysisMetadata::SymlinkAction {
                    owner: self.owner.clone(),
                    input_path,
                    output: self.outputs[0].clone(),
                    is_executable: *is_executable,
                });
            }
            ActionKind::TemplateExpand { template, substitutions, is_executable, computed_substitutions } => {
                let mut subs: BTreeMap<String, String> = substitutions.iter().cloned().collect();
                for (k, v) in computed_substitutions {
                    subs.insert(k.clone(), v.clone());
                }
                out.push(ActionAnalysisMetadata::TemplateExpansionAction {
                    owner: self.owner.clone(),
                    template: template.clone(),
                    output: self.outputs[0].clone(),
                    substitutions: subs,
                    is_executable: *is_executable,
                });
            }
        }
        out
    }
}

fn same_paths(a: &[Artifact], b: &[Artifact]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.exec_path == y.exec_path)
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
}

impl ActionGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn actions(&self) -> impl Iterator<Item = &Action> {
        self.generating.values()
    }

    pub fn generating_action(&self, exec_path: &str) -> Option<&Action> {
        self.generating.get(exec_path)
    }

    /// Registers `action`, mirroring `MapBasedActionGraph.registerAction` plus the
    /// `ArtifactPrefixConflict` check and `Actions.canBeShared` sharing rule.
    pub fn register(&mut self, action: &Action) -> Result<(), ActionConflict> {
        for out in &action.outputs {
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
            // Prefix conflict: one output is a directory prefix of another.
            for (other, prev) in &self.generating {
                let (short, long, first_a, second_a) = if p.len() < other.len() {
                    (&p, other, action, prev)
                } else {
                    (other, &p, prev, action)
                };
                if long.len() > short.len()
                    && long.starts_with(short.as_str())
                    && long.as_bytes()[short.len()] == b'/'
                {
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
            self.generating
                .entry(out.exec_path.path_string().to_string())
                .or_insert_with(|| action.clone());
        }
        Ok(())
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

/// `ActionConflictException.debugSuffix` (the parts that exist in this model).
fn debug_suffix(a: &Action, b: &Action) -> String {
    let mut sb = String::new();
    detail(&mut sb, "Label", &a.owner.canonical_form(), &b.owner.canonical_form());
    detail(&mut sb, "RuleClass", &a.owner_kind, &b.owner_kind);
    detail(&mut sb, "Configuration", &a.owner_config, &b.owner_config);
    detail(&mut sb, "Mnemonic", &a.mnemonic, &b.mnemonic);
    detail(&mut sb, "Action key", &a.key(), &b.key());
    detail(
        &mut sb,
        "Progress message",
        a.progress_message.as_deref().unwrap_or("(null)"),
        b.progress_message.as_deref().unwrap_or("(null)"),
    );
    let pi = |x: &Action| x.primary_input().map(|i| i.to_string()).unwrap_or("(null)".into());
    detail(&mut sb, "PrimaryInput", &pi(a), &pi(b));
    let po = |x: &Action| x.primary_output().map(|i| i.to_string()).unwrap_or("(null)".into());
    detail(&mut sb, "PrimaryOutput", &po(a), &po(b));
    sb
}

#[cfg(test)]
mod tests {
    use super::*;

    fn art(p: &str) -> Artifact {
        Artifact::derived("k8-fastbuild", PathFragment::create(p), Label::parse_canonical("//a:b").unwrap())
    }

    fn write(owner: &str, out: &str, text: &str) -> Action {
        let mut a = Action::new(
            Label::parse_canonical(owner).unwrap(),
            "FileWrite",
            ActionKind::FileWrite { content: FileContent::Literal(text.into()), is_executable: false },
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
                    param_file: Some(ParamFileInfo { use_always: true, leading_args: 0, ..Default::default() }),
                }],
                shell_command: None,
            },
        );
        a.outputs.push(art("a/out"));
        let (argv, params) = a.command_line();
        assert_eq!(argv[0], "/bin/tool");
        assert_eq!(argv[1], "@bazel-out/k8-fastbuild/bin/a/out-0.params");
        assert_eq!(String::from_utf8(params[0].1.clone()).unwrap(), "--x\n'a b'\n");
        let lowered = a.lower();
        assert_eq!(lowered.len(), 2);
    }
}
