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

//! `package_group` contents (`PackageSpecification`, `PackageGroupContents`) and visibility
//! label classification (`RuleVisibility`, `PackageGroupsRuleVisibility`).

use std::collections::{BTreeMap, BTreeSet};
use zel_cmdline::{Label, PackageIdentifier, RepositoryMapping, RepositoryName};
use zel_vfs::PathFragment;

/// One entry of a `package_group(packages = [...])` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageSpec {
    /// `public`: all packages.
    AllPackages,
    /// `private`: no packages.
    NoPackages,
    /// `//pkg`: exactly this package.
    Single(PackageIdentifier),
    /// `//pkg/...`: this package and all packages below it.
    AllBeneath(PackageIdentifier),
    /// `-<spec>`: negation (applied at the group level).
    Negative(Box<PackageSpec>),
}

impl PackageSpec {
    /// Parses a spec like `PackageSpecification.fromString(mapping, repo, spec, true, true)`.
    pub fn parse(
        mapping: &RepositoryMapping,
        repo: &RepositoryName,
        spec: &str,
    ) -> Result<Self, String> {
        let (negative, spec) = match spec.strip_prefix('-') {
            Some(rest) => {
                if rest == "public" || rest == "private" {
                    return Err(format!("Cannot negate \"{rest}\" package specification"));
                }
                (true, rest)
            }
            None => (false, spec),
        };
        let positive = Self::parse_positive(mapping, repo, spec)?;
        Ok(if negative {
            Self::Negative(Box::new(positive))
        } else {
            positive
        })
    }

    fn parse_positive(
        mapping: &RepositoryMapping,
        repo: &RepositoryName,
        spec: &str,
    ) -> Result<Self, String> {
        match spec {
            "public" => return Ok(Self::AllPackages),
            "private" => return Ok(Self::NoPackages),
            _ => {}
        }
        if !spec.starts_with("//") && !spec.starts_with('@') {
            return Err(format!(
                "invalid package name '{spec}': must start with '//', '@', or be 'public' or 'private'"
            ));
        }
        // Split off the repository.
        let (repo_name, path) = if let Some(rest) = spec.strip_prefix('@') {
            let canonical = rest.starts_with('@');
            let rest = rest.trim_start_matches('@');
            let idx = rest
                .find("//")
                .ok_or_else(|| format!("invalid package name '{spec}': missing '//'"))?;
            let r = &rest[..idx];
            let repo = if canonical {
                RepositoryName::create_unvalidated(r)
            } else {
                mapping.get(r)
            };
            (repo, &rest[idx + 2..])
        } else {
            (repo.clone(), &spec[2..])
        };
        let (pkg, beneath) = if path == "..." {
            ("", true)
        } else if let Some(p) = path.strip_suffix("/...") {
            (p, true)
        } else {
            (path, false)
        };
        // A `//pkg:all`-style spec is not valid here.
        if pkg.contains(':') {
            return Err(format!(
                "invalid package name '{pkg}': package names may not contain ':'"
            ));
        }
        let id = PackageIdentifier::create(repo_name, PathFragment::create(pkg))
            .map_err(|e| e.to_string())?;
        Ok(if beneath {
            Self::AllBeneath(id)
        } else {
            Self::Single(id)
        })
    }

    fn matches_positive(&self, pkg: &PackageIdentifier) -> bool {
        match self {
            Self::AllPackages => true,
            Self::NoPackages => false,
            Self::Single(p) => p == pkg,
            Self::AllBeneath(p) => {
                p.repository() == pkg.repository()
                    && pkg.package_fragment().starts_with(p.package_fragment())
            }
            Self::Negative(_) => false,
        }
    }
}

/// The evaluated contents of a package group: its own specs plus transitive `includes`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageGroupContents {
    pub specs: Vec<PackageSpec>,
}

impl PackageGroupContents {
    /// `true` iff `pkg` matches some positive spec and no negative spec.
    pub fn contains(&self, pkg: &PackageIdentifier) -> bool {
        let mut positive = false;
        for s in &self.specs {
            match s {
                PackageSpec::Negative(inner) => {
                    if inner.matches_positive(pkg) {
                        return false;
                    }
                }
                other => positive |= other.matches_positive(pkg),
            }
        }
        positive
    }
}

/// Resolves the transitive contents of the package group `group`, following `includes`.
///
/// `lookup` returns `(direct specs, included group labels)` for a package group label.
pub fn resolve_package_group(
    group: &Label,
    lookup: &mut dyn FnMut(&Label) -> Result<(Vec<PackageSpec>, Vec<Label>), String>,
) -> Result<PackageGroupContents, String> {
    let mut seen = BTreeSet::new();
    let mut out = PackageGroupContents::default();
    let mut stack = vec![group.clone()];
    while let Some(g) = stack.pop() {
        if !seen.insert(g.clone()) {
            continue;
        }
        let (specs, includes) = lookup(&g)?;
        out.specs.extend(specs);
        stack.extend(includes);
    }
    Ok(out)
}

/// Parsed `visibility` attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Private,
    /// Visible to the packages of these `package_group` labels (and `//pkg:__pkg__` /
    /// `//pkg:__subpackages__` pseudo-labels).
    Groups(Vec<Label>),
}

/// Classifies a converted `visibility` list the way `BuildType.NODEP_LABEL_LIST` visibility
/// conversion does, producing Bazel's error messages.
pub fn parse_visibility(labels: &[Label]) -> Result<Visibility, String> {
    let mut public = false;
    let mut private = false;
    let mut groups = Vec::new();
    for l in labels {
        let f = l.canonical_form();
        if f == "//visibility:public" {
            public = true;
        } else if f == "//visibility:private" {
            private = true;
        } else {
            groups.push(l.clone());
        }
    }
    if (public || private) && (groups.len() + usize::from(public) + usize::from(private)) > 1 {
        return Err(
            "Public or private visibility labels (e.g. //visibility:public or //visibility:private) cannot be used in combination with other labels"
                .to_string(),
        );
    }
    Ok(if public {
        Visibility::Public
    } else if private || groups.is_empty() {
        Visibility::Private
    } else {
        Visibility::Groups(groups)
    })
}

/// Checks whether `from` may see a target whose visibility is `vis`, owned by package `owner`.
///
/// `groups` maps package-group labels to their resolved contents.
pub fn is_visible(
    vis: &Visibility,
    owner: &PackageIdentifier,
    from: &PackageIdentifier,
    groups: &BTreeMap<Label, PackageGroupContents>,
) -> bool {
    if owner == from {
        return true;
    }
    match vis {
        Visibility::Public => true,
        Visibility::Private => false,
        Visibility::Groups(gs) => gs.iter().any(|g| {
            if g.name() == "__pkg__" {
                return g.package_identifier() == from;
            }
            if g.name() == "__subpackages__" {
                return g.package_identifier().repository() == from.repository()
                    && from
                        .package_fragment()
                        .starts_with(g.package_identifier().package_fragment());
            }
            groups.get(g).map(|c| c.contains(from)).unwrap_or(false)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(p: &str) -> PackageIdentifier {
        PackageIdentifier::create_in_main_repo(p).unwrap()
    }

    fn spec(s: &str) -> Result<PackageSpec, String> {
        PackageSpec::parse(&RepositoryMapping::always_fallback(), &RepositoryName::main(), s)
    }

    #[test]
    fn spec_parsing_and_matching() {
        let g = PackageGroupContents {
            specs: vec![spec("//a/...").unwrap(), spec("//b").unwrap(), spec("-//a/secret/...").unwrap()],
        };
        assert!(g.contains(&pkg("a")));
        assert!(g.contains(&pkg("a/x/y")));
        assert!(g.contains(&pkg("b")));
        assert!(!g.contains(&pkg("b/c")));
        assert!(!g.contains(&pkg("a/secret")));
        assert!(!g.contains(&pkg("a/secret/z")));
        assert!(!g.contains(&pkg("c")));
        let root = PackageGroupContents { specs: vec![spec("//...").unwrap()] };
        assert!(root.contains(&pkg("")) && root.contains(&pkg("any/thing")));
        assert!(PackageGroupContents { specs: vec![spec("public").unwrap()] }.contains(&pkg("q")));
        assert!(!PackageGroupContents { specs: vec![spec("private").unwrap()] }.contains(&pkg("q")));
    }

    #[test]
    fn spec_errors() {
        assert_eq!(
            spec("foo/bar").unwrap_err(),
            "invalid package name 'foo/bar': must start with '//', '@', or be 'public' or 'private'"
        );
        assert_eq!(spec("-public").unwrap_err(), "Cannot negate \"public\" package specification");
        assert!(spec("//a:b").is_err());
    }

    #[test]
    fn visibility_semantics() {
        let l = |s: &str| Label::parse_canonical(s).unwrap();
        assert_eq!(parse_visibility(&[l("//visibility:public")]).unwrap(), Visibility::Public);
        assert_eq!(parse_visibility(&[]).unwrap(), Visibility::Private);
        assert!(parse_visibility(&[l("//visibility:public"), l("//x:g")])
            .unwrap_err()
            .starts_with("Public or private visibility labels"));
        let vis = parse_visibility(&[l("//lib:__subpackages__"), l("//x:g")]).unwrap();
        let mut groups = BTreeMap::new();
        groups.insert(
            l("//x:g"),
            PackageGroupContents { specs: vec![spec("//friends").unwrap()] },
        );
        assert!(is_visible(&vis, &pkg("owner"), &pkg("lib/sub"), &groups));
        assert!(is_visible(&vis, &pkg("owner"), &pkg("friends"), &groups));
        assert!(!is_visible(&vis, &pkg("owner"), &pkg("stranger"), &groups));
        assert!(is_visible(&vis, &pkg("owner"), &pkg("owner"), &groups));
        let pkg_only = parse_visibility(&[l("//lib:__pkg__")]).unwrap();
        assert!(is_visible(&pkg_only, &pkg("o"), &pkg("lib"), &groups));
        assert!(!is_visible(&pkg_only, &pkg("o"), &pkg("lib/sub"), &groups));
    }
}
