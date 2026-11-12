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

//! `SpawnStrategyRegistry` / `SpawnStrategyResolver`: which strategy runs which spawn.
//!
//! Precedence for a spawn (`SpawnStrategyRegistry.getStrategies`):
//! 1. `--strategy_regexp` filters against the action's progress message (the last-specified
//!    matching filter wins, an identical filter given twice keeps its last identifiers),
//! 2. `--strategy=Mnemonic=...` / `--genrule_strategy` (the latter registered first, so
//!    `--strategy=Genrule=...` overrides it; last occurrence of a mnemonic wins),
//! 3. `--spawn_strategy` (default `[remote,] worker, sandboxed, local`).
//!
//! The resolver then asks each candidate whether it `canExec` the spawn and picks the first;
//! if none can, `SpawnStrategyResolver`'s error is produced.

use std::collections::BTreeMap;
use zel_bzlmod::download::Regex;
use zel_sandbox::{sandbox, Spawn};

/// A registered strategy implementation (what `toString()` prints in Bazel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StrategyKind {
    Standalone,
    ProcessWrapperSandbox,
    DarwinSandbox,
    LinuxSandbox,
    Worker,
    Remote,
    Dynamic,
}

impl StrategyKind {
    pub fn name(self) -> &'static str {
        match self {
            StrategyKind::Standalone => "standalone",
            StrategyKind::ProcessWrapperSandbox => "processwrapper-sandbox",
            StrategyKind::DarwinSandbox => "darwin-sandbox",
            StrategyKind::LinuxSandbox => "linux-sandbox",
            StrategyKind::Worker => "worker",
            StrategyKind::Remote => "remote",
            StrategyKind::Dynamic => "DynamicSpawnStrategy",
        }
    }

    pub fn is_sandboxed(self) -> bool {
        matches!(
            self,
            StrategyKind::ProcessWrapperSandbox | StrategyKind::DarwinSandbox | StrategyKind::LinuxSandbox
        )
    }

    /// `SpawnStrategy.canExec` for the strategies whose answer only depends on the spawn. The
    /// `remote` and `dynamic` strategies need registry state; use
    /// [`SpawnStrategyRegistry::can_exec`] for them.
    pub fn can_exec(self, spawn: &Spawn) -> bool {
        match self {
            StrategyKind::Standalone => true,
            k if k.is_sandboxed() => sandbox::may_be_sandboxed(spawn),
            StrategyKind::Worker => {
                zel_sandbox::worker::supports_workers(&spawn.execution_info)
                    || zel_sandbox::worker::supports_multiplex_workers(&spawn.execution_info)
            }
            // `RemoteSpawnRunner.canExec` needs to know whether an executor is configured and
            // `DynamicSpawnStrategy.canExec` the dynamic strategy lists: both are answered by
            // `SpawnStrategyRegistry::can_exec`.
            StrategyKind::Remote => zel_remote::spawn_runner::may_be_executed_remotely(spawn),
            StrategyKind::Dynamic => true,
            _ => false,
        }
    }
}

/// What this process can offer, determined at startup like `SandboxModule.setup`.
#[derive(Debug, Clone, Default)]
pub struct StrategyAvailability {
    pub process_wrapper_sandbox: bool,
    pub darwin_sandbox: bool,
    pub linux_sandbox: bool,
    pub remote: bool,
    /// A remote executor is configured (`RemoteModule.shouldEnableRemoteExecution`): the `remote`
    /// strategy can then execute spawns and joins the default strategy list.
    pub remote_exec: bool,
    /// `--internal_spawn_scheduler` / `--experimental_spawn_scheduler` (registers `dynamic`).
    pub dynamic: bool,
}

impl StrategyAvailability {
    /// Detects the sandbox support of this host.
    pub fn detect() -> Self {
        Self {
            process_wrapper_sandbox: sandbox::process_wrapper_supported(),
            darwin_sandbox: sandbox::darwin_sandbox_supported(),
            linux_sandbox: sandbox::linux_sandbox_supported(),
            // `RemoteModule` always registers `remote` (a placeholder that cannot execute
            // anything unless an executor is configured).
            remote: true,
            remote_exec: false,
            // `--internal_spawn_scheduler` defaults to true.
            dynamic: true,
        }
    }
}

/// Parsed strategy flags.
#[derive(Debug, Clone, Default)]
pub struct StrategyOptions {
    /// `--spawn_strategy` values, comma-split and concatenated in command-line order.
    pub spawn_strategy: Vec<String>,
    pub genrule_strategy: Vec<String>,
    /// `--strategy=Mnemonic=a,b` in command-line order.
    pub strategy: Vec<(String, Vec<String>)>,
    /// `--strategy_regexp=<filter>=a,b` in command-line order.
    pub strategy_regexp: Vec<(String, Vec<String>)>,
    /// `--dynamic_local_strategy=[mnemonic=]a,b` (empty mnemonic = fallback for all).
    pub dynamic_local: Vec<(String, Vec<String>)>,
    /// `--dynamic_remote_strategy=[mnemonic=]a,b`.
    pub dynamic_remote: Vec<(String, Vec<String>)>,
    /// `--experimental_local_lockfree_output`: adds `standalone` to the default dynamic local list.
    pub local_lockfree_output: bool,
    /// `--remote_local_fallback_strategy` (empty = `local`).
    pub remote_local_fallback_strategy: String,
}

fn split_commas(v: &str) -> Vec<String> {
    v.split(',').map(|s| s.to_string()).collect()
}

/// `Converters.StringToStringListConverter` (`[name=]v1[,v2...]`, empty names allowed).
pub fn parse_name_to_list(_flag: &str, v: &str) -> Result<(String, Vec<String>), String> {
    match v.split_once('=') {
        Some((k, val)) => Ok((k.to_string(), split_commas(val))),
        None => Ok((String::new(), split_commas(v))),
    }
}

impl StrategyOptions {
    /// Adds the dynamic-execution flags (`--dynamic_local_strategy`, `--dynamic_remote_strategy`).
    pub fn with_dynamic(mut self, local: &[String], remote: &[String]) -> Result<Self, String> {
        for v in local {
            self.dynamic_local.push(parse_name_to_list("dynamic_local_strategy", v)?);
        }
        for v in remote {
            self.dynamic_remote.push(parse_name_to_list("dynamic_remote_strategy", v)?);
        }
        Ok(self)
    }

    /// Builds the options from the raw flag values (`--spawn_strategy` etc.). Errors use the
    /// option converters' messages.
    pub fn from_raw(
        spawn_strategy: &[String],
        genrule_strategy: &[String],
        strategy: &[String],
        strategy_regexp: &[String],
    ) -> Result<Self, String> {
        let mut o = StrategyOptions::default();
        for v in spawn_strategy {
            o.spawn_strategy.extend(split_commas(v));
        }
        for v in genrule_strategy {
            o.genrule_strategy.extend(split_commas(v));
        }
        for v in strategy {
            o.strategy.push(parse_name_to_list("strategy", v)?);
        }
        for v in strategy_regexp {
            let (k, val) = v.split_once('=').ok_or_else(|| {
                format!("Invalid value for --strategy_regexp: '{v}': Variable definition must be in the form of a 'name=value' assignment")
            })?;
            if k.is_empty() {
                return Err(format!("Invalid value for --strategy_regexp: '{v}': Variable name must not be empty"));
            }
            o.strategy_regexp.push((k.to_string(), split_commas(val)));
        }
        Ok(o)
    }
}

/// `RegexFilter`: inclusion/exclusion regexes matched with `Matcher.find()`.
#[derive(Debug, Clone)]
pub struct RegexFilter {
    inclusions: Vec<(String, Regex)>,
    exclusions: Vec<(String, Regex)>,
    pub original: String,
    /// Canonical identity (sorted, deduped patterns) used for filter equality.
    identity: (Vec<String>, Vec<String>),
}

impl RegexFilter {
    /// `RegexFilterConverter.convert`.
    pub fn convert(input: &str) -> Result<Self, String> {
        if input.starts_with("--") {
            return Err(format!(
                "Failed to build filter: value looks like another flag ({input}). Either escape the value with \"\\-\\-\", or pass an explicit value to the flag."
            ));
        }
        let (mut inc, mut exc) = (Vec::new(), Vec::new());
        // Split on ',' but not on '\,'.
        let mut pieces = Vec::new();
        let mut cur = String::new();
        let chars: Vec<char> = input.chars().collect();
        for (i, &c) in chars.iter().enumerate() {
            if c == ',' && !(i > 0 && chars[i - 1] == '\\') {
                pieces.push(std::mem::take(&mut cur));
            } else {
                cur.push(c);
            }
        }
        pieces.push(cur);
        for p in pieces {
            let mut p = p.replace("\\,", ",").replace("\\@", "@");
            let excluded = p.starts_with('-');
            if excluded || p.starts_with('+') {
                p.remove(0);
            }
            if !p.is_empty() {
                (if excluded { &mut exc } else { &mut inc }).push(p);
            }
        }
        let compile = |v: Vec<String>| -> Result<Vec<(String, Regex)>, String> {
            let mut set: Vec<String> = v;
            set.sort();
            set.dedup();
            set.into_iter()
                .map(|p| {
                    Regex::new(&format!(".*(?:{p}).*"))
                        .map(|r| (p.clone(), r))
                        .map_err(|e| format!("Failed to build valid regular expression: {e}"))
                })
                .collect()
        };
        let inclusions = compile(inc)?;
        let exclusions = compile(exc)?;
        let identity = (
            inclusions.iter().map(|(p, _)| p.clone()).collect(),
            exclusions.iter().map(|(p, _)| p.clone()).collect(),
        );
        Ok(Self { inclusions, exclusions, original: input.to_string(), identity })
    }

    /// `isIncluded`: not matching any exclusion and matching some inclusion (if any).
    pub fn is_included(&self, value: &str) -> bool {
        if self.exclusions.iter().any(|(_, r)| r.full_match(value).is_some()) {
            return false;
        }
        if self.inclusions.is_empty() {
            return true;
        }
        self.inclusions.iter().any(|(_, r)| r.full_match(value).is_some())
    }
}

impl PartialEq for RegexFilter {
    fn eq(&self, o: &Self) -> bool {
        self.identity == o.identity
    }
}

/// `String.hashCode()`.
fn java_hash(s: &str) -> i32 {
    let mut h: i32 = 0;
    for u in s.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(u as i32);
    }
    h
}

/// Iteration order of a `java.util.HashMap<String, _>` filled with `keys` in order.
fn java_hashmap_order(keys: &[String]) -> Vec<String> {
    let mut cap = 16usize;
    let mut n = 0usize;
    let mut uniq: Vec<&String> = Vec::new();
    for k in keys {
        if !uniq.contains(&k) {
            uniq.push(k);
            n += 1;
            while n > cap * 3 / 4 {
                cap *= 2;
            }
        }
    }
    let mut v: Vec<(usize, usize, &String)> = uniq
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let h = java_hash(k) as u32;
            let h = h ^ (h >> 16);
            ((h as usize) & (cap - 1), i, *k)
        })
        .collect();
    v.sort();
    v.into_iter().map(|(_, _, k)| k.clone()).collect()
}

#[derive(Debug)]
pub struct SpawnStrategyRegistry {
    /// identifier -> implementation (later registrations win).
    identifier_to_kind: Vec<(String, StrategyKind)>,
    default_strategies: Vec<StrategyKind>,
    mnemonic_to_strategies: BTreeMap<String, Vec<StrategyKind>>,
    /// In priority order (last specified first), deduplicated by filter identity.
    regex_filters: Vec<(RegexFilter, Vec<StrategyKind>, Vec<String>)>,
    /// Local / remote branch strategies of dynamic execution per mnemonic (`""` = all others).
    dynamic_local: BTreeMap<String, Vec<StrategyKind>>,
    dynamic_remote: BTreeMap<String, Vec<StrategyKind>>,
    remote_local_fallback: StrategyKind,
    remote_exec: bool,
}

impl SpawnStrategyRegistry {
    /// The strategies registered by the modules, in `Bazel.BAZEL_MODULES` order: remote, worker,
    /// standalone (`standalone`, `local`), sandbox (`processwrapper-sandbox`, `linux-sandbox`,
    /// `darwin-sandbox`, each also `sandboxed`; later wins), dynamic (`dynamic`,
    /// `dynamic_worker`).
    fn registrations(av: &StrategyAvailability) -> Vec<(String, StrategyKind)> {
        let mut r: Vec<(String, StrategyKind)> = Vec::new();
        let mut reg = |k: StrategyKind, ids: &[&str]| {
            for i in ids {
                r.push((i.to_string(), k));
            }
        };
        if av.remote {
            reg(StrategyKind::Remote, &["remote"]);
        }
        reg(StrategyKind::Worker, &["worker"]);
        reg(StrategyKind::Standalone, &["standalone", "local"]);
        if av.process_wrapper_sandbox {
            reg(StrategyKind::ProcessWrapperSandbox, &["sandboxed", "processwrapper-sandbox"]);
        }
        if av.linux_sandbox {
            reg(StrategyKind::LinuxSandbox, &["sandboxed", "linux-sandbox"]);
        }
        if av.darwin_sandbox {
            reg(StrategyKind::DarwinSandbox, &["sandboxed", "darwin-sandbox"]);
        }
        if av.dynamic {
            reg(StrategyKind::Dynamic, &["dynamic", "dynamic_worker"]);
        }
        r
    }

    fn lookup(map: &[(String, StrategyKind)], id: &str) -> Option<StrategyKind> {
        map.iter().rev().find(|(i, _)| i == id).map(|(_, k)| *k)
    }

    fn to_strategies(
        map: &[(String, StrategyKind)],
        ids: &[String],
        request: &str,
    ) -> Result<Vec<StrategyKind>, String> {
        let mut out = Vec::new();
        for id in ids {
            if id.is_empty() {
                continue;
            }
            match Self::lookup(map, id) {
                Some(k) => out.push(k),
                None => {
                    let keys: Vec<String> = map.iter().map(|(i, _)| i.clone()).collect();
                    return Err(format!(
                        "'{id}' was requested for {request} but no strategy with that identifier was registered. Valid values are: [{}]",
                        java_hashmap_order(&keys).join(", ")
                    ));
                }
            }
        }
        Ok(out)
    }

    /// `BazelStrategyModule.registerSpawnStrategies` + `Builder.build()`.
    pub fn build(opts: &StrategyOptions, av: &StrategyAvailability) -> Result<Self, String> {
        let map = Self::registrations(av);
        // Default strategies.
        let mut spawn_strategies: Vec<String> = opts.spawn_strategy.iter().filter(|s| !s.is_empty()).cloned().collect();
        if spawn_strategies.is_empty() {
            if av.remote_exec {
                spawn_strategies.push("remote".into());
            }
            spawn_strategies.push("worker".into());
            spawn_strategies.push("sandboxed".into());
            spawn_strategies.push("local".into());
        }
        let mut default_strategies = Self::to_strategies(&map, &spawn_strategies, "explicit default strategies")?;

        // Mnemonic filters: Genrule first so `--strategy=Genrule=` overrides it; last wins.
        let mut mnemonic_ids: Vec<(String, Vec<String>)> = vec![("Genrule".to_string(), opts.genrule_strategy.clone())];
        mnemonic_ids.extend(opts.strategy.iter().cloned());
        let mut mnemonic_to_strategies: BTreeMap<String, Vec<StrategyKind>> = BTreeMap::new();
        let mut last: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (m, ids) in mnemonic_ids {
            last.insert(m, ids);
        }
        for (m, ids) in last {
            let ks = Self::to_strategies(&map, &ids, &format!("mnemonic {m}"))?;
            if !ks.is_empty() && !m.is_empty() {
                mnemonic_to_strategies.insert(m, ks);
            }
        }

        // Description filters: reversed, first of each distinct filter wins.
        let mut regex_filters: Vec<(RegexFilter, Vec<StrategyKind>, Vec<String>)> = Vec::new();
        for (f, ids) in opts.strategy_regexp.iter().rev() {
            let filter = RegexFilter::convert(f).map_err(|e| format!("Invalid value for --strategy_regexp: {e}"))?;
            if regex_filters.iter().any(|(g, _, _)| *g == filter) {
                continue;
            }
            let ks = Self::to_strategies(&map, ids, &format!("filter {}", filter.original))?;
            regex_filters.push((filter, ks, ids.clone()));
        }
        // Dynamic execution branches (`DynamicExecutionModule.registerSpawnStrategies`).
        let mut dynamic_local = BTreeMap::new();
        let mut dynamic_remote = BTreeMap::new();
        if av.dynamic {
            let throw_if_dynamic = |ids: &[String], flag: &str| -> Result<(), String> {
                if ids.iter().any(|i| i == "dynamic" || i == "dynamic_worker") {
                    return Err(format!(
                        "Cannot use strategy [dynamic, dynamic_worker] in flag {flag} as it would create a cycle during execution"
                    ));
                }
                Ok(())
            };
            let mut local_ids: BTreeMap<String, Vec<String>> = BTreeMap::new();
            let mut defaults = vec!["worker".to_string()];
            if map.iter().any(|(i, _)| i == "sandboxed") {
                defaults.push("sandboxed".into());
            }
            if opts.local_lockfree_output {
                defaults.push("standalone".into());
            }
            local_ids.insert(String::new(), defaults);
            for (m, ids) in &opts.dynamic_local {
                throw_if_dynamic(ids, "--dynamic_local_strategy")?;
                local_ids.insert(m.clone(), ids.clone());
            }
            for (m, ids) in local_ids {
                let ks = Self::to_strategies(&map, &ids, &format!("local mnemonic {m}"))?;
                dynamic_local.insert(m, ks);
            }
            let mut remote_ids: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for (m, ids) in &opts.dynamic_remote {
                throw_if_dynamic(ids, "--dynamic_remote_strategy")?;
                remote_ids.insert(m.clone(), ids.clone());
            }
            if opts.dynamic_remote.is_empty() {
                remote_ids.insert(String::new(), vec!["remote".into()]);
            }
            for (m, ids) in remote_ids {
                let ks = Self::to_strategies(&map, &ids, &format!("remote mnemonic {m}"))?;
                dynamic_remote.insert(m, ks);
            }
        }
        let fb_id = if opts.remote_local_fallback_strategy.is_empty() { "local" } else { opts.remote_local_fallback_strategy.as_str() };
        let remote_local_fallback = Self::lookup(&map, fb_id).ok_or_else(|| {
            let keys: Vec<String> = map.iter().map(|(i, _)| i.clone()).collect();
            format!(
                "'{fb_id}' was requested for remote fallback strategy but no strategy with that identifier was registered. Valid values are: [{}]",
                java_hashmap_order(&keys).join(", ")
            )
        })?;
        Ok(Self {
            identifier_to_kind: map,
            default_strategies,
            mnemonic_to_strategies,
            regex_filters,
            dynamic_local,
            dynamic_remote,
            remote_local_fallback,
            remote_exec: av.remote_exec,
        })
    }

    /// `SpawnStrategy.canExec` including the registry-dependent `remote` and `dynamic` ones.
    pub fn can_exec(&self, kind: StrategyKind, spawn: &Spawn) -> bool {
        match kind {
            StrategyKind::Remote => self.remote_exec && kind.can_exec(spawn),
            StrategyKind::Dynamic => self.dynamic_can_exec_local(spawn).is_some() || self.dynamic_can_exec_remote(spawn).is_some(),
            k => k.can_exec(spawn),
        }
    }

    fn dynamic_list<'a>(map: &'a BTreeMap<String, Vec<StrategyKind>>, mnemonic: &str) -> &'a [StrategyKind] {
        map.get(mnemonic).or_else(|| map.get("")).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// The first local-branch strategy able to run `spawn`, or `None` (`getLocalStrategy`).
    pub fn dynamic_can_exec_local(&self, spawn: &Spawn) -> Option<StrategyKind> {
        // `ExecutionPolicy.canRunLocally`: `Spawns.mayBeExecutedLocally`.
        if spawn.execution_info.contains_key("no-local") {
            return None;
        }
        Self::dynamic_list(&self.dynamic_local, &spawn.mnemonic).iter().copied().find(|k| self.can_exec(*k, spawn))
    }

    /// The first remote-branch strategy able to run `spawn`, or `None` (`getRemoteStrategy`).
    pub fn dynamic_can_exec_remote(&self, spawn: &Spawn) -> Option<StrategyKind> {
        // `ExecutionPolicy.canRunRemotely`: `Spawns.mayBeExecutedRemotely`.
        if !zel_remote::spawn_runner::may_be_executed_remotely(spawn) {
            return None;
        }
        Self::dynamic_list(&self.dynamic_remote, &spawn.mnemonic).iter().copied().find(|k| self.can_exec(*k, spawn))
    }

    /// The strategy `--remote_local_fallback_strategy` selects for remote execution fallbacks.
    pub fn remote_local_fallback(&self) -> StrategyKind {
        self.remote_local_fallback
    }

    /// `getStrategies(resourceOwner, mnemonic)`: also returns the progress line Bazel emits when
    /// a description filter matched (`"<description> with context [ids]"`).
    pub fn get_strategies(&self, mnemonic: &str, description: &str) -> (Vec<StrategyKind>, Option<String>) {
        for (f, ks, ids) in &self.regex_filters {
            if f.is_included(description) {
                let progress = format!("{description} with context [{}]", ids.join(", "));
                if !ks.is_empty() {
                    return (ks.clone(), Some(progress));
                }
                // Matched but resolved to no strategies: Bazel falls through (empty list).
                return self.after_regex(mnemonic, Some(progress));
            }
        }
        self.after_regex(mnemonic, None)
    }

    fn after_regex(&self, mnemonic: &str, progress: Option<String>) -> (Vec<StrategyKind>, Option<String>) {
        if let Some(ks) = self.mnemonic_to_strategies.get(mnemonic) {
            return (ks.clone(), progress);
        }
        (self.default_strategies.clone(), progress)
    }

    pub fn default_strategies(&self) -> &[StrategyKind] {
        &self.default_strategies
    }

    pub fn registered_identifiers(&self) -> Vec<String> {
        let keys: Vec<String> = self.identifier_to_kind.iter().map(|(i, _)| i.clone()).collect();
        java_hashmap_order(&keys)
    }

    /// `SpawnStrategyResolver.resolve`: the strategies able to execute `spawn`, in priority
    /// order, or Bazel's `NO_USABLE_STRATEGY_FOUND` message.
    pub fn resolve(&self, spawn: &Spawn, description: &str) -> Result<(Vec<StrategyKind>, Option<String>), String> {
        let (strategies, progress) = self.get_strategies(&spawn.mnemonic, description);
        let execable: Vec<StrategyKind> = strategies.iter().copied().filter(|s| self.can_exec(*s, spawn)).collect();
        if execable.is_empty() {
            return Err(format!(
                "{} spawn cannot be executed with any of the available strategies: [{}]. Your --spawn_strategy, --genrule_strategy, --strategy and/or --allowed_strategies_by_exec_platform flags are probably too strict. Visit https://github.com/bazelbuild/bazel/issues/7480 for advice.",
                spawn.mnemonic,
                strategies.iter().map(|s| s.name()).collect::<Vec<_>>().join(", ")
            ));
        }
        Ok((execable, progress))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn av() -> StrategyAvailability {
        StrategyAvailability { process_wrapper_sandbox: true, darwin_sandbox: true, ..Default::default() }
    }

    fn sp(mnemonic: &str) -> Spawn {
        Spawn { mnemonic: mnemonic.into(), ..Default::default() }
    }

    fn reg(opts: StrategyOptions) -> SpawnStrategyRegistry {
        SpawnStrategyRegistry::build(&opts, &av()).unwrap()
    }

    #[test]
    fn default_order_is_worker_sandboxed_local() {
        let r = reg(StrategyOptions::default());
        assert_eq!(
            r.default_strategies(),
            [StrategyKind::Worker, StrategyKind::DarwinSandbox, StrategyKind::Standalone]
        );
        // A plain spawn can't use the worker; it goes to the (darwin) sandbox.
        let (k, _) = r.resolve(&sp("CppCompile"), "Compiling a.cc").unwrap();
        assert_eq!(k, [StrategyKind::DarwinSandbox, StrategyKind::Standalone]);
        // A worker-capable spawn prefers the worker.
        let mut s = sp("Javac");
        s.execution_info.insert("supports-workers".into(), "1".into());
        assert_eq!(r.resolve(&s, "x").unwrap().0[0], StrategyKind::Worker);
        // `local` execution requirement skips the sandbox.
        let mut s = sp("Genrule");
        s.execution_info.insert("local".into(), "1".into());
        assert_eq!(r.resolve(&s, "x").unwrap().0, [StrategyKind::Standalone]);
    }

    #[test]
    fn sandboxed_alias_follows_registration_order_without_darwin() {
        let a = StrategyAvailability { process_wrapper_sandbox: true, ..Default::default() };
        let r = SpawnStrategyRegistry::build(&StrategyOptions::default(), &a).unwrap();
        assert_eq!(r.default_strategies()[1], StrategyKind::ProcessWrapperSandbox);
        let a = StrategyAvailability::default();
        let r = SpawnStrategyRegistry::build(
            &StrategyOptions { spawn_strategy: vec!["local".into()], ..Default::default() },
            &a,
        )
        .unwrap();
        assert_eq!(r.default_strategies(), [StrategyKind::Standalone]);
        let e = SpawnStrategyRegistry::build(
            &StrategyOptions { spawn_strategy: vec!["sandboxed".into()], ..Default::default() },
            &a,
        )
        .unwrap_err();
        assert!(e.starts_with("'sandboxed' was requested for explicit default strategies but no strategy with that identifier was registered. Valid values are: ["), "{e}");
    }

    #[test]
    fn mnemonic_beats_default_and_genrule_strategy_is_overridable() {
        let o = StrategyOptions::from_raw(
            &["sandboxed".into()],
            &["local".into()],
            &["Javac=worker,local".into(), "CppCompile=standalone".into(), "Javac=sandboxed".into()],
            &[],
        )
        .unwrap();
        let r = reg(o);
        assert_eq!(r.get_strategies("Genrule", "").0, [StrategyKind::Standalone]);
        assert_eq!(r.get_strategies("Javac", "").0, [StrategyKind::DarwinSandbox], "last wins");
        assert_eq!(r.get_strategies("CppCompile", "").0, [StrategyKind::Standalone]);
        assert_eq!(r.get_strategies("Other", "").0, [StrategyKind::DarwinSandbox]);
        // --strategy=Genrule=... overrides --genrule_strategy.
        let o = StrategyOptions::from_raw(&[], &["local".into()], &["Genrule=sandboxed".into()], &[]).unwrap();
        assert_eq!(reg(o).get_strategies("Genrule", "").0, [StrategyKind::DarwinSandbox]);
    }

    #[test]
    fn strategy_regexp_has_priority_and_last_wins() {
        let o = StrategyOptions::from_raw(
            &["sandboxed".into()],
            &[],
            &["Foo=standalone".into()],
            &["Compiling.*/bar=local".into(), "Compiling=sandboxed".into(), "//excl,-//excl/skip=local".into()],
        )
        .unwrap();
        let r = reg(o);
        let (k, progress) = r.get_strategies("Foo", "Compiling //foo/bar/baz");
        assert_eq!(k, [StrategyKind::DarwinSandbox], "later --strategy_regexp wins over earlier");
        assert_eq!(progress.unwrap(), "Compiling //foo/bar/baz with context [sandboxed]");
        assert_eq!(r.get_strategies("Foo", "Linking x").0, [StrategyKind::Standalone], "falls to mnemonic");
        assert_eq!(r.get_strategies("Foo", "Building //excl/pkg").0, [StrategyKind::Standalone]);
        assert_eq!(r.get_strategies("Bar", "Building //excl/pkg").0, [StrategyKind::Standalone]);
        assert_eq!(r.get_strategies("Bar", "Building //excl/skip").0, [StrategyKind::DarwinSandbox]);
        // Identical filter twice: last identifiers win.
        let o = StrategyOptions::from_raw(&[], &[], &[], &["x=local".into(), "x=sandboxed".into()]).unwrap();
        assert_eq!(reg(o).get_strategies("M", "x").0, [StrategyKind::DarwinSandbox]);
    }

    #[test]
    fn regex_filter_semantics() {
        let f = RegexFilter::convert("//foo.*\\.cc,-//foo/bar").unwrap();
        assert!(f.is_included("Compiling //foo/x.cc"));
        assert!(!f.is_included("Compiling //foo/bar/x.cc"));
        assert!(!f.is_included("other"));
        assert!(RegexFilter::convert("-only/exclude").unwrap().is_included("anything else"));
        assert!(RegexFilter::convert("a\\,b").unwrap().is_included("xa,by"));
        assert!(RegexFilter::convert("--x").unwrap_err().starts_with("Failed to build filter: value looks like another flag (--x)"));
        assert!(RegexFilter::convert("(").unwrap_err().starts_with("Failed to build valid regular expression"));
        assert!(RegexFilter::convert("^Comp").unwrap().is_included("Compiling"));
        assert!(!RegexFilter::convert("^Comp").unwrap().is_included("xCompiling"));
    }

    #[test]
    fn unknown_strategy_error_mentions_request_and_valid_values() {
        let e = SpawnStrategyRegistry::build(
            &StrategyOptions::from_raw(&[], &[], &["Javac=bogus".into()], &[]).unwrap(),
            &av(),
        )
        .unwrap_err();
        assert!(e.starts_with("'bogus' was requested for mnemonic Javac but no strategy with that identifier was registered. Valid values are: ["));
        for id in ["worker", "standalone", "local", "sandboxed", "processwrapper-sandbox", "darwin-sandbox"] {
            assert!(e.contains(id), "{e}");
        }
        assert!(!e.contains("remote"));
        let e = SpawnStrategyRegistry::build(
            &StrategyOptions::from_raw(&[], &[], &[], &["x=nope".into()]).unwrap(),
            &av(),
        )
        .unwrap_err();
        assert!(e.contains("was requested for filter x but"));
        let e = SpawnStrategyRegistry::build(&StrategyOptions::from_raw(&["remote".into()], &[], &[], &[]).unwrap(), &av()).unwrap_err();
        assert!(e.contains("'remote' was requested"));
        let parsed = StrategyOptions::from_raw(&[], &[], &["noequals".into()], &[]).unwrap();
        assert_eq!(parsed.strategy, vec![("".into(), vec!["noequals".into()])]);
    }

    #[test]
    fn no_usable_strategy_error_matches_resolver() {
        let r = reg(StrategyOptions { spawn_strategy: vec!["worker".into()], ..Default::default() });
        let e = r.resolve(&sp("Genrule"), "x").unwrap_err();
        assert_eq!(
            e,
            "Genrule spawn cannot be executed with any of the available strategies: [worker]. Your --spawn_strategy, --genrule_strategy, --strategy and/or --allowed_strategies_by_exec_platform flags are probably too strict. Visit https://github.com/bazelbuild/bazel/issues/7480 for advice."
        );
    }

    #[test]
    fn java_hashmap_order_emulation() {
        // Known Java iteration order for these keys (hash & 15): "local"=..., verified by formula.
        let keys: Vec<String> = ["worker", "standalone", "local", "sandboxed"].iter().map(|s| s.to_string()).collect();
        let o = java_hashmap_order(&keys);
        assert_eq!(o.len(), 4);
        assert_eq!(java_hash("a"), 97);
        assert_eq!(java_hash("local"), 103_145_323);
    }

    #[test]
    fn dynamic_registered_only_when_scheduler_enabled() {
        let mut a = av();
        let o = StrategyOptions { spawn_strategy: vec!["dynamic".into()], ..Default::default() };
        assert!(SpawnStrategyRegistry::build(&o, &a).is_err());
        a.dynamic = true;
        a.remote = true; // `remote` is always registered by RemoteModule (dynamic's default remote branch)
        let r = SpawnStrategyRegistry::build(&o, &a).unwrap();
        assert_eq!(r.default_strategies(), [StrategyKind::Dynamic]);
    }
}
