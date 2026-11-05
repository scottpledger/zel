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

//! Reporting of a failed top-level analysis like `SkyframeErrorProcessor` / `ConfiguredTargetFunction`:
//! the rule's error events, then (for rule-reported errors) `<loc>: Analysis of target '<failing>'
//! (config: <hash>) failed`, and the `Analysis of target '<root>' failed; build aborted[: Analysis failed]`
//! trailer (the suffix is the cause message when the failing target is not the root itself).

use zel_cmdline::Label;

/// The diagnostics of one failed top-level target.
pub struct AnalysisFailure {
    /// `ERROR: ...` event lines (continuation lines of multi-line messages stay unprefixed).
    pub events: Vec<String>,
    /// `: Analysis failed` or empty: the cause suffix of the `failed; build aborted` message.
    pub abort_suffix: &'static str,
}

/// The label of the rule that reported `error` (`in <kind> rule <label>:`), if any.
fn failing_rule(error: &str) -> Option<String> {
    let first = error.lines().next()?;
    let idx = first.find(" rule ")?;
    let rest = &first[idx + " rule ".len()..];
    let end = rest.find(": ")?;
    let l = &rest[..end];
    // `in <kind>` must precede ("... in cc_library rule //x:y: ...").
    if !first[..idx].contains(" in ") && !first[..idx].starts_with("in ") {
        return None;
    }
    if l.contains(' ') {
        return None;
    }
    Some(l.to_string())
}

/// Builds the report for `error` raised while analysing `root`; `rule_loc` gives the BUILD location
/// of a rule label and `config` is the short checksum of the failing configuration.
pub fn analysis_failure(
    error: &str,
    root: &Label,
    config: &str,
    rule_loc: &dyn Fn(&str) -> Option<String>,
) -> AnalysisFailure {
    let mut events = Vec::new();
    for (i, line) in error.lines().enumerate() {
        if i == 0 && !line.starts_with("ERROR:") {
            events.push(format!("ERROR: {line}"));
        } else {
            events.push(line.to_string());
        }
    }
    let is_cycle = error.contains("cycle in dependency graph");
    let referenced = error.contains(" and referenced by ");
    let root_s = root.to_string();
    let failing = failing_rule(error).unwrap_or_else(|| root_s.clone());
    if !is_cycle && !referenced {
        if let Some(loc) = rule_loc(&failing) {
            events.push(format!("ERROR: {loc}: Analysis of target '{failing}' (config: {config}) failed"));
        }
    }
    let abort_suffix = if referenced || (!is_cycle && failing != root_s) { ": Analysis failed" } else { "" };
    AnalysisFailure { events, abort_suffix }
}
