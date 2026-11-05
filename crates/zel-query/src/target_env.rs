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

//! The unconfigured-target query environment (`SkyQueryEnvironment` + `BlazeTargetAccessor`):
//! lazily loads packages through a [`PatternResolver`], exposes attribute values, dependency
//! edges (with `--[no]implicit_deps` / `--[no]tool_deps` filtering), visibility, BUILD/load
//! files, siblings and test-suite expansion.

use crate::engine::{Diag, Node, NodeSet, QueryEnv, QueryError};
use crate::parse::{self, Arg, Expr, FuncSpec};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use zel_cmdline::{Label, PackageIdentifier, RepositoryMapping, RepositoryName};
use zel_packages::attr::{AttrType, AttrValue, SelectPart};
use zel_packages::pkg_spec::{PackageGroupContents, PackageSpec};
use zel_packages::{Package, PatternResolver, RuleInstance, Target};
use zel_vfs::PathFragment;

/// Dependency-edge related options shared by all query flavours.
#[derive(Debug, Clone)]
pub struct QueryOpts {
    pub keep_going: bool,
    pub implicit_deps: bool,
    pub tool_deps: bool,
    pub include_aspects: bool,
    pub strict_test_suite: bool,
    pub universe_scope: Vec<String>,
    pub infer_universe_scope: bool,
    pub package_group_includes_double_slash: bool,
    pub nodep_deps: bool,
    pub aspect_deps: String,
    /// Whether `SkyQueryEnvironment` semantics apply: unordered/streamed output together with a
    /// (constant or inferred) universe scope. Enables `allrdeps`/`rbuildfiles` and strict scope.
    pub sky: bool,
    /// `--experimental_graphless_query` (`TriState`): `Some(true)` = yes, `Some(false)` = no, `None` = auto.
    pub graphless_query: Option<bool>,
}

impl Default for QueryOpts {
    fn default() -> Self {
        QueryOpts {
            keep_going: false,
            implicit_deps: true,
            tool_deps: true,
            include_aspects: true,
            strict_test_suite: false,
            universe_scope: Vec::new(),
            infer_universe_scope: false,
            package_group_includes_double_slash: true,
            nodep_deps: true,
            aspect_deps: "conservative".into(),
            sky: false,
            graphless_query: None,
        }
    }
}

/// A loaded target together with its package.
#[derive(Debug, Clone)]
pub struct TInfo {
    pub target: Target,
    pub pkg: Arc<Package>,
}

/// Bazel-style attribute discriminator numbers (`Build.Attribute.Discriminator`).
pub mod disc {
    pub const INTEGER: i32 = 1;
    pub const STRING: i32 = 2;
    pub const LABEL: i32 = 3;
    pub const OUTPUT: i32 = 4;
    pub const STRING_LIST: i32 = 5;
    pub const LABEL_LIST: i32 = 6;
    pub const OUTPUT_LIST: i32 = 7;
    pub const DISTRIBUTION_SET: i32 = 8;
    pub const LICENSE: i32 = 9;
    pub const STRING_DICT: i32 = 10;
    pub const STRING_LIST_DICT: i32 = 13;
    pub const BOOLEAN: i32 = 14;
    pub const TRISTATE: i32 = 15;
    pub const INTEGER_LIST: i32 = 16;
    pub const LABEL_DICT_UNARY: i32 = 19;
    pub const SELECTOR_LIST: i32 = 20;
    pub const LABEL_KEYED_STRING_DICT: i32 = 21;

    /// The discriminator number for a `Build.Attribute.Discriminator` name.
    pub fn from_name(n: &str) -> Option<i32> {
        (1..=21).find(|d| name(*d) == n)
    }

    pub fn name(d: i32) -> &'static str {
        match d {
            1 => "INTEGER",
            2 => "STRING",
            3 => "LABEL",
            4 => "OUTPUT",
            5 => "STRING_LIST",
            6 => "LABEL_LIST",
            7 => "OUTPUT_LIST",
            8 => "DISTRIBUTION_SET",
            9 => "LICENSE",
            10 => "STRING_DICT",
            11 => "FILESET_ENTRY_LIST",
            12 => "LABEL_LIST_DICT",
            13 => "STRING_LIST_DICT",
            14 => "BOOLEAN",
            15 => "TRISTATE",
            16 => "INTEGER_LIST",
            18 => "UNKNOWN",
            19 => "LABEL_DICT_UNARY",
            20 => "SELECTOR_LIST",
            21 => "LABEL_KEYED_STRING_DICT",
            _ => "UNKNOWN",
        }
    }
}

/// One attribute of a rule as `Rule.getAttributes()` exposes it.
#[derive(Debug, Clone)]
pub struct AttrEntry {
    pub name: String,
    pub ty: AttrType,
    pub value: AttrValue,
    pub explicit: bool,
    pub nodep: bool,
    pub tool: bool,
    pub configurable: bool,
    pub disc: i32,
    /// A string attribute whose default is `null` (not set and no default value).
    pub null_default: bool,
    /// Whether the proto `nodep` field is serialized for this attribute (`AttributeFormatter`).
    pub nodep_set: bool,
}

/// Order of the attributes every rule class inherits from `BaseRuleClasses` (after `name`).
const COMMON_ORDER: &[&str] = &[
    "visibility",
    "deprecation",
    "tags",
    "generator_name",
    "generator_function",
    "generator_location",
    "testonly",
    "features",
    "compatible_with",
    "restricted_to",
    "licenses",
    "distribs",
    "target_compatible_with",
    "toolchains",
    "exec_compatible_with",
    "exec_properties",
    "aspect_hints",
    "package_metadata",
];

/// Declaration order of the own attributes of common native rule classes (`RuleClass.Builder`
/// order in Bazel's rule definitions).
fn native_attr_order(rule_class: &str) -> &'static [&'static str] {
    match rule_class {
        "genrule" => &[
            "srcs", "outs", "cmd", "cmd_bash", "cmd_ps", "cmd_bat", "tools", "exec_tools", "executable", "local",
            "message", "output_licenses", "output_to_bindir", "stamp",
        ],
        "filegroup" => &["srcs", "data", "output_group", "path"],
        "alias" => &["actual"],
        "test_suite" => &["tests"],
        "config_setting" => &["values", "define_values", "flag_values", "constraint_values"],
        "cc_library" => &[
            "srcs", "hdrs", "textual_hdrs", "deps", "data", "copts", "defines", "includes", "linkopts", "alwayslink",
        ],
        "cc_binary" => &["srcs", "deps", "data", "copts", "defines", "includes", "linkopts", "linkstatic"],
        "cc_test" => &["srcs", "deps", "data", "copts", "defines", "includes", "linkopts", "linkstatic", "args", "env"],
        "java_library" => &["srcs", "deps", "data", "resources", "exports", "runtime_deps", "javacopts"],
        "java_binary" => &["srcs", "deps", "data", "main_class", "resources", "runtime_deps", "jvm_flags", "args"],
        "java_test" => &["srcs", "deps", "data", "test_class", "resources", "runtime_deps", "jvm_flags", "args"],
        "py_library" => &["srcs", "deps", "data", "imports"],
        "py_binary" => &["srcs", "deps", "data", "main", "imports", "args"],
        "py_test" => &["srcs", "deps", "data", "main", "imports", "args"],
        "proto_library" => &["srcs", "deps", "strip_import_prefix", "import_prefix"],
        _ => &[],
    }
}

fn disc_for(_name: &str, ty: AttrType) -> i32 {
    match ty {
        AttrType::Int => disc::INTEGER,
        AttrType::String => disc::STRING,
        AttrType::Label => disc::LABEL,
        AttrType::Output => disc::OUTPUT,
        AttrType::StringList => disc::STRING_LIST,
        AttrType::LabelList => disc::LABEL_LIST,
        AttrType::OutputList => disc::OUTPUT_LIST,
        AttrType::StringDict => disc::STRING_DICT,
        AttrType::StringListDict => disc::STRING_LIST_DICT,
        AttrType::Bool => disc::BOOLEAN,
        AttrType::Tristate => disc::TRISTATE,
        AttrType::IntList => disc::INTEGER_LIST,
        AttrType::StringKeyedLabelDict => disc::LABEL_DICT_UNARY,
        AttrType::LabelKeyedStringDict => disc::LABEL_KEYED_STRING_DICT,
    }
}

fn type_of_value(v: &AttrValue) -> AttrType {
    match v {
        AttrValue::Int(_) => AttrType::Int,
        AttrValue::Str(_) => AttrType::String,
        AttrValue::Bool(_) => AttrType::Bool,
        AttrValue::Tristate(_) => AttrType::Tristate,
        AttrValue::StrList(_) => AttrType::StringList,
        AttrValue::IntList(_) => AttrType::IntList,
        AttrValue::StrDict(_) => AttrType::StringDict,
        AttrValue::StrListDict(_) => AttrType::StringListDict,
        AttrValue::Label(_) => AttrType::Label,
        AttrValue::LabelList(_) => AttrType::LabelList,
        AttrValue::LabelKeyedStrDict(_) => AttrType::LabelKeyedStringDict,
        AttrValue::StrKeyedLabelDict(_) => AttrType::StringKeyedLabelDict,
        AttrValue::Output(_) => AttrType::Output,
        AttrValue::OutputList(_) => AttrType::OutputList,
        AttrValue::Select(parts) => parts
            .iter()
            .find_map(|p| match p {
                SelectPart::Plain(v) => Some(type_of_value(v)),
                SelectPart::Select { entries, .. } => entries.first().map(|(_, v)| type_of_value(v)),
            })
            .unwrap_or(AttrType::StringList),
    }
}

/// The attribute list of a rule in Bazel order: `name`, the common attributes, then the rule
/// class' own attributes, then (for open rule classes) attributes outside the schema.
pub fn rule_attributes(rule: &RuleInstance) -> Vec<AttrEntry> {
    let rc = rule.extra.rule_class.as_ref();
    let mut names: Vec<String> = vec!["name".to_string()];
    let schema_has = |n: &str| rc.map(|rc| rc.attr_or_implicit(n).is_some()).unwrap_or(false);
    if let Some(rc) = rc {
        if !rc.attr_order.is_empty() {
            for n in &rc.attr_order {
                if n != "name" && !names.contains(n) && (rc.attr_or_implicit(n).is_some() || rule.extra.attr_values.contains_key(n)) {
                    names.push(n.clone());
                }
            }
        } else {
            for c in COMMON_ORDER {
                if schema_has(c) || rule.extra.attr_values.contains_key(*c) {
                    names.push((*c).to_string());
                }
            }
            for n in native_attr_order(&rc.name) {
                if rc.attrs.contains_key(*n) && !names.contains(&(*n).to_string()) {
                    names.push((*n).to_string());
                }
            }
            for n in rc.attrs.keys() {
                if !names.contains(n) {
                    names.push(n.clone());
                }
            }
        }
    } else {
        for c in COMMON_ORDER {
            if rule.extra.attr_values.contains_key(*c) {
                names.push((*c).to_string());
            }
        }
    }
    for n in rule.extra.attr_values.keys() {
        if !names.contains(n) {
            names.push(n.clone());
        }
    }
    let mut out = Vec::new();
    for n in names {
        let schema = rc.and_then(|rc| rc.attr_or_implicit(&n));
        let (value, explicit) = if n == "name" {
            (AttrValue::Str(rule.label.name().to_string()), true)
        } else if n == "visibility" {
            let v = rule.extra.attr_values.get(&n).cloned().unwrap_or_else(|| {
                if rule.extra.effective_visibility.is_empty() {
                    // Without `default_visibility`, rules are private to their package.
                    let private = Label::parse_canonical("//visibility:private").ok();
                    AttrValue::LabelList(private.into_iter().collect())
                } else {
                    AttrValue::LabelList(rule.extra.effective_visibility.clone())
                }
            });
            (v, rule.extra.visibility_explicit)
        } else if n == "features" && rule.extra.own_features.is_some() {
            (rule.extra.own_features.clone().unwrap(), true)
        } else if rule.extra.pkg_default_attrs.contains(&n) {
            // Populated from `package()`: never *explicitly specified*. `testonly`/`deprecation`/
            // `package_metadata` are computed defaults that evaluate to the package value;
            // `compatible_with`/`restricted_to`/`features` stay at the schema default (the
            // package values live in `PackageArgs`).
            match n.as_str() {
                "testonly" | "deprecation" | "package_metadata" => (rule.extra.attr_values[&n].clone(), false),
                _ => match schema {
                    Some(a) => (a.default_value(), false),
                    None => continue,
                },
            }
        } else if let Some(v) = rule.extra.attr_values.get(&n) {
            (v.clone(), true)
        } else if n == "generator_name" && rule.extra.generator_name.is_some() {
            (AttrValue::Str(rule.extra.generator_name.clone().unwrap_or_default()), true)
        } else if n == "generator_function" && rule.extra.generator_function.is_some() {
            (AttrValue::Str(rule.extra.generator_function.clone().unwrap_or_default()), true)
        } else if n == "generator_location" && rule.extra.generator_location.is_some() {
            (AttrValue::Str(rule.extra.generator_location.clone().unwrap_or_default()), true)
        } else if n == "timeout" && schema.is_some() && rule.extra.attr_values.get("size").is_some() | schema.is_some() {
            // `timeout` defaults according to `size` (`TestSize.getDefaultTimeout`).
            let size = match rule.extra.attr_values.get("size") {
                Some(AttrValue::Str(s)) => s.clone(),
                _ => match rc.and_then(|rc| rc.attr("size")).map(|a| a.default_value()) {
                    Some(AttrValue::Str(s)) => s,
                    _ => "medium".to_string(),
                },
            };
            let d = match size.as_str() {
                "small" => "short",
                "large" => "long",
                "enormous" => "eternal",
                _ => "moderate",
            };
            (AttrValue::Str(d.to_string()), false)
        } else if let Some(src) = schema.and_then(|a| a.rule_based_default.as_ref()) {
            // Late-bound `:alias` of `label_flag`/`label_setting`: query shows the rule-based
            // default, the raw `build_setting_default` label.
            let v = match rule.extra.attr_values.get(src.as_str()) {
                Some(AttrValue::Label(l)) => AttrValue::Label(l.clone()),
                Some(AttrValue::Str(s)) => AttrValue::Label(Label::parse_canonical(s).ok()),
                _ => AttrValue::Label(None),
            };
            (v, false)
        } else if let Some(a) = schema {
            (a.default_value(), false)
        } else {
            continue;
        };
        let ty = schema.map(|a| a.ty).unwrap_or_else(|| type_of_value(&value));
        // Generated rule definitions (`rule_defs_gen`) carry Bazel's exact flags; hand-typed
        // classes fall back to the name-based approximation.
        let nodep = match schema {
            Some(a) if a.proto_type.is_some() => a.nodep,
            _ => matches!(n.as_str(), "visibility" | "deprecation" | "licenses" | "generator_name" | "generator_function" | "generator_location"),
        };
        let disc = match schema.and_then(|a| a.proto_type) {
            Some(p) => disc::from_name(p).unwrap_or_else(|| disc_for(&n, ty)),
            None => disc_for(&n, ty),
        };
        let null_default = !explicit
            && ty == AttrType::String
            && !rule.extra.pkg_default_attrs.contains(&n)
            && schema.map(|a| a.default.is_none()).unwrap_or(false);
        let nodep_set = schema.and_then(|a| a.nodep_set).unwrap_or(matches!(
            ty,
            AttrType::String | AttrType::Label | AttrType::Output | AttrType::StringList | AttrType::LabelList
                | AttrType::StringKeyedLabelDict | AttrType::LabelKeyedStringDict | AttrType::OutputList
        ));
        out.push(AttrEntry {
            nodep_set,
            null_default,
            disc,
            configurable: schema.map(|a| a.configurable).unwrap_or(true) && n != "name",
            tool: schema.map(|a| a.tool_dep.unwrap_or(a.cfg_exec)).unwrap_or(false),
            name: n,
            ty,
            value,
            explicit,
            nodep,
        });
    }
    out
}

/// Java `toString` of an attribute value (as `attr()` matches against).
pub fn java_string(v: &AttrValue) -> Option<String> {
    fn list<T: ToString>(l: &[T]) -> String {
        format!("[{}]", l.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(", "))
    }
    Some(match v {
        AttrValue::Int(i) => i.to_string(),
        AttrValue::Str(s) => s.clone(),
        AttrValue::Bool(b) => if *b { "1" } else { "0" }.to_string(),
        AttrValue::Tristate(t) => match *t {
            t if t < 0 => "-1".to_string(),
            0 => "0".to_string(),
            _ => "1".to_string(),
        },
        AttrValue::StrList(l) => list(l),
        AttrValue::IntList(l) => list(l),
        AttrValue::StrDict(d) => format!(
            "{{{}}}",
            d.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", ")
        ),
        AttrValue::StrListDict(d) => format!(
            "{{{}}}",
            d.iter().map(|(k, v)| format!("{k}={}", list(v))).collect::<Vec<_>>().join(", ")
        ),
        AttrValue::Label(l) | AttrValue::Output(l) => l.as_ref()?.to_string(),
        AttrValue::LabelList(l) | AttrValue::OutputList(l) => list(l),
        AttrValue::LabelKeyedStrDict(d) => format!(
            "{{{}}}",
            d.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", ")
        ),
        AttrValue::StrKeyedLabelDict(d) => format!(
            "{{{}}}",
            d.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", ")
        ),
        AttrValue::Select(_) => return None,
    })
}

/// `PossibleAttributeValues`-style enumeration: every value the attribute can take (one per
/// `select()` branch combination, approximated as the concatenation of per-part alternatives).
pub fn possible_values(v: &AttrValue) -> Vec<AttrValue> {
    match v {
        AttrValue::Select(parts) => {
            // List-typed selects: one value holding every branch's elements (a superset), like
            // `getConcatenatedSelectorListsOfListType` / `getReachableLabels`.
            let first = parts
                .iter()
                .find_map(|p| match p {
                    SelectPart::Plain(v) => Some(v.clone()),
                    SelectPart::Select { entries, .. } => entries.first().map(|(_, v)| v.clone()),
                })
                .unwrap_or(AttrValue::StrList(Vec::new()));
            let is_list = matches!(
                first,
                AttrValue::StrList(_) | AttrValue::LabelList(_) | AttrValue::OutputList(_) | AttrValue::IntList(_)
            );
            if is_list {
                let mut all: Vec<AttrValue> = Vec::new();
                for p in parts {
                    match p {
                        SelectPart::Plain(v) => all.push(v.clone()),
                        SelectPart::Select { entries, .. } => all.extend(entries.iter().map(|(_, v)| v.clone())),
                    }
                }
                return concat_values(&all)
                    .map(|v| match v {
                        AttrValue::LabelList(l) => {
                            let mut seen = Vec::new();
                            for x in l {
                                if !seen.contains(&x) {
                                    seen.push(x);
                                }
                            }
                            AttrValue::LabelList(seen)
                        }
                        other => other,
                    })
                    .into_iter()
                    .collect();
            }
            let mut combos: Vec<Vec<AttrValue>> = vec![Vec::new()];
            for p in parts {
                let alts: Vec<AttrValue> = match p {
                    SelectPart::Plain(v) => vec![v.clone()],
                    SelectPart::Select { entries, .. } => entries.iter().map(|(_, v)| v.clone()).collect(),
                };
                let mut next = Vec::new();
                for c in &combos {
                    for a in &alts {
                        let mut c2 = c.clone();
                        c2.push(a.clone());
                        next.push(c2);
                    }
                }
                combos = next;
            }
            let mut out = Vec::new();
            for c in combos {
                if let Some(v) = concat_values(&c) {
                    if !out.contains(&v) {
                        out.push(v);
                    }
                }
            }
            out
        }
        other => vec![other.clone()],
    }
}

fn concat_values(vs: &[AttrValue]) -> Option<AttrValue> {
    let mut it = vs.iter();
    let mut acc = it.next()?.clone();
    for v in it {
        acc = match (acc, v) {
            (AttrValue::StrList(mut a), AttrValue::StrList(b)) => {
                a.extend(b.iter().cloned());
                AttrValue::StrList(a)
            }
            (AttrValue::LabelList(mut a), AttrValue::LabelList(b)) => {
                a.extend(b.iter().cloned());
                AttrValue::LabelList(a)
            }
            (AttrValue::OutputList(mut a), AttrValue::OutputList(b)) => {
                a.extend(b.iter().cloned());
                AttrValue::OutputList(a)
            }
            (AttrValue::IntList(mut a), AttrValue::IntList(b)) => {
                a.extend(b.iter().cloned());
                AttrValue::IntList(a)
            }
            (AttrValue::StrDict(mut a), AttrValue::StrDict(b)) => {
                a.extend(b.iter().map(|(k, v)| (k.clone(), v.clone())));
                AttrValue::StrDict(a)
            }
            (AttrValue::StrListDict(mut a), AttrValue::StrListDict(b)) => {
                a.extend(b.iter().map(|(k, v)| (k.clone(), v.clone())));
                AttrValue::StrListDict(a)
            }
            (AttrValue::LabelKeyedStrDict(mut a), AttrValue::LabelKeyedStrDict(b)) => {
                a.extend(b.iter().cloned());
                AttrValue::LabelKeyedStrDict(a)
            }
            (_, b) => b.clone(),
        };
    }
    Some(acc)
}

/// Labels referenced by `visibility` that are real targets (`package_group`s).
pub fn visibility_group_labels(labels: &[Label]) -> Vec<Label> {
    labels
        .iter()
        .filter(|l| {
            let n = l.name();
            n != "__pkg__"
                && n != "__subpackages__"
                && !(l.package_identifier().package_fragment().path_string() == "visibility"
                    && (n == "public" || n == "private"))
        })
        .cloned()
        .collect()
}

/// Output labels of a rule: the `OutputFile` targets of the package it generates.
pub fn rule_outputs(pkg: &Package, rule: &Label) -> Vec<Label> {
    let mut v: Vec<Label> = pkg
        .targets
        .values()
        .filter_map(|t| match t {
            Target::OutputFile { label, generating_rule } if generating_rule == rule => Some(label.clone()),
            _ => None,
        })
        .collect();
    v.sort_by(crate::engine::label_cmp);
    v
}

/// All labels a rule depends on, with the attribute each one comes from, in attribute order.
/// `filter` mirrors `DependencyFilter` (`implicit`, `tool`).
pub fn rule_deps(rule: &RuleInstance, implicit: bool, tool: bool) -> Vec<(Label, String)> {
    rule_deps_ext(rule, implicit, tool, true)
}

pub fn rule_deps_ext(rule: &RuleInstance, implicit: bool, tool: bool, nodep: bool) -> Vec<(Label, String)> {
    let mut out: Vec<(Label, String)> = Vec::new();
    for a in rule_attributes(rule) {
        if matches!(a.ty, AttrType::Output | AttrType::OutputList) {
            continue;
        }
        if !implicit && !a.explicit {
            continue;
        }
        if !tool && a.tool {
            continue;
        }
        if !nodep && a.nodep {
            continue;
        }
        // `AggregatingAttributeMapper.visitLabels` never visits `NODEP_LABEL`/`NODEP_LABEL_LIST`
        // attributes (`build_setting_default`, `toolchain`, `transitive_configs`, ...): only the
        // visibility labels are visited, separately (`visitTargetVisibility`).
        if a.nodep && a.name != "visibility" {
            continue;
        }
        let labels = if a.name == "visibility" {
            visibility_group_labels(&a.value.labels())
        } else {
            a.value.labels()
        };
        for l in labels {
            if !out.iter().any(|(x, _)| *x == l) {
                out.push((l, a.name.clone()));
            }
        }
    }
    // `LabelVisitationUtils.visitRuleToolchains`: toolchain types of the rule class.
    for l in rule_class_toolchain_types(rule) {
        if !out.iter().any(|(x, _)| *x == l) {
            out.push((l, "_hidden_toolchain_types".to_string()));
        }
    }
    out
}

/// `RuleClass.getToolchainTypes()` of a Starlark rule (`rule(toolchains = [...])`).
pub fn rule_class_toolchain_types(rule: &RuleInstance) -> Vec<Label> {
    use zel_starlark::Value;
    let Some(spec) = rule.extra.rule_class.as_ref().and_then(|rc| rc.starlark.as_ref()) else { return Vec::new() };
    let Some(v) = spec.kwargs.get("toolchains") else { return Vec::new() };
    zel_starlark::eval::builtins::value_to_sequence(v)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|x| match &x {
            Value::Struct(s) if s.fields.contains_key("_toolchain_type") => match s.fields.get("name") {
                Some(Value::Label(l)) => Some(l.clone()),
                Some(Value::String(n)) => Label::parse_canonical(n).ok(),
                _ => None,
            },
            Value::Label(l) => Some(l.clone()),
            Value::String(s) => Label::parse_canonical(s).ok(),
            _ => None,
        })
        .collect()
}

pub fn aspect_deps(rule: &RuleInstance, tool: bool, nodep: bool) -> Vec<(Label, String)> {
    let mut out = Vec::new();
    let Some(rc) = &rule.extra.rule_class else { return out };
    for attr in rc.attrs.values() {
        for asp_v in &attr.aspects {
            if let Some(asp_def) = zel_packages::aspect::aspect_of(asp_v) {
                for asp_attr in &asp_def.attrs {
                    if !tool && asp_attr.cfg_exec {
                        continue;
                    }
                    let is_nodep = matches!(asp_attr.name.as_str(), "visibility" | "deprecation" | "licenses");
                    if !nodep && is_nodep {
                        continue;
                    }
                    if let Some(ref val) = asp_attr.default {
                        for l in val.labels() {
                            if !out.iter().any(|(x, _)| *x == l) {
                                out.push((l, asp_attr.name.clone()));
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

pub fn rule_deps_with_aspect_opt(
    rule: &RuleInstance,
    implicit: bool,
    tool: bool,
    nodep: bool,
    aspect_deps_mode: &str,
) -> Vec<(Label, String)> {
    let mut out = rule_deps_ext(rule, implicit, tool, nodep);
    if aspect_deps_mode != "off" {
        for (l, name) in aspect_deps(rule, tool, nodep) {
            if !out.iter().any(|(x, _)| *x == l) {
                out.push((l, name));
            }
        }
    }
    out
}

/// Whether the rule is a test rule (`TargetUtils.isTestRule`).
pub fn is_test_rule(rule: &RuleInstance) -> bool {
    match &rule.extra.rule_class {
        Some(rc) => rc.is_test && rc.name != "test_suite",
        None => rule.rule_class.ends_with("_test"),
    }
}

pub fn is_test_suite(rule: &RuleInstance) -> bool {
    rule.rule_class == "test_suite"
}

/// `TargetUtils.isExecutableNonTestRule`.
pub fn is_executable_non_test(rule: &RuleInstance) -> bool {
    let exec = rule.executable || rule.extra.rule_class.as_ref().map(|rc| rc.is_executable).unwrap_or(false);
    exec && !is_test_rule(rule)
}

/// Root of the source tree a package lives in (workspace root or external repo root).
pub fn package_root(pkg: &Package) -> PathFragment {
    let mut dir = pkg.build_file.parent_directory().unwrap_or_else(PathFragment::empty);
    for _ in 0..pkg.id.package_fragment().segments().len() {
        dir = dir.parent_directory().unwrap_or_else(PathFragment::empty);
    }
    dir
}

fn relativize(path: &str, root: &PathFragment) -> String {
    let r = root.path_string();
    match path.strip_prefix(r) {
        Some(rest) if rest.starts_with('/') && !r.is_empty() => rest[1..].to_string(),
        _ => path.to_string(),
    }
}

/// `FormatUtils.getLocation`.
pub fn target_location(info: &TInfo, relative: bool) -> String {
    let pkg = &info.pkg;
    let loc = match &info.target {
        Target::Rule(r) => r.extra.location.clone().unwrap_or_else(|| format!("{}:1:1", pkg.build_file)),
        Target::InputFile { label } => {
            let dir = pkg.build_file.parent_directory().unwrap_or_else(PathFragment::empty);
            format!("{}:1:1", dir.get_relative_str(label.name()))
        }
        t => pkg
            .target_locations
            .get(t.label().name())
            .cloned()
            .unwrap_or_else(|| format!("{}:1:1", pkg.build_file)),
    };
    if relative {
        // `file:line:col`
        if let Some(idx) = loc.find(':') {
            let (file, rest) = loc.split_at(idx);
            return format!("{}{}", relativize(file, &package_root(pkg)), rest);
        }
    }
    loc
}

/// Parses all `load()` statements of a `.bzl` source into canonical label strings.
fn parse_loads(path: &str, src: &str) -> Vec<String> {
    let mut lexer = zel_starlark::Lexer::new(path, src);
    let Ok(tokens) = lexer.lex_all() else { return Vec::new() };
    let mut parser = zel_starlark::Parser::new(tokens);
    let Ok(stmts) = parser.parse_module() else { return Vec::new() };
    stmts
        .into_iter()
        .filter_map(|s| match s {
            zel_starlark::syntax::ast::Stmt::Load(l, _) => Some(l.module_label),
            _ => None,
        })
        .collect()
}

/// The environment.
pub struct TargetEnv<'a> {
    pub resolver: &'a PatternResolver,
    pub opts: QueryOpts,
    cache: RefCell<HashMap<Label, Option<Rc<TInfo>>>>,
    /// Pretty-printed top-level expression (for error messages).
    pub toplevel: String,
    groups: RefCell<HashMap<Label, Rc<PackageGroupContents>>>,
    pending_events: RefCell<Vec<zel_packages::PackageEvent>>,
    bzl_loads: RefCell<HashMap<Label, Vec<Label>>>,
    /// Results of `preload_patterns`, keyed by the raw pattern.
    preloaded: RefCell<HashMap<String, Vec<Node>>>,
    /// Packages whose `package contains errors` events were already emitted.
    pkg_error_reported: RefCell<HashSet<String>>,
}

impl<'a> TargetEnv<'a> {
    pub fn new(resolver: &'a PatternResolver, opts: QueryOpts) -> Self {
        TargetEnv {
            resolver,
            opts,
            cache: RefCell::new(HashMap::new()),
            toplevel: String::new(),
            groups: RefCell::new(HashMap::new()),
            pending_events: RefCell::new(Vec::new()),
            bzl_loads: RefCell::new(HashMap::new()),
            preloaded: RefCell::new(HashMap::new()),
            pkg_error_reported: RefCell::new(HashSet::new()),
        }
    }

    fn fs_exists(&self, p: &PathFragment) -> bool {
        self.resolver.loader().config().lookup.fs.stat(p, true).is_ok()
    }

    /// `Package.getTarget` including the implicit BUILD-file target.
    pub fn info(&self, label: &Label) -> Result<Rc<TInfo>, String> {
        if let Some(c) = self.cache.borrow().get(label) {
            return match c {
                Some(i) => Ok(i.clone()),
                None => Err(self.missing_message(label)),
            };
        }
        let pkg = self.resolver.package(label.package_identifier()).map_err(|e| e.message)?;
        let fs = self.resolver.loader().config().lookup.fs.clone();
        let r = match pkg.get_target(label.name(), fs.as_ref()) {
            Ok(t) => Ok(t.clone()),
            Err(e) => {
                if label.name() == pkg.build_file.base_name() || label.name().ends_with(".bzl") {
                    Ok(Target::InputFile { label: label.clone() })
                } else if Self::assumed_input_file(&pkg, label) && self.resolver.loader().config().lookup.label_crossing_subpackage_error(label).is_none() {
                    Ok(Target::InputFile { label: label.clone() })
                } else {
                    Err(e)
                }
            }
        };
        match r {
            Ok(t) => {
                let info = Rc::new(TInfo { target: t, pkg });
                self.cache.borrow_mut().insert(label.clone(), Some(info.clone()));
                Ok(info)
            }
            Err(e) => Err(e),
        }
    }

    /// `Package.Builder.createAssumedInputFiles`: a label in a label-typed attribute of a rule
    /// that is not declared by a symbolic macro, pointing into the same package, becomes a source
    /// file target even if the file does not exist.
    fn assumed_input_file(pkg: &Package, label: &Label) -> bool {
        if label.package_identifier() != &pkg.id {
            return false;
        }
        for t in pkg.targets.values() {
            if let Target::Rule(r) = t {
                if r.extra.macro_instance.is_some() {
                    continue;
                }
                for (an, v) in &r.extra.attr_values {
                    let is_output = r
                        .extra
                        .rule_class
                        .as_ref()
                        .and_then(|rc| rc.attr(an))
                        .map(|a| matches!(a.ty, AttrType::Output | AttrType::OutputList))
                        .unwrap_or(false);
                    if !is_output && v.labels().iter().any(|l| l == label) {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn missing_message(&self, label: &Label) -> String {
        format!("no such target '{label}'")
    }

    fn info_of(&self, n: &Node) -> Option<Rc<TInfo>> {
        self.info(&n.label).ok()
    }

    /// Package group contents (transitively through `includes`).
    fn group_contents(&self, label: &Label) -> Option<Rc<PackageGroupContents>> {
        if let Some(g) = self.groups.borrow().get(label) {
            return Some(g.clone());
        }
        let mut seen: BTreeSet<Label> = BTreeSet::new();
        let mut stack = vec![label.clone()];
        let mut out = PackageGroupContents::default();
        while let Some(g) = stack.pop() {
            if !seen.insert(g.clone()) {
                continue;
            }
            let info = self.info(&g).ok()?;
            if let Target::PackageGroup { packages, .. } = &info.target {
                let mapping = RepositoryMapping::always_fallback();
                for p in packages {
                    if let Ok(spec) = PackageSpec::parse(&mapping, g.repository(), p) {
                        out.specs.push(spec);
                    }
                }
                if let Some(incl) = info.pkg.group_includes.get(g.name()) {
                    stack.extend(incl.iter().cloned());
                }
            }
        }
        let rc = Rc::new(out);
        self.groups.borrow_mut().insert(label.clone(), rc.clone());
        Some(rc)
    }

    /// Visibility specification labels of a target (`getVisibilityDeclaredLabels`).
    pub fn visibility_labels(&self, info: &TInfo) -> Vec<Label> {
        match &info.target {
            Target::Rule(r) => r.extra.effective_visibility.clone(),
            Target::InputFile { label } => info
                .pkg
                .exported_files
                .get(label.name())
                .cloned()
                .unwrap_or_else(|| {
                    // `InputFile.getVisibility`: the package default, else private.
                    let d = self.default_visibility(&info.pkg);
                    if d.is_empty() {
                        vec![Label::parse_canonical("//visibility:private").unwrap()]
                    } else {
                        d
                    }
                }),
            Target::OutputFile { generating_rule, .. } => match self.info(generating_rule) {
                Ok(i) => self.visibility_labels(&i),
                Err(_) => Vec::new(),
            },
            Target::PackageGroup { .. } | Target::EnvironmentGroup { .. } => vec![Label::parse_canonical("//visibility:public").unwrap()],
        }
    }

    fn default_visibility(&self, pkg: &Package) -> Vec<Label> {
        pkg.args.default_visibility.clone().unwrap_or_default()
    }

    /// Whether package `from_pkg` is allowed by the visibility labels (owned by `owner`).
    fn spec_contains(&self, vis: &[Label], owner: &PackageIdentifier, from: &PackageIdentifier) -> bool {
        if owner == from {
            return true;
        }
        for l in vis {
            let n = l.name();
            let pkgpath = l.package_identifier().package_fragment().path_string();
            // `//visibility:public` written in an external repository's BUILD file is the same
            // special label (`RuleVisibility.PUBLIC_LABEL` is parsed repository-independently).
            if pkgpath == "visibility" {
                if n == "public" {
                    return true;
                }
                continue;
            }
            if n == "__pkg__" {
                if l.package_identifier() == from {
                    return true;
                }
            } else if n == "__subpackages__" {
                if l.repository() == from.repository()
                    && from.package_fragment().starts_with(l.package_identifier().package_fragment())
                {
                    return true;
                }
            } else if let Some(g) = self.group_contents(l) {
                if g.contains(from) {
                    return true;
                }
            }
        }
        false
    }

    fn label_package_id(l: &Label) -> PackageIdentifier {
        l.package_identifier().clone()
    }

    /// Loads referenced by a package's BUILD file or bzl file, as labels.
    fn load_labels_of_package(&self, pkg: &Package) -> Vec<Label> {
        let mut out = Vec::new();
        for l in &pkg.loads {
            if let Some(label) = self.resolve_load_label(&l.module_label, &pkg.id) {
                if !out.contains(&label) {
                    out.push(label);
                }
            }
        }
        out
    }

    fn resolve_load_label(&self, raw: &str, pkg: &PackageIdentifier) -> Option<Label> {
        if raw.starts_with(':') {
            return Label::create(pkg.clone(), &raw[1..]).ok();
        }
        if !raw.starts_with('/') && !raw.starts_with('@') {
            return Label::create(pkg.clone(), raw).ok();
        }
        let mapping = self.resolver.loader().config().lookup.resolver.mapping(pkg.repository());
        Label::parse_with_repo_mapping(raw, &mapping).ok()
    }

    /// Source path of a `.bzl` label, if resolvable.
    fn bzl_path(&self, l: &Label) -> Option<PathFragment> {
        let lookup = &self.resolver.loader().config().lookup;
        let root = lookup.resolver.repo_root(l.repository())?;
        Some(root.get_relative(l.package_identifier().package_fragment()).get_relative_str(l.name()))
    }

    fn bzl_direct_loads(&self, l: &Label) -> Vec<Label> {
        if let Some(c) = self.bzl_loads.borrow().get(l) {
            return c.clone();
        }
        let mut out = Vec::new();
        if let Some(p) = self.bzl_path(l) {
            let fs = &self.resolver.loader().config().lookup.fs;
            if let Ok(bytes) = fs.read_file(&p) {
                let src = String::from_utf8_lossy(&bytes).to_string();
                for raw in parse_loads(p.path_string(), &src) {
                    if let Some(x) = self.resolve_load_label(&raw, l.package_identifier()) {
                        if !out.contains(&x) {
                            out.push(x);
                        }
                    }
                }
            }
        }
        self.bzl_loads.borrow_mut().insert(l.clone(), out.clone());
        out
    }

    /// Transitively loaded `.bzl` files of a package, in discovery order.
    fn transitive_bzl_loads(&self, pkg: &Package) -> Vec<Label> {
        let mut out: Vec<Label> = Vec::new();
        let mut stack: Vec<Label> = self.load_labels_of_package(pkg);
        stack.reverse();
        while let Some(l) = stack.pop() {
            if out.contains(&l) {
                continue;
            }
            out.push(l.clone());
            let mut sub = self.bzl_direct_loads(&l);
            sub.reverse();
            stack.extend(sub);
        }
        out
    }

    /// Transitively loaded `.bzl` files of the package's BUILD file (`computeBuildFileDependencies`).
    pub fn build_file_loads(&self, pkg: &Package) -> Vec<Label> {
        self.transitive_bzl_loads(pkg)
    }

    fn build_file_label(pkg: &Package) -> Label {
        Label::create(pkg.id.clone(), pkg.build_file.base_name()).unwrap()
    }

    fn universe_patterns(&self, expr_patterns: &[String]) -> Vec<String> {
        if !self.opts.universe_scope.is_empty() {
            self.opts.universe_scope.clone()
        } else {
            expr_patterns.to_vec()
        }
    }

    /// Resolves one pattern; `//pkg:BUILD` (the implicit BUILD-file target) is handled here
    /// because the pattern resolver only knows declared targets.
    fn resolve_one(&self, pattern: &str) -> (Vec<Node>, Vec<String>) {
        // `//dir/...`: packages with errors are reported (`package contains errors: <pkg>`) but do
        // not fail the pattern: the targets of all other packages stay in the result.
        if let Ok(zel_cmdline::TargetPattern::TargetsBelowDirectory { directory, include_files }) =
            zel_cmdline::TargetPattern::parse_with(
                pattern,
                &self.resolver.relative_dir,
                &RepositoryName::main(),
                &self.resolver.main_mapping,
            )
        {
            let mut pkgs = Vec::new();
            if let Err(m) = self.resolver.find_packages_below(&directory, &mut pkgs) {
                return (Vec::new(), vec![m]);
            }
            let mut nodes: Vec<Node> = Vec::new();
            let mut errors: Vec<String> = Vec::new();
            // Without `--keep_going`, a package that cannot be loaded at all (`NoSuchPackageException`,
            // e.g. a failing `load()`) aborts the whole `//dir/...` evaluation with
            // `error loading package under directory '<dir>': <message>`; the packages that merely
            // contain errors are then not reported (`RecursivePackageProviderBackedTargetPatternResolver`).
            if !self.opts.keep_going {
                let mut fatal: Option<String> = None;
                for id in &pkgs {
                    // Packages are loaded in parallel: every package's events are reported.
                    if let Err(e) = self.resolver.package(id) {
                        fatal.get_or_insert(e.message);
                    }
                }
                if let Some(m) = fatal {
                    let evs = self.resolver.drain_new_events();
                    self.pending_events.borrow_mut().extend(evs);
                    return (
                        Vec::new(),
                        vec![format!("error loading package under directory '{}': {m}", directory.package_fragment())],
                    );
                }
            }
            for id in pkgs {
                match self.resolver.package(&id) {
                    Ok(pkg) => {
                        let evs = self.resolver.drain_new_events();
                        self.pending_events.borrow_mut().extend(evs);
                        if pkg.contains_errors {
                            let frag = id.package_fragment().to_string();
                            let first = pkg
                                .events
                                .iter()
                                .find(|e| e.kind == zel_packages::lookup::EventKind::Error)
                                .map(|e| e.message.clone())
                                .unwrap_or_default();
                            if self.pkg_error_reported.borrow_mut().insert(frag.clone()) {
                                let mut pe = self.pending_events.borrow_mut();
                                if self.opts.keep_going || directory.package_fragment().is_empty() {
                                    pe.push(zel_packages::PackageEvent::error(None, format!("package contains errors: {frag}")));
                                }
                                pe.push(zel_packages::PackageEvent::error(None, format!("package contains errors: {frag}: {first}")));
                            }
                            let msg = format!("error loading package '{frag}': Package '{frag}' contains errors");
                            errors.push(if self.toplevel == pattern {
                                format!("\0Error evaluating '{pattern}': {msg}")
                            } else {
                                format!("\0Target parsing failed due to unexpected exception: {msg}")
                            });
                        }
                        nodes.extend(
                            zel_packages::PatternResolver::rules_or_all(&pkg, include_files).iter().map(|t| node(t.label())),
                        );
                    }
                    Err(e) => errors.push(e.message),
                }
            }
            return (nodes, errors);
        }
        let outcome = self.resolver.resolve_patterns(&[pattern.to_string()]);
        self.pending_events.borrow_mut().extend(outcome.events.iter().cloned());
        let mut nodes: Vec<Node> = outcome.targets.iter().map(|t| node(t.label())).collect();
        let mut errors: Vec<String> = Vec::new();
        for e in &outcome.errors {
            match &e.package_errors {
                // A package that loaded with errors is not a pattern failure in Bazel: it reports
                // `package contains errors: <pkg>: <first error>` and (without `--keep_going`)
                // fails the evaluation with an empty message (`describe` omits the colon).
                // The package's declared targets stay usable.
                Some(pe) => {
                    self.pending_events.borrow_mut().push(zel_packages::PackageEvent::error(
                        None,
                        format!("package contains errors: {}: {}", pe.package.package_fragment(), pe.first_error),
                    ));
                    if let Ok(pkg) = self.resolver.package(&pe.package) {
                        nodes.extend(
                            zel_packages::PatternResolver::rules_or_all(&pkg, pe.include_files)
                                .iter()
                                .map(|t| node(t.label())),
                        );
                    }
                    errors.push(String::new());
                }
                None => errors.push(e.message.clone()),
            }
        }
        if !errors.is_empty() {
            if let Ok(zel_cmdline::TargetPattern::SingleTarget(l)) =
                zel_cmdline::TargetPattern::parse_with(
                    pattern,
                    &self.resolver.relative_dir,
                    &RepositoryName::main(),
                    &self.resolver.main_mapping,
                )
            {
                if self.info(&l).is_ok() {
                    nodes.push(node(&l));
                    errors.clear();
                }
            }
        }
        (nodes, errors)
    }

    pub fn report_event(&self, ev: zel_packages::PackageEvent) {
        self.pending_events.borrow_mut().push(ev);
    }

    /// Events (errors, warnings, `print()` output) of every package loaded so far that have not
    /// been reported yet.
    pub fn take_events(&self) -> Vec<zel_packages::PackageEvent> {
        let mut v: Vec<_> = self.pending_events.borrow_mut().drain(..).collect();
        v.extend(self.resolver.drain_new_events());
        v
    }
}

/// A pattern error that was already reported through `package contains errors` events.
fn silent(e: &str) -> bool {
    e.is_empty() || e.starts_with('\0')
}

fn node(l: &Label) -> Node {
    Node::new(l.clone())
}

impl<'a> QueryEnv for TargetEnv<'a> {
    fn functions(&self) -> Vec<FuncSpec> {
        let mut f = parse::DEFAULT_FUNCTIONS.to_vec();
        if self.opts.sky {
            f.push(parse::ALLRDEPS);
            f.push(parse::RBUILDFILES);
        }
        f
    }

    fn preload_patterns(&mut self, patterns: &[String], diag: &mut Diag) -> Result<(), QueryError> {
        if self.opts.sky {
            return Ok(());
        }
        for p in patterns {
            if self.preloaded.borrow().contains_key(p) {
                continue;
            }
            let (nodes, errors) = self.resolve_one(p);
            if let Some(err) = errors.first() {
                if !diag.keep_going {
                    if let Some(raw) = err.strip_prefix('\0') {
                        return Err(QueryError::raw(raw.to_string()));
                    }
                    if err.is_empty() {
                        let mut qe = QueryError::new(String::new());
                        qe.expr = Some(self.toplevel.clone());
                        return Err(qe);
                    }
                    return Err(QueryError::raw(err.clone()));
                }
                // The error was already reported as `package contains errors` events.
                diag.had_error |= errors.iter().any(|e| silent(e));
                for e in errors.iter().filter(|e| !silent(e)) {
                    diag.report(format!("Skipping '{p}': {e}"));
                }
                let keep = if errors.iter().any(|e| silent(e)) { nodes } else { Vec::new() };
                self.preloaded.borrow_mut().insert(p.clone(), keep);
            } else {
                self.preloaded.borrow_mut().insert(p.clone(), nodes);
            }
        }
        Ok(())
    }

    fn resolve_pattern(&mut self, expr: &Expr, pattern: &str, diag: &mut Diag) -> Result<Vec<Node>, QueryError> {
        if let Some(n) = self.preloaded.borrow().get(pattern) {
            return Ok(n.clone());
        }
        let (nodes, errors) = self.resolve_one(pattern);
        if let Some(err) = errors.first() {
            if !diag.keep_going {
                // `SkyQueryEnvironment` fails while building the universe; the legacy
                // environments fail while evaluating the target literal itself.
                if self.opts.sky {
                    let mut qe = QueryError::new(format!("Building universe scope failed: {err}"));
                    qe.expr = Some(self.toplevel.clone());
                    return Err(qe);
                }
                return Err(QueryError::for_expr(expr, err.clone()));
            }
            diag.had_error |= errors.iter().any(|e| silent(e));
            for e in errors.iter().filter(|e| !silent(e)) {
                diag.report(e.to_string());
            }
        }
        Ok(nodes)
    }

    fn location_prefix(&mut self, n: &Node) -> String {
        match self.info_of(n).as_deref().map(|i| &i.target) {
            Some(Target::Rule(r)) => r.extra.location.clone().map(|x| format!("{x}: ")).unwrap_or_default(),
            _ => String::new(),
        }
    }

    fn direct_deps(&mut self, expr: &Expr, n: &Node, diag: &mut Diag) -> Result<Vec<Node>, QueryError> {
        let info = match self.info(&n.label) {
            Ok(i) => i,
            Err(_) => return Ok(Vec::new()),
        };
        let mut raw: Vec<(Label, Option<Label>)> = Vec::new();
        match &info.target {
            Target::Rule(r) => {
                for (l, _) in rule_deps_with_aspect_opt(r, self.opts.implicit_deps, self.opts.tool_deps, self.opts.nodep_deps, &self.opts.aspect_deps) {
                    raw.push((l, Some(r.label.clone())));
                }
            }
            Target::OutputFile { generating_rule, .. } => raw.push((generating_rule.clone(), None)),
            Target::InputFile { label } => {
                if self.opts.nodep_deps {
                    for l in visibility_group_labels(&self.visibility_labels(&info)) {
                        raw.push((l, Some(label.clone())));
                    }
                }
            }
            Target::PackageGroup { label, .. } => {
                if let Some(incl) = info.pkg.group_includes.get(label.name()) {
                    for l in incl {
                        raw.push((l.clone(), Some(label.clone())));
                    }
                }
            }
            // An environment group has no dependency edges (`EnvironmentGroup` is a leaf target).
            Target::EnvironmentGroup { .. } => {}
        }
        let mut out = Vec::new();
        for (l, referrer) in raw {
            match self.info(&l) {
                Ok(_) => out.push(node(&l)),
                Err(msg) => {
                    let loc = match &info.target {
                        Target::Rule(r) => r.extra.location.clone().map(|x| format!("{x}: ")).unwrap_or_default(),
                        _ => String::new(),
                    };
                    let _ = referrer;
                    diag.edge_errs.push(crate::engine::EdgeErr {
                        from: n.clone(),
                        dep: node(&l),
                        loc,
                        msg: msg.clone(),
                    });
                    diag.closure_error = true;
                    let _ = expr;
                }
            }
        }
        Ok(out)
    }

    fn universe_roots(&mut self, _expr: &Expr, patterns: &[String], diag: &mut Diag) -> Result<Vec<Node>, QueryError> {
        let pats = self.universe_patterns(patterns);
        let mut roots = Vec::new();
        if pats.is_empty() {
            return Ok(roots);
        }
        for p in &pats {
            let (nodes, errors) = self.resolve_one(p);
            if let Some(err) = errors.first() {
                if !diag.keep_going {
                    let mut qe = QueryError::new(format!("Building universe scope failed: {err}"));
                    qe.expr = Some(self.toplevel.clone());
                    return Err(qe);
                }
            }
            for n in nodes {
                if !roots.contains(&n) {
                    roots.push(n);
                }
            }
        }
        Ok(roots)
    }

    fn target_kind(&mut self, n: &Node) -> String {
        match self.info_of(n).map(|i| i.target.clone()) {
            Some(t) => t.target_kind(),
            None => "unknown".to_string(),
        }
    }

    fn is_rule(&mut self, n: &Node) -> bool {
        matches!(self.info_of(n).as_deref().map(|i| &i.target), Some(Target::Rule(_)))
    }

    fn is_test_rule(&mut self, n: &Node) -> bool {
        match self.info_of(n).as_deref().map(|i| &i.target) {
            Some(Target::Rule(r)) => is_test_rule(r),
            _ => false,
        }
    }

    fn is_test_suite(&mut self, n: &Node) -> bool {
        match self.info_of(n).as_deref().map(|i| &i.target) {
            Some(Target::Rule(r)) => is_test_suite(r),
            _ => false,
        }
    }

    fn is_executable_non_test_rule(&mut self, n: &Node) -> bool {
        match self.info_of(n).as_deref().map(|i| &i.target) {
            Some(Target::Rule(r)) => is_executable_non_test(r),
            _ => false,
        }
    }

    fn package_name(&mut self, n: &Node) -> String {
        n.label.package_name().to_string()
    }

    fn attr_strings(&mut self, n: &Node, attr: &str) -> Vec<String> {
        let Some(info) = self.info_of(n) else { return Vec::new() };
        let Target::Rule(r) = &info.target else { return Vec::new() };
        let Some(a) = rule_attributes(r).into_iter().find(|a| a.name == attr) else {
            return Vec::new();
        };
        possible_values(&a.value).iter().filter_map(java_string).collect()
    }

    fn string_list_attr(&mut self, n: &Node, attr: &str) -> Vec<String> {
        let Some(info) = self.info_of(n) else { return Vec::new() };
        let Target::Rule(r) = &info.target else { return Vec::new() };
        match r.attr_or_default(attr) {
            Some(AttrValue::StrList(l)) => l,
            Some(v @ AttrValue::Select(_)) => possible_values(&v)
                .into_iter()
                .flat_map(|v| if let AttrValue::StrList(l) = v { l } else { Vec::new() })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn prerequisites(
        &mut self,
        expr: &Expr,
        n: &Node,
        attr: &str,
        err_prefix: &str,
        diag: &mut Diag,
    ) -> Result<Vec<Node>, QueryError> {
        let Some(info) = self.info_of(n) else { return Ok(Vec::new()) };
        let Target::Rule(r) = &info.target else { return Ok(Vec::new()) };
        let entries = rule_attributes(r);
        let Some(a) = entries.iter().find(|a| a.name == attr) else { return Ok(Vec::new()) };
        let mut labels = if a.name == "visibility" {
            visibility_group_labels(&a.value.labels())
        } else {
            // `labels()` does not include the condition keys of `select()`s.
            fn no_keys(v: &AttrValue) -> Vec<Label> {
                match v {
                    AttrValue::Select(parts) => {
                        let mut out = Vec::new();
                        for p in parts {
                            match p {
                                SelectPart::Plain(v) => out.extend(no_keys(v)),
                                SelectPart::Select { entries, .. } => {
                                    for (_, v) in entries {
                                        out.extend(no_keys(v));
                                    }
                                }
                            }
                        }
                        out
                    }
                    other => other.labels(),
                }
            }
            match &a.value {
                // `labels(outs, ...)`: the declared output files.
                AttrValue::OutputList(l) => l.clone(),
                AttrValue::Output(Some(l)) => vec![l.clone()],
                v => no_keys(v),
            }
        };
        if attr == "tests" && is_test_suite(r) && labels.is_empty() {
            // `$implicit_tests`: all non-manual test rules of the package.
            labels = info
                .pkg
                .targets
                .values()
                .filter_map(|t| match t {
                    Target::Rule(x) if is_test_rule(x) => {
                        let manual = matches!(x.attr_or_default("tags"), Some(AttrValue::StrList(t)) if t.iter().any(|s| s == "manual"));
                        if manual {
                            None
                        } else {
                            Some(x.label.clone())
                        }
                    }
                    _ => None,
                })
                .collect();
        }
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for l in labels {
            if !seen.insert(l.clone()) {
                continue;
            }
            match self.info(&l) {
                Ok(_) => out.push(node(&l)),
                Err(msg) => diag.handle_error(expr, format!("{err_prefix}{msg}"))?,
            }
        }
        Ok(out)
    }

    fn siblings(&mut self, n: &Node, _diag: &mut Diag) -> Result<Vec<Node>, QueryError> {
        let Some(info) = self.info_of(n) else { return Ok(Vec::new()) };
        let mut out: Vec<Node> = info
            .pkg
            .targets
            .values()
            .map(|t| node(t.label()))
            .collect();
        let bf = Self::build_file_label(&info.pkg);
        if !out.iter().any(|x| x.label == bf) {
            out.push(node(&bf));
        }
        Ok(out)
    }

    fn visible(&mut self, _expr: &Expr, to: &Node, from: &Node, _diag: &mut Diag) -> Result<bool, QueryError> {
        let Some(info) = self.info_of(from) else { return Ok(false) };
        let vis = self.visibility_labels(&info);
        let owner = Self::label_package_id(&from.label);
        let to_pkg = Self::label_package_id(&to.label);
        Ok(self.spec_contains(&vis, &owner, &to_pkg))
    }

    fn buildfiles(&mut self, _expr: &Expr, set: &NodeSet, _diag: &mut Diag) -> Result<Vec<Node>, QueryError> {
        let mut out = Vec::new();
        let mut pkgs: Vec<Arc<Package>> = Vec::new();
        for n in set.iter() {
            if let Some(info) = self.info_of(n) {
                if !pkgs.iter().any(|p| p.id == info.pkg.id) {
                    pkgs.push(info.pkg.clone());
                }
                // Also the BUILD files of packages whose package groups are referenced.
                if let Target::Rule(r) = &info.target {
                    for l in visibility_group_labels(&r.extra.effective_visibility) {
                        if let Ok(gi) = self.info(&l) {
                            if !pkgs.iter().any(|p| p.id == gi.pkg.id) {
                                pkgs.push(gi.pkg.clone());
                            }
                        }
                    }
                }
            }
        }
        for p in pkgs {
            let bfn = node(&Self::build_file_label(&p));
            if !out.contains(&bfn) {
                out.push(bfn);
            }
            for l in self.transitive_bzl_loads(&p) {
                let ln = node(&l);
                if !out.contains(&ln) {
                    out.push(ln);
                }
                if let Ok(bzl_pkg) = self.resolver.package(l.package_identifier()) {
                    let bfl = node(&Self::build_file_label(&bzl_pkg));
                    if !out.contains(&bfl) {
                        out.push(bfl);
                    }
                }
            }
        }
        Ok(out)
    }

    fn loadfiles(&mut self, _expr: &Expr, set: &NodeSet, _diag: &mut Diag) -> Result<Vec<Node>, QueryError> {
        let mut out = Vec::new();
        let mut seen_pkgs = HashSet::new();
        for n in set.iter() {
            if let Some(info) = self.info_of(n) {
                if seen_pkgs.insert(info.pkg.id.clone()) {
                    for l in self.transitive_bzl_loads(&info.pkg) {
                        out.push(node(&l));
                    }
                }
            }
        }
        Ok(out)
    }

    fn rbuildfiles(&mut self, expr: &Expr, words: &[String], diag: &mut Diag) -> Result<Vec<Node>, QueryError> {
        // Resolve each word to a file path / label.
        let mut files: Vec<Label> = Vec::new();
        for w in words {
            if let Ok(pat) = zel_cmdline::TargetPattern::parse(w, &PathFragment::empty()) {
                if let zel_cmdline::TargetPattern::SingleTarget(l) = pat {
                    files.push(l);
                    continue;
                }
            }
            let frag = PathFragment::create(w);
            let id = PackageIdentifier::create(RepositoryName::main(), frag.parent_directory().unwrap_or_else(PathFragment::empty));
            if let Ok(id) = id {
                if let Ok(l) = Label::create(id, frag.base_name()) {
                    files.push(l);
                }
            }
        }
        if files.is_empty() {
            return Ok(Vec::new());
        }
        // Universe: every package below the universe patterns.
        let pats = self.universe_patterns(&["//...".to_string()]);
        let roots = self.universe_roots(expr, &pats, diag)?;
        let mut pkgs: Vec<Arc<Package>> = Vec::new();
        let mut seen = HashSet::new();
        for r in &roots {
            if let Some(i) = self.info_of(r) {
                if seen.insert(i.pkg.id.clone()) {
                    pkgs.push(i.pkg.clone());
                }
            }
        }
        let mut out = Vec::new();
        for p in pkgs {
            let bf = Self::build_file_label(&p);
            let loads = self.transitive_bzl_loads(&p);
            if files.contains(&bf) || files.iter().any(|f| loads.contains(f)) {
                out.push(node(&bf));
            }
        }
        Ok(out)
    }

    fn strict_test_suite(&self) -> bool {
        self.opts.strict_test_suite
    }

    fn strict_universe(&self) -> bool {
        self.opts.sky
    }
}

/// `--query_file`/residue joining and other command-line helpers live in the server.
pub fn sorted_labels(set: &[Node]) -> Vec<Node> {
    let mut v = set.to_vec();
    v.sort_by(|a, b| crate::engine::label_cmp(&a.label, &b.label));
    v
}

/// Adds package-level information used by the `package` and `location` formatters.
pub fn package_display(l: &Label) -> String {
    let id = l.package_identifier().canonical_form();
    id.strip_prefix("//").map(|s| s.to_string()).unwrap_or(id)
}

#[allow(dead_code)]
fn _unused(_: BTreeMap<String, String>, _: Arg) {}
