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

//! Option interpretation precedence ordering matching Bazel's `OptionPriority.java`.

use crate::error::OptionsParsingError;
use std::cmp::Ordering;
use std::fmt;

/// The priority category of option values, in increasing priority order.
/// Mirrors `com.google.devtools.common.options.OptionPriority.PriorityCategory`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum PriorityCategory {
    /// Values specified as default in Option annotations.
    Default = 0,

    /// Overrides default options at runtime while still allowing manual user override.
    ComputedDefault = 1,

    /// Options coming from a configuration file or rc file.
    RcFile = 2,

    /// Options coming from the command line.
    CommandLine = 3,

    /// Options coming from an invocation policy.
    InvocationPolicy = 4,

    /// Unconditionally overrides any user-provided options.
    SoftwareRequirement = 5,
}

impl PriorityCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::ComputedDefault => "computed_default",
            Self::RcFile => "rc_file",
            Self::CommandLine => "command_line",
            Self::InvocationPolicy => "invocation_policy",
            Self::SoftwareRequirement => "software_requirement",
        }
    }
}

impl fmt::Display for PriorityCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The position of an option in interpretation order.
/// Uses a last-option-wins system for single valued options,
/// and preserves order for multiple-valued options.
/// Mirrors `com.google.devtools.common.options.OptionPriority`.
#[derive(Debug, Clone, Eq)]
pub struct OptionPriority {
    pub category: PriorityCategory,
    pub indices: Vec<usize>,
    pub already_expanded: bool,
}

impl OptionPriority {
    pub fn new(category: PriorityCategory, indices: Vec<usize>) -> Self {
        Self {
            category,
            indices,
            already_expanded: false,
        }
    }

    pub fn lowest_at_category(category: PriorityCategory) -> Self {
        Self::new(category, vec![0])
    }

    pub fn next(&self) -> Self {
        let mut new_indices = self.indices.clone();
        if let Some(last) = new_indices.last_mut() {
            *last += 1;
        } else {
            new_indices.push(0);
        }
        Self::new(self.category, new_indices)
    }

    pub fn get_child_priority(&mut self) -> Result<Self, OptionsParsingError> {
        if self.already_expanded {
            return Err(OptionsParsingError::new("Tried to expand option too many times"));
        }
        self.already_expanded = true;
        let mut child_indices = self.indices.clone();
        child_indices.push(0);
        Ok(Self::new(self.category, child_indices))
    }
}

impl PartialEq for OptionPriority {
    fn eq(&self, other: &Self) -> bool {
        self.category == other.category && self.indices == other.indices
    }
}

impl Ord for OptionPriority {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.category != other.category {
            return self.category.cmp(&other.category);
        }
        for (a, b) in self.indices.iter().zip(other.indices.iter()) {
            if a != b {
                return a.cmp(b);
            }
        }
        self.indices.len().cmp(&other.indices.len())
    }
}

impl PartialOrd for OptionPriority {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for OptionPriority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OptionPriority({:?}, {:?})", self.category, self.indices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_priority_ordering() {
        let p_default = OptionPriority::lowest_at_category(PriorityCategory::Default);
        let p_rc1 = OptionPriority::lowest_at_category(PriorityCategory::RcFile);
        let p_rc2 = p_rc1.next();
        let p_cli = OptionPriority::lowest_at_category(PriorityCategory::CommandLine);
        let p_policy = OptionPriority::lowest_at_category(PriorityCategory::InvocationPolicy);

        assert!(p_default < p_rc1);
        assert!(p_rc1 < p_rc2);
        assert!(p_rc2 < p_cli);
        assert!(p_cli < p_policy);

        let mut p_cli_parent = p_cli.clone();
        let p_cli_child = p_cli_parent.get_child_priority().unwrap();
        assert!(p_cli < p_cli_child);
        assert!(p_cli_child < p_cli.next());
    }
}
