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

//! Bazel startup & command-line option parser and `.bazelrc` processor.
//! Faithful native Rust reimplementation of:
//! - `com.google.devtools.common.options`
//! - `com.google.devtools.build.lib.runtime.BlazeOptionHandler`
//! - `com.google.devtools.build.lib.runtime.ConfigExpander`
//! - `src/main/cpp/option_processor.cc`, `startup_options.cc`, `rc_file.cc`

pub mod canonicalize;
pub mod config_expander;
pub mod converters;
mod converters_ext;
pub mod definition;
mod enum_type_names;
pub mod error;
pub mod flag_tables;
pub mod invocation_policy;
pub mod parser;
pub mod priority;
pub mod rc;
mod semantics_keys;
pub mod semantics_info;
pub mod spellcheck;
pub mod startup;
pub mod tags;
pub mod types;

pub use canonicalize::{canonicalize_flags, CanonicalizeCommandOptions};
pub use config_expander::{expand_command_args, get_platform_name};
pub use converters::{
    convert_assignment, convert_boolean, convert_bytesize, convert_comma_list,
    convert_compilation_mode, convert_double, convert_duration, convert_flag_alias, convert_int,
    convert_enum, convert_long, convert_option, convert_tristate, convert_value,
};
pub use definition::{OptionDefinition, OptionValueDescription};
pub use error::OptionsParsingError;
pub use flag_tables::{
    get_command_abbrev_map, get_command_allowed_classes, get_command_options_map,
    get_startup_options_map, ALL_COMMAND_OPTIONS, ALL_STARTUP_OPTIONS, COMMAND_INHERITANCE, COMMAND_OPTION_CLASSES,
    COMMON_OPTION_CLASSES, FRAGMENT_OPTION_CLASSES,
};
pub use invocation_policy::{
    base64_decode, AllowValues, ConfigOrder, ConfigPolicy, DisallowValues, FlagPolicy,
    InvocationPolicy, PolicyOperation, ReplacementValue, SetValue, SetValueBehavior, UseDefault,
};
pub use parser::{split_unsupported_common_args, ParsedCommandOptions};
pub use priority::{OptionPriority, PriorityCategory};
pub use rc::{discover_and_load_rc_files, get_command_ancestry, tokenize_rc_line, BazelrcConfig};
pub use semantics_info::starlark_semantics_string;
pub use spellcheck::{did_you_mean, edit_distance, suggest};
pub use startup::{split_command_line, StartupOptions};
pub use tags::{OptionDocumentationCategory, OptionEffectTag, OptionMetadataTag};
pub use types::{OptionType, OptionValue, TriState};
