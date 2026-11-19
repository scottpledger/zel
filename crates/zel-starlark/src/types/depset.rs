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
        Self::new_checked(order, direct, transitive, true)
    }

    pub fn new_checked(
        order: DepsetOrder,
        direct: Vec<Value>,
        transitive: Vec<Arc<Depset>>,
        always_check_elements: bool,
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

        // Element validation & homogeneity check (mirrors Bazel Depset.checkElement)
        let mut first_type: Option<&'static str> = None;
        let mut dedup_direct = Vec::with_capacity(direct.len());
        #[allow(clippy::mutable_key_type)]
        let mut seen_direct = HashSet::new();
        for item in direct {
            check_depset_element(&item, always_check_elements)?;
            let tname = item.type_name();
            match first_type {
                None => first_type = Some(tname),
                Some(ft) if ft != tname => {
                    return Err(StarlarkError(format!(
                        "depset elements must all have the same type, got '{ft}' and '{tname}'"
                    )));
                }
                _ => {}
            }
            if seen_direct.insert(item.clone()) {
                dedup_direct.push(item);
            }
        }

        // NestedSet lays out each node's children according to its own order at construction:
        // LINK_ORDER (Topological) reverses both direct elements and transitive sets (before dedup).
        let stored_transitive = if order == DepsetOrder::Topological {
            dedup_direct.reverse();
            let mut seen_t = HashSet::new();
            let mut out_t = Vec::with_capacity(transitive.len());
            for t in transitive.into_iter().rev() {
                if seen_t.insert(t.id) {
                    out_t.push(t);
                }
            }
            out_t
        } else {
            let mut seen_t = HashSet::new();
            let mut out_t = Vec::with_capacity(transitive.len());
            for t in transitive {
                if seen_t.insert(t.id) {
                    out_t.push(t);
                }
            }
            out_t
        };

        let id = DEPSET_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        Ok(Self {
            id,
            order,
            direct: dedup_direct,
            transitive: stored_transitive,
        })
    }

    /// Whether the set has no elements at all (Depset.truth() is !isEmpty()).
    pub fn is_empty(&self) -> bool {
        self.direct.is_empty() && self.transitive.iter().all(|t| t.is_empty())
    }

    pub fn to_list(&self) -> Vec<Value> {
        let mut raw = Vec::new();
        let mut visited_sets = HashSet::new();
        self.collect_nested(&mut raw, &mut visited_sets);

        let mut deduped = Vec::new();
        #[allow(clippy::mutable_key_type)]
        let mut seen = HashSet::new();
        for item in raw {
            if seen.insert(item.clone()) {
                deduped.push(item);
            }
        }
        if self.order == DepsetOrder::Topological {
            deduped.reverse();
        }
        deduped
    }

    fn collect_nested(&self, out: &mut Vec<Value>, visited: &mut HashSet<u64>) {
        if !visited.insert(self.id) {
            return;
        }
        if self.order == DepsetOrder::Preorder {
            out.extend(self.direct.iter().cloned());
            for t in &self.transitive {
                t.collect_nested(out, visited);
            }
        } else {
            for t in &self.transitive {
                t.collect_nested(out, visited);
            }
            out.extend(self.direct.iter().cloned());
        }
    }
}

fn check_depset_element(x: &Value, strict: bool) -> Result<(), StarlarkError> {
    if strict && !x.is_immutable() {
        return Err(StarlarkError(
            "depset elements must not be mutable values".to_string(),
        ));
    }
    if matches!(x, Value::List(_) | Value::Dict(_) | Value::Set(_)) {
        return Err(StarlarkError(format!(
            "depsets cannot contain items of type '{}'",
            x.type_name()
        )));
    }
    Ok(())
}

impl fmt::Display for Depset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let items: Vec<String> = self.to_list().into_iter().map(|v| v.repr()).collect();
        write!(f, "depset([{}])", items.join(", "))
    }
}
