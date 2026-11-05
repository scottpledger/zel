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

//! Error types for options parsing matching Bazel's exceptions and exit codes.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionsParsingError {
    pub message: String,
    pub failed_arg: Option<String>,
}

impl OptionsParsingError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            failed_arg: None,
        }
    }

    pub fn with_arg(msg: impl Into<String>, arg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            failed_arg: Some(arg.into()),
        }
    }

    /// An `OptionsParsingException` with an invalid argument (`BlazeOptionHandler` prints
    /// `<arg> :: <message>`).
    pub fn with_failed_arg(arg: &str, message: String) -> Self {
        Self { message, failed_arg: Some(arg.to_string()) }
    }

    pub fn unrecognized_option(arg: &str, suggestion: &str) -> Self {
        Self {
            message: format!("Unrecognized option: {arg}{suggestion}"),
            failed_arg: Some(arg.to_string()),
        }
    }

    pub fn unknown_startup_option(arg: &str, product_name: &str) -> Self {
        Self {
            message: format!(
                "Unknown startup option: '{arg}'.\n  For more info, run '{product_name} help startup_options'."
            ),
            failed_arg: Some(arg.to_string()),
        }
    }

    pub fn startup_option_missing_value(arg: &str, product_name: &str) -> Self {
        Self {
            message: format!(
                "Startup option '{arg}' expects a value.\nUsage: '{arg}=somevalue' or '{arg} somevalue'.\n  For more info, run '{product_name} help startup_options'."
            ),
            failed_arg: Some(arg.to_string()),
        }
    }

    /// Formats the error into standard Bazel command-line error output.
    /// E.g. "ERROR: --bogus :: Unrecognized option: --bogus"
    pub fn format_bazel_error(&self) -> String {
        if let Some(ref arg) = self.failed_arg {
            return format!("ERROR: {arg} :: {}", self.message);
        }
        if self.message.starts_with("ERROR: ") {
            self.message.clone()
        } else {
            format!("ERROR: {}", self.message)
        }
    }
}

impl fmt::Display for OptionsParsingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OptionsParsingError {}

impl From<String> for OptionsParsingError {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<&str> for OptionsParsingError {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}
