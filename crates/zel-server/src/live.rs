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

//! Live output streaming: the daemon installs a [`LiveSink`] on the command thread; the
//! per-command UI (`BuildObserver`) registers its output buffer there, and the daemon drains it
//! while the command runs, sending `RunResponse.standard_output/standard_error` chunks.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use zel_events::ui::BufferOutErr;

/// Shared between the daemon's streaming loop and the command thread.
#[derive(Default)]
pub struct LiveSink {
    buffers: Mutex<Vec<Arc<BufferOutErr>>>,
    history: Mutex<(Vec<u8>, Vec<u8>)>,
}

impl LiveSink {
    /// Drains everything written so far: `(stdout, stderr)`.
    pub fn drain(&self) -> (Vec<u8>, Vec<u8>) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        for b in self
            .buffers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
        {
            out.extend(b.take_out());
            err.extend(b.take_err());
        }
        if !out.is_empty() || !err.is_empty() {
            let mut h = self.history.lock().unwrap_or_else(|e| e.into_inner());
            h.0.extend_from_slice(&out);
            h.1.extend_from_slice(&err);
        }
        (out, err)
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<LiveSink>>> = const { RefCell::new(None) };
}

/// Installs (or clears) the sink for the calling thread.
pub fn set_current(sink: Option<Arc<LiveSink>>) {
    CURRENT.with(|c| *c.borrow_mut() = sink);
}

/// Takes any output already drained for live streaming on the current command thread.
pub fn take_history() -> (Vec<u8>, Vec<u8>) {
    CURRENT.with(|c| match c.borrow().as_ref() {
        Some(s) => std::mem::take(&mut *s.history.lock().unwrap_or_else(|e| e.into_inner())),
        None => (Vec::new(), Vec::new()),
    })
}

/// Registers a UI output buffer with the current thread's sink (no-op when not streaming).
pub fn register(buf: Arc<BufferOutErr>) {
    CURRENT.with(|c| {
        if let Some(s) = c.borrow().as_ref() {
            s.buffers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(buf);
        }
    });
}
