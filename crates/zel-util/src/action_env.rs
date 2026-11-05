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

//! Bazel's environment semantics: `util.EnvVar`, `actions.ActionEnvironment`, the shell action
//! environment of `BazelRuleClassProvider.SHELL_ACTION_ENV`, the test environment of
//! `BuildConfigurationValue.setupTestEnvironment` + `TestPolicy`, the repository environment of
//! `CommandEnvironment` and `SpawnAction.createActionEnvironment`.
//!
//! Variables inherited from the client environment are *not* baked in: they are looked up in the
//! per-command client environment (`--client_env`) when the action executes, and the values that
//! were used take part in the action cache key (`ActionCacheChecker.computeEffectiveEnvironment`).

use crate::Fingerprint;
use std::collections::{BTreeMap, BTreeSet};

/// `EnvVar`: a request to set, inherit or unset an environment variable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EnvVar {
    /// `NAME=value`.
    Set(String, String),
    /// `NAME`: inherit the value from the client environment.
    Inherit(String),
    /// `=NAME`: unset the variable.
    Unset(String),
}

impl EnvVar {
    pub fn name(&self) -> &str {
        match self {
            EnvVar::Set(n, _) | EnvVar::Inherit(n) | EnvVar::Unset(n) => n,
        }
    }

    /// `EnvVar.Converter.convert`.
    pub fn parse(input: &str) -> Result<EnvVar, String> {
        if input.is_empty() || input == "=" {
            return Err("Variable definitions must be in the form of a 'name=value', 'name', or '=name' assignment".to_string());
        }
        match input.find('=') {
            Some(0) => Ok(EnvVar::Unset(input[1..].to_string())),
            None => Ok(EnvVar::Inherit(input.to_string())),
            Some(pos) => Ok(EnvVar::Set(input[..pos].to_string(), input[pos + 1..].to_string())),
        }
    }

    /// `EnvVar.Converter.reverseForStarlark`.
    pub fn render(&self) -> String {
        match self {
            EnvVar::Set(n, v) => format!("{n}={v}"),
            EnvVar::Inherit(n) => n.clone(),
            EnvVar::Unset(n) => format!("={n}"),
        }
    }
}

/// Parses every `--action_env`-style value; entries that do not convert are skipped (the option
/// parser reports them with Bazel's converter error before we get here).
pub fn parse_env_vars<I, S>(raw: I) -> Vec<EnvVar>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    raw.into_iter().filter_map(|s| EnvVar::parse(s.as_ref()).ok()).collect()
}

/// Fixed values, variables inherited from the client environment and explicitly unset variables.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActionEnvironment {
    fixed: BTreeMap<String, String>,
    inherited: BTreeSet<String>,
    unset: BTreeSet<String>,
}

impl ActionEnvironment {
    pub const EMPTY: ActionEnvironment = ActionEnvironment {
        fixed: BTreeMap::new(),
        inherited: BTreeSet::new(),
        unset: BTreeSet::new(),
    };

    /// `ActionEnvironment.create(fixed, inherited, unset)`.
    pub fn create(
        fixed: BTreeMap<String, String>,
        inherited: BTreeSet<String>,
        unset: BTreeSet<String>,
    ) -> Self {
        Self { fixed, inherited, unset }
    }

    /// `ActionEnvironment.create(fixed)`.
    pub fn from_fixed(fixed: BTreeMap<String, String>) -> Self {
        Self::create(fixed, BTreeSet::new(), BTreeSet::new())
    }

    /// `ActionEnvironment.split(env, unsetEnv)`: `None` values are inherited.
    pub fn split(env: &BTreeMap<String, Option<String>>, unset: &BTreeSet<String>) -> Self {
        let mut fixed = BTreeMap::new();
        let mut inherited = BTreeSet::new();
        for (k, v) in env {
            match v {
                Some(v) => {
                    fixed.insert(k.clone(), v.clone());
                }
                None => {
                    inherited.insert(k.clone());
                }
            }
        }
        Self::create(fixed, inherited, unset.clone())
    }

    pub fn fixed_env(&self) -> &BTreeMap<String, String> {
        &self.fixed
    }

    pub fn inherited_env(&self) -> &BTreeSet<String> {
        &self.inherited
    }

    pub fn unset_env(&self) -> &BTreeSet<String> {
        &self.unset
    }

    pub fn is_empty(&self) -> bool {
        self.fixed.is_empty() && self.inherited.is_empty() && self.unset.is_empty()
    }

    /// `ActionEnvironment.resolve`: fixed values, then values inherited from `client_env` (these
    /// win over fixed ones), then removal of the unset variables.
    pub fn resolve(&self, result: &mut BTreeMap<String, String>, client_env: &BTreeMap<String, String>) {
        for (k, v) in &self.fixed {
            result.insert(k.clone(), v.clone());
        }
        for var in &self.inherited {
            if let Some(v) = client_env.get(var) {
                result.insert(var.clone(), v.clone());
            }
        }
        for u in &self.unset {
            result.remove(u);
        }
    }

    /// The complete environment as a fresh map.
    pub fn resolved(&self, client_env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        self.resolve(&mut m, client_env);
        m
    }

    /// `ActionCacheChecker.computeEffectiveEnvironment`: the inherited variables that exist in
    /// the client environment, with their values.
    pub fn used_client_env(&self, client_env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        self.inherited
            .iter()
            .filter_map(|k| client_env.get(k).map(|v| (k.clone(), v.clone())))
            .collect()
    }

    /// `ActionEnvironment.addTo(Fingerprint)`.
    pub fn add_to(&self, fp: &mut Fingerprint) {
        // `Fingerprint.addStringMap`: size followed by the entries in key order.
        fp.add_i32(self.fixed.len() as i32);
        for (k, v) in &self.fixed {
            fp.add_str(k).add_str(v);
        }
        fp.add_strings(self.inherited.iter());
        fp.add_strings(self.unset.iter());
    }

    /// `ActionEnvironment.withAdditionalFixedVariables`: the given values overwrite existing
    /// fixed ones and cancel an explicit unset of the same name. Inherited variables are kept as
    /// they are (an inherited value still wins in [`resolve`](Self::resolve)).
    pub fn with_additional_fixed_variables(&self, vars: &BTreeMap<String, String>) -> Self {
        if vars.is_empty() {
            return self.clone();
        }
        let mut fixed = self.fixed.clone();
        for (k, v) in vars {
            fixed.insert(k.clone(), v.clone());
        }
        let unset = self.unset.iter().filter(|u| !vars.contains_key(*u)).cloned().collect();
        Self::create(fixed, self.inherited.clone(), unset)
    }
}

/// `SpawnAction.createActionEnvironment`: the environment of an action given the configuration's
/// action environment, `use_default_shell_env` and the action's own `env`.
pub fn create_spawn_action_environment(
    configuration: &ActionEnvironment,
    use_default_shell_env: bool,
    environment: &BTreeMap<String, String>,
) -> ActionEnvironment {
    if use_default_shell_env && !environment.is_empty() {
        // Inherited variables override fixed ones in `resolve`; the action's fixed variables must
        // override the inherited part of the user-provided environment, so filter it explicitly.
        let filtered: BTreeSet<String> = configuration
            .inherited
            .iter()
            .filter(|n| !environment.contains_key(*n))
            .cloned()
            .collect();
        ActionEnvironment::create(configuration.fixed.clone(), filtered, configuration.unset.clone())
            .with_additional_fixed_variables(environment)
    } else if use_default_shell_env {
        configuration.clone()
    } else {
        ActionEnvironment::from_fixed(environment.clone())
    }
}

/// `BazelRuleClassProvider.pathOrDefault` for non-Windows hosts (Bazel 9.3.2).
pub fn default_path_for_os(_os: &str) -> &'static str {
    // 9.3.2 includes /usr/local/bin on every non-Windows OS (python3 on macOS lives there, #8536).
    "/bin:/usr/bin:/usr/local/bin"
}

/// The default `PATH` of strict actions on this host.
pub fn default_path() -> &'static str {
    default_path_for_os(std::env::consts::OS)
}

/// The command-line derived inputs of every environment (`CoreOptions.action_env`,
/// `host_action_env`, `TestOptions.test_env`, `StrictActionEnvOptions`, ...). Stored in the
/// build configuration key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EnvConfig {
    /// `--incompatible_strict_action_env` (default true).
    pub strict_action_env: bool,
    /// `--action_env`, command-line order.
    pub action_env: Vec<EnvVar>,
    /// `--host_action_env`, command-line order (replaces `action_env` in exec configurations).
    pub host_action_env: Vec<EnvVar>,
    /// `--test_env`, command-line order.
    pub test_env: Vec<EnvVar>,
    /// `--enable_runfiles` resolved for this host.
    pub runfiles_enabled: bool,
}

impl Default for EnvConfig {
    fn default() -> Self {
        Self {
            strict_action_env: true,
            action_env: Vec::new(),
            host_action_env: Vec::new(),
            test_env: Vec::new(),
            runfiles_enabled: true,
        }
    }
}

impl EnvConfig {
    /// `BazelRuleClassProvider.SHELL_ACTION_ENV` evaluated for the target (`is_exec == false`)
    /// or exec configuration (whose `action_env` is `--host_action_env`,
    /// `builtin_exec_platforms.bzl`).
    pub fn action_environment(&self, is_exec: bool) -> ActionEnvironment {
        self.action_environment_for_os(is_exec, std::env::consts::OS)
    }

    pub fn action_environment_for_os(&self, is_exec: bool, os: &str) -> ActionEnvironment {
        let mut env: BTreeMap<String, Option<String>> = BTreeMap::new();
        // `None` entries inherit the client environment, which is only known at execution time.
        if !self.strict_action_env {
            env.insert("LD_LIBRARY_PATH".into(), None);
        }
        if self.strict_action_env {
            env.insert("PATH".into(), Some(default_path_for_os(os).to_string()));
        } else {
            env.insert("PATH".into(), None);
        }
        // Options take precedence over the defaults; the latest occurrence of a name wins.
        let mut unset = BTreeSet::new();
        let vars = if is_exec { &self.host_action_env } else { &self.action_env };
        for v in vars {
            match v {
                EnvVar::Set(n, val) => {
                    env.insert(n.clone(), Some(val.clone()));
                    unset.remove(n);
                }
                EnvVar::Inherit(n) => {
                    env.insert(n.clone(), None);
                    unset.remove(n);
                }
                EnvVar::Unset(n) => {
                    env.remove(n);
                    unset.insert(n.clone());
                }
            }
        }
        if !self.runfiles_enabled {
            env.insert("RUNFILES_MANIFEST_ONLY".into(), Some("1".into()));
        }
        ActionEnvironment::split(&env, &unset)
    }

    /// `BuildConfigurationValue.setupTestEnvironment`.
    pub fn test_environment(&self) -> ActionEnvironment {
        let mut env: BTreeMap<String, Option<String>> = BTreeMap::new();
        // Variables unset via `--test_env==NAME` are also removed from the default test
        // environment (e.g. HOME).
        let mut unset = BTreeSet::new();
        for v in &self.test_env {
            match v {
                EnvVar::Set(n, val) => {
                    env.insert(n.clone(), Some(val.clone()));
                    unset.remove(n);
                }
                EnvVar::Inherit(n) => {
                    env.insert(n.clone(), None);
                    unset.remove(n);
                }
                EnvVar::Unset(n) => {
                    env.remove(n);
                    unset.insert(n.clone());
                }
            }
        }
        ActionEnvironment::split(&env, &unset)
    }
}

/// `StandaloneTestStrategy.ENV_VARS` as resolved by `TestPolicy.computeTestEnvironment` (first
/// layer of the test environment; `--action_env`, `--test_env` and rule environments override
/// and may unset it).
pub fn test_policy_environment(runfiles_dir: &str, tmp_dir: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("TZ".to_string(), "UTC".to_string());
    m.insert("TEST_SRCDIR".to_string(), runfiles_dir.to_string());
    m.insert("JAVA_RUNFILES".to_string(), runfiles_dir.to_string());
    m.insert("PYTHON_RUNFILES".to_string(), runfiles_dir.to_string());
    m.insert("RUNFILES_DIR".to_string(), runfiles_dir.to_string());
    m.insert("TEST_TMPDIR".to_string(), tmp_dir.to_string());
    // The first layer, so users can override via `--test_env=HOME=...` / `HOME` or unset it.
    m.insert("HOME".to_string(), tmp_dir.to_string());
    m.insert("RUN_UNDER_RUNFILES".to_string(), "1".to_string());
    m
}

/// `ALWAYS_INHERITED_REPO_ENV` on this host.
pub fn always_inherited_repo_env() -> &'static [&'static str] {
    if cfg!(windows) {
        &["PATH", "PATHEXT"]
    } else {
        &["PATH"]
    }
}

/// `CommandEnvironment`'s repository environments: `(repo_env, nonstrict_repo_env)`.
///
/// `repo_env` starts from the client environment (only `PATH` with
/// `--experimental_strict_repo_env`), `nonstrict_repo_env` from the full client environment; then
/// `--repo_env` is applied to both in order: `NAME=value` sets (with `%bazel_workspace%`
/// replaced), `NAME` inherits when present in the client environment, `=NAME` removes.
pub fn repository_environments(
    client_env: &BTreeMap<String, String>,
    repo_env: &[EnvVar],
    strict_repo_env: bool,
    workspace: Option<&str>,
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let mut strict: BTreeMap<String, String> = if strict_repo_env {
        client_env
            .iter()
            .filter(|(k, _)| always_inherited_repo_env().contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    } else {
        client_env.clone()
    };
    let mut nonstrict = client_env.clone();
    for v in repo_env {
        match v {
            EnvVar::Set(n, val) => {
                let val = match workspace {
                    Some(w) => val.replace("%bazel_workspace%", w),
                    None => val.clone(),
                };
                strict.insert(n.clone(), val.clone());
                nonstrict.insert(n.clone(), val);
            }
            EnvVar::Inherit(n) => {
                if let Some(val) = client_env.get(n) {
                    strict.insert(n.clone(), val.clone());
                    nonstrict.insert(n.clone(), val.clone());
                }
            }
            EnvVar::Unset(n) => {
                strict.remove(n);
                nonstrict.remove(n);
            }
        }
    }
    (strict, nonstrict)
}

/// `RunCommand.ENV_VARIABLES_TO_CLEAR_UNCONDITIONALLY`.
pub const RUN_ENV_VARIABLES_TO_CLEAR_UNCONDITIONALLY: &[&str] =
    &["JAVA_RUNFILES", "RUNFILES_DIR", "RUNFILES_MANIFEST_FILE", "RUNFILES_MANIFEST_ONLY", "TEST_SRCDIR"];

/// `RunCommand.getCommandLineInfo` for a non-test target: the environment of `bazel run` and the
/// variables to clear in the client's own environment.
///
/// `base` is the common run environment (`BUILD_WORKSPACE_DIRECTORY`, ...). `--run_env` is applied
/// first (`NAME=value` sets, `NAME` inherits from the client environment or, when absent there,
/// removes, `=NAME` removes and adds to the clear list), then the target's environment
/// (`RunEnvironmentInfo` / `env` + `env_inherit`) overrides it. The final process environment is
/// the client environment (the client execs the target in its own environment) with these
/// variables set and the clear list removed.
pub fn run_environment(
    base: BTreeMap<String, String>,
    run_env: &[EnvVar],
    target_env: &ActionEnvironment,
    client_env: &BTreeMap<String, String>,
) -> (BTreeMap<String, String>, BTreeSet<String>) {
    let mut env = base;
    let mut clear: BTreeSet<String> = BTreeSet::new();
    for v in run_env {
        match v {
            EnvVar::Set(n, val) => {
                env.insert(n.clone(), val.clone());
                clear.remove(n);
            }
            EnvVar::Inherit(n) => {
                match client_env.get(n) {
                    Some(val) => {
                        env.insert(n.clone(), val.clone());
                    }
                    None => {
                        env.remove(n);
                    }
                }
                clear.remove(n);
            }
            EnvVar::Unset(n) => {
                env.remove(n);
                clear.insert(n.clone());
            }
        }
    }
    target_env.resolve(&mut env, client_env);
    clear.extend(RUN_ENV_VARIABLES_TO_CLEAR_UNCONDITIONALLY.iter().map(|s| s.to_string()));
    (env, clear)
}

/// `--client_env=NAME=VALUE` arguments (as the C++ client sends them) to a map; later occurrences
/// win. Returns `None` when the argument is not a `--client_env` flag.
pub fn parse_client_env_arg(arg: &str) -> Option<(String, String)> {
    let rest = arg.strip_prefix("--client_env=")?;
    // `AssignmentConverter`: `name=value` (the value may be empty or contain '=').
    match rest.split_once('=') {
        Some((n, v)) if !n.is_empty() => Some((n.to_string(), v.to_string())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn cfg(strict: bool, action_env: &[&str]) -> EnvConfig {
        EnvConfig { strict_action_env: strict, action_env: parse_env_vars(action_env), ..Default::default() }
    }

    #[test]
    fn converter_forms() {
        assert_eq!(EnvVar::parse("A=b=c"), Ok(EnvVar::Set("A".into(), "b=c".into())));
        assert_eq!(EnvVar::parse("A="), Ok(EnvVar::Set("A".into(), String::new())));
        assert_eq!(EnvVar::parse("A"), Ok(EnvVar::Inherit("A".into())));
        assert_eq!(EnvVar::parse("=A"), Ok(EnvVar::Unset("A".into())));
        assert!(EnvVar::parse("").is_err());
        assert!(EnvVar::parse("=").is_err());
        assert_eq!(EnvVar::parse("=A").unwrap().render(), "=A");
    }

    #[test]
    fn strict_default_environment() {
        let e = cfg(true, &[]).action_environment_for_os(false, "linux");
        assert_eq!(e.fixed_env(), &m(&[("PATH", "/bin:/usr/bin:/usr/local/bin")]));
        assert!(e.inherited_env().is_empty());
        // The client's PATH is irrelevant under strict env.
        let client = m(&[("PATH", "/opt/x"), ("LD_LIBRARY_PATH", "/lib"), ("FOO", "1")]);
        assert_eq!(e.resolved(&client), m(&[("PATH", "/bin:/usr/bin:/usr/local/bin")]));
    }

    #[test]
    fn default_path_includes_usr_local_on_every_os() {
        for os in ["linux", "macos", "freebsd"] {
            let e = cfg(true, &[]).action_environment_for_os(false, os);
            assert_eq!(e.fixed_env()["PATH"], "/bin:/usr/bin:/usr/local/bin");
        }
    }

    #[test]
    fn non_strict_inherits_path_and_ld_library_path() {
        let e = cfg(false, &[]).action_environment_for_os(false, "linux");
        assert!(e.fixed_env().is_empty());
        let inh: Vec<_> = e.inherited_env().iter().cloned().collect();
        assert_eq!(inh, vec!["LD_LIBRARY_PATH".to_string(), "PATH".to_string()]);
        let client = m(&[("PATH", "/opt/x:/bin"), ("LD_LIBRARY_PATH", "/lib"), ("FOO", "1")]);
        assert_eq!(
            e.resolved(&client),
            m(&[("PATH", "/opt/x:/bin"), ("LD_LIBRARY_PATH", "/lib")])
        );
        // A variable missing from the client environment is simply absent.
        assert!(e.resolved(&BTreeMap::new()).is_empty());
        assert_eq!(e.used_client_env(&m(&[("PATH", "p")])), m(&[("PATH", "p")]));
    }

    #[test]
    fn action_env_set_inherit_unset_last_wins() {
        let e = cfg(true, &["FOO=bar", "HOME", "FOO=baz", "BAZ=1", "=BAZ", "PATH=/custom"])
            .action_environment_for_os(false, "linux");
        assert_eq!(e.fixed_env()["FOO"], "baz");
        assert_eq!(e.fixed_env()["PATH"], "/custom");
        assert!(!e.fixed_env().contains_key("BAZ"));
        assert!(e.unset_env().contains("BAZ"));
        assert!(e.inherited_env().contains("HOME"));
        let r = e.resolved(&m(&[("HOME", "/home/u"), ("BAZ", "from-client")]));
        assert_eq!(r.get("HOME").map(String::as_str), Some("/home/u"));
        assert_eq!(r.get("FOO").map(String::as_str), Some("baz"));
        assert!(!r.contains_key("BAZ"));
        // `=NAME` after `NAME=` and a later `NAME=` re-sets.
        let e = cfg(true, &["A=1", "=A", "A=2"]).action_environment_for_os(false, "linux");
        assert_eq!(e.fixed_env()["A"], "2");
        assert!(e.unset_env().is_empty());
        // Inherit after a fixed value turns it into an inherited variable (value from client).
        let e = cfg(true, &["A=1", "A"]).action_environment_for_os(false, "linux");
        assert!(!e.fixed_env().contains_key("A"));
        assert!(e.inherited_env().contains("A"));
        // Unsetting the default PATH.
        let e = cfg(true, &["=PATH"]).action_environment_for_os(false, "linux");
        assert!(!e.resolved(&BTreeMap::new()).contains_key("PATH"));
    }

    #[test]
    fn exec_configuration_uses_host_action_env_only() {
        let c = EnvConfig {
            action_env: parse_env_vars(["T=1"]),
            host_action_env: parse_env_vars(["H=2"]),
            ..Default::default()
        };
        let t = c.action_environment_for_os(false, "linux");
        let x = c.action_environment_for_os(true, "linux");
        assert_eq!(t.fixed_env()["T"], "1");
        assert!(!t.fixed_env().contains_key("H"));
        assert_eq!(x.fixed_env()["H"], "2");
        assert!(!x.fixed_env().contains_key("T"));
        // Strictness applies to both.
        assert_eq!(x.fixed_env()["PATH"], "/bin:/usr/bin:/usr/local/bin");
    }

    #[test]
    fn runfiles_disabled_sets_manifest_only() {
        let c = EnvConfig { runfiles_enabled: false, ..Default::default() };
        assert_eq!(c.action_environment_for_os(false, "linux").fixed_env()["RUNFILES_MANIFEST_ONLY"], "1");
    }

    #[test]
    fn test_env_layers() {
        let c = EnvConfig { test_env: parse_env_vars(["FOO=baz", "BAR", "=HOME", "X=1", "X=2"]), ..Default::default() };
        let t = c.test_environment();
        assert_eq!(t.fixed_env(), &m(&[("FOO", "baz"), ("X", "2")]));
        assert!(t.inherited_env().contains("BAR"));
        assert!(t.unset_env().contains("HOME"));
        let mut env = test_policy_environment("/rf", "/tmp/t");
        assert_eq!(env["HOME"], "/tmp/t");
        t.resolve(&mut env, &m(&[("BAR", "client")]));
        assert!(!env.contains_key("HOME"));
        assert_eq!(env["BAR"], "client");
        assert_eq!(env["FOO"], "baz");
        assert_eq!(env["TZ"], "UTC");
        assert_eq!(env["TEST_TMPDIR"], "/tmp/t");
        assert_eq!(env["RUN_UNDER_RUNFILES"], "1");
    }

    #[test]
    fn spawn_action_environment_variants() {
        let cfg = cfg(false, &["FOO=cfg", "KEEP"]).action_environment_for_os(false, "linux");
        let explicit = m(&[("KEEP", "explicit"), ("NEW", "n")]);
        // Not using the default shell env: only the explicit variables.
        let e = create_spawn_action_environment(&cfg, false, &explicit);
        assert_eq!(e.fixed_env(), &explicit);
        assert!(e.inherited_env().is_empty());
        // Default env without explicit: the configuration's environment.
        assert_eq!(create_spawn_action_environment(&cfg, true, &BTreeMap::new()), cfg);
        // Default env + explicit: explicit overrides inherited variables of the same name.
        let e = create_spawn_action_environment(&cfg, true, &explicit);
        assert_eq!(e.fixed_env()["KEEP"], "explicit");
        assert_eq!(e.fixed_env()["FOO"], "cfg");
        assert!(!e.inherited_env().contains("KEEP"));
        assert!(e.inherited_env().contains("PATH"));
        let r = e.resolved(&m(&[("KEEP", "client"), ("PATH", "/p")]));
        assert_eq!(r["KEEP"], "explicit");
        assert_eq!(r["PATH"], "/p");
    }

    #[test]
    fn with_additional_fixed_variables_cancels_unset_but_not_inherited() {
        let base = ActionEnvironment::create(
            m(&[("A", "1")]),
            ["I".to_string()].into(),
            ["U".to_string()].into(),
        );
        let e = base.with_additional_fixed_variables(&m(&[("U", "x"), ("I", "fixed")]));
        assert!(e.unset_env().is_empty());
        let r = e.resolved(&m(&[("I", "client")]));
        assert_eq!(r["U"], "x");
        assert_eq!(r["I"], "client");
    }

    #[test]
    fn fingerprint_depends_on_inherited_names_and_unset() {
        let fp = |e: &ActionEnvironment| {
            let mut f = Fingerprint::new();
            e.add_to(&mut f);
            f.hex_digest_and_reset()
        };
        let a = cfg(true, &[]).action_environment_for_os(false, "linux");
        let b = cfg(true, &["HOME"]).action_environment_for_os(false, "linux");
        let c = cfg(true, &["=HOME"]).action_environment_for_os(false, "linux");
        assert_ne!(fp(&a), fp(&b));
        assert_ne!(fp(&a), fp(&c));
        assert_ne!(fp(&b), fp(&c));
        assert_eq!(fp(&a), fp(&a.clone()));
    }

    #[test]
    fn repository_environment_semantics() {
        let client = m(&[("PATH", "/p"), ("HOME", "/h"), ("SECRET", "s")]);
        let vars = parse_env_vars(["A=%bazel_workspace%/x", "SECRET", "MISSING", "=HOME"]);
        let (strict, nonstrict) = repository_environments(&client, &vars, true, Some("/ws"));
        assert_eq!(strict, m(&[("PATH", "/p"), ("A", "/ws/x"), ("SECRET", "s")]));
        assert_eq!(nonstrict, m(&[("PATH", "/p"), ("A", "/ws/x"), ("SECRET", "s")]));
        let (loose, nonstrict) = repository_environments(&client, &parse_env_vars(["B=1"]), false, None);
        assert_eq!(loose, m(&[("PATH", "/p"), ("HOME", "/h"), ("SECRET", "s"), ("B", "1")]));
        assert_eq!(loose, nonstrict);
        // `--action_env` never feeds the repository environment (no such input at all).
        let (strict_only, full) = repository_environments(&client, &[], true, None);
        assert_eq!(strict_only, m(&[("PATH", "/p")]));
        assert_eq!(full, client);
    }

    #[test]
    fn run_environment_layers() {
        let base = m(&[("BUILD_WORKSPACE_DIRECTORY", "/ws")]);
        let client = m(&[("HOME", "/h"), ("CLIENT", "c")]);
        let run_env = parse_env_vars(["A=1", "HOME", "NOPE", "=GONE", "B=2", "B=3"]);
        let target = ActionEnvironment::create(
            m(&[("A", "from-target"), ("T", "t")]),
            ["CLIENT".to_string()].into(),
            BTreeSet::new(),
        );
        let (env, clear) = run_environment(base, &run_env, &target, &client);
        assert_eq!(
            env,
            m(&[
                ("BUILD_WORKSPACE_DIRECTORY", "/ws"),
                ("A", "from-target"),
                ("HOME", "/h"),
                ("B", "3"),
                ("T", "t"),
                ("CLIENT", "c")
            ])
        );
        assert!(clear.contains("GONE"));
        assert!(clear.contains("TEST_SRCDIR") && clear.contains("RUNFILES_DIR"));
        assert!(!env.contains_key("NOPE"));
        // A later `NAME=` cancels an earlier `=NAME`.
        let (_, clear) = run_environment(BTreeMap::new(), &parse_env_vars(["=X", "X=1"]), &ActionEnvironment::EMPTY, &client);
        assert!(!clear.contains("X"));
    }

    #[test]
    fn client_env_arg_parsing() {
        assert_eq!(parse_client_env_arg("--client_env=A=b=c"), Some(("A".into(), "b=c".into())));
        assert_eq!(parse_client_env_arg("--client_env=A="), Some(("A".into(), String::new())));
        assert_eq!(parse_client_env_arg("--client_env=A"), None);
        assert_eq!(parse_client_env_arg("--other=A=b"), None);
    }
}
