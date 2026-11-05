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

//! Cycle bookkeeping and Bazel-format cycle reports (`CycleInfo`, `CyclesReporter`,
//! `AbstractLabelCycleReporter.printCycle`).

use crate::SkyKey;
use std::collections::HashSet;

/// A dependency cycle as seen from one top-level key (`CycleInfo`): the keys leading from the
/// top-level key to the cycle (exclusive) and the cycle itself. The cycle starts at the node
/// where the path enters it and is *not* closed (the first key is implicitly repeated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleInfo {
    pub path_to_cycle: Vec<SkyKey>,
    pub cycle: Vec<SkyKey>,
}

impl CycleInfo {
    pub fn new(path_to_cycle: Vec<SkyKey>, cycle: Vec<SkyKey>) -> Self {
        Self { path_to_cycle, cycle }
    }

    /// Cycle rotated so that it starts at `idx` (every member sees the cycle from itself).
    pub fn rotated(cycle: &[SkyKey], idx: usize) -> Vec<SkyKey> {
        let mut v = cycle[idx..].to_vec();
        v.extend_from_slice(&cycle[..idx]);
        v
    }

    /// `CycleDeduper` identity: the *set* of keys on the cycle.
    pub fn identity(&self) -> Vec<SkyKey> {
        let mut v = self.cycle.clone();
        v.sort();
        v
    }
}

/// `AbstractLabelCycleReporter.printCycle`: appends the cycle block to `msg` and returns the key
/// the cycle is reported at (its first element).
pub fn print_cycle(cycle: &[SkyKey], msg: &mut String, pretty: &dyn Fn(&SkyKey) -> String) -> SkyKey {
    assert!(!cycle.is_empty());
    let first = &cycle[0];
    if cycle.len() == 1 {
        msg.push_str("\n.-> ");
        msg.push_str(&pretty(first));
        msg.push_str(" [self-edge]");
        msg.push_str("\n`--'");
        return first.clone();
    }
    msg.push_str("\n.-> ");
    msg.push_str(&pretty(first));
    for k in &cycle[1..] {
        msg.push_str("\n|   ");
        msg.push_str(&pretty(k));
    }
    msg.push_str("\n`-- ");
    msg.push_str(&pretty(first));
    first.clone()
}

/// Message text of one cycle (everything after the `in <kind> <label>: ` prefix in Bazel).
pub fn format_cycle_message(info: &CycleInfo, pretty: &dyn Fn(&SkyKey) -> String) -> (String, SkyKey) {
    let mut msg = String::from("cycle in dependency graph:");
    for k in &info.path_to_cycle {
        msg.push_str("\n    ");
        msg.push_str(&pretty(k));
    }
    let at = print_cycle(&info.cycle, &mut msg, pretty);
    (msg, at)
}

/// Reports the cycles of top-level keys with `CyclesReporter` semantics: each distinct cycle is
/// printed in full once; later top-level keys reaching an already-reported cycle get the
/// "target depends on an already-reported cycle" line.
///
/// `describe(key)` returns `(location_prefix, kind_and_label)` such as
/// `("/ws/a/BUILD:1:8", "cc_library rule //a:a")`; the reporter produces
/// `ERROR: <location>: in <kind and label>: <message>` lines.
pub struct CyclesReporter {
    seen: HashSet<Vec<SkyKey>>,
}

impl Default for CyclesReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl CyclesReporter {
    pub fn new() -> Self {
        Self { seen: HashSet::new() }
    }

    pub fn report(
        &mut self,
        top_level: &SkyKey,
        info: &CycleInfo,
        pretty: &dyn Fn(&SkyKey) -> String,
        describe: &dyn Fn(&SkyKey) -> (String, String),
    ) -> String {
        if !self.seen.insert(info.identity()) {
            let (loc, what) = describe(top_level);
            return format!(
                "ERROR: {loc}: in {what}: cycle in dependency graph: target depends on an already-reported cycle"
            );
        }
        let (msg, at) = format_cycle_message(info, pretty);
        let (loc, what) = describe(&at);
        format!("ERROR: {loc}: in {what}: {msg}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> SkyKey {
        SkyKey::Precomputed(s.to_string())
    }
    fn pretty(k: &SkyKey) -> String {
        match k {
            SkyKey::Precomputed(s) => format!("//x:{s}"),
            _ => "?".into(),
        }
    }

    #[test]
    fn bazel_cycle_text() {
        let info = CycleInfo::new(vec![k("top"), k("mid")], vec![k("a"), k("b"), k("c")]);
        let (m, at) = format_cycle_message(&info, &pretty);
        assert_eq!(
            m,
            "cycle in dependency graph:\n    //x:top\n    //x:mid\n.-> //x:a\n|   //x:b\n|   //x:c\n`-- //x:a"
        );
        assert_eq!(at, k("a"));
    }

    #[test]
    fn self_edge_text() {
        let info = CycleInfo::new(vec![], vec![k("a")]);
        let (m, _) = format_cycle_message(&info, &pretty);
        assert_eq!(m, "cycle in dependency graph:\n.-> //x:a [self-edge]\n`--'");
    }

    #[test]
    fn reporter_dedupes_cycles_across_top_level_keys() {
        let mut r = CyclesReporter::new();
        let d = |key: &SkyKey| (format!("/ws/BUILD:{}", pretty(key).len()), format!("cc_library rule {}", pretty(key)));
        let c1 = CycleInfo::new(vec![k("t1")], vec![k("a"), k("b")]);
        let c2 = CycleInfo::new(vec![k("t2")], vec![k("b"), k("a")]); // same cycle, other entry point
        let m1 = r.report(&k("t1"), &c1, &pretty, &d);
        assert!(m1.starts_with("ERROR: /ws/BUILD:"));
        assert!(m1.contains("in cc_library rule //x:a: cycle in dependency graph:\n    //x:t1\n.-> //x:a\n|   //x:b\n`-- //x:a"));
        let m2 = r.report(&k("t2"), &c2, &pretty, &d);
        assert!(m2.ends_with("cycle in dependency graph: target depends on an already-reported cycle"), "{m2}");
    }

    #[test]
    fn rotation_and_identity() {
        let c = vec![k("a"), k("b"), k("c")];
        assert_eq!(CycleInfo::rotated(&c, 1), vec![k("b"), k("c"), k("a")]);
        let x = CycleInfo::new(vec![], c.clone());
        let y = CycleInfo::new(vec![k("p")], CycleInfo::rotated(&c, 2));
        assert_eq!(x.identity(), y.identity());
    }
}
