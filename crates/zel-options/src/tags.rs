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

//! Option categorization and tagging enums matching Bazel's options framework.

use std::fmt;

/// Describes the intent and effects of an option.
/// Mirrors `com.google.devtools.common.options.OptionEffectTag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum OptionEffectTag {
    Unknown = 0,
    NoOp = 1,
    LosesIncrementalState = 2,
    ChangesInputs = 3,
    AffectsOutputs = 4,
    BuildFileSemantics = 5,
    BazelInternalConfiguration = 6,
    LoadingAndAnalysis = 7,
    Execution = 8,
    HostMachineResourceOptimizations = 9,
    EagernessToExit = 10,
    BazelMonitoring = 11,
    TerminalOutput = 12,
    ActionCommandLines = 13,
    TestRunner = 14,
}

impl OptionEffectTag {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "UNKNOWN",
            Self::NoOp => "NO_OP",
            Self::LosesIncrementalState => "LOSES_INCREMENTAL_STATE",
            Self::ChangesInputs => "CHANGES_INPUTS",
            Self::AffectsOutputs => "AFFECTS_OUTPUTS",
            Self::BuildFileSemantics => "BUILD_FILE_SEMANTICS",
            Self::BazelInternalConfiguration => "BAZEL_INTERNAL_CONFIGURATION",
            Self::LoadingAndAnalysis => "LOADING_AND_ANALYSIS",
            Self::Execution => "EXECUTION",
            Self::HostMachineResourceOptimizations => "HOST_MACHINE_RESOURCE_OPTIMIZATIONS",
            Self::EagernessToExit => "EAGERNESS_TO_EXIT",
            Self::BazelMonitoring => "BAZEL_MONITORING",
            Self::TerminalOutput => "TERMINAL_OUTPUT",
            Self::ActionCommandLines => "ACTION_COMMAND_LINES",
            Self::TestRunner => "TEST_RUNNER",
        }
    }
}

impl fmt::Display for OptionEffectTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Identifies options that are either not supported or intended to break old behavior.
/// Mirrors `com.google.devtools.common.options.OptionMetadataTag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum OptionMetadataTag {
    Experimental = 0,
    IncompatibleChange = 1,
    Deprecated = 2,
    Hidden = 3,
    Internal = 4,
    NonConfigurable = 8,
    FullyRedactedInLogs = 9,
}

impl OptionMetadataTag {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Experimental => "EXPERIMENTAL",
            Self::IncompatibleChange => "INCOMPATIBLE_CHANGE",
            Self::Deprecated => "DEPRECATED",
            Self::Hidden => "HIDDEN",
            Self::Internal => "INTERNAL",
            Self::NonConfigurable => "NON_CONFIGURABLE",
            Self::FullyRedactedInLogs => "FULLY_REDACTED_IN_LOGS",
        }
    }
}

impl fmt::Display for OptionMetadataTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Categories used to group options in generated documentation and help.
/// Mirrors `com.google.devtools.common.options.OptionDocumentationCategory`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum OptionDocumentationCategory {
    Uncategorized = 0,
    Undocumented = 1,
    BazelClientOptions = 2,
    Logging = 3,
    InputStrictness = 4,
    ExecutionStrategy = 5,
    BuildTimeOptimization = 6,
    OutputSelection = 7,
    OutputParameters = 8,
    Signing = 9,
    StarlarkSemantics = 10,
    Testing = 11,
    Toolchain = 12,
    Query = 13,
    ModCommand = 14,
    Bzlmod = 15,
    GenericInputs = 16,
    Remote = 17,
}

impl OptionDocumentationCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Uncategorized => "UNCATEGORIZED",
            Self::Undocumented => "UNDOCUMENTED",
            Self::BazelClientOptions => "BAZEL_CLIENT_OPTIONS",
            Self::Logging => "LOGGING",
            Self::InputStrictness => "INPUT_STRICTNESS",
            Self::ExecutionStrategy => "EXECUTION_STRATEGY",
            Self::BuildTimeOptimization => "BUILD_TIME_OPTIMIZATION",
            Self::OutputSelection => "OUTPUT_SELECTION",
            Self::OutputParameters => "OUTPUT_PARAMETERS",
            Self::Signing => "SIGNING",
            Self::StarlarkSemantics => "STARLARK_SEMANTICS",
            Self::Testing => "TESTING",
            Self::Toolchain => "TOOLCHAIN",
            Self::Query => "QUERY",
            Self::ModCommand => "MOD_COMMAND",
            Self::Bzlmod => "BZLMOD",
            Self::GenericInputs => "GENERIC_INPUTS",
            Self::Remote => "REMOTE",
        }
    }
}

impl fmt::Display for OptionDocumentationCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
