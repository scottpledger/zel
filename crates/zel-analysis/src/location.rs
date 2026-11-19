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

//! `$(location ...)` expansion (`LocationExpander.java`), Make-variable expansion
//! (`TemplateExpander.java`, `Expander.java`), and Bourne-shell tokenization
//! (`ShellUtils.java`).

use crate::action::shell_escape;
use crate::Artifact;
use std::collections::{BTreeMap, BTreeSet};
use zel_cmdline::{Label, PackageIdentifier, RepositoryMapping};

/// Which path `$(location)`/`$(locations)` yield.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationMode {
    /// `LocationExpander.withExecPaths`: `location` = exec path.
    Exec,
    /// `LocationExpander.withRunfilesPaths`: `location` = root-relative path.
    Runfiles,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PathType {
    Location,
    Exec,
    Rlocation,
}

const FUNCTIONS: &[(&str, PathType, bool)] = &[
    ("location", PathType::Location, false),
    ("locations", PathType::Location, true),
    ("execpath", PathType::Exec, false),
    ("execpaths", PathType::Exec, true),
    ("rootpath", PathType::Location, false),
    ("rootpaths", PathType::Location, true),
    ("rlocationpath", PathType::Rlocation, false),
    ("rlocationpaths", PathType::Rlocation, true),
];

/// `PathFragment.getCallablePathString`: relative paths with no `/` are prefixed with `./`.
pub fn callable_path(path: &str) -> String {
    if !path.is_empty() && !path.starts_with('/') && !path.contains('/') {
        format!("./{path}")
    } else {
        path.to_string()
    }
}

/// Root path of an artifact (`Artifact.getRunfilesPath`: `../<repo>/...` for external repos).
fn root_path(a: &Artifact) -> String {
    a.short_path()
}

fn location_fn_error_name(multiple: bool) -> &'static str {
    if multiple {
        "$(locations)"
    } else {
        "$(location)"
    }
}

#[allow(clippy::too_many_arguments)]
fn eval_location_function(
    fname: &str,
    pt: PathType,
    multiple: bool,
    arg: &str,
    label_map: &BTreeMap<Label, Vec<Artifact>>,
    root: &Label,
    mode: LocationMode,
    workspace_name: &str,
    repo_mapping: Option<&RepositoryMapping>,
) -> Result<String, String> {
    let fn_err = location_fn_error_name(multiple);
    let label = parse_label_with_mapping(arg, root.package_identifier(), repo_mapping)
        .map_err(|e| format!("invalid label in {fn_err} expression: {e}"))?;
    let arts = label_map.get(&label).ok_or_else(|| {
        format!(
            "label '{}' in {fn_err} expression is not a declared prerequisite of this rule",
            label_str(&label)
        )
    })?;
    let mut paths: BTreeSet<String> = BTreeSet::new();
    for a in arts {
        let raw = match pt {
            PathType::Exec => a.exec_path.path_string().to_string(),
            PathType::Location => match (fname, mode) {
                ("location" | "locations", LocationMode::Exec) => {
                    a.exec_path.path_string().to_string()
                }
                _ => root_path(a),
            },
            PathType::Rlocation => {
                let sp = a.short_path();
                match sp.strip_prefix("../") {
                    Some(rest) => rest.to_string(),
                    None => format!("{workspace_name}/{sp}"),
                }
            }
        };
        paths.insert(callable_path(&raw));
    }
    if paths.is_empty() {
        return Err(format!(
            "label '{}' in {fn_err} expression expands to no files",
            label_str(&label)
        ));
    }
    if !multiple && paths.len() > 1 {
        let shown: Vec<&String> = paths.iter().take(5).collect();
        return Err(format!(
            "label '{}' in $(location) expression expands to more than one file, please use $(locations {}) instead.  Files (at most 5 shown) are: [{}]",
            label_str(&label),
            label_str(&label),
            shown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(paths
        .into_iter()
        .map(|p| shell_escape(&p))
        .collect::<Vec<_>>()
        .join(" "))
}

/// Expands `$(location x)`-style references in `input`.
///
/// `label_map` maps each allowed label to the files it provides; `root` is the label of the
/// rule being analysed (for relative label parsing); `workspace_name` is the main repository's
/// runfiles directory name (`_main`).
pub fn expand_locations(
    input: &str,
    label_map: &BTreeMap<Label, Vec<Artifact>>,
    root: &Label,
    mode: LocationMode,
    workspace_name: &str,
) -> Result<String, String> {
    expand_locations_with_mapping(input, label_map, root, mode, workspace_name, None)
}

/// Like [`expand_locations`], resolving apparent `@repo` labels through `repo_mapping`.
pub fn expand_locations_with_mapping(
    input: &str,
    label_map: &BTreeMap<Label, Vec<Artifact>>,
    root: &Label,
    mode: LocationMode,
    workspace_name: &str,
    repo_mapping: Option<&RepositoryMapping>,
) -> Result<String, String> {
    let mut result = String::with_capacity(input.len());
    let mut restart = 0usize;
    loop {
        let Some(start) = input[restart..].find("$(").map(|i| i + restart) else {
            result.push_str(&input[restart..]);
            break;
        };
        let Some(nws) = input[start..].find(' ').map(|i| i + start) else {
            result.push_str(&input[restart..start + 2]);
            restart = start + 2;
            continue;
        };
        let fname = &input[start + 2..nws];
        let Some(&(_, pt, multiple)) = FUNCTIONS.iter().find(|(n, _, _)| *n == fname) else {
            result.push_str(&input[restart..start + 2]);
            restart = start + 2;
            continue;
        };
        result.push_str(&input[restart..start]);
        let Some(end) = input[nws..].find(')').map(|i| i + nws) else {
            return Err(format!("unterminated $({fname}) expression"));
        };
        let arg = input[nws + 1..end].trim_matches(|c: char| c <= ' ');
        let replacement = eval_location_function(
            fname,
            pt,
            multiple,
            arg,
            label_map,
            root,
            mode,
            workspace_name,
            repo_mapping,
        )?;
        result.push_str(&replacement);
        restart = end + 1;
    }
    Ok(result)
}

fn label_str(l: &Label) -> String {
    l.canonical_form()
}

/// Parses `:foo`, `foo`, `//pkg:foo`, `@repo//pkg:foo` relative to `pkg`.
pub fn parse_label(arg: &str, pkg: &PackageIdentifier) -> Result<Label, String> {
    parse_label_with_mapping(arg, pkg, None)
}

/// Parses a label argument relative to `pkg` using `repo_mapping` when present.
pub fn parse_label_with_mapping(
    arg: &str,
    pkg: &PackageIdentifier,
    repo_mapping: Option<&RepositoryMapping>,
) -> Result<Label, String> {
    let fallback = RepositoryMapping::always_fallback();
    let rm = repo_mapping.unwrap_or(&fallback);
    Label::parse_with_package_context(arg, pkg, rm).map_err(|e| e.to_string())
}

/// Result of [`expand_template`] (`TemplateExpander.Expansion`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    pub text: String,
    pub looked_up_variables: Vec<String>,
}

fn is_java_identifier_part(c: char) -> bool {
    c == '$' || c == '_' || c.is_alphanumeric()
}

fn scan_variable(chars: &[char], offset: &mut usize) -> Result<String, String> {
    let len = chars.len();
    match chars[*offset] {
        '(' => {
            *offset += 1;
            let start = *offset;
            while *offset < len && chars[*offset] != ')' {
                *offset += 1;
            }
            if *offset >= len {
                return Err("unterminated variable reference".to_string());
            }
            Ok(chars[start..*offset].iter().collect())
        }
        '{' => {
            *offset += 1;
            let start = *offset;
            while *offset < len && chars[*offset] != '}' {
                *offset += 1;
            }
            if *offset >= len {
                return Err("unterminated variable reference".to_string());
            }
            let expr: String = chars[start..*offset].iter().collect();
            Err(format!(
                "'${{{expr}}}' syntax is not supported; use '$({expr})' instead for \"Make\" variables, or escape the '$' as '$$' if you intended this for the shell"
            ))
        }
        c @ ('@' | '<' | '^') => Ok(c.to_string()),
        _ => {
            let start = *offset;
            while *offset + 1 < len && is_java_identifier_part(chars[*offset + 1]) {
                *offset += 1;
            }
            let expr: String = chars[start..=*offset].iter().collect();
            Err(format!(
                "'${expr}' syntax is not supported; use '$({expr})' instead for \"Make\" variables, or escape the '$' as '$$' if you intended this for the shell"
            ))
        }
    }
}

fn expand_template_depth(
    expression: &str,
    lookup_var: &dyn Fn(&str) -> Result<String, String>,
    lookup_fn: &dyn Fn(&str, &str) -> Result<String, String>,
    depth: usize,
) -> Result<Expansion, String> {
    if depth > 10 {
        return Err(format!(
            "potentially unbounded recursion during expansion of '{expression}'"
        ));
    }
    let chars: Vec<char> = expression.chars().collect();
    let len = chars.len();
    let mut offset = 0usize;
    let mut result = String::with_capacity(expression.len());
    let mut looked_up_variables: Vec<String> = Vec::new();
    let mut record = |v: &str| {
        if !looked_up_variables.iter().any(|x| x == v) {
            looked_up_variables.push(v.to_string());
        }
    };
    while offset < len {
        let c = chars[offset];
        if c == '$' {
            offset += 1;
            if offset >= len {
                return Err("unterminated $".to_string());
            }
            if chars[offset] == '$' {
                result.push('$');
            } else {
                let var = scan_variable(&chars, &mut offset)?;
                if let Some((name, rest)) = var.split_once(' ') {
                    let param = rest.trim_matches(|ch: char| ch <= ' ');
                    let value = lookup_fn(name, param)?;
                    record(name);
                    result.push_str(&value);
                } else {
                    let mut value = lookup_var(&var)?;
                    record(&var);
                    if value != var {
                        let sub = expand_template_depth(&value, lookup_var, lookup_fn, depth + 1)?;
                        value = sub.text;
                        for v in &sub.looked_up_variables {
                            record(v);
                        }
                    }
                    result.push_str(&value);
                }
            }
        } else {
            result.push(c);
        }
        offset += 1;
    }
    Ok(Expansion {
        text: result,
        looked_up_variables,
    })
}

/// `TemplateExpander.expand`: expands `$(NAME)`, `$(func arg)`, `$$`, and `$@`/`$<`/`$^`.
/// Variable values are recursively re-expanded (Make `:=` semantics, depth limit 10);
/// function results are not re-expanded.
pub fn expand_template(
    expression: &str,
    lookup_var: &dyn Fn(&str) -> Result<String, String>,
    lookup_fn: &dyn Fn(&str, &str) -> Result<String, String>,
) -> Result<Expansion, String> {
    if !expression.contains('$') {
        return Ok(Expansion {
            text: expression.to_string(),
            looked_up_variables: Vec::new(),
        });
    }
    expand_template_depth(expression, lookup_var, lookup_fn, 0)
}

/// `TemplateExpander.expandSingleVariable`: if `expression` is a single `$@`/`$<`/`$^` or
/// `$(...)`, returns its raw lookup result without recursive expansion.
pub fn expand_single_variable(
    expression: &str,
    lookup_var: &dyn Fn(&str) -> Result<String, String>,
) -> Result<Option<String>, String> {
    let chars: Vec<char> = expression.chars().collect();
    if chars.is_empty() {
        return Ok(None);
    }
    if chars[0] == '$' {
        let mut offset = 1;
        if offset >= chars.len() {
            return Err("unterminated $".to_string());
        }
        if chars[offset] == '$' {
            return Ok(None);
        }
        let var = scan_variable(&chars, &mut offset)?;
        if offset + 1 == chars.len() {
            return lookup_var(&var).map(Some);
        }
    }
    Ok(None)
}

/// `MakeVariableExpander.expand`: `$(NAME)` → value, `$$` → `$`, `$@`/`$<`/`$^` → value.
pub fn expand_make_variables(
    input: &str,
    vars: &dyn Fn(&str) -> Option<String>,
) -> Result<String, String> {
    let lookup_var = |name: &str| vars(name).ok_or_else(|| format!("$({name}) not defined"));
    let lookup_fn = |name: &str, _param: &str| Err(format!("$({name}) not defined"));
    expand_template(input, &lookup_var, &lookup_fn).map(|e| e.text)
}

/// Unified `Expander` + `LocationTemplateContext`: expands both Make variables and
/// `$(location ...)` functions in a single left-to-right pass (so Make variable values may
/// contain `$(location ...)` and are recursively expanded, while location results are not
/// re-expanded).
#[allow(clippy::too_many_arguments)]
pub fn expand_with_locations(
    input: &str,
    vars: &dyn Fn(&str) -> Option<String>,
    label_map: &BTreeMap<Label, Vec<Artifact>>,
    root: &Label,
    mode: LocationMode,
    workspace_name: &str,
    repo_mapping: Option<&RepositoryMapping>,
    windows_path: bool,
) -> Result<Expansion, String> {
    let post = |s: String| {
        if windows_path {
            s.replace('/', "\\")
        } else {
            s
        }
    };
    let lookup_var = |name: &str| vars(name).ok_or_else(|| format!("$({name}) not defined"));
    let lookup_fn = |fname: &str, param: &str| {
        if let Some(&(_, pt, multiple)) = FUNCTIONS.iter().find(|(n, _, _)| *n == fname) {
            eval_location_function(
                fname,
                pt,
                multiple,
                param,
                label_map,
                root,
                mode,
                workspace_name,
                repo_mapping,
            )
            .map(post)
        } else {
            Err(format!("$({fname}) not defined"))
        }
    };
    let mut exp = expand_template(input, &lookup_var, &lookup_fn)?;
    if windows_path {
        // Apply Windows separator conversion to variable-derived segments as well.
        if !exp.looked_up_variables.is_empty() {
            exp.text = exp.text.replace('/', "\\");
        }
    }
    Ok(exp)
}

/// `ShellUtils.tokenize`: Bourne-shell-like word splitting with quotes and backslash escapes.
/// Splits only on ASCII space (`' '`) and tab (`'\t'`), and inside double quotes preserves
/// backslashes unless the next character is `\\` or `"`.
pub fn shell_tokenize(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut force_token = false;
    let mut quote: Option<char> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' {
                    let Some(n) = chars.next() else {
                        return Err("backslash at end of string".into());
                    };
                    if n != '\\' && n != '"' {
                        cur.push('\\');
                    }
                    cur.push(n);
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    force_token = true;
                }
                ' ' | '\t' => {
                    if force_token || !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                        force_token = false;
                    }
                }
                '\\' => {
                    let Some(n) = chars.next() else {
                        return Err("backslash at end of string".into());
                    };
                    cur.push(n);
                    force_token = true;
                }
                c => {
                    cur.push(c);
                }
            },
        }
    }
    if quote.is_some() {
        return Err("unterminated quotation".into());
    }
    if force_token || !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_vfs::PathFragment;

    fn lbl(s: &str) -> Label {
        Label::parse_canonical(s).unwrap()
    }
    fn art(p: &str, owner: &str) -> Artifact {
        Artifact::derived("k8-fastbuild", PathFragment::create(p), lbl(owner))
    }

    #[test]
    fn expands_exec_and_root_paths() {
        let mut m = BTreeMap::new();
        m.insert(lbl("//a:t"), vec![art("a/t.out", "//a:t")]);
        m.insert(
            lbl("//a:single"),
            vec![Artifact::source(
                PathFragment::create("s.txt"),
                lbl("//a:single"),
            )],
        );
        m.insert(
            lbl("@@ext//p:f"),
            vec![Artifact::source(
                PathFragment::create("p/f.txt"),
                lbl("@@ext//p:f"),
            )],
        );
        let root = lbl("//a:r");
        let s = expand_locations(
            "x $(location :t) $(rootpath //a:t) $(execpath :t) $(location :single) $(rootpath @@ext//p:f) y",
            &m,
            &root,
            LocationMode::Exec,
            "_main",
        )
        .unwrap();
        assert_eq!(
            s,
            "x bazel-out/k8-fastbuild/bin/a/t.out a/t.out bazel-out/k8-fastbuild/bin/a/t.out ./s.txt ../ext/p/f.txt y"
        );
        let s =
            expand_locations("$(location :t)", &m, &root, LocationMode::Runfiles, "_main").unwrap();
        assert_eq!(s, "a/t.out");
        let s = expand_locations(
            "$(rlocationpath :t)",
            &m,
            &root,
            LocationMode::Exec,
            "_main",
        )
        .unwrap();
        assert_eq!(s, "_main/a/t.out");
    }

    #[test]
    fn location_errors() {
        let mut m = BTreeMap::new();
        m.insert(
            lbl("//a:two"),
            vec![art("a/1", "//a:two"), art("a/2", "//a:two")],
        );
        let root = lbl("//a:r");
        assert_eq!(
            expand_locations("$(location :zzz)", &m, &root, LocationMode::Exec, "_main")
                .unwrap_err(),
            "label '//a:zzz' in $(location) expression is not a declared prerequisite of this rule"
        );
        assert_eq!(
            expand_locations("$(execpath :zzz)", &m, &root, LocationMode::Exec, "_main")
                .unwrap_err(),
            "label '//a:zzz' in $(location) expression is not a declared prerequisite of this rule"
        );
        let e = expand_locations("$(location :two)", &m, &root, LocationMode::Exec, "_main")
            .unwrap_err();
        assert!(
            e.contains("expands to more than one file, please use $(locations //a:two) instead.  Files (at most 5 shown) are: [")
                && e.contains("bazel-out/k8-fastbuild/bin/a/1"),
            "{e}"
        );
        assert_eq!(
            expand_locations(
                "$(locations :two)",
                &m,
                &root,
                LocationMode::Runfiles,
                "_main"
            )
            .unwrap(),
            "a/1 a/2"
        );
        assert_eq!(
            expand_locations("$(location :two", &m, &root, LocationMode::Exec, "_main")
                .unwrap_err(),
            "unterminated $(location) expression"
        );
        // `$(` without a following space is not a location function and passes through.
        assert_eq!(
            expand_locations("$(foo)", &m, &root, LocationMode::Exec, "_main").unwrap(),
            "$(foo)"
        );
    }

    #[test]
    fn make_vars() {
        let f = |n: &str| match n {
            "A" => Some("1".to_string()),
            "@" => Some("out".to_string()),
            "REF" => Some("$(A)".to_string()),
            "SELF" => Some("SELF".to_string()),
            "LOOP" => Some("$(LOOP)".to_string()),
            "DOLLAR" => Some("$$".to_string()),
            _ => None,
        };
        assert_eq!(
            expand_make_variables("a $(A) $$ $@ $(REF) $(SELF) $(DOLLAR)", &f).unwrap(),
            "a 1 $ out 1 SELF $"
        );
        assert_eq!(
            expand_make_variables("$(B)", &f).unwrap_err(),
            "$(B) not defined"
        );
        assert_eq!(
            expand_make_variables("$(A", &f).unwrap_err(),
            "unterminated variable reference"
        );
        assert!(expand_make_variables("$(LOOP)", &f)
            .unwrap_err()
            .contains("potentially unbounded recursion"));
        assert!(expand_make_variables("$FOO", &f)
            .unwrap_err()
            .contains("'$FOO' syntax is not supported"));
        assert!(expand_make_variables("${FOO}", &f)
            .unwrap_err()
            .contains("'${FOO}' syntax is not supported"));
        assert_eq!(
            expand_make_variables("a$", &f).unwrap_err(),
            "unterminated $"
        );
    }

    #[test]
    fn shell_tokenize_matches_bazel() {
        assert_eq!(
            shell_tokenize("a b\tc\n\"d\\n\"").unwrap(),
            vec!["a", "b", "c\nd\\n"]
        );
        assert_eq!(
            shell_tokenize("-Dfoo=\"\\'foo\" -Dbar").unwrap(),
            vec!["-Dfoo=\\'foo", "-Dbar"]
        );
        assert_eq!(
            shell_tokenize("foo\\").unwrap_err(),
            "backslash at end of string"
        );
        assert_eq!(
            shell_tokenize("\"foo").unwrap_err(),
            "unterminated quotation"
        );
    }

    #[test]
    fn unified_expander_and_repo_mapping() {
        let mut m = BTreeMap::new();
        m.insert(
            lbl("@@canon_repo//p:f"),
            vec![Artifact::source(
                PathFragment::create("p/file with space"),
                lbl("@@canon_repo//p:f"),
            )],
        );
        let mut entries = std::collections::HashMap::new();
        entries.insert(
            "myrepo".to_string(),
            zel_cmdline::RepositoryName::create_unvalidated("canon_repo"),
        );
        let rm = RepositoryMapping::new(entries, zel_cmdline::RepositoryName::main());
        let root = lbl("//a:r");
        let vars = |n: &str| match n {
            "LOC_VAR" => Some("$(location @myrepo//p:f)".to_string()),
            _ => None,
        };
        let exp = expand_with_locations(
            "x $(LOC_VAR) y",
            &vars,
            &m,
            &root,
            LocationMode::Exec,
            "_main",
            Some(&rm),
            true,
        )
        .unwrap();
        assert_eq!(exp.text, "x 'external\\canon_repo\\p\\file with space' y");
        assert_eq!(
            exp.looked_up_variables,
            vec!["LOC_VAR".to_string(), "location".to_string()]
        );
        assert_eq!(
            expand_single_variable("$(LOC_VAR)", &|n| vars(n).ok_or_else(|| "undef".into()))
                .unwrap(),
            Some("$(location @myrepo//p:f)".to_string())
        );
        assert_eq!(
            expand_single_variable("a $(LOC_VAR)", &|_| Ok("x".into())).unwrap(),
            None
        );
    }
}
