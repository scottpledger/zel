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

#[test]
fn analysis_config_flags_invalidate_reused_target_resolver() {
    let w = Ws::new("acfg-invalidation");
    w.write(
        "tc.bzl",
        r#"
MyTcInfo = provider(fields = ["tag"])

def _tc_impl(ctx):
    return [platform_common.ToolchainInfo(info = MyTcInfo(tag = ctx.attr.tag))]

my_tc = rule(
    implementation = _tc_impl,
    attrs = {"tag": attr.string(mandatory = True)},
)

def _consumer_impl(ctx):
    tc = ctx.toolchains["//:tc_type"]
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(output = out, content = tc.info.tag + "\n")
    return [DefaultInfo(files = depset([out]))]

consumer = rule(
    implementation = _consumer_impl,
    toolchains = ["//:tc_type"],
)
"#,
    );
    w.write(
        "BUILD",
        r#"
load("//:tc.bzl", "my_tc", "consumer")

toolchain_type(name = "tc_type")

my_tc(name = "impl_a", tag = "from_a")
my_tc(name = "impl_b", tag = "from_b")

toolchain(
    name = "tc_a",
    toolchain_type = ":tc_type",
    toolchain = ":impl_a",
)

toolchain(
    name = "tc_b",
    toolchain_type = ":tc_type",
    toolchain = ":impl_b",
)

platform(name = "exec_a")
platform(name = "exec_b")

consumer(name = "out")
"#,
    );

    let out_file = || {
        let bin =
            w.rt.dirs
                .bazel_bin(zel_analysis::build_options::auto_host_cpu(), "fastbuild");
        std::fs::read_to_string(PathBuf::from(bin.path_string()).join("out.txt")).unwrap()
    };

    // 1. Cold build with --extra_toolchains=//:tc_a
    let (code, _, err) = w.run(&["build", "--extra_toolchains=//:tc_a", "//:out"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out_file().trim(), "from_a");
    let r1 = w.rt.last_resolver.lock().unwrap().clone().unwrap();

    // 2. Identical warm build reuses TargetResolver.
    let (code, _, err) = w.run(&["build", "--extra_toolchains=//:tc_a", "//:out"]);
    assert_eq!(code, 0, "{err}");
    let r2 = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    assert!(
        Arc::ptr_eq(&r1, &r2),
        "identical --extra_toolchains must reuse resolver"
    );

    // 3. Changing --extra_toolchains MUST invalidate TargetResolver and pick up tc_b.
    let (code, _, err) = w.run(&["build", "--extra_toolchains=//:tc_b", "//:out"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out_file().trim(), "from_b");
    let r3 = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    assert!(
        !Arc::ptr_eq(&r2, &r3),
        "changing --extra_toolchains must invalidate resolver"
    );

    // 4. Changing --extra_execution_platforms MUST invalidate TargetResolver.
    let (code, _, err) = w.run(&[
        "build",
        "--extra_toolchains=//:tc_b",
        "--extra_execution_platforms=//:exec_a",
        "//:out",
    ]);
    assert_eq!(code, 0, "{err}");
    let r4 = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    assert!(
        !Arc::ptr_eq(&r3, &r4),
        "changing --extra_execution_platforms must invalidate resolver"
    );

    // 5. Identical warm build with --extra_execution_platforms reuses TargetResolver again.
    let (code, _, err) = w.run(&[
        "build",
        "--extra_toolchains=//:tc_b",
        "--extra_execution_platforms=//:exec_a",
        "//:out",
    ]);
    assert_eq!(code, 0, "{err}");
    let r5 = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    assert!(
        Arc::ptr_eq(&r4, &r5),
        "identical warm build with --extra_execution_platforms must reuse resolver"
    );

    // 6. Switching --extra_execution_platforms to a different platform MUST invalidate.
    let (code, _, err) = w.run(&[
        "build",
        "--extra_toolchains=//:tc_b",
        "--extra_execution_platforms=//:exec_b",
        "//:out",
    ]);
    assert_eq!(code, 0, "{err}");
    let r6 = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    assert!(
        !Arc::ptr_eq(&r5, &r6),
        "switching --extra_execution_platforms must invalidate resolver"
    );

    // 7. Toggling --nocheck_visibility MUST invalidate TargetResolver.
    let (code, _, err) = w.run(&[
        "build",
        "--extra_toolchains=//:tc_b",
        "--extra_execution_platforms=//:exec_b",
        "--nocheck_visibility",
        "//:out",
    ]);
    assert_eq!(code, 0, "{err}");
    let r7 = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    assert!(
        !Arc::ptr_eq(&r6, &r7),
        "toggling --nocheck_visibility must invalidate resolver"
    );
}

#[test]
fn bzlmod_option_changes_invalidate_cached_session() {
    let w = Ws::new("bzlmod-opts-invalidation");
    let ext_log = w.root.join("ext.log");
    let ext_log_s = ext_log.to_string_lossy().replace('\\', "/");

    // Two alternative local modules for testing --override_module invalidation.
    let mod_v1 = w.root.join("dep_v1");
    let mod_v2 = w.root.join("dep_v2");
    for (dir, val) in [(&mod_v1, "from_v1"), (&mod_v2, "from_v2")] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("MODULE.bazel"),
            "module(name = \"dep_mod\", version = \"1.0.0\")\n",
        )
        .unwrap();
        std::fs::write(dir.join("val.txt"), format!("{val}\n")).unwrap();
        std::fs::write(dir.join("BUILD"), "exports_files([\"val.txt\"])\n").unwrap();
    }

    w.write(
        "ext.bzl",
        &format!(
            r#"
def _repo_impl(rctx):
    rctx.file("BUILD", 'exports_files(["tag.txt"])\n')
    rctx.file("tag.txt", rctx.attr.tag + "\n")

_repo = repository_rule(
    implementation = _repo_impl,
    attrs = {{"tag": attr.string(mandatory = True)}},
)

def _ext_impl(mctx):
    # Append one line per extension evaluation so tests can detect session reuse vs reopen.
    for mod in mctx.modules:
        pass
    _repo(name = "dev_only_repo", tag = "dev_present")

ext = module_extension(implementation = _ext_impl)

def _counted_impl(rctx):
    rctx.execute(["/bin/sh", "-c", "echo run >> '{ext_log_s}'"])
    rctx.file("BUILD", 'exports_files(["marker.txt"])\n')
    rctx.file("marker.txt", "ok\n")

counted_repo = repository_rule(implementation = _counted_impl, local = True)

def _counted_ext_impl(mctx):
    counted_repo(name = "counted")

counted_ext = module_extension(implementation = _counted_ext_impl)
"#
        ),
    );
    w.write(
        "MODULE.bazel",
        &format!(
            r#"
module(name = "opts_ws", version = "1.0.0")
bazel_dep(name = "dep_mod", version = "1.0.0")
local_path_override(
    module_name = "dep_mod",
    path = "{}",
)
ext = use_extension("//:ext.bzl", "ext", dev_dependency = True)
use_repo(ext, "dev_only_repo")
cext = use_extension("//:ext.bzl", "counted_ext")
use_repo(cext, "counted")
"#,
            mod_v1.to_string_lossy().replace('\\', "/")
        ),
    );
    w.write(
        "BUILD",
        r#"
genrule(
    name = "app",
    srcs = ["@dep_mod//:val.txt", "@counted//:marker.txt"],
    outs = ["app.txt"],
    cmd = "cat $(location @dep_mod//:val.txt) > $@",
)
"#,
    );

    let line_count = |p: &std::path::Path| -> usize {
        std::fs::read_to_string(p)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    };
    let app_out = || {
        let bin =
            w.rt.dirs
                .bazel_bin(zel_analysis::build_options::auto_host_cpu(), "fastbuild");
        std::fs::read_to_string(PathBuf::from(bin.path_string()).join("app.txt")).unwrap()
    };

    // 1. Cold build with --lockfile_mode=off (no MODULE.bazel.lock written).
    let (code, _, err) = w.run(&["build", "--lockfile_mode=off", "//:app"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(app_out().trim(), "from_v1");
    assert_eq!(line_count(&ext_log), 1);
    let r1 = w.rt.last_resolver.lock().unwrap().clone().unwrap();

    // 2. Identical warm build reuses BOTH Bzlmod Session and TargetResolver.
    let (code, _, err) = w.run(&["build", "--lockfile_mode=off", "//:app"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        !w.rt.repo_resolver.was_invalidated_this_command(),
        "identical warm build must reuse cached Bzlmod session"
    );
    assert_eq!(line_count(&ext_log), 1);
    let r2 = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    assert!(
        Arc::ptr_eq(&r1, &r2),
        "identical warm build must reuse TargetResolver"
    );

    // 3. Switching --lockfile_mode=off -> --lockfile_mode=error MUST invalidate cached Session
    //    and report the missing/out-of-date lockfile error.
    let (code, _, err) = w.run(&["build", "--lockfile_mode=error", "//:app"]);
    assert!(
        w.rt.repo_resolver.was_invalidated_this_command(),
        "changing --lockfile_mode must invalidate cached Bzlmod session"
    );
    assert_ne!(
        code, 0,
        "expected --lockfile_mode=error failure when lockfile is missing: {err}"
    );
    assert!(
        err.contains("--lockfile_mode=error") || err.contains("MODULE.bazel.lock"),
        "expected lockfile error message, got: {err}"
    );

    // 4. Build with --lockfile_mode=update -> succeeds and caches session.
    let (code, _, err) = w.run(&["build", "--lockfile_mode=update", "//:app"]);
    assert_eq!(code, 0, "{err}");
    let r_upd = w.rt.last_resolver.lock().unwrap().clone().unwrap();

    // Warm repeat with --lockfile_mode=update reuses both Session and TargetResolver.
    let (code, _, err) = w.run(&["build", "--lockfile_mode=update", "//:app"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        !w.rt.repo_resolver.was_invalidated_this_command(),
        "warm --lockfile_mode=update must reuse cached Bzlmod session"
    );
    let r_upd2 = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    assert!(Arc::ptr_eq(&r_upd, &r_upd2));

    // 5. Changing --ignore_dev_dependency MUST invalidate the cached Bzlmod session.
    let (code, _, err) = w.run(&[
        "build",
        "--lockfile_mode=off",
        "--ignore_dev_dependency",
        "//:app",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(
        w.rt.repo_resolver.was_invalidated_this_command(),
        "enabling --ignore_dev_dependency must invalidate cached Bzlmod session"
    );
    let r_dev = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    assert!(!Arc::ptr_eq(&r_upd2, &r_dev));

    // Warm repeat with --ignore_dev_dependency reuses again.
    let (code, _, err) = w.run(&[
        "build",
        "--lockfile_mode=off",
        "--ignore_dev_dependency",
        "//:app",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(
        !w.rt.repo_resolver.was_invalidated_this_command(),
        "warm build with unchanged --ignore_dev_dependency must reuse cached Bzlmod session"
    );

    // 6. Adding --override_module=dep_mod=<mod_v2> MUST invalidate the cached Bzlmod session
    //    and pick up dep_v2's contents.
    let ov_flag = format!("--override_module=dep_mod={}", mod_v2.to_string_lossy());
    let (code, _, err) = w.run(&[
        "build",
        "--lockfile_mode=off",
        "--ignore_dev_dependency",
        &ov_flag,
        "//:app",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(
        w.rt.repo_resolver.was_invalidated_this_command(),
        "adding --override_module must invalidate cached Bzlmod session"
    );
    assert_eq!(app_out().trim(), "from_v2");

    // Warm repeat with --override_module reuses BOTH Session and TargetResolver.
    let r_ov1 = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    let (code, _, err) = w.run(&[
        "build",
        "--lockfile_mode=off",
        "--ignore_dev_dependency",
        &ov_flag,
        "//:app",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(
        !w.rt.repo_resolver.was_invalidated_this_command(),
        "identical warm build with --override_module must reuse cached Bzlmod session"
    );
    let r_ov2 = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    assert!(
        Arc::ptr_eq(&r_ov1, &r_ov2),
        "identical warm build with --override_module must reuse TargetResolver"
    );
}

#[test]
fn cli_aspects_deps_are_reachable_and_do_not_leak_to_warm_builds() {
    let w = Ws::new("cli-aspects-reachability-and-isolation");
    w.write(
        "dep/BUILD",
        r#"
genrule(
    name = "old",
    outs = ["old.txt"],
    cmd = "echo from_aspect_extra > $@",
    deprecation = "old aspect helper is deprecated",
    visibility = ["//visibility:public"],
)
"#,
    );
    w.write(
        "extra/BUILD",
        r#"
genrule(
    name = "aspect_extra",
    srcs = ["//dep:old"],
    outs = ["aspect_extra.txt"],
    cmd = "cat $< > $@",
    visibility = ["//visibility:public"],
)
"#,
    );
    w.write(
        "defs.bzl",
        r#"
def _leaf_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".leaf.txt")
    ctx.actions.write(out, "leaf\n")
    return [DefaultInfo(files = depset([out]))]

leaf_rule = rule(implementation = _leaf_impl)

def _parent_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".parent.txt")
    ctx.actions.write(out, "parent\n")
    return [DefaultInfo(files = depset([out]))]

parent_rule = rule(
    implementation = _parent_impl,
    attrs = {"deps": attr.label_list()},
)

def _my_aspect_impl(target, ctx):
    extra_files = ctx.attr._extra[DefaultInfo].files.to_list()
    out = ctx.actions.declare_file(target.label.name + ".aspect.txt")
    ctx.actions.run_shell(
        inputs = extra_files,
        outputs = [out],
        command = "cat %s > %s" % (extra_files[0].path, out.path),
    )
    transitive = [out]
    for d in getattr(ctx.rule.attr, "deps", []):
        if OutputGroupInfo in d and hasattr(d[OutputGroupInfo], "aspect_out"):
            transitive.extend(d[OutputGroupInfo].aspect_out.to_list())
    return [OutputGroupInfo(aspect_out = depset(transitive))]

my_aspect = aspect(
    implementation = _my_aspect_impl,
    attr_aspects = ["deps"],
    attrs = {
        "_extra": attr.label(default = "//extra:aspect_extra"),
    },
)
"#,
    );
    w.write(
        "BUILD",
        r#"
load(":defs.bzl", "leaf_rule", "parent_rule")

exports_files(["defs.bzl"])

leaf_rule(name = "leaf")
parent_rule(name = "parent", deps = [":leaf"])
"#,
    );

    // 1. Build with CLI `--aspects`: the aspect's extra attribute dep (`//extra:aspect_extra`)
    //    must be included in `reachable_targets` so its generating action executes and feeds
    //    both `parent.aspect.txt` and propagated `leaf.aspect.txt`.
    let (code, _, err) = w.run(&[
        "build",
        "--aspects=//:defs.bzl%my_aspect",
        "--output_groups=+aspect_out",
        "//:parent",
    ]);
    assert_eq!(code, 0, "build with --aspects failed:\n{err}");
    let extra_out = w.bin_dir().join("extra/aspect_extra.txt");
    let parent_aspect_out = w.bin_dir().join("parent.aspect.txt");
    let leaf_aspect_out = w.bin_dir().join("leaf.aspect.txt");
    assert_eq!(
        std::fs::read_to_string(&extra_out).unwrap().trim(),
        "from_aspect_extra",
        "aspect extra attribute dep action must execute"
    );
    assert_eq!(
        std::fs::read_to_string(&parent_aspect_out).unwrap().trim(),
        "from_aspect_extra"
    );
    assert_eq!(
        std::fs::read_to_string(&leaf_aspect_out).unwrap().trim(),
        "from_aspect_extra"
    );
    assert!(
        err.contains("old aspect helper is deprecated"),
        "deprecation warning on aspect-pulled dep must be reported:\n{err}"
    );
    let r_aspect = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    assert!(
        r_aspect.has_cli_aspects(),
        "resolver used with CLI --aspects must be flagged"
    );

    // Remove the aspect-generated outputs so we can prove a follow-up build without `--aspects`
    // does not re-run or re-emit any aspect actions/outputs from the earlier command.
    std::fs::remove_file(&extra_out).unwrap();
    std::fs::remove_file(&parent_aspect_out).unwrap();
    std::fs::remove_file(&leaf_aspect_out).unwrap();

    // 2. Follow-up warm build WITHOUT `--aspects` (even requesting `--output_groups=+aspect_out`):
    //    must NOT reuse the CLI-aspect-polluted resolver and must NOT emit aspect outputs.
    let (code, _, err) = w.run(&["build", "--output_groups=+aspect_out", "//:parent"]);
    assert_eq!(code, 0, "follow-up build without --aspects failed:\n{err}");
    let r_clean = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    assert!(
        !Arc::ptr_eq(&r_aspect, &r_clean),
        "warm build without --aspects must not reuse a resolver that ran CLI aspects"
    );
    assert!(!r_clean.has_cli_aspects());
    assert!(
        !parent_aspect_out.exists() && !leaf_aspect_out.exists() && !extra_out.exists(),
        "CLI aspect outputs must not leak into subsequent builds without --aspects"
    );
    assert!(
        !err.contains("old aspect helper is deprecated"),
        "deprecation warnings from aspect-only deps must not leak into warm builds without --aspects:\n{err}"
    );

    // 3. Subsequent identical warm build without `--aspects` DOES reuse the clean resolver.
    let (code, _, err) = w.run(&["build", "--output_groups=+aspect_out", "//:parent"]);
    assert_eq!(code, 0, "{err}");
    let r_clean2 = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    assert!(
        Arc::ptr_eq(&r_clean, &r_clean2),
        "clean warm build without --aspects should reuse the clean resolver"
    );

    // 4. `aquery` after a `--aspects` build must also not leak aspect actions unless `--aspects`
    //    or `--skyframe_state` is requested.
    let (code, _, err) = w.run(&[
        "build",
        "--nobuild",
        "--aspects=//:defs.bzl%my_aspect",
        "//:parent",
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = w.run(&["aquery", "//:parent"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        !out.contains("parent.aspect.txt"),
        "plain aquery must not leak CLI aspect actions from previous build:\n{out}"
    );
}

#[test]
fn no_module_workspace_is_incremental_and_aquery_skyframe_state_works() {
    let w = Ws::new("no-module-incremental-and-aquery");
    // Remove MODULE.bazel so this exercises the plain (no-MODULE.bazel) workspace path
    // regardless of whether `/workspace/MODULE.bazel` exists on the test host.
    std::fs::remove_file(w.ws.join("MODULE.bazel")).unwrap();
    w.write(
        "lib/BUILD",
        "genrule(name = 'l', outs = ['l.out'], cmd = 'echo l > $@', visibility = ['//visibility:public'])\n",
    );
    w.write(
        "app/BUILD",
        "genrule(name = 'other', outs = ['o.out'], cmd = 'echo o > $@')\n",
    );

    let (code, _, err) = w.run(&["build", "--nobuild", "//lib:l", "//app:other"]);
    assert_eq!(code, 0, "{err}");
    let loads_after_first = w.rt.skyframe.package_loads();
    assert!(loads_after_first >= 2);
    let r1 = w.rt.last_resolver.lock().unwrap().clone().unwrap();

    // Warm repeat in a no-MODULE.bazel workspace must keep cached synthetic Bzlmod session,
    // avoid re-evaluating packages, and reuse TargetResolver.
    let (code, _, err) = w.run(&["build", "--nobuild", "//lib:l", "//app:other"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        !w.rt.repo_resolver.was_invalidated_this_command(),
        "warm build without MODULE.bazel must not invalidate synthetic Bzlmod session"
    );
    assert_eq!(
        w.rt.skyframe.package_loads(),
        loads_after_first,
        "no package should be re-evaluated on warm build without MODULE.bazel"
    );
    let r2 = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    assert!(Arc::ptr_eq(&r1, &r2));

    // `aquery --skyframe_state` immediately after the build must dump the Genrule actions.
    let (code, out, err) = w.run(&["aquery", "--skyframe_state", "--output=textproto"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("mnemonic: \"Genrule\""),
        "aquery --skyframe_state must dump actions from last build:\n{out}"
    );

    // Creating MODULE.bazel must invalidate the cached synthetic session.
    w.write("MODULE.bazel", "module(name = \"added\")\n");
    let (code, _, err) = w.run(&["build", "--nobuild", "//lib:l"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        w.rt.repo_resolver.was_invalidated_this_command(),
        "creating MODULE.bazel must invalidate cached no-module session"
    );
    let r3 = w.rt.last_resolver.lock().unwrap().clone().unwrap();
    assert!(!Arc::ptr_eq(&r2, &r3));
}
