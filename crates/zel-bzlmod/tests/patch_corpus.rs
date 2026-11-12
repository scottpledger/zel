//! Differential test of the native `ctx.patch` implementation against recorded answers of Bazel's real
//! `PatchUtil` (java-diff-utils 4.12). The corpus lives in `tests/fixtures/patch_corpus/<case>/` and is
//! generated and recorded by `scripts/bzlmod_harness/patch_corpus.py` (see its docstring).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    if !from.exists() {
        return;
    }
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let ft = std::fs::symlink_metadata(&src).unwrap().file_type();
        if ft.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&src).unwrap(), &dst).unwrap();
        } else if ft.is_dir() {
            copy_tree(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

fn snapshot(root: &Path) -> String {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(root).unwrap().display().to_string();
            let md = std::fs::symlink_metadata(&p).unwrap();
            if md.file_type().is_symlink() {
                out.push(format!("{rel} L {}", std::fs::read_link(&p).unwrap().display()));
            } else if md.is_dir() {
                out.push(format!("{rel} D"));
                walk(root, &p, out);
            } else {
                let mode = md.permissions().mode() & 0o700;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
                let bytes = std::fs::read(&p).unwrap();
                let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                out.push(format!("{rel} F {mode:#o} {hex}"));
            }
        }
    }
    let mut out = vec![];
    walk(root, root, &mut out);
    out.sort();
    let mut s = out.join("\n");
    s.push('\n');
    s
}

#[test]
fn native_patch_matches_bazel_patchutil() {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/patch_corpus");
    let mut names: Vec<PathBuf> = std::fs::read_dir(&corpus).unwrap().map(|e| e.unwrap().path()).collect();
    names.sort();
    assert!(names.len() >= 100, "corpus too small: {}", names.len());
    let work = std::env::temp_dir().join(format!("zel-patch-corpus-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    let mut failures = vec![];
    for dir in &names {
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        let out = work.join(&name);
        copy_tree(&dir.join("before"), &out);
        // Git does not preserve non-0644/0755 modes in working trees after clone; restore the
        // exact modes `patch_corpus.py` sets before running the patch.
        match name.as_str() {
            "perm_preserve_ro" | "perm_ro_file_modify_mode_change" => {
                let _ = std::fs::set_permissions(out.join("f.txt"), std::fs::Permissions::from_mode(0o444));
            }
            "perm_preserve_600" => {
                let _ = std::fs::set_permissions(out.join("f.txt"), std::fs::Permissions::from_mode(0o600));
            }
            "perm_preserve_group_exec" => {
                let _ = std::fs::set_permissions(out.join("f.txt"), std::fs::Permissions::from_mode(0o654));
            }
            _ => {}
        }
        let out = std::fs::canonicalize(&out).unwrap();
        let strip: i64 = std::fs::read_to_string(dir.join("strip")).unwrap().trim().parse().unwrap();
        let result = zel_bzlmod::patch::apply(&dir.join("patch"), strip, &out);
        let out_str = out.display().to_string();
        let got = match &result {
            Ok(()) => "OK".to_string(),
            Err(zel_bzlmod::patch::PatchError::Failed(e)) => {
                format!("ERR PatchFailedException: {}", e.replace(&out_str, "$OUT").replace('\n', "\\n"))
            }
            Err(zel_bzlmod::patch::PatchError::Io(e)) => format!("ERR IOException: {}", e.replace(&out_str, "$OUT").replace('\n', "\\n")),
        };
        let want = std::fs::read_to_string(dir.join("result")).unwrap();
        let want = want.trim_end_matches('\n');
        // Java reports I/O failures with many exception classes (FileNotFoundException, ...); all are `IOException` here.
        let want = match want.strip_prefix("ERR ").and_then(|r| r.split_once(": ")) {
            Some((class, msg)) if class.ends_with("IOException") || class.ends_with("FileNotFoundException") || class.ends_with("FileSystemException") || class.ends_with("NoSuchFileException") || class.ends_with("AccessDeniedException") || class.ends_with("NotDirectoryException") || class.ends_with("FileAlreadyExistsException") => {
                format!("ERR IOException: {msg}")
            }
            _ => want.to_string(),
        };
        let want = want.replace("/private/tmp", "/tmp").replace("/private/etc", "/etc");
        let want = want.as_str();
        let got = got.replace("/private/tmp", "/tmp").replace("/private/etc", "/etc");
        let got = got.as_str();
        let mut problems = vec![];
        // Uncaught Java runtime exceptions (a crash of the Bazel server): any failure is acceptable.
        let java_crash = want.strip_prefix("ERR ").is_some_and(|r| {
            r.starts_with("crash") || r.split_once(": ").is_some_and(|(c, _)| (!c.ends_with("Exception") && !c.ends_with("FileSystemException")) || c.starts_with("IllegalArgument") || c.starts_with("NumberFormat") || c.starts_with("IndexOutOfBounds") || c.starts_with("NullPointer"))
        });
        if got != want && !(java_crash && result.is_err()) {
            problems.push(format!("result: want `{want}` got `{got}`"));
        }
        if !java_crash {
            let want_tree = std::fs::read_to_string(dir.join("expected")).unwrap();
            let got_tree = snapshot(&out);
            if got_tree != want_tree {
                problems.push(format!("tree differs:\n--- bazel\n{want_tree}--- zel\n{got_tree}"));
            }
        }
        if !problems.is_empty() {
            failures.push(format!("{name}: {}", problems.join("\n")));
        }
    }
    let _ = std::fs::remove_dir_all(&work);
    assert!(failures.is_empty(), "{} of {} corpus cases differ from Bazel:\n{}", failures.len(), names.len(), failures.join("\n\n"));
}
