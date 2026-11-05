//! Structural model of Bazel's `NestedSet` (`NestedSet.java` constructor / `NestedSetBuilder`),
//! enough to reproduce `NestedSetFingerprintCache` (action keys fingerprint the nesting).

use std::sync::Arc;

/// `Order.ordinal()`.
pub const STABLE_ORDER: i32 = 0;
pub const COMPILE_ORDER: i32 = 1;

/// A child of a set: a direct element or a (>= 2 element) transitive array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NNode<E> {
    Leaf(E),
    Arr(Arc<Vec<NNode<E>>>),
}

/// A nested set: `children == None` is the empty set; a single element is stored as a bare leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NSet<E> {
    pub order: i32,
    pub children: Option<NNode<E>>,
}

impl<E> Default for NSet<E> {
    fn default() -> Self {
        NSet { order: COMPILE_ORDER, children: None }
    }
}

impl<E: Clone + PartialEq> NSet<E> {
    pub fn empty(order: i32) -> Self {
        NSet { order, children: None }
    }

    pub fn is_empty(&self) -> bool {
        self.children.is_none()
    }

    /// `NestedSetBuilder.build()` of a builder with `direct` items (`add`/`addAll`, deduplicated)
    /// and `transitive` sets (`addTransitive`, empty sets ignored, identical arrays collapsed).
    pub fn build(order: i32, direct: Vec<E>, transitive: Vec<NSet<E>>) -> Self {
        let mut d: Vec<E> = Vec::new();
        for e in direct {
            if !d.contains(&e) {
                d.push(e);
            }
        }
        let mut t: Vec<NSet<E>> = Vec::new();
        for s in transitive {
            if s.is_empty() {
                continue;
            }
            // `CompactHashSet<NestedSet>`: identity (`NestedSet` has no `equals`).
            let dup = t.iter().any(|x| match (&x.children, &s.children) {
                (Some(NNode::Arr(a)), Some(NNode::Arr(b))) => Arc::ptr_eq(a, b),
                _ => false,
            });
            if !dup {
                t.push(s);
            }
        }
        if d.is_empty() && t.is_empty() {
            return Self::empty(order);
        }
        if t.len() == 1 && d.len() <= 1 {
            let cand = &t[0];
            if cand.order == order {
                let reuse = match (d.first(), &cand.children) {
                    (None, _) => true,
                    (Some(x), Some(NNode::Leaf(y))) => x == y,
                    _ => false,
                };
                if reuse {
                    return cand.clone();
                }
            }
        }
        // `NestedSet(order, direct, transitive)` for STABLE/COMPILE order (not preorder):
        // transitive children first, then the direct elements not already hoisted.
        let mut children: Vec<NNode<E>> = Vec::new();
        let mut hoisted: Vec<E> = Vec::new();
        for s in &t {
            match &s.children {
                Some(NNode::Arr(a)) => children.push(NNode::Arr(a.clone())),
                Some(NNode::Leaf(x)) => {
                    if !hoisted.contains(x) {
                        hoisted.push(x.clone());
                        children.push(NNode::Leaf(x.clone()));
                    }
                }
                None => {}
            }
        }
        for e in d {
            if !hoisted.contains(&e) {
                children.push(NNode::Leaf(e));
            }
        }
        let node = match children.len() {
            0 => return Self::empty(order),
            1 => children.pop().unwrap(),
            _ => NNode::Arr(Arc::new(children)),
        };
        NSet { order, children: Some(node) }
    }

    /// Flattened elements (traversal order, duplicates kept; only for tests/debugging).
    pub fn to_vec(&self) -> Vec<E> {
        fn walk<E: Clone>(n: &NNode<E>, out: &mut Vec<E>) {
            match n {
                NNode::Leaf(e) => out.push(e.clone()),
                NNode::Arr(a) => a.iter().for_each(|c| walk(c, out)),
            }
        }
        let mut v = Vec::new();
        if let Some(c) = &self.children {
            walk(c, &mut v);
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn singleton_transitive_is_hoisted_and_deduplicated() {
        let leaf = NSet::build(STABLE_ORDER, vec!["b"], vec![]);
        let s = NSet::build(STABLE_ORDER, vec!["b"], vec![leaf.clone()]);
        assert_eq!(s.to_vec(), vec!["b"]);
        let leaf2 = NSet::build(STABLE_ORDER, vec!["c"], vec![]);
        let s = NSet::build(STABLE_ORDER, vec!["a"], vec![leaf, leaf2]);
        assert_eq!(s.to_vec(), vec!["b", "c", "a"]);
    }
}
