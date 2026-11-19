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

use std::io;
use std::sync::Arc;
use zel_client::{execute_cli_with_exec, perform_exec_request};
use zel_vfs::{LocalFileSystem, PathFragment};

fn canonical_cwd() -> String {
    let Ok(cur) = std::env::current_dir() else {
        return "/".to_string();
    };
    let resolved = std::fs::canonicalize(&cur).unwrap_or(cur);
    let s = resolved.to_string_lossy();
    #[cfg(windows)]
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    s.to_string()
}

fn main() {
    zel_server::sys::setup_std_streams();
    zel_server::sys::unlimit_resources();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = PathFragment::create(&canonical_cwd());
    let fs = Arc::new(LocalFileSystem::default());

    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    let mut pending = None;
    let mut exit_code =
        execute_cli_with_exec(&args, &cwd, fs, &mut stdout, &mut stderr, &mut pending);
    if exit_code == 0 {
        if let Some(p) = pending {
            use std::io::Write;
            let _ = stdout.flush();
            let _ = stderr.flush();
            exit_code = perform_exec_request(&p);
        }
    }
    std::process::exit(exit_code);
}
