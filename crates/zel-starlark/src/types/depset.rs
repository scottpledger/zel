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

static DEPSET_ID_COUNTER: AtomicU64 = AtomicU64::new(5);

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
            "default" => Ok(Self::Default),
            "postorder" => Ok(Self::Postorder),
            "preorder" => Ok(Self::Preorder),
            "topological" => Ok(Self::Topological),
            other => Err(StarlarkError(format!("Invalid order: {other}"))),
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

    fn empty_id(self) -> u64 {
        match self {
            Self::Default => 1,
            Self::Postorder => 2,
            Self::Preorder => 3,
            Self::Topological => 4,
        }
    }
}

/// Bazel `depset` value supporting O(1) transitive nesting and ordered deduplicated flattening.
#[derive(Debug)]
pub struct Depset {
    pub id: u64,
    pub order: DepsetOrder,
    node_order: DepsetOrder,
    pub direct: Vec<Value>,
    pub transitive: Vec<Arc<Depset>>,
    pub elem_type: Option<String>,
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
        // Incompatible order & transitive element type check (skipping empty transitive sets)
        let mut first_type: Option<String> = None;
        for t in &transitive {
            if t.is_empty() {
                continue;
            }
            if t.order != DepsetOrder::Default && order != DepsetOrder::Default && t.order != order
            {
                return Err(StarlarkError(format!(
                    "Order '{}' is incompatible with transitive depset order '{}'",
                    order.as_str(),
                    t.order.as_str()
                )));
            }
            if let Some(tt) = &t.elem_type {
                match &first_type {
                    None => first_type = Some(tt.clone()),
                    Some(ft) if ft != tt => {
                        return Err(StarlarkError(format!(
                            "cannot add an item of type '{tt}' to a depset of '{ft}'"
                        )));
                    }
                    _ => {}
                }
            }
        }

        // Element validation & homogeneity check (mirrors Bazel Depset.checkElement)
        let mut dedup_direct = Vec::with_capacity(direct.len());
        #[allow(clippy::mutable_key_type)]
        let mut seen_direct = HashSet::new();
        for item in direct {
            check_depset_element(&item, always_check_elements)?;
            let tname = depset_element_type(&item);
            match &first_type {
                None => first_type = Some(tname),
                Some(ft) if ft != &tname => {
                    return Err(StarlarkError(format!(
                        "cannot add an item of type '{tname}' to a depset of '{ft}'"
                    )));
                }
                _ => {}
            }
            if seen_direct.insert(item.clone()) {
                dedup_direct.push(item);
            }
        }

        let non_empty_transitive: Vec<Arc<Depset>> =
            transitive.into_iter().filter(|t| !t.is_empty()).collect();

        // NestedSet lays out each node's children according to its own order at construction:
        // LINK_ORDER (Topological) reverses both direct elements and transitive sets (before dedup).
        let stored_transitive = if order == DepsetOrder::Topological {
            dedup_direct.reverse();
            let mut seen_t = HashSet::new();
            let mut out_t = Vec::with_capacity(non_empty_transitive.len());
            for t in non_empty_transitive.into_iter().rev() {
                if seen_t.insert(t.id) {
                    out_t.push(t);
                }
            }
            out_t
        } else {
            let mut seen_t = HashSet::new();
            let mut out_t = Vec::with_capacity(non_empty_transitive.len());
            for t in non_empty_transitive {
                if seen_t.insert(t.id) {
                    out_t.push(t);
                }
            }
            out_t
        };

        if dedup_direct.is_empty() && stored_transitive.is_empty() {
            return Ok(Self {
                id: order.empty_id(),
                order,
                node_order: order,
                direct: Vec::new(),
                transitive: Vec::new(),
                elem_type: None,
            });
        }
        if stored_transitive.len() == 1
            && (dedup_direct.is_empty()
                || (dedup_direct.len() == 1
                    && stored_transitive[0].transitive.is_empty()
                    && stored_transitive[0].direct.len() == 1
                    && stored_transitive[0].direct[0] == dedup_direct[0]))
        {
            let t0 = &stored_transitive[0];
            return Ok(Self {
                id: t0.id,
                order,
                node_order: t0.node_order,
                direct: t0.direct.clone(),
                transitive: t0.transitive.clone(),
                elem_type: first_type,
            });
        }
        let id = DEPSET_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        Ok(Self {
            id,
            order,
            node_order: order,
            direct: dedup_direct,
            transitive: stored_transitive,
            elem_type: first_type,
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
        if self.node_order == DepsetOrder::Preorder {
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

fn depset_element_type(x: &Value) -> String {
    match x {
        Value::ProviderInstance { provider_name, .. } => provider_name.clone(),
        Value::Host(kind, _) => kind.clone(),
        other => other.type_name().to_string(),
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
        if self.order == DepsetOrder::Default {
            write!(f, "depset([{}])", items.join(", "))
        } else {
            write!(
                f,
                "depset([{}], order = {:?})",
                items.join(", "),
                self.order.as_str()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depset_parity_repr_identity_and_transitive_types() {
        assert!(DepsetOrder::parse("stable").is_err());
        assert_eq!(
            DepsetOrder::parse("bogus").unwrap_err().0,
            "Invalid order: bogus"
        );
        let e1 = Depset::new(DepsetOrder::Preorder, vec![], vec![]).unwrap();
        let e2 = Depset::new(DepsetOrder::Preorder, vec![], vec![]).unwrap();
        let e_def = Depset::new(DepsetOrder::Default, vec![], vec![]).unwrap();
        assert_eq!(e1, e2);
        assert_ne!(e1, e_def);
        assert_eq!(e1.to_string(), "depset([], order = \"preorder\")");
        assert_eq!(e_def.to_string(), "depset([])");

        // Empty transitive depset of different order is skipped in compatibility check.
        let post_with_empty_pre =
            Depset::new(DepsetOrder::Postorder, vec![], vec![Arc::new(e1)]).unwrap();
        assert_eq!(
            post_with_empty_pre.to_string(),
            "depset([], order = \"postorder\")"
        );

        // Singleton reuse and transitive element type homogeneity.
        let s1 = Arc::new(
            Depset::new(
                DepsetOrder::Default,
                vec![Value::String("x".into())],
                vec![],
            )
            .unwrap(),
        );
        let s2 = Depset::new(DepsetOrder::Default, vec![], vec![s1.clone()]).unwrap();
        assert_eq!(*s1, s2);
        let bad = Depset::new(DepsetOrder::Default, vec![Value::Int(1.into())], vec![s1]);
        assert!(bad.is_err());
    }
}
