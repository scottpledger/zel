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

//! `RuleClass` and the registry of native rule classes (`RuleClass.java`,
//! `BazelRuleClassProvider`).

use crate::attr::{AttrType, AttrValue, Attribute};
use crate::rule_defs_gen::{GenAttr, D, RULE_DEFS, USER_ATTRS};
use std::collections::BTreeMap;
use std::sync::Arc;
use zel_cmdline::Label;
use zel_starlark::Value;

/// A rule class: the attribute schema and flags of a rule kind (`cc_library`, a Starlark `rule()`,
/// ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleClass {
    pub name: String,
    pub attrs: BTreeMap<String, Attribute>,
    /// Bazel's attribute declaration order (`Rule.getAttributes()`), including the hidden
    /// `$`/`:` attributes of [`RuleClass::implicit`].
    pub attr_order: Vec<String>,
    /// Hidden implicit attributes (`$test_wrapper`, `:action_listener`, ...) generated from real
    /// Bazel's rule definitions (`rule_defs_gen`). They exist for `query` (output, implicit deps,
    /// `--notool_deps`) only: analysis works on [`RuleClass::attrs`].
    pub implicit: BTreeMap<String, Attribute>,
    pub is_test: bool,
    pub is_executable: bool,
    /// For rule classes whose attribute schema is only partially modeled, attributes that are not
    /// in the schema are accepted and typed by inference instead of rejected.
    pub open: bool,
    /// Implicit output templates (`"%{name}.txt"`), keyed by the Starlark `outputs` dict key.
    pub implicit_outputs: Vec<(String, String)>,
    pub is_starlark: bool,
    /// `rule(..., build_setting = ...)`.
    pub build_setting: bool,
    /// Starlark `rule()` definition (implementation function and the remaining parameters).
    pub starlark: Option<Arc<StarlarkRuleSpec>>,
}

/// The parts of a Starlark `rule(...)` call needed at analysis time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StarlarkRuleSpec {
    /// The `implementation` callable.
    pub implementation: Value,
    /// Remaining `rule()` keyword arguments (`toolchains`, `provides`, `fragments`, `cfg`,
    /// `exec_groups`, `exec_compatible_with`, `output_to_genfiles`, `analysis_test`,
    /// `build_setting`, ...), unparsed.
    pub kwargs: BTreeMap<String, Value>,
    /// Label of the `.bzl` file that defined the rule (as `//pkg:file.bzl`).
    pub defined_in: String,
    /// Hex digest of the defining `.bzl` and its transitive loads
    /// (`RuleClass.getRuleDefinitionEnvironmentDigest`, query's `$rule_implementation_hash`).
    pub definition_digest: String,
    /// Instantiation/definition stack of the rule definition itself (`file:line:col in fn`).
    pub definition_stack: Vec<(String, String)>,
}

impl RuleClass {
    pub fn attr(&self, name: &str) -> Option<&Attribute> {
        self.attrs.get(name)
    }

    /// An attribute of the schema, including the hidden implicit ones.
    pub fn attr_or_implicit(&self, name: &str) -> Option<&Attribute> {
        self.attrs.get(name).or_else(|| self.implicit.get(name))
    }
}

fn sl() -> AttrType {
    AttrType::StringList
}
fn ll() -> AttrType {
    AttrType::LabelList
}

fn common_attrs() -> Vec<Attribute> {
    vec![
        Attribute::new("name", AttrType::String).mandatory().nonconfigurable(),
        Attribute::new("visibility", AttrType::LabelList).nonconfigurable(),
        Attribute::new("tags", sl()).nonconfigurable(),
        Attribute::new("deprecation", AttrType::String).nonconfigurable(),
        Attribute::new("testonly", AttrType::Bool).nonconfigurable(),
        Attribute::new("features", sl()),
        Attribute::new("compatible_with", ll()).nonconfigurable(),
        Attribute::new("restricted_to", ll()).nonconfigurable(),
        Attribute::new("target_compatible_with", ll()),
        Attribute::new("exec_compatible_with", ll()).nonconfigurable(),
        Attribute::new("exec_properties", AttrType::StringDict),
        Attribute::new("toolchains", ll()),
        Attribute::new("aspect_hints", ll()).nonconfigurable(),
        Attribute::new("package_metadata", ll()).nonconfigurable(),
        Attribute::new("applicable_licenses", ll()).nonconfigurable(),
        Attribute::new("licenses", sl()).nonconfigurable(),
        Attribute::new("distribs", sl()).nonconfigurable(),
        Attribute::new("generator_name", AttrType::String).nonconfigurable(),
        Attribute::new("generator_function", AttrType::String).nonconfigurable(),
        Attribute::new("generator_location", AttrType::String).nonconfigurable(),
    ]
}

fn test_attrs() -> Vec<Attribute> {
    vec![
        Attribute::new("size", AttrType::String)
            .default(AttrValue::Str("medium".into()))
            .nonconfigurable(),
        Attribute::new("timeout", AttrType::String).nonconfigurable(),
        Attribute::new("flaky", AttrType::Bool).nonconfigurable(),
        Attribute::new("shard_count", AttrType::Int)
            .default(AttrValue::Int(-1))
            .nonconfigurable(),
        Attribute::new("local", AttrType::Bool).nonconfigurable(),
        Attribute::new("args", sl()),
        Attribute::new("env", AttrType::StringDict),
        Attribute::new("env_inherit", sl()),
    ]
}

fn binary_attrs() -> Vec<Attribute> {
    vec![
        Attribute::new("args", sl()),
        Attribute::new("env", AttrType::StringDict),
        Attribute::new("output_licenses", sl()).nonconfigurable(),
    ]
}

struct Builder {
    rc: RuleClass,
}

impl Builder {
    fn new(name: &str) -> Self {
        let mut attrs = BTreeMap::new();
        let mut attr_order = Vec::new();
        for a in common_attrs() {
            attr_order.push(a.name.clone());
            attrs.insert(a.name.clone(), a);
        }
        Self {
            rc: RuleClass {
                name: name.to_string(),
                attrs,
                attr_order,
                implicit: BTreeMap::new(),
                is_test: false,
                is_executable: false,
                open: false,
                implicit_outputs: Vec::new(),
                is_starlark: false,
                build_setting: false,
                starlark: None,
            },
        }
    }
    fn attr(mut self, a: Attribute) -> Self {
        if !self.rc.attr_order.contains(&a.name) {
            self.rc.attr_order.push(a.name.clone());
        }
        self.rc.attrs.insert(a.name.clone(), a);
        self
    }
    fn attrs(mut self, v: Vec<Attribute>) -> Self {
        for a in v {
            if !self.rc.attr_order.contains(&a.name) {
                self.rc.attr_order.push(a.name.clone());
            }
            self.rc.attrs.insert(a.name.clone(), a);
        }
        self
    }
    fn test(mut self) -> Self {
        self.rc.is_test = true;
        self.rc.is_executable = true;
        self.attrs(test_attrs())
    }
    fn binary(mut self) -> Self {
        self.rc.is_executable = true;
        self.attrs(binary_attrs())
    }
    fn open(mut self) -> Self {
        self.rc.open = true;
        self
    }
    fn build(self) -> Arc<RuleClass> {
        Arc::new(self.rc)
    }
}


/// The `AttrType` zel uses for a Bazel attribute discriminator name.
fn attr_type_of(proto: &str, nodep: bool) -> AttrType {
    match proto {
        // `NODEP_LABEL` (`build_setting_default`, `toolchain`, ...) prints as STRING in proto but is a
        // label for everything else (xml `<label>`, `rule-input`).
        "STRING" if nodep => AttrType::Label,
        "STRING" => AttrType::String,
        "INTEGER" => AttrType::Int,
        "BOOLEAN" => AttrType::Bool,
        "TRISTATE" => AttrType::Tristate,
        // `NODEP_LABEL_LIST` (visibility, transitive_configs) prints as STRING_LIST.
        "STRING_LIST" if nodep => AttrType::LabelList,
        "STRING_LIST" | "DISTRIBUTION_SET" | "LICENSE" => AttrType::StringList,
        "LABEL_LIST" => AttrType::LabelList,
        "LABEL" => AttrType::Label,
        "OUTPUT" => AttrType::Output,
        "OUTPUT_LIST" => AttrType::OutputList,
        "STRING_DICT" => AttrType::StringDict,
        "STRING_LIST_DICT" | "LABEL_LIST_DICT" => AttrType::StringListDict,
        "LABEL_KEYED_STRING_DICT" => AttrType::LabelKeyedStringDict,
        "LABEL_DICT_UNARY" => AttrType::StringKeyedLabelDict,
        "INTEGER_LIST" => AttrType::IntList,
        _ => AttrType::String,
    }
}

fn parse_gen_label(s: &str) -> Option<Label> {
    Label::parse_canonical(s).ok()
}

/// The default value of a generated attribute, converted to the attribute's type.
fn gen_default(d: &D, ty: AttrType) -> Option<AttrValue> {
    Some(match (d, ty) {
        (D::None, _) => return None,
        (D::Str(s), AttrType::String) => AttrValue::Str((*s).to_string()),
        (D::Int(i), AttrType::Int) => AttrValue::Int(*i),
        (D::Bool(b), AttrType::Bool) => AttrValue::Bool(*b),
        (D::Tristate(t), AttrType::Tristate) => AttrValue::Tristate(*t),
        (D::Label(s), AttrType::Label) => AttrValue::Label(parse_gen_label(s)),
        (D::List(l), AttrType::StringList) => AttrValue::StrList(l.iter().map(|s| s.to_string()).collect()),
        (D::List(l), AttrType::LabelList) => AttrValue::LabelList(l.iter().filter_map(|s| parse_gen_label(s)).collect()),
        (D::Dict(d), AttrType::StringDict) => {
            AttrValue::StrDict(d.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
        }
        // `License.NONE` is the default of `licenses`; zel stores the license types as strings.
        (D::License(_), _) => return None,
        _ => return None,
    })
}

/// Makes `rc`'s attribute schema match Bazel's: attributes follow the generated declaration order
/// (`attr_order`), attributes Bazel does not define are dropped (unless the class is `open`), and
/// the generated type/nodep/configurability/tool/default information is applied. `user` attributes
/// of Starlark rules are placed where Bazel puts them (after the generated ones).
fn apply_gen(rc: &mut RuleClass, defs: &[GenAttr], user: Vec<Attribute>) {
    let mut old = std::mem::take(&mut rc.attrs);
    let mut user_by_name: BTreeMap<String, Attribute> = BTreeMap::new();
    let mut user_order: Vec<String> = Vec::new();
    for u in user {
        user_order.push(u.name.clone());
        user_by_name.insert(u.name.clone(), u);
    }
    rc.implicit.clear();
    let mut order: Vec<String> = vec!["name".to_string()];
    if let Some(n) = old.remove("name").or_else(|| user_by_name.remove("name")) {
        rc.attrs.insert("name".to_string(), n);
    } else {
        rc.attrs.insert("name".to_string(), Attribute::new("name", AttrType::String).mandatory().nonconfigurable());
    }
    let mut place_user = |rc: &mut RuleClass, order: &mut Vec<String>, user_by_name: &mut BTreeMap<String, Attribute>| {
        for n in &user_order {
            if let Some(a) = user_by_name.remove(n) {
                order.push(n.clone());
                rc.attrs.insert(n.clone(), a);
            }
        }
    };
    for g in defs {
        if g.name == USER_ATTRS {
            place_user(rc, &mut order, &mut user_by_name);
            continue;
        }
        let mut from_user = user_by_name.remove(g.name);
        let mut placed = false;
        if from_user.is_none() {
            // Already placed with the user attributes (the loader adds the typed
            // `build_setting_default`/`help` of build settings there): Bazel appends them after
            // the user attributes, in `RuleClass.Builder.build()`.
            if let Some(placed_attr) = rc.attrs.remove(g.name) {
                order.retain(|n| n != g.name);
                from_user = Some(placed_attr);
                placed = true;
            }
        }
        let user_defined = from_user.is_some() && !placed;
        let mut a = from_user
            .or_else(|| old.remove(g.name))
            .unwrap_or_else(|| Attribute::new(g.name, attr_type_of(g.ty, g.nodep)));
        let want = attr_type_of(g.ty, g.nodep);
        if !user_defined && !placed && a.ty != want {
            a.ty = want;
            a.default = None;
        }
        a.configurable = g.configurable && (user_defined && a.configurable || !user_defined);
        a.nodep = g.nodep;
        a.nodep_set = Some(g.nodep_set);
        a.tool_dep = Some(g.tool);
        a.proto_type = Some(g.ty);
        a.order_independent = g.ordered;
        if let D::RuleBased(src) = &g.default {
            a.rule_based_default = Some((*src).to_string());
        }
        if !user_defined && (!placed || a.ty == want) {
            a.mandatory = a.mandatory || g.mandatory;
            if let Some(d) = gen_default(&g.default, a.ty) {
                a.default = Some(d);
            }
        }
        order.push(g.name.to_string());
        if g.name.starts_with('$') || g.name.starts_with(':') {
            rc.implicit.insert(g.name.to_string(), a);
        } else {
            rc.attrs.insert(g.name.to_string(), a);
        }
    }
    place_user(rc, &mut order, &mut user_by_name);
    if rc.open {
        for (n, a) in old {
            order.push(n.clone());
            rc.attrs.insert(n, a);
        }
    }
    rc.attr_order = order;
}

/// The generated definition of a rule kind (`genrule`, `@starlark:test`, ...).
pub fn generated_defs(kind: &str) -> Option<&'static [GenAttr]> {
    RULE_DEFS.iter().find(|(k, _)| *k == kind).map(|(_, d)| *d)
}

fn a(name: &str, ty: AttrType) -> Attribute {
    Attribute::new(name, ty)
}

/// The native rule classes known to the loader (a subset of `BazelRuleClassProvider`).
///
/// Core rules (`genrule`, `filegroup`, `alias`, `test_suite`, `config_setting`, `genquery`,
/// platform/constraint rules) have complete schemas. For the language rule families
/// (`cc_*`, `java_*`, `py_*`, `sh_*`, `proto_library`) the principal attributes are modeled and the
/// class is marked `open` (see [`RuleClass::open`]).
pub fn native_rule_classes() -> BTreeMap<String, Arc<RuleClass>> {
    let mut v: Vec<Arc<RuleClass>> = Vec::new();

    v.push(
        Builder::new("filegroup")
            .attr(a("srcs", ll()))
            .attr(a("data", ll()))
            .attr(a("output_group", AttrType::String).nonconfigurable())
            .build(),
    );
    let genrule_setup = Label::parse_canonical("@bazel_tools//tools/genrule:genrule-setup.sh")
        .unwrap_or_else(|_| Label::parse_canonical("@@bazel_tools//tools/genrule:genrule-setup.sh").unwrap());
    v.push(
        Builder::new("genrule")
            .attr(a("srcs", ll()))
            .attr(a("tools", ll()).cfg_exec())
            .attr(a("outs", AttrType::OutputList).mandatory().non_empty().nonconfigurable())
            .attr(a("cmd", AttrType::String))
            .attr(a("cmd_bash", AttrType::String))
            .attr(a("cmd_ps", AttrType::String))
            .attr(a("cmd_bat", AttrType::String))
            .attr(a("exec_tools", ll()).cfg_exec())
            .attr(a("executable", AttrType::Bool).nonconfigurable())
            .attr(a("local", AttrType::Bool).nonconfigurable())
            .attr(a("message", AttrType::String).nonconfigurable())
            .attr(a("output_licenses", sl()).nonconfigurable())
            .attr(a("output_to_bindir", AttrType::Bool).nonconfigurable())
            .attr(a("stamp", AttrType::Bool).nonconfigurable())
            .attr(Attribute::new("$genrule_setup", AttrType::Label).default(AttrValue::Label(Some(genrule_setup))).cfg_exec())
            .build(),
    );
    v.push(
        Builder::new("alias")
            .attr(a("actual", AttrType::Label).mandatory())
            .build(),
    );
    v.push(
        Builder::new("test_suite")
            .attr(a("tests", ll()).nonconfigurable())
            .attr(a("tests_to_run", ll()).nonconfigurable())
            .build(),
    );
    v.push(
        Builder::new("config_setting")
            .attr(a("values", AttrType::StringDict).nonconfigurable())
            .attr(a("define_values", AttrType::StringDict).nonconfigurable())
            .attr(a("flag_values", AttrType::LabelKeyedStringDict).nonconfigurable())
            .attr(a("constraint_values", ll()).nonconfigurable())
            .build(),
    );
    v.push(
        Builder::new("genquery")
            .attr(a("expression", AttrType::String).mandatory().nonconfigurable())
            .attr(a("scope", ll()).mandatory().non_empty().nonconfigurable())
            .attr(a("strict", AttrType::Bool).default(AttrValue::Bool(true)).nonconfigurable())
            .attr(a("opts", sl()).nonconfigurable())
            .attr(a("compressed_output", AttrType::Bool).nonconfigurable())
            .build(),
    );
    // Platforms & toolchains.
    v.push(Builder::new("constraint_setting")
        .attr(a("default_constraint_value", AttrType::Label).nonconfigurable()).build());
    v.push(Builder::new("constraint_value")
        .attr(a("constraint_setting", AttrType::Label).mandatory().nonconfigurable()).build());
    v.push(Builder::new("platform")
        .attr(a("constraint_values", ll()).nonconfigurable())
        .attr(a("parents", ll()).nonconfigurable())
        .attr(a("remote_execution_properties", AttrType::String).nonconfigurable())
        .attr(a("exec_properties", AttrType::StringDict).nonconfigurable())
        .attr(a("flags", sl()).nonconfigurable()).build());
    v.push(Builder::new("toolchain_type").attr(a("no_match_error", AttrType::String).nonconfigurable()).build());
    v.push(Builder::new("toolchain")
        .attr(a("toolchain_type", AttrType::Label).mandatory().nonconfigurable())
        .attr(a("toolchain", AttrType::Label).mandatory())
        .attr(a("exec_compatible_with", ll()).nonconfigurable())
        .attr(a("target_compatible_with", ll()).nonconfigurable())
        .attr(a("target_settings", ll()))
        .attr(a("use_target_platform_constraints", AttrType::Bool).nonconfigurable()).build());
    v.push(Builder::new("environment").build());
    // Native rule classes of `GenericRules`/`JavaRules` that only have a generated schema
    // (`rule_defs_gen`, derived from the Java rule definitions by `scripts/gen_rule_defs.py`).
    for native in ["extra_action", "action_listener", "starlark_doc_extract", "cc_toolchain_alias"] {
        let mut b = Builder::new(native);
        if native == "starlark_doc_extract" {
            // `StarlarkDocExtractRule`: `fromFunctions(BINARYPROTO_OUT, TEXTPROTO_OUT)`.
            b.rc.implicit_outputs = vec![
                ("binaryproto".to_string(), "%{name}.binaryproto".to_string()),
                ("textproto".to_string(), "%{name}.textproto".to_string()),
            ];
        }
        v.push(b.build());
    }
    // `JavaPluginsFlagAliasRule`: exposes the `--plugin` flag as a `JavaPluginInfo`; only
    // instantiable as `@bazel_tools//tools/jdk:java_plugins_flag_alias`.
    v.push(Builder::new("java_plugins_flag_alias").build());
    // `CcLibcTopAlias` (a `CommonAliasRule`): aliases the `--grte_top` label, if any.
    v.push(Builder::new("cc_libc_top_alias").build());

    // Shell: Bazel 9 has no native `sh_*` rules (they live in `@rules_shell`).

    // C++/Java/Python/proto rules are not native in Bazel 9: they are Starlark rules from
    // @rules_cc, @rules_java, @rules_python and @protobuf (loaded via `load()`).

    // Build settings.
    v.push(Builder::new("label_flag")
        .attr(a("build_setting_default", AttrType::Label).nonconfigurable()).build());
    v.push(Builder::new("label_setting")
        .attr(a("build_setting_default", AttrType::Label).nonconfigurable()).build());

    v.into_iter()
        .map(|rc| {
            let mut rc = Arc::try_unwrap(rc).unwrap_or_else(|a| (*a).clone());
            if rc.open {
                // Hand-typed classes: the common order-independent attributes (`BaseRuleClasses`).
                for n in ["tags", "features", "visibility"] {
                    if let Some(a) = rc.attrs.get_mut(n) {
                        a.order_independent = true;
                    }
                }
            } else {
                if let Some(defs) = generated_defs(&rc.name) {
                    apply_gen(&mut rc, defs, Vec::new());
                }
            }
            (rc.name.clone(), Arc::new(rc))
        })
        .collect()
}

/// Builds a rule class for a Starlark-defined `rule()`.
pub fn starlark_rule_class(
    name: &str,
    user_attrs: Vec<Attribute>,
    executable: bool,
    test: bool,
    implicit_outputs: Vec<(String, String)>,
    build_setting: bool,
) -> RuleClass {
    let mut b = Builder::new(name);
    if test {
        b.rc.is_test = true;
        b.rc.is_executable = true;
    } else if executable {
        b.rc.is_executable = true;
    }
    let kind = if test {
        "@starlark:test"
    } else if build_setting {
        "@starlark:build_setting"
    } else if executable {
        "@starlark:executable"
    } else {
        "@starlark"
    };
    match generated_defs(kind) {
        Some(defs) => {
            // The generated schema replaces the hand-typed common/test/binary attributes.
            b.rc.attrs.clear();
            apply_gen(&mut b.rc, defs, user_attrs);
        }
        None => b = b.attrs(user_attrs),
    }
    b.rc.implicit_outputs = implicit_outputs;
    b.rc.is_starlark = true;
    b.rc.build_setting = build_setting;
    b.rc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_classes_have_common_attrs() {
        let rcs = native_rule_classes();
        let g = &rcs["genrule"];
        assert!(g.attr("name").unwrap().mandatory);
        assert!(g.attr("outs").unwrap().mandatory);
        assert!(g.attr("visibility").is_some());
        assert!(!g.open);
        // Bazel 9 has no native `sh_*` rules.
        assert!(!rcs.contains_key("sh_binary") && !rcs.contains_key("sh_test") && !rcs.contains_key("sh_library"));
    }
}
