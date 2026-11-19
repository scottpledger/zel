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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

/// Shared mutability token for an evaluation thread (`Mutability.java`).
///
/// Mutable Starlark containers (`StarlarkList`, `StarlarkDict`, `StarlarkSet`) hold a reference
/// to their creating evaluation's `Mutability`. Calling `freeze()` freezes every value bound
/// to this `Mutability` in O(1) time.
#[derive(Debug)]
pub struct Mutability {
    frozen: AtomicBool,
    allows_unsafe_shallow_freeze: bool,
}

impl Mutability {
    pub fn create() -> Arc<Self> {
        Arc::new(Self {
            frozen: AtomicBool::new(false),
            allows_unsafe_shallow_freeze: false,
        })
    }

    pub fn create_allowing_shallow_freeze() -> Arc<Self> {
        Arc::new(Self {
            frozen: AtomicBool::new(false),
            allows_unsafe_shallow_freeze: true,
        })
    }

    pub fn immutable() -> Arc<Self> {
        static IMMUTABLE: OnceLock<Arc<Mutability>> = OnceLock::new();
        Arc::clone(IMMUTABLE.get_or_init(|| {
            Arc::new(Self {
                frozen: AtomicBool::new(true),
                allows_unsafe_shallow_freeze: false,
            })
        }))
    }

    pub fn is_frozen(&self) -> bool {
        self.frozen.load(Ordering::Acquire)
    }

    pub fn freeze(&self) {
        self.frozen.store(true, Ordering::Release);
    }

    pub fn allows_unsafe_shallow_freeze(&self) -> bool {
        self.allows_unsafe_shallow_freeze
    }
}
