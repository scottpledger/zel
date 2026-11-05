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

//! Bazel Label, PackageIdentifier, RepositoryName, RepositoryMapping, and TargetPattern parsing.
//! Mirrors `src/main/java/com/google/devtools/build/lib/cmdline`.

use std::collections::HashMap;
use std::fmt;
use zel_vfs::PathFragment;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelSyntaxError(pub String);

impl fmt::Display for LabelSyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LabelSyntaxError {}

/// Exact port of `com.google.devtools.build.lib.cmdline.LabelValidator`.
pub mod validator {
    pub const PACKAGE_NAME_ERROR: &str =
        "package names may contain A-Z, a-z, 0-9, or any of ' !\"#$%&'()*+,-./;<=>?[]^_`{|}~' (any ASCII character except 0-31, 127, ':', or '\\')";
    pub const PACKAGE_NAME_DOT_ERROR: &str = "package name component contains only '.' characters";

    fn is_allowed_in_package_name(c: char) -> bool {
        c.is_ascii_alphanumeric() || " !\"#$%&'()*+,-./;<=>?@[]^_`{|}~".contains(c)
    }

    fn is_always_allowed_in_target(c: char) -> bool {
        if !c.is_ascii() {
            return true;
        }
        c.is_ascii_alphanumeric() || " \"#$&'()*+,;<=>?[]{|}~".contains(c) || "!%-@^_`".contains(c)
    }

    pub fn validate_package_name(package_name: &str) -> Result<(), &'static str> {
        if package_name.is_empty() {
            return Ok(());
        }
        if package_name.starts_with('/') {
            return Err("package names may not start with '/'");
        }
        if !package_name.chars().all(is_allowed_in_package_name) {
            return Err(PACKAGE_NAME_ERROR);
        }
        if package_name.ends_with('/') {
            return Err("package names may not end with '/'");
        }

        let bytes = package_name.as_bytes();
        let mut non_dot = false;
        let mut last_slash = true;
        for i in (-1..(bytes.len() as isize)).rev() {
            let c = if i >= 0 { bytes[i as usize] } else { b'/' };
            if c == b'/' {
                if last_slash {
                    return Err("package names may not contain '//' path separators");
                }
                if !non_dot {
                    return Err(PACKAGE_NAME_DOT_ERROR);
                }
                non_dot = false;
                last_slash = true;
            } else {
                if c != b'.' {
                    non_dot = true;
                }
                last_slash = false;
            }
        }
        Ok(())
    }

    pub fn validate_target_name(target_name: &str) -> Result<(), String> {
        if target_name.is_empty() {
            return Err("empty target name".to_string());
        }
        if target_name.starts_with('/') {
            return Err("target names may not start with '/'".to_string());
        }
        if target_name.starts_with('.') {
            if target_name.starts_with("../") || target_name == ".." {
                return Err("target names may not contain up-level references '..'".to_string());
            } else if target_name == "." {
                return Ok(());
            } else if target_name.starts_with("./") {
                return Err("target names may not contain '.' as a path segment".to_string());
            }
        }
        if target_name.ends_with('\r') {
            return Err(
                "target names may not end with carriage returns (perhaps the input source is CRLF-terminated)"
                    .to_string(),
            );
        }

        let bytes = target_name.as_bytes();
        for (ii, c) in target_name.char_indices() {
            if is_always_allowed_in_target(c) || c == '.' {
                continue;
            }
            if c == '/' {
                if target_name[ii..].starts_with("/../") {
                    return Err("target names may not contain up-level references '..'".to_string());
                } else if target_name[ii..].starts_with("/./") {
                    return Err("target names may not contain '.' as a path segment".to_string());
                } else if target_name[ii..].starts_with("//") {
                    return Err("target names may not contain '//' path separators".to_string());
                }
                continue;
            }
            let b = bytes[ii];
            if b <= 0x1f || b == 0x7f {
                return Err(format!(
                    "target names may not contain non-printable characters: '\\x{:02X}'",
                    b
                ));
            }
            return Err(format!("target names may not contain '{c}'"));
        }

        if target_name.ends_with("/..") {
            return Err("target names may not contain up-level references '..'".to_string());
        }
        if target_name.ends_with("/.") {
            return Ok(());
        }
        if target_name.ends_with('/') {
            return Err("target names may not end with '/'".to_string());
        }
        Ok(())
    }
}

/// The canonical name of an external repository, matching `RepositoryName.java`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct RepositoryName {
    name: String,
}

impl RepositoryName {
    pub fn main() -> Self {
        Self {
            name: String::new(),
        }
    }

    pub fn bazel_tools() -> Self {
        Self {
            name: "bazel_tools".to_string(),
        }
    }

    pub fn builtins() -> Self {
        Self {
            name: "_builtins".to_string(),
        }
    }

    pub fn create(name: &str) -> Result<Self, LabelSyntaxError> {
        Self::validate(name)?;
        Ok(Self {
            name: name.to_string(),
        })
    }

    pub fn create_unvalidated(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    pub fn validate(name: &str) -> Result<(), LabelSyntaxError> {
        if name.is_empty() || name == "_builtins" {
            return Ok(());
        }
        if name == "." || name == ".." {
            return Err(LabelSyntaxError(format!(
                "invalid repository name '{name}': repo names are not allowed to be '{name}'"
            )));
        }
        let valid = name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+'));
        if !valid {
            return Err(LabelSyntaxError(format!(
                "invalid repository name '{name}': repo names may contain only A-Z, a-z, 0-9, '-', '_', '.' and '+'"
            )));
        }
        Ok(())
    }

    pub fn is_apparent(name: &str) -> bool {
        !name.is_empty() && !name.contains('+')
    }

    pub fn is_main(&self) -> bool {
        self.name.is_empty()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn name_with_at(&self) -> String {
        format!("@@{}", self.name)
    }

    pub fn canonical_form(&self) -> String {
        if self.is_main() {
            String::new()
        } else {
            self.name_with_at()
        }
    }
}

impl fmt::Display for RepositoryName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical_form())
    }
}

/// Maps apparent repository names (`@foo`) to canonical repository names (`@@foo+1.0`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepositoryMapping {
    entries: HashMap<String, RepositoryName>,
    /// Apparent names in `ImmutableMap` insertion order when known (empty: unordered).
    order: Vec<String>,
    owner_repo: RepositoryName,
}

impl RepositoryMapping {
    pub fn always_fallback() -> Self {
        Self::default()
    }

    pub fn new(entries: HashMap<String, RepositoryName>, owner_repo: RepositoryName) -> Self {
        Self {
            entries,
            order: Vec::new(),
            owner_repo,
        }
    }

    /// A mapping that remembers the iteration order of Bazel's `ImmutableMap` of entries.
    pub fn new_ordered(ordered: Vec<(String, RepositoryName)>, owner_repo: RepositoryName) -> Self {
        let order: Vec<String> = ordered.iter().map(|(a, _)| a.clone()).collect();
        Self { entries: ordered.into_iter().collect(), order, owner_repo }
    }

    /// All `(apparent name, canonical repository)` entries (used to invert the mapping when
    /// printing labels in their display form).
    pub fn entries(&self) -> Box<dyn Iterator<Item = (&String, &RepositoryName)> + '_> {
        if self.order.len() == self.entries.len() && !self.order.is_empty() {
            Box::new(self.order.iter().filter_map(move |k| self.entries.get_key_value(k)))
        } else {
            Box::new(self.entries.iter())
        }
    }

    pub fn get(&self, apparent_name: &str) -> RepositoryName {
        if apparent_name.is_empty() {
            return self.owner_repo.clone();
        }
        self.entries
            .get(apparent_name)
            .cloned()
            .unwrap_or_else(|| RepositoryName::create_unvalidated(apparent_name))
    }
}

/// Identifies a Bazel package (`RepositoryName` + package `PathFragment`), matching `PackageIdentifier.java`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageIdentifier {
    repository: RepositoryName,
    pkg_fragment: PathFragment,
}

impl PackageIdentifier {
    pub fn create(
        repository: RepositoryName,
        pkg_fragment: PathFragment,
    ) -> Result<Self, LabelSyntaxError> {
        if pkg_fragment.is_absolute() {
            return Err(LabelSyntaxError(format!(
                "invalid package name '{}': package names may not start with '/'",
                pkg_fragment
            )));
        }
        if let Err(msg) = validator::validate_package_name(pkg_fragment.path_string()) {
            return Err(LabelSyntaxError(format!(
                "invalid package name '{}': {}",
                pkg_fragment, msg
            )));
        }
        Ok(Self {
            repository,
            pkg_fragment,
        })
    }

    pub fn create_in_main_repo(pkg: &str) -> Result<Self, LabelSyntaxError> {
        Self::create(RepositoryName::main(), PathFragment::create(pkg))
    }

    pub fn repository(&self) -> &RepositoryName {
        &self.repository
    }

    pub fn package_fragment(&self) -> &PathFragment {
        &self.pkg_fragment
    }

    pub fn canonical_form(&self) -> String {
        format!(
            "{}//{}",
            self.repository.canonical_form(),
            self.pkg_fragment
        )
    }
}

impl fmt::Display for PackageIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.canonical_form())
    }
}

/// `LabelParser.Parts`: the syntactic decomposition of a raw label string (`Parts.parse`), validated
/// (repo name, package name, target name) in the same order as `validateAndCreate`.
struct LabelParts {
    repo: Option<String>,
    repo_is_canonical: bool,
    pkg_is_absolute: bool,
    pkg: String,
    pkg_ends_with_triple_dots: bool,
    target: String,
}

impl LabelParts {
    fn parse(raw: &str) -> Result<Self, LabelSyntaxError> {
        let repo_is_canonical = raw.starts_with("@@");
        let double_slash = raw.find("//");
        let repo: Option<String>;
        let start_of_package: usize;
        let pkg_is_absolute: bool;
        if raw.starts_with('@') {
            match double_slash {
                None => {
                    let repo = raw[if repo_is_canonical { 2 } else { 1 }..].to_string();
                    return Self::validate_and_create(
                        Some(repo.clone()),
                        repo_is_canonical,
                        true,
                        "",
                        false,
                        &repo,
                    );
                }
                Some(d) => {
                    repo = Some(raw[if repo_is_canonical { 2 } else { 1 }..d].to_string());
                    start_of_package = d + 2;
                    pkg_is_absolute = true;
                }
            }
        } else {
            pkg_is_absolute = double_slash == Some(0);
            start_of_package = if double_slash == Some(0) { 2 } else { 0 };
            repo = None;
        }
        let colon = raw[start_of_package..].find(':').map(|i| i + start_of_package);
        let raw_pkg = &raw[start_of_package..colon.unwrap_or(raw.len())];
        let dots = raw_pkg.ends_with("/...") || raw_pkg == "...";
        let strip = |p: &str| -> String {
            if let Some(x) = p.strip_suffix("/...") {
                x.to_string()
            } else if p == "..." {
                String::new()
            } else {
                p.to_string()
            }
        };
        let (pkg, target) = if colon.is_none() && dots {
            (strip(raw_pkg), String::new())
        } else if colon.is_none() && !pkg_is_absolute {
            (String::new(), raw[start_of_package..].to_string())
        } else {
            let pkg = strip(raw_pkg);
            let target = match colon {
                Some(c) => raw[c + 1..].to_string(),
                None => pkg[pkg.rfind('/').map(|i| i + 1).unwrap_or(0)..].to_string(),
            };
            (pkg, target)
        };
        Self::validate_and_create(repo, repo_is_canonical, pkg_is_absolute, &pkg, dots, &target)
            .map(|mut p| {
                p.pkg_is_absolute = pkg_is_absolute;
                p
            })
    }

    fn validate_and_create(
        repo: Option<String>,
        repo_is_canonical: bool,
        pkg_is_absolute: bool,
        pkg: &str,
        dots: bool,
        target: &str,
    ) -> Result<Self, LabelSyntaxError> {
        if let Some(r) = &repo {
            RepositoryName::validate(r)?;
        }
        let perhaps = if pkg.ends_with(&format!("/{target}")) {
            format!(" (perhaps you meant \":{target}\"?)")
        } else {
            String::new()
        };
        if let Err(err) = validator::validate_package_name(pkg) {
            return Err(LabelSyntaxError(format!("invalid package name '{pkg}': {err}{perhaps}")));
        }
        let mut target = target.to_string();
        if !(dots && target.is_empty()) {
            if let Err(err) = validator::validate_target_name(&target) {
                return Err(LabelSyntaxError(format!(
                    "invalid target name '{target}': {err}{perhaps}"
                )));
            }
            if let Some(t) = target.strip_suffix("/.") {
                target = t.to_string();
            }
        }
        Ok(Self {
            repo,
            repo_is_canonical,
            pkg_is_absolute,
            pkg: pkg.to_string(),
            pkg_ends_with_triple_dots: dots,
            target,
        })
    }
}


/// Validates like `PackageIdentifier.parse` (`[@repo]//pkg` or `pkg`; errors as `LabelSyntaxException`).
pub fn validate_package_identifier(input: &str) -> Result<(), LabelSyntaxError> {
    if input.contains(':') {
        return Err(LabelSyntaxError(format!(
            "invalid package identifier '{input}': contains ':'"
        )));
    }
    LabelParts::parse(&format!("{input}:dummy_target")).map(|_| ())
}

/// A parsed Bazel target label (`@@repo//pkg:target`), matching `Label.java`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Label {
    package_identifier: PackageIdentifier,
    name: String,
}

impl Label {
    pub fn create(pkg: PackageIdentifier, name: &str) -> Result<Self, LabelSyntaxError> {
        if let Err(msg) = validator::validate_target_name(name) {
            return Err(LabelSyntaxError(format!(
                "invalid target name '{name}': {msg}"
            )));
        }
        Ok(Self {
            package_identifier: pkg,
            name: name.to_string(),
        })
    }

    /// Parses an absolute label (`//pkg:target`, `@repo//pkg:target`, `@@repo//pkg`, `@repo`).
    pub fn parse_canonical(raw: &str) -> Result<Self, LabelSyntaxError> {
        Self::parse_with_repo_mapping(raw, &RepositoryMapping::always_fallback())
    }

    pub fn parse_with_repo_mapping(
        raw: &str,
        repo_mapping: &RepositoryMapping,
    ) -> Result<Self, LabelSyntaxError> {
        let parts = LabelParts::parse(raw)?;
        if parts.pkg_ends_with_triple_dots {
            return Err(LabelSyntaxError(format!(
                "invalid label '{raw}': package name cannot contain '...'"
            )));
        }
        if !parts.pkg_is_absolute {
            return Err(LabelSyntaxError(format!(
                "invalid label '{raw}': absolute label must begin with '@' or '//'"
            )));
        }
        let repo = match parts.repo.as_deref() {
            None => repo_mapping.get(""),
            Some(r) if parts.repo_is_canonical => RepositoryName::create(r)?,
            Some(r) => repo_mapping.get(r),
        };
        let pkg = PackageIdentifier::create(repo, PathFragment::create(&parts.pkg))?;
        Self::create(pkg, &parts.target)
    }

    pub fn package_identifier(&self) -> &PackageIdentifier {
        &self.package_identifier
    }

    pub fn package_name(&self) -> &str {
        self.package_identifier.package_fragment().path_string()
    }

    pub fn repository(&self) -> &RepositoryName {
        self.package_identifier.repository()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn canonical_form(&self) -> String {
        format!("{}:{}", self.package_identifier.canonical_form(), self.name)
    }

    pub fn unambiguous_canonical_form(&self) -> String {
        let repo_prefix = if self.repository().is_main() {
            "@@".to_string()
        } else {
            self.repository().name_with_at()
        };
        format!("{repo_prefix}//{}:{}", self.package_name(), self.name)
    }

    pub fn shorthand_form(&self) -> String {
        let pkg = self.package_name();
        let last_seg = pkg.rsplit('/').next().unwrap_or(pkg);
        if !pkg.is_empty() && last_seg == self.name {
            self.package_identifier.canonical_form()
        } else {
            self.canonical_form()
        }
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical_form())
    }
}

/// Target pattern kinds matching `com.google.devtools.build.lib.cmdline.TargetPattern`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetPattern {
    /// A bare path such as `foo/bar/baz` (no colon, not `//`-prefixed), interpreted as a
    /// file/target by looking up the enclosing package (`InterpretPathAsTarget`). `path` is the
    /// path relative to the workspace root (the client's relative directory already applied).
    PathAsTarget { original: String, path: String },
    /// A single target, e.g. `//foo/bar:baz`.
    SingleTarget(Label),
    /// All targets in a package: `//foo/bar:all` (rules only) or `:*` / `:all-targets`.
    TargetsInPackage {
        package: PackageIdentifier,
        include_files: bool,
    },
    /// All targets recursively below a directory: `//foo/...` (rules only) or `//foo/...:*`.
    TargetsBelowDirectory {
        directory: PackageIdentifier,
        include_files: bool,
    },
}

/// Port of `LabelParser.Parts`.
struct Parts {
    repo: Option<String>,
    repo_is_canonical: bool,
    pkg_is_absolute: bool,
    pkg: String,
    pkg_ends_with_triple_dots: bool,
    target: String,
}

fn strip_trailing_triple_dots(pkg: &str) -> String {
    if let Some(p) = pkg.strip_suffix("/...") {
        p.to_string()
    } else if pkg == "..." {
        String::new()
    } else {
        pkg.to_string()
    }
}

fn perhaps_you_meant(pkg: &str, target: &str) -> String {
    if pkg.ends_with(&format!("/{target}")) {
        format!(" (perhaps you meant \":{target}\"?)")
    } else {
        String::new()
    }
}

impl Parts {
    fn parse(raw: &str) -> Result<Self, LabelSyntaxError> {
        let repo_is_canonical = raw.starts_with("@@");
        let double_slash = raw.find("//");
        let repo: Option<String>;
        let start_of_package: usize;
        let pkg_is_absolute: bool;
        if raw.starts_with('@') {
            let skip = if repo_is_canonical { 2 } else { 1 };
            match double_slash {
                None => {
                    let r = raw[skip..].to_string();
                    return Self::validate_and_create(
                        Some(r.clone()),
                        repo_is_canonical,
                        true,
                        String::new(),
                        false,
                        r,
                    );
                }
                Some(ds) => {
                    repo = Some(raw[skip..ds].to_string());
                    start_of_package = ds + 2;
                    pkg_is_absolute = true;
                }
            }
        } else {
            pkg_is_absolute = double_slash == Some(0);
            start_of_package = if pkg_is_absolute { 2 } else { 0 };
            repo = None;
        }
        let colon = raw[start_of_package..].find(':').map(|i| i + start_of_package);
        let raw_pkg = &raw[start_of_package..colon.unwrap_or(raw.len())];
        let ends_dots = raw_pkg.ends_with("/...") || raw_pkg == "...";
        let (pkg, target);
        if colon.is_none() && ends_dots {
            pkg = strip_trailing_triple_dots(raw_pkg);
            target = String::new();
        } else if colon.is_none() && !pkg_is_absolute {
            pkg = String::new();
            target = raw[start_of_package..].to_string();
        } else {
            pkg = strip_trailing_triple_dots(raw_pkg);
            target = match colon {
                Some(c) => raw[c + 1..].to_string(),
                None => pkg.rsplit('/').next().unwrap_or("").to_string(),
            };
        }
        Self::validate_and_create(repo, repo_is_canonical, pkg_is_absolute, pkg, ends_dots, target)
    }

    fn validate_and_create(
        repo: Option<String>,
        repo_is_canonical: bool,
        pkg_is_absolute: bool,
        pkg: String,
        pkg_ends_with_triple_dots: bool,
        target: String,
    ) -> Result<Self, LabelSyntaxError> {
        if let Some(r) = &repo {
            RepositoryName::validate(r)?;
        }
        if let Err(e) = validator::validate_package_name(&pkg) {
            return Err(LabelSyntaxError(format!(
                "invalid package name '{pkg}': {e}{}",
                perhaps_you_meant(&pkg, &target)
            )));
        }
        let mut target = target;
        if !(pkg_ends_with_triple_dots && target.is_empty()) {
            if let Err(e) = validator::validate_target_name(&target) {
                return Err(LabelSyntaxError(format!(
                    "invalid target name '{target}': {e}{}",
                    perhaps_you_meant(&pkg, &target)
                )));
            }
            if let Some(t) = target.strip_suffix("/.") {
                target = t.to_string();
            }
        }
        Ok(Self {
            repo,
            repo_is_canonical,
            pkg_is_absolute,
            pkg,
            pkg_ends_with_triple_dots,
            target,
        })
    }
}

impl TargetPattern {
    /// Parses `raw` against the main repository, relative to `relative_dir` (the client's working
    /// directory relative to the workspace root), with no repository mapping.
    pub fn parse(raw: &str, relative_dir: &PathFragment) -> Result<Self, LabelSyntaxError> {
        Self::parse_with(
            raw,
            relative_dir,
            &RepositoryName::main(),
            &RepositoryMapping::always_fallback(),
        )
    }

    /// Port of `TargetPattern.Parser.parse`.
    pub fn parse_with(
        raw: &str,
        relative_dir: &PathFragment,
        current_repo: &RepositoryName,
        mapping: &RepositoryMapping,
    ) -> Result<Self, LabelSyntaxError> {
        let parts = Parts::parse(raw).map_err(|e| LabelSyntaxError(e.0))?;
        let has_colon = raw.contains(':');
        // `foo/bar/baz` is treated as a file path.
        if !parts.pkg_is_absolute
            && current_repo.is_main()
            && parts.pkg.is_empty()
            && !parts.pkg_ends_with_triple_dots
            && !has_colon
        {
            return Ok(Self::PathAsTarget {
                original: raw.to_string(),
                path: relative_dir.get_relative_str(&parts.target).path_string().to_string(),
            });
        }
        let repo = match &parts.repo {
            None => current_repo.clone(),
            Some(r) if parts.repo_is_canonical => RepositoryName::create_unvalidated(r.clone()),
            Some(r) => mapping.get(r),
        };
        let pkg_fragment = if parts.pkg_is_absolute {
            PathFragment::create(&parts.pkg)
        } else {
            relative_dir.get_relative_str(&parts.pkg)
        };
        let package = PackageIdentifier::create(repo, pkg_fragment)?;
        if parts.pkg_ends_with_triple_dots {
            return match parts.target.as_str() {
                "" | "all" => Ok(Self::TargetsBelowDirectory {
                    directory: package,
                    include_files: false,
                }),
                "*" | "all-targets" => Ok(Self::TargetsBelowDirectory {
                    directory: package,
                    include_files: true,
                }),
                _ => Err(LabelSyntaxError(format!(
                    "Invalid target pattern {raw}: '...' can only be used with wildcard targets"
                ))),
            };
        }
        if has_colon && parts.target == "all" {
            return Ok(Self::TargetsInPackage {
                package,
                include_files: false,
            });
        }
        if has_colon && (parts.target == "*" || parts.target == "all-targets") {
            return Ok(Self::TargetsInPackage {
                package,
                include_files: true,
            });
        }
        let label = Label::create(package, &parts.target).map_err(|e| {
            LabelSyntaxError(format!("invalid target name: '{}'; {}", parts.target, e.0))
        })?;
        Ok(Self::SingleTarget(label))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternSign {
    Positive,
    Negative,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedTargetPattern {
    pub sign: PatternSign,
    pub pattern: TargetPattern,
}

impl SignedTargetPattern {
    pub fn parse(raw: &str, relative_dir: &PathFragment) -> Result<Self, LabelSyntaxError> {
        Self::parse_with(
            raw,
            relative_dir,
            &RepositoryName::main(),
            &RepositoryMapping::always_fallback(),
        )
    }

    /// Port of `SignedTargetPattern.parse`: a leading `-` negates the pattern.
    pub fn parse_with(
        raw: &str,
        relative_dir: &PathFragment,
        current_repo: &RepositoryName,
        mapping: &RepositoryMapping,
    ) -> Result<Self, LabelSyntaxError> {
        match raw.strip_prefix('-') {
            Some(rest) => Ok(Self {
                sign: PatternSign::Negative,
                pattern: TargetPattern::parse_with(rest, relative_dir, current_repo, mapping)?,
            }),
            None => Ok(Self {
                sign: PatternSign::Positive,
                pattern: TargetPattern::parse_with(raw, relative_dir, current_repo, mapping)?,
            }),
        }
    }
}

/// Reads target patterns from the command line and/or `--target_pattern_file`
/// (`TargetPatternsHelper.readFrom`, without the `--target_query` variants).
///
/// `cwd` is the client's working directory used to resolve a relative pattern file.
pub fn read_target_patterns(
    residue: &[String],
    target_pattern_file: &str,
    cwd: &std::path::Path,
) -> Result<Vec<String>, String> {
    if target_pattern_file.is_empty() {
        return Ok(residue.to_vec());
    }
    if !residue.is_empty() {
        return Err(
            "Command-line target pattern and --target_pattern_file cannot both be specified"
                .to_string(),
        );
    }
    let path = cwd.join(target_pattern_file);
    let bytes = std::fs::read(&path).map_err(|e| {
        format!(
            "I/O error reading from {}: {}",
            path.display(),
            io_error_message(&e)
        )
    })?;
    // ISO-8859-1: bytes map 1:1 to chars.
    let text: String = bytes.iter().map(|&b| b as char).collect();
    Ok(text
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

fn io_error_message(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => "No such file or directory".to_string(),
        std::io::ErrorKind::PermissionDenied => "Permission denied".to_string(),
        _ => e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_label_parsing_and_shorthand() {
        let l1 = Label::parse_canonical("//src/main/java:bazel").unwrap();
        assert_eq!(l1.package_name(), "src/main/java");
        assert_eq!(l1.name(), "bazel");
        assert_eq!(l1.canonical_form(), "//src/main/java:bazel");

        let l2 = Label::parse_canonical("//src/tools/singlejar").unwrap();
        assert_eq!(l2.package_name(), "src/tools/singlejar");
        assert_eq!(l2.name(), "singlejar");
        assert_eq!(l2.canonical_form(), "//src/tools/singlejar:singlejar");
        assert_eq!(l2.shorthand_form(), "//src/tools/singlejar");

        let l3 = Label::parse_canonical("@rules_rs//rs:rust_binary.bzl").unwrap();
        assert_eq!(l3.repository().name(), "rules_rs");
        assert_eq!(l3.package_name(), "rs");
        assert_eq!(l3.name(), "rust_binary.bzl");
        assert_eq!(l3.canonical_form(), "@@rules_rs//rs:rust_binary.bzl");
    }

    #[test]
    fn test_label_validation_errors() {
        assert!(Label::parse_canonical("//foo//bar:baz").is_err());
        assert!(Label::parse_canonical("//foo/..:baz").is_err());
        assert!(Label::parse_canonical("//foo:../baz").is_err());
        assert!(Label::parse_canonical("//foo:bar/").is_err());
    }

    fn pat(raw: &str, rel: &str) -> Result<TargetPattern, String> {
        TargetPattern::parse(raw, &PathFragment::create(rel)).map_err(|e| e.0)
    }

    fn pkg(p: &str) -> PackageIdentifier {
        PackageIdentifier::create_in_main_repo(p).unwrap()
    }

    #[test]
    fn test_target_patterns() {
        let rel = PathFragment::empty();
        let p1 = TargetPattern::parse("//src/test/...", &rel).unwrap();
        assert_eq!(
            p1,
            TargetPattern::TargetsBelowDirectory {
                directory: pkg("src/test"),
                include_files: false,
            }
        );
        let p2 = SignedTargetPattern::parse("-//src/test/shell:all", &rel).unwrap();
        assert_eq!(p2.sign, PatternSign::Negative);
    }

    #[test]
    fn pattern_forms() {
        use TargetPattern::*;
        let single = |l: &str| SingleTarget(Label::parse_canonical(l).unwrap());
        assert_eq!(pat("//foo:bar", "").unwrap(), single("//foo:bar"));
        assert_eq!(pat("//foo/bar", "").unwrap(), single("//foo/bar:bar"));
        assert_eq!(pat("//foo:all", "").unwrap(), TargetsInPackage { package: pkg("foo"), include_files: false });
        assert_eq!(pat("//foo:*", "").unwrap(), TargetsInPackage { package: pkg("foo"), include_files: true });
        assert_eq!(pat("//foo:all-targets", "").unwrap(), TargetsInPackage { package: pkg("foo"), include_files: true });
        assert_eq!(pat("//:all", "").unwrap(), TargetsInPackage { package: pkg(""), include_files: false });
        assert_eq!(pat("//...", "").unwrap(), TargetsBelowDirectory { directory: pkg(""), include_files: false });
        assert_eq!(pat("//...:all", "").unwrap(), TargetsBelowDirectory { directory: pkg(""), include_files: false });
        assert_eq!(pat("//...:*", "").unwrap(), TargetsBelowDirectory { directory: pkg(""), include_files: true });
        assert_eq!(pat("//foo/...:all-targets", "").unwrap(), TargetsBelowDirectory { directory: pkg("foo"), include_files: true });
        // A package that happens to be named `all` is still a package.
        assert_eq!(pat("//foo/all", "").unwrap(), single("//foo/all:all"));
        // `...` needs a wildcard target.
        assert_eq!(
            pat("//foo/...:bar", "").unwrap_err(),
            "Invalid target pattern //foo/...:bar: '...' can only be used with wildcard targets"
        );
    }

    #[test]
    fn relative_patterns_use_client_directory() {
        use TargetPattern::*;
        let single = |l: &str| SingleTarget(Label::parse_canonical(l).unwrap());
        assert_eq!(pat(":util", "src/core").unwrap(), single("//src/core:util"));
        assert_eq!(pat("sub:x", "src/core").unwrap(), single("//src/core/sub:x"));
        assert_eq!(pat("sub/x:y", "a").unwrap(), single("//a/sub/x:y"));
        assert_eq!(pat("...", "src/core").unwrap(), TargetsBelowDirectory { directory: pkg("src/core"), include_files: false });
        assert_eq!(pat("sub/...", "src/core").unwrap(), TargetsBelowDirectory { directory: pkg("src/core/sub"), include_files: false });
        assert_eq!(pat(":all", "src/core").unwrap(), TargetsInPackage { package: pkg("src/core"), include_files: false });
        assert_eq!(pat("...:*", "x").unwrap(), TargetsBelowDirectory { directory: pkg("x"), include_files: true });
        // Absolute patterns ignore the client directory.
        assert_eq!(pat("//top:t", "src/core").unwrap(), single("//top:t"));
        // Bare paths are interpreted as files/targets.
        assert_eq!(
            pat("foo/bar.cc", "src").unwrap(),
            PathAsTarget { original: "foo/bar.cc".into(), path: "src/foo/bar.cc".into() }
        );
        assert_eq!(
            pat("foo", "").unwrap(),
            PathAsTarget { original: "foo".into(), path: "foo".into() }
        );
    }

    #[test]
    fn external_repo_patterns() {
        use TargetPattern::*;
        let mut entries = HashMap::new();
        entries.insert("rules_rs".to_string(), RepositoryName::create("rules_rs+1.0").unwrap());
        let mapping = RepositoryMapping::new(entries, RepositoryName::main());
        let parse = |raw: &str| {
            TargetPattern::parse_with(raw, &PathFragment::empty(), &RepositoryName::main(), &mapping)
                .map_err(|e| e.0)
        };
        let ext = |p: &str| {
            PackageIdentifier::create(RepositoryName::create("rules_rs+1.0").unwrap(), PathFragment::create(p)).unwrap()
        };
        assert_eq!(parse("@rules_rs//...").unwrap(), TargetsBelowDirectory { directory: ext(""), include_files: false });
        assert_eq!(parse("@rules_rs//src:all").unwrap(), TargetsInPackage { package: ext("src"), include_files: false });
        assert_eq!(parse("@rules_rs//:*").unwrap(), TargetsInPackage { package: ext(""), include_files: true });
        assert_eq!(parse("@@rules_rs+1.0//src/...").unwrap(), TargetsBelowDirectory { directory: ext("src"), include_files: false });
        assert_eq!(
            parse("@rules_rs//a:b").unwrap(),
            SingleTarget(Label::create(ext("a"), "b").unwrap())
        );
        assert_eq!(
            parse("@rules_rs").unwrap(),
            SingleTarget(Label::create(ext(""), "rules_rs").unwrap())
        );
        // Unmapped apparent names pass through.
        assert!(matches!(
            parse("@other//x:y").unwrap(),
            SingleTarget(l) if l.repository().name() == "other"
        ));
    }

    #[test]
    fn pattern_syntax_errors() {
        assert_eq!(
            pat("//foo//bar", "").unwrap_err(),
            "invalid package name 'foo//bar': package names may not contain '//' path separators (perhaps you meant \":bar\"?)"
        );
        assert!(pat("//foo:bar baz/../x", "").unwrap_err().starts_with("invalid target name 'bar baz/../x':"));
        assert!(pat("//foo:", "").unwrap_err().starts_with("invalid target name '':"));
        assert!(pat("//foo/..:x", "").unwrap_err().starts_with("invalid package name 'foo/..':"));
        assert!(pat(":/abs", "").unwrap_err().contains("target names may not start with '/'"));
        // perhaps-you-meant
        let e = pat("//foo/bar:bar/", "").unwrap_err();
        assert!(e.starts_with("invalid target name 'bar/':"), "{e}");
    }

    #[test]
    fn signed_patterns() {
        let rel = PathFragment::empty();
        let neg = SignedTargetPattern::parse("-//a/...", &rel).unwrap();
        assert_eq!(neg.sign, PatternSign::Negative);
        assert_eq!(neg.pattern, TargetPattern::TargetsBelowDirectory { directory: pkg("a"), include_files: false });
        let pos = SignedTargetPattern::parse("//a:b", &rel).unwrap();
        assert_eq!(pos.sign, PatternSign::Positive);
        // Bazel has no `+` prefix.
        assert!(SignedTargetPattern::parse("+//a:b", &rel).is_err());
        // Negative relative pattern uses the client directory.
        let n = SignedTargetPattern::parse("-:t", &PathFragment::create("x")).unwrap();
        assert!(matches!(n.pattern, TargetPattern::SingleTarget(l) if l.canonical_form() == "//x:t"));
    }

    #[test]
    fn target_pattern_file() {
        let dir = std::env::temp_dir().join(format!("zel-tpf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pats.txt"), "//a:b # comment\n\n  //c/...  \n# only comment\n-//c/d\n").unwrap();
        let got = read_target_patterns(&[], "pats.txt", &dir).unwrap();
        assert_eq!(got, vec!["//a:b", "//c/...", "-//c/d"]);
        assert_eq!(
            read_target_patterns(&["//x".to_string()], "pats.txt", &dir).unwrap_err(),
            "Command-line target pattern and --target_pattern_file cannot both be specified"
        );
        let err = read_target_patterns(&[], "missing.txt", &dir).unwrap_err();
        assert!(err.starts_with("I/O error reading from "), "{err}");
        assert!(err.ends_with("missing.txt: No such file or directory"), "{err}");
        assert_eq!(
            read_target_patterns(&["//x".to_string()], "", &dir).unwrap(),
            vec!["//x"]
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
