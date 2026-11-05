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

//! Typed rule attributes: `Type`/`BuildType`/`Attribute` of `lib/packages`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use zel_cmdline::{Label, PackageIdentifier, RepositoryMapping};
use zel_starlark::{SelectorElement, StarlarkDict, StarlarkList, Value};

/// Attribute types (`Type.INTEGER`, `BuildType.LABEL_LIST`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrType {
    Int,
    String,
    Bool,
    Tristate,
    StringList,
    IntList,
    StringDict,
    StringListDict,
    Label,
    LabelList,
    LabelKeyedStringDict,
    StringKeyedLabelDict,
    Output,
    OutputList,
}

impl AttrType {
    /// The type's name as printed by Bazel in error messages (`Type.toString()`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Int => "int",
            Self::String => "string",
            Self::Bool => "bool",
            Self::Tristate => "int",
            Self::StringList => "list(string)",
            Self::IntList => "list(int)",
            Self::StringDict => "dict(string, string)",
            Self::StringListDict => "dict(string, list(string))",
            Self::Label => "label",
            Self::LabelList => "list(label)",
            Self::LabelKeyedStringDict => "dict(label, string)",
            Self::StringKeyedLabelDict => "dict(string, label)",
            Self::Output => "output",
            Self::OutputList => "list(output)",
        }
    }
}

/// A converted attribute value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttrValue {
    Int(i64),
    Str(String),
    Bool(bool),
    Tristate(i32),
    StrList(Vec<String>),
    IntList(Vec<i64>),
    StrDict(BTreeMap<String, String>),
    StrListDict(BTreeMap<String, Vec<String>>),
    Label(Option<Label>),
    LabelList(Vec<Label>),
    LabelKeyedStrDict(Vec<(Label, String)>),
    /// `attr.string_keyed_label_dict`.
    StrKeyedLabelDict(Vec<(String, Label)>),
    Output(Option<Label>),
    OutputList(Vec<Label>),
    /// A configurable value: concatenation of plain values and `select()` branches.
    Select(Vec<SelectPart>),
}

/// One element of a `select()` chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectPart {
    Plain(AttrValue),
    Select {
        /// `(condition label, value)`; the default branch has label `//conditions:default`.
        entries: Vec<(Label, AttrValue)>,
        no_match_error: String,
    },
}

/// What kinds of files a label attribute accepts (`FileTypeSet`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FileTypes {
    #[default]
    Any,
    NoFiles,
    Extensions(Vec<String>),
}

/// A rule attribute definition (`Attribute.java`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    pub name: String,
    pub ty: AttrType,
    pub mandatory: bool,
    pub non_empty: bool,
    pub configurable: bool,
    pub default: Option<AttrValue>,
    pub allowed_files: FileTypes,
    /// A Starlark label attribute declared with neither `allow_files` nor `allow_single_file`:
    /// `Attribute.Builder` then allows no files (`FileTypeSet.NO_FILE`).
    pub files_unspecified: bool,
    /// Late-bound default whose rule-based default (shown by `query`) is the raw value of this
    /// attribute of the rule (`label_flag`'s `:alias` -> `build_setting_default`).
    pub rule_based_default: Option<String>,
    /// `AttributeFormatter.maybeSetNoDep` serializes `nodep` for this attribute's type (generated
    /// definitions only; `None` = not known).
    pub nodep_set: Option<bool>,
    pub allow_single_file: bool,
    pub allowed_values: Option<Vec<String>>,
    pub doc: String,
    /// `attr.label(executable = True)`.
    pub executable: bool,
    /// `cfg = "exec"` / `"host"` on a label attribute.
    pub cfg_exec: bool,
    /// `cfg = config.exec(exec_group = "g")`: the exec group whose platform defines the
    /// execution configuration.
    pub cfg_exec_group: Option<String>,
    /// Late-bound default (`default = configuration_field(fragment, name)`): resolved from the
    /// configuration option `name` at analysis time.
    pub late_bound: Option<(String, String)>,
    /// `allow_rules = [...]`: rule classes accepted as dependencies regardless of providers
    /// (`RuleContext.checkRuleDependencyClass`).
    pub allowed_rule_classes: Option<Vec<String>>,
    /// Computed default (`default = fn`): a Starlark function whose parameter names are the
    /// names of the rule's other attributes; called at analysis time.
    pub computed_default: Option<zel_starlark::Value>,
    /// Starlark transition (`cfg = my_transition`), if any.
    pub cfg_transition: Option<zel_starlark::Value>,
    /// `providers = [[A, B], [C]]`: each inner list is a conjunction (provider keys).
    pub providers: Vec<Vec<String>>,
    /// `aspects = [...]` raw values.
    pub aspects: Vec<zel_starlark::Value>,
    /// Query-visible `nodep` flag (`NODEP_LABEL_LIST`, `generator_*`, `deprecation`, ...).
    pub nodep: bool,
    /// Whether the attribute's dependencies are tool (exec configuration) deps for
    /// `--notool_deps`, when known from the generated rule definitions (`rule_defs_gen`);
    /// `None` falls back to [`Attribute::cfg_exec`].
    pub tool_dep: Option<bool>,
    /// The `Build.Attribute.Discriminator` Bazel reports for the attribute when it differs from
    /// what the value type implies (`LICENSE`, `STRING_LIST` for nodep label lists,
    /// `LABEL_LIST_DICT`, ...).
    pub proto_type: Option<&'static str>,
    /// `Attribute.orderIndependent()`: plain list values are sorted when converted.
    pub order_independent: bool,
}

impl Attribute {
    pub fn new(name: &str, ty: AttrType) -> Self {
        Self {
            name: name.to_string(),
            ty,
            mandatory: false,
            non_empty: false,
            configurable: true,
            default: None,
            allowed_files: FileTypes::Any,
            files_unspecified: false,
            rule_based_default: None,
            nodep_set: None,
            allow_single_file: false,
            allowed_values: None,
            doc: String::new(),
            executable: false,
            cfg_exec: false,
            cfg_exec_group: None,
            late_bound: None,
            computed_default: None,
            allowed_rule_classes: None,
            cfg_transition: None,
            providers: Vec::new(),
            aspects: Vec::new(),
            nodep: false,
            tool_dep: None,
            proto_type: None,
            order_independent: false,
        }
    }

    pub fn mandatory(mut self) -> Self {
        self.mandatory = true;
        self
    }
    pub fn non_empty(mut self) -> Self {
        self.non_empty = true;
        self
    }
    pub fn nonconfigurable(mut self) -> Self {
        self.configurable = false;
        self
    }
    pub fn cfg_exec(mut self) -> Self {
        self.cfg_exec = true;
        self
    }
    pub fn default(mut self, v: AttrValue) -> Self {
        self.default = Some(v);
        self
    }
    pub fn allowed_values(mut self, v: &[&str]) -> Self {
        self.allowed_values = Some(v.iter().map(|s| s.to_string()).collect());
        self
    }
    pub fn extensions(mut self, e: &[&str]) -> Self {
        self.allowed_files = FileTypes::Extensions(e.iter().map(|s| s.to_string()).collect());
        self
    }
    pub fn no_files(mut self) -> Self {
        self.allowed_files = FileTypes::NoFiles;
        self
    }

    /// The value used when the attribute is not set explicitly.
    pub fn default_value(&self) -> AttrValue {
        if let Some(d) = &self.default {
            return d.clone();
        }
        match self.ty {
            AttrType::Int => AttrValue::Int(0),
            AttrType::String => AttrValue::Str(String::new()),
            AttrType::Bool => AttrValue::Bool(false),
            AttrType::Tristate => AttrValue::Tristate(-1),
            AttrType::StringList => AttrValue::StrList(Vec::new()),
            AttrType::IntList => AttrValue::IntList(Vec::new()),
            AttrType::StringDict => AttrValue::StrDict(BTreeMap::new()),
            AttrType::StringListDict => AttrValue::StrListDict(BTreeMap::new()),
            AttrType::Label => AttrValue::Label(None),
            AttrType::LabelList => AttrValue::LabelList(Vec::new()),
            AttrType::LabelKeyedStringDict => AttrValue::LabelKeyedStrDict(Vec::new()),
            AttrType::StringKeyedLabelDict => AttrValue::StrKeyedLabelDict(Vec::new()),
            AttrType::Output => AttrValue::Output(None),
            AttrType::OutputList => AttrValue::OutputList(Vec::new()),
        }
    }
}

/// Context needed to convert Starlark values into attribute values.
#[derive(Clone)]
pub struct ConvertCtx<'a> {
    pub pkg: &'a PackageIdentifier,
    pub mapping: &'a RepositoryMapping,
    pub rule_class: &'a str,
}

fn type_of(v: &Value) -> &'static str {
    v.type_name()
}

fn mismatch(v: &Value, ty: AttrType, attr: &str, rule_class: &str) -> String {
    format!(
        "expected value of type '{}' for attribute '{}' in '{}' rule, but got {} ({})",
        ty.name(),
        attr,
        rule_class,
        v.repr(),
        type_of(v)
    )
}

fn seq(v: &Value) -> Option<Vec<Value>> {
    match v {
        Value::List(l) => Some(l.lock().unwrap().elements.clone()),
        Value::Tuple(t) => Some(t.clone()),
        _ => None,
    }
}

fn dict_items(v: &Value) -> Option<Vec<(Value, Value)>> {
    match v {
        Value::Dict(d) => Some(d.lock().unwrap().items()),
        _ => None,
    }
}

/// Converts a label string relative to the current package (`LabelConverter`).
pub fn convert_label(s: &str, ctx: &ConvertCtx<'_>) -> Result<Label, String> {
    if s.starts_with("//") || s.starts_with('@') {
        Label::parse_with_repo_mapping(s, ctx.mapping).map_err(|e| e.to_string())
    } else if let Some(name) = s.strip_prefix(':') {
        Label::create(ctx.pkg.clone(), name).map_err(|e| e.to_string())
    } else {
        // `foo/bar.cc` is shorthand for `:foo/bar.cc`.
        Label::create(ctx.pkg.clone(), s).map_err(|e| e.to_string())
    }
}

fn convert_one_label(v: &Value, attr: &str, idx: Option<usize>, ctx: &ConvertCtx<'_>) -> Result<Label, String> {
    match v {
        Value::Label(l) => Ok(l.clone()),
        Value::String(s) => convert_label(s, ctx).map_err(|e| match idx {
            Some(i) => format!(
                "invalid label '{s}' in element {i} of attribute '{attr}' in '{}' rule: {e}",
                ctx.rule_class
            ),
            None => format!(
                "invalid label '{s}' in attribute '{attr}' in '{}' rule: {e}",
                ctx.rule_class
            ),
        }),
        other => Err(format!(
            "expected value of type 'string' for element {} of attribute '{attr}' in '{}' rule, but got {} ({})",
            idx.unwrap_or(0),
            ctx.rule_class,
            other.repr(),
            type_of(other)
        )),
    }
}

fn convert_output_label(
    v: &Value,
    attr: &str,
    idx: Option<usize>,
    ctx: &ConvertCtx<'_>,
) -> Result<Label, String> {
    let l = convert_one_label(v, attr, idx, ctx)?;
    if l.package_identifier() != ctx.pkg {
        return Err(format!(
            "label '{}' is not in the current package",
            l.canonical_form()
        ));
    }
    Ok(l)
}

/// Converts a non-`select` Starlark value to the attribute's type.
pub fn convert_plain(
    v: &Value,
    ty: AttrType,
    attr: &str,
    ctx: &ConvertCtx<'_>,
) -> Result<AttrValue, String> {
    let err = || mismatch(v, ty, attr, ctx.rule_class);
    match ty {
        AttrType::Int => match v {
            Value::Int(i) => i
                .to_i64()
                .filter(|n| i32::try_from(*n).is_ok())
                .map(AttrValue::Int)
                .ok_or_else(|| format!("integer value '{}' for attribute '{attr}' in '{}' rule is out of range for a 32-bit integer", i, ctx.rule_class)),
            _ => Err(err()),
        },
        AttrType::String => match v {
            Value::String(s) => Ok(AttrValue::Str(s.clone())),
            _ => Err(err()),
        },
        AttrType::Bool => match v {
            Value::Bool(b) => Ok(AttrValue::Bool(*b)),
            Value::Int(i) if i.to_i64() == Some(0) => Ok(AttrValue::Bool(false)),
            Value::Int(i) if i.to_i64() == Some(1) => Ok(AttrValue::Bool(true)),
            _ => Err(err()),
        },
        AttrType::Tristate => match v {
            Value::Bool(b) => Ok(AttrValue::Tristate(i32::from(*b))),
            Value::Int(i) => match i.to_i64() {
                Some(n @ -1..=1) => Ok(AttrValue::Tristate(n as i32)),
                _ => Err(format!(
                    "expected value of type 'int' in range [-1, 1] for attribute '{attr}' in '{}' rule, but got {}",
                    ctx.rule_class,
                    v.repr()
                )),
            },
            _ => Err(err()),
        },
        AttrType::StringList => {
            let items = seq(v).ok_or_else(err)?;
            let mut out = Vec::with_capacity(items.len());
            for (i, it) in items.iter().enumerate() {
                match it {
                    Value::String(s) => out.push(s.clone()),
                    other => {
                        return Err(format!(
                            "expected value of type 'string' for element {i} of attribute '{attr}' in '{}' rule, but got {} ({})",
                            ctx.rule_class,
                            other.repr(),
                            type_of(other)
                        ))
                    }
                }
            }
            Ok(AttrValue::StrList(out))
        }
        AttrType::IntList => {
            let items = seq(v).ok_or_else(err)?;
            let mut out = Vec::with_capacity(items.len());
            for (i, it) in items.iter().enumerate() {
                match it {
                    Value::Int(n) => match n.to_i64() {
                        Some(n) => out.push(n),
                        None => return Err(err()),
                    },
                    other => {
                        return Err(format!(
                            "expected value of type 'int' for element {i} of attribute '{attr}' in '{}' rule, but got {} ({})",
                            ctx.rule_class,
                            other.repr(),
                            type_of(other)
                        ))
                    }
                }
            }
            Ok(AttrValue::IntList(out))
        }
        AttrType::StringKeyedLabelDict => {
            let items = dict_items(v).ok_or_else(err)?;
            let mut out = Vec::new();
            for (i, (k, val)) in items.iter().enumerate() {
                let Value::String(k) = k else { return Err(err()) };
                out.push((k.clone(), convert_one_label(val, attr, Some(i), ctx)?));
            }
            Ok(AttrValue::StrKeyedLabelDict(out))
        }
        AttrType::StringDict => {
            let items = dict_items(v).ok_or_else(err)?;
            let mut out = BTreeMap::new();
            for (k, val) in items {
                match (&k, &val) {
                    (Value::String(k), Value::String(val)) => {
                        out.insert(k.clone(), val.clone());
                    }
                    _ => return Err(err()),
                }
            }
            Ok(AttrValue::StrDict(out))
        }
        AttrType::StringListDict => {
            let items = dict_items(v).ok_or_else(err)?;
            let mut out = BTreeMap::new();
            for (k, val) in items {
                let Value::String(k) = k else { return Err(err()) };
                let list = seq(&val).ok_or_else(err)?;
                let mut strs = Vec::new();
                for e in list {
                    match e {
                        Value::String(s) => strs.push(s),
                        _ => return Err(err()),
                    }
                }
                out.insert(k, strs);
            }
            Ok(AttrValue::StrListDict(out))
        }
        AttrType::Label => match v {
            Value::None => Ok(AttrValue::Label(None)),
            _ => Ok(AttrValue::Label(Some(convert_one_label(v, attr, None, ctx)?))),
        },
        AttrType::LabelList => {
            let items = seq(v).ok_or_else(err)?;
            let mut out = Vec::with_capacity(items.len());
            for (i, it) in items.iter().enumerate() {
                out.push(convert_one_label(it, attr, Some(i), ctx)?);
            }
            Ok(AttrValue::LabelList(out))
        }
        AttrType::LabelKeyedStringDict => {
            let items = dict_items(v).ok_or_else(err)?;
            let mut out = Vec::new();
            for (i, (k, val)) in items.iter().enumerate() {
                let l = convert_one_label(k, attr, Some(i), ctx)?;
                let Value::String(s) = val else { return Err(err()) };
                out.push((l, s.clone()));
            }
            Ok(AttrValue::LabelKeyedStrDict(out))
        }
        AttrType::Output => match v {
            Value::None => Ok(AttrValue::Output(None)),
            _ => Ok(AttrValue::Output(Some(convert_output_label(v, attr, None, ctx)?))),
        },
        AttrType::OutputList => {
            let items = seq(v).ok_or_else(err)?;
            let mut out = Vec::new();
            for (i, it) in items.iter().enumerate() {
                out.push(convert_output_label(it, attr, Some(i), ctx)?);
            }
            Ok(AttrValue::OutputList(out))
        }
    }
}

/// Converts any Starlark value (including `select()` chains) to `ty`.
pub fn convert_value(
    v: &Value,
    attr: &Attribute,
    ctx: &ConvertCtx<'_>,
) -> Result<AttrValue, String> {
    if let Value::SelectorList(sl) = v {
        if !attr.configurable {
            return Err(format!("attribute \"{}\" is not configurable", attr.name));
        }
        let mut parts = Vec::new();
        for el in &sl.elements {
            match el {
                SelectorElement::Native(val) => {
                    parts.push(SelectPart::Plain(convert_plain(val, attr.ty, &attr.name, ctx)?))
                }
                SelectorElement::Select(map) => {
                    let mut entries = Vec::new();
                    let no_match_error = String::new();
                    for (k, val) in map {
                        let key = if k == "//conditions:default" {
                            Label::parse_canonical(&k).map_err(|e| e.to_string())?
                        } else {
                            convert_label(&k, ctx).map_err(|e| {
                                format!("invalid label '{k}' in select() key of attribute '{}': {e}", attr.name)
                            })?
                        };
                        // `None` in a select branch means the attribute's default value.
                        let converted = if matches!(val, Value::None) {
                            attr.default_value()
                        } else {
                            convert_plain(val, attr.ty, &attr.name, ctx)?
                        };
                        entries.push((key, converted));
                    }
                    parts.push(SelectPart::Select {
                        entries,
                        no_match_error,
                    });
                }
            }
        }
        return Ok(AttrValue::Select(parts));
    }
    convert_plain(v, attr.ty, &attr.name, ctx)
}

impl AttrValue {
    /// Converts back to a Starlark value (labels become canonical strings).
    pub fn to_value(&self) -> Value {
        fn list(items: Vec<Value>) -> Value {
            Value::List(Arc::new(Mutex::new(StarlarkList::new(items))))
        }
        fn dict(items: Vec<(Value, Value)>) -> Value {
            Value::Dict(Arc::new(Mutex::new(
                StarlarkDict::from_entries(items).unwrap_or_default(),
            )))
        }
        match self {
            Self::Int(i) => Value::Int(zel_starlark::BigInt::from_i64(*i)),
            Self::Str(s) => Value::String(s.clone()),
            Self::Bool(b) => Value::Bool(*b),
            Self::Tristate(t) => Value::Int(zel_starlark::BigInt::from_i64(i64::from(*t))),
            Self::StrList(l) => list(l.iter().map(|s| Value::String(s.clone())).collect()),
            Self::IntList(l) => list(l.iter().map(|n| Value::Int(zel_starlark::BigInt::from_i64(*n))).collect()),
            Self::StrDict(d) => dict(
                d.iter()
                    .map(|(k, v)| (Value::String(k.clone()), Value::String(v.clone())))
                    .collect(),
            ),
            Self::StrListDict(d) => dict(
                d.iter()
                    .map(|(k, v)| {
                        (
                            Value::String(k.clone()),
                            list(v.iter().map(|s| Value::String(s.clone())).collect()),
                        )
                    })
                    .collect(),
            ),
            Self::Label(l) | Self::Output(l) => match l {
                Some(l) => Value::String(l.canonical_form()),
                None => Value::None,
            },
            Self::LabelList(l) | Self::OutputList(l) => {
                list(l.iter().map(|l| Value::String(l.canonical_form())).collect())
            }
            Self::LabelKeyedStrDict(d) => dict(
                d.iter()
                    .map(|(k, v)| (Value::String(k.canonical_form()), Value::String(v.clone())))
                    .collect(),
            ),
            Self::StrKeyedLabelDict(d) => dict(
                d.iter()
                    .map(|(k, v)| (Value::String(k.clone()), Value::String(v.canonical_form())))
                    .collect(),
            ),
            Self::Select(parts) => {
                // Preserve selector structure for query output.
                let mut elems = Vec::new();
                for p in parts {
                    match p {
                        SelectPart::Plain(v) => elems.push(SelectorElement::Native(v.to_value())),
                        SelectPart::Select { entries, .. } => {
                            let mut map = Vec::new();
                            for (k, v) in entries {
                                map.push((k.canonical_form(), v.to_value()));
                            }
                            elems.push(SelectorElement::Select(map));
                        }
                    }
                }
                Value::SelectorList(zel_starlark::SelectorList { elements: elems })
            }
        }
    }

    /// Flat list of strings for the "legacy" convenience fields (`srcs`, `deps`, ...): labels in
    /// canonical form; for `select()` the default branch (or the first branch) is used.
    pub fn flat_strings(&self) -> Vec<String> {
        match self {
            Self::Str(s) => vec![s.clone()],
            Self::StrList(l) => l.clone(),
            Self::Label(Some(l)) | Self::Output(Some(l)) => vec![l.canonical_form()],
            Self::LabelList(l) | Self::OutputList(l) => {
                l.iter().map(|l| l.canonical_form()).collect()
            }
            Self::LabelKeyedStrDict(d) => d.iter().map(|(k, _)| k.canonical_form()).collect(),
            Self::StrKeyedLabelDict(d) => d.iter().map(|(_, v)| v.canonical_form()).collect(),
            Self::Select(parts) => {
                let mut out = Vec::new();
                for p in parts {
                    match p {
                        SelectPart::Plain(v) => out.extend(v.flat_strings()),
                        SelectPart::Select { entries, .. } => {
                            let chosen = entries
                                .iter()
                                .find(|(k, _)| k.canonical_form() == "//conditions:default")
                                .or_else(|| entries.first());
                            if let Some((_, v)) = chosen {
                                out.extend(v.flat_strings());
                            }
                        }
                    }
                }
                out
            }
            _ => Vec::new(),
        }
    }

    /// All labels referenced by this value, including every `select()` branch and key.
    pub fn labels(&self) -> Vec<Label> {
        match self {
            Self::Label(Some(l)) => vec![l.clone()],
            Self::LabelList(l) => l.clone(),
            Self::LabelKeyedStrDict(d) => d.iter().map(|(k, _)| k.clone()).collect(),
            Self::StrKeyedLabelDict(d) => d.iter().map(|(_, v)| v.clone()).collect(),
            Self::Select(parts) => {
                let mut out = Vec::new();
                for p in parts {
                    match p {
                        SelectPart::Plain(v) => out.extend(v.labels()),
                        SelectPart::Select { entries, .. } => {
                            for (k, v) in entries {
                                if k.canonical_form() != "//conditions:default" {
                                    out.push(k.clone());
                                }
                                out.extend(v.labels());
                            }
                        }
                    }
                }
                out
            }
            _ => Vec::new(),
        }
    }

    pub fn is_select(&self) -> bool {
        matches!(self, Self::Select(_))
    }

    /// Whether this (non-select) value is empty, for `non_empty` checks.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::StrList(l) => l.is_empty(),
            Self::StrDict(d) => d.is_empty(),
            Self::StrListDict(d) => d.is_empty(),
            Self::LabelList(l) | Self::OutputList(l) => l.is_empty(),
            Self::LabelKeyedStrDict(d) => d.is_empty(),
            Self::StrKeyedLabelDict(d) => d.is_empty(),
            Self::IntList(l) => l.is_empty(),
            _ => false,
        }
    }
}

/// Infers an [`AttrValue`] for attributes of "open" rule classes that have no schema entry.
pub fn infer_attribute(name: &str, v: &Value, rule_class: &str) -> Result<Attribute, String> {
    let ty = match v {
        Value::String(_) => AttrType::String,
        Value::Int(_) => AttrType::Int,
        Value::Bool(_) => AttrType::Bool,
        Value::List(_) | Value::Tuple(_) => AttrType::StringList,
        Value::Dict(_) => AttrType::StringDict,
        Value::SelectorList(_) => AttrType::StringList,
        other => {
            return Err(format!(
                "unsupported value of type '{}' for attribute '{name}' in '{rule_class}' rule",
                type_of(other)
            ))
        }
    };
    Ok(Attribute::new(name, ty))
}
