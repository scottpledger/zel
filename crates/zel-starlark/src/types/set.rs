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

/// Insertion-ordered set supporting any hashable elements.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StarlarkSet {
    pub elements: Vec<Value>,
    pub frozen: bool,
    pub iter_count: usize,
}

impl StarlarkSet {
    pub fn new() -> Self {
        Self {
            elements: Vec::new(),
            frozen: false,
            iter_count: 0,
        }
    }

    pub fn from_elements(elements: Vec<Value>) -> Result<Self, StarlarkError> {
        let mut set = Self::new();
        for elem in elements {
            set.add(elem)?;
        }
        Ok(set)
    }

    pub fn check_mutable(&self) -> Result<(), StarlarkError> {
        if self.frozen {
            return Err(StarlarkError("trying to mutate a frozen set value".to_string()));
        }
        if self.iter_count > 0 {
            return Err(StarlarkError(
                "set value is temporarily immutable due to active for-loop iteration".to_string(),
            ));
        }
        Ok(())
    }

    pub fn freeze(&mut self) {
        if !self.frozen {
            self.frozen = true;
            for elem in &self.elements {
                elem.freeze();
            }
        }
    }

    pub fn len(&self) -> usize {
        self.elements.len()
    }

    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    pub fn contains(&self, item: &Value) -> bool {
        self.elements.iter().any(|x| x == item)
    }

    pub fn add(&mut self, item: Value) -> Result<(), StarlarkError> {
        self.check_mutable()?;
        if !item.is_hashable() {
            return Err(StarlarkError(format!("unhashable type: '{}'", item.type_name())));
        }
        if !self.contains(&item) {
            self.elements.push(item);
        }
        Ok(())
    }

    pub fn clear(&mut self) -> Result<(), StarlarkError> {
        self.check_mutable()?;
        self.elements.clear();
        Ok(())
    }

    pub fn discard(&mut self, item: &Value) -> Result<(), StarlarkError> {
        self.check_mutable()?;
        if let Some(pos) = self.elements.iter().position(|x| x == item) {
            self.elements.remove(pos);
        }
        Ok(())
    }

    pub fn remove(&mut self, item: &Value) -> Result<(), StarlarkError> {
        self.check_mutable()?;
        if let Some(pos) = self.elements.iter().position(|x| x == item) {
            self.elements.remove(pos);
            Ok(())
        } else {
            Err(StarlarkError(format!("item {item:?} not found in set")))
        }
    }

    pub fn pop(&mut self) -> Result<Value, StarlarkError> {
        self.check_mutable()?;
        self.elements
            .pop()
            .ok_or_else(|| StarlarkError("pop(): set is empty".to_string()))
    }

    pub fn issubset(&self, other: &Self) -> bool {
        self.elements.iter().all(|x| other.contains(x))
    }

    pub fn issuperset(&self, other: &Self) -> bool {
        other.issubset(self)
    }

    pub fn isdisjoint(&self, other: &Self) -> bool {
        !self.elements.iter().any(|x| other.contains(x))
    }

    pub fn union(&self, other: &Self) -> Self {
        let mut res = self.clone();
        res.frozen = false;
        res.iter_count = 0;
        for elem in &other.elements {
            if !res.contains(elem) {
                res.elements.push(elem.clone());
            }
        }
        res
    }

    pub fn intersection(&self, other: &Self) -> Self {
        let mut res = Self::new();
        for elem in &self.elements {
            if other.contains(elem) {
                res.elements.push(elem.clone());
            }
        }
        res
    }

    pub fn difference(&self, other: &Self) -> Self {
        let mut res = Self::new();
        for elem in &self.elements {
            if !other.contains(elem) {
                res.elements.push(elem.clone());
            }
        }
        res
    }

    pub fn symmetric_difference(&self, other: &Self) -> Self {
        let mut res = Self::new();
        for elem in &self.elements {
            if !other.contains(elem) {
                res.elements.push(elem.clone());
            }
        }
        for elem in &other.elements {
            if !self.contains(elem) {
                res.elements.push(elem.clone());
            }
        }
        res
    }

    pub fn update(&mut self, other: &Self) -> Result<(), StarlarkError> {
        self.check_mutable()?;
        for elem in &other.elements {
            self.add(elem.clone())?;
        }
        Ok(())
    }

    pub fn difference_update(&mut self, other: &Self) -> Result<(), StarlarkError> {
        self.check_mutable()?;
        self.elements.retain(|x| !other.contains(x));
        Ok(())
    }

    pub fn intersection_update(&mut self, other: &Self) -> Result<(), StarlarkError> {
        self.check_mutable()?;
        self.elements.retain(|x| other.contains(x));
        Ok(())
    }

    pub fn symmetric_difference_update(&mut self, other: &Self) -> Result<(), StarlarkError> {
        self.check_mutable()?;
        let sym = self.symmetric_difference(other);
        self.elements = sym.elements;
        Ok(())
    }
}
