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

//! Derives a [`FailureDetail`] for command results that did not attach one explicitly, so every
//! failing command reports a category/code whose metadata exit code equals the process exit code
//! (the invariant `DetailedExitCode` relies on).

use zel_proto::failure_details::{all_categories, Category, FailureDetail, InterruptedCode};
use zel_util::ExitCode;

/// First code of `category` whose `FailureDetailMetadata.exit_code` equals `exit`.
pub(crate) fn category_for_exit(preferred: &[&str], exit: i32) -> Option<Category> {
    let reg = zel_proto::schema::registry();
    let fd = reg.message("failure_details.FailureDetail")?;
    let try_cat = |name: &str| -> Option<Category> {
        let f = fd.field_by_name(name)?;
        let zel_proto::schema::FieldType::Message(sub) = &f.ty else { return None };
        let cf = reg.message(sub)?.field_by_number(1)?;
        let zel_proto::schema::FieldType::Enum(en) = &cf.ty else { return None };
        let e = reg.enum_desc(en)?;
        let (code_name, _) = e.values.iter().find(|(_, n)| e.exit_code_of(*n) == Some(exit))?;
        Category::named(name, code_name)
    };
    preferred
        .iter()
        .find_map(|c| try_cat(c))
        .or_else(|| all_categories().iter().find_map(|(_, n)| try_cat(n)))
}

fn first_error_line(stderr: &str) -> String {
    stderr
        .lines()
        .find_map(|l| l.strip_prefix("ERROR: "))
        .unwrap_or("command failed")
        .to_string()
}

/// Default failure detail for `code`, classifying the command's stderr for exit code 1.
pub(crate) fn default_for(code: ExitCode, command: &str, stderr: &[u8]) -> Option<FailureDetail> {
    if code == ExitCode::SUCCESS {
        return None;
    }
    let text = String::from_utf8_lossy(stderr);
    let msg = first_error_line(&text);
    let named = |c: &str, n: &str| Category::named(c, n);
    let cat = match code.numeric_exit_code() {
        1 => {
            if text.contains("(Exit ") || text.contains("failed: ") {
                named("spawn", "NON_ZERO_EXIT")
            } else if text.contains("no such package") {
                named("package_loading", "PACKAGE_MISSING")
            } else if text.contains("no such target") {
                named("package_loading", "TARGET_MISSING")
            } else if text.contains("Traceback (most recent call last)") && !text.contains(" rule //") {
                named("package_loading", "STARLARK_EVAL_ERROR")
            } else if text.contains(" rule //") || text.contains("errors encountered while analyzing") {
                named("analysis", "CONFIGURED_VALUE_CREATION_FAILED")
            } else if text.to_ascii_lowercase().contains("cycle") {
                named("analysis", "CYCLE")
            } else if text.contains("Target pattern parsing failed") {
                named("target_patterns", "TARGET_PATTERN_PARSE_FAILURE")
            } else {
                named("execution", "NON_ACTION_EXECUTION_FAILURE")
            }
        }
        2 => named("command", "OPTIONS_PARSE_FAILURE"),
        3 => named("test_command", "TESTS_FAILED"),
        4 => named("test_command", "NO_TEST_TARGETS"),
        8 => Some(Category::Interrupted(InterruptedCode::Interrupted)),
        9 => named("command", "ANOTHER_COMMAND_RUNNING"),
        33 => named("crash", "CRASH_OOM"),
        34 => category_for_exit(&["remote_execution", "remote_options"], 34),
        36 => category_for_exit(&["execution", "filesystem", "package_loading"], 36),
        37 => named("crash", "CRASH_UNKNOWN"),
        38 => named("build_progress", "BES_UPLOAD_TIMEOUT_ERROR"),
        45 => named("build_progress", "BES_STREAM_NOT_RETRYING_FAILURE"),
        48 => category_for_exit(&["external_deps"], 48),
        n => category_for_exit(&[], n),
    }?;
    let _ = command;
    Some(FailureDetail::new(msg, cat))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zel_util::DetailedExitCode;

    #[test]
    fn detail_exit_code_matches_process_exit_code() {
        let cases: &[(ExitCode, &str)] = &[
            (ExitCode::BUILD_FAILURE, "ERROR: x: Executing genrule //p:bad failed: (Exit 3)"),
            (ExitCode::BUILD_FAILURE, "ERROR: no such package 'p': BUILD file not found"),
            (ExitCode::BUILD_FAILURE, "ERROR: Skipping '//:x': no such target"),
            (ExitCode::BUILD_FAILURE, "ERROR: /w/BUILD:1:1: in my_rule rule //:t: boom"),
            (ExitCode::BUILD_FAILURE, "something else entirely"),
            (ExitCode::COMMAND_LINE_ERROR, "ERROR: bad flag"),
            (ExitCode::TESTS_FAILED, ""),
            (ExitCode::NO_TESTS_FOUND, ""),
            (ExitCode::INTERRUPTED, ""),
            (ExitCode::LOCK_HELD_NOBLOCK_FOR_LOCK, ""),
            (ExitCode::OOM_ERROR, ""),
            (ExitCode::REMOTE_ERROR, "ERROR: remote"),
            (ExitCode::LOCAL_ENVIRONMENTAL_ERROR, ""),
            (ExitCode::BLAZE_INTERNAL_ERROR, ""),
            (ExitCode::TRANSIENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR, ""),
            (ExitCode::PERSISTENT_BUILD_EVENT_SERVICE_UPLOAD_ERROR, ""),
            (ExitCode::EXTERNAL_DEPS_ERROR, ""),
        ];
        for (code, err) in cases {
            let fd = default_for(*code, "build", err.as_bytes()).unwrap_or_else(|| panic!("no detail for {code}"));
            assert_eq!(fd.exit_code(), Some(code.numeric_exit_code()), "{code}: {fd:?}");
            let d = DetailedExitCode::of_detail(fd).unwrap();
            assert_eq!(d.exit_code.numeric_exit_code(), code.numeric_exit_code());
        }
        assert!(default_for(ExitCode::SUCCESS, "build", b"").is_none());
        let fd = default_for(ExitCode::BUILD_FAILURE, "build", b"INFO: x\nERROR: the message\n").unwrap();
        assert_eq!(fd.message, "the message");
    }
}
