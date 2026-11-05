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

//! Terminal queries (`isatty`, window size) via libc symbols already linked by `std`
//! (no external crates). Mirrors what the Bazel client passes as `--isatty` and
//! `--terminal_columns`.

#[cfg(unix)]
mod sys {
    use std::os::raw::{c_int, c_ulong, c_ushort, c_void};

    #[repr(C)]
    #[derive(Default)]
    pub struct WinSize {
        pub rows: c_ushort,
        pub cols: c_ushort,
        pub xpixel: c_ushort,
        pub ypixel: c_ushort,
    }

    extern "C" {
        pub fn isatty(fd: c_int) -> c_int;
        pub fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    }

    #[cfg(target_os = "macos")]
    pub const TIOCGWINSZ: c_ulong = 0x4008_7468;
    #[cfg(not(target_os = "macos"))]
    pub const TIOCGWINSZ: c_ulong = 0x5413;

    pub fn size(fd: c_int) -> Option<(u16, u16)> {
        let mut ws = WinSize::default();
        // SAFETY: `ws` is a valid, correctly laid-out `winsize` for TIOCGWINSZ.
        let rc = unsafe { ioctl(fd, TIOCGWINSZ, &mut ws as *mut WinSize as *mut c_void) };
        if rc == 0 && ws.cols > 0 {
            Some((ws.cols, ws.rows))
        } else {
            None
        }
    }
}

/// Whether the given file descriptor (0 = stdin, 1 = stdout, 2 = stderr) is a terminal.
pub fn isatty(fd: i32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: isatty only inspects the descriptor.
        unsafe { sys::isatty(fd) == 1 }
    }
    #[cfg(not(unix))]
    {
        let _ = fd;
        false
    }
}

/// Terminal `(columns, rows)` for `fd`, if it is a terminal that reports a size.
pub fn terminal_size(fd: i32) -> Option<(u16, u16)> {
    #[cfg(unix)]
    {
        sys::size(fd)
    }
    #[cfg(not(unix))]
    {
        let _ = fd;
        None
    }
}

/// Best-effort terminal width: ioctl on stderr/stdout, then `$COLUMNS`, else `None`.
pub fn terminal_columns() -> Option<usize> {
    for fd in [2, 1] {
        if let Some((c, _)) = terminal_size(fd) {
            return Some(c as usize);
        }
    }
    std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()).filter(|c| *c > 0)
}

/// Whether `$TERM` indicates a terminal without cursor control (`dumb`/unset).
pub fn term_is_dumb() -> bool {
    match std::env::var("TERM") {
        Ok(t) => t.is_empty() || t == "dumb",
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_do_not_crash() {
        let _ = isatty(0);
        let _ = terminal_size(2);
        let _ = terminal_columns();
        assert!(!isatty(-1));
    }
}
