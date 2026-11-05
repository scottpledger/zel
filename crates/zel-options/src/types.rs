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

//! Types, enums, and representations for option values.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;
use zel_util::resource_converter::IntegerSpec;

/// Three-state boolean value matching Bazel's `TriState.java`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TriState {
    Auto = 0,
    Yes = 1,
    No = 2,
}

impl TriState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Yes => "yes",
            Self::No => "no",
        }
    }

    pub fn to_bool(&self) -> Option<bool> {
        match self {
            Self::Auto => None,
            Self::Yes => Some(true),
            Self::No => Some(false),
        }
    }
}

impl fmt::Display for TriState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An `EnumConverter` / `BoolOrEnumConverter` (`Not a valid {type_name}: '{input}' (should be ..)`).
#[derive(Debug, PartialEq, Eq)]
pub struct EnumSpec {
    /// The converter's `typeName`.
    pub type_name: &'static str,
    /// The enum constants' `toString()` values, lowercased, in declaration order.
    pub values: &'static [&'static str],
    /// `BoolOrEnumConverter`: the constant that `true`-like inputs map to.
    pub true_value: Option<&'static str>,
    /// `BoolOrEnumConverter`: the constant that `false`-like inputs map to.
    pub false_value: Option<&'static str>,
}

/// Converters (by Java class simple name) that implement `BooleanStyleOption`: they accept the
/// boolean forms `--flag` / `--noflag` in addition to `--flag=<value>`.
pub const BOOLEAN_STYLE_CONVERTERS: &[&str] = &[
    "MnemonicBooleanConverter",
    "SlimProfileConverter",
];

/// The declared schema type of an option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionType {
    Boolean,
    TriState,
    Int,
    Long,
    Double,
    String,
    Duration,
    ByteSize,
    Label,
    ListString,
    ListLabel,
    Map,
    Void,
    /// `EnumConverter` / `BoolOrEnumConverter`.
    Enum(&'static EnumSpec),
    /// `ResourceConverter.IntegerConverter` subclasses (`--jobs`, `--local_test_jobs`, ...). The
    /// raw string is preserved by the parser for the execution phase to re-evaluate.
    Resource(IntegerSpec),
    /// `ResourceConverter.AssignmentConverter` (`--local_resources=name=value`).
    ResourceAssignment,
    /// A converter implemented by name in `converters::convert_custom`.
    Custom(&'static str),
}

impl OptionType {
    /// Whether this option uses boolean syntax (`--flag`, `--noflag`, `--flag=true/false`).
    pub fn uses_boolean_syntax(&self) -> bool {
        match self {
            OptionType::Boolean | OptionType::TriState => true,
            // `BoolOrEnumConverter`.
            OptionType::Enum(spec) => spec.true_value.is_some(),
            // `BooleanStyleOption`.
            OptionType::Custom(name) => BOOLEAN_STYLE_CONVERTERS.contains(name),
            _ => false,
        }
    }

    /// Whether this option requires a value argument on the command line.
    pub fn requires_value(&self) -> bool {
        !self.uses_boolean_syntax() && !matches!(self, OptionType::Void)
    }

    /// Whether this option is a list type.
    pub fn is_list(&self) -> bool {
        matches!(self, OptionType::ListString | OptionType::ListLabel)
    }
}

/// A parsed and typed option value.
#[derive(Debug, Clone, PartialEq)]
pub enum OptionValue {
    Boolean(bool),
    TriState(TriState),
    Int(i32),
    Long(i64),
    Double(f64),
    String(String),
    Duration(Duration),
    ByteSize(u64),
    Label(String),
    List(Vec<OptionValue>),
    Map(BTreeMap<String, String>),
    /// A `Map.Entry` value (`name=value` assignments).
    Entry(String, Box<OptionValue>),
    Void,
}

impl OptionValue {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Boolean(b) => Some(*b),
            Self::TriState(t) => t.to_bool(),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i32> {
        match self {
            Self::Int(i) => Some(*i),
            Self::Long(l) => Some(*l as i32),
            _ => None,
        }
    }

    pub fn as_long(&self) -> Option<i64> {
        match self {
            Self::Long(l) => Some(*l),
            Self::Int(i) => Some(*i as i64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s.as_str()),
            Self::Label(l) => Some(l.as_str()),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[OptionValue]> {
        match self {
            Self::List(l) => Some(l.as_slice()),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&BTreeMap<String, String>> {
        match self {
            Self::Map(m) => Some(m),
            _ => None,
        }
    }

    pub fn to_string_repr(&self) -> String {
        match self {
            Self::Boolean(b) => if *b { "true".to_string() } else { "false".to_string() },
            Self::TriState(t) => t.as_str().to_string(),
            Self::Int(i) => i.to_string(),
            Self::Long(l) => l.to_string(),
            Self::Double(d) => d.to_string(),
            Self::String(s) => s.clone(),
            Self::Duration(d) => {
                // Canonical `DurationConverter` input form without losing sub-second precision.
                if d.subsec_nanos() == 0 {
                    format!("{}s", d.as_secs())
                } else if d.subsec_nanos() % 1_000_000 == 0 {
                    format!("{}ms", d.as_millis())
                } else {
                    format!("{}ns", d.as_nanos())
                }
            }
            Self::ByteSize(b) => b.to_string(),
            Self::Label(l) => l.clone(),
            Self::List(items) => {
                let parts: Vec<String> = items.iter().map(|item| item.to_string_repr()).collect();
                parts.join(",")
            }
            Self::Map(m) => {
                let parts: Vec<String> = m.iter().map(|(k, v)| format!("{k}={v}")).collect();
                parts.join(",")
            }
            Self::Entry(k, v) => format!("{k}={}", v.to_string_repr()),
            Self::Void => "".to_string(),
        }
    }
}
