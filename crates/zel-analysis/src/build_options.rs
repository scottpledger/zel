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

//! Port of `BuildOptions` (`checksum()`) over the full typed option state.
//!
//! A `BuildOptions` is a sorted map `FragmentOptions class -> every option of that class` plus
//! the Starlark options map.  Its checksum (Bazel 9.x, `BuildOptions.checksum`) is
//!
//! ```text
//! fp = Fingerprint()
//! for options in fragmentOptionsMap.values():   // sorted by class name
//!     fp.addString(options.cacheKey())          // "<class>{name=VALUE, ...}"
//! fp.addString(mapToCacheKey(starlarkOptionsMap))
//! fp.addString(mapToCacheKey(scopes))
//! hex(sha256)                                   // or 64 zeros when empty
//! ```
//!
//! where `VALUE` is `EMPTY` for an empty `List`, `NULL` for `null`, else the Java `toString()` of
//! the value, escaped (`\` and `"`) and double-quoted.  The set of option classes and the
//! option schemas (`build_options_table.rs`) are generated from Bazel's sources by
//! `scripts/extract_config_options.py`; values are the Java `toString()` renderings of the
//! converted values, implemented here per Java type.

use std::collections::BTreeMap;

pub use crate::build_options_table::{CONFIG_FRAGMENTS, CONFIG_OPTION_CLASSES, EXEC_PROPAGATED};

/// One `@Option` of a `FragmentOptions` class (generated schema).
#[derive(Debug, Clone, Copy)]
pub struct ConfigOption {
    pub name: &'static str,
    pub java_type: &'static str,
    pub converter: &'static str,
    pub default: &'static str,
    pub multiple: bool,
}

/// The Java value of an option as far as `OptionsBase.cacheKey()` can observe it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConfigValue {
    Null,
    /// `java.util.List`; every element is its `toString()`.
    List(Vec<String>),
    /// Anything else, as its `toString()`.
    Scalar(String),
}

impl ConfigValue {
    /// `toString()` as printed by `bazel config` (`null`, `[a, b]`, ...).
    pub fn display(&self) -> String {
        match self {
            ConfigValue::Null => "null".to_string(),
            ConfigValue::List(v) => format!("[{}]", v.join(", ")),
            ConfigValue::Scalar(s) => s.clone(),
        }
    }

    /// `OptionsBase.mapToCacheKey` rendering of one value.
    fn cache_key(&self) -> String {
        match self {
            ConfigValue::List(v) if v.is_empty() => "EMPTY".to_string(),
            ConfigValue::Null => "NULL".to_string(),
            other => format!("\"{}\"", escape(&other.display())),
        }
    }
}

fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\\' || c == '"' {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

pub const CORE_OPTIONS: &str = "com.google.devtools.build.lib.analysis.config.CoreOptions";
pub const TEST_OPTIONS: &str = "com.google.devtools.build.lib.analysis.test.TestConfiguration$TestOptions";

/// `option name -> (class, schema)`.
pub fn find_option(name: &str) -> Option<(&'static str, &'static ConfigOption)> {
    for (cls, opts) in CONFIG_OPTION_CLASSES {
        if let Ok(i) = opts.binary_search_by(|o| o.name.cmp(name)) {
            return Some((cls, &opts[i]));
        }
    }
    None
}

/// Converts one raw option string like the option's Java converter, returning the `toString()` of
/// the converted element(s).  `canon_label` canonicalizes a label string (`//a:b` -> `@@//a:b`).
pub type LabelCanonicalizer<'a> = &'a dyn Fn(&str) -> Result<String, String>;

/// Default label canonicalization for labels without repo-mapping context (schema defaults).
pub fn canonical_label_default(raw: &str) -> Result<String, String> {
    Ok(if let Some(rest) = raw.strip_prefix("@@") {
        format!("@@{rest}")
    } else if let Some(rest) = raw.strip_prefix('@') {
        format!("@@{rest}")
    } else if raw.starts_with("//") {
        format!("@@{raw}")
    } else {
        raw.to_string()
    })
}

fn parse_bool(raw: &str) -> Result<bool, String> {
    match raw.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "t" | "y" => Ok(true),
        "0" | "false" | "no" | "f" | "n" => Ok(false),
        _ => Err(format!("invalid boolean '{raw}'")),
    }
}

/// `TriState.toString()` (enum name).
fn tri_state(raw: &str) -> String {
    match raw.to_ascii_lowercase().as_str() {
        "auto" | "-1" => "AUTO",
        "1" | "true" | "yes" | "t" | "y" => "YES",
        "0" | "false" | "no" | "f" | "n" => "NO",
        _ => "AUTO",
    }
    .to_string()
}

/// Java `Duration.toString()` for whole seconds (`PT1M`, `PT1H30M`, `PT30S`).
fn duration_string(secs: i64) -> String {
    if secs == 0 {
        return "PT0S".to_string();
    }
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    let mut o = String::from("PT");
    if h != 0 {
        o.push_str(&format!("{h}H"));
    }
    if m != 0 {
        o.push_str(&format!("{m}M"));
    }
    if s != 0 {
        o.push_str(&format!("{s}S"));
    }
    o
}

/// `RegexFilter.toString()` of `RegexFilterConverter.convert(input)`.
fn regex_filter_string(input: &str) -> String {
    let mut inc: Vec<String> = Vec::new();
    let mut exc: Vec<String> = Vec::new();
    // Split on ',' but not on '\,'.
    let mut pieces: Vec<String> = vec![String::new()];
    let chars: Vec<char> = input.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == ',' && !(i > 0 && chars[i - 1] == '\\') {
            pieces.push(String::new());
        } else {
            pieces.last_mut().unwrap().push(c);
        }
    }
    for p in pieces {
        let mut p = p.replace("\\,", ",");
        let excluded = p.starts_with('-');
        if excluded || p.starts_with('+') {
            p.remove(0);
        }
        if !p.is_empty() {
            if excluded { &mut exc } else { &mut inc }.push(p);
        }
    }
    let union = |list: &mut Vec<String>| -> Option<String> {
        if list.is_empty() {
            return None;
        }
        list.sort();
        list.dedup();
        Some(format!("(?:(?>{}))", list.join(")|(?>")))
    };
    let (i, e) = (union(&mut inc), union(&mut exc));
    let mut out = String::new();
    if let Some(i) = &i {
        out.push_str(&i.replace(',', "\\,"));
        if e.is_some() {
            out.push(',');
        }
    }
    if let Some(e) = &e {
        out.push('-');
        out.push_str(&e.replace(',', "\\,"));
    }
    out
}

fn split_unescaped_commas(s: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == ',' && !(i > 0 && chars[i - 1] == '\\') {
            out.push(String::new());
        } else {
            out.last_mut().unwrap().push(c);
        }
    }
    out
}

fn enum_string(java_type: &str, raw: &str) -> String {
    match java_type {
        // Enums with a lower-case `toString()`.
        "CompilationMode" | "StripMode" => raw.to_ascii_lowercase(),
        _ => raw.replace('-', "_").to_ascii_uppercase(),
    }
}

/// Java types whose option value is a plain `String` / pass-through.
fn is_plain_string_type(t: &str) -> bool {
    matches!(t, "String" | "PathFragment")
}

fn env_var_string(raw: &str) -> String {
    if let Some(n) = raw.strip_prefix('-') {
        format!("Unset[name={n}]")
    } else if let Some((n, v)) = raw.split_once('=') {
        format!("Set[name={n}, value={v}]")
    } else {
        format!("Inherit[name={raw}]")
    }
}

/// Converts the raw command-line values of `opt` (all occurrences, in order) into the Java value.
pub fn convert_raws(
    opt: &ConfigOption,
    raws: &[String],
    canon: LabelCanonicalizer<'_>,
    host_cpu: &str,
) -> Result<ConfigValue, String> {
    let t = opt.java_type;
    let conv = opt.converter;
    // Element-level conversion.
    let elem = |raw: &str| -> Result<String, String> {
        let inner = t
            .strip_prefix("List<")
            .or_else(|| t.strip_prefix("ImmutableList<"))
            .and_then(|s| s.strip_suffix('>'))
            .unwrap_or(t);
        match inner {
            "Label" => canon(raw),
            "String" => Ok(raw.to_string()),
            "Converters.EnvVar" => Ok(env_var_string(raw)),
            "Map.Entry<String,String>" => {
                let (k, v) = raw.split_once('=').ok_or_else(|| format!("Variable definitions must be in the form of a 'name=value' assignment: '{raw}'"))?;
                Ok(format!("{k}={v}"))
            }
            "Map.Entry<Label,String>" => {
                let (k, v) = raw.split_once('=').ok_or_else(|| format!("Expected 'label=value': '{raw}'"))?;
                Ok(format!("{}={v}", canon(k)?))
            }
            "CompilationMode" => Ok(enum_string("CompilationMode", raw)),
            "PerLabelOptions" => {
                if conv.ends_with("RunsPerTestConverter") {
                    // `N` or `regex@N`.
                    let (filter, n) = match raw.split_once('@') {
                        Some((f, n)) => (f.to_string(), n.to_string()),
                        None => (".*".to_string(), raw.to_string()),
                    };
                    return Ok(format!("{} Options: [{n}]", regex_filter_string(&filter)));
                }
                match raw.split_once('@') {
                    None => Ok(format!("{} Options: []", regex_filter_string(raw))),
                    Some((f, o)) => {
                        let opts: Vec<String> = split_unescaped_commas(o)
                            .into_iter()
                            .filter(|s| !s.trim().is_empty())
                            .map(|s| s.replace("\\,", ","))
                            .collect();
                        Ok(format!("{} Options: [{}]", regex_filter_string(f), opts.join(", ")))
                    }
                }
            }
            "ExecutionInfoModifier" => {
                if raw.is_empty() {
                    Ok("ExecutionInfoModifier{option=, expressions=[]}".to_string())
                } else {
                    let mut exprs = Vec::new();
                    for spec in raw.split(',') {
                        let Some((pat, rest)) = spec.rsplit_once('=') else {
                            return Err(format!("malformed expression '{spec}'"));
                        };
                        let (remove, key) = if let Some(k) = rest.strip_prefix('-') {
                            (true, k)
                        } else if let Some(k) = rest.strip_prefix('+') {
                            (false, k)
                        } else {
                            return Err(format!("malformed expression '{spec}'"));
                        };
                        exprs.push(format!("Expression{{regex={pat}, remove={remove}, key={key}}}"));
                    }
                    Ok(format!("ExecutionInfoModifier{{option={raw}, expressions=[{}]}}", exprs.join(", ")))
                }
            }
            other => Ok(other.to_string()),
        }
    };
    let is_list = t.starts_with("List<") || t.starts_with("ImmutableList<");
    if is_list {
        let comma = conv.contains("CommaSeparated") || conv.ends_with("LabelListConverter") || conv.ends_with("LabelOrderedSetConverter");
        let mut out: Vec<String> = Vec::new();
        if conv.ends_with("EmptyListConverter") {
            return Ok(ConfigValue::List(vec![]));
        }
        if conv.ends_with("FissionOptionConverter") {
            // List<CompilationMode> built from an ImmutableSet.
            let mut modes: Vec<String> = Vec::new();
            for raw in raws {
                if raw == "yes" {
                    modes.extend(["fastbuild", "dbg", "opt"].map(String::from));
                } else if raw != "no" {
                    for m in raw.split(',') {
                        modes.push(m.to_ascii_lowercase());
                    }
                }
            }
            let mut seen: Vec<String> = Vec::new();
            for m in modes {
                if !seen.contains(&m) {
                    seen.push(m);
                }
            }
            return Ok(ConfigValue::List(seen));
        }
        for raw in raws {
            if comma {
                for piece in raw.split(',').filter(|p| !p.is_empty()) {
                    out.push(elem(piece)?);
                }
            } else {
                out.push(elem(raw)?);
            }
        }
        // Non-multiple set/list options keep only the last occurrence's elements.
        return Ok(ConfigValue::List(out));
    }
    let raw = raws.last().map(String::as_str).unwrap_or("");
    Ok(match t {
        "boolean" | "Boolean" => ConfigValue::Scalar(parse_bool(raw)?.to_string()),
        "int" | "Integer" | "long" | "Long" => {
            let n: i64 = raw.trim().parse().map_err(|_| format!("'{raw}' is not an int"))?;
            ConfigValue::Scalar(n.to_string())
        }
        "double" | "Double" => {
            let d: f64 = raw.trim().parse().map_err(|_| format!("'{raw}' is not a double"))?;
            ConfigValue::Scalar(if d.fract() == 0.0 { format!("{d:.1}") } else { d.to_string() })
        }
        "Void" => ConfigValue::Null,
        "TriState" => ConfigValue::Scalar(tri_state(raw)),
        "Label" => {
            if raw.is_empty() && conv.contains("EmptyToNull") {
                ConfigValue::Null
            } else {
                ConfigValue::Scalar(canon(raw)?)
            }
        }
        "RegexFilter" => ConfigValue::Scalar(regex_filter_string(raw)),
        "PlatformMappingKey" => ConfigValue::Scalar(format!(
            "PlatformMappingKey{{path={}, wasExplicitlySetByUser={}}}",
            if raw.is_empty() { "platform_mappings" } else { raw },
            !raw.is_empty()
        )),
        "Map<TestTimeout,Duration>" => {
            let defaults = [("short", 60), ("moderate", 300), ("long", 900), ("eternal", 3600)];
            let secs: Vec<i64> = raw.split(',').filter_map(|p| p.trim().parse().ok()).collect();
            let vals: Vec<i64> = match secs.len() {
                1 if secs[0] > 0 => vec![secs[0]; 4],
                4 => secs,
                _ => defaults.iter().map(|d| d.1).collect(),
            };
            let parts: Vec<String> = defaults.iter().zip(vals).map(|((n, _), v)| format!("{n}={}", duration_string(v))).collect();
            ConfigValue::Scalar(format!("{{{}}}", parts.join(", ")))
        }
        "Map<String,Label>" => {
            // LabelMapConverter: `Key` or `Key=label`.
            let parts: Vec<String> = raw
                .split(',')
                .filter(|p| !p.is_empty())
                .map(|p| match p.split_once('=') {
                    Some((k, v)) => canon(v).map(|v| format!("{k}={v}")),
                    None => Ok(format!("{p}=null")),
                })
                .collect::<Result<_, _>>()?;
            ConfigValue::Scalar(format!("{{{}}}", parts.join(", ")))
        }
        "DottedVersion.Option" => {
            if raw.is_empty() {
                ConfigValue::Null
            } else {
                ConfigValue::Scalar(raw.to_string())
            }
        }
        "RunUnder" => {
            // `RunUnderConverter`: shell-tokenized; a first word starting with `//` or `@` is a label.
            let words: Vec<String> = raw.split_whitespace().map(str::to_string).collect();
            let rest = format!("[{}]", words.iter().skip(1).cloned().collect::<Vec<_>>().join(", "));
            let first = words.first().cloned().unwrap_or_default();
            if first.starts_with("//") || first.starts_with('@') {
                ConfigValue::Scalar(format!("LabelRunUnder[value={raw}, options={rest}, label={}]", canon(&first)?))
            } else {
                ConfigValue::Scalar(format!("CommandRunUnder[value={raw}, options={rest}, command={first}]"))
            }
        }
        t if is_plain_string_type(t) => {
            if conv.ends_with("AutoCpuConverter") && raw.is_empty() {
                ConfigValue::Scalar(host_cpu.to_string())
            } else if conv.ends_with("PlatformTypeConverter") {
                ConfigValue::Scalar(raw.to_ascii_lowercase())
            } else {
                ConfigValue::Scalar(raw.to_string())
            }
        }
        // Enums (`EnumConverter`): the constant's `toString()`.
        other => ConfigValue::Scalar(enum_string(other, raw)),
    })
}

/// `AutoCpuConverter` for the host (`HOST_CPU` on this machine).
pub fn auto_host_cpu() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "darwin_arm64",
        ("macos", _) => "darwin_x86_64",
        ("linux", "aarch64") => "aarch64",
        ("linux", "x86_64") => "k8",
        ("windows", _) => "x64_windows",
        (_, arch) => arch,
    }
}

/// The option's value when it is not set on the command line.
fn initial_value(opt: &ConfigOption, host_cpu: &str) -> ConfigValue {
    let is_list = opt.java_type.starts_with("List<") || opt.java_type.starts_with("ImmutableList<");
    if opt.default == "null" {
        return if is_list { ConfigValue::List(vec![]) } else { ConfigValue::Null };
    }
    if opt.java_type == "Void" {
        return ConfigValue::Null;
    }
    if opt.java_type == "RunUnder" {
        return ConfigValue::Null;
    }
    let raws: Vec<String> = if opt.default.is_empty() && is_list { vec![] } else { vec![opt.default.to_string()] };
    convert_raws(opt, &raws, &canonical_label_default, host_cpu).unwrap_or(ConfigValue::Null)
}

/// Full option state of a configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildOptions {
    /// class -> option name -> value (every option of every fragment class present).
    pub fragments: BTreeMap<&'static str, BTreeMap<&'static str, ConfigValue>>,
    /// Starlark build settings (`--//pkg:flag=v`), label string -> `toString()` of the value.
    pub starlark: BTreeMap<String, String>,
    /// Scope of Starlark flags (`BuildOptions.scopes`), label string -> `toString()` of the scope.
    pub scopes: BTreeMap<String, String>,
}

impl BuildOptions {
    /// All fragments with every option at its default; `cpu`/`host_cpu` follow the host.
    pub fn defaults(host_cpu: &str) -> Self {
        let mut fragments = BTreeMap::new();
        for (cls, opts) in CONFIG_OPTION_CLASSES {
            let mut m = BTreeMap::new();
            for o in opts.iter() {
                m.insert(o.name, initial_value(o, host_cpu));
            }
            fragments.insert(*cls, m);
        }
        let mut b = BuildOptions { fragments, starlark: BTreeMap::new(), scopes: BTreeMap::new() };
        // PlatformOptions: an empty `--platforms` means the host platform.
        let host = b.get("host_platform").map(|v| v.display()).unwrap_or_default();
        if b.get("platforms") == Some(&ConfigValue::List(vec![])) {
            b.set("platforms", ConfigValue::List(vec![host]));
        }
        b
    }

    pub fn get(&self, name: &str) -> Option<&ConfigValue> {
        let (cls, o) = find_option(name)?;
        self.fragments.get(cls)?.get(o.name)
    }

    /// Sets a native option; returns false for unknown options (not part of any fragment).
    pub fn set(&mut self, name: &str, value: ConfigValue) -> bool {
        match find_option(name) {
            Some((cls, o)) => {
                if let Some(m) = self.fragments.get_mut(cls) {
                    m.insert(o.name, value);
                }
                true
            }
            None => false,
        }
    }

    /// `FragmentOptions.getNormalized()` of `CoreOptions`, `PlatformOptions` and `TestOptions`
    /// (applied by `BuildOptions.Builder.build`).
    pub fn normalize(&mut self) {
        fn list(o: &BuildOptions, n: &str) -> Vec<String> {
            match o.get(n) {
                Some(ConfigValue::List(l)) => l.clone(),
                _ => vec![],
            }
        }
        // `normalizeEntries`: last value per key, first-occurrence order.
        fn normalize_entries(entries: Vec<String>) -> Vec<String> {
            let mut m: Vec<(String, String)> = Vec::new();
            for e in entries {
                let (k, v) = e.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())).unwrap_or((e.clone(), String::new()));
                match m.iter_mut().find(|(mk, _)| *mk == k) {
                    Some(slot) => slot.1 = v,
                    None => m.push((k, v)),
                }
            }
            m.into_iter().map(|(k, v)| format!("{k}={v}")).collect()
        }
        fn sort_entries(mut entries: Vec<String>) -> Vec<String> {
            entries.sort_by(|a, b| a.split('=').next().cmp(&b.split('=').next()));
            entries
        }
        // `normalizeEnvVars`: last entry per variable name, order preserved by first occurrence.
        fn normalize_env(entries: Vec<String>) -> Vec<String> {
            let name = |e: &str| -> String {
                let inner = e.split_once('[').map(|x| x.1).unwrap_or(e);
                inner.trim_start_matches("name=").split([',', ']']).next().unwrap_or("").to_string()
            };
            let mut out: Vec<(String, String)> = Vec::new();
            for e in entries {
                let n = name(&e);
                match out.iter_mut().find(|(k, _)| *k == n) {
                    Some(slot) => slot.1 = e,
                    None => out.push((n, e)),
                }
            }
            out.into_iter().map(|(_, e)| e).collect()
        }
        let allowed = {
            let mut v = list(self, "allowed_cpu_values");
            v.sort();
            v.dedup();
            v
        };
        let defines = sort_entries(normalize_entries(list(self, "define")));
        let features = {
            let mut enabled = std::collections::BTreeSet::new();
            let mut disabled = std::collections::BTreeSet::new();
            let mut state: std::collections::HashMap<String, bool> = Default::default();
            for f in list(self, "features") {
                if let Some(n) = f.strip_prefix('-') {
                    state.insert(n.to_string(), false);
                } else {
                    state.entry(f).or_insert(true);
                }
            }
            for (k, v) in state {
                if v { enabled.insert(k); } else { disabled.insert(k); }
            }
            enabled.into_iter().chain(disabled.into_iter().map(|d| format!("-{d}"))).collect::<Vec<_>>()
        };
        let action_env = normalize_env(list(self, "action_env"));
        let host_action_env = normalize_env(list(self, "host_action_env"));
        let flag_alias = sort_entries(normalize_entries(list(self, "flag_alias")));
        let test_env = normalize_env(list(self, "test_env"));
        // PlatformOptions: dedupeKeepingLast(extra_toolchains), only the first of `platforms`.
        let extra_toolchains = {
            let v = list(self, "extra_toolchains");
            let mut out: Vec<String> = Vec::new();
            for t in v.into_iter().rev() {
                if !out.contains(&t) {
                    out.push(t);
                }
            }
            out.reverse();
            out
        };
        let platforms = list(self, "platforms");
        if self.fragments.contains_key(CORE_OPTIONS) {
            self.set("allowed_cpu_values", ConfigValue::List(allowed));
            self.set("define", ConfigValue::List(defines));
            self.set("features", ConfigValue::List(features));
            self.set("action_env", ConfigValue::List(action_env));
            self.set("host_action_env", ConfigValue::List(host_action_env));
            self.set("flag_alias", ConfigValue::List(flag_alias));
        }
        if self.get("extra_toolchains").is_some() {
            self.set("extra_toolchains", ConfigValue::List(extra_toolchains));
            if platforms.len() > 1 {
                self.set("platforms", ConfigValue::List(vec![platforms[0].clone()]));
            }
        }
        if self.fragments.contains_key(TEST_OPTIONS) {
            self.set("test_env", ConfigValue::List(test_env));
        }
    }

    /// `NoConfigTransition` result: fresh CoreOptions defaults only (independent of `self`).
    pub fn core_only(&self) -> BuildOptions {
        BuildOptions::no_config(auto_host_cpu())
    }

    /// `TestTrimmingLogic.trim`: drops `TestOptions` and reduces `--run_under` to its label.
    pub fn trim_test_configuration(&self) -> BuildOptions {
        let mut out = self.clone();
        out.fragments.remove(TEST_OPTIONS);
        if let Some(core) = out.fragments.get_mut(CORE_OPTIONS) {
            if let Some(v) = core.get("run_under").cloned() {
                core.insert("run_under", trim_run_under(&v));
            }
        }
        out
    }

    /// The Starlark-implemented exec transition (`bazel_exec_transition` in
    /// `builtin_exec_platforms.bzl`) followed by `ExecutionTransitionFactory.ExecTransitionFinalizer`
    /// (test trimming happens afterwards, honouring `--trim_test_configuration`). `exec_platform` is the canonical execution platform label.
    pub fn exec(&self, exec_platform: &str) -> BuildOptions {
        // `FunctionTransitionUtil.maybeGetExecDefaults`: options the transition does not set take
        // their defaults; propagated ones (and defines, test options, Starlark flags) copy over.
        let src = self;
        let mut o = BuildOptions::defaults(auto_host_cpu());
        o.fragments.retain(|cls, _| src.fragments.contains_key(cls));
        for name in EXEC_PROPAGATED {
            if let Some(v) = src.get(name) {
                o.set(name, v.clone());
            }
        }
        if let Some(t) = src.fragments.get(TEST_OPTIONS) {
            o.fragments.insert(TEST_OPTIONS, t.clone());
        }
        if let Some(d) = src.get("define") {
            o.set("define", d.clone());
        }
        if src.get("incompatible_exclude_starlark_flags_from_exec_config").map(|v| v.display()) != Some("true".into()) {
            o.starlark = src.starlark.clone();
        }
        let get = |o: &BuildOptions, n: &str| o.get(n).cloned().unwrap_or(ConfigValue::Null);
        let orig = src;
        let list = |v: ConfigValue| match v {
            ConfigValue::List(l) => l,
            _ => vec![],
        };
        // _core_options
        let host_mode = get(&o, "host_compilation_mode");
        let host_cpu = get(&o, "host_cpu");
        let host_action_env = get(&o, "host_action_env");
        let host_features = get(&o, "host_features");
        o.set("compilation_mode", host_mode);
        o.set("is exec configuration", ConfigValue::Scalar("true".into()));
        o.set("cpu", host_cpu);
        o.set("stamp", ConfigValue::Scalar("false".into()));
        o.set("action_env", host_action_env);
        o.set("features", host_features);
        o.set("experimental_action_listener", ConfigValue::List(vec![]));
        // AndroidConfiguration.Options / AppleCommandLineOptions / ConfigFeatureFlagOptions
        o.set("Android configuration distinguisher", ConfigValue::Scalar("MAIN".into()));
        let host_min_os = get(&o, "host_macos_minimum_os");
        o.set("macos_minimum_os", host_min_os);
        o.set("apple_platform_type", ConfigValue::Scalar("macos".into()));
        o.set("enforce_transitive_configs_for_config_feature_flag", ConfigValue::Scalar("false".into()));
        // CppOptions
        let g0: Vec<String> = if cfg!(windows) { vec![] } else { vec!["-g0".to_string()] };
        let with_g0 = |v: ConfigValue| ConfigValue::List(list(v).into_iter().chain(g0.clone()).collect());
        let host_grte = get(&o, "host_grte_top");
        o.set("grte_top", host_grte);
        let (c, cx) = (get(&o, "host_copt"), get(&o, "host_cxxopt"));
        o.set("copt", with_g0(c));
        o.set("cxxopt", with_g0(cx));
        for (to, from) in [("conlyopt", "host_conlyopt"), ("per_file_copt", "host_per_file_copt"), ("linkopt", "host_linkopt")] {
            let v = get(&o, from);
            o.set(to, v);
        }
        o.set("strip", ConfigValue::Scalar("always".into()));
        // JavaOptions._java_options
        let host_jvmopt = get(&o, "host_jvmopt");
        o.set(
            "jvmopt",
            if list(host_jvmopt.clone()).is_empty() {
                ConfigValue::List(vec!["-XX:ErrorFile=/dev/stderr".to_string()])
            } else {
                host_jvmopt
            },
        );
        for (to, from) in [
            ("javacopt", "host_javacopt"),
            ("java_launcher", "host_java_launcher"),
            ("java_language_version", "tool_java_language_version"),
            ("java_runtime_version", "tool_java_runtime_version"),
        ] {
            let v = get(&o, from);
            o.set(to, v);
        }
        // ExecTransitionFinalizer: state copied from the source configuration.
        for name in [
            "flag_alias",
            "modify_execution_info",
            "experimental_override_platform_cpu_name",
            "incompatible_disable_select_on",
            "incompatible_target_cpu_from_platform",
        ] {
            if let Some(v) = orig.get(name) {
                o.set(name, v.clone());
            }
        }
        o.set("platforms", ConfigValue::List(vec![exec_platform.to_string()]));
        let suffix = if orig.get("experimental_exec_configuration_distinguisher").map(|v| v.display().to_ascii_uppercase()) == Some("LEGACY".to_string()) {
            let mut h: i32 = 0;
            for u in exec_platform.encode_utf16() {
                h = h.wrapping_mul(31).wrapping_add(u as i32);
            }
            format!("exec-{:X}", h as u32)
        } else {
            "exec".to_string()
        };
        o.set("platform_suffix", ConfigValue::Scalar(suffix));
        o
    }

    /// `NoConfigTransition`: only `CoreOptions` (at defaults) remains.
    pub fn no_config(host_cpu: &str) -> BuildOptions {
        let mut b = BuildOptions::defaults(host_cpu);
        b.fragments.retain(|cls, _| *cls == CORE_OPTIONS);
        if let Some(core) = b.fragments.get_mut(CORE_OPTIONS) {
            core.insert("flag_alias", ConfigValue::List(vec![]));
        }
        b
    }

    /// `FragmentOptions.cacheKey()` of one class.
    pub fn cache_key_of(cls: &str, opts: &BTreeMap<&'static str, ConfigValue>) -> String {
        let mut s = format!("{cls}{{");
        s.push_str(&map_to_cache_key(opts.iter().map(|(k, v)| ((*k).to_string(), v.cache_key()))));
        s.push('}');
        s
    }

    /// `BuildOptions.checksum()`.
    pub fn checksum(&self) -> String {
        if self.fragments.is_empty() && self.starlark.is_empty() {
            return "0".repeat(64);
        }
        let mut buf = Vec::new();
        for (cls, opts) in &self.fragments {
            add_string(&mut buf, &Self::cache_key_of(cls, opts));
        }
        // starlarkOptionsMap: value rendered as a scalar (`"<toString>"`).
        add_string(
            &mut buf,
            &map_to_cache_key(self.starlark.iter().map(|(k, v)| {
                // A list-typed flag renders as `[a, b]`; `mapToCacheKey` prints an empty list as `EMPTY`.
                let value = if v == "[]" { ConfigValue::List(vec![]) } else { ConfigValue::Scalar(v.clone()) };
                (k.clone(), value.cache_key())
            })),
        );
        // `scopes`: every Starlark flag that was set carries its `ScopeType`; flags that do not declare a
        // `scope` get the `DEFAULT` placeholder (verified against the 9.3.2 binary: ids only match with it).
        let mut scopes: BTreeMap<String, String> = self.scopes.clone();
        for k in self.starlark.keys() {
            let def_scope = if k.starts_with("@@rules_python+//python/config_settings:") {
                "UNIVERSAL"
            } else {
                "DEFAULT"
            };
            scopes.entry(k.clone()).or_insert_with(|| def_scope.to_string());
        }
        add_string(
            &mut buf,
            &map_to_cache_key(scopes.iter().map(|(k, v)| (k.clone(), ConfigValue::Scalar(v.clone()).cache_key()))),
        );
        zel_util::sha256::hex_digest(&buf)
    }
}

fn trim_run_under(v: &ConfigValue) -> ConfigValue {
    match v {
        ConfigValue::Scalar(s) if s.starts_with("LabelRunUnder[") => {
            // Keep only the label.
            match s.rfind("label=") {
                Some(i) => ConfigValue::Scalar(format!("LabelRunUnder[value=, options=[], {}", &s[i..])),
                None => ConfigValue::Null,
            }
        }
        _ => ConfigValue::Null,
    }
}

fn map_to_cache_key(entries: impl Iterator<Item = (String, String)>) -> String {
    let mut s = String::new();
    for (k, v) in entries {
        s.push_str(&k);
        s.push('=');
        s.push_str(&v);
        s.push_str(", ");
    }
    s
}

fn add_string(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let mut n = bytes.len() as u64;
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            buf.push(b);
            break;
        }
        buf.push(b | 0x80);
    }
    buf.extend_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_sorted_for_binary_search() {
        for (_, opts) in CONFIG_OPTION_CLASSES {
            assert!(opts.windows(2).all(|w| w[0].name < w[1].name));
        }
        assert!(find_option("compilation_mode").is_some());
        assert!(find_option("stamp").is_some());
    }

    #[test]
    fn empty_options_are_zeros() {
        let b = BuildOptions { fragments: BTreeMap::new(), starlark: BTreeMap::new(), scopes: BTreeMap::new() };
        assert_eq!(b.checksum(), "0".repeat(64));
    }

    #[test]
    fn regex_filter_matches_java() {
        assert_eq!(regex_filter_string("-.*"), "-(?:(?>.*))");
        assert_eq!(regex_filter_string(".*"), "(?:(?>.*))");
    }

    #[test]
    fn defaults_have_expected_values() {
        let b = BuildOptions::defaults("darwin_arm64");
        assert_eq!(b.get("cpu").unwrap().display(), "darwin_arm64");
        assert_eq!(b.get("compilation_mode").unwrap().display(), "fastbuild");
        assert_eq!(b.get("stamp").unwrap().display(), "false");
        assert_eq!(b.get("platform_suffix"), Some(&ConfigValue::Null));
        assert_eq!(b.get("platforms").unwrap().display(), "[@@bazel_tools//tools:host_platform]");
    }

    /// Oracle: `bazel config` (9.3.2) lists the no-config configuration as `c07488d3...` on a
    /// darwin_arm64 host; it must come out of the checksum port, not a table.
    #[test]
    fn noconfig_checksum_matches_bazel_9_3_2_on_darwin_arm64() {
        assert_eq!(
            BuildOptions::no_config("darwin_arm64").checksum(),
            "c07488d3cf81516db87b4f8d054ee06cf3041d6d67e9d321eff2fb7c216ff314"
        );
    }

    #[test]
    #[ignore]
    fn dump_defaults_for_diff() {
        let mut b = BuildOptions::defaults("darwin_arm64");
        b.set("flag_alias", ConfigValue::List(vec![
            "build_python_zip=@@rules_python+//python/config_settings:build_python_zip".into(),
            "experimental_python_import_all_repositories=@@rules_python+//python/config_settings:experimental_python_import_all_repositories".into(),
            "incompatible_default_to_explicit_init_py=@@rules_python+//python/config_settings:incompatible_default_to_explicit_init_py".into(),
            "python_path=@@rules_python+//python/config_settings:python_path".into(),
        ]));
        let mut out = String::new();
        for (cls, opts) in &b.fragments {
            out.push_str(&format!("FragmentOptions {cls} {{\n"));
            for (k, v) in opts {
                if *k == "define" { continue; }
                out.push_str(&format!("  {k}: {}\n", v.display()));
            }
            out.push_str("}\n");
        }
        std::fs::write("/tmp/w6/zel_default.txt", out).unwrap();
        eprintln!("CHECKSUM {}", b.checksum());
        eprintln!("TRIMMED {}", b.trim_test_configuration().checksum());
    }

    #[test]
    fn exec_keeps_all_fragments() {
        let k = crate::BuildConfigurationKey::target("darwin_arm64", "opt");
        let e = k.to_exec();
        assert!(e.build_options().fragments.len() > 10);
        let ee = e.to_exec();
        assert!(ee.build_options().fragments.len() > 10);
        assert!(k.build_options().fragments.len() > 10);
    }

    #[test]
    fn trimming_drops_test_fragment_only() {
        let b = BuildOptions::defaults("darwin_arm64");
        eprintln!("CORE_ONLY CHECKSUM: {}", b.core_only().checksum());
        let t = b.trim_test_configuration();
        assert_eq!(t.fragments.len() + 1, b.fragments.len());
        assert_ne!(t.checksum(), b.checksum());
    }
}
