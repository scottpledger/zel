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
//! `set_param_file_format`.

use crate::objects::{artifact_of, file_value};
use crate::{Artifact, CommandLineArgs, ParamFileFormat, ParamFileInfo};
use std::collections::BTreeMap;
use zel_starlark::{EvalContext, StarlarkError, Value};

/// Prefix of a command-line element that stands for the files of a tree artifact (directory):
/// `\u{1}ZEL_TREE\u{1}<format_each>\u{1}<before_each>\u{1}<exec path of the directory>`.
pub const TREE_MARKER: &str = "\u{1}ZEL_TREE\u{1}";
pub(crate) const TREE_HEAD_MARKER: &str = "\u{1}ZEL_TREE_HEAD\u{1}";
pub(crate) const TREE_TAIL_MARKER: &str = "\u{1}ZEL_TREE_TAIL\u{1}";
pub(crate) const TREE_NONFLAGS_START: &str = "\u{1}ZEL_TREE_NF_S\u{1}";
pub(crate) const TREE_NONFLAGS_GROUP_START: &str = "\u{1}ZEL_TREE_NFG_S\u{1}";
pub(crate) const TREE_NONFLAGS_END: &str = "\u{1}ZEL_TREE_NF_E\u{1}";
const TREE_SEP: char = '\u{1}';

/// Collapses one `Args` group for `flag_per_line` (`StarlarkCustomCommandLine.GroupedPreprocessedArgs`).
pub(crate) fn collapse_flag_per_line_group(group: Vec<String>, dst: &mut Vec<String>) {
    match group.as_slice() {
        [] => {}
        [only] => dst.push(only.clone()),
        [first, rest @ ..] if first.is_empty() => dst.push(rest.join(" ")),
        [first, rest @ ..] => dst.push(format!("{first}={}", rest.join(" "))),
    }
}

/// Expands the tree-artifact placeholders of `argv` (see [`TREE_MARKER`]). `list_dir` returns the
/// exec paths of the files below a directory (recursively, sorted), as found after the actions
/// producing the tree ran; `Bazel` expands `add_all(tree_artifact)` the same way at execution.
pub fn expand_tree_artifact_args(
    argv: &[String],
    list_dir: &dyn Fn(&str) -> Result<Vec<String>, String>,
) -> Result<Vec<String>, String> {
    let mut out = Vec::with_capacity(argv.len());
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        if a == TREE_NONFLAGS_START || a == TREE_NONFLAGS_GROUP_START {
            let grouped = a == TREE_NONFLAGS_GROUP_START;
            let start_inner = i + 1;
            let mut end_inner = start_inner;
            while end_inner < argv.len() && argv[end_inner] != TREE_NONFLAGS_END {
                end_inner += 1;
            }
            let expanded = expand_tree_artifact_args(&argv[start_inner..end_inner], list_dir)?;
            let candidates = if grouped {
                let mut collapsed = Vec::with_capacity(1);
                collapse_flag_per_line_group(expanded, &mut collapsed);
                collapsed
            } else {
                expanded
            };
            out.extend(candidates.into_iter().filter(|s| !s.starts_with("--")));
            i = if end_inner < argv.len() {
                end_inner + 1
            } else {
                argv.len()
            };
            continue;
        }
        if let Some(head) = a.strip_prefix(TREE_HEAD_MARKER) {
            // Conditional group: HEAD, one or more TREE_MARKERs, optional TAIL.
            i += 1;
            let mut group_expanded = Vec::new();
            while i < argv.len() {
                if let Some(rest) = argv[i].strip_prefix(TREE_MARKER) {
                    expand_one_tree(rest, list_dir, &mut group_expanded)?;
                    i += 1;
                } else {
                    break;
                }
            }
            let tail = if i < argv.len() {
                if let Some(t) = argv[i].strip_prefix(TREE_TAIL_MARKER) {
                    i += 1;
                    Some(t)
                } else {
                    None
                }
            } else {
                None
            };
            if !group_expanded.is_empty() {
                out.push(head.to_string());
                out.extend(group_expanded);
                if let Some(t) = tail {
                    out.push(t.to_string());
                }
            }
            continue;
        }
        if a.starts_with(TREE_MARKER) {
            let mut group_expanded = Vec::new();
            while i < argv.len() {
                if let Some(r) = argv[i].strip_prefix(TREE_MARKER) {
                    expand_one_tree(r, list_dir, &mut group_expanded)?;
                    i += 1;
                } else {
                    break;
                }
            }
            let tail = if i < argv.len() {
                if let Some(t) = argv[i].strip_prefix(TREE_TAIL_MARKER) {
                    i += 1;
                    Some(t)
                } else {
                    None
                }
            } else {
                None
            };
            if !group_expanded.is_empty() {
                out.extend(group_expanded);
                if let Some(t) = tail {
                    out.push(t.to_string());
                }
            }
            continue;
        }
        if let Some(t) = a.strip_prefix(TREE_TAIL_MARKER) {
            out.push(t.to_string());
            i += 1;
            continue;
        }
        out.push(a.clone());
        i += 1;
    }
    Ok(out)
}

fn expand_one_tree(
    rest: &str,
    list_dir: &dyn Fn(&str) -> Result<Vec<String>, String>,
    out: &mut Vec<String>,
) -> Result<(), String> {
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
        out.push(if fmt.is_empty() {
            f
        } else {
            format_arg(fmt, &f).map_err(|e| e.0)?
        });
    }
    Ok(())
}

#[derive(Default)]
pub(crate) struct ArgsBuilder {
    pub args: Vec<String>,
    pub groups: Vec<(usize, usize)>,
    pub param_file: Option<ParamFileInfo>,
    pub format: Option<ParamFileFormat>,
    pub directory_artifacts: Vec<Artifact>,
}

impl ArgsBuilder {
    pub fn finish(&self) -> CommandLineArgs {
        let mut p = self.param_file.clone();
        if let Some(ref mut p) = p {
            if let Some(f) = self.format {
                p.format = f;
            }
            p.flag_groups = self.groups.clone();
        }
        CommandLineArgs {
            args: self.args.clone(),
            param_file: p,
        }
    }
}

fn e(m: impl Into<String>) -> StarlarkError {
    StarlarkError(m.into())
}

/// Validates a `SingleStringArgFormatter` format string (`%s` exactly once, `%%` is literal `%`).
pub(crate) fn validate_format_str(param: &str, fmt: &str) -> Result<(), StarlarkError> {
    let mut seen = 0;
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next() {
                Some('s') => seen += 1,
                Some('%') => {}
                _ => {
                    return Err(e(format!(
                        "Invalid value for parameter \"{param}\": Expected string with a single \"%s\", got \"{fmt}\""
                    )))
                }
            }
        }
    }
    if seen != 1 {
        return Err(e(format!(
            "Invalid value for parameter \"{param}\": Expected string with a single \"%s\", got \"{fmt}\""
        )));
    }
    Ok(())
}

/// `SingleStringArgFormatter`: substitutes the single `%s`; `%%` is a literal percent.
pub(crate) fn format_arg(fmt: &str, arg: &str) -> Result<String, StarlarkError> {
    let mut out = String::new();
    let mut seen = 0;
    let mut chars = fmt.chars();
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
        Value::Bool(b) => Ok(if *b { "True" } else { "False" }.to_string()),
        Value::Float(f) => Ok(zel_starlark::format_float(*f)),
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

fn flatten(v: &Value, func: &str) -> Result<Vec<Value>, StarlarkError> {
    match v {
        Value::Depset(d) => Ok(d.to_list()),
        Value::List(_) | Value::Tuple(_) => zel_starlark::eval::builtins::value_to_sequence(v),
        other => Err(e(format!(
            "in call to {func}(), parameter 'values' got value of type '{}', want 'sequence or depset'",
            other.type_name()
        ))),
    }
}

fn arg<'a>(
    pos: &'a [Value],
    kw: &'a BTreeMap<String, Value>,
    i: usize,
    name: &str,
) -> Option<&'a Value> {
    kw.get(name).or_else(|| pos.get(i))
}

fn check_kw(
    func: &str,
    pos: &[Value],
    max_pos: usize,
    kw: &BTreeMap<String, Value>,
    allowed: &[&str],
) -> Result<(), StarlarkError> {
    if pos.len() > max_pos {
        return Err(e(format!(
            "{func}() accepts no more than {max_pos} positional argument{} but got {}",
            if max_pos == 1 { "" } else { "s" },
            pos.len()
        )));
    }
    for k in kw.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(e(format!("{func}() got unexpected keyword argument '{k}'")));
        }
    }
    Ok(())
}

fn want_bool(
    v: Option<&Value>,
    func: &str,
    param: &str,
    default: bool,
) -> Result<bool, StarlarkError> {
    match v {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(e(format!(
            "in call to {func}(), parameter '{param}' got value of type '{}', want 'bool'",
            other.type_name()
        ))),
    }
}

fn want_arg_name(v: &Value) -> Result<String, StarlarkError> {
    match v {
        Value::String(s) => Ok(s.clone()),
        other => Err(e(format!(
            "expected value of type 'string' for arg name, got '{}'",
            other.type_name()
        ))),
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

fn validate_map_each(
    fn_val: Option<&Value>,
    allow_closure: bool,
) -> Result<Option<&Value>, StarlarkError> {
    let Some(f) = fn_val else { return Ok(None) };
    if matches!(f, Value::None) {
        return Ok(None);
    }
    if let Value::UserFunction(u) = f {
        if !allow_closure && (!u.closure.scopes.is_empty() || u.name == "<lambda>") {
            return Err(e(format!(
                "to avoid unintended retention of analysis data structures, the map_each function (declared at {}) must be declared by a top-level def statement",
                u.location
            )));
        }
    }
    Ok(Some(f))
}

enum MappedItem {
    Concrete(String),
    TreeDir(String),
}

/// Applies `map_each` (`None` → identity) to the elements, returning the expanded items.
fn map_elements(
    ctx: &mut EvalContext,
    items: Vec<Value>,
    map_each: Option<&Value>,
    expand_dirs: bool,
    func: &str,
) -> Result<Vec<MappedItem>, StarlarkError> {
    let mut out = Vec::new();
    for it in items {
        match map_each {
            None => {
                if expand_dirs {
                    if let Some(a) = artifact_of(&it) {
                        if a.is_tree_artifact {
                            out.push(MappedItem::TreeDir(a.exec_path.path_string().to_string()));
                            continue;
                        }
                    }
                }
                out.push(MappedItem::Concrete(stringify(&it, func)?));
            }
            Some(f) => {
                // `Args.java`: pass the directory expander when the user function declares
                // 2 or more parameters (including optional / keyword-only / *args / **kwargs).
                let wants_expander = matches!(f, Value::UserFunction(u) if u.params.len() >= 2);
                let call_args = if wants_expander {
                    vec![it, Value::Host("tree_expander".into(), "0".into())]
                } else {
                    vec![it]
                };
                let r = ctx.call_value(f.clone(), call_args, BTreeMap::new())?;
                match r {
                    Value::None => {}
                    Value::String(s) => out.push(MappedItem::Concrete(s)),
                    Value::List(_) | Value::Tuple(_) => {
                        for x in zel_starlark::eval::builtins::value_to_sequence(&r)? {
                            match x {
                                Value::String(s) => out.push(MappedItem::Concrete(s)),
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
            check_kw("add", pos, 2, kw, &["arg_name_or_value", "value", "format"])?;
            let first = arg(pos, kw, 0, "arg_name_or_value").ok_or_else(|| {
                e("add() missing 1 required positional argument: arg_name_or_value")
            })?;
            let value = arg(pos, kw, 1, "value");
            let fmt = opt_str(kw.get("format"), "add", "format")?;
            if let Some(f) = &fmt {
                validate_format_str("format", f)?;
            }
            let fmtd = |s: String| match &fmt {
                Some(f) => format_arg(f, &s),
                None => Ok(s),
            };
            let check_single = |v: &Value| -> Result<(), StarlarkError> {
                if matches!(v, Value::Depset(_) | Value::List(_) | Value::Tuple(_)) {
                    return Err(e(
                        "Args.add() doesn't accept vectorized arguments. Please use Args.add_all() or Args.add_joined() instead.",
                    ));
                }
                if artifact_of(v).is_some_and(|a| a.is_tree_artifact) {
                    return Err(e(
                        "Cannot add directories to Args#add since they may expand to multiple values. Either use Args#add_all (if you want expansion) or args.add(directory.path) (if you do not).",
                    ));
                }
                Ok(())
            };
            let start = b.args.len();
            match value {
                // 9.3.2: a `None` value is stringified (`args.add("--flag", None)` -> `--flag None`).
                Some(v) => {
                    check_single(v)?;
                    b.args.push(want_arg_name(first)?);
                    let vs = if matches!(v, Value::None) {
                        "None".to_string()
                    } else {
                        stringify(v, "add")?
                    };
                    b.args.push(fmtd(vs)?);
                }
                None => {
                    check_single(first)?;
                    let vs = if matches!(first, Value::None) {
                        "None".to_string()
                    } else {
                        stringify(first, "add")?
                    };
                    b.args.push(fmtd(vs)?);
                }
            }
            let end = b.args.len();
            if end > start {
                b.groups.push((start, end));
            }
            Ok(Value::Host("args".into(), String::new()))
        }
        "add_all" => {
            check_kw(
                "add_all",
                pos,
                2,
                kw,
                &[
                    "arg_name_or_values",
                    "values",
                    "map_each",
                    "format_each",
                    "before_each",
                    "omit_if_empty",
                    "uniquify",
                    "expand_directories",
                    "terminate_with",
                    "allow_closure",
                ],
            )?;
            let first = arg(pos, kw, 0, "arg_name_or_values").ok_or_else(|| {
                e("add_all() missing 1 required positional argument: arg_name_or_values")
            })?;
            let (name, values) = match arg(pos, kw, 1, "values") {
                Some(v) => (Some(want_arg_name(first)?), v.clone()),
                None => (None, first.clone()),
            };
            let items = flatten(&values, "add_all")?;
            let before_each = opt_str(kw.get("before_each"), "add_all", "before_each")?;
            let format_each = opt_str(kw.get("format_each"), "add_all", "format_each")?;
            let terminate = opt_str(kw.get("terminate_with"), "add_all", "terminate_with")?;
            if let Some(f) = &format_each {
                validate_format_str("format_each", f)?;
            }
            let omit_if_empty =
                want_bool(kw.get("omit_if_empty"), "add_all", "omit_if_empty", true)?;
            let uniquify = want_bool(kw.get("uniquify"), "add_all", "uniquify", false)?;
            let expand_dirs_param = want_bool(
                kw.get("expand_directories"),
                "add_all",
                "expand_directories",
                true,
            )?;
            let allow_closure =
                want_bool(kw.get("allow_closure"), "add_all", "allow_closure", false)?;
            let map_each = validate_map_each(kw.get("map_each"), allow_closure)?;

            if expand_dirs_param {
                for item in &items {
                    if let Some(a) = artifact_of(item) {
                        if a.is_tree_artifact {
                            b.directory_artifacts.push(a);
                        }
                    }
                }
            }
            let expand_dirs = expand_dirs_param && map_each.is_none();
            let mut mapped = map_elements(ctx, items, map_each, expand_dirs, "add_all")?;
            if uniquify {
                let mut seen = std::collections::HashSet::new();
                mapped.retain(|m| match m {
                    MappedItem::Concrete(s) => seen.insert((0u8, s.clone())),
                    MappedItem::TreeDir(d) => seen.insert((1u8, d.clone())),
                });
            }
            if mapped.is_empty() && omit_if_empty {
                return Ok(Value::None);
            }
            let start = b.args.len();
            let has_concrete = mapped.iter().any(|m| matches!(m, MappedItem::Concrete(_)));
            let only_trees_omit = omit_if_empty && !has_concrete && !mapped.is_empty();
            if let Some(n) = name {
                if only_trees_omit {
                    b.args.push(format!("{TREE_HEAD_MARKER}{n}"));
                } else {
                    b.args.push(n);
                }
            }
            for item in mapped {
                match item {
                    MappedItem::TreeDir(dir) => {
                        b.args.push(format!(
                            "{TREE_MARKER}{}{TREE_SEP}{}{TREE_SEP}{dir}",
                            format_each.clone().unwrap_or_default(),
                            before_each.clone().unwrap_or_default()
                        ));
                    }
                    MappedItem::Concrete(s) => {
                        let s = match &format_each {
                            Some(f) => format_arg(f, &s)?,
                            None => s,
                        };
                        if let Some(be) = &before_each {
                            b.args.push(be.clone());
                        }
                        b.args.push(s);
                    }
                }
            }
            if let Some(t) = terminate {
                if only_trees_omit {
                    b.args.push(format!("{TREE_TAIL_MARKER}{t}"));
                } else {
                    b.args.push(t);
                }
            }
            let end = b.args.len();
            if end > start {
                b.groups.push((start, end));
            }
            Ok(Value::Host("args".into(), String::new()))
        }
        "add_joined" => {
            check_kw(
                "add_joined",
                pos,
                2,
                kw,
                &[
                    "arg_name_or_values",
                    "values",
                    "join_with",
                    "map_each",
                    "format_each",
                    "format_joined",
                    "omit_if_empty",
                    "uniquify",
                    "expand_directories",
                    "allow_closure",
                ],
            )?;
            let first = arg(pos, kw, 0, "arg_name_or_values").ok_or_else(|| {
                e("add_joined() missing 1 required positional argument: arg_name_or_values")
            })?;
            let (name, values) = match arg(pos, kw, 1, "values") {
                Some(v) => (Some(want_arg_name(first)?), v.clone()),
                None => (None, first.clone()),
            };
            let join_with = match kw.get("join_with") {
                Some(Value::String(s)) => s.clone(),
                Some(other) => {
                    return Err(e(format!(
                        "in call to add_joined(), parameter 'join_with' got value of type '{}', want 'string'",
                        other.type_name()
                    )))
                }
                None => return Err(e("add_joined() missing 1 required keyword-only argument: join_with")),
            };
            let format_each = opt_str(kw.get("format_each"), "add_joined", "format_each")?;
            let format_joined = opt_str(kw.get("format_joined"), "add_joined", "format_joined")?;
            if let Some(f) = &format_each {
                validate_format_str("format_each", f)?;
            }
            if let Some(f) = &format_joined {
                validate_format_str("format_joined", f)?;
            }
            let omit_if_empty =
                want_bool(kw.get("omit_if_empty"), "add_joined", "omit_if_empty", true)?;
            let uniquify = want_bool(kw.get("uniquify"), "add_joined", "uniquify", false)?;
            let _expand_dirs = want_bool(
                kw.get("expand_directories"),
                "add_joined",
                "expand_directories",
                true,
            )?;
            let allow_closure = want_bool(
                kw.get("allow_closure"),
                "add_joined",
                "allow_closure",
                false,
            )?;
            let map_each = validate_map_each(kw.get("map_each"), allow_closure)?;

            let items = flatten(&values, "add_joined")?;
            let mapped = map_elements(ctx, items, map_each, false, "add_joined")?;
            let mut strs: Vec<String> = mapped
                .into_iter()
                .map(|m| match m {
                    MappedItem::Concrete(s) | MappedItem::TreeDir(s) => s,
                })
                .collect();
            if uniquify {
                let mut seen = std::collections::HashSet::new();
                strs.retain(|s| seen.insert(s.clone()));
            }
            if strs.is_empty() && omit_if_empty {
                return Ok(Value::None);
            }
            let mut formatted = Vec::with_capacity(strs.len());
            for s in strs {
                formatted.push(match &format_each {
                    Some(f) => format_arg(f, &s)?,
                    None => s,
                });
            }
            let mut joined = formatted.join(&join_with);
            if let Some(f) = format_joined {
                joined = format_arg(&f, &joined)?;
            }
            let start = b.args.len();
            if let Some(n) = name {
                b.args.push(n);
            }
            b.args.push(joined);
            let end = b.args.len();
            b.groups.push((start, end));
            Ok(Value::Host("args".into(), String::new()))
        }
        "use_param_file" => {
            check_kw(
                "use_param_file",
                pos,
                1,
                kw,
                &["param_file_arg", "use_always"],
            )?;
            let flag = match arg(pos, kw, 0, "param_file_arg") {
                Some(Value::String(s)) => s.clone(),
                Some(other) => {
                    return Err(e(format!(
                        "in call to use_param_file(), parameter 'param_file_arg' got value of type '{}', want 'string'",
                        other.type_name()
                    )))
                }
                None => return Err(e("use_param_file() missing 1 required positional argument: param_file_arg")),
            };
            validate_format_str("param_file_arg", &flag)?;
            let use_always =
                want_bool(kw.get("use_always"), "use_param_file", "use_always", false)?;
            let fmt = b
                .format
                .or_else(|| b.param_file.as_ref().map(|p| p.format))
                .unwrap_or(ParamFileFormat::Shell);
            b.param_file = Some(ParamFileInfo {
                flag_format: flag,
                format: fmt,
                use_always,
                leading_args: 0,
                flag_groups: b.groups.clone(),
            });
            Ok(Value::None)
        }
        "set_param_file_format" => {
            check_kw("set_param_file_format", pos, 1, kw, &["format"])?;
            if b.format.is_some() {
                return Err(e("set_param_file_format() may only be called once"));
            }
            let f = match arg(pos, kw, 0, "format") {
                Some(Value::String(s)) => s.clone(),
                Some(other) => {
                    return Err(e(format!(
                        "in call to set_param_file_format(), parameter 'format' got value of type '{}', want 'string'",
                        other.type_name()
                    )))
                }
                None => return Err(e("set_param_file_format() missing 1 required positional argument: format")),
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
        assert_eq!(
            got,
            vec!["tool", "--in=out/tree/a", "--in=out/tree/b", "last"]
        );
        let got =
            expand_tree_artifact_args(&[format!("{TREE_MARKER}{TREE_SEP}-I{TREE_SEP}d")], &|_| {
                Ok(vec!["d/x".into()])
            })
            .unwrap();
        assert_eq!(got, vec!["-I", "d/x"]);
        // Empty tree with omit_if_empty drops HEAD and TAIL.
        let empty = vec![
            format!("{TREE_HEAD_MARKER}--inputs"),
            format!("{TREE_MARKER}{TREE_SEP}{TREE_SEP}empty_dir"),
            format!("{TREE_TAIL_MARKER}--end"),
        ];
        assert!(expand_tree_artifact_args(&empty, &|_| Ok(vec![]))
            .unwrap()
            .is_empty());
        assert_eq!(
            expand_tree_artifact_args(&empty, &|_| Ok(vec!["empty_dir/f".into()])).unwrap(),
            vec!["--inputs", "empty_dir/f", "--end"]
        );
    }

    #[test]
    fn format_arg_semantics() {
        assert_eq!(format_arg("--x=%s", "v").unwrap(), "--x=v");
        assert_eq!(format_arg("100%%-%s", "v").unwrap(), "100%-v");
        assert!(format_arg("no placeholder", "v").is_err());
        assert!(format_arg("%s %s", "v").is_err());
        assert!(validate_format_str("param_file_arg", "%%s").is_err());
        assert!(validate_format_str("param_file_arg", "100%%-%s").is_ok());
    }
}
