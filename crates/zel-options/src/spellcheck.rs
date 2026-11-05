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

//! Spell checking and suggestion engine matching Bazel's `SpellChecker.java`.

/// Computes the Levenshtein edit distance between two strings with a cutoff.
/// If distance exceeds `max_edit_distance`, returns -1.
pub fn edit_distance(s1: &str, s2: &str, max_edit_distance: usize) -> i32 {
    if s1 == s2 {
        return 0;
    }
    let len1 = s1.chars().count();
    let len2 = s2.chars().count();
    if (len1 as isize - len2 as isize).unsigned_abs() > max_edit_distance {
        return -1;
    }

    let s1_chars: Vec<char> = s1.chars().collect();
    let s2_chars: Vec<char> = s2.chars().collect();

    let mut row: Vec<usize> = (0..=len2).collect();

    for i in 1..=len1 {
        row[0] = i;
        let mut best_in_row = row[0];
        let mut previous = i - 1;

        for j in 1..=len2 {
            let old = row[j];
            let cost = if s1_chars[i - 1] == s2_chars[j - 1] { 0 } else { 1 };
            row[j] = std::cmp::min(previous + cost, 1 + std::cmp::min(row[j - 1], row[j]));
            previous = old;
            best_in_row = std::cmp::min(best_in_row, row[j]);
        }
        if best_in_row > max_edit_distance {
            return -1;
        }
    }

    let result = row[len2];
    if result <= max_edit_distance {
        result as i32
    } else {
        -1
    }
}

/// Finds the most similar string in `words` to `input`, ignoring case.
pub fn suggest<'a>(input: &str, words: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut best: Option<String> = None;
    let input_len = input.chars().count();
    let mut best_distance = std::cmp::min(5, (input_len + 1) / 2);
    let input_lower = input.to_lowercase();

    for candidate in words {
        let candidate_lower = candidate.to_lowercase();
        let d = edit_distance(&input_lower, &candidate_lower, best_distance);
        if d >= 0 && (d as usize) < best_distance {
            best_distance = d as usize;
            best = Some(candidate.to_string());
        }
    }

    best
}

/// Returns `" (did you mean 'x'?)"` or `""`.
pub fn did_you_mean<'a>(input: &str, words: impl IntoIterator<Item = &'a str>) -> String {
    match suggest(input, words) {
        Some(s) => format!(" (did you mean '{s}'?)"),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spellcheck() {
        let candidates = vec!["--compilation_mode", "--subcommands", "--keep_going"];
        assert_eq!(
            did_you_mean("--subcommand", candidates.iter().copied()),
            " (did you mean '--subcommands'?)"
        );
        assert_eq!(
            did_you_mean("--keep_goin", candidates.iter().copied()),
            " (did you mean '--keep_going'?)"
        );
        assert_eq!(
            did_you_mean("--completely_unrelated", candidates.iter().copied()),
            ""
        );
    }
}
