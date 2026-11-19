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
use std::hash::{Hash, Hasher};

/// Lazy `range` sequence supporting O(1) indexing, slicing, and length.
#[derive(Debug, Clone, Copy, Eq)]
pub struct RangeList {
    pub start: i64,
    pub stop: i64,
    pub step: i64,
    pub len: usize,
}

impl PartialEq for RangeList {
    fn eq(&self, other: &Self) -> bool {
        if self.len != other.len {
            return false;
        }
        if self.len == 0 {
            return true;
        }
        if self.len == 1 {
            return self.start == other.start;
        }
        self.start == other.start && self.step == other.step
    }
}

impl Hash for RangeList {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.len.hash(state);
        if self.len >= 1 {
            self.start.hash(state);
        }
        if self.len >= 2 {
            self.step.hash(state);
        }
    }
}

impl RangeList {
    pub fn new(start: i64, stop: i64, step: i64) -> Result<Self, StarlarkError> {
        if step == 0 {
            return Err(StarlarkError("step cannot be 0".to_string()));
        }
        let len_i128: i128 = if step > 0 && start < stop {
            ((stop as i128) - (start as i128) + (step as i128) - 1) / (step as i128)
        } else if step < 0 && start > stop {
            let abs_step = -(step as i128);
            ((start as i128) - (stop as i128) + abs_step - 1) / abs_step
        } else {
            0
        };
        if len_i128 > (i32::MAX as i128) {
            let repr = if step == 1 {
                format!("range({start}, {stop})")
            } else {
                format!("range({start}, {stop}, {step})")
            };
            return Err(StarlarkError(format!(
                "len({repr}) exceeds signed 32-bit range"
            )));
        }
        let len = len_i128 as usize;
        Ok(Self {
            start,
            stop,
            step,
            len,
        })
    }

    /// Slices this range in O(1) returning another [`RangeList`], matching Bazel's `RangeList.getSlice`.
    pub fn slice(
        &self,
        start_opt: Option<i32>,
        end_opt: Option<i32>,
        slice_step: i32,
    ) -> Result<Self, StarlarkError> {
        if slice_step == 0 {
            return Err(StarlarkError("slice step cannot be zero".to_string()));
        }
        let len = self.len as i64;
        let to_slice_index = |idx: i32, clamp_neg: i64, clamp_hi: i64| -> i64 {
            let v = idx as i64;
            if v < 0 {
                (len + v).max(clamp_neg)
            } else {
                v.min(clamp_hi)
            }
        };
        let (start_idx, stop_idx) = if slice_step > 0 {
            let s = start_opt.map(|i| to_slice_index(i, 0, len)).unwrap_or(0);
            let e = end_opt.map(|i| to_slice_index(i, 0, len)).unwrap_or(len);
            (s, e)
        } else {
            let s = start_opt
                .map(|i| to_slice_index(i, -1, len - 1))
                .unwrap_or(len - 1);
            let e = end_opt
                .map(|i| to_slice_index(i, -1, len - 1))
                .unwrap_or(-1);
            (s, e)
        };

        let sub_len = if slice_step > 0 && start_idx < stop_idx {
            ((stop_idx - start_idx + (slice_step as i64) - 1) / (slice_step as i64)) as usize
        } else if slice_step < 0 && start_idx > stop_idx {
            let abs_s = -(slice_step as i64);
            ((start_idx - stop_idx + abs_s - 1) / abs_s) as usize
        } else {
            0
        };

        let new_start = self.start + start_idx * self.step;
        let raw_step = (self.step as i128) * (slice_step as i128);
        if let Ok(step_i32) = i32::try_from(raw_step) {
            let new_step = step_i32 as i64;
            let new_stop = self.start + stop_idx * self.step;
            Ok(Self {
                start: new_start,
                stop: new_stop,
                step: new_step,
                len: sub_len,
            })
        } else {
            // Step product overflows signed 32-bit int: Bazel clamps step to i32::MAX / i32::MIN
            // and adjusts stop for a 0- or 1-element range.
            let clamped_step = if raw_step > 0 {
                i32::MAX as i64
            } else {
                i32::MIN as i64
            };
            let new_stop = if sub_len == 0 {
                new_start
            } else if clamped_step > 0 {
                new_start + 1
            } else {
                new_start - 1
            };
            Ok(Self {
                start: new_start,
                stop: new_stop,
                step: clamped_step,
                len: sub_len.min(1),
            })
        }
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
