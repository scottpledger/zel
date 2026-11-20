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

use crate::eval::frame::CallFrame;
use crate::semantics::StarlarkSemantics;
use crate::syntax::location::Location;
use crate::types::list::StarlarkError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Thread state during evaluation managing the call stack and frame isolation.
#[derive(Debug)]
pub struct StarlarkThread {
    pub frames: Vec<CallFrame>,
    pub semantics: StarlarkSemantics,
    pub steps: u64,
    pub step_limit: u64,
    pub interrupted: Arc<AtomicBool>,
}

impl StarlarkThread {
    pub fn new(semantics: StarlarkSemantics) -> Self {
        let step_limit = if semantics.max_computation_steps > 0 {
            semantics.max_computation_steps
        } else {
            i64::MAX as u64
        };
        Self {
            frames: Vec::new(),
            semantics,
            steps: 0,
            step_limit,
            interrupted: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn set_step_limit(&mut self, limit: u64) {
        self.step_limit = limit;
    }

    pub fn check_interrupted(&self) -> Result<(), StarlarkError> {
        if self.interrupted.swap(false, Ordering::SeqCst) {
            return Err(StarlarkError(
                "Starlark computation interrupted".to_string(),
            ));
        }
        Ok(())
    }

    pub fn check_step(&mut self, sem_limit: u64) -> Result<(), StarlarkError> {
        self.check_interrupted()?;
        let effective_limit = if sem_limit > 0 {
            sem_limit
        } else {
            self.step_limit
        };
        self.steps = self.steps.wrapping_add(1);
        if self.steps >= effective_limit {
            return Err(StarlarkError(
                "Starlark computation cancelled: too many steps".to_string(),
            ));
        }
        if (self.steps & 0xff) == 0 {
            crate::profiler::record_sample(self);
        }
        Ok(())
    }

    pub fn is_recursive_call(&self, function_name: &str) -> bool {
        self.frames.iter().any(|f| f.function_name == function_name)
    }

    pub fn push_frame(
        &mut self,
        function_name: impl Into<String>,
        location: Location,
    ) -> Result<(), StarlarkError> {
        let name = function_name.into();
        if !self.semantics.allow_recursion && self.is_recursive_call(&name) {
            return Err(StarlarkError(format!(
                "function '{name}' called recursively"
            )));
        }
        self.frames.push(CallFrame::new(name, location));
        Ok(())
    }

    /// Like [`StarlarkThread::push_frame`] but recursion is detected by function identity, so
    /// distinct functions that share a name (`<lambda>`) may call each other.
    pub fn push_frame_for(
        &mut self,
        function_name: impl Into<String>,
        function_id: usize,
        location: Location,
    ) -> Result<(), StarlarkError> {
        let name = function_name.into();
        if !self.semantics.allow_recursion
            && self.frames.iter().any(|f| f.function_id == function_id)
        {
            return Err(StarlarkError(format!(
                "function '{name}' called recursively"
            )));
        }
        let mut frame = CallFrame::new(name, location);
        frame.function_id = function_id;
        self.frames.push(frame);
        Ok(())
    }

    pub fn pop_frame(&mut self) -> Option<CallFrame> {
        self.frames.pop()
    }

    pub fn current_frame_mut(&mut self) -> Option<&mut CallFrame> {
        self.frames.last_mut()
    }

    pub fn current_frame(&self) -> Option<&CallFrame> {
        self.frames.last()
    }

    /// Bazel-format traceback ending at `at` (the position of the failing call, e.g. `fail(...)`):
    /// each frame is reported with the position of the call it is currently executing, followed
    /// by the (trimmed) source line, e.g.
    ///
    /// ```text
    /// Traceback (most recent call last):
    /// \tFile "/ws/defs.bzl", line 3, column 9, in _impl
    /// \t\tfail("boom")
    /// ```
    ///
    /// Frames entered from native code (empty call-site file) are not preceded by a
    /// `<toplevel>` frame.
    pub fn format_stack_trace_at(&self, at: &Location) -> String {
        let mut entries: Vec<(String, Location)> = Vec::new();
        let non_comp: Vec<&CallFrame> = self
            .frames
            .iter()
            .filter(|f| f.function_name != "<comprehension>")
            .collect();
        if let Some(first) = non_comp.first() {
            if !first.location.file.is_empty() {
                entries.push(("<toplevel>".to_string(), first.location.clone()));
            }
        }
        for (i, f) in non_comp.iter().enumerate() {
            let pos = non_comp
                .get(i + 1)
                .map(|n| n.location.clone())
                .unwrap_or_else(|| at.clone());
            entries.push((f.function_name.clone(), pos));
        }
        if entries.is_empty() && !(at.file.is_empty() && at.line == 0) {
            entries.push(("<toplevel>".to_string(), at.clone()));
        }
        let at_prefix = at.to_string();
        let mut out = if at_prefix.is_empty() {
            "Traceback (most recent call last):\n".to_string()
        } else {
            format!("{at_prefix}: Traceback (most recent call last):\n")
        };
        for (name, loc) in entries {
            out.push_str(&format!("\tFile \"{}\"", loc.file));
            if loc.line != 0 {
                out.push_str(&format!(", line {}", loc.line));
                if loc.column != 0 {
                    out.push_str(&format!(", column {}", loc.column));
                }
            }
            out.push_str(&format!(", in {name}\n"));
            if let Some(src) = source_line(&loc.file, loc.line) {
                out.push_str(&format!("\t\t{src}\n"));
            }
        }
        out
    }

    pub fn format_stack_trace(&self) -> String {
        if self.frames.is_empty() {
            return String::new();
        }
        let mut out = String::from("Traceback (most recent call last):\n");
        for frame in &self.frames {
            out.push_str(&format!("\tFile \"{}\"", frame.location.file));
            if frame.location.line != 0 {
                out.push_str(&format!(", line {}", frame.location.line));
                if frame.location.column != 0 {
                    out.push_str(&format!(", column {}", frame.location.column));
                }
            }
            out.push_str(&format!(", in {}\n", frame.function_name));
            if let Some(src) = source_line(&frame.location.file, frame.location.line) {
                out.push_str(&format!("\t\t{src}\n"));
            }
        }
        out
    }
}

thread_local! {
    static SOURCES: std::cell::RefCell<std::collections::HashMap<String, String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Registers in-memory source text for `file` (e.g. registry `MODULE.bazel` contents that do not
/// exist on disk) so tracebacks can show source lines. Per-thread.
pub fn register_source(file: &str, text: &str) {
    SOURCES.with(|s| {
        s.borrow_mut().insert(file.to_string(), text.to_string());
    });
}

/// Returns the trimmed text of 1-based line `line` of `file`, if known or readable from disk.
pub fn source_line(file: &str, line: usize) -> Option<String> {
    if file.is_empty() || line == 0 {
        return None;
    }
    if let Some(r) = SOURCES.with(|s| {
        s.borrow()
            .get(file)
            .map(|t| t.lines().nth(line - 1).map(|l| l.trim().to_string()))
    }) {
        return r;
    }
    let text = std::fs::read_to_string(file).ok()?;
    text.lines().nth(line - 1).map(|l| l.trim().to_string())
}
