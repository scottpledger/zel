//! Native patch application: a port of Bazel's `PatchUtil` (unified diff / git-style
//! diffs, fuzzy hunk application as java-diff-utils `applyFuzzy(…, 2)`, renames,
//! create/delete, owner permission bits). No external `patch` binary is used and no
//! `.orig`/`.rej` files are produced.

use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

type R<T> = Result<T, String>;

struct ChunkHeader {
    old_size: usize,
    new_size: usize,
}

#[derive(PartialEq)]
enum Chk {
    Complete,
    Continue,
    Error,
}

impl ChunkHeader {
    fn check(&self, o: usize, n: usize) -> Chk {
        if o == self.old_size && n == self.new_size {
            Chk::Complete
        } else if o <= self.old_size && n <= self.new_size {
            Chk::Continue
        } else {
            Chk::Error
        }
    }
}

/// Parses `@@ -a[,b] +c[,d] @@`; returns (a, b, c, d) with default counts of 1.
fn parse_chunk_header(s: &str) -> Option<(usize, usize, usize, usize)> {
    let rest = s.strip_prefix("@@")?;
    let rest = rest.trim_start();
    if rest.len() == s.len() - 2 {
        return None; // needs whitespace
    }
    let rest = rest.strip_prefix('-')?;
    let (a, rest) = num_pair(rest)?;
    let rest2 = rest.trim_start();
    if rest2.len() == rest.len() {
        return None;
    }
    let rest2 = rest2.strip_prefix('+')?;
    let (c, rest3) = num_pair(rest2)?;
    let rest3t = rest3.trim_start();
    if rest3t.len() == rest3.len() || rest3t != "@@" {
        return None;
    }
    Some((a.0, a.1.unwrap_or(1), c.0, c.1.unwrap_or(1)))
}

fn num_pair(s: &str) -> Option<((usize, Option<usize>), &str)> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    let a: usize = s[..end].parse().ok()?;
    let rest = &s[end..];
    if let Some(r) = rest.strip_prefix(',') {
        let e2 = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
        if e2 == 0 {
            return None;
        }
        let b: usize = r[..e2].parse().ok()?;
        Some(((a, Some(b)), &r[e2..]))
    } else {
        Some(((a, None), rest))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Lt {
    OldFile,
    NewFile,
    ChunkHead,
    Add,
    Del,
    Eql,
    GitHeader,
    RenameFrom,
    RenameTo,
    NewMode,
    NewFileMode,
    OtherGit,
    NoNewline,
    Unknown,
}

const GIT_PREFIXES: [&str; 11] = [
    "old mode ",
    "new mode ",
    "deleted file mode ",
    "new file mode ",
    "copy from ",
    "copy to ",
    "rename old ",
    "rename new ",
    "similarity index ",
    "dissimilarity index ",
    "index ",
];

fn line_type(line: &str, reading: bool, git: bool) -> Lt {
    if line.starts_with('\\') {
        return Lt::NoNewline;
    }
    if reading {
        if line.starts_with('+') {
            return Lt::Add;
        }
        if line.starts_with('-') {
            return Lt::Del;
        }
        if line.starts_with(' ') || line.is_empty() {
            return Lt::Eql;
        }
    } else {
        if line.starts_with("--- ") {
            return Lt::OldFile;
        }
        if line.starts_with("+++ ") {
            return Lt::NewFile;
        }
        if line.starts_with("diff --git ") {
            return Lt::GitHeader;
        }
        if git {
            if line.starts_with("rename from ") {
                return Lt::RenameFrom;
            }
            if line.starts_with("rename to ") {
                return Lt::RenameTo;
            }
            if line.starts_with("new mode ") {
                return Lt::NewMode;
            }
            if line.starts_with("new file mode ") {
                return Lt::NewFileMode;
            }
            if GIT_PREFIXES.iter().any(|p| line.starts_with(p)) {
                return Lt::OtherGit;
            }
        }
    }
    if line.starts_with("@@") && line.rfind("@@") != Some(0) {
        if let Some(pos) = line[2..].find("@@") {
            let pos = pos + 2;
            if parse_chunk_header(&line[..pos + 2]).is_some() {
                return Lt::ChunkHead;
            }
        }
    }
    Lt::Unknown
}

/// Reads lines the way Guava's `readLines` does (\n, \r\n and \r terminate lines); latin1.
fn read_lines(path: &Path) -> R<Vec<String>> {
    let bytes = std::fs::read(path).map_err(|e| io_msg(path, &e))?;
    Ok(split_lines(&bytes))
}

fn split_lines(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    let mut pending = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' || b == b'\r' {
            out.push(std::mem::take(&mut cur));
            pending = false;
            if b == b'\r' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                i += 1;
            }
        } else {
            cur.push(b as char);
            pending = true;
        }
        i += 1;
    }
    if pending {
        out.push(cur);
    }
    out
}

/// Marks an error message as a Java `IOException` (reported without the "Error applying patch" wrapper).
const IO_MARK: char = '\u{1}';

/// Message of a `java.io.IOException` for `p`: `<path> (<strerror>)`.
fn io_msg(p: &Path, e: &std::io::Error) -> String {
    let m = match e.raw_os_error() {
        Some(2) => "No such file or directory".to_string(),
        Some(13) => "Permission denied".to_string(),
        Some(20) => "Not a directory".to_string(),
        Some(21) => "Is a directory".to_string(),
        Some(17) => "File exists".to_string(),
        Some(62) => "Too many levels of symbolic links".to_string(),
        Some(66) => "Directory not empty".to_string(),
        _ => e.to_string(),
    };
    format!("{IO_MARK}{} ({})", p.display(), m)
}

fn write_lines(path: &Path, lines: &[String]) -> R<()> {
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let mut bytes = Vec::new();
    for l in lines {
        bytes.extend(l.chars().map(|c| c as u32 as u8));
        bytes.push(b'\n');
    }
    // Replace any symlink/readonly file rather than writing through it.
    let _ = std::fs::remove_file(path);
    std::fs::write(path, bytes).map_err(|e| io_msg(path, &e))
}

fn strip_path(path: &str, mut strip: i64) -> &str {
    let b = path.as_bytes();
    let mut pos = 0;
    while pos < b.len() && strip > 0 {
        if b[pos] == b'/' {
            strip -= 1;
        }
        pos += 1;
    }
    &path[pos..]
}

fn extract_path(line: &str, strip: i64, loc: usize) -> R<Option<String>> {
    let line = line.split('\t').next().unwrap_or("");
    if line.len() > 4 {
        let path = line[4..].trim();
        if path == "/dev/null" {
            return Ok(None);
        }
        let p = strip_path(path, strip);
        if !p.is_empty() {
            return Ok(Some(p.to_string()));
        }
    }
    Err(format!("Cannot determine file name with strip = {strip} at line {loc}:\n{line}"))
}

fn normalize(base: &Path, rel: &str) -> PathBuf {
    let mut out = if rel.starts_with('/') { PathBuf::from("/") } else { base.to_path_buf() };
    for c in Path::new(rel).components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(s) => out.push(s),
            _ => {}
        }
    }
    out
}

fn get_file_path(path: &str, out_dir: &Path, loc: usize) -> R<PathBuf> {
    let fp = normalize(out_dir, path);
    if !fp.starts_with(out_dir) {
        return Err(format!(
            "Cannot patch file outside of external repository ({}), file path = \"{}\" at line {}",
            out_dir.display(),
            path,
            loc
        ));
    }
    let mut real_out: Option<PathBuf> = None;
    let mut anc: Option<&Path> = Some(&fp);
    while let Some(a) = anc {
        if a == out_dir {
            break;
        }
        if std::fs::symlink_metadata(a).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
            if real_out.is_none() {
                real_out = std::fs::canonicalize(out_dir).ok();
            }
            match std::fs::canonicalize(a) {
                Ok(res) => {
                    if let Some(ro) = &real_out {
                        if !res.starts_with(ro) {
                            return Err(format!(
                                "Cannot patch file outside of external repository ({}), file path = \"{}\" at line {} (resolves to {})",
                                out_dir.display(),
                                path,
                                loc,
                                res.display()
                            ));
                        }
                    }
                }
                Err(e) => {
                    let detail = resolve_symlinks_error(a).unwrap_or_else(|| e.to_string());
                    return Err(format!("Cannot resolve file path \"{path}\" at line {loc}: {detail}"));
                }
            }
        }
        anc = a.parent();
    }
    Ok(fp)
}

/// A `ChangeDelta` of java-diff-utils (`UnifiedDiffUtils.parseUnifiedDiff` only creates those).
/// `Path.resolveSymbolicLinks()` failure message (`<unresolvable path> (<strerror>)`) for `p`.
fn resolve_symlinks_error(p: &Path) -> Option<String> {
    let mut cur: PathBuf = p.to_path_buf();
    for _ in 0..40 {
        let mut resolved = PathBuf::new();
        let mut comps = cur.components().peekable();
        let mut restart = false;
        while let Some(c) = comps.next() {
            resolved.push(c.as_os_str());
            match std::fs::symlink_metadata(&resolved) {
                Err(e) => {
                    let m = match e.raw_os_error() {
                        Some(2) => "No such file or directory",
                        Some(20) => "Not a directory",
                        Some(13) => "Permission denied",
                        _ => return None,
                    };
                    let mut full = resolved.clone();
                    for r in comps {
                        full.push(r.as_os_str());
                    }
                    return Some(format!("{} ({m})", full.display()));
                }
                Ok(md) if md.file_type().is_symlink() => {
                    let target = std::fs::read_link(&resolved).ok()?;
                    let mut next = if target.is_absolute() { target } else { resolved.parent()?.join(target) };
                    for r in comps {
                        next.push(r.as_os_str());
                    }
                    cur = normalize(Path::new("/"), &next.display().to_string());
                    restart = true;
                    break;
                }
                Ok(_) => {}
            }
        }
        if !restart {
            return None;
        }
    }
    Some(format!("{} (Too many levels of symbolic links)", p.display()))
}

#[derive(Debug)]
struct Delta {
    /// `Chunk.getPosition()` of the source chunk (`old_ln - 1`).
    pos: i64,
    source: Vec<String>,
    target: Vec<String>,
}

/// `UnifiedDiffUtils.parseUnifiedDiff`: skips everything up to the first `+++` line, then collects
/// each hunk as one delta. The hunk header's line numbers are used as given (a `0` counts as `1`),
/// without the unified-diff convention for empty ranges.
fn parse_unified(content: &[String]) -> Vec<Delta> {
    let mut deltas = Vec::new();
    let mut in_prelude = true;
    let mut raw: Vec<(char, String)> = Vec::new();
    let mut old_ln: i64 = 0;
    let mut flush = |raw: &mut Vec<(char, String)>, old_ln: i64, deltas: &mut Vec<Delta>| {
        if raw.is_empty() {
            return;
        }
        let mut source = Vec::new();
        let mut target = Vec::new();
        for (tag, rest) in raw.drain(..) {
            if tag == ' ' || tag == '-' {
                source.push(rest.clone());
            }
            if tag == ' ' || tag == '+' {
                target.push(rest);
            }
        }
        deltas.push(Delta { pos: old_ln - 1, source, target });
    };
    for l in content {
        if in_prelude {
            if l.starts_with("+++") {
                in_prelude = false;
            }
            continue;
        }
        if let Some((a, _, c, _)) = parse_chunk_header_exact(l) {
            flush(&mut raw, old_ln, &mut deltas);
            old_ln = if a == 0 { 1 } else { a as i64 };
            let _ = c;
        } else if let Some(first) = l.chars().next() {
            let rest: String = l.chars().skip(1).collect();
            if first == ' ' || first == '+' || first == '-' {
                raw.push((first, rest));
            }
        } else {
            raw.push((' ', String::new()));
        }
    }
    flush(&mut raw, old_ln, &mut deltas);
    deltas
}

/// Like [`parse_chunk_header`] (the header must be the entire string).
fn parse_chunk_header_exact(s: &str) -> Option<(usize, usize, usize, usize)> {
    parse_chunk_header(s)
}

/// `ArrayList.get`'s bounds check message.
fn ioobe(idx: i64, len: usize) -> String {
    format!("Index {idx} out of bounds for length {len}")
}

#[derive(PartialEq)]
enum Verify {
    Ok,
    OutOfTarget,
    Mismatch,
}

/// `Chunk.verifyChunk(target, fuzz, position)`, including its out-of-bounds exception when the chunk
/// ends exactly one line past the end of the target.
fn verify_chunk(d: &Delta, target: &[String], fuzz: i64, position: i64) -> Result<Verify, String> {
    let size = d.source.len() as i64;
    let last_index = size - fuzz;
    let last = position + size - 1;
    let tlen = target.len() as i64;
    if position + fuzz > tlen || last - fuzz > tlen {
        return Ok(Verify::OutOfTarget);
    }
    let mut i = fuzz;
    while i < last_index {
        let idx = position + i;
        if idx < 0 || idx >= tlen {
            return Err(ioobe(idx, target.len()));
        }
        // `lines.get(i)` cannot fail for `0 <= i < size`; a negative fuzz never happens.
        if target[idx as usize] != d.source[i as usize] {
            return Ok(Verify::Mismatch);
        }
        i += 1;
    }
    Ok(Verify::Ok)
}

/// `ChangeDelta.applyFuzzyToAt`: removes the source lines (minus `fuzz` at either end) and inserts
/// the target lines (minus `fuzz` at either end) at `position + fuzz`.
fn apply_fuzzy_to_at(d: &Delta, target: &mut Vec<String>, fuzz: i64, position: i64) -> Result<(), String> {
    let size = d.source.len() as i64;
    let mut i = fuzz;
    while i < size - fuzz {
        let idx = position + fuzz;
        if idx < 0 || idx >= target.len() as i64 {
            return Err(ioobe(idx, target.len()));
        }
        target.remove(idx as usize);
        i += 1;
    }
    let tsize = d.target.len() as i64;
    let (from, to) = (fuzz, tsize - fuzz);
    if from < 0 {
        return Err(format!("fromIndex = {from}"));
    }
    if to > tsize {
        return Err(format!("toIndex = {to}"));
    }
    if from > to {
        return Err(format!("fromIndex({from}) > toIndex({to})"));
    }
    let mut i = fuzz;
    for line in &d.target[from as usize..to as usize] {
        let idx = position + i;
        if idx < 0 || idx > target.len() as i64 {
            return Err(format!("Index: {idx}, Size: {}", target.len()));
        }
        target.insert(idx as usize, line.clone());
        i += 1;
    }
    Ok(())
}

/// `Patch.findPositionWithFuzzAndMoreDelta`: a negative result means "not found" (including a match at a
/// negative position, which the caller cannot tell apart).
fn find_position_with_fuzz_and_more_delta(
    d: &Delta,
    content: &[String],
    fuzz: i64,
    more: i64,
    default_position: i64,
    last_patch_end: i64,
    before_out: &mut bool,
    after_out: &mut bool,
) -> Result<i64, String> {
    if !*before_out && default_position - more + fuzz <= last_patch_end {
        *before_out = true;
    }
    if !*after_out && (content.len() as i64) < default_position + more + d.source.len() as i64 - fuzz {
        *after_out = true;
    }
    if !*before_out && verify_chunk(d, content, fuzz, default_position - more)? == Verify::Ok {
        return Ok(default_position - more);
    }
    if !*after_out && verify_chunk(d, content, fuzz, default_position + more)? == Verify::Ok {
        return Ok(default_position + more);
    }
    Ok(-1)
}

/// `Patch.applyFuzzy(content, max_fuzz)` for a patch holding the single delta `d` (which is how
/// `PatchUtil` applies hunks: one at a time, last to first).
fn apply_delta(content: &mut Vec<String>, d: &Delta, max_fuzz: i64) -> Result<(), String> {
    let last_patch_end: i64 = -1;
    let default_position = d.pos;
    let mut found: Option<(i64, i64)> = None;
    'fuzzes: for fuzz in 0..=max_fuzz {
        // findPositionWithFuzz
        if verify_chunk(d, content, fuzz, default_position)? == Verify::Ok {
            found = Some((fuzz, default_position));
            break 'fuzzes;
        }
        let (mut before_out, mut after_out) = (false, false);
        let mut more: i64 = 0;
        loop {
            let pos = find_position_with_fuzz_and_more_delta(
                d, content, fuzz, more, default_position, last_patch_end, &mut before_out, &mut after_out,
            )?;
            if pos >= 0 {
                found = Some((fuzz, pos));
                break 'fuzzes;
            }
            if before_out && after_out {
                break;
            }
            more += 1;
        }
    }
    match found {
        Some((fuzz, pos)) => apply_fuzzy_to_at(d, content, fuzz, pos),
        None => Err("could not apply patch due to CONTENT_DOES_NOT_MATCH_TARGET".into()),
    }
}

fn owner_perm(p: &Path) -> i32 {
    match std::fs::metadata(p) {
        Ok(m) => ((m.permissions().mode() >> 6) & 7) as i32,
        Err(_) => -1,
    }
}

fn apply_to_file(deltas: &[Delta], old: Option<&Path>, new: Option<&Path>, renaming: bool, mut perm: i32) -> R<()> {
    let input = match (old, new) {
        (Some(o), _) if o.exists() => Some(o),
        (_, Some(n)) if n.exists() => Some(n),
        _ => None,
    };
    let old_content = match input {
        None => vec![],
        Some(f) => {
            if perm == -1 {
                perm = owner_perm(f);
            }
            read_lines(f)?
        }
    };
    let mut content = old_content;
    for d in deltas.iter().rev() {
        apply_delta(&mut content, d, 2).map_err(|m| {
            format!(
                "in patch applied to {}: {}, error applying change near line {}",
                old.map(|p| p.display().to_string()).unwrap_or_else(|| "null".into()),
                m,
                d.pos + 1
            )
        })?;
    }
    let output = if old.map(|o| o.exists()).unwrap_or(false) && !renaming { old } else { new };
    if let Some(o) = old {
        let _ = std::fs::remove_file(o);
    }
    let is_delete = new.is_none() && content.is_empty();
    if let (Some(out), false) = (output, is_delete) {
        write_lines(out, &content)?;
        if perm != -1 {
            if let Ok(m) = std::fs::metadata(out) {
                let mut mode = m.permissions().mode();
                // `PatchUtil.setFilePermission`: `Path.setReadable/Writable/Executable` modify only 0400 / 0200 / 0111.
                let p = perm as u32;
                let upd = |m: u32, bits: u32, on: bool| if on { m | bits } else { m & !bits };
                mode = upd(mode, 0o400, p & 4 != 0);
                mode = upd(mode, 0o200, p & 2 != 0);
                mode = upd(mode, 0o111, p & 1 != 0);
                let _ = std::fs::set_permissions(out, std::fs::Permissions::from_mode(mode));
            }
        }
    }
    Ok(())
}

/// Applies `patch_file` under `out_dir` with `strip` leading path components removed.
pub fn apply(patch_file: &Path, strip: i64, out_dir: &Path) -> Result<(), PatchError> {
    apply_inner(patch_file, strip, out_dir).map_err(|m| match m.strip_prefix(IO_MARK) {
        Some(io) => PatchError::Io(io.to_string()),
        None => PatchError::Failed(m),
    })
}

/// Why [`apply`] failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchError {
    /// `PatchFailedException`.
    Failed(String),
    /// `IOException` (message as Java prints it).
    Io(String),
}

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PatchError::Failed(m) | PatchError::Io(m) => f.write_str(m),
        }
    }
}

fn apply_inner(patch_file: &Path, strip: i64, out_dir: &Path) -> R<()> {
    if !patch_file.exists() {
        return Err(format!("Cannot find patch file: {}", patch_file.display()));
    }
    let mut is_git = false;
    let mut has_from = false;
    let mut has_to = false;
    let mut reading = false;
    let mut content: Vec<String> = Vec::new();
    let mut header: Option<ChunkHeader> = None;
    let mut old_str: Option<String> = None;
    let mut new_str: Option<String> = None;
    let mut old_file: Option<PathBuf> = None;
    let mut new_file: Option<PathBuf> = None;
    let mut oc = 0usize;
    let mut nc = 0usize;
    let mut perm: i32 = -1;

    let lines = read_lines(patch_file)?;
    for i in 0..=lines.len() {
        let line: &str = if i < lines.len() { &lines[i] } else { "$" };
        let ty = line_type(line, reading, is_git);
        if ty == Lt::GitHeader || ty == Lt::Unknown || (ty == Lt::OldFile && header.is_some()) {
            let renaming = is_git && has_from && has_to;
            if !content.is_empty() || renaming || perm != -1 {
                let start = i + 1 - content.len().min(i + 1);
                if !content.is_empty() {
                    if content.len() < 2 || !content[0].starts_with("---") || !content[1].starts_with("+++") {
                        return Err(format!("The patch content must start with ---/+++ prelude lines at line {start}."));
                    }
                    match &header {
                        None => return Err(format!("Looks like a unified diff at line {start}, but no patch chunk was found.")),
                        Some(h) => {
                            if h.check(oc, nc) == Chk::Continue {
                                return Err(format!("Expecting more chunk line at line {}", start + content.len()));
                            }
                        }
                    }
                }
                if renaming {
                    let mut oe = String::new();
                    let mut ne = String::new();
                    match &old_file {
                        None => oe = ", old file name is not specified".into(),
                        Some(f) if !f.exists() => oe = format!(", old file name ({}) doesn't exist", old_str.clone().unwrap_or_default()),
                        _ => {}
                    }
                    match &new_file {
                        None => ne = ", new file name is not specified".into(),
                        Some(f) if f.exists() => ne = format!(", new file name ({}) already exists", new_str.clone().unwrap_or_default()),
                        _ => {}
                    }
                    if !oe.is_empty() || !ne.is_empty() {
                        return Err(format!("Cannot rename file (near line {start}){oe}{ne}."));
                    }
                }
                let deltas = parse_unified(&content);
                // checkFilesStatusForPatching
                if old_file.is_none() && new_file.is_none() {
                    return Err(format!("Wrong patch format near line {start}, neither new file or old file are specified."));
                }
                let is_add = deltas.len() == 1 && deltas[0].source.is_empty();
                let oex = old_file.as_ref().map(|f| f.exists()).unwrap_or(false);
                let nex = new_file.as_ref().map(|f| f.exists()).unwrap_or(false);
                if !is_add && !oex && !nex {
                    let oe = match &old_file {
                        None => ", old file name is not specified".to_string(),
                        Some(_) => format!(", old file name ({}) doesn't exist", old_str.clone().unwrap_or_default()),
                    };
                    let ne = match &new_file {
                        None => ", new file name is not specified".to_string(),
                        Some(_) => format!(", new file name ({}) doesn't exist", new_str.clone().unwrap_or_default()),
                    };
                    return Err(format!("Cannot find file to patch (near line {start}){oe}{ne}."));
                }
                apply_to_file(&deltas, old_file.as_deref(), new_file.as_deref(), renaming, perm)?;
            }
            content.clear();
            header = None;
            old_str = None;
            new_str = None;
            old_file = None;
            new_file = None;
            perm = -1;
            oc = 0;
            nc = 0;
            reading = false;
            is_git = ty == Lt::GitHeader;
            if is_git {
                let args: Vec<&str> = line.split(' ').collect();
                if args.len() >= 4 {
                    let o = strip_path(args[2], strip).to_string();
                    if !o.is_empty() {
                        old_file = Some(get_file_path(&o, out_dir, i + 1)?);
                    }
                    old_str = Some(o);
                    let n = strip_path(args[3], strip).to_string();
                    if !n.is_empty() {
                        new_file = Some(get_file_path(&n, out_dir, i + 1)?);
                    }
                    new_str = Some(n);
                }
            }
            has_from = false;
            has_to = false;
        }
        match ty {
            Lt::OldFile => {
                content.push(line.to_string());
                let p = extract_path(line, strip, i + 1)?;
                old_file = match &p {
                    Some(p) => Some(get_file_path(p, out_dir, i + 1)?),
                    None => None,
                };
                old_str = p;
            }
            Lt::NewFile => {
                content.push(line.to_string());
                let p = extract_path(line, strip, i + 1)?;
                new_file = match &p {
                    Some(p) => Some(get_file_path(p, out_dir, i + 1)?),
                    None => None,
                };
                new_str = p;
            }
            Lt::NewMode | Lt::NewFileMode => {
                let index = if ty == Lt::NewMode { 12 } else { 17 };
                if line.len() <= index {
                    return Err(format!("Truncated file mode at line {}: {}", i + 1, line));
                }
                let c = line.as_bytes()[index];
                if !(b'0'..=b'7').contains(&c) {
                    return Err(format!("Wrong file mode format at line {}: {}", i + 1, line));
                }
                perm = (c - b'0') as i32;
            }
            Lt::ChunkHead => {
                let pos = line[2..].find("@@").unwrap() + 2;
                let hs = &line[..pos + 2];
                content.push(hs.to_string());
                let (_, ob, _, nb) = parse_chunk_header(hs).ok_or_else(|| format!("Wrong chunk header: {hs}"))?;
                header = Some(ChunkHeader { old_size: ob, new_size: nb });
                oc = 0;
                nc = 0;
                reading = true;
            }
            Lt::Add | Lt::Del | Lt::Eql => {
                let (what, da, dd) = match ty {
                    Lt::Add => ("an added", 0, 1),
                    Lt::Del => ("a deleted", 1, 0),
                    _ => ("a context", 1, 1),
                };
                let _ = dd;
                if ty == Lt::Add {
                    nc += 1;
                } else if ty == Lt::Del {
                    oc += 1;
                } else {
                    oc += 1;
                    nc += 1;
                }
                let _ = da;
                content.push(line.to_string());
                match header.as_ref().map(|h| h.check(oc, nc)) {
                    Some(Chk::Complete) => reading = false,
                    Some(Chk::Error) => {
                        return Err(format!(
                            "Wrong chunk detected near line {}: {}, does not expect {} line here.",
                            i + 1,
                            line,
                            what
                        ))
                    }
                    _ => {}
                }
            }
            Lt::NoNewline => {
                if !content.is_empty() && header.is_some() {
                    content.push(line.to_string());
                }
            }
            Lt::RenameFrom => {
                has_from = true;
                if old_str.is_none() {
                    let s = line[12..].trim().to_string();
                    if s.is_empty() {
                        return Err(format!("Cannot determine file name from line {}:\n{}", i + 1, line));
                    }
                    old_file = Some(get_file_path(&s, out_dir, i + 1)?);
                    old_str = Some(s);
                }
            }
            Lt::RenameTo => {
                has_to = true;
                if new_str.is_none() {
                    let s = line[10..].trim().to_string();
                    if s.is_empty() {
                        return Err(format!("Cannot determine file name from line {}:\n{}", i + 1, line));
                    }
                    new_file = Some(get_file_path(&s, out_dir, i + 1)?);
                    new_str = Some(s);
                }
            }
            Lt::OtherGit | Lt::GitHeader | Lt::Unknown => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zel-patch-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn modify_with_offset_and_no_orig() {
        let d = tmp("mod");
        std::fs::write(d.join("a.txt"), "x\ny\n1\n2\n3\n4\n5\n").unwrap();
        std::fs::write(
            d.join("p.diff"),
            "--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,3 @@\n 1\n-2\n+two\n 3\n",
        )
        .unwrap();
        apply(&d.join("p.diff"), 1, &d).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "x\ny\n1\ntwo\n3\n4\n5\n");
        assert!(!d.join("a.txt.orig").exists());
    }

    #[test]
    fn create_delete_rename() {
        let d = tmp("cdr");
        std::fs::write(d.join("old.txt"), "a\nb\n").unwrap();
        std::fs::write(d.join("gone.txt"), "z\n").unwrap();
        std::fs::write(
            d.join("p.diff"),
            "diff --git a/old.txt b/new.txt\nrename from old.txt\nrename to new.txt\n\
             diff --git a/gone.txt b/gone.txt\n--- a/gone.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-z\n\
             --- /dev/null\n+++ b/made.txt\n@@ -0,0 +1,2 @@\n+m\n+n\n",
        )
        .unwrap();
        apply(&d.join("p.diff"), 1, &d).unwrap();
        assert!(d.join("new.txt").exists() && !d.join("old.txt").exists());
        assert!(!d.join("gone.txt").exists());
        assert_eq!(std::fs::read_to_string(d.join("made.txt")).unwrap(), "m\nn\n");
    }

    #[test]
    fn mismatch_and_escape_errors() {
        let d = tmp("err");
        std::fs::write(d.join("a.txt"), "q\n").unwrap();
        std::fs::write(d.join("p.diff"), "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-nope\n+x\n").unwrap();
        let e = apply(&d.join("p.diff"), 1, &d).unwrap_err().to_string();
        assert!(e.contains("error applying change near line 1"), "{e}");
        std::fs::write(d.join("p2.diff"), "--- a/../../x\n+++ b/../../x\n@@ -0,0 +1 @@\n+x\n").unwrap();
        let e = apply(&d.join("p2.diff"), 1, &d).unwrap_err().to_string();
        assert!(e.contains("outside of external repository"), "{e}");
    }

    #[test]
    fn leading_equals_line_in_registry_patch() {
        // BCR patches (e.g. rules_shell's module_dot_bazel_version.patch) start with a line of `=`.
        let d = tmp("eq");
        std::fs::write(d.join("MODULE.bazel"), "module(\n    name = \"m\",\n    version = \"0.0.0\",\n)\n\nbazel_dep(name = \"a\", version = \"1\")\n").unwrap();
        std::fs::write(
            d.join("p.diff"),
            "===================================================================\n--- a/MODULE.bazel\n+++ b/MODULE.bazel\n@@ -1,4 +1,4 @@\n module(\n     name = \"m\",\n-    version = \"0.0.0\",\n+    version = \"0.6.1\",\n )\n",
        )
        .unwrap();
        apply(&d.join("p.diff"), 1, &d).unwrap();
        assert!(std::fs::read_to_string(d.join("MODULE.bazel")).unwrap().contains("0.6.1"));
    }

}
