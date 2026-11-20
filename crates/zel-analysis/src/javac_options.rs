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

//! Port of Bazel's `JavacOptions` and `WerrorCustomOption`
//! (`com.google.devtools.build.buildjar.javac`).

use std::collections::HashSet;

/// Insertion-ordered boolean map matching `java.util.LinkedHashMap<String, Boolean>`.
#[derive(Debug, Clone, Default)]
struct OrderedBoolMap {
    entries: Vec<(String, bool)>,
}

impl OrderedBoolMap {
    fn clear(&mut self) {
        self.entries.clear();
    }

    fn insert(&mut self, key: String, val: bool) {
        if let Some((_, v)) = self.entries.iter_mut().find(|(k, _)| k == &key) {
            *v = val;
        } else {
            self.entries.push((key, val));
        }
    }

    fn remove(&mut self, key: &str) {
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == key) {
            self.entries.remove(pos);
        }
    }

    fn get(&self, key: &str) -> Option<bool> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| *v)
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn iter(&self) -> impl Iterator<Item = &(String, bool)> {
        self.entries.iter()
    }
}

/// Bazel's `-Werror:` custom option accumulator (`WerrorCustomOption.java`).
#[derive(Debug, Clone, Default)]
pub struct WerrorCustomOption {
    defaults_order: Vec<String>,
    defaults_set: HashSet<String>,
    warnings: OrderedBoolMap,
}

impl WerrorCustomOption {
    pub fn new() -> Self {
        Self::with_defaults(std::iter::empty::<&str>())
    }

    pub fn with_defaults<I, S>(defaults: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut defaults_order = Vec::new();
        let mut defaults_set = HashSet::new();
        for d in defaults {
            let s = d.as_ref().to_string();
            if defaults_set.insert(s.clone()) {
                defaults_order.push(s);
            }
        }
        let mut opt = Self {
            defaults_order,
            defaults_set,
            warnings: OrderedBoolMap::default(),
        };
        opt.seed_defaults();
        opt
    }

    /// Parses a single `-Werror` or `-Werror:...` flag into a fresh `WerrorCustomOption`.
    pub fn create(arg: &str) -> Self {
        let mut opt = Self::new();
        opt.process(arg);
        opt
    }

    fn seed_defaults(&mut self) {
        for d in &self.defaults_order {
            self.warnings.insert(d.clone(), true);
        }
    }

    pub fn process(&mut self, arg: &str) -> bool {
        if arg == "-Werror" {
            self.warnings.clear();
            self.warnings.insert("all".to_string(), true);
            self.seed_defaults();
            return true;
        }
        let Some(spec) = arg.strip_prefix("-Werror:") else {
            return false;
        };
        for raw in spec.split(',') {
            match raw {
                "all" | "+all" => {
                    self.warnings.clear();
                    self.warnings.insert("all".to_string(), true);
                    self.seed_defaults();
                }
                "-all" | "none" => {
                    self.warnings.clear();
                    self.seed_defaults();
                }
                _ if raw.starts_with('-') => {
                    let cat = &raw[1..];
                    if !self.defaults_set.contains(cat) {
                        self.warnings.insert(cat.to_string(), false);
                    }
                }
                _ => {
                    let cat = raw.strip_prefix('+').unwrap_or(raw);
                    if !cat.is_empty() {
                        self.warnings.insert(cat.to_string(), true);
                    }
                }
            }
        }
        true
    }

    pub fn is_enabled(&self, lint_category: &str) -> bool {
        let all = self.warnings.get("all").unwrap_or(false);
        self.warnings.get(lint_category).unwrap_or(all)
    }

    pub fn to_flag(&self) -> Option<String> {
        if self.warnings.is_empty() {
            return None;
        }
        let mut out = String::from("-Werror:");
        let mut first = true;
        for (cat, enabled) in self.warnings.iter() {
            if !first {
                out.push(',');
            }
            first = false;
            if !*enabled {
                out.push('-');
            }
            out.push_str(cat);
        }
        Some(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum XlintBasis {
    Empty,
    Recommended,
    All,
    None,
}

#[derive(Debug, Clone)]
struct XlintOptionNormalizer {
    enforced_order: Vec<String>,
    enforced_set: HashSet<String>,
    basis: XlintBasis,
    overrides: OrderedBoolMap,
}

impl XlintOptionNormalizer {
    fn new(enforced: &[String]) -> Self {
        let mut enforced_order = Vec::new();
        let mut enforced_set = HashSet::new();
        for e in enforced {
            if enforced_set.insert(e.clone()) {
                enforced_order.push(e.clone());
            }
        }
        let mut n = Self {
            enforced_order,
            enforced_set,
            basis: XlintBasis::Empty,
            overrides: OrderedBoolMap::default(),
        };
        n.reset_overrides();
        n
    }

    fn reset_overrides(&mut self) {
        self.overrides.clear();
        if self.basis != XlintBasis::All {
            for e in &self.enforced_order {
                self.overrides.insert(e.clone(), true);
            }
        }
    }

    fn process(&mut self, arg: &str) -> bool {
        if arg == "-nowarn" {
            self.basis = XlintBasis::None;
            self.reset_overrides();
            return true;
        }
        if arg == "-Xlint" {
            self.basis = XlintBasis::Recommended;
            self.reset_overrides();
            return true;
        }
        let Some(spec) = arg.strip_prefix("-Xlint:") else {
            return false;
        };
        for item in spec.split(',') {
            match item {
                "all" | "" => {
                    self.basis = XlintBasis::All;
                    self.reset_overrides();
                }
                "none" => {
                    self.basis = XlintBasis::None;
                    self.reset_overrides();
                }
                _ if item.starts_with('-') => {
                    let cat = &item[1..];
                    if !self.enforced_set.contains(cat) {
                        if self.basis == XlintBasis::None {
                            self.overrides.remove(cat);
                        } else {
                            self.overrides.insert(cat.to_string(), false);
                        }
                    }
                }
                cat => {
                    if self.basis == XlintBasis::All {
                        self.overrides.remove(cat);
                    } else {
                        self.overrides.insert(cat.to_string(), true);
                    }
                }
            }
        }
        true
    }

    fn append_normalized(&self, out: &mut Vec<String>) {
        match self.basis {
            XlintBasis::Empty => {}
            XlintBasis::Recommended => out.push("-Xlint".to_string()),
            XlintBasis::All => out.push("-Xlint:all".to_string()),
            XlintBasis::None => out.push("-Xlint:none".to_string()),
        }
        let mut plus = Vec::new();
        let mut minus = Vec::new();
        for (cat, enabled) in self.overrides.iter() {
            if *enabled {
                if self.basis != XlintBasis::All {
                    plus.push(cat.as_str());
                }
            } else if self.basis != XlintBasis::None {
                minus.push(cat.as_str());
            }
        }
        if !plus.is_empty() {
            out.push(format!("-Xlint:{}", plus.join(",")));
        }
        if !minus.is_empty() {
            out.push(format!("-Xlint:-{}", minus.join(",-")));
        }
    }
}

/// Normalizes `-source`, `-target`, `--release` (last-writer-wins) and gates JDK 9+ modular/preview
/// flags on an effective `--release` or `-target` version strictly greater than 8
/// (`JavacOptions.ReleaseOptionNormalizer`).
#[derive(Debug, Clone, Default)]
pub struct ReleaseOptionNormalizer {
    source: Option<String>,
    target: Option<String>,
    release: Option<String>,
    modular_flags: Vec<String>,
    enable_preview: bool,
}

impl ReleaseOptionNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consumes `options[idx]` (and `options[idx + 1]` for two-token options) if recognized.
    /// Returns the number of tokens consumed (`0` if unrecognized).
    pub fn process_at(&mut self, options: &[String], idx: usize) -> usize {
        let arg = options[idx].as_str();
        match arg {
            "-source" => {
                if let Some(val) = options.get(idx + 1) {
                    self.source = Some(val.clone());
                    self.release = None;
                    2
                } else {
                    1
                }
            }
            "-target" | "--target" => {
                if let Some(val) = options.get(idx + 1) {
                    self.target = Some(val.clone());
                    self.release = None;
                    2
                } else {
                    1
                }
            }
            "--release" => {
                if let Some(val) = options.get(idx + 1) {
                    self.release = Some(val.clone());
                    self.source = None;
                    self.target = None;
                    2
                } else {
                    1
                }
            }
            "--add-exports" | "--add-opens" | "--add-modules" => {
                if let Some(val) = options.get(idx + 1) {
                    self.modular_flags.push(arg.to_string());
                    self.modular_flags.push(val.clone());
                    2
                } else {
                    1
                }
            }
            "--enable-preview" => {
                self.enable_preview = true;
                1
            }
            _ if arg.starts_with("--release=") => {
                self.release = Some(arg["--release=".len()..].to_string());
                self.source = None;
                self.target = None;
                1
            }
            _ if arg.starts_with("--add-exports=")
                || arg.starts_with("--add-opens=")
                || arg.starts_with("--add-modules=") =>
            {
                self.modular_flags.push(arg.to_string());
                1
            }
            _ => 0,
        }
    }

    pub fn append_normalized(&self, out: &mut Vec<String>) {
        let allow_modular = if let Some(rel) = &self.release {
            out.push("--release".to_string());
            out.push(rel.clone());
            parse_java_version_above_8(rel)
        } else {
            if let Some(src) = &self.source {
                out.push("-source".to_string());
                out.push(src.clone());
            }
            if let Some(tgt) = &self.target {
                out.push("-target".to_string());
                out.push(tgt.clone());
                parse_java_version_above_8(tgt)
            } else {
                false
            }
        };
        if allow_modular {
            out.extend(self.modular_flags.iter().cloned());
            if self.enable_preview {
                out.push("--enable-preview".to_string());
            }
        }
    }
}

/// Parses a Java version string after stripping an optional `"1."` prefix via `Ints.tryParse`
/// semantics and returns `true` iff the parsed integer is `> 8`.
pub fn parse_java_version_above_8(version: &str) -> bool {
    let rest = version.strip_prefix("1.").unwrap_or(version);
    if rest.is_empty() {
        return false;
    }
    let digits = rest.strip_prefix('+').unwrap_or(rest);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    digits.parse::<i32>().is_ok_and(|v| v > 8)
}

/// Bazel-specific javac option classifier and normalizer (`JavacOptions.java`).
#[derive(Debug, Clone, Default)]
pub struct JavacOptions {
    warnings_as_errors_default: Vec<String>,
}

impl JavacOptions {
    pub fn create_with_warnings_as_errors_default(defaults: Vec<String>) -> Self {
        Self {
            warnings_as_errors_default: defaults,
        }
    }

    /// Returns `true` for Bazel-specific flags (`-Werror:...` and `-Xep*`). Bare `-Werror` is a
    /// standard `javac` flag and returns `false`.
    pub fn is_bazel_specific_flag(opt: &str) -> bool {
        opt.starts_with("-Werror:") || opt.starts_with("-Xep")
    }

    pub fn remove_bazel_specific_flags<I, S>(opts: I) -> Vec<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        opts.into_iter()
            .filter(|o| !Self::is_bazel_specific_flag(o.as_ref()))
            .map(|o| o.as_ref().to_string())
            .collect()
    }

    /// Full `BazelJavaBuilder` normalization: unrecognized flags stay in place, followed by
    /// `XlintOptionNormalizer`, `WErrorOptionNormalizer`, and `ReleaseOptionNormalizer`.
    pub fn normalize(&self, options: &[String]) -> Vec<String> {
        let mut passthrough = Vec::with_capacity(options.len());
        let mut xlint = XlintOptionNormalizer::new(&self.warnings_as_errors_default);
        let mut werror = WerrorCustomOption::with_defaults(&self.warnings_as_errors_default);
        let mut release = ReleaseOptionNormalizer::new();

        let mut i = 0;
        while i < options.len() {
            let consumed = release.process_at(options, i);
            if consumed > 0 {
                i += consumed;
                continue;
            }
            let arg = &options[i];
            if xlint.process(arg) || werror.process(arg) {
                i += 1;
                continue;
            }
            passthrough.push(arg.clone());
            i += 1;
        }

        xlint.append_normalized(&mut passthrough);
        if let Some(wf) = werror.to_flag() {
            passthrough.push(wf);
        }
        release.append_normalized(&mut passthrough);
        passthrough
    }

    /// Normalization with an empty enforced-warning list (the `BazelJavaBuilder` default).
    pub fn normalize_default(options: &[String]) -> Vec<String> {
        Self::default().normalize(options)
    }

    /// `ReleaseOptionNormalizer`-only pass (used by header compilation and `VanillaJavaBuilder`).
    pub fn normalize_release_only(options: &[String]) -> Vec<String> {
        let mut passthrough = Vec::with_capacity(options.len());
        let mut release = ReleaseOptionNormalizer::new();
        let mut i = 0;
        while i < options.len() {
            let consumed = release.process_at(options, i);
            if consumed > 0 {
                i += consumed;
            } else {
                passthrough.push(options[i].clone());
                i += 1;
            }
        }
        release.append_normalized(&mut passthrough);
        passthrough
    }

    /// `VanillaJavaBuilder` normalization: release normalizer only, then strip Bazel-specific flags.
    pub fn normalize_vanilla(options: &[String]) -> Vec<String> {
        let rel = Self::normalize_release_only(options);
        Self::remove_bazel_specific_flags(rel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn release_normalizer_last_writer_wins_and_gates_modular_flags() {
        // `--release` clears prior `-source` and `-target`, and `--release=17` re-emits as two tokens.
        assert_eq!(
            JavacOptions::normalize_default(&s(&[
                "-source",
                "8",
                "-target",
                "8",
                "--add-exports",
                "java.compiler/com.sun.tools.javac.api=ALL-UNNAMED",
                "--enable-preview",
                "--release=17",
            ])),
            s(&[
                "--release",
                "17",
                "--add-exports",
                "java.compiler/com.sun.tools.javac.api=ALL-UNNAMED",
                "--enable-preview",
            ])
        );

        // Later `-source` / `-target` clears `--release`; version <= 8 (`1.8` / `8`) drops modular flags.
        assert_eq!(
            JavacOptions::normalize_default(&s(&[
                "--release",
                "17",
                "--add-opens=java.base/java.lang=ALL-UNNAMED",
                "--enable-preview",
                "-source",
                "1.8",
                "-target",
                "1.8",
            ])),
            s(&["-source", "1.8", "-target", "1.8"])
        );

        // Source-only level (no `-target` and no `--release`) drops modular flags.
        assert_eq!(
            JavacOptions::normalize_default(&s(&[
                "-source",
                "21",
                "--add-modules",
                "jdk.incubator.vector",
            ])),
            s(&["-source", "21"])
        );

        // Trailing flag with missing argument is consumed and dropped.
        assert_eq!(
            JavacOptions::normalize_default(&s(&["-g", "--release"])),
            s(&["-g"])
        );
    }

    #[test]
    fn werror_and_xlint_collapse_and_enforced_defaults() {
        let jo = JavacOptions::create_with_warnings_as_errors_default(s(&["deprecation"]));
        let out = jo.normalize(&s(&[
            "-encoding",
            "UTF-8",
            "-Xlint:all",
            "-Xlint:-serial,-deprecation",
            "-Werror",
            "-Werror:-cast,-deprecation",
        ]));
        // `deprecation` is enforced: `-deprecation` cannot disable it in Xlint or Werror!
        assert_eq!(
            out,
            s(&[
                "-encoding",
                "UTF-8",
                "-Xlint:all",
                "-Xlint:-serial",
                "-Werror:all,deprecation,-cast",
            ])
        );

        let w = WerrorCustomOption::create("-Werror:cast,rawtypes,-cast");
        assert!(!w.is_enabled("cast"));
        assert!(w.is_enabled("rawtypes"));
        assert!(!w.is_enabled("unchecked"));
    }

    #[test]
    fn vanilla_normalizer_keeps_bare_werror_and_strips_bazel_flags() {
        assert!(!JavacOptions::is_bazel_specific_flag("-Werror"));
        assert!(JavacOptions::is_bazel_specific_flag("-Werror:all"));
        assert!(JavacOptions::is_bazel_specific_flag(
            "-Xep:FallThrough:ERROR"
        ));

        assert_eq!(
            JavacOptions::normalize_vanilla(&s(&[
                "-Werror",
                "-Werror:cast",
                "-XepDisableAllChecks",
                "--release=11",
                "--add-exports=m/p=ALL-UNNAMED",
            ])),
            s(&[
                "-Werror",
                "--release",
                "11",
                "--add-exports=m/p=ALL-UNNAMED",
            ])
        );
    }
}
