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

use crate::types::list::StarlarkError;
use crate::types::value::Value;
use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static DEPSET_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Traversal order for `depset` (`NestedSet` in Java).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum DepsetOrder {
    #[default]
    Default,
    Postorder,
    Preorder,
    Topological,
}

impl DepsetOrder {
    pub fn parse(s: &str) -> Result<Self, StarlarkError> {
        match s {
            "default" | "stable" => Ok(Self::Default),
            "postorder" | "compile" => Ok(Self::Postorder),
            "preorder" | "naive_link" => Ok(Self::Preorder),
            "topological" | "link" => Ok(Self::Topological),
            other => Err(StarlarkError(format!("Invalid depset order: '{other}'"))),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Postorder => "postorder",
            Self::Preorder => "preorder",
            Self::Topological => "topological",
        }
    }
}

/// Bazel `depset` value supporting O(1) transitive nesting and ordered deduplicated flattening.
#[derive(Debug)]
pub struct Depset {
    pub id: u64,
    pub order: DepsetOrder,
    pub direct: Vec<Value>,
    pub transitive: Vec<Arc<Depset>>,
}

impl PartialEq for Depset {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Depset {}

impl std::hash::Hash for Depset {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl Depset {
    pub fn new(
        order: DepsetOrder,
        direct: Vec<Value>,
        transitive: Vec<Arc<Depset>>,
    ) -> Result<Self, StarlarkError> {
        // Incompatible order check
        for t in &transitive {
            if t.order != DepsetOrder::Default && order != DepsetOrder::Default && t.order != order
            {
                return Err(StarlarkError(format!(
                    "Order '{}' is incompatible with transitive depset order '{}'",
                    order.as_str(),
                    t.order.as_str()
                )));
            }
        }

        // Element homogeneity and hashability check
        if let Some(first) = direct.first() {
            if !first.is_hashable() {
                return Err(StarlarkError(format!(
                    "depset elements must be hashable, got unhashable '{}'",
                    first.type_name()
                )));
            }
            let first_type = first.type_name();
            for item in direct.iter().skip(1) {
                if !item.is_hashable() {
                    return Err(StarlarkError(format!(
                        "depset elements must be hashable, got unhashable '{}'",
                        item.type_name()
                    )));
                }
                if item.type_name() != first_type {
                    return Err(StarlarkError(format!(
                        "depset elements must all have the same type, got '{}' and '{}'",
                        first_type,
                        item.type_name()
                    )));
                }
            }
        }

        let id = DEPSET_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        Ok(Self {
            id,
            order,
            direct,
            transitive,
        })
    }

    /// Whether the set has no elements at all (Depset.truth() is !isEmpty()).
    pub fn is_empty(&self) -> bool {
        self.direct.is_empty() && self.transitive.iter().all(|t| t.is_empty())
    }

    pub fn to_list(&self) -> Vec<Value> {
        let mut raw = Vec::new();
        let mut visited_sets = HashSet::new();
        match self.order {
            DepsetOrder::Topological => {
                // Topological sort: right-to-left postorder, then reverse
                self.collect_right_to_left_postorder(&mut raw, &mut visited_sets);
                raw.reverse();
            }
            // `NestedSet`: every node's children array is laid out according to *its own* order
            // when it is built (preorder: direct members first; stable/postorder: transitive
            // members first), so nodes of different orders can be nested.
            _ => self.collect_nested(&mut raw, &mut visited_sets),
        }

        let mut deduped = Vec::new();
        let mut seen = HashSet::new();
        for item in raw {
            if seen.insert(item.clone()) {
                deduped.push(item);
            }
        }
        deduped
    }

    fn collect_nested(&self, out: &mut Vec<Value>, visited: &mut HashSet<u64>) {
        if !visited.insert(self.id) {
            return;
        }
        let transitive = |out: &mut Vec<Value>, visited: &mut HashSet<u64>| {
            for t in &self.transitive {
                if t.order == DepsetOrder::Topological {
                    let mut sub = Vec::new();
                    t.collect_right_to_left_postorder(&mut sub, visited);
                    sub.reverse();
                    out.extend(sub);
                } else {
                    t.collect_nested(out, visited);
                }
            }
        };
        if self.order == DepsetOrder::Preorder {
            out.extend(self.direct.iter().cloned());
            transitive(out, visited);
        } else {
            transitive(out, visited);
            out.extend(self.direct.iter().cloned());
        }
    }

    #[allow(dead_code)]
    fn collect_postorder(&self, out: &mut Vec<Value>, visited: &mut HashSet<u64>) {
        if !visited.insert(self.id) {
            return;
        }
        for t in &self.transitive {
            t.collect_postorder(out, visited);
        }
        for d in &self.direct {
            out.push(d.clone());
        }
    }

    #[allow(dead_code)]
    fn collect_preorder(&self, out: &mut Vec<Value>, visited: &mut HashSet<u64>) {
        if !visited.insert(self.id) {
            return;
        }
        for d in &self.direct {
            out.push(d.clone());
        }
        for t in &self.transitive {
            t.collect_preorder(out, visited);
        }
    }

    fn collect_right_to_left_postorder(&self, out: &mut Vec<Value>, visited: &mut HashSet<u64>) {
        if !visited.insert(self.id) {
            return;
        }
        for t in self.transitive.iter().rev() {
            t.collect_right_to_left_postorder(out, visited);
        }
        for d in self.direct.iter().rev() {
            out.push(d.clone());
        }
    }
}

impl fmt::Display for Depset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let items: Vec<String> = self.to_list().into_iter().map(|v| v.repr()).collect();
        write!(f, "depset([{}])", items.join(", "))
    }
}
