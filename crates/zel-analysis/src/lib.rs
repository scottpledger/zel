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

//! Bazel Analysis Phase (`BuildConfigurationValue`, `Transition`, `Artifact`,
//! `ActionAnalysisMetadata`, `ConfiguredTarget`, and Core Providers).
//! Mirrors `src/main/java/com/google/devtools/build/lib/analysis` and `actions`.

use std::collections::BTreeMap;
use std::fmt;
use zel_cmdline::Label;
use zel_util::Fingerprint;
use zel_vfs::PathFragment;

pub mod action;
pub mod args;
pub mod build_options;
mod build_options_table;
pub mod fragments;
pub mod javac_options;
pub mod location;
pub mod nested_set;
pub mod objects;
pub mod repo_mapping;
pub mod runfiles;
pub mod select;
pub mod starlark_rule;
pub mod transition;

pub use action::*;
pub use args::{expand_tree_artifact_args, TREE_MARKER};
pub use runfiles::*;

/// Key identifying a `BuildConfigurationValue` in Skyframe.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BuildConfigurationKey {
    pub cpu: String,
    pub compilation_mode: String,
    pub is_exec: bool,
    pub core_only: bool,
    /// The target keeps the test fragment (`TestTrimmingTransitionFactory.create` returned
    /// `NoTransition`: test rules, test_suite, aliases): its options are the untrimmed ones.
    pub keep_test: bool,
    /// Exec configuration that keeps the test fragment (the exec transition applied directly to a test
    /// target's attribute, e.g. its coverage tools, before any test trimming).
    pub keep_test_exec: bool,
    /// Options a transition sets *after* the exec transition (`name` without dashes -> value);
    /// they also contribute to the `ST-<hash>` suffix through `transition_changes`.
    pub post_exec: BTreeMap<String, String>,
    /// `--define=k=v` values.
    pub user_defines: BTreeMap<String, String>,
    /// Starlark build-setting values (`--//pkg:flag=v`), keyed by canonical label string.
    pub flag_values: BTreeMap<String, String>,
    /// `config.<type>` of each Starlark flag in `flag_values` (label -> `string`/`bool`/`int`/`string_list`/...).
    pub flag_types: BTreeMap<String, String>,
    /// Other native options by name without dashes (`copt`, `cxxopt`, `platforms`, ...);
    /// multi-valued options are comma-joined.
    pub options: BTreeMap<String, String>,
    /// `--platforms` (target platform labels).
    pub platforms: Vec<String>,
    /// Options whose value differs from the baseline (top-level) configuration, as rendered by
    /// Java's `toString` (`[a, b]` for lists). Drives the `ST-<hash>` suffix of the output
    /// directory name; `cpu`, `compilation_mode` and `platform_suffix` never appear here (they
    /// are explicit in the directory name).
    pub transition_changes: BTreeMap<String, String>,
    /// Baseline rendering (`None` = unset) of every option touched by a transition, so that a
    /// later transition restoring the baseline value removes the change again
    /// (`OptionsDiff` against the baseline options).
    pub transition_baseline: BTreeMap<String, Option<String>>,
    /// Environment-related options (`--action_env`, `--host_action_env`, `--test_env`,
    /// `--incompatible_strict_action_env`, `--enable_runfiles`) in command-line order.
    pub env: zel_util::action_env::EnvConfig,
    /// Every native option set on the command line (post expansion), typed as its Java value
    /// (`build_options::ConfigValue`) together with the raw rendering that was put into
    /// [`options`](Self::options); drives [`build_options`](Self::build_options).
    pub native: BTreeMap<String, NativeSetting>,
}

/// A command-line native option: the raw (comma-joined) string mirrored in
/// `BuildConfigurationKey::options` and the converted Java value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NativeSetting {
    pub raw: String,
    pub value: build_options::ConfigValue,
}

/// Options that are explicit in the output directory name and therefore excluded from the
/// `ST-<hash>` (`OutputPathMnemonicComputer`).
pub const EXPLICIT_IN_OUTPUT_PATH: &[&str] = &[
    "//command_line_option:cpu",
    "//command_line_option:compilation_mode",
    "//command_line_option:platform_suffix",
];

fn varint_len_prefixed(s: &str, out: &mut Vec<u8>) {
    let mut n = s.len();
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
    out.extend_from_slice(s.as_bytes());
}

/// `OutputPathMnemonicComputer.transitionDirectoryNameFragment`: the first 12 hex digits of the
/// SHA-256 of the length-prefixed (protobuf varint) `key=value` strings.
pub fn transition_directory_name_fragment<'a>(opts: impl IntoIterator<Item = &'a str>) -> String {
    let mut buf = Vec::new();
    for o in opts {
        varint_len_prefixed(o, &mut buf);
    }
    let d = zel_util::sha256::digest(&buf);
    format!("ST-{}", &zel_util::to_hex(&d)[..12])
}

impl Default for BuildConfigurationKey {
    fn default() -> Self {
        Self::target("k8", "fastbuild")
    }
}

impl BuildConfigurationKey {
    fn platform_in_output_dir(&self) -> bool {
        self.native
            .get("experimental_platform_in_output_dir")
            .map(|setting| {
                !matches!(
                    setting.value.display().to_ascii_uppercase().as_str(),
                    "NO" | "FALSE" | "0"
                )
            })
            .unwrap_or(true)
    }

    pub fn target(cpu: impl Into<String>, compilation_mode: impl Into<String>) -> Self {
        Self {
            cpu: cpu.into(),
            compilation_mode: compilation_mode.into(),
            is_exec: false,
            core_only: false,
            keep_test: false,
            keep_test_exec: false,
            post_exec: BTreeMap::new(),
            user_defines: BTreeMap::new(),
            flag_values: BTreeMap::new(),
            flag_types: BTreeMap::new(),
            options: BTreeMap::new(),
            platforms: Vec::new(),
            transition_changes: BTreeMap::new(),
            transition_baseline: BTreeMap::new(),
            env: zel_util::action_env::EnvConfig::default(),
            native: BTreeMap::new(),
        }
    }

    /// Returns the string rendering of option `name` from `options`, `native`, or the default in
    /// `build_options_table`.
    pub fn option_str(&self, name: &str) -> Option<String> {
        let null_default = build_options::find_option(name)
            .map(|(_, o)| o.default == "null")
            .unwrap_or(false);
        if let Some(v) = self.options.get(name) {
            if null_default && (v == "null" || v == NULL_VALUE) {
                return None;
            }
            return Some(v.clone());
        }
        let native_name = if self.is_exec && name == "java_launcher" {
            "host_java_launcher"
        } else {
            name
        };
        if let Some(n) = self.native.get(native_name) {
            return match &n.value {
                build_options::ConfigValue::Null => None,
                other => Some(other.display()),
            };
        }
        build_options::find_option(name)
            .map(|(_, o)| o.default)
            .filter(|d| *d != "null")
            .map(str::to_string)
    }

    /// Reads a boolean option (`options` / `native` / default).
    pub fn bool_option(&self, name: &str, default: bool) -> bool {
        let val = self.option_str(name);
        match val.as_deref().map(str::to_ascii_lowercase).as_deref() {
            Some("true" | "1" | "yes" | "t" | "y") => true,
            Some("false" | "0" | "no" | "f" | "n") => false,
            _ => default,
        }
    }

    /// The configuration's action environment (`BuildConfigurationValue.getActionEnvironment`):
    /// `--host_action_env` replaces `--action_env` in exec configurations.
    pub fn action_environment(&self) -> zel_util::action_env::ActionEnvironment {
        self.env.action_environment(self.is_exec)
    }

    /// `BuildConfigurationValue.getTestActionEnvironment` (`--test_env`): empty when test
    /// options have been trimmed (`!keep_test && !keep_test_exec` or `core_only`).
    pub fn test_action_environment(&self) -> zel_util::action_env::ActionEnvironment {
        let trimmed = self.core_only
            || (!self.keep_test
                && !self.keep_test_exec
                && self.bool_option("trim_test_configuration", true));
        if trimmed {
            zel_util::action_env::ActionEnvironment::EMPTY
        } else {
            self.env.test_environment()
        }
    }

    /// The exec configuration for the default execution platform (`@platforms//host:host`).
    pub fn to_exec(&self) -> Self {
        self.to_exec_for("@@platforms//host:host")
    }

    /// The exec configuration of the tools a target needs when no toolchain resolution picked a platform:
    /// the execution platform is `--host_platform` (default `@platforms//host:host`).
    pub fn to_exec_on_host_platform(&self) -> Self {
        let host = self
            .native
            .get("host_platform")
            .map(|s| s.value.display())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "@@platforms//host:host".to_string());
        self.to_exec_for(&host)
    }

    /// The extra exec configurations `--collect_code_coverage` creates for the coverage tools of a test
    /// target (`coverage_support`, `lcov_merger`, ...): the exec transition of the *untrimmed* test
    /// configuration, and the tools' Java launcher variant with its own transition suffix. The Java
    /// runtime version is what the 9.3.2 coverage tools request (observed with the real binary:
    /// `...-opt-exec-ST-9b53c6127b00`).
    pub fn coverage_tool_configs(&self) -> Vec<Self> {
        let base = self.to_exec_on_host_platform();
        let mut untrimmed = base.clone();
        untrimmed.keep_test_exec = true;
        let mut java = base;
        java.post_exec.insert(
            "java_runtime_version".to_string(),
            "remotejdk_25".to_string(),
        );
        java.transition_changes.insert(
            "//command_line_option:java_runtime_version".to_string(),
            "remotejdk_25".to_string(),
        );
        vec![untrimmed, java]
    }

    pub fn to_core_only(&self) -> Self {
        let mut out = Self::target(build_options::auto_host_cpu(), "fastbuild");
        out.core_only = true;
        out
    }

    /// `ExecutionTransitionFactory`: the target configuration with `--platforms` set to the
    /// execution platform, `--compilation_mode` to `--host_compilation_mode` (`opt`) and the
    /// platform suffix `exec`. The baseline for the `ST-<hash>` diff stays the original
    /// top-level configuration.
    pub fn to_exec_for(&self, exec_platform: &str) -> Self {
        // The exec transition is idempotent: an exec configuration for the same execution
        // platform maps to itself (its `cpu`/`compilation_mode` already come from `--host_*`).
        if self.is_exec && self.platforms.len() == 1 && self.platforms[0] == exec_platform {
            return self.clone();
        }
        let mut out = self.clone();
        let native = |n: &str| self.native.get(n).map(|s| s.value.display());
        out.compilation_mode = native("host_compilation_mode")
            .or_else(|| self.options.get("host_compilation_mode").cloned())
            .unwrap_or_else(|| "opt".to_string());
        out.is_exec = true;
        out.keep_test = false;
        // `_core_options`: `cpu` becomes `--host_cpu` in the exec configuration.
        out.cpu = native("host_cpu")
            .or_else(|| self.options.get("host_cpu").cloned())
            .unwrap_or_else(|| build_options::auto_host_cpu().to_string());
        // JavaOptions.getExec: --plugin is not propagated to the exec configuration (avoids cycles),
        // while `java_launcher` in the exec configuration comes from `host_java_launcher`.
        out.options.remove("plugin");
        let host_launcher = self
            .options
            .get("host_java_launcher")
            .cloned()
            .or_else(|| native("host_java_launcher"))
            .filter(|s| !s.is_empty() && s != "null" && s != NULL_VALUE);
        if let Some(hl) = host_launcher {
            out.options.insert("java_launcher".to_string(), hl);
        } else {
            out.options.remove("java_launcher");
        }
        const KEY: &str = "//command_line_option:platforms";
        // `SkyframeExecutor.setBaselineConfiguration`: BASELINE_EXEC_CONFIGURATION is the exec
        // transition applied to the top-level options with the *host platform* as exec platform,
        // so only a different exec platform (or later transitions) contributes an `ST-<hash>`.
        let host_platform = native("host_platform")
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "@@platforms//host:host".to_string());
        out.transition_baseline
            .entry(KEY.to_string())
            .or_insert_with(|| Some(render_list(&[host_platform.clone()])));
        out.platforms = vec![exec_platform.to_string()];
        let new = render_list(&out.platforms);
        if out.transition_baseline[KEY].as_deref() == Some(new.as_str()) {
            out.transition_changes.remove(KEY);
        } else {
            out.transition_changes.insert(KEY.to_string(), new);
        }
        out
    }

    /// Output directory name: `<cpu>-<compilation_mode>[-<platform_suffix>][-ST-<hash>]`, e.g.
    /// `darwin_arm64-fastbuild`, `darwin_arm64-opt-exec`.
    pub fn mnemonic(&self) -> String {
        if self.core_only {
            return "fastbuild-noconfig".to_string();
        }
        let base = self.base_mnemonic();
        if self.transition_changes.is_empty() {
            return base;
        }
        let entries: Vec<String> = self
            .transition_changes
            .iter()
            .map(|(k, v)| {
                if v == NULL_VALUE {
                    format!("{k}@null")
                } else {
                    format!("{k}={v}")
                }
            })
            .collect();
        format!(
            "{base}-{}",
            transition_directory_name_fragment(entries.iter().map(String::as_str))
        )
    }

    /// The mnemonic `bazel config` lists: the base mnemonic plus `ST-<hash>` of the transition changes,
    /// except that an exec platform change is already visible in the base mnemonic (`p-opt-exec`) and
    /// so does not contribute to the hash (verified against the 9.3.2 binary).
    pub fn config_display_mnemonic(&self) -> String {
        if self.core_only {
            return "fastbuild-noconfig".to_string();
        }
        let platform_in_output_dir = self.platform_in_output_dir();
        let entries: Vec<String> = self
            .transition_changes
            .iter()
            .filter(|(k, _)| {
                !(platform_in_output_dir
                    && self.is_exec
                    && k.as_str() == "//command_line_option:platforms")
            })
            .map(|(k, v)| {
                if v == NULL_VALUE {
                    format!("{k}@null")
                } else {
                    format!("{k}={v}")
                }
            })
            .collect();
        if entries.is_empty() {
            return self.base_mnemonic();
        }
        format!(
            "{}-{}",
            self.base_mnemonic(),
            transition_directory_name_fragment(entries.iter().map(String::as_str))
        )
    }

    /// `BuildConfigurationValue.getMnemonic()` without the `ST-<hash>` suffix: what `config`
    /// lists (`<cpu>-<compilation_mode>[-exec|-<platform_suffix>]`).
    pub fn base_mnemonic(&self) -> String {
        if self.core_only {
            return "fastbuild-noconfig".to_string();
        }
        let mode_setting = self
            .native
            .get("experimental_platform_in_output_dir")
            .map(|n| n.value.display().to_ascii_uppercase())
            .unwrap_or_else(|| "AUTO".to_string());
        // The exec configuration of a non-default execution platform is named after that platform
        // when `--experimental_platform_in_output_dir` is `auto` or `yes`.
        // When `yes`, target configurations also use the platform short name when `--platforms` is set.
        let cpu_part = if self.is_exec && self.platform_in_output_dir() {
            self.platforms
                .first()
                .filter(|p| {
                    !matches!(
                        p.as_str(),
                        "@@platforms//host:host" | "@platforms//host:host"
                    )
                })
                .map(|p| p.rsplit([':', '/']).next().unwrap_or(p).to_string())
                .unwrap_or_else(|| self.cpu.clone())
        } else if !self.is_exec && matches!(mode_setting.as_str(), "YES" | "TRUE" | "1") {
            self.platforms
                .first()
                .map(|p| p.rsplit([':', '/']).next().unwrap_or(p).to_string())
                .unwrap_or_else(|| self.cpu.clone())
        } else {
            self.cpu.clone()
        };
        let mut base = format!("{}-{}", cpu_part, self.compilation_mode);
        if self.is_exec {
            base.push_str("-exec");
            if self
                .native
                .get("experimental_exec_configuration_distinguisher")
                .map(|n| n.value.display().to_ascii_uppercase())
                == Some("LEGACY".to_string())
            {
                let plat = self
                    .platforms
                    .first()
                    .map(String::as_str)
                    .unwrap_or("@@platforms//host:host");
                let mut h: i32 = 0;
                for u in plat.encode_utf16() {
                    h = h.wrapping_mul(31).wrapping_add(u as i32);
                }
                base.push_str(&format!("-{:08X}", h as u32));
            } else if !self.platform_in_output_dir() {
                base.push_str("-ST-d57f47055a04");
            }
        } else {
            if let Some(tag) = self
                .native
                .get("cc_output_directory_tag")
                .map(|n| n.value.display())
                .filter(|s| !s.is_empty())
            {
                base.push('-');
                base.push_str(&tag);
            }
            if let Some(sfx) = self
                .options
                .get("platform_suffix")
                .filter(|s| !s.is_empty())
            {
                base.push('-');
                base.push_str(sfx);
            }
        }
        base
    }

    /// The full typed option state of this configuration as `BuildOptions` would hold it for a
    /// test-capable target configuration (all fragments incl. `TestOptions`); for exec
    /// configurations the result of the exec transition (already test-trimmed).
    pub fn untrimmed_build_options(&self) -> build_options::BuildOptions {
        use build_options::{find_option, BuildOptions, ConfigValue};
        let host_cpu = build_options::auto_host_cpu();
        let mut o = BuildOptions::defaults(host_cpu);
        for (name, n) in &self.native {
            o.set(name, n.value.clone());
        }
        // `PlatformOptions.platforms` defaults to `--host_platform` when it is not set explicitly.
        if !self.native.contains_key("platforms") {
            if let Some(h) = self
                .native
                .get("host_platform")
                .map(|n| n.value.display())
                .filter(|h| !h.is_empty())
            {
                o.set("platforms", ConfigValue::List(vec![h]));
            }
        }
        o.set("cpu", ConfigValue::Scalar(self.cpu.clone()));
        o.set(
            "compilation_mode",
            ConfigValue::Scalar(self.compilation_mode.clone()),
        );
        // `--define`: command-line order with later values replacing earlier ones in place
        // (`FragmentOptions.normalizeEntries`), or the transition-written map.
        let from_native: Option<Vec<String>> =
            self.native.get("define").and_then(|n| match &n.value {
                ConfigValue::List(l) => Some(l.clone()),
                _ => None,
            });
        let defines: Vec<String> = match from_native {
            Some(l) => {
                let mut m: Vec<(String, String)> = Vec::new();
                for e in &l {
                    let (k, v) = e.split_once('=').unwrap_or((e, ""));
                    match m.iter_mut().find(|(mk, _)| mk == k) {
                        Some(slot) => slot.1 = v.to_string(),
                        None => m.push((k.to_string(), v.to_string())),
                    }
                }
                let as_map: BTreeMap<String, String> = m.iter().cloned().collect();
                if as_map == self.user_defines {
                    m.into_iter().map(|(k, v)| format!("{k}={v}")).collect()
                } else {
                    self.user_defines
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect()
                }
            }
            None => self
                .user_defines
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect(),
        };
        o.set("define", ConfigValue::List(defines));
        if !self.platforms.is_empty() {
            o.set("platforms", ConfigValue::List(self.platforms.clone()));
        }
        // Options written by transitions differ from what the command line set.
        for (name, raw) in &self.options {
            if self.native.get(name).map(|n| &n.raw) == Some(raw) {
                continue;
            }
            if let Some((_, opt)) = find_option(name) {
                let is_list = opt.java_type.starts_with("List<")
                    || opt.java_type.starts_with("ImmutableList<");
                if opt.default == "null" && (raw == "null" || raw == NULL_VALUE) {
                    o.set(
                        name,
                        if is_list {
                            ConfigValue::List(vec![])
                        } else {
                            ConfigValue::Null
                        },
                    );
                    continue;
                }
                let raws: Vec<String> = if is_list {
                    raw.split(',')
                        .filter(|p| !p.is_empty())
                        .map(str::to_string)
                        .collect()
                } else {
                    vec![raw.clone()]
                };
                if let Ok(v) = build_options::convert_raws(
                    opt,
                    &raws,
                    &build_options::canonical_label_default,
                    host_cpu,
                ) {
                    o.set(name, v);
                }
            }
        }
        for (k, v) in &self.transition_changes {
            let Some(name) = k.strip_prefix("//command_line_option:") else {
                continue;
            };
            if let Some((_, opt)) = find_option(name) {
                if v == NULL_VALUE {
                    let is_list = opt.java_type.starts_with("List<")
                        || opt.java_type.starts_with("ImmutableList<");
                    o.set(
                        name,
                        if is_list {
                            ConfigValue::List(vec![])
                        } else {
                            ConfigValue::Null
                        },
                    );
                } else if let Some(inner) = v.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
                    // Java `List.toString` rendering.
                    let items: Vec<String> = if inner.is_empty() {
                        vec![]
                    } else {
                        inner.split(", ").map(str::to_string).collect()
                    };
                    o.set(name, ConfigValue::List(items));
                } else {
                    o.set(name, ConfigValue::Scalar(v.clone()));
                }
            }
        }
        for (k, v) in &self.flag_values {
            // `Label.toString()`: the main repository prints as `//pkg:name`.
            let label = k
                .strip_prefix("@@")
                .filter(|r| r.starts_with("//"))
                .unwrap_or(k);
            let rendered = match self.flag_types.get(k) {
                Some(ty) => starlark_rule::render_flag_value(ty, v),
                None => v.clone(),
            };
            o.starlark.insert(label.to_string(), rendered);
        }
        o.normalize();
        if self.is_exec {
            let platform = self
                .platforms
                .first()
                .cloned()
                .unwrap_or_else(|| "@@platforms//host:host".to_string());
            let mut e = o.exec(&platform);
            for (name, v) in &self.post_exec {
                e.set(name, ConfigValue::Scalar(v.clone()));
            }
            e.normalize();
            return e;
        }
        o
    }

    /// The options of the configuration non-test targets are analysed in (`TestTrimmingLogic.trim`).
    pub fn build_options(&self) -> build_options::BuildOptions {
        if self.core_only {
            return self.untrimmed_build_options().core_only();
        }
        let o = self.untrimmed_build_options();
        // `TestTrimmingTransition` is a no-op with `--notrim_test_configuration`.
        let trim =
            o.get("trim_test_configuration").map(|v| v.display()) != Some("false".to_string());
        if !trim || (self.keep_test && !self.is_exec) || self.keep_test_exec {
            o
        } else {
            o.trim_test_configuration()
        }
    }

    /// `BuildOptions.checksum()` of the (test-trimmed) configuration non-test targets use.
    pub fn checksum(&self) -> String {
        self.build_options().checksum()
    }

    pub fn short_hash(&self) -> String {
        let full = self.checksum();
        let hash = full[..7].to_string();
        record_configuration(&hash, self);
        record_configuration(&full, self);
        hash
    }

    /// Global `--features` (or `--host_features` in an exec configuration).
    pub fn global_features(&self) -> Vec<String> {
        let bo = self.build_options();
        if let Some(build_options::ConfigValue::List(l)) = bo.get("features") {
            if !l.is_empty() {
                return l.clone();
            }
        }
        let key = if self.is_exec {
            "host_features"
        } else {
            "features"
        };
        self.options
            .get(key)
            .or_else(|| self.options.get("features"))
            .map(|s| {
                s.split(',')
                    .filter(|x| !x.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }
}

static CONFIGURATION_REGISTRY: std::sync::Mutex<BTreeMap<String, BuildConfigurationKey>> =
    std::sync::Mutex::new(BTreeMap::new());

fn record_configuration(hash: &str, key: &BuildConfigurationKey) {
    let mut reg = CONFIGURATION_REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !reg.contains_key(hash) {
        reg.insert(hash.to_string(), key.clone());
    }
}

/// Every configuration whose hash was computed in this process, by `short_hash()` (what
/// `bazel config` lists, restricted by the caller to those still present in Skyframe).
pub fn registered_configurations() -> BTreeMap<String, BuildConfigurationKey> {
    CONFIGURATION_REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Forgets every recorded configuration. Called when the analysis cache is discarded (the options affecting
/// analysis changed between two invocations): Skyframe then drops the `BuildConfigurationValue` nodes that
/// `bazel config` lists, so only the configurations of the most recent analysis remain.
pub fn reset_registered_configurations() {
    CONFIGURATION_REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|h, c| (c.is_exec && h == "04bf134") || c.core_only);
}

pub fn clear_exec_configurations() {
    CONFIGURATION_REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|_, c| c.core_only);
}

/// Marker stored in `transition_changes` for an option a transition reset to "unset".
pub const NULL_VALUE: &str = "\u{0}null";

/// Java `List.toString` rendering: `[a, b]`.
pub fn render_list(items: &[String]) -> String {
    format!("[{}]", items.join(", "))
}

/// Configuration transition kinds (`NoTransition`, `ExecTransition`, `TargetTransition`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigurationTransition {
    None,
    Exec,
    Target,
}

impl ConfigurationTransition {
    pub fn apply(&self, current: &BuildConfigurationKey) -> BuildConfigurationKey {
        match self {
            Self::None | Self::Target => current.clone(),
            Self::Exec => current.to_exec(),
        }
    }
}

/// Represents a source or generated file in the action graph, matching `Artifact.java`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Artifact {
    pub is_source: bool,
    pub is_tree_artifact: bool,
    /// Which output root the artifact lives under (`Source` for source files).
    pub root: ArtifactRoot,
    /// `declare_symlink()` artifact: an unresolved (possibly dangling) symlink.
    pub is_unresolved_symlink: bool,
    /// `ConstantMetadataArtifact` (`volatile-status.txt` and outputs of `transform_version_file`):
    /// action cache digesting tracks existence rather than content so volatile updates do not
    /// invalidate downstream actions.
    pub is_constant_metadata: bool,
    /// Path relative to `$EXECROOT/_main` (e.g. `src/main.rs` or `bazel-out/darwin_arm64-fastbuild/bin/src/app`).
    pub exec_path: PathFragment,
    /// Path relative to the owning root (`src/main.rs` or `src/app`).
    pub root_relative_path: PathFragment,
    pub owner_label: Label,
}

impl Artifact {
    pub fn source(root_relative_path: PathFragment, owner_label: Label) -> Self {
        Self::source_with_layout(root_relative_path, owner_label, false)
    }

    /// Source artifact respecting `--experimental_sibling_repository_layout`
    /// (`../<repo>/...` when true, `external/<repo>/...` when false).
    pub fn source_with_layout(
        root_relative_path: PathFragment,
        owner_label: Label,
        sibling_repository_layout: bool,
    ) -> Self {
        let (root_relative_path, exec_path) = if owner_label.repository().is_main() {
            (root_relative_path.clone(), root_relative_path)
        } else {
            let repo_name = owner_label.repository().name();
            let raw = root_relative_path.path_string();
            let legacy_exact = format!("external/{repo_name}");
            let legacy_prefix = format!("{legacy_exact}/");
            let sib_exact = format!("../{repo_name}");
            let sib_prefix = format!("{sib_exact}/");
            let repo_rel = if raw == legacy_exact || raw == sib_exact {
                ""
            } else if let Some(s) = raw.strip_prefix(legacy_prefix.as_str()) {
                s
            } else if let Some(s) = raw.strip_prefix(sib_prefix.as_str()) {
                s
            } else {
                raw
            };
            let prefix = if sibling_repository_layout {
                sib_exact
            } else {
                legacy_exact
            };
            let exec = if repo_rel.is_empty() {
                PathFragment::create(&prefix)
            } else {
                PathFragment::create(&format!("{prefix}/{repo_rel}"))
            };
            (PathFragment::create(repo_rel), exec)
        };
        Self {
            is_source: true,
            is_tree_artifact: false,
            root: ArtifactRoot::Source,
            is_unresolved_symlink: false,
            is_constant_metadata: false,
            exec_path,
            root_relative_path,
            owner_label,
        }
    }

    /// `OutputDirectories.getBinDirectory(repository)`: `bazel-out/<repo>/<mnemonic>/bin`
    /// for external repositories under `--experimental_sibling_repository_layout`, and
    /// `bazel-out/<mnemonic>/bin` otherwise.
    pub fn bin_dir_for_repo(
        config_mnemonic: &str,
        repo: &zel_cmdline::RepositoryName,
        sibling_repository_layout: bool,
    ) -> String {
        if sibling_repository_layout && !repo.is_main() {
            format!("bazel-out/{}/{config_mnemonic}/bin", repo.name())
        } else {
            format!("bazel-out/{config_mnemonic}/bin")
        }
    }

    /// Derived files of targets in external repositories live below `external/<repo>/` (their
    /// root-relative path carries the prefix; `short_path` strips it again) unless
    /// `--experimental_sibling_repository_layout` is enabled.
    fn external_prefixed(owner: &Label, rel: PathFragment) -> PathFragment {
        let repo = owner.package_identifier().repository();
        if repo.is_main() {
            return rel;
        }
        let prefix = format!("external/{}/", repo.name());
        if rel.path_string().starts_with(prefix.as_str()) {
            rel
        } else {
            PathFragment::create(&format!("{prefix}{}", rel.path_string()))
        }
    }

    pub fn derived(
        config_mnemonic: &str,
        root_relative_path: PathFragment,
        owner_label: Label,
    ) -> Self {
        Self::derived_in_root_with_layout(
            config_mnemonic,
            ArtifactRoot::Bin,
            root_relative_path,
            owner_label,
            false,
        )
    }

    pub fn derived_with_layout(
        config_mnemonic: &str,
        root_relative_path: PathFragment,
        owner_label: Label,
        sibling_repository_layout: bool,
    ) -> Self {
        Self::derived_in_root_with_layout(
            config_mnemonic,
            ArtifactRoot::Bin,
            root_relative_path,
            owner_label,
            sibling_repository_layout,
        )
    }
}

impl Artifact {
    /// Derived artifact under an explicit root (`bin` or `genfiles`) for a configuration
    /// output-directory name such as `darwin_arm64-fastbuild`. Artifacts owned by a label in
    /// an external repository live under `external/<repo>/` below the root, like Bazel.
    pub fn derived_in_root(
        config_mnemonic: &str,
        root: ArtifactRoot,
        root_relative_path: PathFragment,
        owner_label: Label,
    ) -> Self {
        Self::derived_in_root_with_layout(
            config_mnemonic,
            root,
            root_relative_path,
            owner_label,
            false,
        )
    }

    /// Like [`Artifact::derived_in_root`], respecting `--experimental_sibling_repository_layout`
    /// (`bazel-out/<repo>/<mnemonic>/bin/<pkg>/...` for external repositories when enabled).
    pub fn derived_in_root_with_layout(
        config_mnemonic: &str,
        root: ArtifactRoot,
        root_relative_path: PathFragment,
        owner_label: Label,
        sibling_repository_layout: bool,
    ) -> Self {
        let repo = owner_label.package_identifier().repository();
        let root_relative_path = if sibling_repository_layout {
            root_relative_path
        } else {
            Self::external_prefixed(&owner_label, root_relative_path)
        };
        // `--incompatible_merge_genfiles_directory` (default): genfiles live under `bin`.
        let bin_dir = Self::bin_dir_for_repo(config_mnemonic, repo, sibling_repository_layout);
        let exec_path = if root_relative_path.is_empty() {
            PathFragment::create(&bin_dir)
        } else {
            PathFragment::create(&format!("{bin_dir}/{root_relative_path}"))
        };
        Self {
            is_source: false,
            is_tree_artifact: false,
            root,
            is_unresolved_symlink: false,
            is_constant_metadata: false,
            exec_path,
            root_relative_path,
            owner_label,
        }
    }

    /// Base name (`foo.txt` of `a/b/foo.txt`).
    pub fn basename(&self) -> &str {
        self.root_relative_path.base_name()
    }

    /// File extension without the dot, or `""` (`File.extension`).
    pub fn extension(&self) -> &str {
        let b = self.basename();
        match b.rfind('.') {
            Some(i) => &b[i + 1..],
            None => "",
        }
    }

    /// `File.dirname`: the directory of the exec path (always relative to the execution root).
    pub fn dirname(&self) -> String {
        let sp = self.exec_path.path_string().to_string();
        match sp.rfind('/') {
            Some(i) => sp[..i].to_string(),
            None => String::new(),
        }
    }

    /// `Artifact.getRepositoryRelativePath`: path relative to the artifact's repository root.
    pub fn repository_relative_path(&self) -> PathFragment {
        let repo = self.owner_label.repository();
        if repo.is_main() {
            return self.root_relative_path.clone();
        }
        let rel = self.root_relative_path.path_string();
        let legacy_exact = format!("external/{}", repo.name());
        let legacy_prefix = format!("{legacy_exact}/");
        let sib_exact = format!("../{}", repo.name());
        let sib_prefix = format!("{sib_exact}/");
        if rel == legacy_exact || rel == sib_exact {
            PathFragment::empty()
        } else if let Some(s) = rel.strip_prefix(legacy_prefix.as_str()) {
            PathFragment::create(s)
        } else if let Some(s) = rel.strip_prefix(sib_prefix.as_str()) {
            PathFragment::create(s)
        } else {
            self.root_relative_path.clone()
        }
    }

    /// `Artifact.getOutputDirRelativePath(siblingRepositoryLayout)`: used by
    /// `ctx.actions.declare_file(..., sibling = ...)` and `declare_directory` / `declare_symlink`.
    pub fn output_dir_relative_path(&self, sibling_repository_layout: bool) -> PathFragment {
        if self.is_source {
            if sibling_repository_layout {
                self.repository_relative_path()
            } else {
                self.exec_path.clone()
            }
        } else {
            self.root_relative_path.clone()
        }
    }

    /// `File.short_path`: path relative to the runfiles root (`../repo/x` for external repos).
    pub fn short_path(&self) -> String {
        let repo = self.owner_label.repository();
        if repo.is_main() {
            self.root_relative_path.path_string().to_string()
        } else {
            let rel_frag = self.repository_relative_path();
            let rel = rel_frag.path_string();
            if rel.is_empty() {
                format!("../{}", repo.name())
            } else {
                format!("../{}/{}", repo.name(), rel)
            }
        }
    }

    /// `File.root.path`: exec path of the root directory (`""` for source files).
    pub fn root_path(&self) -> String {
        let ep = self.exec_path.path_string();
        let rel = self.root_relative_path.path_string();
        if self.is_source {
            String::new()
        } else {
            ep.strip_suffix(rel)
                .unwrap_or(ep)
                .trim_end_matches('/')
                .to_string()
        }
    }

    /// `Artifact.equalsWithoutOwner`: compares exec path, root, and special-artifact flags,
    /// ignoring `owner_label`.
    pub fn equals_without_owner(&self, other: &Self) -> bool {
        self.is_source == other.is_source
            && self.is_tree_artifact == other.is_tree_artifact
            && self.is_unresolved_symlink == other.is_unresolved_symlink
            && self.is_constant_metadata == other.is_constant_metadata
            && self.root == other.root
            && self.exec_path == other.exec_path
    }

    /// `Artifact.isDirectory`: true for tree artifacts.
    pub fn is_directory(&self) -> bool {
        self.is_tree_artifact
    }

    /// `Artifact.isRunfilesTree`: true when this artifact represents a `.runfiles` tree directory.
    pub fn is_runfiles_tree(&self) -> bool {
        self.is_tree_artifact && self.exec_path.base_name().ends_with(".runfiles")
    }

    /// `Artifact.valueIsShareable`: true for standard artifacts.
    pub fn value_is_shareable(&self) -> bool {
        true
    }
}

/// Ownerless wrapper around [`Artifact`] (`Actions.OwnerlessArtifactWrapper` /
/// `MapBasedActionGraph` key): equality and ordering ignore `owner_label`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OwnerlessArtifactWrapper {
    pub is_source: bool,
    pub is_tree_artifact: bool,
    pub is_unresolved_symlink: bool,
    pub root: ArtifactRoot,
    pub exec_path: PathFragment,
}

impl From<&Artifact> for OwnerlessArtifactWrapper {
    fn from(a: &Artifact) -> Self {
        Self {
            is_source: a.is_source,
            is_tree_artifact: a.is_tree_artifact,
            is_unresolved_symlink: a.is_unresolved_symlink,
            root: a.root,
            exec_path: a.exec_path.clone(),
        }
    }
}

/// Output root of an [`Artifact`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArtifactRoot {
    /// Workspace/external-repo source tree.
    Source,
    /// `bazel-out/<cfg>/bin`
    Bin,
    /// Rules with `output_to_genfiles = True` (same directory as `Bin`, genfiles merged).
    Genfiles,
}

impl fmt::Display for Artifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.exec_path)
    }
}

/// What the executor needs to run a `JavaCompileAction` with a reduced classpath
/// (`JavaCompileAction.java`): the first attempt passes only the direct jars plus the jars named in
/// the `.jdeps` of the direct dependencies; if JavaBuilder reports
/// `requires_reduced_classpath_fallback` the action is re-run with the full classpath.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JavaCompileSpec {
    /// `JavaClasspathMode` name: `BAZEL` or `BAZEL_NO_FALLBACK`.
    pub classpath_mode: String,
    /// Index of the classpath command line among the action's arguments (its param file is
    /// written by the executor for every attempt).
    pub classpath_arg_index: usize,
    /// Exec path of that param file (set when the action is lowered).
    pub classpath_params_path: String,
    /// The full classpath (`transitiveInputs`), in order.
    pub transitive_inputs: Vec<String>,
    pub direct_jars: Vec<String>,
    /// `.jdeps` files of the direct dependencies (`dependencyArtifacts`).
    pub dependency_artifacts: Vec<String>,
    /// Inputs that are needed even when the classpath is reduced (`mandatoryInputs`).
    pub mandatory_inputs: Vec<String>,
    /// The action's `.jdeps` output.
    pub deps_proto: String,
}

/// Execution-time Starlark translator callback for `actions.transform_version_file` and
/// `actions.transform_info_file` (`BuildInfoFileWriteAction`).
pub type BuildInfoTranslateFn = std::sync::Arc<
    dyn Fn(&BTreeMap<String, String>) -> Result<Vec<(String, String)>, String> + Send + Sync,
>;

#[derive(Clone)]
pub struct BuildInfoTranslator {
    pub key: String,
    func: BuildInfoTranslateFn,
}

impl BuildInfoTranslator {
    pub fn new(key: impl Into<String>, func: BuildInfoTranslateFn) -> Self {
        Self {
            key: key.into(),
            func,
        }
    }

    pub fn translate(
        &self,
        status_map: &BTreeMap<String, String>,
    ) -> Result<Vec<(String, String)>, String> {
        (self.func)(status_map)
    }
}

impl fmt::Debug for BuildInfoTranslator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BuildInfoTranslator")
            .field("key", &self.key)
            .finish()
    }
}

impl PartialEq for BuildInfoTranslator {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for BuildInfoTranslator {}

/// Parses workspace status file bytes (`stable-status.txt` / `volatile-status.txt`) into `out`,
/// matching `BuildInfoTranslationHelper::ParseFile`: splits each `' '`-bearing line on the first
/// `' '` without trimming values and fails with `<key> is duplicated in the file.` if `key` is
/// already present, while lines without `' '` insert `(line, "")` only if absent (ignoring later
/// spaceless repeats without overwriting existing values).
pub fn parse_workspace_status_file(
    bytes: &[u8],
    out: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    let text = String::from_utf8_lossy(bytes);
    let body = text.strip_suffix('\n').unwrap_or(&text);
    if body.is_empty() {
        return Ok(());
    }
    for raw_line in body.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() {
            continue;
        }
        if let Some((key, val)) = line.split_once(' ') {
            if out.contains_key(key) {
                return Err(format!("{key} is duplicated in the file."));
            }
            out.insert(key.to_string(), val.to_string());
        } else {
            out.entry(line.to_string()).or_default();
        }
    }
    Ok(())
}

/// Reads and parses a workspace status file from `fs`, returning `Could not open file: <path>`
/// without modifying `out` when `path` cannot be opened.
pub fn read_workspace_status_file(
    fs: &dyn zel_vfs::FileSystem,
    path: &PathFragment,
    out: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    let bytes = fs
        .read_file(path)
        .map_err(|_| format!("Could not open file: {path}"))?;
    parse_workspace_status_file(&bytes, out)
}

/// Formats a UTC Unix epoch timestamp in Bazel's `FORMATTED_DATE` layout (`yyyy MMM d HH mm ss EEE`,
/// e.g. `1970 Jan 1 00 00 00 Thu`).
pub fn format_utc_build_date(epoch_secs: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let days = epoch_secs.div_euclid(86_400);
    let sod = epoch_secs.rem_euclid(86_400);
    let hour = sod / 3_600;
    let minute = (sod % 3_600) / 60;
    let second = sod % 60;
    let wday = (days + 4).rem_euclid(7) as usize;
    // Howard Hinnant's civil_from_days (UTC proleptic Gregorian).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    format!(
        "{y:04} {} {d} {hour:02} {minute:02} {second:02} {}",
        MONTHS[(m - 1) as usize],
        WEEKDAYS[wday],
    )
}

/// Action specification generated during the Analysis phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionAnalysisMetadata {
    SpawnAction {
        owner: Label,
        mnemonic: String,
        progress_message: String,
        argv: Vec<String>,
        /// The fixed part of the action's environment (`ActionEnvironment.getFixedEnv`).
        env: BTreeMap<String, String>,
        /// Variables taken from the client environment when the action executes
        /// (`ActionEnvironment.getInheritedEnv`, `Action.getClientEnvironmentVariables`).
        inherited_env: std::collections::BTreeSet<String>,
        /// Variables removed from the resolved environment (`ActionEnvironment.getUnsetEnv`).
        unset_env: std::collections::BTreeSet<String>,
        inputs: Vec<Artifact>,
        outputs: Vec<Artifact>,
        execution_info: BTreeMap<String, String>,
        /// Tool inputs of the spawn (`Action.getTools` / `Spawn.getToolFiles`).
        tools: Vec<Artifact>,
        /// Reduced-classpath data of a `JavaCompileAction` (`--experimental_java_classpath=bazel`).
        java_compile: Option<Box<JavaCompileSpec>>,
    },
    FileWriteAction {
        owner: Label,
        output: Artifact,
        content: Vec<u8>,
        is_executable: bool,
    },
    SymlinkAction {
        owner: Label,
        input_path: PathFragment,
        output: Artifact,
        is_executable: bool,
    },
    TemplateExpansionAction {
        owner: Label,
        template: Artifact,
        output: Artifact,
        substitutions: Vec<(String, String)>,
        is_executable: bool,
    },
    BuildInfoTranslateAction {
        owner: Label,
        template: Artifact,
        status_file: Artifact,
        output: Artifact,
        is_volatile: bool,
        translator: BuildInfoTranslator,
    },
}

impl ActionAnalysisMetadata {
    pub fn mnemonic(&self) -> &str {
        match self {
            Self::SpawnAction { mnemonic, .. } => mnemonic.as_str(),
            Self::FileWriteAction { .. } => "FileWrite",
            Self::SymlinkAction { .. } => "Symlink",
            Self::TemplateExpansionAction { .. } => "TemplateExpand",
            Self::BuildInfoTranslateAction { .. } => "TranslateBuildInfo",
        }
    }

    pub fn owner(&self) -> &Label {
        match self {
            Self::SpawnAction { owner, .. }
            | Self::FileWriteAction { owner, .. }
            | Self::SymlinkAction { owner, .. }
            | Self::TemplateExpansionAction { owner, .. }
            | Self::BuildInfoTranslateAction { owner, .. } => owner,
        }
    }

    pub fn inputs(&self) -> Vec<Artifact> {
        match self {
            Self::SpawnAction { inputs, .. } => inputs.clone(),
            Self::TemplateExpansionAction { template, .. } => vec![template.clone()],
            Self::BuildInfoTranslateAction {
                status_file,
                template,
                ..
            } => vec![status_file.clone(), template.clone()],
            Self::FileWriteAction { .. } | Self::SymlinkAction { .. } => Vec::new(),
        }
    }

    pub fn tools(&self) -> &[Artifact] {
        match self {
            Self::SpawnAction { tools, .. } => tools.as_slice(),
            _ => &[],
        }
    }

    pub fn outputs(&self) -> Vec<Artifact> {
        match self {
            Self::SpawnAction { outputs, .. } => outputs.clone(),
            Self::FileWriteAction { output, .. }
            | Self::SymlinkAction { output, .. }
            | Self::TemplateExpansionAction { output, .. }
            | Self::BuildInfoTranslateAction { output, .. } => vec![output.clone()],
        }
    }

    /// Computes a deterministic SHA-256 action key over the action's mnemonic, command line,
    /// environment, and output paths.
    pub fn action_key(&self) -> String {
        let mut fp = Fingerprint::new();
        fp.add_str(self.mnemonic());
        match self {
            Self::SpawnAction {
                argv,
                env,
                inherited_env,
                unset_env,
                inputs,
                outputs,
                ..
            } => {
                fp.add_strings(argv);
                for (k, v) in env {
                    fp.add_str(k).add_str(v);
                }
                if !inherited_env.is_empty() || !unset_env.is_empty() {
                    fp.add_str("inherited").add_strings(inherited_env.iter());
                    fp.add_str("unset").add_strings(unset_env.iter());
                }
                for inp in inputs {
                    fp.add_str(inp.exec_path.path_string());
                }
                for out in outputs {
                    fp.add_str(out.exec_path.path_string());
                }
            }
            Self::FileWriteAction {
                output,
                content,
                is_executable,
                ..
            } => {
                fp.add_str(output.exec_path.path_string())
                    .add_bytes(content)
                    .add_bool(*is_executable);
            }
            Self::SymlinkAction {
                input_path,
                output,
                is_executable,
                ..
            } => {
                fp.add_str(input_path.path_string())
                    .add_str(output.exec_path.path_string())
                    .add_bool(*is_executable);
            }
            Self::TemplateExpansionAction {
                template,
                output,
                substitutions,
                is_executable,
                ..
            } => {
                fp.add_str(template.exec_path.path_string())
                    .add_str(output.exec_path.path_string())
                    .add_bool(*is_executable);
                for (k, v) in substitutions {
                    fp.add_str(k).add_str(v);
                }
            }
            Self::BuildInfoTranslateAction {
                template,
                status_file,
                output,
                is_volatile,
                translator,
                ..
            } => {
                fp.add_str(template.exec_path.path_string())
                    .add_str(status_file.exec_path.path_string())
                    .add_str(output.exec_path.path_string())
                    .add_bool(*is_volatile)
                    .add_str(&translator.key);
            }
        }
        if let Self::SpawnAction {
            java_compile: Some(jc),
            ..
        } = self
        {
            // `JavaCompileAction.computeKey`: the classpath mode and the (ordered) full classpath.
            fp.add_str(&jc.classpath_mode)
                .add_strings(jc.transitive_inputs.iter());
        }
        fp.hex_digest_and_reset()
    }

    pub fn argv(&self) -> Vec<String> {
        match self {
            Self::SpawnAction { argv, .. } => argv.clone(),
            _ => Vec::new(),
        }
    }

    pub fn pretty_print(&self) -> String {
        match self {
            Self::SpawnAction {
                progress_message, ..
            } => progress_message.clone(),
            Self::FileWriteAction {
                output,
                is_executable,
                ..
            } => {
                // `AbstractFileWriteAction.getProgressMessage`
                format!(
                    "{} {}",
                    if *is_executable {
                        "Writing script"
                    } else {
                        "Writing file"
                    },
                    output.exec_path
                )
            }
            Self::SymlinkAction { output, .. } => {
                format!("Symlinking {}", output.exec_path)
            }
            Self::TemplateExpansionAction { output, .. } => {
                format!("Expanding template {}", output.exec_path)
            }
            Self::BuildInfoTranslateAction { is_volatile, .. } => {
                if *is_volatile {
                    "Translating volatile BuildInfo file".to_string()
                } else {
                    "Translating stable BuildInfo file".to_string()
                }
            }
        }
    }

    pub fn format_aquery_text(&self, config: &BuildConfigurationKey) -> String {
        let mut out = String::new();
        out.push_str(&format!("action '{}'\n", self.pretty_print()));
        out.push_str(&format!("  Mnemonic: {}\n", self.mnemonic()));
        out.push_str(&format!("  Target: {}\n", self.owner().canonical_form()));
        out.push_str(&format!(
            "  Configuration: {} ({})\n",
            config.mnemonic(),
            config.short_hash()
        ));
        out.push_str(&format!("  ActionKey: {}\n", self.action_key()));
        let inputs = self.inputs();
        if !inputs.is_empty() {
            let list: Vec<String> = inputs.iter().map(|a| a.exec_path.to_string()).collect();
            out.push_str(&format!("  Inputs: [{}]\n", list.join(", ")));
        }
        let outputs = self.outputs();
        if !outputs.is_empty() {
            let list: Vec<String> = outputs.iter().map(|a| a.exec_path.to_string()).collect();
            out.push_str(&format!("  Outputs: [{}]\n", list.join(", ")));
        }
        let argv = self.argv();
        if !argv.is_empty() {
            out.push_str(&format!("  Command Line: ({})\n", argv.join(" \\\n    ")));
        }
        out.push('\n');
        out
    }
}

/// Built-in `DefaultInfo` provider matching `DefaultInfo.java`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DefaultInfo {
    pub files: Vec<Artifact>,
    pub data_runfiles: Vec<Artifact>,
    pub default_runfiles: Vec<Artifact>,
    pub executable: Option<Artifact>,
}

/// Built-in `OutputGroupInfo` provider.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputGroupInfo {
    pub groups: BTreeMap<String, Vec<Artifact>>,
}

/// A configured target produced by evaluating a `Target` under a `BuildConfigurationKey`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredTarget {
    pub label: Label,
    pub configuration: BuildConfigurationKey,
    pub rule_class: String,
    pub default_info: DefaultInfo,
    pub output_groups: OutputGroupInfo,
    /// Flat actions for the current executor (see [`Action::lower`]).
    pub actions: Vec<ActionAnalysisMetadata>,
    pub is_test: bool,
    /// Typed actions (richer than `actions`; lowered into it).
    pub typed_actions: Vec<Action>,
    /// All providers by key: bare name for built-ins (`DefaultInfo`), `<bzl label>%<Name>`
    /// for Starlark providers. Values are `Value::ProviderInstance`.
    pub providers: BTreeMap<String, zel_starlark::Value>,
    pub runfiles_support: Option<RunfilesSupport>,
    /// `$collect_cc_coverage` (`_collect_cc_coverage`) executable of a test rule: becomes
    /// `CC_CODE_COVERAGE_SCRIPT` and a test input under coverage (`TestActionBuilder`).
    pub collect_cc_coverage: Option<Artifact>,
    pub testonly: bool,
    /// Effective visibility labels (package_group / `//pkg:__pkg__` style specs).
    pub visibility: Vec<Label>,
    /// `RunEnvironmentInfo` of an executable/test rule (environment of `bazel run` and of the
    /// test action).
    pub run_environment: Option<RunEnvironment>,
    /// `InstrumentedFilesInfo` (when the rule returned one): files to instrument and coverage
    /// metadata files.
    pub instrumented_files: Option<InstrumentedFiles>,
    /// Name of the aspect that decorated this configured target (e.g. `//pkg:defs.bzl%asp`).
    pub aspect_name: Option<String>,
    /// `AliasProvider`: chain of alias labels from head (outermost alias) to tail (alias
    /// pointing to the real non-alias target). Empty for non-alias targets.
    pub alias_chain: Vec<Label>,
    /// Real target's label at the end of an alias chain (`None` for non-alias targets).
    pub actual_label: Option<Label>,
    /// Real target's rule class at the end of an alias chain (`None` for non-alias targets).
    pub actual_rule_class: Option<String>,
    /// Real target's `testonly` flag when reached through an alias.
    pub actual_testonly: Option<bool>,
    /// `LateBoundAliasProvider`: true for late-bound alias rules (`label_flag`, `label_setting`,
    /// `cc_libc_top_alias`, etc.), even when they resolve to empty.
    pub is_late_bound_alias: bool,
    /// `VisibilityProvider.isCreatedInSymbolicMacro`: whether this target was created inside a
    /// symbolic macro.
    pub created_in_symbolic_macro: bool,
    /// Analysis-time rule warnings emitted while configuring this target.
    pub warnings: Vec<String>,
    /// For `alias` targets: the chain of aliased target labels ending at the final non-alias target.
    pub alias_targets: Vec<Label>,
    /// Non-`None` when this target is incompatible with the target platform (`target_compatible_with`).
    pub incompatible: Option<IncompatiblePlatform>,
}

/// Reason a configured target is incompatible with its target platform (`IncompatiblePlatformProvider`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncompatiblePlatform {
    pub target_platform: Label,
    pub constraints: Vec<Label>,
    /// Dependency chain from this target down to the first incompatible target (empty if directly incompatible).
    pub dependency_chain: Vec<Label>,
}

/// `RunEnvironmentInfo`: fixed variables plus names inherited from the client environment.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunEnvironment {
    pub environment: BTreeMap<String, String>,
    pub inherited_environment: Vec<String>,
}

/// `InstrumentedFilesInfo`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InstrumentedFiles {
    pub instrumented_files: Vec<Artifact>,
    pub metadata_files: Vec<Artifact>,
    /// `coverage_environment` of the rule and its dependencies (added to the test environment).
    pub environment: std::collections::BTreeMap<String, String>,
    /// `coverage_support_files` of the rule and its dependencies (extra test inputs).
    pub support_files: Vec<Artifact>,
}

impl ConfiguredTarget {
    /// A configured target with empty providers/actions.
    pub fn new(label: Label, configuration: BuildConfigurationKey, rule_class: String) -> Self {
        Self {
            label,
            configuration,
            rule_class,
            default_info: DefaultInfo::default(),
            output_groups: OutputGroupInfo::default(),
            actions: Vec::new(),
            is_test: false,
            typed_actions: Vec::new(),
            providers: BTreeMap::new(),
            runfiles_support: None,
            collect_cc_coverage: None,
            testonly: false,
            visibility: Vec::new(),
            run_environment: None,
            instrumented_files: None,
            aspect_name: None,
            alias_chain: Vec::new(),
            actual_label: None,
            actual_rule_class: None,
            actual_testonly: None,
            is_late_bound_alias: false,
            created_in_symbolic_macro: false,
            warnings: Vec::new(),
            alias_targets: Vec::new(),
            incompatible: None,
        }
    }

    /// `AliasProvider.getDependencyLabel`: head of the alias chain, or `self.label`.
    pub fn dependency_label(&self) -> &Label {
        self.alias_chain.first().unwrap_or(&self.label)
    }

    /// The underlying non-alias target's label (or `self.label` if not an alias).
    pub fn actual_target_label(&self) -> &Label {
        self.actual_label.as_ref().unwrap_or(&self.label)
    }

    /// The underlying non-alias target's rule class (or `self.rule_class` if not an alias).
    pub fn actual_rule_class_name(&self) -> &str {
        self.actual_rule_class
            .as_deref()
            .unwrap_or(&self.rule_class)
    }

    /// `AliasProvider.getDependencyLabels`: at most `[chain_head, actual_label]`.
    pub fn dependency_labels(&self) -> Vec<Label> {
        if let Some(head) = self.alias_chain.first() {
            vec![head.clone(), self.actual_target_label().clone()]
        } else {
            vec![self.label.clone()]
        }
    }

    /// `AliasProvider.isAlias`.
    pub fn is_alias(&self) -> bool {
        !self.alias_chain.is_empty()
    }

    /// `AliasProvider.describeTargetWithAliases`.
    pub fn describe_with_aliases(&self, with_kind: bool) -> String {
        let kind = if with_kind {
            format!("{} rule", self.actual_rule_class_name())
        } else {
            "target".to_string()
        };
        if self.alias_chain.is_empty() {
            return format!("{kind} '{}'", self.label.canonical_form());
        }
        let mut s = format!(
            "alias '{}' referring to {kind} '{}'",
            self.alias_chain[0].canonical_form(),
            self.actual_target_label().canonical_form()
        );
        if self.alias_chain.len() > 1 {
            let mid: Vec<String> = self.alias_chain[1..]
                .iter()
                .map(|l| l.canonical_form())
                .collect();
            s.push_str(&format!(" through '{}'", mid.join("' -> '")));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_and_action_key_stability() {
        let host_cpu = build_options::auto_host_cpu();
        let cfg = BuildConfigurationKey::target(host_cpu, "opt");
        assert_eq!(cfg.mnemonic(), format!("{host_cpu}-opt"));
        let exec_cfg = cfg.to_exec();
        assert_eq!(exec_cfg.mnemonic(), format!("{host_cpu}-opt-exec"));

        let mut no_platform_dir = cfg.clone();
        no_platform_dir.native.insert(
            "experimental_platform_in_output_dir".into(),
            NativeSetting {
                raw: "false".into(),
                value: build_options::ConfigValue::Scalar("NO".into()),
            },
        );
        assert_eq!(
            no_platform_dir.to_exec().config_display_mnemonic(),
            format!("{host_cpu}-opt-exec-ST-d57f47055a04")
        );

        let lbl = Label::parse_canonical("//src:app").unwrap();
        let out_art = Artifact::derived(
            &cfg.mnemonic(),
            PathFragment::create("src/app"),
            lbl.clone(),
        );
        let act1 = ActionAnalysisMetadata::FileWriteAction {
            owner: lbl.clone(),
            output: out_art.clone(),
            content: b"#!/bin/sh\necho hi\n".to_vec(),
            is_executable: true,
        };
        let act2 = act1.clone();
        assert_eq!(act1.action_key(), act2.action_key());
    }

    #[test]
    fn workspace_status_parser_and_utc_date_formatting_match_bazel() {
        assert_eq!(format_utc_build_date(0), "1970 Jan 1 00 00 00 Thu");
        assert_eq!(
            format_utc_build_date(1_700_000_000),
            "2023 Nov 14 22 13 20 Tue"
        );
        let mut map = BTreeMap::new();
        parse_workspace_status_file(
            b"BUILD_EMBED_LABEL rel 1.2.3\nEMPTY_KEY\n\nSTABLE_SPACE  leading_and_inner spaces \r\n",
            &mut map,
        )
        .unwrap();
        assert_eq!(map.get("BUILD_EMBED_LABEL").unwrap(), "rel 1.2.3");
        assert_eq!(map.get("EMPTY_KEY").unwrap(), "");
        assert_eq!(
            map.get("STABLE_SPACE").unwrap(),
            " leading_and_inner spaces "
        );
        let mut spaceless_repeat = BTreeMap::new();
        parse_workspace_status_file(b"FOO\nFOO\n", &mut spaceless_repeat).unwrap();
        assert_eq!(spaceless_repeat.get("FOO").map(String::as_str), Some(""));

        let mut val_then_spaceless = BTreeMap::new();
        parse_workspace_status_file(b"FOO val\nFOO\n", &mut val_then_spaceless).unwrap();
        assert_eq!(
            val_then_spaceless.get("FOO").map(String::as_str),
            Some("val")
        );

        let mut spaceless_then_val = BTreeMap::new();
        assert_eq!(
            parse_workspace_status_file(b"FOO\nFOO val\n", &mut spaceless_then_val),
            Err("FOO is duplicated in the file.".to_string())
        );

        let mut bad = BTreeMap::new();
        assert_eq!(
            parse_workspace_status_file(b"DUP_KEY a\nDUP_KEY b\n", &mut bad),
            Err("DUP_KEY is duplicated in the file.".to_string())
        );
    }

    #[test]
    fn exec_transition_treats_null_host_java_launcher_as_unset() {
        let host_cpu = build_options::auto_host_cpu();
        let baseline_exec = BuildConfigurationKey::target(host_cpu, "fastbuild").to_exec();

        let mut cfg = BuildConfigurationKey::target(host_cpu, "fastbuild");
        cfg.options
            .insert("java_launcher".into(), "@@//pkg:target_launcher".into());
        cfg.native.insert(
            "java_launcher".into(),
            NativeSetting {
                raw: "//pkg:target_launcher".into(),
                value: build_options::ConfigValue::Scalar("@@//pkg:target_launcher".into()),
            },
        );
        cfg.options
            .insert("host_java_launcher".into(), "null".into());
        cfg.native.insert(
            "host_java_launcher".into(),
            NativeSetting {
                raw: String::new(),
                value: build_options::ConfigValue::Null,
            },
        );

        let exec_cfg = cfg.to_exec();
        assert!(
            !exec_cfg.options.contains_key("java_launcher"),
            "exec options should omit unset java_launcher: {:?}",
            exec_cfg.options
        );
        assert_eq!(exec_cfg.option_str("java_launcher"), None);
        assert_eq!(
            fragments::late_bound_label("java", "launcher", &exec_cfg),
            None
        );
        assert_eq!(
            exec_cfg.build_options().get("java_launcher"),
            Some(&build_options::ConfigValue::Null)
        );
        assert_eq!(exec_cfg.checksum(), baseline_exec.checksum());

        cfg.options
            .insert("host_java_launcher".into(), "@@//pkg:host_launcher".into());
        let exec_with_host = cfg.to_exec();
        assert_eq!(
            exec_with_host.option_str("java_launcher").as_deref(),
            Some("@@//pkg:host_launcher")
        );
        assert_eq!(
            fragments::late_bound_label("java", "launcher", &exec_with_host),
            Some(Label::parse_canonical("@@//pkg:host_launcher").unwrap())
        );
    }
}
