//! Argument binding for `repository_ctx` / `module_ctx` / path native methods, mirroring the
//! `@StarlarkMethod` parameter declarations of `StarlarkBaseExternalContext`,
//! `StarlarkRepositoryContext` and `ModuleExtensionContext` together with the error messages of
//! `net.starlark.java.eval.BuiltinFunction` argument processing.

use std::collections::BTreeMap;
use zel_starlark::{StarlarkError, Value};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ty {
    Str,
    Bool,
    Int,
    Dict,
    Any,
    /// string, Label or path
    P3,
    /// string or path
    P2,
    /// string or Iterable of strings
    Url,
    /// Sequence of strings
    StrSeq,
    /// Any sequence
    Seq,
    /// string or None
    StrOrNone,
}

struct Param {
    name: &'static str,
    ty: Ty,
    required: bool,
    pos_only: bool,
    kw_only: bool,
}

fn parse_spec(spec: &str) -> Vec<Param> {
    spec.split_whitespace()
        .map(|tok| {
            let (mut tok, mut pos_only, mut kw_only) = (tok, false, false);
            if let Some(t) = tok.strip_prefix('^') {
                tok = t;
                pos_only = true;
            }
            if let Some(t) = tok.strip_prefix('*') {
                tok = t;
                kw_only = true;
            }
            let (required, tok) = match tok.strip_suffix('!') {
                Some(t) => (true, t),
                None => (false, tok),
            };
            let (name, ty) = tok.split_once(':').unwrap_or((tok, "a"));
            let ty = match ty {
                "s" => Ty::Str,
                "b" => Ty::Bool,
                "i" => Ty::Int,
                "d" => Ty::Dict,
                "p3" => Ty::P3,
                "p2" => Ty::P2,
                "u" => Ty::Url,
                "l" => Ty::StrSeq,
                "q" => Ty::Seq,
                "sn" => Ty::StrOrNone,
                _ => Ty::Any,
            };
            Param { name: Box::leak(name.to_string().into_boxed_str()), ty, required, pos_only, kw_only }
        })
        .collect()
}

/// Parameter spec of a context method, or `None` if the method is not described here.
fn spec_of(kind: &str, member: &str) -> Option<&'static str> {
    Some(match (kind, member) {
        (_, "path") => "^path:p3!",
        (_, "file") => "^path:p3! content:s executable:b legacy_utf8:b",
        (_, "read") => "^path:p3! *watch:s",
        (_, "watch") => "^path:p3!",
        (_, "which") => "^program:s!",
        (_, "getenv") => "^name:s! ^default:sn",
        (_, "report_progress") => "^status:s",
        (_, "execute") => "^arguments:q! timeout:i environment:d quiet:b working_directory:s",
        (_, "extract") => "archive:p3! output:p3 strip_prefix:s *rename_files:d *watch_archive:s *stripPrefix:s *strip_components:i *type:s",
        (_, "download") => "url:u! output:p3 sha256:s executable:b allow_fail:b canonical_id:s auth:d headers:d *integrity:s *block:b",
        (_, "download_and_extract") => {
            "url:u! output:p3 sha256:s type:s strip_prefix:s allow_fail:b canonical_id:s auth:d headers:d *integrity:s *rename_files:d *stripPrefix:s *strip_components:i"
        }
        ("module_ctx", "is_dev_dependency") | ("module_ctx", "tag_sort_key") => "^tag!",
        ("repository_ctx", "symlink") => "^target:p3! ^link_name:p3!",
        ("repository_ctx", "template") => "^path:p3! ^template:p3! substitutions:d executable:b *watch_template:s",
        ("repository_ctx", "delete") => "^path:p2!",
        ("repository_ctx", "rename") => "^src:p3! ^dst:p3!",
        ("repository_ctx", "patch") => "^patch_file:p3! strip:i *directory:p3 *watch_patch:s",
        ("repository_ctx", "watch_tree") => "^path:p3!",
        ("repository_ctx", "repo_metadata") => "*reproducible:b *attrs_for_reproducibility:d",
        _ => return None,
    })
}

fn is_path(v: &Value) -> bool {
    matches!(v, Value::Host(k, _) if k == "path")
}

fn all_strings(v: &Value) -> bool {
    match zel_starlark::eval::builtins::value_to_sequence(v) {
        Ok(items) => items.iter().all(|i| matches!(i, Value::String(_))),
        Err(_) => false,
    }
}

fn type_ok(ty: Ty, v: &Value) -> bool {
    match ty {
        Ty::Any => true,
        Ty::Str => matches!(v, Value::String(_)),
        Ty::Bool => matches!(v, Value::Bool(_)),
        Ty::Int => matches!(v, Value::Int(_)),
        Ty::Dict => matches!(v, Value::Dict(_)),
        Ty::P3 => matches!(v, Value::String(_) | Value::Label(_)) || is_path(v),
        Ty::P2 => matches!(v, Value::String(_)) || is_path(v),
        Ty::Url => match v {
            Value::String(_) => true,
            Value::List(_) | Value::Tuple(_) => true,
            _ => false,
        },
        Ty::StrSeq => match v {
            Value::List(_) | Value::Tuple(_) => all_strings(v),
            _ => false,
        },
        Ty::StrOrNone => matches!(v, Value::String(_) | Value::None),
        Ty::Seq => matches!(v, Value::List(_) | Value::Tuple(_) | Value::Range(_)),
    }
}

fn want(ty: Ty) -> &'static str {
    match ty {
        Ty::Any => "unknown",
        Ty::Str => "string",
        Ty::Bool => "bool",
        Ty::Int => "int",
        Ty::Dict => "dict",
        Ty::P3 => "string, Label, or path",
        Ty::P2 => "string or path",
        Ty::Url => "string or Iterable",
        Ty::StrSeq => "sequence",
        Ty::StrOrNone => "string or NoneType",
        Ty::Seq => "sequence",
    }
}

/// Prefix marking binding errors reported at the argument-evaluation location (see `bind_with`).
pub(crate) const STALE: &str = "\u{2}STALE\u{2}";

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Binds positional and keyword arguments to the parameters of `kind.member`. Returns arguments keyed by
/// parameter name, or `None` when the method has no spec. Defaults are not materialised.
pub(crate) fn bind(kind: &str, member: &str, pos: &[Value], kw: &BTreeMap<String, Value>) -> Result<Option<BTreeMap<String, Value>>, StarlarkError> {
    let Some(spec) = spec_of(kind, member) else { return Ok(None) };
    bind_with(member, spec, pos, kw).map(Some)
}

fn bind_with(fname: &str, spec: &str, pos: &[Value], kw: &BTreeMap<String, Value>) -> Result<BTreeMap<String, Value>, StarlarkError> {
    let params = parse_spec(spec);
    // With keyword arguments Bazel binds each argument while still evaluating the call's arguments, so such errors
    // are reported at the (possibly stale) location after the argument evaluation.
    let tag = if kw.is_empty() { "" } else { STALE };
    let e = |m: String| StarlarkError(format!("{tag}{m}"));
    let late = |m: String| StarlarkError(m);
    let type_err = |p: &Param, v: &Value| e(format!("in call to {fname}(), parameter '{}' got value of type '{}', want '{}'", p.name, v.type_name(), want(p.ty)));
    let mut out: BTreeMap<String, Value> = BTreeMap::new();
    // Positional arguments bind (and are type checked) in declaration order.
    let positional: Vec<&Param> = params.iter().filter(|p| !p.kw_only).collect();
    let mut consumed = 0;
    for (p, v) in positional.iter().zip(pos.iter()) {
        if !type_ok(p.ty, v) {
            return Err(type_err(p, v));
        }
        out.insert(p.name.to_string(), v.clone());
        consumed += 1;
    }
    if pos.len() > consumed {
        return Err(if consumed == 0 {
            late(format!("{fname}() got unexpected positional argument"))
        } else {
            late(format!("{fname}() accepts no more than {consumed} positional argument{} but got {}", plural(consumed), pos.len()))
        });
    }
    for (k, v) in kw {
        match params.iter().find(|p| p.name == k) {
            None => {
                let names: Vec<&str> = params.iter().map(|p| p.name).collect();
                let mut msg = format!("{fname}() got unexpected keyword argument '{k}'");
                if let Some(best) = suggest(k, &names) {
                    msg.push_str(&format!(" (did you mean '{best}'?)"));
                }
                return Err(e(msg));
            }
            Some(p) if p.pos_only => return Err(e(format!("{fname}() got named argument for positional-only parameter '{k}'"))),
            Some(p) => {
                if !type_ok(p.ty, v) {
                    return Err(type_err(p, v));
                }
                if out.contains_key(p.name) {
                    return Err(e(format!("{fname}() got multiple values for argument '{k}'")));
                }
                out.insert(p.name.to_string(), v.clone());
            }
        }
    }
    let missing_pos: Vec<&str> = params.iter().filter(|p| p.required && !p.kw_only && !out.contains_key(p.name)).map(|p| p.name).collect();
    if !missing_pos.is_empty() {
        return Err(late(format!("{fname}() missing {} required positional argument{}: {}", missing_pos.len(), plural(missing_pos.len()), missing_pos.join(", "))));
    }
    let missing_kw: Vec<&str> = params.iter().filter(|p| p.required && p.kw_only && !out.contains_key(p.name)).map(|p| p.name).collect();
    if !missing_kw.is_empty() {
        return Err(late(format!("{fname}() missing {} required named argument{}: {}", missing_kw.len(), plural(missing_kw.len()), missing_kw.join(", "))));
    }
    Ok(out)
}

/// `SpellChecker.suggest`: the most similar word (case-insensitive), if close enough.
fn suggest<'a>(input: &str, words: &[&'a str]) -> Option<&'a str> {
    let mut best = None;
    let mut best_d = std::cmp::min(5, (input.chars().count() + 1) / 2) as i64;
    let input = input.to_lowercase();
    for w in words {
        let d = edit_distance(&input, &w.to_lowercase(), best_d);
        if d >= 0 && d < best_d {
            best_d = d;
            best = Some(*w);
        }
    }
    best
}

/// `SpellChecker.editDistance`: Levenshtein distance, or -1 when it exceeds `max`.
fn edit_distance(s1: &str, s2: &str, max: i64) -> i64 {
    if s1 == s2 {
        return 0;
    }
    let a: Vec<char> = s1.chars().collect();
    let b: Vec<char> = s2.chars().collect();
    if (a.len() as i64 - b.len() as i64).abs() > max {
        return -1;
    }
    let mut row: Vec<i64> = (0..=b.len() as i64).collect();
    for i in 1..=a.len() {
        row[0] = i as i64;
        let mut best_in_row = row[0];
        let mut previous = i as i64 - 1;
        for j in 1..=b.len() {
            let old = row[j];
            row[j] = std::cmp::min(previous + if a[i - 1] == b[j - 1] { 0 } else { 1 }, 1 + std::cmp::min(row[j - 1], row[j]));
            previous = old;
            best_in_row = best_in_row.min(row[j]);
        }
        if best_in_row > max {
            return -1;
        }
    }
    let r = row[b.len()];
    if r <= max {
        r
    } else {
        -1
    }
}

/// Binds arguments against an ad-hoc spec (used for methods of non-context objects such as `path`).
pub(crate) fn bind_raw(fname: &str, spec: &str, pos: &[Value], kw: &BTreeMap<String, Value>) -> Result<BTreeMap<String, Value>, StarlarkError> {
    bind_with(fname, spec, pos, kw)
}

/// `SpellChecker.didYouMean`: `" (did you mean 'x'?)"` or the empty string.
pub(crate) fn did_you_mean(input: &str, words: &[&str]) -> String {
    match suggest(input, words) {
        Some(w) => format!(" (did you mean '{w}'?)"),
        None => String::new(),
    }
}
