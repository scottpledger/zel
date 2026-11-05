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

use crate::types::value::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StarlarkError(pub String);

impl std::fmt::Display for StarlarkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StarlarkError {}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StarlarkList {
    pub elements: Vec<Value>,
    pub frozen: bool,
    pub iter_count: usize,
}

impl StarlarkList {
    pub fn new(elements: Vec<Value>) -> Self {
        Self {
            elements,
            frozen: false,
            iter_count: 0,
        }
    }

    pub fn check_mutable(&self) -> Result<(), StarlarkError> {
        if self.frozen {
            return Err(StarlarkError("trying to mutate a frozen list value".to_string()));
        }
        if self.iter_count > 0 {
            return Err(StarlarkError(
                "list value is temporarily immutable due to active for-loop iteration".to_string(),
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

    pub fn append(&mut self, item: Value) -> Result<(), StarlarkError> {
        self.check_mutable()?;
        self.elements.push(item);
        Ok(())
    }

    pub fn extend(&mut self, items: Vec<Value>) -> Result<(), StarlarkError> {
        self.check_mutable()?;
        self.elements.extend(items);
        Ok(())
    }

    pub fn clear(&mut self) -> Result<(), StarlarkError> {
        self.check_mutable()?;
        self.elements.clear();
        Ok(())
    }

    pub fn insert(&mut self, index: i64, item: Value) -> Result<(), StarlarkError> {
        self.check_mutable()?;
        let len = self.elements.len() as i64;
        let mut idx = if index < 0 {
            len + index
        } else {
            index
        };
        if idx < 0 {
            idx = 0;
        } else if idx > len {
            idx = len;
        }
        self.elements.insert(idx as usize, item);
        Ok(())
    }

    pub fn pop(&mut self, index: Option<i64>) -> Result<Value, StarlarkError> {
        self.check_mutable()?;
        if self.elements.is_empty() {
            return Err(StarlarkError("pop from empty list".to_string()));
        }
        let len = self.elements.len() as i64;
        let idx = match index {
            Some(i) => {
                let actual = if i < 0 { len + i } else { i };
                if actual < 0 || actual >= len {
                    return Err(StarlarkError("pop index out of range".to_string()));
                }
                actual as usize
            }
            None => self.elements.len() - 1,
        };
        Ok(self.elements.remove(idx))
    }

    pub fn remove(&mut self, item: &Value) -> Result<(), StarlarkError> {
        self.check_mutable()?;
        let pos = self
            .elements
            .iter()
            .position(|x| x == item)
            .ok_or_else(|| StarlarkError(format!("item {item:?} not found in list")))?;
        self.elements.remove(pos);
        Ok(())
    }

    pub fn index(
        &self,
        item: &Value,
        start: Option<i64>,
        end: Option<i64>,
    ) -> Result<i64, StarlarkError> {
        let len = self.elements.len() as i64;
        let s = start.unwrap_or(0);
        let s = if s < 0 { (len + s).max(0) } else { s.min(len) } as usize;
        let e = end.unwrap_or(len);
        let e = if e < 0 { (len + e).max(0) } else { e.min(len) } as usize;

        if s < e && s < self.elements.len() {
            let slice_end = e.min(self.elements.len());
            for i in s..slice_end {
                if &self.elements[i] == item {
                    return Ok(i as i64);
                }
            }
        }
        Err(StarlarkError(format!("item {item:?} not found in list")))
    }
}
