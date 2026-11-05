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

//! `glob()` / `subpackages()` evaluation (`UnixGlob`, `NonSkyframeGlobber`, `GlobCache`).

use std::collections::BTreeSet;
use zel_vfs::{DirentType, FileSystem, PathFragment};

/// What a glob operation returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobOp {
    /// Files only (`exclude_directories = 1`).
    Files,
    /// Files and directories (`exclude_directories = 0`).
    FilesAndDirs,
    /// Subpackage directories (`subpackages()`).
    Subpackages,
}

/// `UnixGlob.checkPatternForError`.
pub fn check_pattern_for_error(pattern: &str) -> Option<String> {
    if pattern.is_empty() {
        return Some("pattern cannot be empty".into());
    }
    if pattern.starts_with('/') {
        return Some("pattern cannot be absolute".into());
    }
    for seg in pattern.split('/') {
        if seg.is_empty() {
            return Some("empty segment not permitted".into());
        }
        if seg == "." || seg == ".." {
            return Some(format!("segment '{seg}' not permitted"));
        }
        if seg.contains("**") && seg != "**" {
            return Some("recursive wildcard must be its own segment".into());
        }
    }
    None
}

/// Matches one path segment against a pattern segment with `*` and `?` wildcards.
pub fn segment_matches(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = s.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) && p[pi] != '*' {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Matches a whole relative path against a pattern (with `**` segments).
pub fn path_matches(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => {
            // `**` matches zero or more segments.
            (0..=path.len()).any(|k| path_matches(rest, &path[k..]))
        }
        Some((p, rest)) => match path.split_first() {
            Some((s, prest)) => segment_matches(p, s) && path_matches(rest, prest),
            None => false,
        },
    }
}

/// Describes where a glob is evaluated.
pub struct GlobContext<'a> {
    pub fs: &'a dyn FileSystem,
    pub package_dir: &'a PathFragment,
    /// Skips symlinked `bazel-*` convenience entries at the package root (set for the main repo
    /// root package).
    pub skip_convenience_symlinks: bool,
}

fn is_subpackage_dir(fs: &dyn FileSystem, dir: &PathFragment) -> bool {
    ["BUILD.bazel", "BUILD"].iter().any(|n| {
        fs.stat(&dir.get_relative_str(n), true)
            .map(|s| s.is_file)
            .unwrap_or(false)
    })
}

struct Walker<'a, 'b> {
    ctx: &'a GlobContext<'b>,
    op: GlobOp,
    out: BTreeSet<String>,
}

impl Walker<'_, '_> {
    /// Entries of `rel_dir` as `(name, is_dir)`; dangling symlinks are skipped.
    fn entries(&self, rel_dir: &str) -> Vec<(String, bool)> {
        let abs = if rel_dir.is_empty() {
            self.ctx.package_dir.clone()
        } else {
            self.ctx.package_dir.get_relative_str(rel_dir)
        };
        let Ok(list) = self.ctx.fs.readdir(&abs) else {
            return Vec::new();
        };
        let mut v = Vec::new();
        for e in list {
            if rel_dir.is_empty()
                && self.ctx.skip_convenience_symlinks
                && e.name.starts_with("bazel-")
                && e.dirent_type == DirentType::Symlink
            {
                continue;
            }
            let child = abs.get_relative_str(&e.name);
            match self.ctx.fs.stat(&child, true) {
                Ok(st) => v.push((e.name, st.is_directory)),
                Err(_) => continue,
            }
        }
        v
    }

    fn join(rel_dir: &str, name: &str) -> String {
        if rel_dir.is_empty() {
            name.to_string()
        } else {
            format!("{rel_dir}/{name}")
        }
    }

    fn can_descend(&self, rel: &str) -> bool {
        !is_subpackage_dir(self.ctx.fs, &self.ctx.package_dir.get_relative_str(rel))
    }

    fn want(&mut self, rel: String, is_dir: bool) {
        match self.op {
            GlobOp::Files => {
                if !is_dir {
                    self.out.insert(rel);
                }
            }
            GlobOp::FilesAndDirs => {
                self.out.insert(rel);
            }
            GlobOp::Subpackages => {}
        }
    }

    fn walk(&mut self, segs: &[&str], rel_dir: &str) {
        let Some((seg, rest)) = segs.split_first() else {
            return;
        };
        if *seg == "**" {
            if rest.is_empty() {
                // `**` as last segment: everything below, recursively.
                for (name, is_dir) in self.entries(rel_dir) {
                    let rel = Self::join(rel_dir, &name);
                    if is_dir {
                        if !self.can_descend(&rel) {
                            continue;
                        }
                        self.want(rel.clone(), true);
                        self.walk(segs, &rel);
                    } else {
                        self.want(rel, false);
                    }
                }
                // `**` also matches the empty path for directories patterns like `foo/**`:
                // the directory itself (when listing dirs) is added by the caller.
                return;
            }
            // Zero directories matched.
            self.walk(rest, rel_dir);
            // One or more directories.
            for (name, is_dir) in self.entries(rel_dir) {
                if is_dir {
                    let rel = Self::join(rel_dir, &name);
                    if self.can_descend(&rel) {
                        self.walk(segs, &rel);
                    }
                }
            }
            return;
        }
        let has_wild = seg.contains('*') || seg.contains('?');
        if rest.is_empty() {
            if has_wild {
                for (name, is_dir) in self.entries(rel_dir) {
                    if segment_matches(seg, &name) {
                        let rel = Self::join(rel_dir, &name);
                        if is_dir && !self.can_descend(&rel) {
                            continue;
                        }
                        self.want(rel, is_dir);
                    }
                }
            } else {
                let rel = Self::join(rel_dir, seg);
                let abs = self.ctx.package_dir.get_relative_str(&rel);
                if let Ok(st) = self.ctx.fs.stat(&abs, true) {
                    if st.is_directory && !self.can_descend(&rel) {
                        return;
                    }
                    self.want(rel, st.is_directory);
                }
            }
        } else {
            for (name, is_dir) in self.entries(rel_dir) {
                if is_dir && (if has_wild { segment_matches(seg, &name) } else { *seg == name }) {
                    let rel = Self::join(rel_dir, &name);
                    if self.can_descend(&rel) {
                        self.walk(rest, &rel);
                    }
                }
            }
        }
    }

    /// Collects subpackage directories matching `segs`.
    fn walk_subpackages(&mut self, rel_dir: &str, all: &mut Vec<String>) {
        for (name, is_dir) in self.entries(rel_dir) {
            if !is_dir {
                continue;
            }
            let rel = Self::join(rel_dir, &name);
            if is_subpackage_dir(self.ctx.fs, &self.ctx.package_dir.get_relative_str(&rel)) {
                all.push(rel.clone());
                // Bazel's subpackages() reports nested subpackages as well when the pattern is
                // recursive; keep descending.
            }
            self.walk_subpackages(&rel, all);
        }
    }
}

/// Result of a glob: matches, plus per-include-pattern emptiness for `allow_empty` checking.
pub struct GlobResult {
    pub matches: Vec<String>,
    /// Include patterns that matched nothing (before applying `exclude`).
    pub empty_includes: Vec<String>,
    /// `true` if matches existed before `exclude` but all were excluded.
    pub all_excluded: bool,
}

/// Evaluates `includes` minus `excludes` in `ctx`. Patterns must already be validated.
pub fn evaluate(
    ctx: &GlobContext<'_>,
    includes: &[String],
    excludes: &[String],
    op: GlobOp,
) -> GlobResult {
    let mut all = BTreeSet::new();
    let mut empty_includes = Vec::new();
    for pat in includes {
        let segs: Vec<&str> = pat.split('/').collect();
        let mut w = Walker {
            ctx,
            op,
            out: BTreeSet::new(),
        };
        if op == GlobOp::Subpackages {
            let mut subs = Vec::new();
            w.walk_subpackages("", &mut subs);
            for s in subs {
                let parts: Vec<&str> = s.split('/').collect();
                if path_matches(&segs, &parts) {
                    w.out.insert(s);
                }
            }
        } else {
            w.walk(&segs, "");
            // `foo/**` also matches directory `foo` itself when directories are included.
            if segs.last() == Some(&"**") && op == GlobOp::FilesAndDirs && segs.len() > 1 {
                let dir = segs[..segs.len() - 1].join("/");
                let abs = ctx.package_dir.get_relative_str(&dir);
                if !dir.contains('*')
                    && !dir.contains('?')
                    && ctx.fs.stat(&abs, true).map(|s| s.is_directory).unwrap_or(false)
                {
                    w.out.insert(dir);
                }
            }
        }
        if w.out.is_empty() {
            empty_includes.push(pat.clone());
        }
        all.extend(w.out);
    }
    let before = all.len();
    let excl: Vec<Vec<&str>> = excludes.iter().map(|e| e.split('/').collect()).collect();
    let matches: Vec<String> = all
        .into_iter()
        .filter(|p| {
            let parts: Vec<&str> = p.split('/').collect();
            !excl.iter().any(|e| path_matches(e, &parts))
        })
        .collect();
    GlobResult {
        all_excluded: before > 0 && matches.is_empty(),
        matches,
        empty_includes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use zel_vfs::InMemoryFileSystem;

    fn fs_with(files: &[&str]) -> (Arc<InMemoryFileSystem>, PathFragment) {
        let fs = Arc::new(InMemoryFileSystem::default());
        let root = PathFragment::create("/ws");
        fs.create_directory_and_parents(&root).unwrap();
        for f in files {
            let p = root.get_relative_str(f);
            if let Some(parent) = p.parent_directory() {
                fs.create_directory_and_parents(&parent).unwrap();
            }
            fs.write_file(&p, b"x").unwrap();
        }
        (fs, root)
    }

    fn run(files: &[&str], inc: &[&str], exc: &[&str], op: GlobOp) -> Vec<String> {
        let (fs, root) = fs_with(files);
        let ctx = GlobContext {
            fs: fs.as_ref(),
            package_dir: &root,
            skip_convenience_symlinks: false,
        };
        let inc: Vec<String> = inc.iter().map(|s| s.to_string()).collect();
        let exc: Vec<String> = exc.iter().map(|s| s.to_string()).collect();
        evaluate(&ctx, &inc, &exc, op).matches
    }

    #[test]
    fn pattern_validation() {
        assert_eq!(check_pattern_for_error("").unwrap(), "pattern cannot be empty");
        assert_eq!(check_pattern_for_error("/a").unwrap(), "pattern cannot be absolute");
        assert_eq!(check_pattern_for_error("a//b").unwrap(), "empty segment not permitted");
        assert_eq!(check_pattern_for_error("a/../b").unwrap(), "segment '..' not permitted");
        assert_eq!(check_pattern_for_error("a/./b").unwrap(), "segment '.' not permitted");
        assert_eq!(
            check_pattern_for_error("a**b").unwrap(),
            "recursive wildcard must be its own segment"
        );
        assert!(check_pattern_for_error("**/*.rs").is_none());
    }

    #[test]
    fn basic_wildcards() {
        let files = ["a.rs", "b.rs", "c.txt", "sub/d.rs", "sub/deep/e.rs", ".hidden.rs"];
        assert_eq!(
            run(&files, &["*.rs"], &[], GlobOp::Files),
            vec![".hidden.rs", "a.rs", "b.rs"]
        );
        assert_eq!(
            run(&files, &["**/*.rs"], &[], GlobOp::Files),
            vec![".hidden.rs", "a.rs", "b.rs", "sub/d.rs", "sub/deep/e.rs"]
        );
        assert_eq!(
            run(&files, &["sub/**"], &[], GlobOp::Files),
            vec!["sub/d.rs", "sub/deep/e.rs"]
        );
        assert_eq!(run(&files, &["?.txt"], &[], GlobOp::Files), vec!["c.txt"]);
        assert_eq!(run(&files, &["c.txt"], &[], GlobOp::Files), vec!["c.txt"]);
        assert!(run(&files, &["nothing*"], &[], GlobOp::Files).is_empty());
    }

    #[test]
    fn exclude_and_directories() {
        let files = ["a.rs", "b.rs", "sub/d.rs"];
        assert_eq!(
            run(&files, &["**/*.rs"], &["b.rs", "sub/*"], GlobOp::Files),
            vec!["a.rs"]
        );
        // exclude_directories=0 includes directories.
        assert_eq!(run(&files, &["*"], &[], GlobOp::FilesAndDirs), vec!["a.rs", "b.rs", "sub"]);
        assert_eq!(run(&files, &["*"], &[], GlobOp::Files), vec!["a.rs", "b.rs"]);
    }

    #[test]
    fn subpackage_boundaries() {
        let files = ["a.rs", "sub/BUILD", "sub/x.rs", "ok/y.rs", "b/BUILD.bazel", "b/z.rs"];
        assert_eq!(
            run(&files, &["**/*.rs"], &[], GlobOp::Files),
            vec!["a.rs", "ok/y.rs"]
        );
        assert_eq!(run(&files, &["*"], &[], GlobOp::FilesAndDirs), vec!["a.rs", "ok"]);
        assert!(run(&files, &["sub/x.rs"], &[], GlobOp::Files).is_empty());
        assert_eq!(
            run(&files, &["**"], &[], GlobOp::Subpackages),
            vec!["b", "sub"]
        );
        assert_eq!(run(&files, &["s*"], &[], GlobOp::Subpackages), vec!["sub"]);
    }

    #[test]
    fn empty_reporting() {
        let (fs, root) = fs_with(&["a.rs"]);
        let ctx = GlobContext {
            fs: fs.as_ref(),
            package_dir: &root,
            skip_convenience_symlinks: false,
        };
        let r = evaluate(
            &ctx,
            &["*.rs".to_string(), "*.zz".to_string()],
            &["a.rs".to_string()],
            GlobOp::Files,
        );
        assert_eq!(r.empty_includes, vec!["*.zz"]);
        assert!(r.all_excluded);
    }
}
