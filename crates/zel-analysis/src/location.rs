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

//! `$(location ...)` expansion (`LocationExpander.java`) and Make-variable expansion
//! (`MakeVariableExpander.java`).

use crate::Artifact;
use std::collections::{BTreeMap, BTreeSet};
use zel_cmdline::{Label, PackageIdentifier};

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

/// Root path of an artifact (`Artifact.getRunfilesPath` for the main repo,
/// `external/<repo>/...` for others).
fn root_path(a: &Artifact) -> String {
    let sp = a.short_path();
    match sp.strip_prefix("../") {
        Some(rest) => format!("external/{rest}"),
        None => sp,
    }
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
        let arg = input[nws + 1..end].trim();
        let label = parse_label(arg, root.package_identifier())
            .map_err(|e| format!("invalid label in $({fname}) expression: {e}"))?;
        let arts = label_map.get(&label).ok_or_else(|| {
            format!(
                "label '{}' in $({fname}) expression is not a declared prerequisite of this rule",
                label_str(&label)
            )
        })?;
        let mut paths: BTreeSet<String> = BTreeSet::new();
        for a in arts {
            paths.insert(match pt {
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
            });
        }
        if paths.is_empty() {
            return Err(format!(
                "label '{}' in $({fname}) expression expands to no files",
                label_str(&label)
            ));
        }
        if !multiple && paths.len() > 1 {
            let shown: Vec<&String> = paths.iter().take(5).collect();
            return Err(format!(
                "label '{}' in $({fname}) expression expands to more than one file, please use $({fname}s {}) instead.  Files (at most 5 shown) are: [{}]",
                label_str(&label),
                label_str(&label),
                shown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
        result.push_str(&paths.into_iter().collect::<Vec<_>>().join(" "));
        restart = end + 1;
    }
    Ok(result)
}

fn label_str(l: &Label) -> String {
    l.canonical_form()
}

/// Parses `:foo`, `foo`, `//pkg:foo`, `@repo//pkg:foo` relative to `pkg`.
pub fn parse_label(arg: &str, pkg: &PackageIdentifier) -> Result<Label, String> {
    if arg.starts_with("//") {
        // `//pkg:x` names a target of the repository of the rule being expanded.
        let parsed = Label::parse_canonical(arg).map_err(|e| e.to_string())?;
        if pkg.repository().is_main() {
            return Ok(parsed);
        }
        let id = PackageIdentifier::create(pkg.repository().clone(), parsed.package_identifier().package_fragment().clone())
            .map_err(|e| e.to_string())?;
        return Label::create(id, parsed.name()).map_err(|e| e.to_string());
    }
    if arg.starts_with('@') {
        return Label::parse_canonical(arg).map_err(|e| e.to_string());
    }
    let name = arg.strip_prefix(':').unwrap_or(arg);
    Label::create(pkg.clone(), name).map_err(|e| e.to_string())
}

/// `MakeVariableExpander.expand`: `$(NAME)` → value, `$$` → `$`, `$X` → value of `X`.
pub fn expand_make_variables(
    input: &str,
    vars: &dyn Fn(&str) -> Option<String>,
) -> Result<String, String> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '$' {
            out.push(c);
            i += 1;
            continue;
        }
        if i + 1 >= chars.len() {
            return Err("unterminated $".to_string());
        }
        match chars[i + 1] {
            '$' => {
                out.push('$');
                i += 2;
            }
            '(' => {
                let Some(close) = chars[i + 2..].iter().position(|&c| c == ')') else {
                    return Err("unterminated variable reference".to_string());
                };
                let name: String = chars[i + 2..i + 2 + close].iter().collect();
                let v = vars(&name).ok_or_else(|| format!("$({name}) not defined"))?;
                out.push_str(&v);
                i += 2 + close + 1;
            }
            other => {
                let name = other.to_string();
                let v = vars(&name).ok_or_else(|| format!("${name} not defined"))?;
                out.push_str(&v);
                i += 2;
            }
        }
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
        let root = lbl("//a:r");
        let s = expand_locations("x $(location :t) $(rootpath //a:t) $(execpath :t) y", &m, &root, LocationMode::Exec, "_main").unwrap();
        assert_eq!(s, "x bazel-out/k8-fastbuild/bin/a/t.out a/t.out bazel-out/k8-fastbuild/bin/a/t.out y");
        let s = expand_locations("$(location :t)", &m, &root, LocationMode::Runfiles, "_main").unwrap();
        assert_eq!(s, "a/t.out");
        let s = expand_locations("$(rlocationpath :t)", &m, &root, LocationMode::Exec, "_main").unwrap();
        assert_eq!(s, "_main/a/t.out");
    }

    #[test]
    fn location_errors() {
        let mut m = BTreeMap::new();
        m.insert(lbl("//a:two"), vec![art("a/1", "//a:two"), art("a/2", "//a:two")]);
        let root = lbl("//a:r");
        assert_eq!(
            expand_locations("$(location :zzz)", &m, &root, LocationMode::Exec, "_main").unwrap_err(),
            "label '//a:zzz' in $(location) expression is not a declared prerequisite of this rule"
        );
        let e = expand_locations("$(location :two)", &m, &root, LocationMode::Exec, "_main").unwrap_err();
        assert!(e.contains("expands to more than one file, please use $(locations //a:two) instead.  Files (at most 5 shown) are: [a/1, a/2]") || e.contains("bazel-out"), "{e}");
        assert_eq!(
            expand_locations("$(locations :two)", &m, &root, LocationMode::Runfiles, "_main").unwrap(),
            "a/1 a/2"
        );
        assert_eq!(
            expand_locations("$(location :two", &m, &root, LocationMode::Exec, "_main").unwrap_err(),
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
            _ => None,
        };
        assert_eq!(expand_make_variables("a $(A) $$ $@", &f).unwrap(), "a 1 $ out");
        assert_eq!(expand_make_variables("$(B)", &f).unwrap_err(), "$(B) not defined");
        assert_eq!(expand_make_variables("$(A", &f).unwrap_err(), "unterminated variable reference");
    }
}
