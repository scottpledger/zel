use std::path::PathBuf;
use std::sync::Arc;
use zel_proto::command_server::{ExecRequest, RunRequest};
use zel_server::{BlazeDirectories, BlazeRuntime};
use zel_vfs::{FileSystem, LocalFileSystem, PathFragment};

struct Ws {
    ws: PathBuf,
    rt: BlazeRuntime,
    root: PathBuf,
}

impl Ws {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zel-srv-parity-{name}-{}", std::process::id()));
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if rel.ends_with(".sh") {
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
            }
        }
    }

    fn run(&self, argv: &[&str]) -> (i32, String, String, Option<ExecRequest>) {
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
            r.exec_request,
        )
    }
}

impl Drop for Ws {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn target_compatible_with_wildcard_skip_and_explicit_fail_and_flag_override() {
    let w = Ws::new("tcw");
    w.write(
        "BUILD",
        r#"
constraint_setting(name = "cs")
constraint_value(name = "cv", constraint_setting = ":cs")
filegroup(name = "good", srcs = [])
filegroup(name = "bad", srcs = [], target_compatible_with = [":cv"])
"#,
    );

    let (code, _, err, _) = w.run(&["build", "//:all"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("Target //:bad was skipped"), "{err}");

    let (code2, _, err2, _) = w.run(&["build", "//:bad"]);
    assert_ne!(code2, 0);
    assert!(
        err2.contains(
            "Target //:bad is incompatible and cannot be built, but was explicitly requested"
        ),
        "{err2}"
    );

    let (code3, _, err3, _) = w.run(&["build", "--skip_incompatible_explicit_targets", "//:bad"]);
    assert_eq!(code3, 0, "{err3}");
    assert!(err3.contains("Target //:bad was skipped"), "{err3}");
}

#[test]
fn starlark_flag_alias_chain_and_compile_one_dependency() {
    let w = Ws::new("star-alias");
    w.write(
        "defs.bzl",
        r#"
BSI = provider(fields = ["value"])
def _f(ctx): return [BSI(value = ctx.build_setting_value)]
str_flag = rule(implementation = _f, build_setting = config.string(flag = True))
def _r(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(out, ctx.attr._f[BSI].value)
    return [DefaultInfo(files = depset([out]))]
my_rule = rule(implementation = _r, attrs = {"srcs": attr.label_list(allow_files = True), "_f": attr.label(default = "//:real_flag")})
"#,
    );
    w.write(
        "BUILD",
        r#"
load(":defs.bzl", "str_flag", "my_rule")
str_flag(name = "real_flag", build_setting_default = "def")
alias(name = "flag_alias", actual = ":real_flag")
my_rule(name = "r", srcs = ["a.src"])
"#,
    );
    w.write("a.src", "hi");

    let (code, _, err, _) = w.run(&[
        "build",
        "--//:flag_alias=via_alias",
        "--compile_one_dependency",
        "a.src",
    ]);
    assert_eq!(code, 0, "{err}");
    let out_txt = std::fs::read_to_string(w.root.join(format!(
        "ob/execroot/_main/bazel-out/{}-fastbuild/bin/r.txt",
        zel_analysis::build_options::auto_host_cpu()
    )))
    .unwrap();
    assert_eq!(out_txt, "via_alias");
}

#[test]
fn run_alias_expands_args_location_and_missing_runfiles_fails_build() {
    let w = Ws::new("run-alias");
    w.write(
        "defs.bzl",
        r#"
def _bin(ctx):
    exe = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.symlink(output = exe, target_file = ctx.files.srcs[0], is_executable = True)
    return [DefaultInfo(executable = exe, files = depset([exe]), runfiles = ctx.runfiles(files = ctx.files.data))]
sh_bin = rule(
    implementation = _bin,
    executable = True,
    attrs = {
        "srcs": attr.label_list(allow_files = True),
        "data": attr.label_list(allow_files = True),
        "args": attr.string_list(),
    },
)
"#,
    );
    w.write(
        "BUILD",
        r#"
load(":defs.bzl", "sh_bin")
sh_bin(name = "b", srcs = ["b.sh"], data = ["d.txt"], args = ["$(location :d.txt)"])
alias(name = "b_alias", actual = ":b")
sh_bin(name = "missing_data", srcs = ["b.sh"], data = ["nope.txt"])
"#,
    );
    w.write("b.sh", "#!/bin/sh\necho $@\n");
    w.write("d.txt", "data");

    let (code, _, err, req) = w.run(&["run", "//:b_alias"]);
    assert_eq!(code, 0, "{err}");
    let req = req.unwrap();
    let argv: Vec<String> = req
        .argv
        .iter()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    assert!(argv.iter().any(|a| a == "./d.txt"), "{argv:?}");

    let (code2, _, err2, _) = w.run(&["build", "//:missing_data"]);
    assert_ne!(code2, 0);
    assert!(
        err2.contains("//:missing_data: missing input file '//:nope.txt'"),
        "{err2}"
    );
}

#[test]
fn config_options_accepts_java_language_version_and_test_suite_negative_subtraction() {
    let w = Ws::new("ts-neg");
    w.write(
        "defs.bzl",
        r##"
def _t(ctx):
    exe = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.write(exe, "#!/bin/sh\nexit 0\n", is_executable = True)
    return [DefaultInfo(executable = exe, files = depset([exe]))]
my_test = rule(implementation = _t, test = True)
"##,
    );
    w.write(
        "BUILD",
        r#"
load(":defs.bzl", "my_test")
my_test(name = "t1")
my_test(name = "t2")
test_suite(name = "ts", tests = [":t1", ":t2"])
config_setting(name = "j17", values = {"java_language_version": "17"})
filegroup(name = "sel", srcs = select({":j17": ["a.txt"], "//conditions:default": []}))
"#,
    );
    w.write("a.txt", "17");

    let (code, _, err, _) = w.run(&["build", "--java_language_version=17", "//:sel"]);
    assert_eq!(code, 0, "{err}");

    let (code2, _, err2, _) = w.run(&["test", "--", "//:ts", "-//:t1", "-//:t2"]);
    assert_eq!(code2, 4, "{err2}");
    assert!(
        err2.contains("All specified test targets were excluded by filters"),
        "{err2}"
    );
}
