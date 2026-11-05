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

//! `version` (`VersionCommand` + `BlazeVersionInfo`) and `shutdown` (`ShutdownCommand`).

use crate::{build_label, version_product_prefix, writeln_bytes, BlazeRuntime};
use std::sync::atomic::Ordering;
use zel_options::ParsedCommandOptions;
use zel_util::ExitCode;

/// The embedded build data (`build-data.properties`), keyed like `BlazeVersionInfo`.
pub fn build_data() -> Vec<(String, String)> {
    let ts: i64 = option_env!("ZEL_BUILD_TIMESTAMP")
        .and_then(|v| v.parse().ok())
        .or_else(|| {
            // Bazel builds (no build script): the binary's own modification time.
            std::env::current_exe()
                .and_then(std::fs::metadata)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
        })
        .unwrap_or(0);
    vec![
        ("Build label".to_string(), build_label()),
        ("Build target".to_string(), "@@//crates/zel-client:zel".to_string()),
        ("Build time".to_string(), format!("{} ({ts})", ctime(ts))),
        ("Build timestamp".to_string(), ts.to_string()),
        ("Build timestamp as int".to_string(), ts.to_string()),
    ]
}

/// `Thu Oct 19 17:47:24 2023` (UTC).
pub fn ctime(ts: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] =
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let days = ts.div_euclid(86400);
    let secs = ts.rem_euclid(86400);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{} {} {:02} {:02}:{:02}:{:02} {}",
        DAYS[days.rem_euclid(7) as usize],
        MONTHS[(m - 1) as usize],
        d,
        secs / 3600,
        secs % 3600 / 60,
        secs % 60,
        y
    )
}

impl BlazeRuntime {
    /// `VersionCommand.exec`.
    pub(crate) fn cmd_version(&self, args: &[String], stdout: &mut Vec<u8>, stderr: &mut Vec<u8>) -> ExitCode {
        // The client's `--version` fast path forwards `--startup_version`.
        let gnu = args.iter().any(|a| a == "--startup_version")
            || match ParsedCommandOptions::parse_for_command(
                "version",
                &args.iter().filter(|a| *a != "--startup_version").cloned().collect::<Vec<_>>(),
            ) {
                Ok(p) => p.get_bool("gnu_format").unwrap_or(false),
                Err(e) => {
                    writeln_bytes(stderr, &e.format_bazel_error());
                    return ExitCode::COMMAND_LINE_ERROR;
                }
            };
        // `@Command(allowResidue = false)`: `OptionsParser.parse` rejects any residue with
        // "Unrecognized arguments: <residue>" (all of it, space separated).
        let residue: Vec<&String> = args.iter().filter(|a| !a.starts_with('-') && *a != "--startup_version").collect();
        if !residue.is_empty() {
            let joined: Vec<&str> = residue.iter().map(|s| s.as_str()).collect();
            writeln_bytes(stderr, &format!("ERROR: Unrecognized arguments: {}", joined.join(" ")));
            return ExitCode::COMMAND_LINE_ERROR;
        }
        if gnu {
            let label = build_label();
            let version = if label.is_empty() { "no_version".to_string() } else { label };
            writeln_bytes(stdout, &format!("{} {version}", version_product_prefix()));
        } else {
            let summary: Vec<String> = build_data().into_iter().map(|(k, v)| format!("{k}: {v}")).collect();
            writeln_bytes(stdout, &summary.join("\n"));
        }
        ExitCode::SUCCESS
    }

    /// `ShutdownCommand.exec`: `--iff_heap_size_greater_than` compares against this process's
    /// resident memory (the native analog of the JVM's `totalMemory()`).
    pub(crate) fn cmd_shutdown(
        &self,
        args: &[String],
        stderr: &mut Vec<u8>,
        termination_expected: &mut bool,
    ) -> ExitCode {
        let parsed = match ParsedCommandOptions::parse_for_command("shutdown", args) {
            Ok(p) => p,
            Err(e) => {
                writeln_bytes(stderr, &e.format_bazel_error());
                return ExitCode::COMMAND_LINE_ERROR;
            }
        };
        if !parsed.targets.is_empty() {
            writeln_bytes(stderr, &format!("ERROR: Unrecognized arguments: {}", parsed.targets.join(" ")));
            return ExitCode::COMMAND_LINE_ERROR;
        }
        let limit = parsed.get_int("iff_heap_size_greater_than").unwrap_or(0) as i64;
        if limit == 0 || crate::sys::resident_bytes() as i64 > limit * 1000 * 1000 {
            self.shutdown_requested.store(true, Ordering::SeqCst);
            *termination_expected = true;
        }
        ExitCode::SUCCESS
    }
}
