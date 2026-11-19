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

//! Parameter-file argument escapers matching `GccParamFileEscaper.java` and
//! `WindowsParamFileEscaper.java`.

/// Escapes a single argument for a gcc/clang `@file` parameter file
/// (`com.google.devtools.build.lib.util.GccParamFileEscaper.escapeString`).
///
/// An empty argument becomes `''`; otherwise `'`, `"`, `\`, space, tab, CR, LF,
/// form feed (`\x0C`), and vertical tab (`\x0B`) are backslash-escaped.
pub fn gcc_escape_param(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    let needs_escape = |c: char| {
        matches!(
            c,
            '\'' | '"' | '\\' | ' ' | '\t' | '\r' | '\n' | '\x0c' | '\x0b'
        )
    };
    if !s.chars().any(needs_escape) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        if needs_escape(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Escapes all arguments in input order using [`gcc_escape_param`].
pub fn gcc_escape_all<I, S>(args: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .map(|s| gcc_escape_param(s.as_ref()))
        .collect()
}

/// Escapes a single argument for a Windows (`lld-link` / MSVC) `@file` parameter file
/// (`com.google.devtools.build.lib.util.WindowsParamFileEscaper.escapeString`).
///
/// A non-empty argument without `\` or `"` is returned unchanged unless it contains
/// space, tab, CR, or LF (`\f` and `\x0B` do not trigger quoting), in which case it is
/// wrapped in `"..."`. Otherwise each `"` is preceded by `2 * n + 1` backslashes (where
/// `n` is the length of the immediately preceding backslash run), other backslash runs
/// are copied unchanged, and a trailing backslash run is doubled only when surrounding
/// quotes are added (empty argument or whitespace).
pub fn windows_escape_param(s: &str) -> String {
    if s.is_empty() {
        return "\"\"".to_string();
    }
    let has_ws = s.chars().any(|c| matches!(c, ' ' | '\t' | '\r' | '\n'));
    if !s.contains('\\') && !s.contains('"') {
        return if has_ws {
            format!("\"{s}\"")
        } else {
            s.to_string()
        };
    }

    let mut out = String::with_capacity(s.len() + 4);
    if has_ws {
        out.push('"');
    }
    let mut backslashes = 0usize;
    for c in s.chars() {
        match c {
            '\\' => {
                backslashes += 1;
            }
            '"' => {
                for _ in 0..(2 * backslashes + 1) {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            }
            _ => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                out.push(c);
                backslashes = 0;
            }
        }
    }
    let trailing = if has_ws { 2 * backslashes } else { backslashes };
    for _ in 0..trailing {
        out.push('\\');
    }
    if has_ws {
        out.push('"');
    }
    out
}

/// Escapes all arguments in input order using [`windows_escape_param`], preserving empty strings.
pub fn windows_escape_all<I, S>(args: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .map(|s| windows_escape_param(s.as_ref()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gcc_escaper_matches_bazel() {
        assert_eq!(gcc_escape_param(""), "''");
        assert_eq!(gcc_escape_param("plain-arg_1.o"), "plain-arg_1.o");
        assert_eq!(gcc_escape_param("a b\tc\nd\re"), "a\\ b\\\tc\\\nd\\\re");
        assert_eq!(gcc_escape_param("a\x0cb\x0bc"), "a\\\x0cb\\\x0bc");
        assert_eq!(gcc_escape_param("'\"\\"), "\\'\\\"\\\\");
        assert_eq!(
            gcc_escape_all(["", "foo", "a b"]),
            vec!["''", "foo", "a\\ b"]
        );
    }

    #[test]
    fn windows_escaper_matches_bazel() {
        assert_eq!(windows_escape_param(""), "\"\"");
        assert_eq!(windows_escape_param("foo"), "foo");
        assert_eq!(windows_escape_param("foo bar"), "\"foo bar\"");
        // Form feed and vertical tab do not trigger Windows param quoting.
        assert_eq!(
            windows_escape_param("foo\x0cbar\x0bbaz"),
            "foo\x0cbar\x0bbaz"
        );
        // Backslashes without quotes or whitespace stay unchanged (including trailing).
        assert_eq!(windows_escape_param("C:\\foo\\bar\\"), "C:\\foo\\bar\\");
        // Trailing backslashes double when wrapped in quotes due to whitespace.
        assert_eq!(
            windows_escape_param("C:\\Program Files\\foo\\"),
            "\"C:\\Program Files\\foo\\\\\""
        );
        // Backslashes before quotes become 2*n + 1 backslashes whether quoted or unquoted.
        assert_eq!(windows_escape_param("a\"b"), "a\\\"b");
        assert_eq!(windows_escape_param("a\\\"b"), "a\\\\\\\"b");
        assert_eq!(windows_escape_param("a \\\"b"), "\"a \\\\\\\"b\"");
        assert_eq!(
            windows_escape_all(["", "x", "y z", "a\\"]),
            vec!["\"\"", "x", "\"y z\"", "a\\"]
        );
    }
}
