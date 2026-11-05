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

//! `ctx.actions.args()` (`Args.java`): `add`, `add_all`, `add_joined`, `use_param_file`,
//! `set_param_file_format`. Arguments are materialised eagerly (including `map_each`
//! callbacks) when called, rather than lazily at execution time.

use crate::objects::{artifact_of, file_value};
use crate::{CommandLineArgs, ParamFileFormat, ParamFileInfo};
use std::collections::BTreeMap;
use zel_starlark::{EvalContext, StarlarkError, Value};

/// Prefix of a command-line element that stands for the files of a tree artifact (directory):
/// `\u{1}ZEL_TREE\u{1}<format_each>\u{1}<before_each>\u{1}<exec path of the directory>`.
pub const TREE_MARKER: &str = "\u{1}ZEL_TREE\u{1}";
const TREE_SEP: char = '\u{1}';

/// Expands the tree-artifact placeholders of `argv` (see [`TREE_MARKER`]). `list_dir` returns the
/// exec paths of the files below a directory (recursively, sorted), as found after the actions
/// producing the tree ran; `Bazel` expands `add_all(tree_artifact)` the same way at execution.
pub fn expand_tree_artifact_args(
    argv: &[String],
    list_dir: &dyn Fn(&str) -> Result<Vec<String>, String>,
) -> Result<Vec<String>, String> {
    let mut out = Vec::with_capacity(argv.len());
    for a in argv {
        let Some(rest) = a.strip_prefix(TREE_MARKER) else {
            out.push(a.clone());
            continue;
        };
        let mut parts = rest.splitn(3, TREE_SEP);
        let (fmt, before, dir) = (
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
        );
        for f in list_dir(dir)? {
            if !before.is_empty() {
                out.push(before.to_string());
            }
            out.push(if fmt.is_empty() { f } else { format_arg(fmt, &f).map_err(|e| e.0)? });
        }
    }
    Ok(out)
}

#[derive(Default)]
pub(crate) struct ArgsBuilder {
    pub args: Vec<String>,
    pub param_file: Option<ParamFileInfo>,
    pub format: Option<ParamFileFormat>,
}

impl ArgsBuilder {
    pub fn finish(&self) -> CommandLineArgs {
        let mut p = self.param_file.clone();
        if let (Some(p), Some(f)) = (p.as_mut(), self.format) {
            p.format = f;
        } else if p.is_none() && self.format.is_some() {
            // set_param_file_format alone does not enable param files.
        }
        CommandLineArgs { args: self.args.clone(), param_file: p }
    }
}

fn e(m: impl Into<String>) -> StarlarkError {
    StarlarkError(m.into())
}

/// `SingleStringArgFormatter`: substitutes the single `%s`; `%%` is a literal percent.
pub(crate) fn format_arg(fmt: &str, arg: &str) -> Result<String, StarlarkError> {
    let mut out = String::new();
    let mut seen = 0;
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next() {
                Some('s') => {
                    out.push_str(arg);
                    seen += 1;
                }
                Some('%') => out.push('%'),
                _ => {
                    return Err(e(format!(
                        "Invalid value for parameter \"format\": Expected string with a single \"%s\", got \"{fmt}\""
                    )))
                }
            }
        } else {
            out.push(c);
        }
    }
    if seen != 1 {
        return Err(e(format!(
            "Invalid value for parameter \"format\": Expected string with a single \"%s\", got \"{fmt}\""
        )));
    }
    Ok(out)
}

/// Converts one element to its command-line string (`CommandLineItem.expandToCommandLine`).
pub(crate) fn stringify(v: &Value, what: &str) -> Result<String, StarlarkError> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Int(i) => Ok(i.to_string()),
        Value::Label(l) => Ok(l.canonical_form()),
        Value::Host(k, _) if k == "file" => {
            Ok(artifact_of(v).expect("file").exec_path.path_string().to_string())
        }
        Value::Host(k, _) if k == "path" => Ok(v.to_string()),
        other => Err(e(format!(
            "Error in {what}: Cannot convert object of type '{}' to a command line argument; expected string, int, File, or Label",
            other.type_name()
        ))),
    }
}

fn flatten(v: &Value) -> Result<Vec<Value>, StarlarkError> {
    match v {
        Value::Depset(d) => Ok(d.to_list()),
        Value::List(_) | Value::Tuple(_) => zel_starlark::eval::builtins::value_to_sequence(v),
        other => Err(e(format!(
            "in call to add_all(), parameter 'values' got value of type '{}', want 'sequence or depset'",
            other.type_name()
        ))),
    }
}

fn arg<'a>(pos: &'a [Value], kw: &'a BTreeMap<String, Value>, i: usize, name: &str) -> Option<&'a Value> {
    kw.get(name).or_else(|| pos.get(i))
}

fn check_kw(
    func: &str,
    kw: &BTreeMap<String, Value>,
    allowed: &[&str],
) -> Result<(), StarlarkError> {
    for k in kw.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(e(format!("{func}() got unexpected keyword argument '{k}'")));
        }
    }
    Ok(())
}

fn truthy(v: Option<&Value>, default: bool) -> bool {
    match v {
        None | Some(Value::None) => default,
        Some(x) => x.is_truthy(),
    }
}

fn opt_str(v: Option<&Value>, func: &str, param: &str) -> Result<Option<String>, StarlarkError> {
    match v {
        None | Some(Value::None) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(o) => Err(e(format!(
            "in call to {func}(), parameter '{param}' got value of type '{}', want 'string or NoneType'",
            o.type_name()
        ))),
    }
}

/// Applies `map_each` (`None` → identity) to the elements, returning the expanded strings.
fn map_elements(
    ctx: &mut EvalContext,
    items: Vec<Value>,
    map_each: Option<&Value>,
    func: &str,
) -> Result<Vec<String>, StarlarkError> {
    let mut out = Vec::new();
    for it in items {
        match map_each {
            None | Some(Value::None) => out.push(stringify(&it, func)?),
            Some(f) => {
                // `map_each(value, tree_expander)`: a two-parameter callback also receives the
                // directory expander.
                let wants_expander = matches!(f, Value::UserFunction(u) if u.params.iter().filter(|p| !p.is_args && !p.is_kwargs && p.default.is_none() && !p.is_kw_only).count() >= 2);
                let call_args = if wants_expander { vec![it, Value::Host("tree_expander".into(), "0".into())] } else { vec![it] };
                let r = ctx.call_value(f.clone(), call_args, BTreeMap::new())?;
                match r {
                    Value::None => {}
                    Value::String(s) => out.push(s),
                    Value::List(_) | Value::Tuple(_) => {
                        for x in zel_starlark::eval::builtins::value_to_sequence(&r)? {
                            match x {
                                Value::String(s) => out.push(s),
                                other => {
                                    return Err(e(format!(
                                        "Expected map_each to return string, None, or list of strings, found list containing {}",
                                        other.type_name()
                                    )))
                                }
                            }
                        }
                    }
                    other => {
                        return Err(e(format!(
                            "Expected map_each to return string, None, or list of strings, found {}",
                            other.type_name()
                        )))
                    }
                }
            }
        }
    }
    Ok(out)
}

pub(crate) fn args_method(
    b: &mut ArgsBuilder,
    ctx: &mut EvalContext,
    method: &str,
    pos: &[Value],
    kw: &BTreeMap<String, Value>,
) -> Result<Value, StarlarkError> {
    match method {
        "add" => {
            check_kw("add", kw, &["arg_name_or_value", "value", "format"])?;
            let first = arg(pos, kw, 0, "arg_name_or_value")
                .ok_or_else(|| e("add() missing 1 required positional argument: arg_name_or_value"))?;
            let value = arg(pos, kw, 1, "value");
            let fmt = opt_str(kw.get("format").or_else(|| pos.get(2)), "add", "format")?;
            let fmtd = |s: String| match &fmt {
                Some(f) => format_arg(f, &s),
                None => Ok(s),
            };
            match value {
                // 9.3.2: a `None` value is stringified (`args.add("--flag", None)` -> `--flag None`).
                Some(v) => {
                    b.args.push(stringify(first, "add")?);
                    let vs = if matches!(v, Value::None) { "None".to_string() } else { stringify(v, "add")? };
                    b.args.push(fmtd(vs)?);
                }
                None => b.args.push(fmtd(stringify(first, "add")?)?),
            }
            Ok(Value::Host("args".into(), String::new()))
        }
        "add_all" => {
            check_kw(
                "add_all",
                kw,
                &[
                    "arg_name_or_values", "values", "map_each", "format_each", "before_each",
                    "omit_if_empty", "uniquify", "expand_directories", "terminate_with",
                    "allow_closure",
                ],
            )?;
            let first = arg(pos, kw, 0, "arg_name_or_values")
                .ok_or_else(|| e("add_all() missing 1 required positional argument: arg_name_or_values"))?;
            let (name, values) = match arg(pos, kw, 1, "values") {
                Some(v) => (Some(stringify(first, "add_all")?), v.clone()),
                None => (None, first.clone()),
            };
            let items = flatten(&values)?;
            let before_each = opt_str(kw.get("before_each"), "add_all", "before_each")?;
            let format_each = opt_str(kw.get("format_each"), "add_all", "format_each")?;
            let terminate = opt_str(kw.get("terminate_with"), "add_all", "terminate_with")?;
            let mut items = items;
            if truthy(kw.get("uniquify"), false) {
                let mut seen = std::collections::HashSet::new();
                items.retain(|i| seen.insert(i.clone()));
            }
            // Directories (tree artifacts) are expanded to their files *at execution time*
            // (`expand_directories`, default True). Only possible without `map_each`, which
            // would need the children at analysis time; those keep the directory path.
            let expand_dirs = truthy(kw.get("expand_directories"), true)
                && matches!(kw.get("map_each"), None | Some(Value::None));
            let tree_paths: std::collections::HashSet<String> = if expand_dirs {
                items
                    .iter()
                    .filter_map(artifact_of)
                    .filter(|a| a.is_tree_artifact)
                    .map(|a| a.exec_path.path_string().to_string())
                    .collect()
            } else {
                Default::default()
            };
            let mut strs = map_elements(ctx, items, kw.get("map_each"), "add_all")?;
            if truthy(kw.get("uniquify"), false) {
                let mut seen = std::collections::HashSet::new();
                strs.retain(|s| seen.insert(s.clone()));
            }
            if strs.is_empty() && truthy(kw.get("omit_if_empty"), true) {
                return Ok(Value::None);
            }
            if let Some(n) = name {
                b.args.push(n);
            }
            for s in strs {
                if tree_paths.contains(&s) {
                    // Placeholder expanded by `expand_tree_artifact_args` before the spawn runs.
                    if let Some(f) = &format_each {
                        format_arg(f, "")?;
                    }
                    b.args.push(format!(
                        "{TREE_MARKER}{}{TREE_SEP}{}{TREE_SEP}{s}",
                        format_each.clone().unwrap_or_default(),
                        before_each.clone().unwrap_or_default()
                    ));
                    continue;
                }
                let s = match &format_each {
                    Some(f) => format_arg(f, &s)?,
                    None => s,
                };
                if let Some(be) = &before_each {
                    b.args.push(be.clone());
                }
                b.args.push(s);
            }
            if let Some(t) = terminate {
                b.args.push(t);
            }
            Ok(Value::None)
        }
        "add_joined" => {
            check_kw(
                "add_joined",
                kw,
                &[
                    "arg_name_or_values", "values", "join_with", "map_each", "format_each",
                    "format_joined", "omit_if_empty", "uniquify", "expand_directories",
                    "allow_closure",
                ],
            )?;
            let first = arg(pos, kw, 0, "arg_name_or_values")
                .ok_or_else(|| e("add_joined() missing 1 required positional argument: arg_name_or_values"))?;
            let (name, values) = match arg(pos, kw, 1, "values") {
                Some(v) => (Some(stringify(first, "add_joined")?), v.clone()),
                None => (None, first.clone()),
            };
            let join_with = match kw.get("join_with").or_else(|| pos.get(2)) {
                Some(Value::String(s)) => s.clone(),
                _ => return Err(e("add_joined() missing 1 required positional argument: join_with")),
            };
            let format_each = opt_str(kw.get("format_each"), "add_joined", "format_each")?;
            let format_joined = opt_str(kw.get("format_joined"), "add_joined", "format_joined")?;
            let mut items = flatten(&values)?;
            if truthy(kw.get("uniquify"), false) {
                let mut seen = std::collections::HashSet::new();
                items.retain(|i| seen.insert(i.clone()));
            }
            let strs = map_elements(ctx, items, kw.get("map_each"), "add_joined")?;
            if strs.is_empty() && truthy(kw.get("omit_if_empty"), true) {
                return Ok(Value::None);
            }
            let mut mapped = Vec::new();
            for s in strs {
                mapped.push(match &format_each {
                    Some(f) => format_arg(f, &s)?,
                    None => s,
                });
            }
            let mut joined = mapped.join(&join_with);
            if let Some(f) = format_joined {
                joined = format_arg(&f, &joined)?;
            }
            if let Some(n) = name {
                b.args.push(n);
            }
            b.args.push(joined);
            Ok(Value::None)
        }
        "use_param_file" => {
            check_kw("use_param_file", kw, &["param_file_arg", "use_always"])?;
            let flag = match arg(pos, kw, 0, "param_file_arg") {
                Some(Value::String(s)) => s.clone(),
                _ => return Err(e("use_param_file() missing 1 required positional argument: param_file_arg")),
            };
            if flag.matches("%s").count() != 1 {
                return Err(e(
                    "Invalid value for parameter \"param_file_arg\": Expected string with a single \"%s\"",
                ));
            }
            let use_always = truthy(arg(pos, kw, 1, "use_always"), false);
            let fmt = b.param_file.as_ref().map(|p| p.format).unwrap_or(ParamFileFormat::Shell);
            b.param_file = Some(ParamFileInfo { flag_format: flag, format: fmt, use_always, leading_args: 0 });
            Ok(Value::None)
        }
        "set_param_file_format" => {
            check_kw("set_param_file_format", kw, &["format"])?;
            let f = match arg(pos, kw, 0, "format") {
                Some(Value::String(s)) => s.clone(),
                _ => return Err(e("set_param_file_format() missing 1 required positional argument: format")),
            };
            let fmt = match f.as_str() {
                "shell" => ParamFileFormat::Shell,
                "multiline" => ParamFileFormat::Multiline,
                "flag_per_line" => ParamFileFormat::FlagPerLine,
                other => {
                    return Err(e(format!(
                        "Invalid value for parameter \"format\": Expected one of \"shell\", \"multiline\", \"flag_per_line\", got \"{other}\""
                    )))
                }
            };
            b.format = Some(fmt);
            if let Some(p) = b.param_file.as_mut() {
                p.format = fmt;
            }
            Ok(Value::None)
        }
        other => Err(e(format!("'Args' value has no field or method '{other}'"))),
    }
}

#[allow(dead_code)]
pub(crate) fn _unused(a: &crate::Artifact) -> Value {
    file_value(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_artifact_markers_expand_at_execution() {
        let argv = vec![
            "tool".to_string(),
            format!("{TREE_MARKER}--in=%s{TREE_SEP}{TREE_SEP}out/tree"),
            "last".to_string(),
        ];
        let got = expand_tree_artifact_args(&argv, &|d| {
            assert_eq!(d, "out/tree");
            Ok(vec!["out/tree/a".into(), "out/tree/b".into()])
        })
        .unwrap();
        assert_eq!(got, vec!["tool", "--in=out/tree/a", "--in=out/tree/b", "last"]);
        let got = expand_tree_artifact_args(&[format!("{TREE_MARKER}{TREE_SEP}-I{TREE_SEP}d")], &|_| Ok(vec!["d/x".into()])).unwrap();
        assert_eq!(got, vec!["-I", "d/x"]);
    }

    #[test]
    fn format_arg_semantics() {
        assert_eq!(format_arg("--x=%s", "v").unwrap(), "--x=v");
        assert_eq!(format_arg("100%%-%s", "v").unwrap(), "100%-v");
        assert!(format_arg("no placeholder", "v").is_err());
        assert!(format_arg("%s %s", "v").is_err());
    }
}
