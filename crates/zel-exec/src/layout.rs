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

//! Output base layout and the execroot `SymlinkForest` (`buildtool/SymlinkForest.java`).
//!
//! ```text
//! <output_base>/
//!   action_cache/          CompactPersistentActionCache files
//!   bazel-workers/         persistent worker logs & execroots
//!   command.log            output of the last command (CommandLogModule)
//!   execroot/<workspace>/  the execroot: symlinks to every top-level workspace entry,
//!                          bazel-out/ (real directory), external/<repo> -> ../../external/<repo>
//!   external/              fetched external repositories
//!   install -> <install_base>
//!   java.log -> java.log.<host>.<user>.log.java.<date>.<pid>   (server log analog)
//!   sandbox/               sandbox directories (removed after a build unless --sandbox_debug)
//!   server/                server.pid.txt, request_cookie, response_cookie, server.info ...
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

/// `SymlinkForest.symlinkShouldBePlanted`: links not starting with `<product>-` (the output
/// convenience symlinks), not `external`, and not ignored via `.bazelignore`.
pub fn symlink_should_be_planted(prefix: &str, ignored: &[String], base_name: &str) -> bool {
    !base_name.starts_with(prefix) && base_name != "external" && !ignored.iter().any(|i| i == base_name)
}

/// Top-level entries of `.bazelignore` (`IgnoredSubdirectories`): one path per line, `#` comments,
/// only the first path segment matters for the top-level forest.
pub fn read_bazelignore(workspace: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(workspace.join(".bazelignore")) else { return Vec::new() };
    let mut v = Vec::new();
    for l in text.lines() {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let l = l.trim_start_matches("./").trim_end_matches('/');
        // Only whole top-level directories can be left out of the forest.
        if !l.contains('/') {
            v.push(l.to_string());
        }
    }
    v
}

/// `deleteTreesBelowNotPrefixed`: removes everything under `dir` whose name does not start with
/// `prefix`, without following symlinks.
pub fn delete_trees_below_not_prefixed(dir: &Path, prefix: &str) -> io::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        if e.file_name().to_string_lossy().starts_with(prefix) {
            continue;
        }
        let p = e.path();
        let md = std::fs::symlink_metadata(&p)?;
        if md.is_dir() {
            std::fs::remove_dir_all(&p)?;
        } else {
            std::fs::remove_file(&p)?;
        }
    }
    Ok(())
}

/// `eagerlyPlantSymlinkForestSinglePackagePath`: replaces the execroot's contents by one symlink
/// per top-level entry of the source root. Returns the potentially case-conflicting base names
/// (lowercase) that were *not* planted.
pub fn plant_symlink_forest(
    execroot: &Path,
    source_root: &Path,
    prefix: &str,
    ignored: &[String],
) -> io::Result<BTreeSet<String>> {
    std::fs::create_dir_all(execroot)?;
    delete_trees_below_not_prefixed(execroot, prefix)?;
    let mut by_lower: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for e in std::fs::read_dir(source_root)? {
        let e = e?;
        by_lower.entry(e.file_name().to_string_lossy().to_lowercase()).or_default().push(e.path());
    }
    let mut conflicts = BTreeSet::new();
    for (lower, targets) in by_lower {
        if targets.len() == 1 {
            let target = &targets[0];
            let base = target.file_name().unwrap().to_string_lossy().into_owned();
            if symlink_should_be_planted(prefix, ignored, &base) {
                #[cfg(unix)]
                std::os::unix::fs::symlink(target, execroot.join(&base))?;
            }
        } else {
            conflicts.insert(lower);
        }
    }
    Ok(conflicts)
}

/// `plantSingleSymlinkForExternalRepo`: `execroot/external/<repo>` → `<output_base>/external/<repo>`
/// for every fetched repository directory.
pub fn plant_external_repo_links(execroot: &Path, output_base: &Path) -> io::Result<Vec<PathBuf>> {
    let ext = output_base.join("external");
    let mut planted = Vec::new();
    let Ok(rd) = std::fs::read_dir(&ext) else { return Ok(planted) };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('@') && !n.starts_with('.'))
        .collect();
    names.sort();
    if names.is_empty() {
        return Ok(planted);
    }
    std::fs::create_dir_all(execroot.join("external"))?;
    for n in names {
        let link = execroot.join("external").join(&n);
        if std::fs::symlink_metadata(&link).is_ok() {
            continue;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(ext.join(&n), &link)?;
        planted.push(link);
    }
    Ok(planted)
}

static BZLMOD_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Records `--enable_bzlmod` of the running command (affects the execroot directory name).
pub fn set_bzlmod_enabled(v: bool) {
    BZLMOD_ENABLED.store(v, std::sync::atomic::Ordering::SeqCst);
}

pub fn bzlmod_enabled() -> bool {
    BZLMOD_ENABLED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Unix time at which this server process first asked (used in the server log file name).
pub fn server_start_secs() -> u64 {
    static START: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *START.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    })
}

/// Name of the main repository's execroot directory. With Bzlmod (the default) this is `_main`;
/// without it, the name from `workspace(name = "...")` in `WORKSPACE`/`WORKSPACE.bazel`, or
/// `__main__` (`WorkspaceFileValue` default).
pub fn execroot_workspace_name(workspace_dir: &Path, bzlmod_enabled: bool) -> String {
    if bzlmod_enabled {
        return "_main".to_string();
    }
    for f in ["WORKSPACE.bazel", "WORKSPACE"] {
        if let Ok(text) = std::fs::read_to_string(workspace_dir.join(f)) {
            if let Some(n) = parse_workspace_name(&text) {
                return n;
            }
            break;
        }
    }
    "__main__".to_string()
}

/// Finds `workspace(name = "x")` (outside comments) in a WORKSPACE file.
pub fn parse_workspace_name(text: &str) -> Option<String> {
    let cleaned: String = text
        .lines()
        .map(|l| match l.find('#') {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut idx = 0;
    while let Some(p) = cleaned[idx..].find("workspace(") {
        let start = idx + p;
        // Must not be a suffix of another identifier (e.g. `local_workspace(`).
        let before_ok = start == 0 || !cleaned[..start].chars().last().map_or(false, |c| c.is_alphanumeric() || c == '_');
        if before_ok {
            let rest = &cleaned[start + "workspace(".len()..];
            let end = rest.find(')')?;
            let args = &rest[..end];
            for part in args.split(',') {
                let part = part.trim();
                if let Some(v) = part.strip_prefix("name") {
                    let v = v.trim_start().strip_prefix('=')?.trim();
                    let v = v.trim_matches(|c| c == '"' || c == '\'');
                    return Some(v.to_string());
                }
            }
            return None;
        }
        idx = start + 1;
    }
    None
}

/// Creates the standard output base directories and the `install` symlink.
pub fn ensure_output_base_layout(output_base: &Path, install_base: Option<&Path>) -> io::Result<()> {
    for d in ["execroot", "external", "server", "action_cache"] {
        std::fs::create_dir_all(output_base.join(d))?;
    }
    if let Some(ib) = install_base {
        // The install base exists in real Bazel (the client extracts the embedded binaries and
        // tools there); keep the link from dangling so tools can be installed into it.
        let _ = std::fs::create_dir_all(ib);
        let link = output_base.join("install");
        let cur = std::fs::read_link(&link).ok();
        if cur.as_deref() != Some(ib) {
            let _ = std::fs::remove_file(&link);
            #[cfg(unix)]
            std::os::unix::fs::symlink(ib, &link)?;
        }
    }
    Ok(())
}

extern "C" {
    fn gethostname(name: *mut u8, len: usize) -> i32;
    #[cfg(unix)]
    fn localtime_r(timep: *const i64, result: *mut Tm) -> *mut Tm;
}

#[cfg(unix)]
#[repr(C)]
struct Tm {
    tm_sec: i32,
    tm_min: i32,
    tm_hour: i32,
    tm_mday: i32,
    tm_mon: i32,
    tm_year: i32,
    tm_wday: i32,
    tm_yday: i32,
    tm_isdst: i32,
    tm_gmtoff: i64,
    tm_zone: *const u8,
}

pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: valid buffer of the stated length.
    let rc = unsafe { gethostname(buf.as_mut_ptr(), buf.len()) };
    if rc != 0 {
        return "localhost".into();
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    let raw = String::from_utf8_lossy(&buf[..end]);
    raw.split('.').next().unwrap_or("localhost").to_ascii_lowercase()
}

pub fn local_civil_from_unix(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    #[cfg(unix)]
    unsafe {
        let t = secs as i64;
        let mut tm: Tm = std::mem::zeroed();
        if !localtime_r(&t, &mut tm).is_null() {
            return (
                (tm.tm_year + 1900) as i64,
                (tm.tm_mon + 1) as u32,
                tm.tm_mday as u32,
                tm.tm_hour as u32,
                tm.tm_min as u32,
                tm.tm_sec as u32,
            );
        }
    }
    civil_from_unix(secs)
}

/// `CommandLogModule`: `<output_base>/command.log` holds the output of the last command.
pub fn write_command_log(output_base: &Path, stdout: &[u8], stderr: &[u8]) -> io::Result<()> {
    let mut data = Vec::with_capacity(stdout.len() + stderr.len());
    data.extend_from_slice(stderr);
    data.extend_from_slice(stdout);
    std::fs::write(output_base.join("command.log"), data)
}

/// The server log analog of `java.log`: a per-server log file
/// `java.log.<host>.<user>.log.java.<date>.<pid>` with `java.log` as a symlink to it. Returns the
/// file path; `line` is appended (one `Ymmdd ...` style line per event).
pub fn append_server_log(output_base: &Path, pid: u32, started_secs: u64, line: &str) -> io::Result<PathBuf> {
    use std::io::Write;
    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
    let (y, mo, d, h, mi, s) = local_civil_from_unix(started_secs);
    let name = format!("java.log.{}.{user}.log.java.{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}.{pid}", hostname());
    let file = output_base.join(&name);
    let link = output_base.join("java.log");
    if std::fs::read_link(&link).ok().as_deref() != Some(Path::new(&name)) {
        let _ = std::fs::remove_file(&link);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&name, &link)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&file)?;
    writeln!(f, "{line}")?;
    Ok(file)
}

/// UTC civil time from a unix timestamp (Howard Hinnant's algorithm).
pub fn civil_from_unix(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, (rem / 3600) as u32, ((rem % 3600) / 60) as u32, (rem % 60) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zel-layout-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::canonicalize(&d).unwrap()
    }

    #[test]
    fn forest_links_top_level_entries_and_keeps_output_dirs() {
        let root = tmp("forest");
        let ws = root.join("ws");
        let er = root.join("ob/execroot/_main");
        std::fs::create_dir_all(ws.join("pkg")).unwrap();
        std::fs::write(ws.join("pkg/a.txt"), "a").unwrap();
        std::fs::write(ws.join("MODULE.bazel"), "").unwrap();
        std::fs::create_dir_all(ws.join("external")).unwrap();
        std::fs::create_dir_all(ws.join("skipme")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/nonexistent", ws.join("bazel-out")).unwrap();
        // Pre-existing execroot content: stale link removed, bazel-out kept.
        std::fs::create_dir_all(er.join("bazel-out/k8-fastbuild/bin")).unwrap();
        std::fs::write(er.join("bazel-out/k8-fastbuild/bin/keep"), "k").unwrap();
        std::fs::create_dir_all(er.join("stale_dir")).unwrap();
        let c = plant_symlink_forest(&er, &ws, "bazel-", &["skipme".to_string()]).unwrap();
        assert!(c.is_empty());
        assert_eq!(std::fs::read_to_string(er.join("pkg/a.txt")).unwrap(), "a");
        assert!(std::fs::symlink_metadata(er.join("pkg")).unwrap().file_type().is_symlink());
        assert!(er.join("MODULE.bazel").exists());
        assert!(!er.join("external").exists(), "external/ is never linked");
        assert!(!er.join("skipme").exists(), ".bazelignore'd dir is not linked");
        assert!(!er.join("stale_dir").exists(), "stale entries are deleted");
        assert!(er.join("bazel-out/k8-fastbuild/bin/keep").exists(), "bazel-* kept");
        assert!(std::fs::symlink_metadata(er.join("bazel-out")).unwrap().is_dir(), "bazel-out stays a real dir");
    }

    #[test]
    fn bazelignore_and_external_links() {
        let root = tmp("ext");
        std::fs::write(root.join(".bazelignore"), "# c\nnode_modules\n./vendor/\nnested/dir\n\n").unwrap();
        assert_eq!(read_bazelignore(&root), ["node_modules", "vendor"]);
        let ob = root.join("ob");
        std::fs::create_dir_all(ob.join("external/rules_x+")).unwrap();
        std::fs::create_dir_all(ob.join("external/@bazel_tools.marker")).unwrap();
        std::fs::write(ob.join("external/rules_x+.marker"), "").unwrap();
        let er = ob.join("execroot/_main");
        std::fs::create_dir_all(&er).unwrap();
        let planted = plant_external_repo_links(&er, &ob).unwrap();
        assert_eq!(planted, [er.join("external/rules_x+")]);
        assert!(er.join("external/rules_x+").exists());
        assert!(plant_external_repo_links(&er, &ob).unwrap().is_empty(), "idempotent");
    }

    #[test]
    fn workspace_name_resolution() {
        let d = tmp("wsn");
        assert_eq!(execroot_workspace_name(&d, true), "_main");
        assert_eq!(execroot_workspace_name(&d, false), "__main__");
        std::fs::write(d.join("WORKSPACE"), "# workspace(name = \"commented\")\nload(\"x\", \"y\")\nworkspace(\n    name = \"my_ws\",\n)\n").unwrap();
        assert_eq!(execroot_workspace_name(&d, false), "my_ws");
        std::fs::write(d.join("WORKSPACE.bazel"), "local_workspace(name = \"no\")\nworkspace(name='single')").unwrap();
        assert_eq!(execroot_workspace_name(&d, false), "single");
        assert_eq!(execroot_workspace_name(&d, true), "_main");
        assert_eq!(parse_workspace_name("workspace()"), None);
    }

    #[test]
    fn output_base_layout_command_log_and_server_log() {
        let root = tmp("ob");
        let ob = root.join("ob");
        let ib = root.join("install_base");
        std::fs::create_dir_all(&ib).unwrap();
        ensure_output_base_layout(&ob, Some(&ib)).unwrap();
        for d in ["execroot", "external", "server", "action_cache"] {
            assert!(ob.join(d).is_dir(), "{d}");
        }
        assert_eq!(std::fs::read_link(ob.join("install")).unwrap(), ib);
        ensure_output_base_layout(&ob, Some(&ib)).unwrap();
        write_command_log(&ob, b"out\n", b"err\n").unwrap();
        assert_eq!(std::fs::read_to_string(ob.join("command.log")).unwrap(), "err\nout\n");
        let f = append_server_log(&ob, 4242, 1_700_000_000, "I command: build").unwrap();
        let f2 = append_server_log(&ob, 4242, 1_700_000_000, "I command: test").unwrap();
        assert_eq!(f, f2);
        assert!(f.file_name().unwrap().to_string_lossy().starts_with("java.log."));
        let s = f.file_name().unwrap().to_string_lossy();
        assert!(s.contains(".log.java.20231114-") && s.ends_with(".4242"));
        assert_eq!(std::fs::read_to_string(ob.join("java.log")).unwrap(), "I command: build\nI command: test\n");
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
    }
}
