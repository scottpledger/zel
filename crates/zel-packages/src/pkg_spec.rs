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

use std::collections::BTreeMap;
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
        // `//pkg:__pkg__` and `//pkg:__subpackages__` (in any repo form) are valid package specs.
        if let Ok(mut label) = Label::parse_with_repo_mapping(spec, mapping) {
            let pkg_str = label.package_identifier().package_fragment().path_string();
            let pinned_main = matches!(pkg_str, "visibility" | "conditions");
            if spec.starts_with("//") && !repo.is_main() && !pinned_main {
                if let Ok(rebound_pkg) = PackageIdentifier::create(
                    repo.clone(),
                    label.package_identifier().package_fragment().clone(),
                ) {
                    if let Ok(rebound) = Label::create(rebound_pkg, label.name()) {
                        label = rebound;
                    }
                }
            }
            if label.name() == "__pkg__" {
                return Ok(Self::Single(label.package_identifier().clone()));
            }
            if label.name() == "__subpackages__" {
                return Ok(Self::AllBeneath(label.package_identifier().clone()));
            }
        }
        // Split off the repository.
        let (mut repo_name, path) = if let Some(rest) = spec.strip_prefix('@') {
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
        if spec.starts_with("//") && matches!(pkg, "visibility" | "conditions") {
            repo_name = RepositoryName::main();
        }
        // A `//pkg:all`-style spec is not valid here.
        if pkg.contains(':') {
            return Err(format!(
                "invalid package name '{spec}': package names may not contain ':'"
            ));
        }
        let id = PackageIdentifier::create(repo_name, PathFragment::create(pkg))
            .map_err(|e| format!("invalid package name '{spec}': {e}"))?;
        Ok(if beneath {
            Self::AllBeneath(id)
        } else {
            Self::Single(id)
        })
    }

    pub fn matches_positive(&self, pkg: &PackageIdentifier) -> bool {
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
    pub includes: Vec<PackageGroupContents>,
}

impl PackageGroupContents {
    pub fn from_specs(specs: Vec<PackageSpec>) -> Self {
        Self {
            specs,
            includes: Vec::new(),
        }
    }

    fn own_contains(&self, pkg: &PackageIdentifier) -> bool {
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

    /// `true` iff `pkg` matches this group's own specs (positive and not negative) OR any
    /// transitive `includes` group.
    pub fn contains(&self, pkg: &PackageIdentifier) -> bool {
        self.own_contains(pkg) || self.includes.iter().any(|inc| inc.contains(pkg))
    }

    /// Serializes canonicalized specs so `PackageSpecificationInfo.contains` can evaluate them.
    pub fn encode_canonical(&self) -> String {
        fn enc_pos(spec: &PackageSpec) -> String {
            match spec {
                PackageSpec::AllPackages => "*".to_string(),
                PackageSpec::NoPackages => "-".to_string(),
                PackageSpec::Single(id) => format!(
                    "S\t{}\t{}",
                    id.repository().name(),
                    id.package_fragment().path_string()
                ),
                PackageSpec::AllBeneath(id) => format!(
                    "B\t{}\t{}",
                    id.repository().name(),
                    id.package_fragment().path_string()
                ),
                PackageSpec::Negative(inner) => enc_pos(inner),
            }
        }
        let mut lines = Vec::with_capacity(self.specs.len());
        for s in &self.specs {
            match s {
                PackageSpec::Negative(inner) => lines.push(format!("-\t{}", enc_pos(inner))),
                other => lines.push(format!("+\t{}", enc_pos(other))),
            }
        }
        lines.join("\n")
    }

    /// Reconstructs [`PackageGroupContents`] from [`Self::encode_canonical`].
    pub fn decode_canonical(encoded: &str) -> Self {
        fn dec_pos(rest: &str) -> Option<PackageSpec> {
            if rest == "*" {
                return Some(PackageSpec::AllPackages);
            }
            if rest == "-" {
                return Some(PackageSpec::NoPackages);
            }
            let mut parts = rest.splitn(3, '\t');
            let kind = parts.next()?;
            let repo_raw = parts.next()?;
            let pkg_raw = parts.next()?;
            let repo = if repo_raw.is_empty() {
                RepositoryName::main()
            } else {
                RepositoryName::create_unvalidated(repo_raw)
            };
            let id = PackageIdentifier::create(repo, PathFragment::create(pkg_raw)).ok()?;
            match kind {
                "S" => Some(PackageSpec::Single(id)),
                "B" => Some(PackageSpec::AllBeneath(id)),
                _ => None,
            }
        }
        let mut specs = Vec::new();
        for line in encoded.lines() {
            if let Some(rest) = line.strip_prefix("+\t") {
                if let Some(s) = dec_pos(rest) {
                    specs.push(s);
                }
            } else if let Some(rest) = line.strip_prefix("-\t") {
                if let Some(s) = dec_pos(rest) {
                    specs.push(PackageSpec::Negative(Box::new(s)));
                }
            }
        }
        Self::from_specs(specs)
    }

    /// Reconstructs one or more [`PackageGroupContents`] blocks separated by `"\n---\n"`.
    pub fn decode_canonical_groups(encoded: &str) -> Vec<Self> {
        if encoded.is_empty() {
            return Vec::new();
        }
        encoded
            .split("\n---\n")
            .filter(|block| !block.is_empty())
            .map(Self::decode_canonical)
            .collect()
    }
}

/// Resolves the transitive contents of the package group `group`, following `includes`.
///
/// `lookup` returns `(direct specs, included group labels)` for a package group label.
pub type PackageGroupLookup<'a> =
    &'a mut dyn FnMut(&Label) -> Result<(Vec<PackageSpec>, Vec<Label>), String>;

pub fn resolve_package_group(
    group: &Label,
    lookup: PackageGroupLookup<'_>,
) -> Result<PackageGroupContents, String> {
    fn visit(
        g: &Label,
        lookup: PackageGroupLookup<'_>,
        memo: &mut BTreeMap<Label, PackageGroupContents>,
        visiting: &mut Vec<Label>,
    ) -> Result<PackageGroupContents, String> {
        if let Some(pos) = visiting.iter().position(|x| x == g) {
            let mut cyc: Vec<String> = visiting[pos..].iter().map(|l| l.canonical_form()).collect();
            cyc.push(g.canonical_form());
            return Err(format!("cycle in dependency graph: {}", cyc.join(" -> ")));
        }
        if let Some(cached) = memo.get(g) {
            return Ok(cached.clone());
        }
        visiting.push(g.clone());
        let (specs, includes) = lookup(g)?;
        let mut inc_contents = Vec::with_capacity(includes.len());
        for inc in includes {
            inc_contents.push(visit(&inc, lookup, memo, visiting)?);
        }
        visiting.pop();
        let node = PackageGroupContents {
            specs,
            includes: inc_contents,
        };
        memo.insert(g.clone(), node.clone());
        Ok(node)
    }
    let mut memo = BTreeMap::new();
    let mut visiting = Vec::new();
    visit(group, lookup, &mut memo, &mut visiting)
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

/// Validates and normalizes visibility labels per `RuleVisibility.validateAndSimplify`:
/// - rejects invalid labels in the `//visibility` package (anything other than `public`, `private`,
///   `__pkg__`, `__subpackages__`) before any simplification
/// - collapses any list containing `//visibility:public` to `[//visibility:public]`
/// - turns an empty or all-`//visibility:private` list into `[//visibility:private]`
/// - drops `//visibility:private` when mixed with other labels, preserving order and duplicates
pub fn validate_and_simplify_visibility(labels: &[Label]) -> Result<Vec<Label>, String> {
    let public_label = Label::parse_canonical("//visibility:public").unwrap();
    let private_label = Label::parse_canonical("//visibility:private").unwrap();
    let mut has_public = false;
    let mut num_private = 0usize;
    for l in labels {
        if *l == public_label {
            has_public = true;
        } else if *l == private_label {
            num_private += 1;
        } else if l.repository().is_main()
            && l.package_name() == "visibility"
            && l.name() != "__pkg__"
            && l.name() != "__subpackages__"
        {
            return Err(format!(
                "Invalid visibility label '{}'; did you mean //visibility:public or //visibility:private?",
                l.canonical_form()
            ));
        }
    }
    if has_public {
        return Ok(vec![public_label]);
    }
    if num_private == labels.len() {
        return Ok(vec![private_label]);
    }
    if num_private == 0 {
        return Ok(labels.to_vec());
    }
    Ok(labels
        .iter()
        .filter(|l| **l != private_label)
        .cloned()
        .collect())
}

/// Concatenates a validated/simplified visibility list with `pkg:__pkg__` per
/// `RuleVisibility.concatWithPackage`.
pub fn concat_visibility_with_package(labels: &[Label], pkg: &PackageIdentifier) -> Vec<Label> {
    let public_label = Label::parse_canonical("//visibility:public").unwrap();
    let private_label = Label::parse_canonical("//visibility:private").unwrap();
    let pkg_item = Label::create(pkg.clone(), "__pkg__").unwrap();
    if labels.len() == 1 && labels[0] == public_label {
        return vec![public_label];
    }
    if labels.is_empty() || (labels.len() == 1 && labels[0] == private_label) {
        return vec![pkg_item];
    }
    if labels.contains(&pkg_item) {
        return labels.to_vec();
    }
    let mut out = labels.to_vec();
    out.push(pkg_item);
    out
}

/// Loadable `package_group` dependency labels (`RuleVisibility.getDependencyLabels`).
pub fn visibility_dependency_labels(labels: &[Label]) -> Vec<Label> {
    labels
        .iter()
        .filter(|l| {
            !(l.package_name() == "visibility" && (l.name() == "public" || l.name() == "private"))
                && l.name() != "__pkg__"
                && l.name() != "__subpackages__"
        })
        .cloned()
        .collect()
}

/// Classifies a converted `visibility` list using `RuleVisibility.validateAndSimplify`.
pub fn parse_visibility(labels: &[Label]) -> Result<Visibility, String> {
    let simplified = validate_and_simplify_visibility(labels)?;
    let public_label = Label::parse_canonical("//visibility:public").unwrap();
    let private_label = Label::parse_canonical("//visibility:private").unwrap();
    if simplified.len() == 1 && simplified[0] == public_label {
        Ok(Visibility::Public)
    } else if simplified.len() == 1 && simplified[0] == private_label {
        Ok(Visibility::Private)
    } else {
        Ok(Visibility::Groups(simplified))
    }
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
        PackageSpec::parse(
            &RepositoryMapping::always_fallback(),
            &RepositoryName::main(),
            s,
        )
    }

    #[test]
    fn spec_parsing_and_matching() {
        let g = PackageGroupContents::from_specs(vec![
            spec("//a/...").unwrap(),
            spec("//b").unwrap(),
            spec("-//a/secret/...").unwrap(),
        ]);
        assert!(g.contains(&pkg("a")));
        assert!(g.contains(&pkg("a/x/y")));
        assert!(g.contains(&pkg("b")));
        assert!(!g.contains(&pkg("b/c")));
        assert!(!g.contains(&pkg("a/secret")));
        assert!(!g.contains(&pkg("a/secret/z")));
        assert!(!g.contains(&pkg("c")));
        let root = PackageGroupContents::from_specs(vec![spec("//...").unwrap()]);
        assert!(root.contains(&pkg("")) && root.contains(&pkg("any/thing")));
        assert!(
            PackageGroupContents::from_specs(vec![spec("public").unwrap()]).contains(&pkg("q"))
        );
        assert!(
            !PackageGroupContents::from_specs(vec![spec("private").unwrap()]).contains(&pkg("q"))
        );
        // Negation in one included group does not veto another included group.
        let parent = PackageGroupContents {
            specs: Vec::new(),
            includes: vec![
                PackageGroupContents::from_specs(vec![
                    spec("//a/...").unwrap(),
                    spec("-//a/secret").unwrap(),
                ]),
                PackageGroupContents::from_specs(vec![spec("//a/secret").unwrap()]),
            ],
        };
        assert!(parent.contains(&pkg("a/secret")));
    }

    #[test]
    fn spec_errors() {
        assert_eq!(
            spec("foo/bar").unwrap_err(),
            "invalid package name 'foo/bar': must start with '//', '@', or be 'public' or 'private'"
        );
        assert_eq!(
            spec("-public").unwrap_err(),
            "Cannot negate \"public\" package specification"
        );
        assert!(spec("//a:b").is_err());
        assert_eq!(spec("//a:__pkg__").unwrap(), PackageSpec::Single(pkg("a")));
        assert_eq!(
            spec("//a:__subpackages__").unwrap(),
            PackageSpec::AllBeneath(pkg("a"))
        );
    }

    #[test]
    fn visibility_semantics() {
        let l = |s: &str| Label::parse_canonical(s).unwrap();
        assert_eq!(
            parse_visibility(&[l("//visibility:public")]).unwrap(),
            Visibility::Public
        );
        assert_eq!(parse_visibility(&[]).unwrap(), Visibility::Private);
        assert_eq!(
            parse_visibility(&[l("//visibility:public"), l("//x:g")]).unwrap(),
            Visibility::Public
        );
        assert_eq!(
            parse_visibility(&[l("//visibility:private"), l("//x:g")]).unwrap(),
            Visibility::Groups(vec![l("//x:g")])
        );
        assert!(
            parse_visibility(&[l("//visibility:public"), l("//visibility:typo")])
                .unwrap_err()
                .contains("Invalid visibility label '//visibility:typo'")
        );
        let vis = parse_visibility(&[l("//lib:__subpackages__"), l("//x:g")]).unwrap();
        let mut groups = BTreeMap::new();
        groups.insert(
            l("//x:g"),
            PackageGroupContents::from_specs(vec![spec("//friends").unwrap()]),
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
