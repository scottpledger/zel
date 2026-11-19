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

//! File extension matchers matching `com.google.devtools.build.lib.util.FileType` and
//! `com.google.devtools.build.lib.util.FileTypeSet`.

use crate::Os;
use std::fmt;

/// Matches paths by file extension (`FileType.java`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FileType {
    /// `FileType.NO_EXTENSION`: matches when there is no `.` after the last `/`.
    NoExtension,
    /// `ListFileType`: matches when the filename ends with any of the configured extensions.
    Extensions(Vec<String>),
}

impl FileType {
    pub const fn no_extension() -> Self {
        Self::NoExtension
    }

    pub fn of<I, S>(extensions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::Extensions(extensions.into_iter().map(Into::into).collect())
    }

    /// `FileType.getExtensions()`: empty for `NO_EXTENSION`, otherwise the configured list.
    pub fn extensions(&self) -> &[String] {
        match self {
            Self::NoExtension => &[],
            Self::Extensions(exts) => exts.as_slice(),
        }
    }

    /// `FileType.hasExtension`: case-sensitive `endsWith` on Unix, case-insensitive suffix
    /// match on Windows.
    pub fn has_extension(path: &str, ext: &str) -> bool {
        Self::has_extension_for_os(path, ext, Os::current() == Os::Windows)
    }

    pub fn has_extension_for_os(path: &str, ext: &str, case_insensitive: bool) -> bool {
        if !case_insensitive {
            return path.ends_with(ext);
        }
        let path_chars: Vec<char> = path.chars().collect();
        let ext_chars: Vec<char> = ext.chars().collect();
        if ext_chars.len() > path_chars.len() {
            return false;
        }
        let offset = path_chars.len() - ext_chars.len();
        path_chars[offset..]
            .iter()
            .zip(ext_chars.iter())
            .all(|(a, b)| char_eq_ignore_case(*a, *b))
    }

    pub fn has_any_extension<I, S>(path: &str, exts: I) -> bool
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        exts.into_iter()
            .any(|e| Self::has_extension(path, e.as_ref()))
    }

    /// `FileType.apply(String)` / `matches`.
    pub fn matches(&self, path: &str) -> bool {
        self.matches_for_os(path, Os::current() == Os::Windows)
    }

    pub fn matches_for_os(&self, path: &str, case_insensitive: bool) -> bool {
        match self {
            Self::NoExtension => {
                let after_slash = match path.rfind('/') {
                    Some(idx) => &path[idx + 1..],
                    None => path,
                };
                !after_slash.contains('.')
            }
            Self::Extensions(exts) => exts
                .iter()
                .any(|ext| Self::has_extension_for_os(path, ext, case_insensitive)),
        }
    }

    /// `FileType.contains(Iterable, FileType)` / `contains(T, FileType...)`: requires at least one
    /// matcher and returns true if any item matches any matcher.
    pub fn contains<I, P>(items: I, types: &[FileType]) -> bool
    where
        I: IntoIterator<Item = P>,
        P: AsRef<str>,
    {
        assert!(!types.is_empty(), "Must specify at least one file type");
        items
            .into_iter()
            .any(|p| types.iter().any(|t| t.matches(p.as_ref())))
    }

    pub fn filter<I, P>(items: I, matcher: &FileType) -> Vec<P>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<str>,
    {
        items
            .into_iter()
            .filter(|p| matcher.matches(p.as_ref()))
            .collect()
    }

    pub fn except<I, P>(items: I, matcher: &FileType) -> Vec<P>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<str>,
    {
        items
            .into_iter()
            .filter(|p| !matcher.matches(p.as_ref()))
            .collect()
    }

    /// `FileType.filterList`: empty `types` yields an empty `Vec`.
    pub fn filter_list<I, P>(items: I, types: &[FileType]) -> Vec<P>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<str>,
    {
        if types.is_empty() {
            return Vec::new();
        }
        items
            .into_iter()
            .filter(|p| types.iter().any(|t| t.matches(p.as_ref())))
            .collect()
    }
}

fn char_eq_ignore_case(a: char, b: char) -> bool {
    if a == b {
        return true;
    }
    let au: Vec<char> = a.to_uppercase().collect();
    let bu: Vec<char> = b.to_uppercase().collect();
    if au == bu {
        return true;
    }
    let al: Vec<char> = a.to_lowercase().collect();
    let bl: Vec<char> = b.to_lowercase().collect();
    al == bl
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FileTypeSetRepr {
    AnyFile,
    /// `is_none_singleton` distinguishes the `NO_FILE` singleton (`FileTypeSet.of()`) from an
    /// empty set constructed via `NO_FILE.including()`, matching Java `this == NO_FILE`.
    Set {
        types: Vec<FileType>,
        is_none_singleton: bool,
    },
}

/// A set of [`FileType`] matchers (`com.google.devtools.build.lib.util.FileTypeSet`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTypeSet {
    repr: FileTypeSetRepr,
}

impl FileTypeSet {
    /// `FileTypeSet.ANY_FILE`.
    pub const fn any_file() -> Self {
        Self {
            repr: FileTypeSetRepr::AnyFile,
        }
    }

    /// `FileTypeSet.NO_FILE`.
    pub const fn no_file() -> Self {
        Self {
            repr: FileTypeSetRepr::Set {
                types: Vec::new(),
                is_none_singleton: true,
            },
        }
    }

    /// `FileTypeSet.of(FileType...)`: returns `NO_FILE` when `types` is empty; otherwise
    /// deduplicates `types` in insertion order.
    pub fn of<I>(types: I) -> Self
    where
        I: IntoIterator<Item = FileType>,
    {
        let mut deduped = Vec::new();
        for t in types {
            if !deduped.contains(&t) {
                deduped.push(t);
            }
        }
        if deduped.is_empty() {
            return Self::no_file();
        }
        Self {
            repr: FileTypeSetRepr::Set {
                types: deduped,
                is_none_singleton: false,
            },
        }
    }

    pub fn is_any(&self) -> bool {
        matches!(self.repr, FileTypeSetRepr::AnyFile)
    }

    /// `FileTypeSet.isNone()`: true only for the `NO_FILE` singleton (`of([])` / `no_file()`),
    /// false for `NO_FILE.including([])`.
    pub fn is_none(&self) -> bool {
        matches!(
            self.repr,
            FileTypeSetRepr::Set {
                is_none_singleton: true,
                ..
            }
        )
    }

    /// `FileTypeSet.including(FileType...)`. Panics if `self` is `ANY_FILE` (matching Java, where
    /// `ANY_FILE.types` is `null`).
    pub fn including<I>(&self, extra: I) -> Self
    where
        I: IntoIterator<Item = FileType>,
    {
        match &self.repr {
            FileTypeSetRepr::AnyFile => {
                panic!("Cannot call including() on FileTypeSet.ANY_FILE")
            }
            FileTypeSetRepr::Set { types, .. } => {
                let mut combined = types.clone();
                for t in extra {
                    if !combined.contains(&t) {
                        combined.push(t);
                    }
                }
                Self {
                    repr: FileTypeSetRepr::Set {
                        types: combined,
                        is_none_singleton: false,
                    },
                }
            }
        }
    }

    pub fn matches(&self, path: &str) -> bool {
        self.matches_for_os(path, Os::current() == Os::Windows)
    }

    pub fn matches_for_os(&self, path: &str, case_insensitive: bool) -> bool {
        match &self.repr {
            FileTypeSetRepr::AnyFile => true,
            FileTypeSetRepr::Set { types, .. } => types
                .iter()
                .any(|t| t.matches_for_os(path, case_insensitive)),
        }
    }

    /// `FileTypeSet.getExtensions()`: concatenates each member's extensions in insertion order
    /// (empty for `ANY_FILE`).
    pub fn extensions(&self) -> Vec<String> {
        match &self.repr {
            FileTypeSetRepr::AnyFile => Vec::new(),
            FileTypeSetRepr::Set { types, .. } => types
                .iter()
                .flat_map(|t| t.extensions().iter().cloned())
                .collect(),
        }
    }
}

/// Formats a slice of strings using `StringUtil.joinEnglishList(items, "or")`.
pub fn join_english_list(items: &[String]) -> String {
    match items.len() {
        0 => "nothing".to_string(),
        1 => items[0].clone(),
        2 => format!("{} or {}", items[0], items[1]),
        _ => {
            let head = items[..items.len() - 1].join(", ");
            format!("{head} or {}", items[items.len() - 1])
        }
    }
}

impl fmt::Display for FileTypeSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_any() {
            f.write_str("any files")
        } else {
            f.write_str(&join_english_list(&self.extensions()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_type_extension_and_no_extension() {
        let no_ext = FileType::no_extension();
        assert!(no_ext.matches("foo/bar/BUILD"));
        assert!(no_ext.matches("dir.with.dots/Makefile"));
        assert!(!no_ext.matches("dir/file.cc"));
        assert!(!no_ext.matches(".gitignore"));
        assert!(no_ext.extensions().is_empty());

        let cc = FileType::of([".cc", ".cpp"]);
        assert!(cc.matches("a/b.cc"));
        assert!(cc.matches("a/b.cpp"));
        assert!(!cc.matches_for_os("a/b.CC", false));
        assert!(cc.matches_for_os("a/b.CC", true));
        assert_eq!(
            FileType::filter_list(["a.cc", "b.h"], &[]),
            Vec::<&str>::new()
        );
        assert_eq!(
            FileType::filter_list(["a.cc", "b.h", "c.cpp"], &[cc.clone()]),
            vec!["a.cc", "c.cpp"]
        );
        assert!(FileType::contains(["a.h", "b.cc"], &[cc]));
    }

    #[test]
    fn file_type_set_semantics() {
        let none = FileTypeSet::of([]);
        assert!(none.is_none());
        assert!(!none.matches("a.txt"));

        let empty_including = FileTypeSet::no_file().including([]);
        assert!(!empty_including.is_none());
        assert!(!empty_including.matches("a.txt"));

        let any = FileTypeSet::any_file();
        assert!(any.is_any());
        assert!(any.matches("anything.xyz"));
        assert_eq!(any.to_string(), "any files");
        assert!(any.extensions().is_empty());

        let py = FileType::of([".py"]);
        let sh = FileType::of([".sh", ".bash"]);
        let set = FileTypeSet::of([py.clone(), py]).including([sh]);
        assert!(set.matches("tool.py"));
        assert!(set.matches("run.bash"));
        assert!(!set.matches("lib.rs"));
        assert_eq!(set.extensions(), vec![".py", ".sh", ".bash"]);
        assert_eq!(set.to_string(), ".py, .sh or .bash");
    }
}
