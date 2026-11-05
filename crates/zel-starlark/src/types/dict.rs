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

/// Insertion-ordered dictionary supporting any hashable key.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StarlarkDict {
    pub entries: Vec<(Value, Value)>,
    pub frozen: bool,
    pub iter_count: usize,
}

impl StarlarkDict {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            frozen: false,
            iter_count: 0,
        }
    }

    pub fn from_entries(entries: Vec<(Value, Value)>) -> Result<Self, StarlarkError> {
        let mut dict = Self::new();
        for (k, v) in entries {
            if !k.is_hashable() {
                return Err(StarlarkError(format!("unhashable type: '{}'", k.type_name())));
            }
            dict.insert(k, v)?;
        }
        Ok(dict)
    }

    pub fn check_mutable(&self) -> Result<(), StarlarkError> {
        if self.frozen {
            return Err(StarlarkError("trying to mutate a frozen dict value".to_string()));
        }
        if self.iter_count > 0 {
            return Err(StarlarkError(
                "dict value is temporarily immutable due to active for-loop iteration".to_string(),
            ));
        }
        Ok(())
    }

    pub fn freeze(&mut self) {
        if !self.frozen {
            self.frozen = true;
            for (k, v) in &self.entries {
                k.freeze();
                v.freeze();
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, key: &Value) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn contains_key(&self, key: &Value) -> bool {
        self.entries.iter().any(|(k, _)| k == key)
    }

    pub fn insert(&mut self, key: Value, value: Value) -> Result<Option<Value>, StarlarkError> {
        self.check_mutable()?;
        if !key.is_hashable() {
            return Err(StarlarkError(format!("unhashable type: '{}'", key.type_name())));
        }
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == &key) {
            let old = std::mem::replace(&mut self.entries[pos].1, value);
            Ok(Some(old))
        } else {
            self.entries.push((key, value));
            Ok(None)
        }
    }

    pub fn clear(&mut self) -> Result<(), StarlarkError> {
        self.check_mutable()?;
        self.entries.clear();
        Ok(())
    }

    pub fn pop(&mut self, key: &Value, default: Option<Value>) -> Result<Value, StarlarkError> {
        self.check_mutable()?;
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == key) {
            Ok(self.entries.remove(pos).1)
        } else if let Some(def) = default {
            Ok(def)
        } else {
            Err(StarlarkError(format!("key {} not found in dictionary", key.repr())))
        }
    }

    pub fn popitem(&mut self) -> Result<(Value, Value), StarlarkError> {
        self.check_mutable()?;
        self.entries
            .pop()
            .ok_or_else(|| StarlarkError("popitem(): dictionary is empty".to_string()))
    }

    pub fn setdefault(&mut self, key: Value, default: Value) -> Result<Value, StarlarkError> {
        self.check_mutable()?;
        if !key.is_hashable() {
            return Err(StarlarkError(format!("unhashable type: '{}'", key.type_name())));
        }
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == &key) {
            Ok(self.entries[pos].1.clone())
        } else {
            self.entries.push((key, default.clone()));
            Ok(default)
        }
    }

    pub fn update(&mut self, pairs: Vec<(Value, Value)>) -> Result<(), StarlarkError> {
        self.check_mutable()?;
        for (k, v) in pairs {
            self.insert(k, v)?;
        }
        Ok(())
    }

    pub fn keys(&self) -> Vec<Value> {
        self.entries.iter().map(|(k, _)| k.clone()).collect()
    }

    pub fn values(&self) -> Vec<Value> {
        self.entries.iter().map(|(_, v)| v.clone()).collect()
    }

    pub fn items(&self) -> Vec<(Value, Value)> {
        self.entries.clone()
    }

    pub fn union(&self, other: &Self) -> Self {
        let mut res = self.clone();
        res.frozen = false;
        res.iter_count = 0;
        for (k, v) in &other.entries {
            let _ = res.insert(k.clone(), v.clone());
        }
        res
    }
}
