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

//! Remote Execution API v2 (REAPI) client, Disk Cache (`--disk_cache`), and Chrome Trace Profiler (`--profile`).
//! Mirrors `src/main/java/com/google/devtools/build/lib/remote` and `profiler`.

pub mod credentials;
pub mod digest;
pub mod disk_cache;
pub mod fake_server;
pub mod grpc;
pub mod hpack;
pub mod http2;
pub mod merkle;
pub mod options;
pub mod reapi_client;
pub mod spawn_runner;
#[cfg(test)]
mod spawn_runner_tests;
pub mod transport;

use std::io;
use std::sync::Mutex;
use zel_vfs::{FileSystem, PathFragment};

/// `--disk_cache` client (Bazel layout `ac/` + `cas/`); see [`disk_cache::DiskCache`].
pub type DiskCacheClient = disk_cache::DiskCache;

#[derive(Debug, Clone)]
pub struct TraceEvent {
    pub name: String,
    pub category: String,
    pub start_us: u64,
    pub duration_us: u64,
    pub tid: u32,
}

/// Chrome Trace Format JSON profiler (`--profile`), compatible with `bazel analyze-profile`.
#[derive(Debug, Default)]
pub struct Profiler {
    events: Mutex<Vec<TraceEvent>>,
}

impl Profiler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(
        &self,
        name: impl Into<String>,
        category: impl Into<String>,
        start_us: u64,
        duration_us: u64,
    ) {
        self.events.lock().unwrap().push(TraceEvent {
            name: name.into(),
            category: category.into(),
            start_us,
            duration_us,
            tid: 1,
        });
    }

    pub fn write_profile(&self, fs: &dyn FileSystem, path: &PathFragment) -> io::Result<()> {
        let events = self.events.lock().unwrap();
        let mut json = String::from("{\"traceEvents\":[\n");
        for (i, ev) in events.iter().enumerate() {
            if i > 0 {
                json.push_str(",\n");
            }
            json.push_str(&format!(
                "{{\"name\":\"{}\",\"cat\":\"{}\",\"ph\":\"X\",\"ts\":{},\"dur\":{},\"pid\":1,\"tid\":{}}}",
                ev.name, ev.category, ev.start_us, ev.duration_us, ev.tid
            ));
        }
        json.push_str("\n]}\n");
        fs.write_file(path, json.as_bytes())
    }
}

