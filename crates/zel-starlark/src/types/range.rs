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
use std::fmt;

/// Lazy `range` sequence supporting O(1) indexing, slicing, and length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RangeList {
    pub start: i64,
    pub stop: i64,
    pub step: i64,
    pub len: usize,
}

impl RangeList {
    pub fn new(start: i64, stop: i64, step: i64) -> Result<Self, StarlarkError> {
        if step == 0 {
            return Err(StarlarkError("step cannot be 0".to_string()));
        }
        let len = if step > 0 && start < stop {
            ((stop - start + step - 1) / step) as usize
        } else if step < 0 && start > stop {
            ((start - stop + (-step) - 1) / (-step)) as usize
        } else {
            0
        };
        Ok(Self {
            start,
            stop,
            step,
            len,
        })
    }

    pub fn get(&self, index: i64) -> Option<i64> {
        let actual = if index < 0 {
            (self.len as i64) + index
        } else {
            index
        };
        if actual >= 0 && (actual as usize) < self.len {
            Some(self.start + actual * self.step)
        } else {
            None
        }
    }

    pub fn to_vec(&self) -> Vec<i64> {
        let mut v = Vec::with_capacity(self.len);
        let mut cur = self.start;
        for _ in 0..self.len {
            v.push(cur);
            cur += self.step;
        }
        v
    }
}

impl fmt::Display for RangeList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.step == 1 {
            write!(f, "range({}, {})", self.start, self.stop)
        } else {
            write!(f, "range({}, {}, {})", self.start, self.stop, self.step)
        }
    }
}
