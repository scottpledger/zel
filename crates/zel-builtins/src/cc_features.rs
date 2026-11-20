//! Port of `CcToolchainFeatures`, `FeatureSelection`, `FeatureConfiguration` and
//! `CcToolchainVariables`: the cc toolchain feature system. Features, action configs, flag sets
//! and env sets are read from the Starlark providers built by rules_cc's
//! `cc_toolchain_config_lib.bzl` (`FeatureInfo`, `FlagSetInfo`, `FlagGroupInfo`, ...).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use zel_starlark::{StarlarkError, Value};

type Res<T> = Result<T, StarlarkError>;

fn err<T>(m: impl Into<String>) -> Res<T> {
    Err(StarlarkError(m.into()))
}

// ---------------------------------------------------------------------------------------------
// Reading Starlark values

/// Field of a struct / provider instance / `HeaderInfo`.
pub fn field(v: &Value, name: &str) -> Option<Value> {
    match v {
        Value::Struct(s) => s.fields.get(name).cloned(),
        Value::ProviderInstance { fields, .. } => fields.get(name).cloned(),
        Value::Host(k, _) if k == "HeaderInfo" => {
            crate::cc_internal::header_info_attr(name, std::slice::from_ref(v)).ok()
        }
        _ => None,
    }
}

/// Elements of a list / tuple / depset / `None` (empty).
pub fn items(v: &Value) -> Vec<Value> {
    match v {
        Value::List(l) => l.lock().unwrap().elements.clone(),
        Value::Tuple(t) => t.clone(),
        Value::Depset(d) => d.to_list(),
        Value::Set(s) => s.lock().unwrap().elements.clone(),
        _ => vec![],
    }
}

fn str_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn str_list(v: Option<Value>) -> Vec<String> {
    v.map(|v| items(&v).iter().filter_map(str_of).collect())
        .unwrap_or_default()
}

fn opt_str(v: Option<Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) => Some(s),
        _ => None,
    }
}

fn truthy(v: &Option<Value>) -> bool {
    v.as_ref().map(|v| v.is_truthy()).unwrap_or(false)
}

// ---------------------------------------------------------------------------------------------
// Model

#[derive(Debug, Clone)]
pub struct WithFeatureSet {
    pub features: Vec<String>,
    pub not_features: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FlagGroup {
    pub flags: Vec<Vec<Chunk>>,
    pub nested: Vec<FlagGroup>,
    pub iterate_over: Option<String>,
    pub expand_if_available: Vec<String>,
    pub expand_if_not_available: Vec<String>,
    pub expand_if_true: Option<String>,
    pub expand_if_false: Option<String>,
    pub expand_if_equal: Option<(String, String)>,
    /// Source order of `flags` vs `nested` is irrelevant (a group has only one of them).
    pub ordered: Vec<Expandable>,
}

#[derive(Debug, Clone)]
pub enum Expandable {
    Flag(usize),
    Group(usize),
}

#[derive(Debug, Clone)]
pub struct FlagSet {
    pub actions: Vec<String>,
    pub with_features: Vec<WithFeatureSet>,
    pub flag_groups: Vec<FlagGroup>,
}

#[derive(Debug, Clone)]
pub struct EnvEntry {
    pub key: String,
    pub value: Vec<Chunk>,
    pub expand_if_available: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct EnvSet {
    pub actions: Vec<String>,
    pub entries: Vec<EnvEntry>,
    pub with_features: Vec<WithFeatureSet>,
}

#[derive(Debug, Clone)]
pub struct Feature {
    pub name: String,
    pub enabled: bool,
    pub flag_sets: Vec<FlagSet>,
    pub env_sets: Vec<EnvSet>,
    pub requires: Vec<Vec<String>>,
    pub implies: Vec<String>,
    pub provides: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Tool {
    /// Path as given; `crosstool_relative` tool paths are joined with the toolchain directory.
    pub path: String,
    pub crosstool_relative: bool,
    pub execution_requirements: Vec<String>,
    pub with_features: Vec<WithFeatureSet>,
}

#[derive(Debug, Clone)]
pub struct ActionConfig {
    pub config_name: String,
    pub action_name: String,
    pub enabled: bool,
    pub tools: Vec<Tool>,
    pub flag_sets: Vec<FlagSet>,
    pub implies: Vec<String>,
}

/// A string with `%{var}` references (`StringChunk`s).
#[derive(Debug, Clone)]
pub enum Chunk {
    Literal(String),
    Var(String),
}

/// `CcToolchainFeatures`.
#[derive(Debug)]
pub struct ToolchainFeatures {
    pub features: Vec<Feature>,
    pub action_configs: Vec<ActionConfig>,
    pub tools_directory: String,
    pub artifact_name_patterns: HashMap<String, (String, String)>,
    /// Index of selectables in definition order: features first, then action configs.
    names: Vec<String>,
    by_name: HashMap<String, usize>,
    action_config_by_action: HashMap<String, usize>,
    implies: Vec<Vec<usize>>,
    implied_by: Vec<Vec<usize>>,
    requires: Vec<Vec<Vec<usize>>>,
    required_by: Vec<Vec<usize>>,
    provides: Vec<Vec<String>>,
    default_selectables: Vec<String>,
}

/// `FeatureConfiguration`.
#[derive(Debug)]
pub struct FeatureConfiguration {
    pub toolchain: Arc<ToolchainFeatures>,
    pub requested: BTreeSet<String>,
    pub enabled_features: Vec<usize>,
    pub enabled_names: HashSet<String>,
    pub enabled_action_names: HashSet<String>,
}

// ---------------------------------------------------------------------------------------------
// Parsing

pub fn parse_chunks(s: &str) -> Res<Vec<Chunk>> {
    let mut out = Vec::new();
    let mut lit = String::new();
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '%' {
            match b.get(i + 1) {
                Some('%') => {
                    lit.push('%');
                    i += 2;
                }
                Some('{') => {
                    let Some(end) = b[i + 2..].iter().position(|c| *c == '}') else {
                        return err(format!(
                            "Invalid toolchain configuration: unterminated variable reference in '{s}'"
                        ));
                    };
                    if !lit.is_empty() {
                        out.push(Chunk::Literal(std::mem::take(&mut lit)));
                    }
                    out.push(Chunk::Var(b[i + 2..i + 2 + end].iter().collect()));
                    i += 2 + end + 1;
                }
                _ => {
                    return err(format!(
                        "Invalid toolchain configuration: expected '%%' or '%{{' after '%' in '{s}'"
                    ))
                }
            }
        } else {
            lit.push(b[i]);
            i += 1;
        }
    }
    if !lit.is_empty() {
        out.push(Chunk::Literal(lit));
    }
    Ok(out)
}

fn with_feature_sets(v: Option<Value>) -> Vec<WithFeatureSet> {
    v.map(|v| {
        items(&v)
            .iter()
            .map(|w| WithFeatureSet {
                features: str_list(field(w, "features")),
                not_features: str_list(field(w, "not_features")),
            })
            .collect()
    })
    .unwrap_or_default()
}

fn avail_list(v: Option<Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => vec![s],
        other => str_list(other),
    }
}

fn parse_flag_group(v: &Value) -> Res<FlagGroup> {
    let mut g = FlagGroup {
        flags: vec![],
        nested: vec![],
        iterate_over: opt_str(field(v, "iterate_over")),
        expand_if_available: avail_list(field(v, "expand_if_available")),
        expand_if_not_available: avail_list(field(v, "expand_if_not_available")),
        expand_if_true: opt_str(field(v, "expand_if_true")),
        expand_if_false: opt_str(field(v, "expand_if_false")),
        expand_if_equal: field(v, "expand_if_equal")
            .and_then(|e| Some((opt_str(field(&e, "name"))?, opt_str(field(&e, "value"))?))),
        ordered: vec![],
    };
    for f in str_list(field(v, "flags")) {
        g.ordered.push(Expandable::Flag(g.flags.len()));
        g.flags.push(parse_chunks(&f)?);
    }
    for n in items(&field(v, "flag_groups").unwrap_or(Value::None)) {
        g.ordered.push(Expandable::Group(g.nested.len()));
        g.nested.push(parse_flag_group(&n)?);
    }
    Ok(g)
}

fn parse_flag_set(v: &Value) -> Res<FlagSet> {
    Ok(FlagSet {
        actions: str_list(field(v, "actions")),
        with_features: with_feature_sets(field(v, "with_features")),
        flag_groups: items(&field(v, "flag_groups").unwrap_or(Value::None))
            .iter()
            .map(parse_flag_group)
            .collect::<Res<_>>()?,
    })
}

fn parse_env_set(v: &Value) -> Res<EnvSet> {
    let mut entries = vec![];
    for e in items(&field(v, "env_entries").unwrap_or(Value::None)) {
        entries.push(EnvEntry {
            key: opt_str(field(&e, "key")).unwrap_or_default(),
            value: parse_chunks(&opt_str(field(&e, "value")).unwrap_or_default())?,
            expand_if_available: avail_list(field(&e, "expand_if_available")),
        });
    }
    Ok(EnvSet {
        actions: str_list(field(v, "actions")),
        entries,
        with_features: with_feature_sets(field(v, "with_features")),
    })
}

/// Normalizes a relative or absolute path for `exec_os`, returning `(normalized, is_absolute)`.
pub fn normalize_os_path(raw: &str, is_windows: bool) -> (String, bool) {
    let raw_bytes = raw.as_bytes();
    let raw_win_abs = raw.starts_with("\\\\")
        || (raw_bytes.len() >= 3
            && raw_bytes[0].is_ascii_alphabetic()
            && raw_bytes[1] == b':'
            && (raw_bytes[2] == b'/' || raw_bytes[2] == b'\\'));
    let unified = if is_windows {
        raw.replace('\\', "/")
    } else {
        raw.to_string()
    };
    let (drive_or_root, rest, is_abs) = if is_windows {
        let bytes = unified.as_bytes();
        if unified.starts_with("//") {
            ("//".to_string(), unified.trim_start_matches('/'), true)
        } else if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            let after_drive = &unified[2..];
            let has_slash = after_drive.starts_with('/');
            (
                format!("{}:/", unified[..1].to_ascii_uppercase()),
                after_drive.trim_start_matches('/'),
                has_slash || after_drive.is_empty(),
            )
        } else if unified.starts_with('/') {
            ("/".to_string(), unified.trim_start_matches('/'), true)
        } else {
            (String::new(), unified.as_str(), false)
        }
    } else if unified.starts_with('/') {
        ("/".to_string(), unified.trim_start_matches('/'), true)
    } else {
        (String::new(), unified.as_str(), raw_win_abs)
    };
    let mut segs: Vec<&str> = Vec::new();
    for seg in rest.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            if let Some(last) = segs.last() {
                if *last != ".." {
                    segs.pop();
                    continue;
                }
            }
            if !is_abs {
                segs.push("..");
            }
        } else {
            segs.push(seg);
        }
    }
    let joined = segs.join("/");
    if is_abs {
        (format!("{drive_or_root}{joined}"), true)
    } else {
        (joined, false)
    }
}

fn validate_exec_os(exec_os: Option<&str>) -> Res<bool> {
    let Some(os) = exec_os else {
        return Ok(false);
    };
    match os {
        "DARWIN" | "FREEBSD" | "OPENBSD" | "LINUX" | "UNKNOWN" => Ok(false),
        "WINDOWS" => Ok(true),
        other => err(format!(
            "No enum constant com.google.devtools.build.lib.util.OS.{other}"
        )),
    }
}

/// `artifact_path_of(File)` resolves a `File`/artifact value to the path used by tools.
impl ToolchainFeatures {
    /// Builds the feature system from the `features`, `action_configs` and
    /// `artifact_name_patterns` of a `CcToolchainConfigInfo`.
    pub fn new(
        features: &[Value],
        action_configs: &[Value],
        artifact_name_patterns: &[Value],
        tools_directory: &str,
        tool_path: &dyn Fn(&Value) -> Option<String>,
    ) -> Res<ToolchainFeatures> {
        Self::new_with_os(
            features,
            action_configs,
            artifact_name_patterns,
            tools_directory,
            None,
            tool_path,
        )
    }

    /// Variant of [`Self::new`] that validates `_exec_os_DO_NOT_USE` and normalizes tool paths
    /// for that OS.
    pub fn new_with_os(
        features: &[Value],
        action_configs: &[Value],
        artifact_name_patterns: &[Value],
        tools_directory: &str,
        exec_os: Option<&str>,
        tool_path: &dyn Fn(&Value) -> Option<String>,
    ) -> Res<ToolchainFeatures> {
        let is_windows = validate_exec_os(exec_os)?;
        let check_type = |v: &Value, expected: &str| -> Res<()> {
            if let Some(Value::String(tn)) = field(v, "type_name") {
                if tn != expected {
                    return err(format!(
                        "Invalid toolchain configuration: expected type_name '{expected}', got '{tn}'"
                    ));
                }
            }
            Ok(())
        };
        let mut fs = Vec::new();
        for f in features {
            check_type(f, "feature")?;
            if field(f, "name").is_none() {
                return err(format!(
                    "Invalid toolchain configuration: feature has no name (got {})",
                    f.type_name()
                ));
            }
            let mut flag_sets = vec![];
            for s in items(&field(f, "flag_sets").unwrap_or(Value::None)) {
                check_type(&s, "flag_set")?;
                flag_sets.push(parse_flag_set(&s)?);
            }
            let mut env_sets = vec![];
            for s in items(&field(f, "env_sets").unwrap_or(Value::None)) {
                check_type(&s, "env_set")?;
                env_sets.push(parse_env_set(&s)?);
            }
            let requires = items(&field(f, "requires").unwrap_or(Value::None))
                .iter()
                .map(|r| str_list(field(r, "features")))
                .collect();
            fs.push(Feature {
                name: opt_str(field(f, "name")).unwrap_or_default(),
                enabled: truthy(&field(f, "enabled")),
                flag_sets,
                env_sets,
                requires,
                implies: str_list(field(f, "implies")),
                provides: str_list(field(f, "provides")),
            });
        }
        let mut acs = Vec::new();
        for a in action_configs {
            check_type(a, "action_config")?;
            let mut tools = vec![];
            for t in items(&field(a, "tools").unwrap_or(Value::None)) {
                check_type(&t, "tool")?;
                let tool_value = field(&t, "tool").filter(|v| !matches!(v, Value::None));
                let raw_path_field = field(&t, "path").filter(|v| !matches!(v, Value::None));
                if tool_value.is_some()
                    && raw_path_field
                        .as_ref()
                        .and_then(str_of)
                        .is_some_and(|p| p != "NOT_USED")
                {
                    return err(
                        "Invalid toolchain configuration: tool cannot specify both 'tool' and 'path'",
                    );
                }
                let (path, crosstool_relative) = match tool_value {
                    Some(tv) => (tool_path(&tv).unwrap_or_default(), false),
                    None => {
                        let raw_p = opt_str(raw_path_field).unwrap_or_default();
                        if raw_p.is_empty() {
                            return err(
                                "Invalid toolchain configuration: tool path must not be empty",
                            );
                        }
                        let (norm, is_abs) = normalize_os_path(&raw_p, is_windows);
                        let safe = if norm.is_empty() && !is_abs {
                            ".".to_string()
                        } else {
                            norm
                        };
                        (safe, !is_abs)
                    }
                };
                let mut reqs = Vec::new();
                let mut seen_reqs = HashSet::new();
                for r in str_list(field(&t, "execution_requirements")) {
                    if seen_reqs.insert(r.clone()) {
                        reqs.push(r);
                    }
                }
                tools.push(Tool {
                    path,
                    crosstool_relative,
                    execution_requirements: reqs,
                    with_features: with_feature_sets(field(&t, "with_features")),
                });
            }
            let action_name = opt_str(field(a, "action_name")).unwrap_or_default();
            let mut flag_sets = vec![];
            for s in items(&field(a, "flag_sets").unwrap_or(Value::None)) {
                check_type(&s, "flag_set")?;
                let mut fs = parse_flag_set(&s)?;
                if !fs.actions.is_empty() {
                    return Err(StarlarkError(format!(
                        "action_config {action_name} specifies actions.  An action_config's flag sets automatically apply to the configured action.  Thus, you must not specify action lists in an action_config's flag set."
                    )));
                }
                fs.actions = vec![action_name.clone()];
                flag_sets.push(fs);
            }
            let config_name = if !action_name.is_empty() {
                action_name.clone()
            } else {
                opt_str(field(a, "config_name")).unwrap_or_default()
            };
            acs.push(ActionConfig {
                config_name,
                action_name,
                enabled: truthy(&field(a, "enabled")),
                tools,
                flag_sets,
                implies: str_list(field(a, "implies")),
            });
        }
        let mut patterns: HashMap<String, (String, String)> = HashMap::new();
        for p in artifact_name_patterns {
            check_type(p, "artifact_name_pattern")?;
            let Some(cat) = opt_str(field(p, "category_name")) else {
                return err(
                    "Invalid toolchain configuration: artifact_name_pattern missing category_name",
                );
            };
            let cat_upper = cat.to_ascii_uppercase();
            let Some((def_prefix, allowed_exts)) = artifact_category_spec(&cat_upper) else {
                return err(format!("Artifact category {cat} is not recognized"));
            };
            if cat != cat_upper.to_ascii_lowercase() {
                return err(format!("Artifact category {cat} is not recognized"));
            }
            let prefix = opt_str(field(p, "prefix")).unwrap_or_default();
            let extension = opt_str(field(p, "extension")).unwrap_or_default();
            if !allowed_exts.contains(&extension.as_str()) {
                let formatted_exts: Vec<String> =
                    allowed_exts.iter().map(|e| format!("'{e}'")).collect();
                let list_str = match formatted_exts.len() {
                    0 => String::new(),
                    1 => formatted_exts[0].clone(),
                    2 => format!("{} or {}", formatted_exts[0], formatted_exts[1]),
                    n => format!(
                        "{}, or {}",
                        formatted_exts[..n - 1].join(", "),
                        formatted_exts[n - 1]
                    ),
                };
                return err(format!(
                    "Unrecognized file extension '{extension}', allowed extensions are {list_str}, please check artifact_name_pattern configuration for {cat}."
                ));
            }
            if patterns.contains_key(&cat_upper) {
                return err(format!(
                    "Invalid toolchain configuration: duplicate artifact_name_pattern for category '{cat}'"
                ));
            }
            if prefix == def_prefix && extension == allowed_exts[0] {
                continue;
            }
            patterns.insert(cat_upper, (prefix, extension));
        }
        Self::build(fs, acs, patterns, tools_directory)
    }

    fn build(
        features: Vec<Feature>,
        action_configs: Vec<ActionConfig>,
        artifact_name_patterns: HashMap<String, (String, String)>,
        tools_directory: &str,
    ) -> Res<ToolchainFeatures> {
        let mut names = Vec::new();
        let mut by_name = HashMap::new();
        let mut default_selectables = Vec::new();
        for f in &features {
            if by_name.insert(f.name.clone(), names.len()).is_some() {
                return err(format!(
                    "Invalid toolchain configuration: feature or action config '{}' was specified multiple times.",
                    f.name
                ));
            }
            names.push(f.name.clone());
            if f.enabled {
                default_selectables.push(f.name.clone());
            }
        }
        let mut action_config_by_action = HashMap::new();
        for (i, a) in action_configs.iter().enumerate() {
            if by_name.insert(a.config_name.clone(), names.len()).is_some() {
                return err(format!(
                    "Invalid toolchain configuration: feature or action config '{}' was specified multiple times.",
                    a.config_name
                ));
            }
            names.push(a.config_name.clone());
            if action_config_by_action
                .insert(a.action_name.clone(), i)
                .is_some()
            {
                return err(format!(
                    "Invalid toolchain configuration: multiple action configs for action '{}'",
                    a.action_name
                ));
            }
            if a.enabled {
                default_selectables.push(a.config_name.clone());
            }
        }
        let n = names.len();
        let mut implies = vec![vec![]; n];
        let mut implied_by = vec![vec![]; n];
        let mut requires = vec![vec![]; n];
        let mut required_by = vec![vec![]; n];
        let mut provides = vec![vec![]; n];
        let lookup = |name: &str, reference: &str| -> Res<usize> {
            by_name.get(name).copied().ok_or_else(|| {
                StarlarkError(format!(
                    "Invalid toolchain configuration: feature '{name}', which is referenced from feature '{reference}', is not defined."
                ))
            })
        };
        for f in &features {
            let me = by_name[&f.name];
            for req in &f.requires {
                let mut all = vec![];
                for r in req {
                    let ri = lookup(r, &f.name)?;
                    all.push(ri);
                    required_by[ri].push(me);
                }
                requires[me].push(all);
            }
            for i in &f.implies {
                let ii = lookup(i, &f.name)?;
                implied_by[ii].push(me);
                implies[me].push(ii);
            }
            provides[me] = f.provides.clone();
        }
        for a in &action_configs {
            let me = by_name[&a.config_name];
            for i in &a.implies {
                let ii = lookup(i, &a.config_name)?;
                implied_by[ii].push(me);
                implies[me].push(ii);
            }
        }
        Ok(ToolchainFeatures {
            features,
            action_configs,
            tools_directory: tools_directory.to_string(),
            artifact_name_patterns,
            names,
            by_name,
            action_config_by_action,
            implies,
            implied_by,
            requires,
            required_by,
            provides,
            default_selectables,
        })
    }

    pub fn default_features_and_action_configs(&self) -> Vec<String> {
        self.default_selectables.clone()
    }

    /// `FeatureSelection.run`.
    pub fn configure(self: &Arc<Self>, requested: &[String]) -> Res<FeatureConfiguration> {
        let requested_set: BTreeSet<String> = requested.iter().cloned().collect();
        let requested_sel: Vec<usize> = {
            let mut seen = HashSet::new();
            requested
                .iter()
                .filter_map(|n| self.by_name.get(n).copied())
                .filter(|i| seen.insert(*i))
                .collect()
        };
        let mut enabled: HashSet<usize> = HashSet::new();
        fn enable_all_implied(t: &ToolchainFeatures, s: usize, enabled: &mut HashSet<usize>) {
            if !enabled.insert(s) {
                return;
            }
            for &i in &t.implies[s] {
                enable_all_implied(t, i, enabled);
            }
        }
        for &s in &requested_sel {
            enable_all_implied(self, s, &mut enabled);
        }
        // disableUnsupportedActivatables
        let mut queue: std::collections::VecDeque<usize> = {
            let mut v: Vec<usize> = enabled.iter().copied().collect();
            v.sort();
            v.into()
        };
        let requested_idx: HashSet<usize> = requested_sel.iter().copied().collect();
        fn check(
            t: &ToolchainFeatures,
            s: usize,
            enabled: &mut HashSet<usize>,
            requested: &HashSet<usize>,
        ) {
            if !enabled.contains(&s) || t.is_satisfied(s, enabled, requested) {
                return;
            }
            enabled.remove(&s);
            for &x in &t.implied_by[s].clone() {
                check(t, x, enabled, requested);
            }
            for &x in &t.required_by[s].clone() {
                check(t, x, enabled, requested);
            }
            for &x in &t.implies[s].clone() {
                check(t, x, enabled, requested);
            }
        }
        while let Some(s) = queue.pop_front() {
            check(self, s, &mut enabled, &requested_idx);
        }
        let mut enabled_in_order: Vec<usize> = enabled.iter().copied().collect();
        enabled_in_order.sort();
        let enabled_features: Vec<usize> = enabled_in_order
            .iter()
            .copied()
            .filter(|&i| i < self.features.len())
            .collect();
        // Colliding `provides`.
        let mut by_symbol: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for &i in &enabled_in_order {
            for p in &self.provides[i] {
                by_symbol
                    .entry(p.as_str())
                    .or_default()
                    .push(self.names[i].as_str());
            }
        }
        for (sym, providers) in by_symbol {
            if providers.len() > 1 {
                return err(format!(
                    "Symbol {sym} is provided by all of the following features: {}",
                    providers.join(" ")
                ));
            }
        }
        let mut enabled_names: HashSet<String> = HashSet::new();
        let mut enabled_action_names: HashSet<String> = HashSet::new();
        for &i in &enabled_in_order {
            if i < self.features.len() {
                enabled_names.insert(self.features[i].name.clone());
            } else {
                enabled_action_names.insert(
                    self.action_configs[i - self.features.len()]
                        .action_name
                        .clone(),
                );
            }
        }
        Ok(FeatureConfiguration {
            toolchain: self.clone(),
            requested: requested_set,
            enabled_features,
            enabled_names,
            enabled_action_names,
        })
    }

    fn is_satisfied(&self, s: usize, enabled: &HashSet<usize>, requested: &HashSet<usize>) -> bool {
        let implied_by_enabled = self.implied_by[s].iter().any(|i| enabled.contains(i));
        (requested.contains(&s) || implied_by_enabled)
            && self.implies[s].iter().all(|i| enabled.contains(i))
            && (self.requires[s].is_empty()
                || self.requires[s]
                    .iter()
                    .any(|all| all.iter().all(|r| enabled.contains(r))))
    }

    /// `getArtifactNameForCategory`.
    pub fn artifact_name_for_category(&self, category: &str, output_name: &str) -> Res<String> {
        let (prefix, ext) = self.pattern(category)?;
        let (norm, is_abs) = normalize_os_path(output_name, false);
        if norm.is_empty() || (is_abs && norm == "/") {
            return err(format!("Invalid output_name '{output_name}'"));
        }
        let (dir, base) = match norm.rsplit_once('/') {
            Some((d, b)) => (Some(d), b),
            None => (None, norm.as_str()),
        };
        let name = format!("{prefix}{base}{ext}");
        Ok(match dir {
            Some("") if is_abs => format!("/{name}"),
            Some(d) if !d.is_empty() => format!("{d}/{name}"),
            _ => name,
        })
    }

    pub fn artifact_name_extension_for_category(&self, category: &str) -> Res<String> {
        Ok(self.pattern(category)?.1)
    }

    fn pattern(&self, category: &str) -> Res<(String, String)> {
        let upper = category.to_ascii_uppercase();
        if let Some(p) = self.artifact_name_patterns.get(&upper) {
            return Ok(p.clone());
        }
        default_artifact_pattern(&upper)
            .map(|(p, e)| (p.to_string(), e.to_string()))
            .ok_or_else(|| StarlarkError(format!("No enum constant ArtifactCategory.{category}")))
    }
}

/// `ArtifactCategory`: `(default_prefix, allowed_extensions)` where `allowed_extensions[0]` is
/// the default extension (`ArtifactCategory.java`).
pub fn artifact_category_spec(category: &str) -> Option<(&'static str, &'static [&'static str])> {
    Some(match category {
        "STATIC_LIBRARY" => ("lib", &[".a", ".lib"]),
        "ALWAYSLINK_STATIC_LIBRARY" => ("lib", &[".lo", ".lo.lib"]),
        "DYNAMIC_LIBRARY" => ("lib", &[".so", ".dylib", ".dll", ".pyd", ".wasm"]),
        "EXECUTABLE" => ("", &["", ".exe", ".wasm"]),
        "INTERFACE_LIBRARY" => ("lib", &[".ifso", ".tbd", ".if.lib", ".lib"]),
        "PIC_FILE" => ("", &[".pic"]),
        "INCLUDED_FILE_LIST" => ("", &[".d"]),
        "SERIALIZED_DIAGNOSTICS_FILE" => ("", &[".dia"]),
        "OBJECT_FILE" => ("", &[".o", ".obj"]),
        "PIC_OBJECT_FILE" => ("", &[".pic.o"]),
        "CPP_MODULE" => ("", &[".pcm", ".gcm", ".ifc"]),
        "CPP_MODULES_INFO" => ("", &[".CXXModules.json"]),
        "CPP_MODULES_DDI" => ("", &[".ddi"]),
        "CPP_MODULES_MODMAP" => ("", &[".modmap"]),
        "CPP_MODULES_MODMAP_INPUT" => ("", &[".modmap.input"]),
        "GENERATED_ASSEMBLY" => ("", &[".s", ".asm"]),
        "PROCESSED_HEADER" => ("", &[".processed"]),
        "GENERATED_HEADER" => ("", &[".h"]),
        "PREPROCESSED_C_SOURCE" => ("", &[".i"]),
        "PREPROCESSED_CPP_SOURCE" => ("", &[".ii"]),
        "COVERAGE_DATA_FILE" => ("", &[".gcno"]),
        "CLIF_OUTPUT_PROTO" => ("", &[".opb"]),
        _ => return None,
    })
}

/// `ArtifactCategory` default prefix and (first) extension.
pub fn default_artifact_pattern(category: &str) -> Option<(&'static str, &'static str)> {
    artifact_category_spec(category).map(|(p, exts)| (p, exts[0]))
}

// ---------------------------------------------------------------------------------------------
// Variables

/// `CcToolchainVariables`: a chain of name → value maps (`parent` consulted when absent).
#[derive(Debug, Default)]
pub struct Variables {
    pub parent: Option<Arc<Variables>>,
    pub map: BTreeMap<String, Value>,
}

/// Resolves a `File`/artifact value to its exec path (callers inside the analysis engine).
pub type PathOf<'a> = &'a dyn Fn(&Value) -> Option<String>;

#[derive(Clone)]
enum Var {
    Str(String),
    Bool(bool),
    Seq(Vec<Value>),
    Struct(Value),
}

impl Variables {
    /// All bindings visible from `self` (ancestors first, so descendants override).
    pub fn flattened(&self) -> BTreeMap<String, Value> {
        let mut m = self
            .parent
            .as_ref()
            .map(|p| p.flattened())
            .unwrap_or_default();
        for (k, v) in &self.map {
            m.insert(k.clone(), v.clone());
        }
        m
    }

    fn non_structured(&self, name: &str) -> Option<&Value> {
        match self.map.get(name) {
            Some(Value::None) => None,
            Some(v) => Some(v),
            None => self.parent.as_ref().and_then(|p| p.non_structured(name)),
        }
    }

    fn as_var(v: &Value, path_of: PathOf) -> Option<Var> {
        Some(match v {
            Value::None => return None,
            Value::Bool(b) => Var::Bool(*b),
            Value::String(s) => Var::Str(s.clone()),
            Value::Label(l) => Var::Str(l.canonical_form()),
            Value::List(_) | Value::Tuple(_) | Value::Depset(_) | Value::Set(_) => {
                Var::Seq(items(v))
            }
            Value::Struct(_) | Value::ProviderInstance { .. } => Var::Struct(v.clone()),
            other => match path_of(other) {
                Some(p) => Var::Str(p),
                None => Var::Struct(other.clone()),
            },
        })
    }

    fn lookup(&self, name: &str, path_of: PathOf) -> Res<Option<Var>> {
        if let Some(v) = self.non_structured(name) {
            return Ok(Self::as_var(v, path_of));
        }
        if !name.contains('.') {
            return Ok(None);
        }
        // Longest-prefix split into variable + field path.
        let mut fields: Vec<&str> = vec![];
        let mut path = name;
        let mut var: Option<Var> = None;
        loop {
            let Some(idx) = path.rfind('.') else { break };
            fields.push(&path[idx + 1..]);
            path = &path[..idx];
            if let Some(v) = self.non_structured(path) {
                var = Self::as_var(v, path_of);
                break;
            }
        }
        let Some(mut cur) = var else { return Ok(None) };
        while let Some(f) = fields.pop() {
            cur = match cur {
                Var::Struct(sv) => match field(&sv, f) {
                    Some(fv) => match Self::as_var(&fv, path_of) {
                        Some(v) => v,
                        None => return Ok(None),
                    },
                    None => return Ok(None),
                },
                _ => return Ok(None),
            };
        }
        Ok(Some(cur))
    }

    #[allow(dead_code)]
    fn is_available(&self, name: &str, path_of: PathOf) -> bool {
        matches!(self.lookup(name, path_of), Ok(Some(_)))
    }

    #[allow(dead_code)]
    fn get(&self, name: &str, path_of: PathOf) -> Res<Var> {
        match self.lookup(name, path_of)? {
            Some(v) => Ok(v),
            None => err(format!(
                "Invalid toolchain configuration: Cannot find variable named '{name}'."
            )),
        }
    }

    /// Mirrors `variables.isAvailable(name)` followed by `getVariable(name).getStringValue(name)`
    /// in Bazel's `CcToolchainVariables`.
    pub fn get_string_if_available(&self, name: &str, path_of: PathOf) -> Res<Option<String>> {
        match self.lookup(name, path_of)? {
            Some(v) => string_value(name, &v).map(Some),
            None => Ok(None),
        }
    }
}

fn truthy_var(v: &Var) -> bool {
    match v {
        Var::Bool(b) => *b,
        Var::Str(s) => !s.is_empty(),
        Var::Seq(s) => !s.is_empty(),
        Var::Struct(_) => true,
    }
}

fn var_type(v: &Var) -> &'static str {
    match v {
        Var::Str(_) => "string",
        Var::Bool(_) => "boolean",
        Var::Seq(_) => "sequence",
        Var::Struct(_) => "structure",
    }
}

fn string_value(name: &str, v: &Var) -> Res<String> {
    match v {
        Var::Str(s) => Ok(s.clone()),
        Var::Bool(b) => Ok(if *b { "1".to_string() } else { "0".to_string() }),
        other => err(format!(
            "Invalid toolchain configuration: Cannot expand variable '{name}': expected string, found {}",
            var_type(other)
        )),
    }
}

// ---------------------------------------------------------------------------------------------
// Expansion

/// A scope binding `iterate_over` variable to one element (`SingleVariables`).
struct Scope<'a> {
    base: &'a Variables,
    bound: Vec<(String, Value)>,
}

impl Scope<'_> {
    fn lookup(&self, name: &str, path_of: PathOf) -> Res<Option<Var>> {
        // Innermost binding first (exact name, or a field path below a bound name).
        for (bn, bv) in self.bound.iter().rev() {
            if name == bn {
                return Ok(Variables::as_var(bv, path_of));
            }
            if let Some(rest) = name.strip_prefix(&format!("{bn}.")) {
                let mut cur = Variables::as_var(bv, path_of);
                for f in rest.split('.') {
                    cur = match cur {
                        Some(Var::Struct(sv)) => {
                            field(&sv, f).and_then(|fv| Variables::as_var(&fv, path_of))
                        }
                        _ => None,
                    };
                }
                return Ok(cur);
            }
        }
        self.base.lookup(name, path_of)
    }

    fn is_available(&self, name: &str, path_of: PathOf) -> bool {
        matches!(self.lookup(name, path_of), Ok(Some(_)))
    }

    fn get(&self, name: &str, path_of: PathOf) -> Res<Var> {
        match self.lookup(name, path_of)? {
            Some(v) => Ok(v),
            None => err(format!(
                "Invalid toolchain configuration: Cannot find variable named '{name}'."
            )),
        }
    }

    fn expand_chunks(&self, chunks: &[Chunk], path_of: PathOf) -> Res<String> {
        let mut s = String::new();
        for c in chunks {
            match c {
                Chunk::Literal(l) => s.push_str(l),
                Chunk::Var(v) => s.push_str(&string_value(v, &self.get(v, path_of)?)?),
            }
        }
        Ok(s)
    }
}

fn with_features_satisfied(sets: &[WithFeatureSet], enabled: &HashSet<String>) -> bool {
    if sets.is_empty() {
        return true;
    }
    sets.iter().any(|s| {
        s.features.iter().all(|f| enabled.contains(f))
            && !s.not_features.iter().any(|f| enabled.contains(f))
    })
}

fn expand_group(
    g: &FlagGroup,
    scope: &mut Scope,
    path_of: PathOf,
    out: &mut Vec<String>,
) -> Res<()> {
    for v in &g.expand_if_available {
        if !scope.is_available(v, path_of) {
            return Ok(());
        }
    }
    for v in &g.expand_if_not_available {
        if scope.is_available(v, path_of) {
            return Ok(());
        }
    }
    if let Some(v) = &g.expand_if_true {
        if !scope.is_available(v, path_of) || !truthy_var(&scope.get(v, path_of)?) {
            return Ok(());
        }
    }
    if let Some(v) = &g.expand_if_false {
        if !scope.is_available(v, path_of) || truthy_var(&scope.get(v, path_of)?) {
            return Ok(());
        }
    }
    if let Some((v, val)) = &g.expand_if_equal {
        if !scope.is_available(v, path_of) || string_value(v, &scope.get(v, path_of)?)? != *val {
            return Ok(());
        }
    }
    let run = |scope: &mut Scope, out: &mut Vec<String>| -> Res<()> {
        for e in &g.ordered {
            match e {
                Expandable::Flag(i) => out.push(scope.expand_chunks(&g.flags[*i], path_of)?),
                Expandable::Group(i) => expand_group(&g.nested[*i], scope, path_of, out)?,
            }
        }
        Ok(())
    };
    if let Some(iter) = &g.iterate_over {
        let seq = match scope.get(iter, path_of)? {
            Var::Seq(s) => s,
            other => {
                return err(format!(
                    "Invalid toolchain configuration: Cannot expand variable '{iter}': expected sequence, found {}",
                    var_type(&other)
                ))
            }
        };
        for element in seq {
            scope.bound.push((iter.clone(), element));
            let r = run(scope, out);
            scope.bound.pop();
            r?;
        }
        Ok(())
    } else {
        run(scope, out)
    }
}

fn expand_flag_set(
    fs: &FlagSet,
    action: &str,
    scope: &mut Scope,
    enabled: &HashSet<String>,
    path_of: PathOf,
    out: &mut Vec<String>,
) -> Res<()> {
    if !with_features_satisfied(&fs.with_features, enabled) {
        return Ok(());
    }
    if !fs.actions.iter().any(|a| a == action) {
        return Ok(());
    }
    for g in &fs.flag_groups {
        expand_group(g, scope, path_of, out)?;
    }
    Ok(())
}

impl FeatureConfiguration {
    pub fn is_enabled(&self, feature: &str) -> bool {
        self.enabled_names.contains(feature)
    }

    pub fn action_is_configured(&self, action: &str) -> bool {
        self.enabled_action_names.contains(action)
    }

    /// `getPerFeatureExpansions`: returns `(selectable_name, flags)` buckets so callers such as
    /// `CompileCommandLine` can exempt `unfiltered_compile_flags` from `copts_filter`.
    pub fn per_feature_command_line(
        &self,
        action: &str,
        vars: &Variables,
        path_of: PathOf,
    ) -> Res<Vec<(String, Vec<String>)>> {
        let mut buckets: Vec<(String, Vec<String>)> = Vec::new();
        let mut scope = Scope {
            base: vars,
            bound: vec![],
        };
        if self.action_is_configured(action) {
            let ac = &self.toolchain.action_configs[self.toolchain.action_config_by_action[action]];
            let mut flags = Vec::new();
            for fs in &ac.flag_sets {
                expand_flag_set(
                    fs,
                    action,
                    &mut scope,
                    &self.enabled_names,
                    path_of,
                    &mut flags,
                )?;
            }
            if !flags.is_empty() {
                buckets.push((ac.config_name.clone(), flags));
            }
        }
        for &fi in &self.enabled_features {
            let feat = &self.toolchain.features[fi];
            let mut flags = Vec::new();
            for fs in &feat.flag_sets {
                expand_flag_set(
                    fs,
                    action,
                    &mut scope,
                    &self.enabled_names,
                    path_of,
                    &mut flags,
                )?;
            }
            if !flags.is_empty() {
                buckets.push((feat.name.clone(), flags));
            }
        }
        Ok(buckets)
    }

    /// `getCommandLine`.
    pub fn command_line(
        &self,
        action: &str,
        vars: &Variables,
        path_of: PathOf,
    ) -> Res<Vec<String>> {
        let mut out = Vec::new();
        for (_, flags) in self.per_feature_command_line(action, vars, path_of)? {
            out.extend(flags);
        }
        Ok(out)
    }

    /// `getEnvironmentVariables`.
    pub fn environment(
        &self,
        action: &str,
        vars: &Variables,
        path_of: PathOf,
    ) -> Res<Vec<(String, String)>> {
        let mut out: Vec<(String, String)> = Vec::new();
        let scope = Scope {
            base: vars,
            bound: vec![],
        };
        for &fi in &self.enabled_features {
            for es in &self.toolchain.features[fi].env_sets {
                if !es.actions.iter().any(|a| a == action)
                    || !with_features_satisfied(&es.with_features, &self.enabled_names)
                {
                    continue;
                }
                for e in &es.entries {
                    if !e
                        .expand_if_available
                        .iter()
                        .all(|v| scope.is_available(v, path_of))
                    {
                        continue;
                    }
                    let value = scope.expand_chunks(&e.value, path_of)?;
                    if let Some((_, prev)) = out.iter().find(|(k, _)| *k == e.key) {
                        return err(format!(
                            "Invalid toolchain configuration: Overlapping Variables {} is set to {} and {}",
                            e.key, prev, value
                        ));
                    }
                    out.push((e.key.clone(), value));
                }
            }
        }
        Ok(out)
    }

    fn registered_action_config(&self, action: &str) -> Option<&ActionConfig> {
        self.toolchain
            .action_config_by_action
            .get(action)
            .map(|&idx| &self.toolchain.action_configs[idx])
    }

    fn matching_tool(&self, action: &str) -> Res<&Tool> {
        let ac = self.registered_action_config(action).ok_or_else(|| {
            StarlarkError(format!(
                "Action {action} does not have an enabled configuration in the toolchain."
            ))
        })?;
        ac.tools
            .iter()
            .find(|t| with_features_satisfied(&t.with_features, &self.enabled_names))
            .ok_or_else(|| {
                StarlarkError(format!(
                    "Matching tool for action {action} not found for given feature configuration"
                ))
            })
    }

    /// `getToolPathForAction`.
    pub fn tool_path_for_action(&self, action: &str) -> Res<String> {
        let t = self.matching_tool(action)?;
        if t.crosstool_relative {
            let d = self.toolchain.tools_directory.trim_end_matches('/');
            let combined = if d.is_empty() {
                t.path.clone()
            } else if t.path == "." {
                d.to_string()
            } else {
                format!("{d}/{}", t.path)
            };
            let (norm, is_abs) = normalize_os_path(&combined, false);
            Ok(if norm.is_empty() && !is_abs {
                ".".to_string()
            } else {
                norm
            })
        } else {
            Ok(t.path.clone())
        }
    }

    pub fn tool_requirements_for_action(&self, action: &str) -> Res<Vec<String>> {
        Ok(self.matching_tool(action)?.execution_requirements.clone())
    }
}

// ---------------------------------------------------------------------------------------------
// Registries: native objects handed to Starlark as `Value::Host(kind, id)`.

fn registry<T>() -> &'static Mutex<Vec<Arc<T>>>
where
    T: Send + Sync + 'static,
{
    // One registry per type via a type-keyed map.
    static MAP: OnceLock<Mutex<HashMap<std::any::TypeId, Box<dyn std::any::Any + Send + Sync>>>> =
        OnceLock::new();
    let map = MAP.get_or_init(|| Mutex::new(HashMap::new()));
    let mut g = map.lock().unwrap();
    let entry = g.entry(std::any::TypeId::of::<T>()).or_insert_with(|| {
        let leaked: &'static Mutex<Vec<Arc<T>>> = Box::leak(Box::new(Mutex::new(Vec::new())));
        Box::new(leaked)
    });
    entry
        .downcast_ref::<&'static Mutex<Vec<Arc<T>>>>()
        .copied()
        .unwrap()
}

pub fn register<T: Send + Sync + 'static>(kind: &str, v: T) -> Value {
    let r = registry::<T>();
    let mut g = r.lock().unwrap();
    g.push(Arc::new(v));
    Value::Host(kind.to_string(), (g.len() - 1).to_string())
}

pub fn lookup<T: Send + Sync + 'static>(v: &Value, kind: &str) -> Option<Arc<T>> {
    match v {
        Value::Host(k, id) if k == kind => registry::<T>()
            .lock()
            .unwrap()
            .get(id.parse::<usize>().ok()?)
            .cloned(),
        _ => None,
    }
}

pub const KIND_FEATURES: &str = "cc_toolchain_features";
pub const KIND_FEATURE_CONFIGURATION: &str = "FeatureConfiguration";
pub const KIND_VARIABLES: &str = "cc_toolchain_variables";

pub fn variables_of(v: &Value) -> Option<Arc<Variables>> {
    lookup::<Variables>(v, KIND_VARIABLES)
}

/// `cc_toolchain_variables(vars)` / `combine_cc_toolchain_variables(parent, variables)`.
pub fn make_variables(parent: Option<Arc<Variables>>, map: BTreeMap<String, Value>) -> Value {
    register(KIND_VARIABLES, Variables { parent, map })
}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_starlark::{StarlarkList, StructValue};

    fn mk_struct(fields: Vec<(&str, Value)>) -> Value {
        Value::Struct(Arc::new(StructValue::new(
            fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )))
    }

    fn mk_list(items: Vec<Value>) -> Value {
        Value::List(Arc::new(Mutex::new(StarlarkList::new(items))))
    }

    #[test]
    fn action_config_flag_set_rejects_actions_and_defaults_to_action_name() {
        let fg = mk_struct(vec![("flags", mk_list(vec![Value::String("-O2".into())]))]);
        let bad_fs = mk_struct(vec![
            (
                "actions",
                mk_list(vec![Value::String("other-action".into())]),
            ),
            ("flag_groups", mk_list(vec![fg.clone()])),
        ]);
        let bad_ac = mk_struct(vec![
            ("action_name", Value::String("c-compile".into())),
            ("enabled", Value::Bool(true)),
            ("flag_sets", mk_list(vec![bad_fs])),
        ]);
        let err = ToolchainFeatures::new(&[], &[bad_ac], &[], "", &|_| None)
            .err()
            .unwrap();
        assert!(err.0.contains("specifies actions"), "{}", err.0);

        let good_fs = mk_struct(vec![
            ("actions", mk_list(vec![])),
            ("flag_groups", mk_list(vec![fg])),
        ]);
        let good_ac = mk_struct(vec![
            ("action_name", Value::String("c-compile".into())),
            ("enabled", Value::Bool(true)),
            ("flag_sets", mk_list(vec![good_fs])),
        ]);
        let tf = Arc::new(ToolchainFeatures::new(&[], &[good_ac], &[], "", &|_| None).unwrap());
        let fc = tf.configure(&["c-compile".into()]).unwrap();
        let vars = Variables::default();
        assert_eq!(
            fc.command_line("c-compile", &vars, &|_| None).unwrap(),
            vec!["-O2"]
        );
    }

    #[test]
    fn windows_drive_and_unc_tool_paths_are_not_crosstool_prefixed() {
        let mk_ac = |name: &str, tool_path: &str| {
            let tool = mk_struct(vec![("path", Value::String(tool_path.into()))]);
            mk_struct(vec![
                ("action_name", Value::String(name.into())),
                ("enabled", Value::Bool(true)),
                ("tools", mk_list(vec![tool])),
            ])
        };
        let acs = vec![
            mk_ac("win-fwd", "C:/MSVC/bin/link.exe"),
            mk_ac("win-back", r"D:\LLVM\bin\lld-link.exe"),
            mk_ac("win-unc", r"\\buildsrv\msvc\cl.exe"),
            mk_ac("rel-tool", "bin/clang"),
        ];
        let tf = Arc::new(
            ToolchainFeatures::new(&[], &acs, &[], "external/msvc_tc", &|_| None).unwrap(),
        );
        let fc = tf
            .configure(&[
                "win-fwd".into(),
                "win-back".into(),
                "win-unc".into(),
                "rel-tool".into(),
            ])
            .unwrap();
        assert_eq!(
            fc.tool_path_for_action("win-fwd").unwrap(),
            "C:/MSVC/bin/link.exe"
        );
        assert_eq!(
            fc.tool_path_for_action("win-back").unwrap(),
            r"D:\LLVM\bin\lld-link.exe"
        );
        assert_eq!(
            fc.tool_path_for_action("win-unc").unwrap(),
            r"\\buildsrv\msvc\cl.exe"
        );
        assert_eq!(
            fc.tool_path_for_action("rel-tool").unwrap(),
            "external/msvc_tc/bin/clang"
        );
    }

    #[test]
    fn legacy_cc_flags_make_variable_returns_string_or_empty() {
        let with_flags = mk_struct(vec![(
            "_legacy_cc_flags_make_variable",
            Value::String("-m64 -pthread".into()),
        )]);
        let got = crate::natives::call(
            "cc_common.legacy_cc_flags_make_variable_do_not_use",
            vec![with_flags],
            BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(got, Value::String("-m64 -pthread".into()));

        let without_flags = mk_struct(vec![]);
        let empty = crate::natives::call(
            "cc_common.legacy_cc_flags_make_variable_do_not_use",
            vec![],
            BTreeMap::from([("cc_toolchain".to_string(), without_flags)]),
        )
        .unwrap();
        assert_eq!(empty, Value::String(String::new()));
    }

    #[test]
    fn artifact_category_validation_exec_os_and_bool_var_expansion() {
        let valid_pat = mk_struct(vec![
            ("category_name", Value::String("dynamic_library".into())),
            ("prefix", Value::String("lib".into())),
            ("extension", Value::String(".dylib".into())),
        ]);
        let tf =
            ToolchainFeatures::new_with_os(&[], &[], &[valid_pat], "", Some("DARWIN"), &|_| None)
                .unwrap();
        assert_eq!(
            tf.artifact_name_for_category("dynamic_library", "dir//foo")
                .unwrap(),
            "dir/libfoo.dylib"
        );

        let bad_ext = mk_struct(vec![
            ("category_name", Value::String("object_file".into())),
            ("prefix", Value::String("".into())),
            ("extension", Value::String(".bad".into())),
        ]);
        let e = ToolchainFeatures::new_with_os(&[], &[], &[bad_ext], "", Some("LINUX"), &|_| None)
            .err()
            .unwrap();
        assert!(
            e.0.contains("Unrecognized file extension '.bad'"),
            "{}",
            e.0
        );

        let bad_os = ToolchainFeatures::new_with_os(&[], &[], &[], "", Some("SOLARIS"), &|_| None)
            .err()
            .unwrap();
        assert!(bad_os.0.contains("SOLARIS"), "{}", bad_os.0);

        let win_tool = mk_struct(vec![(
            "path",
            Value::String(r"C:\msvc\bin\..\cl.exe".into()),
        )]);
        let win_ac = mk_struct(vec![
            ("action_name", Value::String("c-compile".into())),
            ("enabled", Value::Bool(true)),
            ("tools", mk_list(vec![win_tool])),
        ]);
        let wtf = Arc::new(
            ToolchainFeatures::new_with_os(
                &[],
                &[win_ac],
                &[],
                "crosstool",
                Some("WINDOWS"),
                &|_| None,
            )
            .unwrap(),
        );
        let wfc = wtf.configure(&[]).unwrap();
        assert_eq!(
            wfc.tool_path_for_action("c-compile").unwrap(),
            "C:/msvc/cl.exe"
        );

        let bool_fg = mk_struct(vec![(
            "flags",
            mk_list(vec![Value::String("-DPIC=%{pic_flag}".into())]),
        )]);
        let bool_fs = mk_struct(vec![
            ("actions", mk_list(vec![Value::String("c-compile".into())])),
            ("flag_groups", mk_list(vec![bool_fg])),
        ]);
        let feat = mk_struct(vec![
            ("name", Value::String("unfiltered_compile_flags".into())),
            ("enabled", Value::Bool(true)),
            ("flag_sets", mk_list(vec![bool_fs])),
        ]);
        let btf = Arc::new(ToolchainFeatures::new(&[feat], &[], &[], "", &|_| None).unwrap());
        let bfc = btf
            .configure(&btf.default_features_and_action_configs())
            .unwrap();
        let mut vmap = BTreeMap::new();
        vmap.insert("pic_flag".into(), Value::Bool(true));
        let bvars = Variables {
            parent: None,
            map: vmap,
        };
        let buckets = bfc
            .per_feature_command_line("c-compile", &bvars, &|_| None)
            .unwrap();
        assert_eq!(
            buckets,
            vec![(
                "unfiltered_compile_flags".to_string(),
                vec!["-DPIC=1".to_string()]
            )]
        );
    }

    #[test]
    fn tool_lookup_errors_on_unregistered_action_and_resolves_disabled_action_config() {
        let tool = mk_struct(vec![
            ("path", Value::String("bin/ar".into())),
            (
                "execution_requirements",
                mk_list(vec![Value::String("no-remote".into())]),
            ),
        ]);
        let disabled_ac = mk_struct(vec![
            (
                "action_name",
                Value::String("c++-link-static-library".into()),
            ),
            ("enabled", Value::Bool(false)),
            ("tools", mk_list(vec![tool])),
        ]);
        let tf = Arc::new(
            ToolchainFeatures::new(&[], &[disabled_ac], &[], "crosstool/tools", &|_| None).unwrap(),
        );
        let fc = tf.configure(&[]).unwrap();

        assert!(!fc.action_is_configured("c++-link-static-library"));
        assert_eq!(
            fc.tool_path_for_action("c++-link-static-library").unwrap(),
            "crosstool/tools/bin/ar"
        );
        assert_eq!(
            fc.tool_requirements_for_action("c++-link-static-library")
                .unwrap(),
            vec!["no-remote"]
        );

        let missing_err = fc.tool_path_for_action("unregistered-action").unwrap_err();
        assert_eq!(
            missing_err.0,
            "Action unregistered-action does not have an enabled configuration in the toolchain."
        );
        let missing_req_err = fc
            .tool_requirements_for_action("unregistered-action")
            .unwrap_err();
        assert_eq!(
            missing_req_err.0,
            "Action unregistered-action does not have an enabled configuration in the toolchain."
        );
    }
}
