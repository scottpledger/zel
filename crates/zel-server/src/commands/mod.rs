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

//! The non-build commands (`lib/runtime/commands` and `bazel/commands` in Bazel), one module per
//! command, plus the command table used by `help`, the dispatcher's unknown-command handling and
//! the `mustRunInWorkspace` check (`BlazeOptionHandler.checkCwdInWorkspace`).

pub mod clean;
pub mod config;
pub mod dump;
pub mod help;
pub mod info;
pub mod license; // also hosts `canonicalize-flags`
pub mod markdown;
pub mod print_action;
pub mod version;

use std::sync::OnceLock;

/// `@Command` annotation data for one command.
#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    pub name: &'static str,
    pub must_run_in_workspace: bool,
    pub hidden: bool,
    pub allow_residue: bool,
    /// `shortDescription` with `%{product}` placeholders.
    pub short_description: &'static str,
    /// `completion` annotation (empty = none).
    pub completion: &'static str,
}

const fn spec(
    name: &'static str,
    must_run_in_workspace: bool,
    hidden: bool,
    allow_residue: bool,
    short_description: &'static str,
    completion: &'static str,
) -> CommandSpec {
    CommandSpec { name, must_run_in_workspace, hidden, allow_residue, short_description, completion }
}

/// All commands of this server, sorted by name (`getSortedCommands`).
pub const COMMANDS: &[CommandSpec] = &[
    spec("aquery", true, false, true, "Analyzes the given targets and queries the action graph.", "label"),
    spec("build", true, false, true, "Builds the specified targets.", "label"),
    spec("canonicalize-flags", false, false, true, "Canonicalizes a list of %{product} options.", ""),
    spec("clean", true, false, true, "Removes output files and optionally stops the server.", ""),
    spec("config", true, true, true, "Displays details of configurations.", "string"),
    spec("coverage", true, false, true, "Generates code coverage report for specified test targets.", "label-test"),
    spec("cquery", true, false, true, "Loads, analyzes, and queries the specified targets w/ configurations.", "label"),
    spec("dump", false, false, false, "Dumps the internal state of the %{product} server process.", ""),
    spec("fetch", true, false, false, "Fetches external repositories that are prerequisites to the targets.", "label"),
    spec("help", false, false, true, "Prints help for commands, or the index.", "command|{startup_options,target-syntax,info-keys}"),
    spec("info", true, false, true, "Displays runtime info about the %{product} server.", "info-key"),
    spec("license", false, false, true, "Prints the license of this software.", ""),
    spec("mobile-install", true, false, true, "Installs targets to mobile devices.", "label"),
    spec("mod", true, false, true, "Queries the Bzlmod external dependency graph", ""),
    spec("print_action", true, false, true, "Prints the command line args for compiling a file.", "label"),
    spec("query", true, false, true, "Executes a dependency graph query.", "label"),
    spec("run", true, false, true, "Runs the specified target.", "label"),
    spec("shutdown", false, false, false, "Stops the %{product} server.", ""),
    spec("test", true, false, true, "Builds and runs the specified test targets.", "label-test"),
    spec("vendor", true, false, false, "Fetches external repositories into a folder specified by the flag --vendor_dir.", ""),
    spec("version", false, false, false, "Prints version information for %{product}.", ""),
];

/// Looks up a command by exact name (Bazel has no command abbreviations).
pub fn command_spec(name: &str) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|c| c.name == name)
}

static PRODUCT_NAME: OnceLock<String> = OnceLock::new();

/// `--product_name` (default `bazel`): the name used in help, info keys and messages.
pub fn product_name() -> &'static str {
    PRODUCT_NAME.get().map(String::as_str).unwrap_or("bazel")
}

/// Sets the product name once (first caller wins), from the server's `--product_name`.
pub fn set_product_name(name: &str) {
    if !name.is_empty() {
        let _ = PRODUCT_NAME.set(name.to_string());
    }
}

static INSTALL_MD5: OnceLock<String> = OnceLock::new();

/// The server's `--install_md5` (digest of the binary the install base was created from).
pub fn install_md5() -> Option<&'static str> {
    INSTALL_MD5.get().map(String::as_str)
}

pub fn set_install_md5(md5: &str) {
    if !md5.is_empty() {
        let _ = INSTALL_MD5.set(md5.to_string());
    }
}

/// `%{product}` expansion used by short descriptions and help templates.
pub fn expand_product(s: &str) -> String {
    s.replace("%{product}", product_name())
}

/// The error `BlazeOptionHandler.checkCwdInWorkspace` reports for a workspace-only command.
pub fn not_in_workspace_message(command: &str) -> String {
    let p = product_name();
    let prefix = if p.is_empty() || p == "bazel" {
        "Bazel".to_string()
    } else {
        let mut c = p.chars();
        match c.next() {
            None => String::new(),
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        }
    };
    format!(
        "{prefix} '{command}' must be invoked from within a workspace \
         (below a directory having a MODULE.bazel file).\n\
         See documentation at https://bazel.build/concepts/build-ref#workspace"
    )
}
