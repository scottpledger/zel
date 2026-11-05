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
use zel_analysis::ConfiguredTarget;
use zel_packages::Target;
use zel_starlark::{Depset, DepsetOrder, EvalContext, HostFn, StarlarkDict, StarlarkError, Value};
use zel_vfs::PathFragment;

const PRELUDE: &str = "def providers(target):\n    return target.providers_dict\n\ndef build_options(target):\n    return target.build_options_dict\n\n";

fn dict(entries: Vec<(Value, Value)>) -> Value {
    Value::Dict(Arc::new(Mutex::new(StarlarkDict::from_entries(entries).unwrap_or_default())))
}

fn list(items: Vec<Value>) -> Value {
    Value::List(Arc::new(Mutex::new(zel_starlark::StarlarkList::new(items))))
}

struct NodeTargetData {
    label: zel_cmdline::Label,
    target_value: Value,
    files: Vec<Value>,
    providers: BTreeMap<String, Value>,
    opts: Option<Vec<(Value, Value)>>,
}

pub(crate) fn run(env: &mut PostEnv<'_>, nodes: &[Node], popts: &PostOpts) -> Result<Vec<u8>, String> {
    let both = !popts.starlark_expr.is_empty() && !popts.starlark_file.is_empty();
    if both {
        return Err("You must not specify both --starlark:expr and --starlark:file".to_string());
    }

    let (source, prefix) = if !popts.starlark_expr.is_empty() {
        (
            format!("{PRELUDE}def format(target):\n    return ({})\n", popts.starlark_expr),
            "invalid --starlark:expr: ",
        )
    } else if !popts.starlark_file_source.is_empty() {
        (
            format!("{PRELUDE}{}", popts.starlark_file_source),
            "invalid --starlark:file: ",
        )
    } else if !popts.starlark_file.is_empty() {
        return Err(format!("invalid --starlark:file: failed to read {}", popts.starlark_file));
    } else {
        (
            format!("{PRELUDE}def format(target):\n    return (str(target.label))\n"),
            "invalid --starlark:expr: ",
        )
    };

    let mut node_data_list = Vec::new();
    let mut by_host_id: BTreeMap<String, usize> = BTreeMap::new();

    for n in nodes {
        let info = env.info_of(&n.label);
        let mut providers: BTreeMap<String, Value> = BTreeMap::new();
        let mut files: Vec<Value> = Vec::new();

        let (ct_arc, default_files) = if let (Some(info), Some(cfg_key)) = (&info, &n.cfg) {
            match &info.target {
                Target::Rule(_) | Target::OutputFile { .. } => {
                    if let Some(ct) = env.analyzed(n).map_err(|e| e.message)? {
                        let runfiles_obj = Value::Host("obj".into(), "<unknown object com.google.devtools.build.lib.analysis.Runfiles>".into());
                        let files_to_run_obj = Value::Host("obj".into(), "<unknown object com.google.devtools.build.lib.analysis.FilesToRunProvider>".into());
                        let file_provider_obj = Value::Host("obj".into(), "<unknown object com.google.devtools.build.lib.analysis.FileProvider>".into());
                        let mut files_for_depset = Vec::new();
                        for a in &ct.default_info.files {
                            let fv = zel_analysis::objects::file_value(a);
                            files.push(fv.clone());
                            files_for_depset.push(fv);
                        }
                        let ds_val = Value::Depset(Arc::new(Depset::new(DepsetOrder::Default, files_for_depset, Vec::new()).unwrap()));
                        providers.insert("FileProvider".into(), file_provider_obj);
                        providers.insert("FilesToRunProvider".into(), files_to_run_obj.clone());
                        let mut og_fields = BTreeMap::new();
                        og_fields.insert("_hidden_top_level_INTERNAL_".into(), ds_val.clone());
                        providers.insert("OutputGroupInfo".into(), Value::Struct(Arc::new(zel_starlark::StructValue { fields: og_fields, is_attr: false, opaque_class: None })));
                        let mut def_fields = BTreeMap::new();
                        def_fields.insert("data_runfiles".into(), runfiles_obj.clone());
                        def_fields.insert("default_runfiles".into(), runfiles_obj);
                        def_fields.insert("files".into(), ds_val);
                        def_fields.insert("files_to_run".into(), files_to_run_obj);
                        providers.insert("DefaultInfo".into(), Value::Struct(Arc::new(zel_starlark::StructValue { fields: def_fields, is_attr: false, opaque_class: None })));
                        let df = ct.default_info.files.clone();
                        (ct, df)
                    } else {
                        let ckey = env.config_key(cfg_key).cloned().unwrap_or_default();
                        (Arc::new(ConfiguredTarget::new(n.label.clone(), ckey, "rule".into())), Vec::new())
                    }
                }
                _ => {
                    let ckey = env.config_key(cfg_key).cloned().unwrap_or_default();
                    (Arc::new(ConfiguredTarget::new(n.label.clone(), ckey, "source file".into())), Vec::new())
                }
            }
        } else {
            let rule_class = if let Some(info) = &info {
                match &info.target {
                    Target::InputFile { .. } => "source file",
                    Target::OutputFile { .. } => "generated file",
                    _ => "source file",
                }
            } else {
                "source file"
            };
            let ckey = env.top.clone();
            (Arc::new(ConfiguredTarget::new(n.label.clone(), ckey, rule_class.into())), Vec::new())
        };
        let _ = default_files;

        let tv = zel_analysis::objects::target_value(ct_arc);
        if let Value::Host(_, ref hid) = tv {
            by_host_id.insert(hid.clone(), node_data_list.len());
        }

        let opts = if let Some(cfg) = n.cfg.as_ref().and_then(|c| env.config_key(c)).cloned() {
            let s = |k: &str, v: String| (Value::String(format!("//command_line_option:{k}")), Value::String(v));
            let mut o = Vec::new();
            o.push(s("compilation_mode", cfg.compilation_mode.clone()));
            o.push(s("cpu", cfg.cpu.clone()));
            o.push(s("is_exec", cfg.is_exec.to_string()));
            o.push((
                Value::String("//command_line_option:platforms".into()),
                list(cfg.platforms.iter().map(|p| Value::String(p.clone())).collect()),
            ));
            for (k, v) in &cfg.options {
                o.push(s(k, v.clone()));
            }
            for (k, v) in &cfg.flag_values {
                o.push((Value::String(k.clone()), Value::String(v.clone())));
            }
            if !cfg.user_defines.is_empty() {
                o.push((
                    Value::String("//command_line_option:define".into()),
                    list(cfg.user_defines.iter().map(|(k, v)| Value::String(format!("{k}={v}"))).collect()),
                ));
            }
            Some(o)
        } else {
            None
        };

        node_data_list.push(NodeTargetData {
            label: n.label.clone(),
            target_value: tv,
            files,
            providers,
            opts,
        });
    }

    let node_data = Arc::new(node_data_list);
    let nd = Arc::clone(&node_data);
    let host_map = Arc::new(by_host_id);
    let hm = Arc::clone(&host_map);

    let host_fn: HostFn = Arc::new(move |_ctx, op, args, _kw| {
        let first = args.first().ok_or_else(|| StarlarkError("missing host object".into()))?;
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
                        "dirname" => Ok(Value::String(art.exec_path.parent_directory().map(|p| p.path_string().to_string()).unwrap_or_default())),
                        "extension" => Ok(Value::String(art.exec_path.base_name().rsplit_once('.').map(|(_, ext)| ext).unwrap_or_default().to_string())),
                        other => Err(StarlarkError(format!("'File' object has no field or method '{other}'"))),
                    };
                }
                return Err(StarlarkError(format!("unknown file operation '{op}'")));
            }
            let idx = hm.get(id_str).copied().or_else(|| id_str.parse().ok()).ok_or_else(|| StarlarkError("invalid target id".into()))?;
            let data = &nd[idx];
            if let Some(attr) = op.strip_prefix("attr:target.") {
                match attr {
                    "label" => Ok(Value::Label(data.label.clone())),
                    "files" => Ok(Value::Depset(Arc::new(
                        Depset::new(DepsetOrder::Default, data.files.clone(), Vec::new())
                            .map_err(|e| StarlarkError(e.0))?,
                    ))),
                    "providers_dict" => {
                        let mut p_entries = Vec::new();
                        for key in &["FileProvider", "FilesToRunProvider", "OutputGroupInfo", "DefaultInfo"] {
                            if let Some(v) = data.providers.get(*key) {
                                p_entries.push((Value::String(key.to_string()), v.clone()));
                            }
                        }
                        for (k, v) in &data.providers {
                            if !["FileProvider", "FilesToRunProvider", "OutputGroupInfo", "DefaultInfo"].contains(&k.as_str()) {
                                p_entries.push((Value::String(k.clone()), v.clone()));
                            }
                        }
                        Ok(dict(p_entries))
                    },
                    "build_options_dict" => Ok(match &data.opts {
                        Some(o) => dict(o.clone()),
                        None => Value::None,
                    }),
                    other => {
                        if let Some(v) = data.providers.get(other) {
                            Ok(v.clone())
                        } else {
                            Err(StarlarkError(format!("'target' object has no field or method '{other}'")))
                        }
                    }
                }
            } else if op == "index:target" {
                let key = args.get(1).ok_or_else(|| StarlarkError("index expects key".into()))?;
                let name = match key {
                    Value::String(s) => s.as_str(),
                    Value::Provider { name } => name.as_str(),
                    Value::Host(_, id) => id.as_str(),
                    other => other.as_str().unwrap_or(""),
                };
                if let Some(v) = data.providers.get(name) {
                    Ok(v.clone())
                } else if let Some((_, v)) = data.providers.iter().find(|(k, _)| k.ends_with(&format!("%{name}")) || k.as_str() == name) {
                    Ok(v.clone())
                } else {
                    Err(StarlarkError(format!("provider '{name}' not found on target '{}'", data.label)))
                }
            } else if op == "contains:target" {
                let key = args.get(1).ok_or_else(|| StarlarkError("contains expects key".into()))?;
                let name = match key {
                    Value::String(s) => s.as_str(),
                    Value::Provider { name } => name.as_str(),
                    Value::Host(_, id) => id.as_str(),
                    other => other.as_str().unwrap_or(""),
                };
                let found = data.providers.contains_key(name) || data.providers.keys().any(|k| k.ends_with(&format!("%{name}")));
                Ok(Value::Bool(found))
            } else {
                Err(StarlarkError(format!("unknown host operation '{op}'")))
            }
        } else {
            Err(StarlarkError("expected host object".into()))
        }
    });

    let mut ctx = EvalContext::new(PathFragment::empty(), None);
    ctx.strict_names = true;
    ctx.host = Some(host_fn);
    ctx.file_name = "--starlark:expr".to_string();
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
        let r = match ctx.call_value(func.clone(), vec![target], BTreeMap::new()) {
            Ok(val) => val,
            Err(e) => {
                let msg = e.0.trim();
                let cleaned = if let Some(idx) = msg.find(": ") {
                    &msg[idx + 2..]
                } else {
                    msg
                };
                env.tenv.report_event(zel_packages::PackageEvent::error(
                    None,
                    format!("Starlark evaluation error for {}: Traceback (most recent call last):\n\tFile \"--starlark:expr\", line 1, in format\nError: {cleaned}", n.label)
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
