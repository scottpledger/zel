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

//! Bazel 9 `--incompatible_autoload_externally` and removed predeclared symbol guards.
//!
//! Mirrors `com.google.devtools.build.lib.packages.AutoloadSymbols` and
//! `BaseRuleClasses.createEmptySymbol` / `CcRules.init` from Bazel 9.3.2.

use std::collections::{BTreeSet, HashSet};
use zel_cmdline::RepositoryName;

/// Predeclared `.bzl` symbols removed in Bazel 9.3.2 that emit a guarded removal diagnostic
/// (`BaseRuleClasses.createEmptySymbol`, registered in `CcRules.init`).
pub const REMOVED_BZL_SYMBOLS: &[(&str, &str)] = &[
    ("CcInfo", "@rules_cc//cc/common:cc_info.bzl"),
    (
        "DebugPackageInfo",
        "@rules_cc//cc/common:debug_package_info.bzl",
    ),
    (
        "CcSharedLibraryInfo",
        "@rules_cc//cc/common:cc_shared_library_info.bzl",
    ),
    (
        "CcSharedLibraryHintInfo",
        "@rules_cc//cc/common:cc_shared_library_hint_info.bzl",
    ),
];

/// Exact Bazel 9.3.2 error message for accessing a removed symbol (`BaseRuleClasses$5`).
pub fn removed_symbol_error(symbol: &str, bzl_load_label: &str) -> String {
    format!(
        "The {symbol} symbol has been removed, add the following to your BUILD/bzl file:\n\nload(\"{bzl_load_label}\", \"{symbol}\")"
    )
}

/// Exact Bazel 9.3.2 error message when an autoloaded symbol's external repo is unavailable
/// (`AutoloadSymbols$1`).
pub fn missing_autoload_error(symbol: &str) -> String {
    if let Some(redirect) = lookup(symbol) {
        let dep_hint = format!(
            "a 'bazel_dep(name = \"{}\", ...)' in your MODULE.bazel file",
            redirect.module_name()
        );
        format!(
            "Couldn't auto load '{symbol}' from '{}'. Ensure that you have {dep_hint} or add an explicit load statement to your BUILD file.",
            redirect.load_label
        )
    } else {
        format!("Couldn't auto load '{symbol}'.")
    }
}

/// Repositories exempt from autoloads by default (`AutoloadSymbols.PREDECLARED_REPOS_DISALLOWING_AUTOLOADS`).
pub const PREDECLARED_REPOS_DISALLOWING_AUTOLOADS: &[&str] = &[
    "protobuf",
    "com_google_protobuf",
    "proto_bazel_features",
    "rules_android",
    "rules_cc",
    "rules_java",
    "rules_java_builtin",
    "compatibility_proxy",
    "rules_python",
    "rules_python_internal",
    "rules_shell",
    "apple_support",
    "build_bazel_apple_support",
    "bazel_skylib",
    "bazel_tools",
    "bazel_features",
    "bazel_features_version",
    "bazel_features_globals",
];

/// Redirect target of one autoloadable symbol or rule (`AutoloadSymbols.SymbolRedirect`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SymbolRedirect {
    pub symbol: &'static str,
    pub load_label: &'static str,
    pub is_rule: bool,
    /// Name exported by the `.bzl` file when renamed (e.g. `ProguardSpecProvider` -> `ProguardSpecInfo`).
    pub new_name: Option<&'static str>,
    /// Symbols that still depend on this symbol if partially removed.
    pub rdeps: &'static [&'static str],
}

impl SymbolRedirect {
    /// Apparent repository name in `load_label` (e.g. `"rules_cc"`, `"com_google_protobuf"`).
    pub fn repo_name(&self) -> &'static str {
        self.load_label
            .strip_prefix('@')
            .and_then(|s| s.split_once("//"))
            .map(|(r, _)| r)
            .unwrap_or("")
    }

    /// Bzlmod module name corresponding to `repo_name()` (`SymbolRedirect.moduleToRepoName`).
    pub fn module_name(&self) -> &'static str {
        match self.repo_name() {
            "build_bazel_apple_support" => "apple_support",
            "com_google_protobuf" => "protobuf",
            other => other,
        }
    }

    /// `(package, file)` portion of `load_label` after `//`.
    pub fn package_and_file(&self) -> (&'static str, &'static str) {
        self.load_label
            .split_once("//")
            .and_then(|(_, rest)| rest.split_once(':'))
            .unwrap_or(("", ""))
    }

    /// Exported symbol name inside the loaded `.bzl` file.
    pub fn exported_name(&self) -> &'static str {
        self.new_name.unwrap_or(self.symbol)
    }
}

const fn sym(
    symbol: &'static str,
    load_label: &'static str,
    rdeps: &'static [&'static str],
) -> SymbolRedirect {
    SymbolRedirect {
        symbol,
        load_label,
        is_rule: false,
        new_name: None,
        rdeps,
    }
}

const fn renamed_sym(
    symbol: &'static str,
    load_label: &'static str,
    new_name: &'static str,
    rdeps: &'static [&'static str],
) -> SymbolRedirect {
    SymbolRedirect {
        symbol,
        load_label,
        is_rule: false,
        new_name: Some(new_name),
        rdeps,
    }
}

const fn rule(symbol: &'static str, load_label: &'static str) -> SymbolRedirect {
    SymbolRedirect {
        symbol,
        load_label,
        is_rule: true,
        new_name: None,
        rdeps: &[],
    }
}

/// Complete `AUTOLOAD_CONFIG` table from Bazel 9.3.2's `AutoloadSymbols.java`.
pub const AUTOLOAD_CONFIG: &[SymbolRedirect] = &[
    sym(
        "CcSharedLibraryInfo",
        "@rules_cc//cc/common:cc_shared_library_info.bzl",
        &["cc_shared_library"],
    ),
    sym(
        "CcSharedLibraryHintInfo",
        "@rules_cc//cc/common:cc_shared_library_hint_info.bzl",
        &["cc_common"],
    ),
    sym(
        "cc_proto_aspect",
        "@com_google_protobuf//bazel/private:bazel_cc_proto_library.bzl",
        &["cc_proto_aspect"],
    ),
    sym(
        "ProtoInfo",
        "@com_google_protobuf//bazel/common:proto_info.bzl",
        &[
            "proto_library",
            "cc_proto_library",
            "java_lite_proto_library",
            "java_proto_library",
            "proto_lang_toolchain",
            "java_binary",
        ],
    ),
    sym(
        "proto_common_do_not_use",
        "@com_google_protobuf//:dummy.bzl",
        &[],
    ),
    sym("cc_common", "@rules_cc//cc/common:cc_common.bzl", &[]),
    sym(
        "CcInfo",
        "@rules_cc//cc/common:cc_info.bzl",
        &[
            "cc_binary",
            "cc_library",
            "cc_test",
            "cc_shared_library",
            "cc_common",
            "java_library",
            "cc_proto_library",
            "java_import",
            "java_runtime",
            "java_binary",
            "objc_library",
            "java_common",
            "JavaInfo",
            "py_extension",
            "cc_import",
            "objc_import",
            "cc_toolchain",
            "PyCcLinkParamsProvider",
            "py_library",
        ],
    ),
    sym(
        "DebugPackageInfo",
        "@rules_cc//cc/common:debug_package_info.bzl",
        &["cc_binary", "cc_test"],
    ),
    sym(
        "CcToolchainConfigInfo",
        "@rules_cc//cc/toolchains:cc_toolchain_config_info.bzl",
        &["cc_toolchain"],
    ),
    sym(
        "java_common",
        "@rules_java//java/common:java_common.bzl",
        &[],
    ),
    sym(
        "JavaInfo",
        "@rules_java//java/common:java_info.bzl",
        &[
            "java_binary",
            "java_library",
            "java_test",
            "java_proto_library",
            "java_lite_proto_library",
            "java_plugin",
            "java_import",
            "java_common",
        ],
    ),
    sym(
        "JavaPluginInfo",
        "@rules_java//java/common:java_plugin_info.bzl",
        &["java_plugin", "java_library", "java_binary", "java_test"],
    ),
    renamed_sym(
        "ProguardSpecProvider",
        "@rules_java//java/common:proguard_spec_info.bzl",
        "ProguardSpecInfo",
        &[
            "java_lite_proto_library",
            "java_import",
            "android_binary",
            "android_library",
        ],
    ),
    sym(
        "PyInfo",
        "@rules_python//python:py_info.bzl",
        &["py_binary", "py_test", "py_library"],
    ),
    sym(
        "PyRuntimeInfo",
        "@rules_python//python:py_runtime_info.bzl",
        &["py_binary", "py_test", "py_library"],
    ),
    renamed_sym(
        "PyCcLinkParamsProvider",
        "@rules_python//python:py_cc_link_params_info.bzl",
        "PyCcLinkParamsInfo",
        &["py_binary", "py_test", "py_library"],
    ),
    sym(
        "AndroidIdeInfo",
        "@rules_android//providers:providers.bzl",
        &[
            "aar_import",
            "android_binary",
            "android_library",
            "android_local_test",
            "android_sdk",
        ],
    ),
    rule("aar_import", "@rules_android//rules:rules.bzl"),
    rule("android_binary", "@rules_android//rules:rules.bzl"),
    rule("android_library", "@rules_android//rules:rules.bzl"),
    rule("android_local_test", "@rules_android//rules:rules.bzl"),
    rule("android_sdk", "@rules_android//rules:rules.bzl"),
    rule(
        "android_tools_defaults_jar",
        "@rules_android//rules:rules.bzl",
    ),
    rule("cc_binary", "@rules_cc//cc:cc_binary.bzl"),
    rule("cc_import", "@rules_cc//cc:cc_import.bzl"),
    rule("cc_library", "@rules_cc//cc:cc_library.bzl"),
    rule(
        "cc_proto_library",
        "@com_google_protobuf//bazel:cc_proto_library.bzl",
    ),
    rule("cc_shared_library", "@rules_cc//cc:cc_shared_library.bzl"),
    rule("cc_test", "@rules_cc//cc:cc_test.bzl"),
    rule("cc_toolchain", "@rules_cc//cc/toolchains:cc_toolchain.bzl"),
    rule(
        "cc_toolchain_suite",
        "@rules_cc//cc/toolchains:cc_toolchain_suite.bzl",
    ),
    rule(
        "fdo_prefetch_hints",
        "@rules_cc//cc/toolchains:fdo_prefetch_hints.bzl",
    ),
    rule("fdo_profile", "@rules_cc//cc/toolchains:fdo_profile.bzl"),
    rule("java_binary", "@rules_java//java:java_binary.bzl"),
    rule("java_import", "@rules_java//java:java_import.bzl"),
    rule("java_library", "@rules_java//java:java_library.bzl"),
    rule(
        "java_lite_proto_library",
        "@com_google_protobuf//bazel:java_lite_proto_library.bzl",
    ),
    rule(
        "java_package_configuration",
        "@rules_java//java/toolchains:java_package_configuration.bzl",
    ),
    rule("java_plugin", "@rules_java//java:java_plugin.bzl"),
    rule(
        "java_proto_library",
        "@com_google_protobuf//bazel:java_proto_library.bzl",
    ),
    rule(
        "java_runtime",
        "@rules_java//java/toolchains:java_runtime.bzl",
    ),
    rule("java_test", "@rules_java//java:java_test.bzl"),
    rule(
        "java_toolchain",
        "@rules_java//java/toolchains:java_toolchain.bzl",
    ),
    rule(
        "memprof_profile",
        "@rules_cc//cc/toolchains:memprof_profile.bzl",
    ),
    rule("objc_import", "@rules_cc//cc:objc_import.bzl"),
    rule("objc_library", "@rules_cc//cc:objc_library.bzl"),
    rule(
        "propeller_optimize",
        "@rules_cc//cc/toolchains:propeller_optimize.bzl",
    ),
    rule(
        "proto_lang_toolchain",
        "@com_google_protobuf//bazel/toolchains:proto_lang_toolchain.bzl",
    ),
    rule(
        "proto_library",
        "@com_google_protobuf//bazel:proto_library.bzl",
    ),
    rule("py_binary", "@rules_python//python:py_binary.bzl"),
    rule("py_library", "@rules_python//python:py_library.bzl"),
    rule("py_runtime", "@rules_python//python:py_runtime.bzl"),
    rule("py_test", "@rules_python//python:py_test.bzl"),
    rule("sh_binary", "@rules_shell//shell:sh_binary.bzl"),
    rule("sh_library", "@rules_shell//shell:sh_library.bzl"),
    rule("sh_test", "@rules_shell//shell:sh_test.bzl"),
    rule(
        "available_xcodes",
        "@build_bazel_apple_support//xcode:available_xcodes.bzl",
    ),
    rule(
        "xcode_config",
        "@build_bazel_apple_support//xcode:xcode_config.bzl",
    ),
    rule(
        "xcode_config_alias",
        "@build_bazel_apple_support//xcode:xcode_config_alias.bzl",
    ),
    rule(
        "xcode_version",
        "@build_bazel_apple_support//xcode:xcode_version.bzl",
    ),
    sym(
        "apple_common",
        "@build_bazel_apple_support//lib:apple_common.bzl",
        &["objc_import", "objc_library"],
    ),
];

/// Looks up the redirect metadata for an autoloadable symbol name.
pub fn lookup(symbol: &str) -> Option<&'static SymbolRedirect> {
    AUTOLOAD_CONFIG.iter().find(|r| r.symbol == symbol)
}

/// Parsed and validated state of `--incompatible_autoload_externally`,
/// `--incompatible_disable_autoloads_in_main_repo`, and `--repositories_without_autoloads`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoloadSymbols {
    enabled: bool,
    disable_in_main_repo: bool,
    autoloaded_symbols: Vec<String>,
    removed_symbols: Vec<String>,
    partially_removed_symbols: Vec<String>,
    repos_disallowing_autoloads: BTreeSet<String>,
}

impl Default for AutoloadSymbols {
    fn default() -> Self {
        Self::empty()
    }
}

impl AutoloadSymbols {
    /// Disabled autoload configuration (the Bazel 9.3.2 default).
    pub fn empty() -> Self {
        Self {
            enabled: false,
            disable_in_main_repo: true,
            autoloaded_symbols: Vec::new(),
            removed_symbols: Vec::new(),
            partially_removed_symbols: Vec::new(),
            repos_disallowing_autoloads: BTreeSet::new(),
        }
    }

    /// Parses and validates `--incompatible_autoload_externally` items.
    pub fn parse(
        entries: &[String],
        repos_without_autoloads: &[String],
        disable_in_main_repo: bool,
    ) -> Result<Self, String> {
        let non_empty: Vec<&str> = entries
            .iter()
            .map(String::as_str)
            .filter(|s| !s.is_empty())
            .collect();
        if non_empty.is_empty() {
            return Ok(Self {
                enabled: false,
                disable_in_main_repo,
                autoloaded_symbols: Vec::new(),
                removed_symbols: Vec::new(),
                partially_removed_symbols: Vec::new(),
                repos_disallowing_autoloads: BTreeSet::new(),
            });
        }

        // Expand `@repo` shortcuts (e.g. `+@rules_cc`, `-@rules_shell`, `@rules_python`).
        let mut expanded: Vec<String> = Vec::new();
        for item in non_empty {
            let (prefix, rest) = if let Some(r) = item.strip_prefix('+') {
                ("+", r)
            } else if let Some(r) = item.strip_prefix('-') {
                ("-", r)
            } else {
                ("", item)
            };
            if rest.starts_with('@') {
                let label_prefix = format!("{rest}//");
                for entry in AUTOLOAD_CONFIG {
                    if entry.load_label.starts_with(&label_prefix) {
                        expanded.push(format!("{prefix}{}", entry.symbol));
                    }
                }
            } else {
                expanded.push(item.to_string());
            }
        }

        // Validate duplicates and unknown symbols (matching `AutoloadSymbols` constructor).
        let mut seen: HashSet<String> = HashSet::new();
        for item in &expanded {
            let sym = item
                .strip_prefix('+')
                .or_else(|| item.strip_prefix('-'))
                .unwrap_or(item.as_str());
            if !seen.insert(sym.to_string()) {
                return Err(format!(
                    "Duplicated symbol '{sym}' in --incompatible_autoload_externally"
                ));
            }
            if lookup(sym).is_none() {
                return Err(format!(
                    "Undefined symbol in --incompatible_autoload_externally: {sym}"
                ));
            }
        }

        let strip_sign = |s: &str| {
            s.strip_prefix('+')
                .or_else(|| s.strip_prefix('-'))
                .unwrap_or(s)
                .to_string()
        };

        let autoloaded_symbols: Vec<String> = expanded
            .iter()
            .filter(|s| !s.starts_with('-') && !s.ends_with("proto_common_do_not_use"))
            .map(|s| strip_sign(s))
            .collect();
        let removed_symbols: Vec<String> = expanded
            .iter()
            .filter(|s| s.starts_with('-') || s.ends_with("proto_common_do_not_use"))
            .map(|s| strip_sign(s))
            .collect();
        let partially_removed_symbols: Vec<String> = expanded
            .iter()
            .filter(|s| !s.starts_with('+'))
            .map(|s| strip_sign(s))
            .collect();

        let mut repos_disallowing_autoloads: BTreeSet<String> =
            PREDECLARED_REPOS_DISALLOWING_AUTOLOADS
                .iter()
                .map(|s| (*s).to_string())
                .collect();
        for r in repos_without_autoloads {
            if !r.is_empty() {
                repos_disallowing_autoloads.insert(r.clone());
            }
        }

        Ok(Self {
            enabled: true,
            disable_in_main_repo,
            autoloaded_symbols,
            removed_symbols,
            partially_removed_symbols,
            repos_disallowing_autoloads,
        })
    }

    /// Validates that no partially-removed symbol is still depended on by a remaining builtin
    /// symbol (`AutoloadSymbols` constructor rdeps check).
    pub fn validate_rdeps(
        &self,
        remaining_without_autoloads: &BTreeSet<String>,
    ) -> Result<(), String> {
        if !self.enabled || self.partially_removed_symbols.is_empty() {
            return Ok(());
        }
        if self
            .partially_removed_symbols
            .iter()
            .any(|s| s == "ProtoInfo")
            && !self
                .removed_symbols
                .iter()
                .any(|s| s == "proto_common_do_not_use")
        {
            return Err(
                "Symbol in 'ProtoInfo' can't be removed, because it's still used by: 'proto_common_do_not_use'"
                    .to_string(),
            );
        }
        for sym in &self.partially_removed_symbols {
            if let Some(redirect) = lookup(sym) {
                let still_used: Vec<&str> = redirect
                    .rdeps
                    .iter()
                    .copied()
                    .filter(|rdep| remaining_without_autoloads.contains(*rdep))
                    .collect();
                if !still_used.is_empty() {
                    return Err(format!(
                        "Symbol '{sym}' can't be removed, because it's still used by: {}",
                        still_used.join(", ")
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn autoloaded_symbols(&self) -> &[String] {
        &self.autoloaded_symbols
    }

    pub fn removed_symbols(&self) -> &[String] {
        &self.removed_symbols
    }

    pub fn partially_removed_symbols(&self) -> &[String] {
        &self.partially_removed_symbols
    }

    /// Whether autoloads are disallowed in `.bzl` files of canonical repo name `repo_name`
    /// (`AutoloadSymbols.autoloadsDisabledForRepo`).
    pub fn disabled_for_repo(&self, repo_name: &str) -> bool {
        if !self.enabled {
            return true;
        }
        let idx = if let Some(i) = repo_name.find('~') {
            Some(i)
        } else {
            repo_name.find('+')
        };
        let prefix = match idx {
            Some(i) => &repo_name[..i],
            None => repo_name,
        };
        self.repos_disallowing_autoloads.contains(prefix)
    }

    /// Whether autoloads are disabled in `.bzl` files belonging to `repo`.
    pub fn disabled_in_bzl_for_repo(&self, repo: &RepositoryName) -> bool {
        if !self.enabled {
            return true;
        }
        if self.disable_in_main_repo && repo.is_main() {
            return true;
        }
        self.disabled_for_repo(repo.name())
    }

    /// Whether autoloads are disabled in `BUILD` files belonging to `repo`.
    pub fn disabled_in_build_for_repo(&self, repo: &RepositoryName) -> bool {
        if !self.enabled {
            return true;
        }
        self.disable_in_main_repo && repo.is_main()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_repo_shortcuts_and_classifies_signs() {
        let a = AutoloadSymbols::parse(&["+@rules_cc".into()], &[], true).unwrap();
        assert!(a.is_enabled());
        assert!(a.autoloaded_symbols().contains(&"cc_library".to_string()));
        assert!(a.autoloaded_symbols().contains(&"cc_binary".to_string()));
        assert!(a.autoloaded_symbols().contains(&"CcInfo".to_string()));
        assert!(a.removed_symbols().is_empty());
        assert!(a.partially_removed_symbols().is_empty());
        assert!(a.disabled_in_build_for_repo(&RepositoryName::main()));
        assert!(!a.disabled_in_build_for_repo(&RepositoryName::create_unvalidated("redis")));
        assert!(a.disabled_in_bzl_for_repo(&RepositoryName::create_unvalidated("rules_cc+")));
        assert!(
            a.disabled_in_bzl_for_repo(&RepositoryName::create_unvalidated(
                "rules_cc++compatibility_proxy+cc_compatibility_proxy"
            ))
        );
        assert!(!a.disabled_in_bzl_for_repo(&RepositoryName::create_unvalidated("my_ext_repo")));
    }

    #[test]
    fn rejects_duplicate_and_undefined_symbols() {
        let dup = AutoloadSymbols::parse(&["+cc_library".into(), "-cc_library".into()], &[], true);
        assert_eq!(
            dup.unwrap_err(),
            "Duplicated symbol 'cc_library' in --incompatible_autoload_externally"
        );
        let undef = AutoloadSymbols::parse(&["+ProtozzzInfo".into()], &[], true);
        assert_eq!(
            undef.unwrap_err(),
            "Undefined symbol in --incompatible_autoload_externally: ProtozzzInfo"
        );
    }

    #[test]
    fn minus_and_bare_classification() {
        let a = AutoloadSymbols::parse(
            &["+sh_binary".into(), "-cc_library".into(), "sh_test".into()],
            &["custom_repo".into()],
            false,
        )
        .unwrap();
        assert_eq!(a.autoloaded_symbols(), &["sh_binary", "sh_test"]);
        assert_eq!(a.removed_symbols(), &["cc_library"]);
        assert_eq!(a.partially_removed_symbols(), &["cc_library", "sh_test"]);
        assert!(!a.disabled_in_build_for_repo(&RepositoryName::main()));
        assert!(a.disabled_for_repo("custom_repo+1.0"));
    }

    #[test]
    fn removed_symbol_error_matches_bazel_9_3_2() {
        assert_eq!(
            removed_symbol_error("CcInfo", "@rules_cc//cc/common:cc_info.bzl"),
            "The CcInfo symbol has been removed, add the following to your BUILD/bzl file:\n\nload(\"@rules_cc//cc/common:cc_info.bzl\", \"CcInfo\")"
        );
    }
}
