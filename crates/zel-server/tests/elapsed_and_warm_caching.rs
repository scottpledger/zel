use std::path::PathBuf;
use std::sync::Arc;
use zel_proto::command_server::RunRequest;
use zel_server::{BlazeDirectories, BlazeRuntime};
use zel_vfs::{FileSystem, LocalFileSystem, PathFragment};

struct Ws {
    ws: PathBuf,
    rt: BlazeRuntime,
    root: PathBuf,
}

impl Ws {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "zel-srv-elapsed-cache-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let ws_dir = root.join("ws");
        std::fs::create_dir_all(&ws_dir).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let ws_dir = root.join("ws");
        std::fs::write(ws_dir.join("MODULE.bazel"), "module(name = \"w\")\n").unwrap();
        std::fs::write(ws_dir.join("BUILD"), "").unwrap();
        let fs: Arc<dyn FileSystem> = Arc::new(LocalFileSystem::default());
        let p = |s: &str| PathFragment::create(root.join(s).to_str().unwrap());
        let dirs = BlazeDirectories {
            workspace_dir: PathFragment::create(ws_dir.to_str().unwrap()),
            output_base: p("ob"),
            install_base: p("ib"),
            output_user_root: p("ur"),
        };
        Self {
            ws: ws_dir,
            rt: BlazeRuntime::new(dirs, fs, 1),
            root,
        }
    }

    fn write(&self, rel: &str, content: &str) {
        let p = self.ws.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, content).unwrap();
    }

    fn run(&self, argv: &[&str]) -> (i32, String, String) {
        let arg: Vec<Vec<u8>> = argv.iter().map(|a| a.as_bytes().to_vec()).collect();
        let req = RunRequest {
            cookie: "c".into(),
            arg,
            client_description: "t".into(),
            ..Default::default()
        };
        let r = self.rt.dispatch_command(&req, "r");
        (
            r.exit_code,
            String::from_utf8_lossy(&r.standard_output).into_owned(),
            String::from_utf8_lossy(&r.standard_error).into_owned(),
        )
    }

    fn bin_dir(&self) -> PathBuf {
        self.root.join(format!(
            "ob/execroot/_main/bazel-out/{}-fastbuild/bin",
            zel_analysis::build_options::auto_host_cpu()
        ))
    }
}

impl Drop for Ws {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn parse_elapsed_secs(stderr: &str) -> Option<f64> {
    for line in stderr.lines() {
        if let Some(rest) = line.strip_prefix("INFO: Elapsed time: ") {
            let num = rest.split('s').next()?;
            return num.trim().parse::<f64>().ok();
        }
    }
    None
}

#[test]
fn elapsed_time_includes_bzlmod_and_loading_not_just_execution() {
    let w = Ws::new("elapsed-full");
    w.write(
        "MODULE.bazel",
        r#"
module(name = "w")
ext = use_extension("//:ext.bzl", "ext")
use_repo(ext, "slow_repo")
"#,
    );
    w.write(
        "ext.bzl",
        r#"
def _repo_impl(rctx):
    rctx.execute(["/bin/sh", "-c", "sleep 0.25"])
    rctx.file("BUILD.bazel", 'exports_files(["data.txt"])\n')
    rctx.file("data.txt", "hello\n")

_slow_repo = repository_rule(
    implementation = _repo_impl,
    local = True,
)

def _ext_impl(mctx):
    _slow_repo(name = "slow_repo")

ext = module_extension(implementation = _ext_impl)
"#,
    );
    w.write(
        "BUILD",
        r#"
genrule(
    name = "fast",
    srcs = ["@slow_repo//:data.txt"],
    outs = ["out.txt"],
    cmd = "cp $< $@",
)
"#,
    );

    let (code, _, err) = w.run(&["build", "--lockfile_mode=off", "//:fast"]);
    assert_eq!(code, 0, "{err}");
    let elapsed =
        parse_elapsed_secs(&err).unwrap_or_else(|| panic!("missing INFO: Elapsed time in:\n{err}"));
    assert!(
        elapsed >= 0.20,
        "expected INFO: Elapsed time to include the >=0.25s repo fetch before execution, got {elapsed:.3}s:\n{err}"
    );

    // Target pattern failure should also report real command elapsed time (not hardcoded 0.050s).
    w.write(
        "ext.bzl",
        r#"
def _repo_impl(rctx):
    rctx.execute(["/bin/sh", "-c", "sleep 0.20"])
    rctx.file("BUILD.bazel", 'exports_files(["data.txt"])\n')
    rctx.file("data.txt", "hello2\n")

_slow_repo = repository_rule(
    implementation = _repo_impl,
    local = True,
)

def _ext_impl(mctx):
    _slow_repo(name = "slow_repo")

ext = module_extension(implementation = _ext_impl)
"#,
    );
    let (code_bad, _, err_bad) = w.run(&["build", "--lockfile_mode=off", "@slow_repo//:missing"]);
    assert_ne!(code_bad, 0);
    let elapsed_bad = parse_elapsed_secs(&err_bad)
        .unwrap_or_else(|| panic!("missing INFO: Elapsed time in:\n{err_bad}"));
    assert!(
        elapsed_bad >= 0.15,
        "expected pattern failure elapsed time to include slow repo fetch (>=0.20s), got {elapsed_bad:.3}s:\n{err_bad}"
    );
}

#[test]
fn warm_build_reuses_bzlmod_and_analysis_and_invalidates_on_changes() {
    let w = Ws::new("warm-cache");
    let fetch_log = w.root.join("fetch_count.log");
    let ext_log = w.root.join("ext_count.log");

    w.write("watched.txt", "v1");
    w.write(
        "MODULE.bazel",
        r#"
module(name = "w")
ext = use_extension("//:ext.bzl", "ext")
use_repo(ext, "local_data")
"#,
    );
    w.write(
        "ext.bzl",
        &format!(
            r#"
def _repo_impl(rctx):
    rctx.execute(["/bin/sh", "-c", "echo fetch >> {fetch}"])
    val = rctx.read(Label("//:watched.txt")).strip()
    rctx.file("BUILD.bazel", 'exports_files(["val.txt"])\n')
    rctx.file("val.txt", val + "\n")

_local_repo = repository_rule(
    implementation = _repo_impl,
    local = True,
)

def _ext_impl(mctx):
    mctx.execute(["/bin/sh", "-c", "echo ext >> {ext}"])
    _local_repo(name = "local_data")

ext = module_extension(implementation = _ext_impl)
"#,
            fetch = fetch_log.display(),
            ext = ext_log.display(),
        ),
    );
    w.write(
        "defs.bzl",
        r#"
def _emit_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".out")
    inp = ctx.file.src
    ctx.actions.run_shell(
        inputs = [inp],
        outputs = [out],
        command = "printf '%s:' '" + ctx.attr.tag + "' > " + out.path + " && cat " + inp.path + " >> " + out.path,
    )
    return [DefaultInfo(files = depset([out]))]

emit = rule(
    implementation = _emit_impl,
    attrs = {
        "src": attr.label(allow_single_file = True),
        "tag": attr.string(),
    },
)
"#,
    );
    w.write(
        "BUILD",
        r#"
load("//:defs.bzl", "emit")
exports_files(["watched.txt"])
emit(name = "app", src = "@local_data//:val.txt", tag = "t1")
"#,
    );

    let line_count = |p: &PathBuf| -> usize {
        std::fs::read_to_string(p)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    };

    // 1. Cold build
    let (code, _, err) = w.run(&["build", "--lockfile_mode=off", "//:app"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(line_count(&ext_log), 1, "extension should run once cold");
    assert_eq!(
        line_count(&fetch_log),
        1,
        "local repo should fetch once cold"
    );
    assert_eq!(
        std::fs::read_to_string(w.bin_dir().join("app.out"))
            .unwrap()
            .trim(),
        "t1:v1"
    );
    let r1 = w.rt.last_resolver.lock().unwrap().clone().unwrap();

    // 2. Warm build immediately after (even inside the 2s racy window)
    let (code, _, err) = w.run(&["build", "--lockfile_mode=off", "//:app"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        line_count(&ext_log),
        1,
        "warm build must reuse cached module extension"
    );
    assert_eq!(
        line_count(&fetch_log),
        1,
        "warm build must not re-fetch local = True repo when inputs are unchanged"
    );
    let r2 = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    assert!(
        Arc::ptr_eq(&r1, &r2),
        "warm build with unchanged inputs must reuse TargetResolver analysis cache"
    );

    // 3. Edit BUILD -> must invalidate analysis cache (even within racy window), keep Bzlmod cache
    w.write(
        "BUILD",
        r#"
load("//:defs.bzl", "emit")
exports_files(["watched.txt"])
emit(name = "app", src = "@local_data//:val.txt", tag = "t2")
"#,
    );
    let (code, _, err) = w.run(&["build", "--lockfile_mode=off", "//:app"]);
    assert_eq!(code, 0, "{err}");
    let r3 = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    assert!(
        !Arc::ptr_eq(&r2, &r3),
        "editing BUILD must invalidate cached TargetResolver"
    );
    assert_eq!(
        line_count(&fetch_log),
        1,
        "editing BUILD must not needlessly re-fetch external repo"
    );
    assert_eq!(
        std::fs::read_to_string(w.bin_dir().join("app.out"))
            .unwrap()
            .trim(),
        "t2:v1"
    );

    // 4. Edit BUILD -> run `query` -> run `build`: intervening query must NOT hide the edit
    w.write(
        "BUILD",
        r#"
load("//:defs.bzl", "emit")
exports_files(["watched.txt"])
emit(name = "app", src = "@local_data//:val.txt", tag = "t3")
"#,
    );
    let (qcode, _, qerr) = w.run(&["query", "--lockfile_mode=off", "//:app"]);
    assert_eq!(qcode, 0, "{qerr}");
    let (code, _, err) = w.run(&["build", "--lockfile_mode=off", "//:app"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        std::fs::read_to_string(w.bin_dir().join("app.out"))
            .unwrap()
            .trim(),
        "t3:v1"
    );

    // 5. Edit watched.txt -> must invalidate the local = True repo AND analysis cache
    w.write("watched.txt", "v2");
    let (code, _, err) = w.run(&["build", "--lockfile_mode=off", "//:app"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        line_count(&fetch_log),
        2,
        "editing a file watched by a local repo rule must re-fetch it"
    );
    assert_eq!(
        std::fs::read_to_string(w.bin_dir().join("app.out"))
            .unwrap()
            .trim(),
        "t3:v2"
    );

    // 6. Edit defs.bzl -> must invalidate analysis cache
    w.write(
        "defs.bzl",
        r#"
def _emit_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".out")
    inp = ctx.file.src
    ctx.actions.run_shell(
        inputs = [inp],
        outputs = [out],
        command = "printf 'bzl2:%s:' '" + ctx.attr.tag + "' > " + out.path + " && cat " + inp.path + " >> " + out.path,
    )
    return [DefaultInfo(files = depset([out]))]

emit = rule(
    implementation = _emit_impl,
    attrs = {
        "src": attr.label(allow_single_file = True),
        "tag": attr.string(),
    },
)
"#,
    );
    let (code, _, err) = w.run(&["build", "--lockfile_mode=off", "//:app"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        std::fs::read_to_string(w.bin_dir().join("app.out"))
            .unwrap()
            .trim(),
        "bzl2:t3:v2"
    );

    // 7. Change build configuration flags -> must invalidate analysis cache
    let r_before_flag = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    let (code, _, err) = w.run(&["build", "--lockfile_mode=off", "--define=FOO=bar", "//:app"]);
    assert_eq!(code, 0, "{err}");
    let r_after_flag = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    assert!(
        !Arc::ptr_eq(&r_before_flag, &r_after_flag),
        "changing --define must invalidate cached TargetResolver"
    );

    // 8. Edit MODULE.bazel -> must reopen Bzlmod session and invalidate analysis cache
    w.write(
        "MODULE.bazel",
        r#"
module(name = "w", version = "0.0.1")
ext = use_extension("//:ext.bzl", "ext")
use_repo(ext, "local_data")
"#,
    );
    let (code, _, err) = w.run(&["build", "--lockfile_mode=off", "--define=FOO=bar", "//:app"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        line_count(&ext_log),
        2,
        "editing MODULE.bazel must invalidate cached Bzlmod session and re-run extension"
    );
    assert_eq!(
        line_count(&fetch_log),
        3,
        "reopened Bzlmod session must re-fetch local = True repo"
    );

    // 9. --nokeep_state_after_build -> clears both Bzlmod and analysis caches for the next build
    let (code, _, err) = w.run(&[
        "build",
        "--lockfile_mode=off",
        "--define=FOO=bar",
        "--nokeep_state_after_build",
        "//:app",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(w.rt.last_resolver.lock().unwrap().is_none());
    let (code, _, err) = w.run(&["build", "--lockfile_mode=off", "--define=FOO=bar", "//:app"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        line_count(&ext_log),
        3,
        "build after --nokeep_state_after_build must reopen Bzlmod session"
    );
    assert_eq!(
        line_count(&fetch_log),
        4,
        "build after --nokeep_state_after_build must re-fetch local = True repo"
    );

    // 10. clean -> clears both Bzlmod and analysis caches
    let (ccode, _, cerr) = w.run(&["clean"]);
    assert_eq!(ccode, 0, "{cerr}");
    let (code, _, err) = w.run(&["build", "--lockfile_mode=off", "--define=FOO=bar", "//:app"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        line_count(&ext_log),
        4,
        "build after clean must reopen Bzlmod session"
    );
    assert_eq!(
        line_count(&fetch_log),
        5,
        "build after clean must re-fetch local = True repo"
    );
}

#[test]
fn warm_reused_resolver_scopes_action_conflicts_to_requested_roots() {
    let w = Ws::new("scoped-conflicts");
    w.write(
        "defs.bzl",
        r#"
def _dup_impl(ctx):
    out = ctx.actions.declare_file("shared.txt")
    ctx.actions.write(output = out, content = ctx.attr.text + "\n")
    return [DefaultInfo(files = depset([out]))]

dup = rule(
    implementation = _dup_impl,
    attrs = {"text": attr.string()},
)
"#,
    );
    w.write(
        "BUILD",
        r#"
load("//:defs.bzl", "dup")
dup(name = "a", text = "from_a")
dup(name = "b", text = "from_b")
"#,
    );

    let (code1, _, err1) = w.run(&["build", "//:a"]);
    assert_eq!(code1, 0, "{err1}");
    let r1 = w.rt.last_resolver.lock().unwrap().clone().unwrap();

    // Building //:b separately while reusing TargetResolver must NOT conflict with //:a.
    let (code2, _, err2) = w.run(&["build", "//:b"]);
    assert_eq!(code2, 0, "{err2}");
    let r2 = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    assert!(Arc::ptr_eq(&r1, &r2));

    // Building BOTH //:a and //:b together MUST still report the action conflict.
    let (code3, _, err3) = w.run(&["build", "//:a", "//:b"]);
    assert_ne!(code3, 0);
    assert!(
        err3.contains("is generated by these conflicting actions"),
        "{err3}"
    );
}
