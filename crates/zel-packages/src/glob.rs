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

//! `glob()` / `subpackages()` evaluation (`UnixGlob`, `NonSkyframeGlobber`, `GlobCache`,
//! `FragmentProducer`, `DirectoryDirentProducer`, `PatternWithWildcardProducer`,
//! `PatternWithoutWildcardProducer`, `SymlinkProducer`).

use crate::lookup::{IgnoredSubdirectories, PackageLookup, PackageLookupConfig};
use std::collections::{BTreeSet, HashSet};
use std::io;
use zel_cmdline::PackageIdentifier;
use zel_vfs::{resolve_symlink_chain, DirentType, FileSystem, PathFragment, SymlinkError};

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

impl GlobOp {
    pub fn bazel_name(self) -> &'static str {
        match self {
            Self::Files => "FILES",
            Self::FilesAndDirs => "FILES_AND_DIRS",
            Self::Subpackages => "SUBPACKAGES",
        }
    }
}

/// Errors returned by [`evaluate_checked`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobEvalError {
    /// Syntax error in an include or exclude pattern (`BadGlobException` -> Starlark `EvalException`).
    BadPattern(String),
    /// Filesystem or symlink-cycle/infinite-expansion error (`IOException` -> package error + `[]`).
    IoError(String),
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

fn is_java_line_terminator(u: u16) -> bool {
    matches!(u, 0x000A | 0x000D | 0x0085 | 0x2028 | 0x2029)
}

/// Matches one path segment against a pattern segment, matching `UnixGlob.matches`:
/// - `""` never matches
/// - `"*"` and `"**"` match everything (including dotfiles)
/// - any other pattern not starting with `'.'` does NOT match a name starting with `'.'`
/// - `*suffix` (no other `*`) and `prefix*` (no other `*`) use fast string prefix/suffix checks
/// - general path strips `'('` and `')'` (per `UnixGlob.makePatternFromWildcard`) and matches
///   UTF-16 code units where `*` and `?` do not cross Java regex line terminators (`\n`, `\r`,
///   `\u0085`, `\u2028`, `\u2029`).
pub fn segment_matches(pat: &str, s: &str) -> bool {
    if pat.is_empty() || s.is_empty() {
        return false;
    }
    if pat == "**" || pat == "*" {
        return true;
    }
    if s.starts_with('.') && !pat.starts_with('.') {
        return false;
    }
    if let Some(rest) = pat.strip_prefix('*') {
        if !rest.contains('*') {
            return s.ends_with(rest);
        }
    }
    if let Some(prefix) = pat.strip_suffix('*') {
        if !prefix.contains('*') {
            return s.starts_with(prefix);
        }
    }
    // General regex path: strip '(' and ')' and match UTF-16 code units.
    let p: Vec<u16> = pat
        .chars()
        .filter(|&c| c != '(' && c != ')')
        .flat_map(|c| {
            let mut buf = [0u16; 2];
            c.encode_utf16(&mut buf).to_vec()
        })
        .collect();
    let t: Vec<u16> = s.encode_utf16().collect();
    const WILDCARD_STAR: u16 = 0; // handled via separate kind vec
    #[derive(Clone, Copy)]
    enum Tok {
        Star,
        Ques,
        Lit(u16),
    }
    let _ = WILDCARD_STAR;
    let toks: Vec<Tok> = pat
        .chars()
        .filter(|&c| c != '(' && c != ')')
        .flat_map(|c| match c {
            '*' => vec![Tok::Star],
            '?' => vec![Tok::Ques],
            other => {
                let mut buf = [0u16; 2];
                other
                    .encode_utf16(&mut buf)
                    .iter()
                    .copied()
                    .map(Tok::Lit)
                    .collect()
            }
        })
        .collect();
    let _ = p;
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        let cur = t[ti];
        if pi < toks.len() {
            match toks[pi] {
                Tok::Ques if !is_java_line_terminator(cur) => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                Tok::Lit(u) if u == cur => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                Tok::Star => {
                    star = Some(pi);
                    mark = ti;
                    pi += 1;
                    continue;
                }
                _ => {}
            }
        }
        if let Some(sp) = star {
            if !is_java_line_terminator(t[mark]) {
                pi = sp + 1;
                mark += 1;
                ti = mark;
                continue;
            }
        }
        return false;
    }
    while pi < toks.len() && matches!(toks[pi], Tok::Star) {
        pi += 1;
    }
    pi == toks.len()
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
    /// Optional package lookup config + package ID so `.bazelignore`, `REPO.bazel`
    /// `ignore_directories()`, `--deleted_packages`, and package-name validation govern subpackage
    /// boundaries and directory pruning.
    pub lookup: Option<(&'a PackageLookupConfig, &'a PackageIdentifier)>,
}

fn is_subpackage_dir_fs(fs: &dyn FileSystem, dir: &PathFragment) -> bool {
    ["BUILD.bazel", "BUILD"].iter().any(|n| {
        fs.stat(&dir.get_relative_str(n), true)
            .map(|s| s.is_file || s.is_special_file)
            .unwrap_or(false)
    })
}

fn is_wildcard_free(pattern: &str) -> bool {
    !pattern.contains('*') && !pattern.contains('?')
}

/// `UnixGlob.removeExcludes`: removes excluded paths from `paths`.
/// - Wildcard-free excludes are literal set removals without pattern validation.
/// - `**/*` excludes with wildcard-free head and tail use prefix/suffix removal without validation.
/// - All other excludes validate via `check_pattern_for_error` and match segments.
pub fn remove_excludes(paths: &mut BTreeSet<String>, excludes: &[String]) -> Result<(), String> {
    let mut head_tail: Vec<(&str, &str)> = Vec::new();
    let mut complex: Vec<Vec<&str>> = Vec::new();
    for ex in excludes {
        if is_wildcard_free(ex) {
            paths.remove(ex.as_str());
            continue;
        }
        if let Some(pos) = ex.find("**/*") {
            let head = &ex[..pos];
            let tail = &ex[pos + 4..];
            if is_wildcard_free(head) && is_wildcard_free(tail) {
                head_tail.push((head, tail));
                continue;
            }
        }
        if let Some(e) = check_pattern_for_error(ex) {
            return Err(format!("{e} (in glob pattern '{ex}')"));
        }
        complex.push(ex.split('/').collect());
    }
    if !head_tail.is_empty() {
        paths.retain(|p| {
            !head_tail
                .iter()
                .any(|(h, t)| p.starts_with(h) && p.ends_with(t))
        });
    }
    if !complex.is_empty() {
        paths.retain(|p| {
            let parts: Vec<&str> = p.split('/').collect();
            !complex.iter().any(|e| path_matches(e, &parts))
        });
    }
    Ok(())
}

struct Walker<'a, 'b> {
    ctx: &'a GlobContext<'b>,
    op: GlobOp,
    ignored: IgnoredSubdirectories,
    out: BTreeSet<String>,
}

impl<'a, 'b> Walker<'a, 'b> {
    fn new(ctx: &'a GlobContext<'b>, op: GlobOp) -> Self {
        let ignored = if let Some((cfg, pkg_id)) = ctx.lookup {
            if let Some(root) = cfg.resolver.repo_root(pkg_id.repository()) {
                cfg.ignored_subdirectories(&root)
            } else {
                IgnoredSubdirectories::default()
            }
        } else {
            IgnoredSubdirectories::default()
        };
        Self {
            ctx,
            op,
            ignored,
            out: BTreeSet::new(),
        }
    }

    fn join(rel_dir: &str, name: &str) -> String {
        if rel_dir.is_empty() {
            name.to_string()
        } else {
            format!("{rel_dir}/{name}")
        }
    }

    fn abs_of(&self, rel: &str) -> PathFragment {
        if rel.is_empty() {
            self.ctx.package_dir.clone()
        } else {
            self.ctx.package_dir.get_relative_str(rel)
        }
    }

    fn is_ignored_dir(&self, rel: &str) -> bool {
        if let Some((_, pkg_id)) = self.ctx.lookup {
            let frag = pkg_id.package_fragment().get_relative_str(rel);
            return self.ignored.is_ignored(&frag);
        }
        false
    }

    /// Checks whether `rel` is a valid subpackage (honor `.bazelignore`, `REPO.bazel`,
    /// `--deleted_packages`, and package-name validity when `lookup` is present).
    fn check_subpackage(&self, rel: &str) -> Result<bool, String> {
        if let Some((cfg, pkg_id)) = self.ctx.lookup {
            let frag = pkg_id.package_fragment().get_relative_str(rel);
            let Ok(sub_id) = PackageIdentifier::create(pkg_id.repository().clone(), frag) else {
                // Invalid package names (e.g. non-ASCII or `.`-only segments) are not subpackages.
                return Ok(false);
            };
            return match cfg.lookup(&sub_id) {
                PackageLookup::Found { .. } => Ok(true),
                PackageLookup::IoError(e) => Err(e),
                _ => Ok(false),
            };
        }
        // Fallback when no PackageLookupConfig is provided: require valid main-repo package name
        // and BUILD / BUILD.bazel.
        if PackageIdentifier::create_in_main_repo(rel).is_err() {
            return Ok(false);
        }
        Ok(is_subpackage_dir_fs(self.ctx.fs, &self.abs_of(rel)))
    }

    fn all_remaining_double_star(segs: &[&str], frag_idx: usize) -> bool {
        segs[frag_idx + 1..].iter().all(|s| *s == "**")
    }

    fn handle_directory(
        &mut self,
        segs: &[&str],
        frag_idx: usize,
        rel: String,
        visited: &mut Option<HashSet<(String, usize)>>,
    ) -> Result<(), String> {
        if self.is_ignored_dir(&rel) {
            return Ok(());
        }
        let is_subpkg = self.check_subpackage(&rel)?;
        if is_subpkg {
            if self.op == GlobOp::Subpackages && Self::all_remaining_double_star(segs, frag_idx) {
                self.out.insert(rel);
            }
            // Never descend into a subpackage (including for `subpackages(["**"])`).
            return Ok(());
        }
        if self.op == GlobOp::FilesAndDirs && frag_idx + 1 == segs.len() {
            self.out.insert(rel.clone());
        }
        let next_idx = if segs[frag_idx] == "**" {
            frag_idx
        } else {
            frag_idx + 1
        };
        if next_idx == segs.len() {
            return Ok(());
        }
        if let Some(v) = visited.as_mut() {
            if !v.insert((rel.clone(), next_idx)) {
                return Ok(());
            }
        }
        self.walk(segs, next_idx, &rel, visited)
    }

    fn want_file(&mut self, rel: String, frag_idx: usize, segs_len: usize) {
        if self.op != GlobOp::Subpackages && frag_idx + 1 == segs_len {
            self.out.insert(rel);
        }
    }

    fn walk(
        &mut self,
        segs: &[&str],
        frag_idx: usize,
        rel_dir: &str,
        visited: &mut Option<HashSet<(String, usize)>>,
    ) -> Result<(), String> {
        if frag_idx >= segs.len() {
            return Ok(());
        }
        let seg = segs[frag_idx];
        if seg == "**" {
            if frag_idx + 1 < segs.len() {
                // `**` matches zero segments.
                let do_skip = match visited.as_mut() {
                    Some(v) => v.insert((rel_dir.to_string(), frag_idx + 1)),
                    None => true,
                };
                if do_skip {
                    self.walk(segs, frag_idx + 1, rel_dir, visited)?;
                }
            } else if self.op == GlobOp::FilesAndDirs && !rel_dir.is_empty() {
                // Trailing `**` includes the intermediate directory it is applied to.
                self.out.insert(rel_dir.to_string());
            }
        }

        let has_wild = seg.contains('*') || seg.contains('?');
        if !has_wild {
            // `PatternWithoutWildcardProducer`
            let rel = Self::join(rel_dir, seg);
            let abs = self.abs_of(&rel);
            if rel_dir.is_empty() && self.ctx.skip_convenience_symlinks && seg.starts_with("bazel-")
            {
                if let Ok(st) = self.ctx.fs.stat(&abs, false) {
                    if st.is_symlink {
                        return Ok(());
                    }
                }
            }
            match resolve_symlink_chain(self.ctx.fs, &abs) {
                Ok(res) => {
                    let Some(st) = res.status else {
                        return Ok(());
                    };
                    if st.is_directory {
                        if res.is_symlink && res.unbounded_ancestor_chain.is_some() {
                            if let Some(chain) = res.unbounded_ancestor_chain {
                                return Err(SymlinkError::InfiniteExpansion {
                                    path_to_chain: Vec::new(),
                                    chain,
                                }
                                .to_string());
                            }
                        }
                        self.handle_directory(segs, frag_idx, rel, visited)?;
                    } else {
                        self.want_file(rel, frag_idx, segs.len());
                    }
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        } else {
            // `PatternWithWildcardProducer`
            let dir_abs = self.abs_of(rel_dir);
            let entries = self.ctx.fs.readdir(&dir_abs).map_err(|e| e.to_string())?;
            for e in entries {
                if e.dirent_type == DirentType::Unknown {
                    continue;
                }
                if rel_dir.is_empty()
                    && self.ctx.skip_convenience_symlinks
                    && e.name.starts_with("bazel-")
                    && e.dirent_type == DirentType::Symlink
                {
                    continue;
                }
                if !segment_matches(seg, &e.name) {
                    continue;
                }
                let rel = Self::join(rel_dir, &e.name);
                let child_abs = dir_abs.get_relative_str(&e.name);
                if e.dirent_type == DirentType::Symlink {
                    match self.ctx.fs.stat(&child_abs, false) {
                        Ok(lst) => {
                            if !lst.is_symlink {
                                return Err(format!(
                                    "readdir and stat disagree about whether {} is a symlink.",
                                    child_abs
                                ));
                            }
                        }
                        Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                        Err(err) => return Err(err.to_string()),
                    }
                    let res = resolve_symlink_chain(self.ctx.fs, &child_abs)
                        .map_err(|err| err.to_string())?;
                    let Some(st) = res.status else {
                        // Dangling symlink is tolerated and omitted.
                        continue;
                    };
                    if st.is_directory {
                        if let Some(chain) = res.unbounded_ancestor_chain {
                            return Err(SymlinkError::InfiniteExpansion {
                                path_to_chain: Vec::new(),
                                chain,
                            }
                            .to_string());
                        }
                        self.handle_directory(segs, frag_idx, rel, visited)?;
                    } else {
                        self.want_file(rel, frag_idx, segs.len());
                    }
                } else if e.dirent_type == DirentType::Directory {
                    self.handle_directory(segs, frag_idx, rel, visited)?;
                } else {
                    self.want_file(rel, frag_idx, segs.len());
                }
            }
            Ok(())
        }
    }
}

/// Result of a glob: matches, plus per-include-pattern emptiness for `allow_empty` checking.
#[derive(Debug)]
pub struct GlobResult {
    pub matches: Vec<String>,
    /// Include patterns that matched nothing (before applying `exclude`).
    pub empty_includes: Vec<String>,
    /// `true` if matches existed before `exclude` but all were excluded.
    pub all_excluded: bool,
}

/// Evaluates `includes` minus `excludes` in `ctx`, returning `GlobEvalError` on invalid patterns
/// (including `?` in `includes`) or filesystem / symlink errors.
pub fn evaluate_checked(
    ctx: &GlobContext<'_>,
    includes: &[String],
    excludes: &[String],
    op: GlobOp,
) -> Result<GlobResult, GlobEvalError> {
    for pat in includes {
        if pat.contains('?') {
            return Err(GlobEvalError::BadPattern(format!(
                "invalid glob pattern '{pat}': wildcard ? forbidden"
            )));
        }
        if let Some(e) = check_pattern_for_error(pat) {
            return Err(GlobEvalError::BadPattern(format!(
                "{e} (in glob pattern '{pat}')"
            )));
        }
    }
    let mut all = BTreeSet::new();
    let mut empty_includes = Vec::new();
    for pat in includes {
        let segs: Vec<&str> = pat.split('/').collect();
        let multi_double_star = segs.iter().filter(|&&s| s == "**").count() > 1;
        let mut visited = multi_double_star.then(HashSet::new);
        let mut w = Walker::new(ctx, op);
        w.walk(&segs, 0, "", &mut visited)
            .map_err(GlobEvalError::IoError)?;
        if w.out.is_empty() {
            empty_includes.push(pat.clone());
        }
        all.extend(w.out);
    }
    let before = all.len();
    remove_excludes(&mut all, excludes).map_err(GlobEvalError::BadPattern)?;
    let matches: Vec<String> = all.into_iter().collect();
    Ok(GlobResult {
        all_excluded: before > 0 && matches.is_empty(),
        matches,
        empty_includes,
    })
}

/// Evaluates `includes` minus `excludes` in `ctx`. Patterns must already be validated.
/// Kept for compatibility; callers needing I/O error propagation should use [`evaluate_checked`].
pub fn evaluate(
    ctx: &GlobContext<'_>,
    includes: &[String],
    excludes: &[String],
    op: GlobOp,
) -> GlobResult {
    evaluate_checked(ctx, includes, excludes, op).unwrap_or(GlobResult {
        matches: Vec::new(),
        empty_includes: includes.to_vec(),
        all_excluded: false,
    })
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
        let empty_frag = PathFragment::empty();
        let _ = empty_frag;
        let ctx = GlobContext {
            fs: fs.as_ref(),
            package_dir: &root,
            skip_convenience_symlinks: false,
            lookup: None,
        };
        let inc: Vec<String> = inc.iter().map(|s| s.to_string()).collect();
        let exc: Vec<String> = exc.iter().map(|s| s.to_string()).collect();
        evaluate_checked(&ctx, &inc, &exc, op).unwrap().matches
    }

    #[test]
    fn pattern_validation() {
        assert_eq!(
            check_pattern_for_error("").unwrap(),
            "pattern cannot be empty"
        );
        assert_eq!(
            check_pattern_for_error("/a").unwrap(),
            "pattern cannot be absolute"
        );
        assert_eq!(
            check_pattern_for_error("a//b").unwrap(),
            "empty segment not permitted"
        );
        assert_eq!(
            check_pattern_for_error("a/../b").unwrap(),
            "segment '..' not permitted"
        );
        assert_eq!(
            check_pattern_for_error("a/./b").unwrap(),
            "segment '.' not permitted"
        );
        assert_eq!(
            check_pattern_for_error("a**b").unwrap(),
            "recursive wildcard must be its own segment"
        );
        assert!(check_pattern_for_error("**/*.rs").is_none());
    }

    #[test]
    fn basic_wildcards_and_dotfiles() {
        let files = [
            "a.rs",
            "b.rs",
            "c.txt",
            "sub/d.rs",
            "sub/deep/e.rs",
            ".hidden.rs",
            ".dot",
        ];
        // Dotfiles are NOT matched by `*.rs` or `**/*.rs`, only by `*`, `**`, or explicit leading `.`.
        assert_eq!(
            run(&files, &["*.rs"], &[], GlobOp::Files),
            vec!["a.rs", "b.rs"]
        );
        assert_eq!(
            run(&files, &[".*.rs"], &[], GlobOp::Files),
            vec![".hidden.rs"]
        );
        assert_eq!(
            run(&files, &["**/*.rs"], &[], GlobOp::Files),
            vec!["a.rs", "b.rs", "sub/d.rs", "sub/deep/e.rs"]
        );
        assert_eq!(
            run(&files, &["sub/**"], &[], GlobOp::Files),
            vec!["sub/d.rs", "sub/deep/e.rs"]
        );
        assert_eq!(run(&files, &["c.txt"], &[], GlobOp::Files), vec!["c.txt"]);
        assert!(run(&files, &["nothing*"], &[], GlobOp::Files).is_empty());
        // `?` in include is forbidden, but allowed in exclude.
        let (fs, root) = fs_with(&files);
        let ctx = GlobContext {
            fs: fs.as_ref(),
            package_dir: &root,
            skip_convenience_symlinks: false,
            lookup: None,
        };
        assert!(matches!(
            evaluate_checked(&ctx, &["?.txt".into()], &[], GlobOp::Files),
            Err(GlobEvalError::BadPattern(_))
        ));
        assert_eq!(
            run(&files, &["*.rs"], &["?.rs"], GlobOp::Files),
            Vec::<String>::new()
        );
        // Parens in general regex path are stripped per UnixGlob.makePatternFromWildcard.
        assert_eq!(
            run(&["foo.cc", "(bar).cc"], &["(*.cc)"], &[], GlobOp::Files),
            vec!["(bar).cc", "foo.cc"]
        );
    }

    #[test]
    fn exclude_and_directories() {
        let files = ["a.rs", "b.rs", "sub/d.rs", "sub/deep/e.rs"];
        assert_eq!(
            run(&files, &["**/*.rs"], &["b.rs", "sub/*"], GlobOp::Files),
            vec!["a.rs", "sub/deep/e.rs"]
        );
        // exclude_directories=0 includes directories, including `*/**` intermediate dir.
        assert_eq!(
            run(&files, &["*"], &[], GlobOp::FilesAndDirs),
            vec!["a.rs", "b.rs", "sub"]
        );
        assert_eq!(
            run(&files, &["*"], &[], GlobOp::Files),
            vec!["a.rs", "b.rs"]
        );
        assert_eq!(
            run(&files, &["*/**"], &[], GlobOp::FilesAndDirs),
            vec!["sub", "sub/d.rs", "sub/deep", "sub/deep/e.rs"]
        );
        // Wildcard-free and `**/*` shortcut excludes skip pattern validation (Bazel UnixGlob).
        assert_eq!(
            run(&files, &["*.rs"], &["../invalid", ""], GlobOp::Files),
            vec!["a.rs", "b.rs"]
        );
    }

    #[test]
    fn subpackage_boundaries() {
        let files = [
            "a.rs",
            "sub/BUILD",
            "sub/x.rs",
            "ok/y.rs",
            "b/BUILD.bazel",
            "b/z.rs",
            "sub/nested/BUILD",
        ];
        assert_eq!(
            run(&files, &["**/*.rs"], &[], GlobOp::Files),
            vec!["a.rs", "ok/y.rs"]
        );
        assert_eq!(
            run(&files, &["*"], &[], GlobOp::FilesAndDirs),
            vec!["a.rs", "ok"]
        );
        assert_eq!(
            run(&files, &["sub/**"], &[], GlobOp::FilesAndDirs),
            Vec::<String>::new()
        );
        assert!(run(&files, &["sub/x.rs"], &[], GlobOp::Files).is_empty());
        // Subpackages does NOT descend into subpackages: `sub/nested` is NOT returned.
        assert_eq!(
            run(&files, &["**"], &[], GlobOp::Subpackages),
            vec!["b", "sub"]
        );
        assert_eq!(run(&files, &["s*"], &[], GlobOp::Subpackages), vec!["sub"]);
    }

    #[test]
    fn symlink_cycle_and_convenience_filter() {
        let (fs, root) = fs_with(&["a.txt", "target/b.txt"]);
        fs.create_symbolic_link(
            &root.get_relative_str("bazel-bin"),
            &PathFragment::create("target"),
        )
        .unwrap();
        let ctx_skip = GlobContext {
            fs: fs.as_ref(),
            package_dir: &root,
            skip_convenience_symlinks: true,
            lookup: None,
        };
        // Both wildcard and literal `bazel-bin/...` are skipped at main-repo root.
        let m = evaluate_checked(&ctx_skip, &["**".into()], &[], GlobOp::Files)
            .unwrap()
            .matches;
        assert_eq!(m, vec!["a.txt", "target/b.txt"]);
        let m2 = evaluate_checked(&ctx_skip, &["bazel-bin/b.txt".into()], &[], GlobOp::Files)
            .unwrap()
            .matches;
        assert!(m2.is_empty());

        // Directory symlink cycle / ancestor expansion fails the glob.
        fs.create_symbolic_link(
            &root.get_relative_str("loop_anc"),
            &PathFragment::create("."),
        )
        .unwrap();
        let err = evaluate_checked(&ctx_skip, &["*".into()], &[], GlobOp::Files).unwrap_err();
        assert!(matches!(err, GlobEvalError::IoError(_)), "{err:?}");
    }

    #[test]
    fn empty_reporting() {
        let (fs, root) = fs_with(&["a.rs"]);
        let empty_frag = PathFragment::empty();
        let _ = empty_frag;
        let ctx = GlobContext {
            fs: fs.as_ref(),
            package_dir: &root,
            skip_convenience_symlinks: false,
            lookup: None,
        };
        let r = evaluate_checked(
            &ctx,
            &["*.rs".to_string(), "*.zz".to_string()],
            &["a.rs".to_string()],
            GlobOp::Files,
        )
        .unwrap();
        assert_eq!(r.empty_includes, vec!["*.zz"]);
        assert!(r.all_excluded);
    }
}
