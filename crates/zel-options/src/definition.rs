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

//! Option definition and parsed option value description.

use crate::priority::OptionPriority;
use crate::tags::{OptionDocumentationCategory, OptionEffectTag, OptionMetadataTag};
use crate::types::{OptionType, OptionValue};

/// Schema-driven definition of an option matching Bazel's `OptionDefinition.java`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionDefinition {
    pub name: &'static str,
    pub abbrev: Option<char>,
    pub option_type: OptionType,
    pub default_value: Option<&'static str>,
    pub allow_multiple: bool,
    pub expansion: Option<&'static [&'static str]>,
    pub implicit_requirements: &'static [&'static str],
    pub old_name: Option<&'static str>,
    pub deprecation_warning: Option<&'static str>,
    pub category: &'static str,
    pub documentation_category: OptionDocumentationCategory,
    pub effect_tags: &'static [OptionEffectTag],
    pub metadata_tags: &'static [OptionMetadataTag],
    pub help: &'static str,
    pub declaring_class: &'static str,
    /// `Converter.getTypeDescription()` as evaluated from the Java source by the table generator; `None` when the
    /// converter's description is computed in a way the generator does not model.
    pub type_description: Option<&'static str>,
    pub commands: &'static [&'static str],
    /// `Converter` class name without the `Converter` suffix (`HelpCommand.createFlagInfo` `type_converter`);
    /// `None` when the converter's simple name does not end in `Converter` (a class literally named `Converter` gives `Some("")`).
    pub type_converter: Option<&'static str>,
    /// `EnumConverter` constants' `toString()` values (`enum_values`), empty for non-enum converters.
    pub enum_values: &'static [&'static str],
    /// `OptionDefinition.usesBooleanValueSyntax()`: `boolean`/`TriState` or a `BoolOrEnumConverter`/`BooleanStyleOption`.
    pub boolean_syntax: bool,
    /// `@Option(valueHelp)`.
    pub value_help: &'static str,
    /// Fully qualified binary name of the declaring options class (`pkg.Outer$Inner`).
    pub java_class: &'static str,
}

impl OptionDefinition {
    pub const fn new(
        name: &'static str,
        abbrev: Option<char>,
        option_type: OptionType,
        default_value: Option<&'static str>,
        allow_multiple: bool,
        expansion: Option<&'static [&'static str]>,
        implicit_requirements: &'static [&'static str],
        old_name: Option<&'static str>,
        deprecation_warning: Option<&'static str>,
        category: &'static str,
        documentation_category: OptionDocumentationCategory,
        effect_tags: &'static [OptionEffectTag],
        metadata_tags: &'static [OptionMetadataTag],
        help: &'static str,
        declaring_class: &'static str,
        type_description: Option<&'static str>,
        commands: &'static [&'static str],
        type_converter: Option<&'static str>,
        enum_values: &'static [&'static str],
        boolean_syntax: bool,
        value_help: &'static str,
        java_class: &'static str,
    ) -> Self {
        Self {
            name,
            abbrev,
            option_type,
            default_value,
            allow_multiple,
            expansion,
            implicit_requirements,
            old_name,
            deprecation_warning,
            category,
            documentation_category,
            effect_tags,
            metadata_tags,
            help,
            declaring_class,
            type_description,
            commands,
            type_converter,
            enum_values,
            boolean_syntax,
            value_help,
            java_class,
        }
    }

    pub fn is_expansion(&self) -> bool {
        self.expansion.map(|e| !e.is_empty()).unwrap_or(false)
    }

    pub fn has_implicit_requirements(&self) -> bool {
        !self.implicit_requirements.is_empty()
    }

    /// `OptionDefinition.usesBooleanValueSyntax()` (derived from the Java declaration by the table generator).
    pub fn uses_boolean_syntax(&self) -> bool {
        self.boolean_syntax
    }

    pub fn requires_value(&self) -> bool {
        // `OptionDefinition.requiresValue()`: `!isVoidField() && !usesBooleanValueSyntax()`.
        self.option_type != OptionType::Void && !self.boolean_syntax
    }

    pub fn is_internal(&self) -> bool {
        self.metadata_tags.contains(&OptionMetadataTag::Internal)
    }

    pub fn is_deprecated(&self) -> bool {
        self.metadata_tags.contains(&OptionMetadataTag::Deprecated)
    }

    pub fn is_experimental(&self) -> bool {
        self.metadata_tags.contains(&OptionMetadataTag::Experimental)
    }

    pub fn is_incompatible_change(&self) -> bool {
        self.metadata_tags.contains(&OptionMetadataTag::IncompatibleChange)
    }
}

/// Description of a parsed option value with provenance and priority.
/// Mirrors `com.google.devtools.common.options.OptionValueDescription`.
#[derive(Debug, Clone, PartialEq)]
pub struct OptionValueDescription {
    pub definition: &'static OptionDefinition,
    pub raw_values: Vec<String>,
    pub value: OptionValue,
    pub priority: OptionPriority,
    pub source: String,
}

impl OptionValueDescription {
    pub fn new(
        definition: &'static OptionDefinition,
        raw_values: Vec<String>,
        value: OptionValue,
        priority: OptionPriority,
        source: String,
    ) -> Self {
        Self {
            definition,
            raw_values,
            value,
            priority,
            source,
        }
    }
}
