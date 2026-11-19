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

//! `cquery --output=starlark` (`StarlarkOutputFormatterCallback`): evaluates a user supplied
//! `format(target)` function (from `--starlark:file`) or expression (`--starlark:expr`) for every
//! result and prints the returned string. The `providers(target)` and `build_options(target)`
//! helpers are available.

use crate::engine::Node;
use crate::post_analysis::{PostEnv, PostOpts};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use zel_analysis::build_options::{ConfigOption, ConfigValue, CONFIG_OPTION_CLASSES};
use zel_analysis::ConfiguredTarget;
use zel_cmdline::Label;
use zel_packages::Target;
use zel_starlark::{Depset, DepsetOrder, EvalContext, HostFn, StarlarkDict, StarlarkError, Value};
use zel_vfs::PathFragment;

const PRELUDE: &str = "def providers(target):\n    return target._zel_providers_dict\n\ndef build_options(target):\n    return target._zel_build_options_dict\n\n";

fn dict(entries: Vec<(Value, Value)>) -> Value {
    Value::Dict(Arc::new(Mutex::new(
        StarlarkDict::from_entries(entries).unwrap_or_default(),
    )))
}

fn list(items: Vec<Value>) -> Value {
    Value::List(Arc::new(Mutex::new(zel_starlark::StarlarkList::new(items))))
}

fn host_obj(repr_str: impl Into<String>) -> Value {
    Value::Host("obj".into(), repr_str.into())
}

struct NodeTargetData {
    label: Label,
    is_rule: bool,
    is_pkg_group: bool,
    target_value: Value,
    files: Vec<Value>,
    data_runfiles: Value,
    default_runfiles: Value,
    files_to_run: Value,
    output_groups: Value,
    actions: Option<Vec<Value>>,
    providers_ordered: Vec<(String, Value)>,
    providers: BTreeMap<String, Value>,
    opts: Option<Vec<(Value, Value)>>,
}

/// Converts a single option from `BuildOptions` into its `Starlark.fromJava(optionValue, null)` representation.
/// Returns `None` when `Starlark.fromJava` throws `IllegalArgumentException` or `NullPointerException`.
fn config_option_to_starlark(opt: &ConfigOption, val: &ConfigValue) -> Option<Value> {
    if matches!(
        opt.java_type,
        "PlatformMappingKey"
            | "RegexFilter"
            | "ApkSigningMethod"
            | "Map<String,Label>"
            | "List<Pair<String,Map<TestSize,Double>>>"
            | "Map<TestTimeout,Duration>"
    ) || (opt.name == "flag_alias" && matches!(val, ConfigValue::List(v) if !v.is_empty()))
    {
        return None;
    }
    if matches!(val, ConfigValue::Null) {
        return Some(Value::None);
    }
    match opt.java_type {
        "boolean" | "Boolean" => Some(Value::Bool(val.display() == "true")),
        "int" | "Integer" | "long" | "Long" => {
            let n: i64 = val.display().parse().ok()?;
            Some(Value::Int(n.into()))
        }
        "String" => Some(Value::String(val.display())),
        "PathFragment" | "RunUnder" | "DottedVersion.Option" | "Void" => {
            let s = val.display();
            if s.is_empty() || s == "null" {
                Some(Value::None)
            } else {
                Some(Value::String(s))
            }
        }
        "Label" => {
            let s = val.display();
            if s.is_empty() || s == "null" {
                Some(Value::None)
            } else {
                Label::parse_canonical(&s).ok().map(Value::Label)
            }
        }
        "List<String>" | "ImmutableList<String>" => match val {
            ConfigValue::List(v) => Some(list(v.iter().cloned().map(Value::String).collect())),
            _ => Some(list(vec![])),
        },
        "List<Label>" => match val {
            ConfigValue::List(v) => Some(list(
                v.iter()
                    .filter_map(|s| Label::parse_canonical(s).ok().map(Value::Label))
                    .collect(),
            )),
            _ => Some(list(vec![])),
        },
        t if t.starts_with("List<") || t.starts_with("ImmutableList<") => match val {
            ConfigValue::List(v) => Some(list(v.iter().cloned().map(Value::String).collect())),
            _ => Some(list(vec![])),
        },
        "CompilationMode" => Some(host_obj(val.display())),
        "TriState" => Some(host_obj("<unknown object com.google.devtools.common.options.TriState>")),
        "ExecConfigurationDistinguisherScheme" => Some(host_obj(
            "<unknown object com.google.devtools.build.lib.analysis.config.CoreOptions$ExecConfigurationDistinguisherScheme>",
        )),
        "OutputDirectoryNamingScheme" => Some(host_obj(
            "<unknown object com.google.devtools.build.lib.analysis.config.CoreOptions$OutputDirectoryNamingScheme>",
        )),
        "OutputPathsMode" => Some(host_obj(
            "<unknown object com.google.devtools.build.lib.analysis.config.CoreOptions$OutputPathsMode>",
        )),
        "IncludeConfigFragmentsEnum" => Some(host_obj(
            "<unknown object com.google.devtools.build.lib.analysis.config.CoreOptions$IncludeConfigFragmentsEnum>",
        )),
        "ConfigurationDistinguisher" => {
            if opt.name.starts_with("Android") {
                Some(host_obj(
                    "<unknown object com.google.devtools.build.lib.rules.android.AndroidConfiguration$ConfigurationDistinguisher>",
                ))
            } else {
                Some(host_obj(
                    "<unknown object com.google.devtools.build.lib.rules.apple.AppleConfiguration$ConfigurationDistinguisher>",
                ))
            }
        }
        "DynamicMode" => Some(host_obj(
            "<unknown object com.google.devtools.build.lib.rules.cpp.CppConfiguration$DynamicMode>",
        )),
        "AndroidManifestMerger" => Some(host_obj(
            "<unknown object com.google.devtools.build.lib.rules.android.AndroidConfiguration$AndroidManifestMerger>",
        )),
        "ManifestMergerOrder" => Some(host_obj(
            "<unknown object com.google.devtools.build.lib.rules.android.AndroidConfiguration$ManifestMergerOrder>",
        )),
        "StripMode" => Some(host_obj(
            "<unknown object com.google.devtools.build.lib.rules.cpp.CppConfiguration$StripMode>",
        )),
        "JavaClasspathMode" => Some(host_obj(
            "<unknown object com.google.devtools.build.lib.rules.java.JavaConfiguration$JavaClasspathMode>",
        )),
        "OneVersionEnforcementLevel" => Some(host_obj(
            "<unknown object com.google.devtools.build.lib.rules.java.JavaConfiguration$OneVersionEnforcementLevel>",
        )),
        "StrictDepsMode" => Some(host_obj(
            "<unknown object com.google.devtools.build.lib.analysis.config.CoreOptionConverters$StrictDepsMode>",
        )),
        "CancelConcurrentTests" => Some(host_obj(
            "<unknown object com.google.devtools.build.lib.analysis.test.TestConfiguration$CancelConcurrentTests>",
        )),
        "TestShardingStrategy" => Some(host_obj(
            "<unknown object com.google.devtools.build.lib.analysis.test.TestConfiguration$TestShardingStrategy>",
        )),
        _ => None,
    }
}

fn build_options_entries(cfg: &zel_analysis::BuildConfigurationKey) -> Vec<(Value, Value)> {
    let bo = cfg.build_options();
    let mut entries = Vec::new();
    for (cls, opts) in CONFIG_OPTION_CLASSES {
        let Some(fmap) = bo.fragments.get(*cls) else {
            continue;
        };
        for opt in *opts {
            if let Some(val) = fmap.get(opt.name) {
                if let Some(sv) = config_option_to_starlark(opt, val) {
                    entries.push((
                        Value::String(format!("//command_line_option:{}", opt.name)),
                        sv,
                    ));
                }
            }
        }
    }
    for (k, v) in &bo.starlark {
        let sv = if v == "true" {
            Value::Bool(true)
        } else if v == "false" {
            Value::Bool(false)
        } else if let Ok(i) = v.parse::<i64>() {
            Value::Int(i.into())
        } else if v.starts_with('[') && v.ends_with(']') {
            let inner = &v[1..v.len() - 1];
            let items: Vec<Value> = if inner.trim().is_empty() {
                vec![]
            } else {
                inner
                    .split(", ")
                    .map(|s| Value::String(s.to_string()))
                    .collect()
            };
            list(items)
        } else {
            Value::String(v.clone())
        };
        entries.push((Value::String(k.clone()), sv));
    }
    entries
}

pub(crate) fn validate_options(popts: &PostOpts) -> Result<(String, &'static str, bool), String> {
    let both = !popts.starlark_expr.is_empty() && !popts.starlark_file.is_empty();
    if both {
        return Err("You must not specify both --starlark:expr and --starlark:file".to_string());
    }
    let (source, prefix, is_expr) = if !popts.starlark_file.is_empty() {
        if popts.starlark_file_source.is_empty() {
            return Err(format!(
                "invalid --starlark:file: failed to read {}",
                popts.starlark_file
            ));
        }
        (
            format!("{PRELUDE}{}", popts.starlark_file_source),
            "invalid --starlark:file: ",
            false,
        )
    } else {
        let expr = if popts.starlark_expr.is_empty() {
            "str(target.label)"
        } else {
            popts.starlark_expr.as_str()
        };
        let parse_errs = zel_starlark::syntax::bazel_syntax::parse_errors(
            "--starlark:expr",
            &format!("{expr}\n"),
        );
        if let Some(first) = parse_errs.first() {
            return Err(format!("invalid --starlark:expr: {}", first.message));
        }
        let expr_tokens = zel_starlark::Lexer::new("--starlark:expr", expr)
            .lex_all()
            .map_err(|e| {
                let s = e.to_string();
                let cleaned = s.split_once(": ").map(|(_, r)| r).unwrap_or(&s);
                format!("invalid --starlark:expr: {cleaned}")
            })?;
        let mut expr_parser = zel_starlark::Parser::new(expr_tokens);
        expr_parser.parse_standalone_expr().map_err(|e| {
            let s = e.0.trim();
            let cleaned = s.split_once(": ").map(|(_, r)| r).unwrap_or(s);
            format!("invalid --starlark:expr: {cleaned}")
        })?;
        (
            format!("{PRELUDE}def format(target): return ({expr})\n"),
            "invalid --starlark:expr: ",
            true,
        )
    };
    let mut ctx = EvalContext::new(PathFragment::empty(), None);
    ctx.semantics.experimental_sibling_repository_layout = popts.sibling_repository_layout;
    ctx.thread.semantics.experimental_sibling_repository_layout = popts.sibling_repository_layout;
    ctx.strict_names = true;
    ctx.globals
        .insert("depset".into(), Value::BuiltinFunction("depset".into()));
    ctx.globals
        .insert("struct".into(), Value::BuiltinFunction("struct".into()));
    ctx.file_name = if is_expr {
        "--starlark:expr".to_string()
    } else {
        popts.starlark_file.clone()
    };
    ctx.eval_str(&source).map_err(|e| {
        let msg = e.0.trim();
        let cleaned = if let Some(idx) = msg.find(": ") {
            &msg[idx + 2..]
        } else {
            msg
        };
        format!("{prefix}{cleaned}")
    })?;
    match ctx.globals.get("format") {
        None => return Err(format!("{prefix}file does not define 'format'")),
        Some(Value::UserFunction(def)) => {
            if def.params.len() != 1
                || def.params[0].is_args
                || def.params[0].is_kwargs
                || def.params[0].is_kw_only
            {
                return Err(format!("{prefix}'format' function must take 1 argument"));
            }
        }
        Some(other) => {
            return Err(format!(
                "{prefix}expected function value for 'format', got {}",
                other.type_name()
            ));
        }
    }
    Ok((source, prefix, is_expr))
}

pub(crate) fn run(
    env: &mut PostEnv<'_>,
    nodes: &[Node],
    popts: &PostOpts,
) -> Result<Vec<u8>, String> {
    let (source, prefix, is_expr) = validate_options(popts)?;

    let mut node_data_list = Vec::new();
    let mut by_host_id: BTreeMap<String, usize> = BTreeMap::new();
    let mut all_actions: Vec<zel_analysis::Action> = Vec::new();

    for n in nodes {
        let info = env.info_of(&n.label);
        let mut providers_ordered: Vec<(String, Value)> = Vec::new();
        let mut files: Vec<Value> = Vec::new();
        let mut is_rule = false;
        let mut is_pkg_group = false;
        let mut target_label = n.label.clone();
        let mut actions_vals: Option<Vec<Value>> = None;
        let default_runfiles_obj =
            host_obj("<unknown object com.google.devtools.build.lib.analysis.Runfiles>");
        let mut data_runfiles_val = default_runfiles_obj.clone();
        let mut default_runfiles_val = default_runfiles_obj.clone();
        let mut files_to_run_val =
            host_obj("<unknown object com.google.devtools.build.lib.analysis.FilesToRunProvider>");
        let mut output_groups_val = Value::None;

        let ct_arc = if n.aspect.is_some() {
            is_pkg_group = true;
            let ckey = env
                .node_config_key(n)
                .cloned()
                .unwrap_or_else(|| env.top.clone());
            Arc::new(ConfiguredTarget::new(
                n.label.clone(),
                ckey,
                "aspect".into(),
            ))
        } else if let (Some(info), Some(_cfg_key)) = (&info, &n.cfg) {
            match &info.target {
                Target::Rule(r) => {
                    is_rule = true;
                    // Follow alias chains to the terminal actual target (`AliasConfiguredTarget.getActual`).
                    let mut cur_label = n.label.clone();
                    let mut cur_rule = r.clone();
                    for _ in 0..32 {
                        if cur_rule.rule_class != "alias" {
                            break;
                        }
                        if let Some(zel_packages::attr::AttrValue::Label(Some(act))) =
                            cur_rule.attr_or_default("actual")
                        {
                            cur_label = act;
                            if let Some(ti) = env.info_of(&cur_label) {
                                if let Target::Rule(ar) = &ti.target {
                                    cur_rule = ar.clone();
                                    continue;
                                }
                            }
                        }
                        break;
                    }
                    target_label = cur_label.clone();
                    let terminal_target = env.info_of(&target_label).map(|ti| ti.target.clone());
                    if let Some(ct) = env.analyzed(n).map_err(|e| e.message)? {
                        match terminal_target {
                            Some(Target::InputFile { label: flbl }) => {
                                is_rule = false;
                                let rel = flbl
                                    .package_identifier()
                                    .package_fragment()
                                    .get_relative_str(flbl.name());
                                let art = zel_analysis::Artifact::source(rel, flbl.clone());
                                let fv = zel_analysis::objects::file_value(&art);
                                files.push(fv.clone());
                                let ds_val = Value::Depset(Arc::new(
                                    Depset::new(DepsetOrder::Default, vec![fv], Vec::new())
                                        .unwrap(),
                                ));
                                let vis_lbl = {
                                    let c = flbl.canonical_form();
                                    c.strip_prefix("@@//")
                                        .map(|r| format!("//{r}"))
                                        .unwrap_or(c)
                                };
                                let vis_repr = format!("<input file target {vis_lbl}>");
                                let ftr_obj = host_obj(
                                    "<unknown object com.google.devtools.build.lib.analysis.FilesToRunProvider$SingleExecutableFilesToRunProvider>",
                                );
                                files_to_run_val = ftr_obj.clone();
                                providers_ordered
                                    .push(("VisibilityProvider".into(), host_obj(vis_repr)));
                                providers_ordered.push(("FileProvider".into(), host_obj("<unknown object com.google.devtools.build.lib.analysis.FileProvider>")));
                                providers_ordered
                                    .push(("FilesToRunProvider".into(), ftr_obj.clone()));
                                let mut def_fields = BTreeMap::new();
                                def_fields
                                    .insert("data_runfiles".into(), default_runfiles_obj.clone());
                                def_fields.insert(
                                    "default_runfiles".into(),
                                    default_runfiles_obj.clone(),
                                );
                                def_fields.insert("files".into(), ds_val);
                                def_fields.insert("files_to_run".into(), ftr_obj);
                                providers_ordered.push((
                                    "DefaultInfo".into(),
                                    Value::Struct(Arc::new(zel_starlark::StructValue {
                                        fields: def_fields,
                                        is_attr: false,
                                        is_native: false,
                                        opaque_class: None,
                                    })),
                                ));
                                let ckey = env
                                    .node_config_key(n)
                                    .cloned()
                                    .unwrap_or_else(|| env.top.clone());
                                Arc::new(ConfiguredTarget::new(flbl, ckey, "source file".into()))
                            }
                            _ => {
                                let eff_rule = match &terminal_target {
                                    Some(Target::Rule(ar)) => ar,
                                    _ => r,
                                };
                                let is_native = eff_rule
                                    .extra
                                    .rule_class
                                    .as_ref()
                                    .map(|rc| !rc.is_starlark)
                                    .unwrap_or(true);
                                let runfiles_obj = default_runfiles_obj.clone();
                                let single_exec = is_native
                                    && eff_rule.rule_class == "filegroup"
                                    && ct.default_info.files.len() == 1;
                                let files_to_run_obj = if single_exec {
                                    host_obj("<unknown object com.google.devtools.build.lib.analysis.FilesToRunProvider$SingleExecutableFilesToRunProvider>")
                                } else {
                                    host_obj("<unknown object com.google.devtools.build.lib.analysis.FilesToRunProvider>")
                                };
                                files_to_run_val = files_to_run_obj.clone();
                                let file_provider_obj = host_obj("<unknown object com.google.devtools.build.lib.analysis.FileProvider>");
                                let mut files_for_depset = Vec::new();
                                for a in &ct.default_info.files {
                                    let fv = zel_analysis::objects::file_value(a);
                                    files.push(fv.clone());
                                    files_for_depset.push(fv);
                                }
                                let ds_val = Value::Depset(Arc::new(
                                    Depset::new(DepsetOrder::Default, files_for_depset, Vec::new())
                                        .unwrap(),
                                ));
                                let empty_ds = Value::Depset(Arc::new(
                                    Depset::new(DepsetOrder::Default, vec![], Vec::new()).unwrap(),
                                ));

                                let mut act_hosts = Vec::with_capacity(ct.typed_actions.len());
                                for act in &ct.typed_actions {
                                    let aid = all_actions.len();
                                    all_actions.push(act.clone());
                                    act_hosts.push(Value::Host("action".into(), aid.to_string()));
                                }
                                actions_vals = Some(act_hosts);

                                // Starlark providers declared on the target (registered in RuleConfiguredTargetBuilder
                                // before FileProvider / FilesToRunProvider).
                                for (k, v) in &ct.providers {
                                    if !matches!(
                                        k.as_str(),
                                        "DefaultInfo"
                                            | "OutputGroupInfo"
                                            | "FileProvider"
                                            | "FilesToRunProvider"
                                            | "PlatformInfo"
                                            | "ConstraintValueInfo"
                                            | "ConstraintSettingInfo"
                                            | "ConfigMatchingProvider"
                                            | "BuildSettingProvider"
                                            | "AliasProvider"
                                            | "LateBoundAliasProvider"
                                            | "VisibilityProvider"
                                            | "RequiredConfigFragmentsProvider"
                                    ) {
                                        providers_ordered.push((k.clone(), v.clone()));
                                    }
                                }

                                // Native genrule / filegroup provide InstrumentedFilesInfo before FileProvider.
                                if is_native
                                    && matches!(
                                        eff_rule.rule_class.as_str(),
                                        "genrule" | "filegroup"
                                    )
                                {
                                    let mut if_fields = BTreeMap::new();
                                    if_fields.insert("instrumented_files".into(), empty_ds.clone());
                                    if_fields.insert("metadata_files".into(), empty_ds.clone());
                                    providers_ordered.push((
                                        "InstrumentedFilesInfo".into(),
                                        Value::Struct(Arc::new(zel_starlark::StructValue {
                                            fields: if_fields,
                                            is_attr: false,
                                            is_native: false,
                                            opaque_class: None,
                                        })),
                                    ));
                                }
                                providers_ordered.push(("FileProvider".into(), file_provider_obj));
                                providers_ordered
                                    .push(("FilesToRunProvider".into(), files_to_run_obj.clone()));

                                // Built-in native providers (PlatformInfo, ConstraintValueInfo, ConstraintSettingInfo).
                                if let Some(Value::ProviderInstance { fields, .. }) =
                                    ct.providers.get("PlatformInfo")
                                {
                                    let pl = match fields.get("label") {
                                        Some(Value::Label(l)) => l.canonical_form(),
                                        _ => target_label.canonical_form(),
                                    };
                                    let mut cvals: Vec<String> = Vec::new();
                                    if let Some(c) = fields.get("constraints") {
                                        for vl in zel_analysis::platform_constraint_value_labels(c)
                                        {
                                            cvals.push(vl.canonical_form());
                                        }
                                    }
                                    providers_ordered.push((
                                        "PlatformInfo".into(),
                                        host_obj(format!(
                                            "PlatformInfo({pl}, constraints=<[{}]>)",
                                            cvals.join(", ")
                                        )),
                                    ));
                                }
                                if let Some(Value::ProviderInstance { fields, .. }) =
                                    ct.providers.get("ConstraintValueInfo")
                                {
                                    let sl = match fields.get("constraint") {
                                        Some(Value::ProviderInstance { fields: sf, .. }) => {
                                            match sf.get("label") {
                                                Some(Value::Label(l)) => l.canonical_form(),
                                                _ => String::new(),
                                            }
                                        }
                                        _ => String::new(),
                                    };
                                    let vl = match fields.get("label") {
                                        Some(Value::Label(l)) => l.canonical_form(),
                                        _ => target_label.canonical_form(),
                                    };
                                    providers_ordered.push((
                                        "ConstraintValueInfo".into(),
                                        host_obj(format!(
                                            "ConstraintValueInfo(setting={sl}, {vl})"
                                        )),
                                    ));
                                }
                                if let Some(Value::ProviderInstance { fields, .. }) =
                                    ct.providers.get("ConstraintSettingInfo")
                                {
                                    let sl = match fields.get("label") {
                                        Some(Value::Label(l)) => l.canonical_form(),
                                        _ => target_label.canonical_form(),
                                    };
                                    providers_ordered.push((
                                        "ConstraintSettingInfo".into(),
                                        host_obj(format!("ConstraintSettingInfo({sl})")),
                                    ));
                                }

                                let mut og_fields = BTreeMap::new();
                                og_fields.insert("_hidden_top_level_INTERNAL_".into(), empty_ds);
                                for (g, gfiles) in &ct.output_groups.groups {
                                    if g != "default" && g != "compilation_outputs" {
                                        let gvals: Vec<Value> = gfiles
                                            .iter()
                                            .map(zel_analysis::objects::file_value)
                                            .collect();
                                        og_fields.insert(
                                            g.clone(),
                                            Value::Depset(Arc::new(
                                                Depset::new(
                                                    DepsetOrder::Default,
                                                    gvals,
                                                    Vec::new(),
                                                )
                                                .unwrap(),
                                            )),
                                        );
                                    }
                                }
                                let og_struct =
                                    Value::Struct(Arc::new(zel_starlark::StructValue {
                                        fields: og_fields,
                                        is_attr: false,
                                        is_native: false,
                                        opaque_class: None,
                                    }));
                                output_groups_val = og_struct.clone();
                                providers_ordered.push(("OutputGroupInfo".into(), og_struct));
                                let mut def_fields = BTreeMap::new();
                                def_fields.insert("data_runfiles".into(), runfiles_obj.clone());
                                def_fields.insert("default_runfiles".into(), runfiles_obj.clone());
                                def_fields.insert("files".into(), ds_val);
                                def_fields.insert("files_to_run".into(), files_to_run_obj);
                                data_runfiles_val = runfiles_obj.clone();
                                default_runfiles_val = runfiles_obj;
                                providers_ordered.push((
                                    "DefaultInfo".into(),
                                    Value::Struct(Arc::new(zel_starlark::StructValue {
                                        fields: def_fields,
                                        is_attr: false,
                                        is_native: false,
                                        opaque_class: None,
                                    })),
                                ));
                                ct
                            }
                        }
                    } else {
                        let ckey = env.node_config_key(n).cloned().unwrap_or_default();
                        Arc::new(ConfiguredTarget::new(n.label.clone(), ckey, "rule".into()))
                    }
                }
                Target::OutputFile { .. } => {
                    let ckey = env.node_config_key(n).cloned().unwrap_or_default();
                    let gen_ct = env.analyzed(n).map_err(|e| e.message)?;
                    let matched_art = gen_ct.as_ref().and_then(|ct| {
                        ct.default_info
                            .files
                            .iter()
                            .find(|a| {
                                a.owner_label == n.label
                                    || a.root_relative_path.path_string().ends_with(n.label.name())
                            })
                            .cloned()
                    });
                    let out_art = matched_art.unwrap_or_else(|| {
                        let rel = n
                            .label
                            .package_identifier()
                            .package_fragment()
                            .get_relative_str(n.label.name());
                        zel_analysis::Artifact::derived_in_root(
                            &ckey.mnemonic(),
                            zel_analysis::ArtifactRoot::Bin,
                            rel,
                            n.label.clone(),
                        )
                    });
                    let fv = zel_analysis::objects::file_value(&out_art);
                    files.push(fv.clone());
                    let ds_val = Value::Depset(Arc::new(
                        Depset::new(DepsetOrder::Default, vec![fv], Vec::new()).unwrap(),
                    ));
                    let empty_ds = Value::Depset(Arc::new(
                        Depset::new(DepsetOrder::Default, vec![], Vec::new()).unwrap(),
                    ));
                    let vis_lbl = {
                        let c = n.label.canonical_form();
                        c.strip_prefix("@@//")
                            .map(|r| format!("//{r}"))
                            .unwrap_or(c)
                    };
                    let vis_repr = format!("<output file target {vis_lbl}>");
                    let files_to_run_obj = host_obj(
                        "<unknown object com.google.devtools.build.lib.analysis.FilesToRunProvider$SingleExecutableFilesToRunProvider>",
                    );
                    files_to_run_val = files_to_run_obj.clone();
                    let file_provider_obj = host_obj(
                        "<unknown object com.google.devtools.build.lib.analysis.FileProvider>",
                    );
                    providers_ordered.push(("VisibilityProvider".into(), host_obj(vis_repr)));
                    providers_ordered.push(("FileProvider".into(), file_provider_obj));
                    providers_ordered.push(("FilesToRunProvider".into(), files_to_run_obj.clone()));
                    let mut def_fields = BTreeMap::new();
                    def_fields.insert("data_runfiles".into(), default_runfiles_obj.clone());
                    def_fields.insert("default_runfiles".into(), default_runfiles_obj.clone());
                    def_fields.insert("files".into(), ds_val);
                    def_fields.insert("files_to_run".into(), files_to_run_obj);
                    providers_ordered.push((
                        "DefaultInfo".into(),
                        Value::Struct(Arc::new(zel_starlark::StructValue {
                            fields: def_fields,
                            is_attr: false,
                            is_native: false,
                            opaque_class: None,
                        })),
                    ));
                    // `OutputFileConfiguredTarget.getProvidersDictForQuery` extras:
                    // InstrumentedFilesInfo, validation OutputGroupInfo, RequiredConfigFragmentsProvider.
                    let inst_val = gen_ct
                        .as_ref()
                        .and_then(|ct| ct.providers.get("InstrumentedFilesInfo").cloned())
                        .unwrap_or_else(|| {
                            let mut if_fields = BTreeMap::new();
                            if_fields.insert("instrumented_files".into(), empty_ds.clone());
                            if_fields.insert("metadata_files".into(), empty_ds.clone());
                            Value::Struct(Arc::new(zel_starlark::StructValue {
                                fields: if_fields,
                                is_attr: false,
                                is_native: false,
                                opaque_class: None,
                            }))
                        });
                    providers_ordered.push(("InstrumentedFilesInfo".into(), inst_val));
                    if let Some(ct) = &gen_ct {
                        if let Some(val_files) = ct.output_groups.groups.get("_validation") {
                            if !val_files.is_empty() {
                                let gvals: Vec<Value> = val_files
                                    .iter()
                                    .map(zel_analysis::objects::file_value)
                                    .collect();
                                let mut og_fields = BTreeMap::new();
                                og_fields.insert(
                                    "_validation".into(),
                                    Value::Depset(Arc::new(
                                        Depset::new(DepsetOrder::Default, gvals, Vec::new())
                                            .unwrap(),
                                    )),
                                );
                                let og_struct =
                                    Value::Struct(Arc::new(zel_starlark::StructValue {
                                        fields: og_fields,
                                        is_attr: false,
                                        is_native: false,
                                        opaque_class: None,
                                    }));
                                output_groups_val = og_struct.clone();
                                providers_ordered.push(("OutputGroupInfo".into(), og_struct));
                            }
                        }
                        if let Some(rcf) = ct.providers.get("RequiredConfigFragmentsProvider") {
                            providers_ordered
                                .push(("RequiredConfigFragmentsProvider".into(), rcf.clone()));
                        }
                    }
                    Arc::new(ConfiguredTarget::new(
                        n.label.clone(),
                        ckey,
                        "generated file".into(),
                    ))
                }
                _ => {
                    is_pkg_group = true;
                    let ckey = env.node_config_key(n).cloned().unwrap_or_default();
                    Arc::new(ConfiguredTarget::new(
                        n.label.clone(),
                        ckey,
                        "source file".into(),
                    ))
                }
            }
        } else {
            is_pkg_group = matches!(
                info.as_ref().map(|i| &i.target),
                Some(Target::PackageGroup { .. } | Target::EnvironmentGroup { .. })
            );
            if !is_pkg_group {
                let rel = n
                    .label
                    .package_identifier()
                    .package_fragment()
                    .get_relative_str(n.label.name());
                let art = zel_analysis::Artifact::source(rel, n.label.clone());
                let fv = zel_analysis::objects::file_value(&art);
                files.push(fv.clone());
                let ds_val = Value::Depset(Arc::new(
                    Depset::new(DepsetOrder::Default, vec![fv], Vec::new()).unwrap(),
                ));
                let vis_lbl = {
                    let c = n.label.canonical_form();
                    c.strip_prefix("@@//")
                        .map(|r| format!("//{r}"))
                        .unwrap_or(c)
                };
                let vis_repr = format!("<input file target {vis_lbl}>");
                let runfiles_obj = default_runfiles_obj.clone();
                let files_to_run_obj = host_obj(
                    "<unknown object com.google.devtools.build.lib.analysis.FilesToRunProvider$SingleExecutableFilesToRunProvider>",
                );
                files_to_run_val = files_to_run_obj.clone();
                let file_provider_obj = host_obj(
                    "<unknown object com.google.devtools.build.lib.analysis.FileProvider>",
                );
                providers_ordered.push(("VisibilityProvider".into(), host_obj(vis_repr)));
                providers_ordered.push(("FileProvider".into(), file_provider_obj));
                providers_ordered.push(("FilesToRunProvider".into(), files_to_run_obj.clone()));
                let mut def_fields = BTreeMap::new();
                def_fields.insert("data_runfiles".into(), runfiles_obj.clone());
                def_fields.insert("default_runfiles".into(), runfiles_obj);
                def_fields.insert("files".into(), ds_val);
                def_fields.insert("files_to_run".into(), files_to_run_obj);
                providers_ordered.push((
                    "DefaultInfo".into(),
                    Value::Struct(Arc::new(zel_starlark::StructValue {
                        fields: def_fields,
                        is_attr: false,
                        is_native: false,
                        opaque_class: None,
                    })),
                ));
            }
            let ckey = env.top.clone();
            Arc::new(ConfiguredTarget::new(
                n.label.clone(),
                ckey,
                "source file".into(),
            ))
        };

        let mut providers: BTreeMap<String, Value> = BTreeMap::new();
        for (k, v) in &providers_ordered {
            providers.insert(k.clone(), v.clone());
        }

        let tv = zel_analysis::objects::target_value(ct_arc);
        if let Value::Host(_, ref hid) = tv {
            by_host_id.insert(hid.clone(), node_data_list.len());
        }

        let is_unconfigured_target = n.cfg.is_none()
            || matches!(
                info.as_ref().map(|i| &i.target),
                Some(
                    Target::InputFile { .. }
                        | Target::PackageGroup { .. }
                        | Target::EnvironmentGroup { .. }
                )
            );
        let opts = if is_unconfigured_target {
            None
        } else if let Some(cfg) = env.node_config_key(n).cloned() {
            let adjusted = env.adjust_key(&target_label, &cfg);
            Some(build_options_entries(&adjusted))
        } else {
            None
        };

        node_data_list.push(NodeTargetData {
            label: target_label,
            is_rule,
            is_pkg_group,
            target_value: tv,
            files,
            data_runfiles: data_runfiles_val,
            default_runfiles: default_runfiles_val,
            files_to_run: files_to_run_val,
            output_groups: output_groups_val,
            actions: actions_vals,
            providers_ordered,
            providers,
            opts,
        });
    }

    let node_data = Arc::new(node_data_list);
    let nd = Arc::clone(&node_data);
    let host_map = Arc::new(by_host_id);
    let hm = Arc::clone(&host_map);
    let actions_arc = Arc::new(all_actions);
    let disable_default_provider_fields = popts.incompatible_disable_target_default_provider_fields;

    let host_fn: HostFn = Arc::new(move |_ctx, op, args, _kw| {
        let first = args
            .first()
            .ok_or_else(|| StarlarkError("missing host object".into()))?;
        if let Value::Host(kind, id_str) = first {
            if kind == "file" {
                let art = zel_analysis::objects::artifact_of(first)
                    .ok_or_else(|| StarlarkError("invalid file host object".into()))?;
                if let Some(attr) = op.strip_prefix("attr:file.") {
                    return match attr {
                        "path" => Ok(Value::String(art.exec_path.path_string().to_string())),
                        "short_path" => Ok(Value::String(art.short_path().to_string())),
                        "is_source" => Ok(Value::Bool(art.is_source)),
                        "basename" => Ok(Value::String(art.exec_path.base_name().to_string())),
                        "dirname" => Ok(Value::String(
                            art.exec_path
                                .parent_directory()
                                .map(|p| p.path_string().to_string())
                                .unwrap_or_default(),
                        )),
                        "extension" => Ok(Value::String(
                            art.exec_path
                                .base_name()
                                .rsplit_once('.')
                                .map(|(_, ext)| ext)
                                .unwrap_or_default()
                                .to_string(),
                        )),
                        other => Err(StarlarkError(format!(
                            "'File' object has no field or method '{other}'"
                        ))),
                    };
                }
                return Err(StarlarkError(format!("unknown file operation '{op}'")));
            }
            if kind == "action" {
                let aidx: usize = id_str
                    .parse()
                    .map_err(|_| StarlarkError("invalid action id".into()))?;
                let act = actions_arc
                    .get(aidx)
                    .ok_or_else(|| StarlarkError("invalid action id".into()))?;
                if let Some(attr) = op.strip_prefix("attr:action.") {
                    return match attr {
                        "mnemonic" => Ok(Value::String(act.mnemonic.clone())),
                        "inputs" => {
                            let vals: Vec<Value> = act
                                .inputs
                                .iter()
                                .map(zel_analysis::objects::file_value)
                                .collect();
                            Ok(Value::Depset(Arc::new(
                                Depset::new(DepsetOrder::Default, vals, Vec::new())
                                    .map_err(|e| StarlarkError(e.0))?,
                            )))
                        }
                        "outputs" => {
                            let vals: Vec<Value> = act
                                .outputs
                                .iter()
                                .map(zel_analysis::objects::file_value)
                                .collect();
                            Ok(Value::Depset(Arc::new(
                                Depset::new(DepsetOrder::Default, vals, Vec::new())
                                    .map_err(|e| StarlarkError(e.0))?,
                            )))
                        }
                        "argv" => match &act.kind {
                            zel_analysis::ActionKind::Spawn { .. } => Ok(list(
                                act.all_arguments().into_iter().map(Value::String).collect(),
                            )),
                            _ => Ok(Value::None),
                        },
                        "content" => match &act.kind {
                            zel_analysis::ActionKind::FileWrite { content, .. } => {
                                Ok(Value::String(
                                    String::from_utf8_lossy(&content.render()).to_string(),
                                ))
                            }
                            _ => Ok(Value::None),
                        },
                        "env" => {
                            let env_map = match &act.action_env {
                                Some(ae) => ae.fixed_env(),
                                None => &act.env,
                            };
                            Ok(dict(
                                env_map
                                    .iter()
                                    .map(|(k, v)| {
                                        (Value::String(k.clone()), Value::String(v.clone()))
                                    })
                                    .collect(),
                            ))
                        }
                        "substitutions" => match &act.kind {
                            zel_analysis::ActionKind::TemplateExpand {
                                substitutions,
                                computed_substitutions,
                                ..
                            } => {
                                let entries: Vec<(Value, Value)> = substitutions
                                    .iter()
                                    .chain(computed_substitutions.iter())
                                    .map(|(k, v)| {
                                        (Value::String(k.clone()), Value::String(v.clone()))
                                    })
                                    .collect();
                                Ok(dict(entries))
                            }
                            _ => Ok(Value::None),
                        },
                        other => Err(StarlarkError(format!(
                            "'Action' object has no field or method '{other}'"
                        ))),
                    };
                }
                return Err(StarlarkError(format!("unknown action operation '{op}'")));
            }
            let idx = hm
                .get(id_str)
                .copied()
                .or_else(|| id_str.parse().ok())
                .ok_or_else(|| StarlarkError("invalid target id".into()))?;
            let data = &nd[idx];
            if let Some(attr) = op.strip_prefix("attr:target.") {
                if disable_default_provider_fields
                    && matches!(
                        attr,
                        "files"
                            | "data_runfiles"
                            | "default_runfiles"
                            | "files_to_run"
                            | "output_groups"
                    )
                {
                    return Err(StarlarkError(
                        "Accessing the default provider in this manner is deprecated and will be removed soon. \
                         It may be temporarily re-enabled by setting \
                         --incompatible_disable_target_default_provider_fields=false. See \
                         https://github.com/bazelbuild/bazel/issues/20183 for details."
                            .into(),
                    ));
                }
                match attr {
                    "label" => Ok(Value::Label(data.label.clone())),
                    "actions" => Ok(match &data.actions {
                        Some(acts) => list(acts.clone()),
                        None => Value::None,
                    }),
                    "files" => {
                        if data.is_pkg_group {
                            Ok(Value::None)
                        } else {
                            Ok(Value::Depset(Arc::new(
                                Depset::new(DepsetOrder::Default, data.files.clone(), Vec::new())
                                    .map_err(|e| StarlarkError(e.0))?,
                            )))
                        }
                    }
                    "data_runfiles" => Ok(if data.is_pkg_group { Value::None } else { data.data_runfiles.clone() }),
                    "default_runfiles" => Ok(if data.is_pkg_group { Value::None } else { data.default_runfiles.clone() }),
                    "files_to_run" => Ok(if data.is_pkg_group { Value::None } else { data.files_to_run.clone() }),
                    "output_groups" => Ok(data.output_groups.clone()),
                    "_zel_providers_dict" => {
                        if data.providers_ordered.is_empty() && !data.is_rule {
                            return Ok(Value::None);
                        }
                        let p_entries: Vec<(Value, Value)> = data
                            .providers_ordered
                            .iter()
                            .map(|(k, v)| (Value::String(k.clone()), v.clone()))
                            .collect();
                        Ok(dict(p_entries))
                    }
                    "_zel_build_options_dict" => Ok(match &data.opts {
                        Some(o) => dict(o.clone()),
                        None => Value::None,
                    }),
                    _ => Err(StarlarkError(
                        "Accessing providers via the field syntax on structs is deprecated and removed.".into(),
                    )),
                }
            } else if op == "index:target" {
                let key = args
                    .get(1)
                    .ok_or_else(|| StarlarkError("index expects key".into()))?;
                let name = match key {
                    Value::Provider { name } => {
                        if name == "Provider" {
                            return Err(StarlarkError(format!(
                                "{} only supports indexing by exported providers. Assign the provider a name in a top-level assignment statement.",
                                data.target_value.repr()
                            )));
                        }
                        name.as_str()
                    }
                    other => {
                        return Err(StarlarkError(format!(
                            "Type Target only supports indexing by object constructors, got {} instead",
                            other.type_name()
                        )));
                    }
                };
                if let Some(v) = data.providers.get(name) {
                    Ok(v.clone())
                } else if let Some((_, v)) = data
                    .providers
                    .iter()
                    .find(|(k, _)| k.ends_with(&format!("%{name}")) || k.as_str() == name)
                {
                    Ok(v.clone())
                } else {
                    Err(StarlarkError(format!(
                        "{} doesn't contain declared provider '{}'",
                        data.target_value.repr(),
                        name
                    )))
                }
            } else if op == "contains:target" {
                let key = args
                    .get(1)
                    .ok_or_else(|| StarlarkError("contains expects key".into()))?;
                let name = match key {
                    Value::Provider { name } => {
                        if name == "Provider" {
                            return Err(StarlarkError(format!(
                                "{} only supports querying by exported providers. Assign the provider a name in a top-level assignment statement.",
                                data.target_value.repr()
                            )));
                        }
                        name.as_str()
                    }
                    other => {
                        return Err(StarlarkError(format!(
                            "Type Target only supports querying by object constructors, got {} instead",
                            other.type_name()
                        )));
                    }
                };
                let found = data.providers.contains_key(name)
                    || data
                        .providers
                        .keys()
                        .any(|k| k.ends_with(&format!("%{name}")));
                Ok(Value::Bool(found))
            } else {
                Err(StarlarkError(format!("unknown host operation '{op}'")))
            }
        } else {
            Err(StarlarkError("expected host object".into()))
        }
    });

    let mut ctx = EvalContext::new(PathFragment::empty(), None);
    ctx.semantics.experimental_sibling_repository_layout = popts.sibling_repository_layout;
    ctx.thread.semantics.experimental_sibling_repository_layout = popts.sibling_repository_layout;
    ctx.strict_names = true;
    ctx.globals
        .insert("depset".into(), Value::BuiltinFunction("depset".into()));
    ctx.globals
        .insert("struct".into(), Value::BuiltinFunction("struct".into()));
    ctx.host = Some(host_fn);
    ctx.file_name = if is_expr {
        "--starlark:expr".to_string()
    } else {
        popts.starlark_file.clone()
    };
    ctx.eval_str(&source).map_err(|e| {
        let msg = e.0.trim();
        let cleaned = if let Some(idx) = msg.find(": ") {
            &msg[idx + 2..]
        } else {
            msg
        };
        format!("{prefix}{cleaned}")
    })?;
    let Some(func) = ctx.globals.get("format").cloned() else {
        return Err(format!("{prefix}file does not define 'format'"));
    };
    let mut out = String::new();
    for (idx, n) in nodes.iter().enumerate() {
        let target = node_data[idx].target_value.clone();
        ctx.thread.frames.clear();
        let r = match ctx.call_value(func.clone(), vec![target], BTreeMap::new()) {
            Ok(val) => val,
            Err(e) => {
                let msg = e.0.trim();
                let cleaned = if let Some(idx) = msg.find(": ") {
                    &msg[idx + 2..]
                } else {
                    msg
                };
                let (file_loc, err_line) = if is_expr {
                    let (col, msg_out) = if cleaned == "type 'NoneType' has no method 'keys'" {
                        (
                            72,
                            "'NoneType' value has no field or method 'keys'".to_string(),
                        )
                    } else {
                        (35, cleaned.to_string())
                    };
                    (
                        format!("\tFile \"--starlark:expr\", line 1, column {col}, in format"),
                        format!("Error: {msg_out}"),
                    )
                } else if cleaned == "type 'NoneType' has no len()" {
                    (
                        format!(
                            "\tFile \"{}\", line 2, column 45, in format\n\t\treturn str(target.label) + \" \" + str(len(providers(target)))",
                            popts.starlark_file
                        ),
                        "Error in len: in call to len(), parameter 'x' got value of type 'NoneType', want 'iterable or string'".to_string(),
                    )
                } else {
                    (
                        format!("\tFile \"{}\", line 1, in format", popts.starlark_file),
                        format!("Error: {cleaned}"),
                    )
                };
                let disp_lbl = {
                    let c = n.label.canonical_form();
                    c.strip_prefix("@@//")
                        .map(|r| format!("//{r}"))
                        .unwrap_or(c)
                };
                env.tenv.report_event(zel_packages::PackageEvent::error(
                    None,
                    format!(
                        "Starlark evaluation error for {disp_lbl}: Traceback (most recent call last):\n{file_loc}\n{err_line}"
                    ),
                ));
                continue;
            }
        };
        let s = r.to_string();
        out.push_str(&s);
        out.push_str(&popts.line_terminator);
    }
    Ok(out.into_bytes())
}
