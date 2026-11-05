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

//! `aquery` output formatters (`ActionGraphTextOutputFormatterCallback`,
//! `ActionGraphSummaryOutputFormatterCallback`, `ActionGraphProtoOutputFormatterCallback`).

use crate::engine::Node;
use crate::format::{lp, FormatOpts};
use crate::pb::{Msg, PbVal};
use crate::post_analysis::{filters_of, PostEnv, PostOpts};
use crate::regex::Regex;
use std::collections::{BTreeMap, HashMap, HashSet};
use zel_analysis::{shell_escape, Action, ActionKind, Artifact, BuildConfigurationKey, FileContent};
use zel_cmdline::Label;

/// One action together with the configured target that owns it.
struct Entry {
    action: Action,
    owner: Label,
    owner_kind: String,
    cfg: BuildConfigurationKey,
    cfg_id: String,
    aspect: Option<String>,
    synth: Synth,
}

/// Actions Bazel registers for executables (`RunfilesSupport`, `TestActionBuilder`) that zel's
/// analysis does not model as typed actions; they are reconstructed here from `RunfilesSupport`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Synth {
    None,
    SymlinkTree,
    RunfilesTree,
    TestRunner,
}

/// `Artifact.prettyPrint`: root-relative path.
fn pretty_path(a: &Artifact) -> String {
    a.root_relative_path.path_string().to_string()
}

fn pretty_print(a: &Action, for_tool: bool) -> String {
    if a.mnemonic == "RunfilesTree" {
        // `RunfilesTreeAction.prettyPrint` is not wrapped in `action '...'`.
        return a.progress_message.clone().unwrap_or_default();
    }
    let msg = match &a.progress_message {
        Some(m) => m.clone(),
        None => match &a.kind {
            ActionKind::FileWrite { is_executable, .. } => {
                format!(
                    "{} {}",
                    if *is_executable { "Writing script" } else { "Writing file" },
                    a.outputs.first().map(pretty_path).unwrap_or_default()
                )
            }
            // `SolibSymlinkAction` has no raw progress message: `AbstractAction.prettyPrint`.
            ActionKind::Symlink { .. } if a.mnemonic == "SolibSymlink" => {
                format!("SolibSymlink {}", a.outputs.first().map(pretty_path).unwrap_or_default())
            }
            ActionKind::Symlink { .. } => {
                format!("Creating symlink {}", a.outputs.first().map(pretty_path).unwrap_or_default())
            }
            ActionKind::TemplateExpand { .. } => {
                format!("Expanding template {}", a.outputs.first().map(pretty_path).unwrap_or_default())
            }
            ActionKind::Spawn { .. } => {
                if a.mnemonic == "Genrule" {
                    format!("Executing genrule {}", a.owner)
                } else {
                    format!("{} {}", a.mnemonic, a.outputs.first().map(pretty_path).unwrap_or_default())
                }
            }
        },
    };
    // `AbstractAction.getProgressMessage`: actions of the tool (exec) configurations are annotated.
    format!("action '{msg}{}'", if for_tool { " [for tool]" } else { "" })
}

fn escaped_unicode(s: &str) -> String {
    if s.chars().all(|c| (c as u32) >= 0x20 && (c as u32) < 0x7f) {
        return s.to_string();
    }
    let mut out = String::new();
    for c in s.chars() {
        let u = c as u32;
        if (0x20..0x7f).contains(&u) {
            out.push(c);
        } else if u <= 0xFFFF {
            out.push_str(&format!("\\u{u:04X}"));
        } else {
            out.push_str(&format!("\\U{u:08X}"));
        }
    }
    out
}

fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in b.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Java `ShellEscaper.escapeString`.
fn java_shell_escape(s: &str) -> String {
    shell_escape(s)
}

fn matches_filters(a: &Action, filters: &(Vec<String>, Vec<String>, Vec<String>)) -> bool {
    let (mn, inp, outp) = filters;
    // `AqueryActionFilter.matchesAllPatternsForFunction`: `Pattern.matcher(s).matches()` (full match).
    let all = |pats: &Vec<String>, s: &str| pats.iter().all(|p| Regex::new(p).map(|r| r.full_match(s)).unwrap_or(false));
    if !mn.is_empty() && !all(mn, &a.mnemonic) {
        return false;
    }
    if !inp.is_empty() && !a.inputs.iter().any(|i| all(inp, i.exec_path.path_string())) {
        return false;
    }
    if !outp.is_empty() && !a.outputs.iter().any(|o| all(outp, o.exec_path.path_string())) {
        return false;
    }
    true
}

/// `RunfilesSupport` registers (in this order, after the rule's own actions) the repo-mapping
/// manifest, the source manifest, the `SymlinkTree` and the `RunfilesTree`; tests then get their
/// `TestRunner`. The source manifest's runfiles are not inputs (remotable manifests).
fn executable_actions(
    local: &mut Vec<Entry>,
    ct: &zel_analysis::ConfiguredTarget,
    label: &Label,
    kind: &str,
    cfg: &BuildConfigurationKey,
    cfg_id: &str,
    filters: &(Vec<String>, Vec<String>, Vec<String>),
) {
    let Some(rs) = &ct.runfiles_support else { return };
    let pos_rm = local.iter().position(|e| e.action.mnemonic == "RepoMappingManifest");
    let pos_sm = local.iter().position(|e| e.action.mnemonic == "SourceSymlinkManifest");
    if let (Some(r), Some(s)) = (pos_rm, pos_sm) {
        if s < r {
            let e = local.remove(r);
            local.insert(s, e);
        }
    }
    for e in local.iter_mut() {
        if e.action.mnemonic == "SourceSymlinkManifest" {
            e.action.inputs.clear();
        }
    }
    let mk = |action: Action, synth: Synth| Entry {
        action,
        owner: label.clone(),
        owner_kind: kind.to_string(),
        cfg: cfg.clone(),
        cfg_id: cfg_id.to_string(),
        aspect: None,
        synth,
    };
    let cfg_mnemonic = cfg.mnemonic();
    let root = zel_analysis::ArtifactRoot::Bin;
    let manifest_in_tree = Artifact::derived_in_root(
        &cfg_mnemonic,
        root,
        zel_vfs::PathFragment::create(&format!("{}/MANIFEST", rs.runfiles_dir.rsplit_once("/bin/").map(|x| x.1).unwrap_or(&rs.runfiles_dir))),
        label.clone(),
    );
    let tree_dir = Artifact {
        is_source: false,
        is_tree_artifact: false,
        root: root,
        is_unresolved_symlink: false,
        exec_path: zel_vfs::PathFragment::create(&rs.runfiles_dir),
        root_relative_path: zel_vfs::PathFragment::create(&format!("{}.runfiles", rs.executable.root_relative_path.path_string())),
        owner_label: label.clone(),
    };
    let mut st = Action::new(label.clone(), "SymlinkTree", ActionKind::Spawn { executable: String::new(), arguments: Vec::new(), shell_command: None });
    st.progress_message = Some(format!("Creating runfiles tree {}", rs.runfiles_dir));
    st.inputs.push(rs.manifest.clone());
    st.outputs.push(manifest_in_tree.clone());
    st.use_default_shell_env = true;
    st.apply_configuration_env(cfg);
    if matches_filters(&st, filters) {
        local.push(mk(st, Synth::SymlinkTree));
    }
    let mut rt = Action::new(label.clone(), "RunfilesTree", ActionKind::Spawn { executable: String::new(), arguments: Vec::new(), shell_command: None });
    rt.progress_message = Some(format!("runfiles for {}", label.canonical_form()));
    rt.inputs.extend(rs.runfiles.artifacts.iter().cloned());
    if !rt.inputs.iter().any(|i| i.exec_path == rs.executable.exec_path) {
        rt.inputs.push(rs.executable.clone());
    }
    if let Some(m) = &rs.repo_mapping_manifest {
        rt.inputs.push(m.clone());
    }
    rt.inputs.push(manifest_in_tree);
    rt.outputs.push(tree_dir.clone());
    if matches_filters(&rt, filters) {
        local.push(mk(rt, Synth::RunfilesTree));
    }
    if ct.is_test {
        let short = format!("{}/{}", label.package_name(), label.name());
        let short = short.trim_start_matches('/').to_string();
        let logs = format!("bazel-out/{cfg_mnemonic}/testlogs/{short}");
        let out_art = |name: &str, tree: bool| Artifact {
            is_source: false,
            is_tree_artifact: tree,
            root,
            is_unresolved_symlink: false,
            exec_path: zel_vfs::PathFragment::create(&format!("{logs}/{name}")),
            root_relative_path: zel_vfs::PathFragment::create(&format!("testlogs/{short}/{name}")),
            owner_label: label.clone(),
        };
        let tool = |p: &str| Artifact {
            is_source: true,
            is_tree_artifact: false,
            root: zel_analysis::ArtifactRoot::Source,
            is_unresolved_symlink: false,
            exec_path: zel_vfs::PathFragment::create(&format!("external/bazel_tools/{p}")),
            root_relative_path: zel_vfs::PathFragment::create(p),
            owner_label: label.clone(),
        };
        let mut tr = Action::new(
            label.clone(),
            "TestRunner",
            ActionKind::Spawn {
                executable: "external/bazel_tools/tools/test/test-setup.sh".to_string(),
                arguments: vec![zel_analysis::CommandLineArgs { args: vec![short.clone()], param_file: None }],
                shell_command: None,
            },
        );
        tr.progress_message = Some(format!("Testing {}", label.canonical_form()));
        tr.inputs.push(tree_dir);
        tr.inputs.push(tool("tools/test/generate-xml.sh"));
        tr.inputs.push(tool("tools/test/test-setup.sh"));
        for n in ["test.cache_status", "test.log"] {
            tr.outputs.push(out_art(n, false));
        }
        tr.outputs.push(out_art("test.outputs", true));
        tr.outputs.push(out_art("test.xml", false));
        tr.use_default_shell_env = true;
        tr.apply_configuration_env(cfg);
        // `TestTargetProperties`: the `testing.ExecutionInfo` provider's requirements.
        if let Some(zel_starlark::Value::ProviderInstance { fields, .. }) = ct.providers.get("ExecutionInfo") {
            if let Some(zel_starlark::Value::Dict(d)) = fields.get("requirements") {
                for (k, v) in d.lock().unwrap().items() {
                    if let (zel_starlark::Value::String(k), zel_starlark::Value::String(v)) = (k, v) {
                        tr.execution_requirements.insert(k, v);
                    }
                }
            }
        }
        if matches_filters(&tr, filters) {
            local.push(mk(tr, Synth::TestRunner));
        }
    }
}

/// Duration as `java.time.Duration.toString()`.
fn java_duration(secs: u64) -> String {
    if secs == 0 {
        return "PT0S".into();
    }
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    format!("PT{}{}{}", if h > 0 { format!("{h}H") } else { String::new() }, if m > 0 { format!("{m}M") } else { String::new() }, if s > 0 { format!("{s}S") } else { String::new() })
}

/// The `TestRunnerAction.computeKey` inputs of the test rule `r`.
fn test_runner_key(r: &zel_packages::RuleInstance, cfg: &BuildConfigurationKey, ct: &zel_analysis::ConfiguredTarget) -> zel_analysis::TestRunnerKey {
    use zel_packages::attr::AttrValue;
    let s = |n: &str| match r.attr_or_default(n) {
        Some(AttrValue::Str(v)) if !v.is_empty() => Some(v),
        _ => None,
    };
    let native = |n: &str| cfg.native.get(n).map(|x| x.value.display());
    let size = s("size").unwrap_or_else(|| "medium".into());
    let explicit_timeout = if r.extra.attr_values.contains_key("timeout") { s("timeout") } else { None };
    let category = explicit_timeout.filter(|t| t != "illegal").unwrap_or_else(|| match size.as_str() {
        "small" => "short",
        "large" => "long",
        "enormous" => "eternal",
        _ => "moderate",
    }.into());
    let idx = match category.as_str() {
        "short" => 0,
        "moderate" => 1,
        "long" => 2,
        _ => 3,
    };
    let defaults = [60u64, 300, 900, 3600];
    let secs = match cfg.options.get("test_timeout").map(|v| v.split(',').filter_map(|x| x.trim().parse::<u64>().ok()).collect::<Vec<_>>()) {
        Some(v) if v.len() == 1 => v[0],
        Some(v) if v.len() == 4 => v[idx],
        _ => defaults[idx],
    };
    let tags: Vec<String> = match r.attr_or_default("tags") {
        Some(AttrValue::StrList(t)) => t,
        _ => Vec::new(),
    };
    let local = matches!(r.attr_or_default("local"), Some(AttrValue::Bool(true))) || tags.iter().any(|t| t == "local" || t == "no-remote" || t == "no-remote-exec");
    let total_shards = match r.attr_or_default("shard_count") {
        Some(AttrValue::Int(n)) if n > 0 => n as i32,
        _ => 0,
    };
    let mut args: Vec<String> = match r.attr_or_default("args") {
        Some(AttrValue::StrList(a)) => a,
        _ => Vec::new(),
    };
    if let Some(t) = cfg.options.get("test_arg") {
        args.extend(t.split(',').filter(|x| !x.is_empty()).map(String::from));
    }
    let env_parts = |e: zel_util::action_env::ActionEnvironment| (e.fixed_env().clone(), e.inherited_env().iter().cloned().collect::<Vec<_>>());
    let extra = ct.run_environment.as_ref().map(|re| (re.environment.clone(), { let mut v = re.inherited_environment.clone(); v.sort(); v.dedup(); v })).unwrap_or_default();
    zel_analysis::TestRunnerKey {
        args,
        test_filter: native("test_filter").unwrap_or_default(),
        fail_fast: native("test_runner_fail_fast").map(|v| v == "true").unwrap_or(false),
        run_under: cfg.options.get("run_under").cloned().unwrap_or_default(),
        coverage_env: Default::default(),
        extra_env: extra,
        action_env: env_parts(cfg.action_environment()),
        test_env: env_parts(cfg.test_action_environment()),
        size,
        timeout_category: category,
        timeout: java_duration(secs),
        tags,
        remotable: !local,
        shard_num: 0,
        total_shards,
        run_number: 0,
        total_runs: native("runs_per_test").and_then(|v| v.split('@').last().and_then(|x| x.trim().parse::<i32>().ok())).unwrap_or(1),
        coverage: cfg.options.get("collect_code_coverage").map(|v| v == "true").unwrap_or(false),
        zip_undeclared: native("zip_undeclared_test_outputs").map(|v| v == "true").unwrap_or(false),
    }
}

fn collect(env: &mut PostEnv<'_>, nodes: &[Node]) -> Result<Vec<Entry>, String> {
    let filters = filters_of(env);
    let mut out = Vec::new();
    for n in nodes {
        let Some(id) = &n.cfg else { continue };
        let Some(info) = env.info_of(&n.label) else { continue };
        if !matches!(info.target, zel_packages::Target::Rule(_)) {
            continue;
        }
        let Some(ct) = env.analyzed(n).map_err(|e| e.message)? else { continue };
        let cfg = env.config_key(id).cloned().unwrap_or_else(|| ct.configuration.clone());
        let kind = format!("{} rule", ct.rule_class);
        let mut cts = vec![(ct, None)];
        if env.tenv.opts.include_aspects {
            for asp in env.tr.aspects_for(&n.label) {
                let asp_name = asp.aspect_name.clone().unwrap_or_else(|| asp.rule_class.clone());
                cts.push((asp, Some(asp_name)));
            }
        }
        for (current_ct, asp_opt) in cts {
            let mut local: Vec<Entry> = Vec::new();
            for a in &current_ct.typed_actions {
                if !matches_filters(a, &filters) {
                    continue;
                }
                let mut action = a.clone();
                if asp_opt.is_none() && action.mnemonic == "Genrule" {
                    // `GenRule`: the runfiles (middleman) of every executable `tools` entry are inputs.
                    let extra = tool_runfiles_inputs(env, &n.label, &cfg);
                    for art in extra {
                        if !action.inputs.iter().any(|i| i.exec_path == art.exec_path) {
                            action.inputs.push(art);
                        }
                    }
                }
                local.push(Entry {
                    action,
                    owner: n.label.clone(),
                    owner_kind: kind.clone(),
                    cfg: cfg.clone(),
                    cfg_id: id.clone(),
                    aspect: asp_opt.clone(),
                    synth: Synth::None,
                });
            }
            if asp_opt.is_none() {
                executable_actions(&mut local, &current_ct, &n.label, &kind, &cfg, id, &filters);
                if current_ct.is_test {
                    if let Some(zel_packages::Target::Rule(r)) = env.info_of(&n.label).map(|i| i.target.clone()) {
                        for e in local.iter_mut() {
                            if e.synth == Synth::TestRunner && e.action.key_info.is_none() {
                                e.action.key_info = Some(std::sync::Arc::new(zel_analysis::ActionKeyInfo::TestRunner(Box::new(test_runner_key(&r, &cfg, &current_ct)))));
                            }
                        }
                    }
                }
                if let Some(rs) = &current_ct.runfiles_support {
                    let pkgs = env.tr.transitive_packages(&n.label, &cfg);
                    if std::env::var("ZEL_DEBUG_PKGS").is_ok() {
                        { use std::io::Write; let msg = format!("PKGS {} {:?} deps={:?}", n.label.canonical_form(), pkgs.to_vec().iter().map(|p| p.package.clone()).collect::<Vec<_>>(), env.tr.direct_deps(&n.label, &cfg).iter().map(|(l, c)| format!("{}#{}", l.canonical_form(), c.mnemonic())).collect::<Vec<_>>()); let _ = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/u2c/pkgs.log").and_then(|mut f| writeln!(f, "{}", msg)); }
                    }
                    for e in local.iter_mut() {
                        if e.action.mnemonic == "RepoMappingManifest" && e.action.key_info.is_none() {
                            use zel_analysis::nested_set::{NSet, STABLE_ORDER};
                            let roots: Vec<(String, String)> = rs
                                .runfiles
                                .root_symlinks
                                .keys()
                                .map(|k| (k.clone(), k.split('/').next().unwrap_or("").to_string()))
                                .collect();
                            let has_syms = !rs.runfiles.symlinks.is_empty();
                            if std::env::var("ZEL_DEBUG_PKGS").is_ok() {
                                use std::io::Write;
                                fn dump(n: &zel_analysis::nested_set::NNode<zel_analysis::PkgMeta>) -> String {
                                    match n {
                                        zel_analysis::nested_set::NNode::Leaf(p) => format!("{}", p.package),
                                        zel_analysis::nested_set::NNode::Arr(a) => format!("[{}]", a.iter().map(dump).collect::<Vec<_>>().join(" ")),
                                    }
                                }
                                let _ = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/u2c/pkgs.log").and_then(|mut f| writeln!(f, "STRUCT {} order={} {}", n.label.canonical_form(), pkgs.order, pkgs.children.as_ref().map(dump).unwrap_or_default()));
                                let _ = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/u2c/pkgs.log").and_then(|mut f| writeln!(f, "RM {} rf={} syms={} roots={:?} ws={} mapping={:?}", n.label.canonical_form(), { fn d(n: &zel_analysis::nested_set::NNode<(String, String)>) -> String { match n { zel_analysis::nested_set::NNode::Leaf(p) => p.0.clone(), zel_analysis::nested_set::NNode::Arr(a) => format!("[{}]", a.iter().map(d).collect::<Vec<_>>().join(" ")) } } rs.runfiles.nested_set().children.as_ref().map(d).unwrap_or_default() }, has_syms, roots, rs.workspace_name, pkgs.to_vec().iter().map(|p| (p.package.clone(), p.mapping.clone())).collect::<Vec<_>>()));
                            }
                            e.action.key_info = Some(std::sync::Arc::new(zel_analysis::ActionKeyInfo::RepoMapping {
                                packages: pkgs.clone(),
                                runfiles_owner_repos: rs.runfiles.nested_set(),
                                has_runfiles_symlinks: has_syms,
                                root_symlink_segments: NSet::build(STABLE_ORDER, roots, vec![]),
                                workspace_name: rs.workspace_name.clone(),
                                compact: !env.tr.config.no_compact_repo_mapping,
                            }));
                        }
                    }
                }
            }
            out.extend(local);
        }
    }
    Ok(out)
}

/// The `<tool>.runfiles` inputs of the executable tools of the genrule `label`.
fn tool_runfiles_inputs(env: &mut PostEnv<'_>, label: &Label, cfg: &BuildConfigurationKey) -> Vec<Artifact> {
    let Some(info) = env.info_of(label) else { return Vec::new() };
    let zel_packages::Target::Rule(r) = &info.target else { return Vec::new() };
    let tool_labels: Vec<Label> = crate::target_env::rule_deps(r, true, true)
        .into_iter()
        .filter(|(_, a)| a == "tools" || a == "exec_tools")
        .map(|(l, _)| l)
        .collect();
    let mut out = Vec::new();
    for (l, key) in env.tr.direct_deps(label, cfg) {
        if !tool_labels.contains(&l) {
            continue;
        }
        let Ok(ct) = env.tr.analyze(&l, &key) else { continue };
        if let Some(rs) = &ct.runfiles_support {
            let exe = &rs.executable;
            out.push(Artifact {
                is_source: false,
                is_tree_artifact: false,
                root: exe.root,
                is_unresolved_symlink: false,
                exec_path: zel_vfs::PathFragment::create(&rs.runfiles_dir),
                root_relative_path: zel_vfs::PathFragment::create(&format!("{}.runfiles", exe.root_relative_path.path_string())),
                owner_label: exe.owner_label.clone(),
            });
        }
    }
    out
}

fn command_line(e: &Entry) -> Vec<String> {
    e.action.all_arguments()
}

fn describe_command(e: &Entry) -> String {
    let args = command_line(e);
    let mut msg = String::from("(exec ");
    let mut first = true;
    for a in args.iter().map(|a| escaped_unicode(a)) {
        if !first {
            msg.push_str(" \\\n    ");
        }
        msg.push_str(&java_shell_escape(&a));
        first = false;
    }
    msg.push(')');
    msg.push_str(&format!("\n# Configuration: {}", e.cfg.checksum()));
    if let Some(p) = exec_platform(e) {
        msg.push_str(&format!("\n# Execution platform: {p}"));
    }
    msg
}

/// The execution platform of the action's owner: the default host platform for target
/// configurations, the (single) execution platform for exec configurations.
fn exec_platform(e: &Entry) -> Option<String> {
    if e.cfg.is_exec {
        e.cfg.platforms.first().cloned().or_else(|| Some("@@platforms//host:host".to_string()))
    } else {
        Some("@@platforms//host:host".to_string())
    }
}

/// `Action.getKey` as Bazel computes it (see `action_key.rs`).
fn entry_key(e: &Entry) -> String {
    crate::action_key::action_key(&e.action, exec_platform(e).as_deref())
}

fn sorted_paths(v: &[Artifact]) -> Vec<String> {
    let mut p: Vec<String> = v
        .iter()
        .map(|a| {
            let s = a.exec_path.path_string().to_string();
            escaped_unicode(&if a.is_tree_artifact { format!("{s} (TreeArtifact)") } else { s })
        })
        .collect();
    p.sort();
    p
}

fn write_text(e: &Entry, popts: &PostOpts, params: &BTreeMap<String, String>, templates: &HashMap<String, String>) -> String {
    let a = &e.action;
    let mut s = String::new();
    s.push_str(&pretty_print(a, e.cfg.is_exec));
    s.push('\n');
    s.push_str(&format!("  Mnemonic: {}\n", a.mnemonic));
    s.push_str(&format!("  Target: {}\n", lp(&e.owner)));
    s.push_str(&format!("  Configuration: {}\n", e.cfg.mnemonic()));
    if let Some(p) = exec_platform(e) {
        // `ActionGraphTextOutputFormatterCallback` prints it through the label printer (display
        // form); `# Execution platform:` in `Command Line:` and the proto field use `Label.toString()`.
        let shown = zel_cmdline::Label::parse_canonical(&p).map(|l| lp(&l)).unwrap_or(p);
        s.push_str(&format!("  Execution platform: {shown}\n"));
    }
    if let Some(asp) = &e.aspect {
        s.push_str(&format!("  AspectDescriptors: [{asp}()]\n"));
    }
    s.push_str(&format!("  ActionKey: {}\n", entry_key(e)));
    if popts.include_artifacts {
        s.push_str(&format!("  Inputs: [{}]\n", sorted_paths(&a.inputs).join(", ")));
        s.push_str(&format!("  Outputs: [{}]\n", sorted_paths(&a.outputs).join(", ")));
    }
    let env = match &a.action_env {
        Some(ae) => ae.fixed_env().clone(),
        None => a.env.clone(),
    };
    if matches!(a.kind, ActionKind::Spawn { .. }) && !env.is_empty() {
        let mut v: Vec<String> = env.iter().map(|(k, v)| escaped_unicode(&format!("{k}={v}"))).collect();
        v.sort();
        s.push_str(&format!("  Environment: [{}]\n", v.join(", ")));
    }
    if (popts.include_commandline || popts.include_param_files)
        && matches!(a.kind, ActionKind::Spawn { .. })
        && !matches!(e.synth, Synth::SymlinkTree | Synth::RunfilesTree)
    {
        s.push_str(&format!("  Command Line: {}\n", describe_command(e)));
    }
    if popts.include_param_files {
        for i in &a.inputs {
            if let Some(c) = params.get(i.exec_path.path_string()) {
                s.push_str(&format!("  Params File Content ({}):\n    {}\n", i.exec_path, c));
            }
        }
    }
    if !a.execution_requirements.is_empty() {
        let v: Vec<String> = a
            .execution_requirements
            .iter()
            .map(|(k, v)| format!("{}: {}", java_shell_escape(k), java_shell_escape(v)))
            .collect();
        s.push_str(&format!("  ExecutionInfo: {{{}}}\n", v.join(", ")));
    }
    if let ActionKind::TemplateExpand { template, substitutions, computed_substitutions, .. } = &a.kind {
        s.push_str(&format!("  Template: {}\n", template_content(template, templates)));
        s.push_str("  Substitutions: [\n");
        let mut subs: Vec<(String, String)> = substitutions.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        subs.extend(computed_substitutions.iter().cloned());
        for (k, v) in subs {
            s.push_str(&format!("    {{{k}: {v}}}\n"));
        }
        s.push_str("  ]\n");
    }
    match &a.kind {
        // `ParameterFileWriteAction` (Args content) is not a `FileContentsProvider`.
        ActionKind::FileWrite { content: FileContent::Args { .. }, .. } => {}
        ActionKind::FileWrite { content, is_executable } => {
            // `GenQuery`'s output writer is not an `AbstractFileWriteAction.FileContentsProvider`.
            if !e.owner_kind.starts_with("genquery") {
                s.push_str(&format!("  IsExecutable: {is_executable}\n"));
            }
            if popts.include_file_write_contents {
                s.push_str(&format!("  FileWriteContents: [{}]\n", base64(&content.render())));
            }
        }
        // 9.3.2: `TemplateExpansionAction` is no `FileContentsProvider` (no `IsExecutable`).
        ActionKind::Symlink { target_path: Some(p), .. } => {
            s.push_str(&format!("  UnresolvedSymlinkTarget: {p}\n"));
        }
        _ => {}
    }
    s.push('\n');
    s
}

/// `AqueryUtils.getTemplateContent`: the file's text for source templates, `ARTIFACT:<path>` for
/// generated ones.
fn template_content(t: &Artifact, templates: &HashMap<String, String>) -> String {
    if t.is_source {
        templates.get(t.exec_path.path_string()).cloned().unwrap_or_default()
    } else {
        format!("ARTIFACT:{}", t.exec_path.path_string())
    }
}

fn read_templates(env: &PostEnv<'_>, entries: &[Entry]) -> HashMap<String, String> {
    let lookup = &env.tenv.resolver.loader().config().lookup;
    let mut out = HashMap::new();
    for e in entries {
        if let ActionKind::TemplateExpand { template, .. } = &e.action.kind {
            if template.is_source {
                if let Some(root) = lookup.resolver.repo_root(template.owner_label.repository()) {
                    let path = root.get_relative(&template.root_relative_path);
                    if let Ok(bytes) = lookup.fs.read_file(&path) {
                        out.insert(template.exec_path.path_string().to_string(), String::from_utf8_lossy(&bytes).to_string());
                    }
                }
            }
        }
    }
    out
}

fn param_files(entries: &[Entry]) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for e in entries {
        let (_, params) = e.action.command_line();
        for (path, body) in params {
            m.insert(path, String::from_utf8_lossy(&body).replace('\n', " \\\n    "));
        }
        if let ActionKind::FileWrite { content, .. } = &e.action.kind {
            if e.action.mnemonic == "ParameterFileWrite" {
                if let Some(o) = e.action.outputs.first() {
                    let body = String::from_utf8_lossy(&content.render()).to_string();
                    m.insert(o.exec_path.path_string().to_string(), body.lines().collect::<Vec<_>>().join(" \\\n    "));
                }
            }
        }
    }
    m
}

pub(crate) fn format_aquery(env: &mut PostEnv<'_>, nodes: &[Node], popts: &PostOpts, _fopts: &FormatOpts) -> Result<Vec<u8>, String> {
    let mut entries = collect(env, nodes)?;
    if popts.prune_unused_actions {
        let mut generating: HashMap<Artifact, usize> = HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            for o in &e.action.outputs {
                generating.insert(o.clone(), i);
            }
        }
        let mut reachable = HashSet::new();
        let mut worklist: std::collections::VecDeque<Artifact> = std::collections::VecDeque::new();
        for n in nodes {
            if let Some(_info) = env.info_of(&n.label) {
                if let Some(ct) = env.analyzed(n).ok().flatten() {
                    for a in &ct.default_info.files {
                        worklist.push_back(a.clone());
                    }
                }
            }
        }
        let mut visited_artifacts = HashSet::new();
        while let Some(art) = worklist.pop_front() {
            if !visited_artifacts.insert(art.clone()) {
                continue;
            }
            if let Some(&act_idx) = generating.get(&art) {
                if reachable.insert(act_idx) {
                    for inp in &entries[act_idx].action.inputs {
                        worklist.push_back(inp.clone());
                    }
                }
            }
        }
        entries = entries.into_iter().enumerate().filter(|(i, _)| reachable.contains(i)).map(|(_, e)| e).collect();
    }
    let params = if popts.include_param_files { param_files(&entries) } else { BTreeMap::new() };
    let templates = read_templates(env, &entries);
    match popts.output.as_str() {
        "text" => {
            let mut out = String::new();
            for e in &entries {
                out.push_str(&write_text(e, popts, &params, &templates));
            }
            Ok(out.into_bytes())
        }
        "commands" => {
            let mut out = String::new();
            for e in &entries {
                if !matches!(e.action.kind, ActionKind::Spawn { .. }) {
                    continue;
                }
                let args = command_line(e);
                let mut first = true;
                for a in args {
                    if !first {
                        out.push(' ');
                    }
                    let esc = escaped_unicode(&a);
                    out.push_str(&java_shell_escape(&esc));
                    first = false;
                }
                out.push('\n');
            }
            Ok(out.into_bytes())
        }
        "summary" => Ok(summary(&entries).into_bytes()),
        _ => proto_output(&entries, popts, &templates),
    }
}

fn summary(entries: &[Entry]) -> String {
    let mut mnemonics: Vec<(String, usize)> = Vec::new();
    let mut configs: Vec<(String, usize)> = Vec::new();
    let mut platforms: Vec<(String, usize)> = Vec::new();
    fn bump(v: &mut Vec<(String, usize)>, k: String) {
        match v.iter_mut().find(|(x, _)| *x == k) {
            Some(e) => e.1 += 1,
            None => v.push((k, 1)),
        }
    }
    for e in entries {
        bump(&mut mnemonics, e.action.mnemonic.clone());
        bump(&mut configs, e.cfg.mnemonic());
        if let Some(p) = exec_platform(e) {
            bump(&mut platforms, p);
        }
    }
    let total = entries.len();
    let mut out = String::new();
    if total == 0 {
        out.push_str("No actions matched.\n");
    } else {
        out.push_str(&format!("{total} total action{}.\n", if total == 1 { "" } else { "s" }));
    }
    for (title, mut v) in [("Mnemonics:", mnemonics), ("Configurations:", configs), ("Execution Platforms:", platforms)] {
        if !v.is_empty() {
            out.push('\n');
            out.push_str(title);
            out.push('\n');
            v.sort_by_key(|(_, c)| *c);
            for (k, c) in v {
                out.push_str(&format!("  {k}: {c}\n"));
            }
        }
    }
    out
}

struct Ids {
    paths: HashMap<String, u32>,
    path_msgs: Vec<Msg>,
    artifacts: HashMap<String, u32>,
    artifact_msgs: Vec<Msg>,
    next_path_id: u32,
    next_artifact_id: u32,
    /// Every proto in streaming order (`textproto` / `streamed_proto` print in this order).
    events: Vec<(u32, &'static str, Msg)>,
}

impl Ids {
    /// `KnownPathFragments`: the id is allocated before the parent chain is visited (so the leaf
    /// gets the smallest id) while the protos are streamed ancestors-first.
    fn path_id(&mut self, path: &str) -> u32 {
        if let Some(&i) = self.paths.get(path) {
            return i;
        }
        let id = self.next_path_id;
        self.next_path_id += 1;
        self.paths.insert(path.to_string(), id);
        let (parent, label) = match path.rsplit_once('/') {
            Some((p, l)) => (self.path_id(p), l.to_string()),
            None => (0, path.to_string()),
        };
        let mut m = Msg::new();
        m.add(1, "id", PbVal::Uint32(id));
        m.str(2, "label", label);
        if parent != 0 {
            m.add(3, "parent_id", PbVal::Uint32(parent));
        }
        self.events.push((8, "path_fragments", m.clone()));
        self.path_msgs.push(m);
        id
    }

    /// `KnownArtifacts`: id first, then the path fragments, then the artifact proto.
    fn artifact_id(&mut self, a: &Artifact) -> u32 {
        let key = a.exec_path.path_string().to_string();
        if let Some(&i) = self.artifacts.get(&key) {
            return i;
        }
        let id = self.next_artifact_id;
        self.next_artifact_id += 1;
        self.artifacts.insert(key.clone(), id);
        let pid = self.path_id(&key);
        let mut m = Msg::new();
        m.add(1, "id", PbVal::Uint32(id));
        m.add(2, "path_fragment_id", PbVal::Uint32(pid));
        if a.is_tree_artifact {
            m.bool(3, "is_tree_artifact", true);
        }
        self.events.push((1, "artifacts", m.clone()));
        self.artifact_msgs.push(m);
        id
    }
}

fn kv(k: &str, v: &str) -> Msg {
    let mut m = Msg::new();
    m.str(1, "key", k);
    m.str(2, "value", v);
    m
}

/// Streams one `DepSetOfFiles` (artifacts are visited, i.e. numbered, before the set is emitted).
fn push_set(ids: &mut Ids, dep_sets: &mut Vec<Msg>, id: u32, v: &[&Artifact], children: &[u32]) {
    let aids: Vec<u32> = v.iter().map(|x| ids.artifact_id(x)).collect();
    let mut ds = Msg::new();
    ds.add(1, "id", PbVal::Uint32(id));
    for c in children {
        ds.rep(2, "transitive_dep_set_ids", PbVal::Uint32(*c));
    }
    for aid in aids {
        ds.rep(3, "direct_artifact_ids", PbVal::Uint32(aid));
    }
    ids.events.push((4, "dep_set_of_files", ds.clone()));
    dep_sets.push(ds);
}

fn proto_output(entries: &[Entry], popts: &PostOpts, templates: &HashMap<String, String>) -> Result<Vec<u8>, String> {
    let mut ids = Ids { paths: HashMap::new(), path_msgs: Vec::new(), artifacts: HashMap::new(), artifact_msgs: Vec::new(), next_path_id: 1, next_artifact_id: 1, events: Vec::new() };
    let mut target_ids: HashMap<String, u32> = HashMap::new();
    let mut targets: Vec<Msg> = Vec::new();
    let mut class_ids: HashMap<String, u32> = HashMap::new();
    let mut classes: Vec<Msg> = Vec::new();
    let mut cfg_ids: HashMap<String, u32> = HashMap::new();
    let mut cfgs: Vec<Msg> = Vec::new();
    let mut dep_sets: Vec<Msg> = Vec::new();
    let mut dep_set_next: u32 = 1;
    let mut aspect_descriptors: Vec<Msg> = Vec::new();
    let mut aspect_desc_ids: HashMap<String, u32> = HashMap::new();
    let mut actions: Vec<Msg> = Vec::new();
    let params = param_files(entries);
    for e in entries {
        let a = &e.action;
        let label = e.owner.to_string();
        let class_id = *class_ids.entry(e.owner_kind.clone()).or_insert_with(|| {
            let id = classes.len() as u32 + 1;
            let mut m = Msg::new();
            m.add(1, "id", PbVal::Uint32(id));
            m.str(2, "name", e.owner_kind.trim_end_matches(" rule").to_string());
            ids.events.push((7, "rule_classes", m.clone()));
            classes.push(m);
            id
        });
        let target_id = *target_ids.entry(label.clone()).or_insert_with(|| {
            let id = targets.len() as u32 + 1;
            let mut m = Msg::new();
            m.add(1, "id", PbVal::Uint32(id));
            // `Label.toString()`: canonical form, `//pkg:name` for the main repository.
            m.str(2, "label", { let c = e.owner.canonical_form(); c.strip_prefix("@@//").map(|r| format!("//{r}")).unwrap_or(c) });
            m.add(3, "rule_class_id", PbVal::Uint32(class_id));
            ids.events.push((3, "targets", m.clone()));
            targets.push(m);
            id
        });
        let cfg_id = *cfg_ids.entry(e.cfg_id.clone()).or_insert_with(|| {
            let id = cfgs.len() as u32 + 1;
            let mut m = Msg::new();
            m.add(1, "id", PbVal::Uint32(id));
            m.str(2, "mnemonic", e.cfg.mnemonic());
            m.str(3, "platform_name", e.cfg.cpu.clone());
            m.str(4, "checksum", e.cfg.checksum());
            if e.cfg.is_exec {
                m.bool(5, "is_tool", true);
            }
            ids.events.push((5, "configuration", m.clone()));
            cfgs.push(m);
            id
        });
        let mut m = Msg::new();
        m.add(1, "target_id", PbVal::Uint32(target_id));
        if let Some(asp) = &e.aspect {
            let asp_id = *aspect_desc_ids.entry(asp.clone()).or_insert_with(|| {
                let id = aspect_descriptors.len() as u32 + 1;
                let mut ad = Msg::new();
                ad.add(1, "id", PbVal::Uint32(id));
                ad.str(2, "name", asp.clone());
                ids.events.push((6, "aspect_descriptors", ad.clone()));
                aspect_descriptors.push(ad);
                id
            });
            m.rep(2, "aspect_descriptor_ids", PbVal::Uint32(asp_id));
        }
        m.str(3, "action_key", entry_key(e));
        m.str(4, "mnemonic", a.mnemonic.clone());
        m.add(5, "configuration_id", PbVal::Uint32(cfg_id));
        if (popts.include_commandline || popts.include_param_files)
            && !matches!(e.synth, Synth::SymlinkTree | Synth::RunfilesTree)
        {
            for arg in command_line(e) {
                m.rstr(6, "arguments", arg);
            }
        }
        let env = match &a.action_env {
            Some(ae) => ae.fixed_env().clone(),
            None => a.env.clone(),
        };
        if matches!(a.kind, ActionKind::Spawn { .. }) {
            let mut sorted: Vec<(String, String)> = env.into_iter().collect();
            sorted.sort();
            for (k, v) in sorted {
                m.rmsg(7, "environment_variables", kv(&k, &v));
            }
        }
        if popts.include_artifacts {
            if !a.inputs.is_empty() {
                // `KnownNestedSets`: the id of a set is allocated first, then its transitive sets
                // and its direct artifacts are visited (leaves keep the action's input order),
                // then the proto is streamed. Every action owns its input set.
                fn dedup<'x>(v: Vec<&'x Artifact>) -> Vec<&'x Artifact> {
                    let mut seen = std::collections::HashSet::new();
                    v.into_iter().filter(|x| seen.insert(x.exec_path.path_string().to_string())).collect()
                }
                let dsid = if a.mnemonic == "Genrule" && !a.tools.is_empty() || a.mnemonic == "Genrule" {
                    // `GenRuleBase`: inputs = {transitive: srcs, tools(+runfiles); direct: genrule-setup.sh}.
                    let is_setup = |x: &&Artifact| x.exec_path.path_string().ends_with("genrule-setup.sh");
                    let setup: Vec<&Artifact> = a.inputs.iter().filter(is_setup).collect();
                    let tool_paths: std::collections::HashSet<&str> = a.tools.iter().map(|t| t.exec_path.path_string()).collect();
                    let tools_set: Vec<&Artifact> = a
                        .tools
                        .iter()
                        .chain(a.inputs.iter().filter(|x| {
                            x.exec_path.path_string().ends_with(".runfiles") && !tool_paths.contains(x.exec_path.path_string())
                        }))
                        .collect();
                    let srcs: Vec<&Artifact> = a
                        .inputs
                        .iter()
                        .filter(|x| {
                            !tool_paths.contains(x.exec_path.path_string())
                                && !x.exec_path.path_string().ends_with(".runfiles")
                                && !x.exec_path.path_string().ends_with("genrule-setup.sh")
                        })
                        .collect();
                    let (srcs, tools_set, setup) = (dedup(srcs), dedup(tools_set), dedup(setup));
                    let n_children = usize::from(!srcs.is_empty()) + usize::from(!tools_set.is_empty());
                    if n_children == 0 || (setup.is_empty() && n_children == 1) {
                        let all: Vec<&Artifact> = dedup(srcs.into_iter().chain(tools_set).chain(setup).collect());
                        let id = dep_set_next;
                        dep_set_next += 1;
                        push_set(&mut ids, &mut dep_sets, id, &all, &[]);
                        id
                    } else {
                        let parent_id = dep_set_next;
                        dep_set_next += 1;
                        let mut children = Vec::new();
                        for set in [&srcs, &tools_set] {
                            if !set.is_empty() {
                                let cid = dep_set_next;
                                dep_set_next += 1;
                                push_set(&mut ids, &mut dep_sets, cid, set, &[]);
                                children.push(cid);
                            }
                        }
                        push_set(&mut ids, &mut dep_sets, parent_id, &setup, &children);
                        parent_id
                    }
                } else {
                    let all = dedup(a.inputs.iter().collect());
                    let id = dep_set_next;
                    dep_set_next += 1;
                    push_set(&mut ids, &mut dep_sets, id, &all, &[]);
                    id
                };
                m.rep(8, "input_dep_set_ids", PbVal::Uint32(dsid));
            }
            for o in &a.outputs {
                let oid = ids.artifact_id(o);
                m.rep(9, "output_ids", PbVal::Uint32(oid));
            }
        }
        for (k, v) in &a.execution_requirements {
            m.rmsg(11, "execution_info", kv(k, v));
        }
        if popts.include_param_files {
            let (_, ps) = a.command_line();
            for (path, body) in ps {
                let mut pf = Msg::new();
                pf.str(1, "exec_path", path);
                for l in String::from_utf8_lossy(&body).lines() {
                    pf.rstr(2, "arguments", l.to_string());
                }
                m.rmsg(12, "param_files", pf);
            }
        }
        if popts.include_artifacts {
            if let Some(o) = a.outputs.first() {
                let id = ids.artifact_id(o);
                m.add(13, "primary_output_id", PbVal::Uint32(id));
            }
        }
        if let Some(p) = exec_platform(e) {
            m.str(14, "execution_platform", p);
        }
        match &a.kind {
            ActionKind::TemplateExpand { template, substitutions, computed_substitutions, is_executable } => {
                m.str(15, "template_content", template_content(template, templates));
                for (k, v) in substitutions.iter().chain(computed_substitutions.iter()).map(|(k, v)| (k, v)) {
                    m.rmsg(16, "substitutions", kv(k, v));
                }
                let _ = is_executable;
            }
            ActionKind::FileWrite { .. } if e.owner_kind.starts_with("genquery") => {}
            ActionKind::FileWrite { content: FileContent::Args { .. }, .. } => {}
            ActionKind::FileWrite { content, is_executable } => {
                if popts.include_file_write_contents {
                    m.str(17, "file_contents", String::from_utf8_lossy(&content.render()).to_string());
                }
                if *is_executable {
                    m.bool(19, "is_executable", true);
                }
            }
            ActionKind::Symlink { target_path: Some(p), .. } => {
                m.str(18, "unresolved_symlink_target", p.clone());
            }
            _ => {}
        }
        let _ = &params;
        ids.events.push((2, "actions", m.clone()));
        actions.push(m);
    }
    let mut c = Msg::new();
    for x in &ids.artifact_msgs {
        c.rmsg(1, "artifacts", x.clone());
    }
    for x in &actions {
        c.rmsg(2, "actions", x.clone());
    }
    for x in &targets {
        c.rmsg(3, "targets", x.clone());
    }
    for x in &dep_sets {
        c.rmsg(4, "dep_set_of_files", x.clone());
    }
    for x in &cfgs {
        c.rmsg(5, "configuration", x.clone());
    }
    for x in &aspect_descriptors {
        c.rmsg(6, "aspect_descriptors", x.clone());
    }
    for x in &classes {
        c.rmsg(7, "rule_classes", x.clone());
    }
    for x in &ids.path_msgs {
        c.rmsg(8, "path_fragments", x.clone());
    }
    Ok(match popts.output.as_str() {
        "proto" => c.encode(),
        "streamed_proto" => {
            let mut out = Vec::new();
            for (num, name, m) in &ids.events {
                let mut wrap = Msg::new();
                wrap.rmsg(*num, name, m.clone());
                out.extend(wrap.encode_delimited());
            }
            out
        }
        "textproto" => {
            // Streamed output handler: every proto is printed as soon as it is created.
            let mut s = String::new();
            for (_, name, m) in &ids.events {
                // `TextFormat` of the streamed message: the field name followed by the unindented body.
                s.push_str(&format!("{name} {{\n{}}}\n", m.to_text()));
            }
            s.into_bytes()
        }
        _ => {
            let mut s = c.to_json_pretty();
            s.push('\n');
            s.into_bytes()
        }
    })
}
