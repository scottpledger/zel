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
use zel_events::ui::{BufferOutErr, UiEventHandler};

/// Shared between the daemon's streaming loop and the command thread.
#[derive(Default)]
pub struct LiveSink {
    buffers: Mutex<Vec<Arc<BufferOutErr>>>,
    uis: Mutex<Vec<Arc<UiEventHandler>>>,
    terminal_size: Mutex<Option<(usize, usize)>>,
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

    /// Applies an `UpdateTerminalSize` RPC to this command's UI handlers (and stores it so a UI
    /// handler registered shortly after the RPC starts still receives the updated dimensions).
    pub fn notify_terminal_size_changed(&self, columns: usize, rows: usize) {
        if columns == 0 {
            return;
        }
        *self.terminal_size.lock().unwrap_or_else(|e| e.into_inner()) = Some((columns, rows));
        let uis = self.uis.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for ui in uis {
            ui.terminal_size_changed(columns);
        }
    }

    /// Last `(columns, rows)` delivered via `UpdateTerminalSize`, if any.
    pub fn terminal_size(&self) -> Option<(usize, usize)> {
        *self.terminal_size.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Current terminal width reported by the first registered `UiEventHandler`, if any.
    pub fn ui_terminal_width(&self) -> Option<usize> {
        self.uis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .first()
            .map(|u| u.terminal_width())
    }

    /// Registers a `UiEventHandler` on this sink, immediately replaying any stored terminal size.
    pub fn add_ui(&self, ui: Arc<UiEventHandler>) {
        let stored = *self.terminal_size.lock().unwrap_or_else(|e| e.into_inner());
        self.uis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Arc::clone(&ui));
        if let Some((cols, _)) = stored {
            if cols > 0 {
                ui.terminal_size_changed(cols);
            }
        }
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

/// Registers a `UiEventHandler` with the current thread's sink so `UpdateTerminalSize` RPCs
/// update its terminal width dynamically (and replays any stored size).
pub fn register_ui(ui: Arc<UiEventHandler>) {
    CURRENT.with(|c| {
        if let Some(s) = c.borrow().as_ref() {
            s.add_ui(ui);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_events::ui::UiOptions;

    #[test]
    fn terminal_size_updates_active_and_late_registered_ui_handlers() {
        let sink = Arc::new(LiveSink::default());
        set_current(Some(Arc::clone(&sink)));

        let ui1 = Arc::new(UiEventHandler::new(
            Arc::new(BufferOutErr::default()),
            UiOptions::default(),
        ));
        assert_eq!(ui1.terminal_width(), 80);
        register_ui(Arc::clone(&ui1));

        sink.notify_terminal_size_changed(132, 43);
        assert_eq!(sink.terminal_size(), Some((132, 43)));
        assert_eq!(ui1.terminal_width(), 132);
        assert_eq!(sink.ui_terminal_width(), Some(132));

        // A UI handler registered after `UpdateTerminalSize` receives the stored width immediately.
        let ui2 = Arc::new(UiEventHandler::new(
            Arc::new(BufferOutErr::default()),
            UiOptions::default(),
        ));
        assert_eq!(ui2.terminal_width(), 80);
        register_ui(Arc::clone(&ui2));
        assert_eq!(ui2.terminal_width(), 132);

        set_current(None);
    }
}
