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
        Self {
            path_to_cycle,
            cycle,
        }
    }

    /// Cycle rotated so that it starts at `idx` (every member sees the cycle from itself).
    pub fn rotated(cycle: &[SkyKey], idx: usize) -> Vec<SkyKey> {
        if cycle.is_empty() {
            return Vec::new();
        }
        let i = idx % cycle.len();
        let mut v = cycle[i..].to_vec();
        v.extend_from_slice(&cycle[..i]);
        v
    }

    /// `CycleDeduper` canonical rotation: rotates `cycle` so the minimum element (by `Ord`,
    /// keeping the earliest occurrence on ties) comes first. Rotations of the same directed cycle
    /// share an identity while distinct directed cycles over the same vertex set (`A->B->C` vs
    /// `A->C->B`) remain distinct.
    pub fn identity(&self) -> Vec<SkyKey> {
        if self.cycle.is_empty() {
            return Vec::new();
        }
        let mut min_idx = 0usize;
        for (idx, k) in self.cycle.iter().enumerate().skip(1) {
            if k < &self.cycle[min_idx] {
                min_idx = idx;
            }
        }
        Self::rotated(&self.cycle, min_idx)
    }

    /// `CycleInfo.normalizeCycle`: returns `None` when `cycle` is empty; if any element of
    /// `path_to_cycle` already appears on `cycle`, trims `path_to_cycle` to the prefix before that
    /// entry point and rotates `cycle` to start at that entry point.
    pub fn normalize_cycle(path_to_cycle: Vec<SkyKey>, cycle: Vec<SkyKey>) -> Option<Self> {
        if cycle.is_empty() {
            return None;
        }
        for (p_idx, pk) in path_to_cycle.iter().enumerate() {
            if let Some(c_idx) = cycle.iter().position(|ck| ck == pk) {
                return Some(Self {
                    path_to_cycle: path_to_cycle[..p_idx].to_vec(),
                    cycle: Self::rotated(&cycle, c_idx),
                });
            }
        }
        Some(Self {
            path_to_cycle,
            cycle,
        })
    }

    /// `CycleInfo.prepareCycles`: prepends `parent` to each cycle's `path_to_cycle` and normalizes
    /// (handling the case where `parent` is itself on the cycle).
    pub fn prepare_cycles(parent: &SkyKey, cycles: &[CycleInfo]) -> Vec<CycleInfo> {
        let mut out = Vec::with_capacity(cycles.len());
        for c in cycles {
            if let Some(c_idx) = c.cycle.iter().position(|k| k == parent) {
                out.push(CycleInfo {
                    path_to_cycle: Vec::new(),
                    cycle: Self::rotated(&c.cycle, c_idx),
                });
            } else if let Some(p_idx) = c.path_to_cycle.iter().position(|k| k == parent) {
                out.push(CycleInfo {
                    path_to_cycle: c.path_to_cycle[p_idx..].to_vec(),
                    cycle: c.cycle.clone(),
                });
            } else {
                let mut path = Vec::with_capacity(1 + c.path_to_cycle.len());
                path.push(parent.clone());
                path.extend(c.path_to_cycle.iter().cloned());
                if let Some(norm) = Self::normalize_cycle(path, c.cycle.clone()) {
                    out.push(norm);
                }
            }
        }
        out
    }
}

/// `AbstractLabelCycleReporter.printCycle`: appends the cycle block to `msg` and returns the key
/// the cycle is reported at (its first element).
pub fn print_cycle(
    cycle: &[SkyKey],
    msg: &mut String,
    pretty: &dyn Fn(&SkyKey) -> String,
) -> SkyKey {
    assert!(!cycle.is_empty(), "cycle must not be empty");
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
pub fn format_cycle_message(
    info: &CycleInfo,
    pretty: &dyn Fn(&SkyKey) -> String,
) -> (String, SkyKey) {
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
        Self {
            seen: HashSet::new(),
        }
    }

    pub fn report(
        &mut self,
        top_level: &SkyKey,
        info: &CycleInfo,
        pretty: &dyn Fn(&SkyKey) -> String,
        describe: &dyn Fn(&SkyKey) -> (String, String),
    ) -> String {
        if info.cycle.is_empty() {
            return String::new();
        }
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
        let d = |key: &SkyKey| {
            (
                format!("/ws/BUILD:{}", pretty(key).len()),
                format!("cc_library rule {}", pretty(key)),
            )
        };
        let c1 = CycleInfo::new(vec![k("t1")], vec![k("a"), k("b")]);
        let c2 = CycleInfo::new(vec![k("t2")], vec![k("b"), k("a")]); // same cycle, other entry point
        let m1 = r.report(&k("t1"), &c1, &pretty, &d);
        assert!(m1.starts_with("ERROR: /ws/BUILD:"));
        assert!(m1.contains("in cc_library rule //x:a: cycle in dependency graph:\n    //x:t1\n.-> //x:a\n|   //x:b\n`-- //x:a"));
        let m2 = r.report(&k("t2"), &c2, &pretty, &d);
        assert!(
            m2.ends_with("cycle in dependency graph: target depends on an already-reported cycle"),
            "{m2}"
        );
    }

    #[test]
    fn rotation_and_identity() {
        let c = vec![k("a"), k("b"), k("c")];
        assert_eq!(CycleInfo::rotated(&c, 1), vec![k("b"), k("c"), k("a")]);
        let x = CycleInfo::new(vec![], c.clone());
        let y = CycleInfo::new(vec![k("p")], CycleInfo::rotated(&c, 2));
        assert_eq!(x.identity(), y.identity());

        // Opposite directed cycle over the same vertices is a distinct cycle!
        let rev = CycleInfo::new(vec![], vec![k("a"), k("c"), k("b")]);
        assert_ne!(x.identity(), rev.identity());

        // normalize_cycle trims overlapping path_to_cycle and rotates cycle to entry.
        let norm = CycleInfo::normalize_cycle(vec![k("top"), k("b")], c.clone()).unwrap();
        assert_eq!(norm.path_to_cycle, vec![k("top")]);
        assert_eq!(norm.cycle, vec![k("b"), k("c"), k("a")]);
        assert!(CycleInfo::normalize_cycle(vec![k("top")], vec![]).is_none());
    }
}
