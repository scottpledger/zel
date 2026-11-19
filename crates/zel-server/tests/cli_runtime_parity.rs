//! Integration tests for CLI/runtime parity in `zel-server`.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use zel_proto::command_server::RunRequest;
use zel_server::{write_server_status_files, BlazeDirectories, BlazeRuntime};
use zel_vfs::{FileSystem, LocalFileSystem, PathFragment};

fn local_registry() -> PathBuf {
    static REG: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    REG.get_or_init(|| {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let root = manifest_dir
            .strip_suffix("/crates/zel-server")
            .or_else(|| manifest_dir.strip_suffix("\\crates\\zel-server"))
            .unwrap();
        let dir = std::env::temp_dir().join(format!("zel-cli-parity-reg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let st = std::process::Command::new("python3")
            .arg(std::path::Path::new(root).join("scripts/make_vendored_registry.py"))
            .arg(&dir)
            .status()
            .expect("python3 required");
        assert!(st.success());
        dir
    })
    .clone()
}

struct Ws {
    ws: PathBuf,
    ob: PathBuf,
    rt: BlazeRuntime,
}

fn make_ws(name: &str) -> Ws {
    let root = std::env::temp_dir().join(format!("zel-cli-parity-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let ws = root.join("ws");
    let ob = root.join("ob");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::create_dir_all(&ob).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let ws = root.join("ws");
    let ob = root.join("ob");
    std::fs::write(
        ws.join("MODULE.bazel"),
        "module(name = \"w\")\nbazel_dep(name = \"rules_shell\", version = \"0.6.1\")\nbazel_dep(name = \"platforms\", version = \"1.0.0\")\n",
    )
    .unwrap();
    std::fs::write(ws.join("BUILD"), "").unwrap();
    let fs: Arc<dyn FileSystem> = Arc::new(LocalFileSystem::default());
    let dirs = BlazeDirectories {
        workspace_dir: PathFragment::create(ws.to_str().unwrap()),
        output_base: PathFragment::create(ob.to_str().unwrap()),
        install_base: PathFragment::create(root.join("ib").to_str().unwrap()),
        output_user_root: PathFragment::create(root.join("ur").to_str().unwrap()),
    };
    Ws {
        ws,
        ob,
        rt: BlazeRuntime::new(dirs, fs, 1234),
    }
}

struct Out {
    code: i32,
    #[allow(dead_code)]
    out: String,
    err: String,
    #[allow(dead_code)]
    exec: Option<zel_proto::command_server::ExecRequest>,
}

impl Ws {
    fn write(&self, rel: &str, content: &str) {
        let p = self.ws.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
        #[cfg(unix)]
        if rel.ends_with(".sh") {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
        }
    }

    fn run(&self, args: &[&str]) -> Out {
        let reg = format!(
            "--registry={}",
            zel_bzlmod::file_url_for_path(&local_registry())
        );
        let mut full_args: Vec<String> = vec![args[0].to_string()];
        if matches!(
            args[0],
            "build" | "test" | "run" | "coverage" | "query" | "cquery" | "aquery"
        ) {
            full_args.push(reg);
        }
        full_args.extend(args[1..].iter().map(|s| s.to_string()));
        let req = RunRequest {
            cookie: "c".into(),
            arg: full_args.into_iter().map(|s| s.into_bytes()).collect(),
            client_description: "test".into(),
            ..Default::default()
        };
        let r = self.rt.dispatch_command(&req, "r");
        Out {
            code: r.exit_code,
            out: String::from_utf8_lossy(&r.standard_output).into_owned(),
            err: String::from_utf8_lossy(&r.standard_error).into_owned(),
            exec: r.exec_request,
        }
    }
}

#[test]
fn starlark_options_validation_aliases_and_types() {
    let w = make_ws("starlark-opts");
    w.write(
        "p/defs.bzl",
        r#"
def _impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(out, str(ctx.build_setting_value))
    return [DefaultInfo(files = depset([out]))]

int_flag = rule(implementation = _impl, build_setting = config.int(flag = True))
bool_flag = rule(implementation = _impl, build_setting = config.bool(flag = True))
str_setting = rule(implementation = _impl, build_setting = config.string(flag = False))
list_flag = rule(implementation = _impl, build_setting = config.string_list(flag = True))
rep_list_flag = rule(implementation = _impl, build_setting = config.string_list(flag = True, repeatable = True))
"#,
    );
    w.write(
        "p/BUILD",
        r#"
load(":defs.bzl", "int_flag", "bool_flag", "str_setting", "list_flag", "rep_list_flag")
int_flag(name = "i", build_setting_default = 3)
bool_flag(name = "b", build_setting_default = False)
str_setting(name = "nonflag", build_setting_default = "x")
list_flag(name = "l", build_setting_default = [])
rep_list_flag(name = "rl", build_setting_default = [])
alias(name = "ial", actual = ":i")
alias(name = "cyc1", actual = ":cyc2")
alias(name = "cyc2", actual = ":cyc1")
config_setting(name = "cs", values = {"compilation_mode": "opt"})
alias(name = "sel_al", actual = select({":cs": ":i", "//conditions:default": ":i"}))
genrule(name = "g", outs = ["g.out"], cmd = "echo hi > $@")
"#,
    );

    // Non-build-setting target rejected
    let r = w.run(&["build", "--//p:g=1", "//p:g"]);
    assert_eq!(r.code, 2, "{}", r.err);
    assert!(r.err.contains("Unrecognized option: //p:g"), "{}", r.err);

    // flag = False rejected
    let r = w.run(&["build", "--//p:nonflag=y", "//p:g"]);
    assert_eq!(r.code, 2, "{}", r.err);
    assert!(
        r.err.contains("Unrecognized option: //p:nonflag=y"),
        "{}",
        r.err
    );

    // Invalid int rejected with While parsing option
    let r = w.run(&["build", "--//p:i=notanint", "//p:g"]);
    assert_eq!(r.code, 2, "{}", r.err);
    assert!(
        r.err
            .contains("While parsing option //p:i=notanint: 'notanint' is not a int"),
        "{}",
        r.err
    );

    // Invalid bool rejected
    let r = w.run(&["build", "--//p:b=maybe", "//p:g"]);
    assert_eq!(r.code, 2, "{}", r.err);
    assert!(
        r.err
            .contains("While parsing option //p:b=maybe: 'maybe' is not a boolean"),
        "{}",
        r.err
    );

    // Bare non-bool with following non-flag token suggests =val
    let r = w.run(&["build", "--//p:i", "42", "//p:g"]);
    assert_eq!(r.code, 2, "{}", r.err);
    assert!(
        r.err
            .contains("Expected value after --//p:i. Did you mean --//p:i=42?"),
        "{}",
        r.err
    );

    // --no prefix on non-bool fails
    let r = w.run(&["build", "--no//p:i", "//p:g"]);
    assert_eq!(r.code, 2, "{}", r.err);
    assert!(
        r.err
            .contains("Illegal use of 'no' prefix on non-boolean option: //p:i"),
        "{}",
        r.err
    );

    // --no//p:b=true does not strip `no`
    let r = w.run(&["build", "--no//p:b=true", "//p:g"]);
    assert_eq!(r.code, 2, "{}", r.err);
    assert!(r.err.contains("Error loading option no//p:b"), "{}", r.err);

    // Alias cycle fails
    let r = w.run(&["build", "--//p:cyc1=5", "//p:g"]);
    assert_eq!(r.code, 2, "{}", r.err);
    assert!(r.err.contains("cycle in alias chain"), "{}", r.err);

    // Alias with select() on actual fails
    let r = w.run(&["build", "--//p:sel_al=5", "//p:g"]);
    assert_eq!(r.code, 2, "{}", r.err);
    assert!(
        r.err.contains("actual value that uses select()"),
        "{}",
        r.err
    );

    // Valid alias to int_flag works
    let r = w.run(&["build", "--//p:ial=7", "//p:i"]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(
        std::fs::read_to_string(w.ob.join(format!(
            "execroot/_main/bazel-out/{}-fastbuild/bin/p/i.txt",
            zel_analysis::build_options::auto_host_cpu()
        )))
        .unwrap(),
        "7"
    );

    // Non-analyzing command rejects --//...
    let r = w.run(&["version", "--//p:i=7"]);
    assert_eq!(r.code, 2, "{}", r.err);
    assert!(r.err.contains("Unrecognized option"), "{}", r.err);
}

#[cfg(unix)]
#[test]
fn client_env_omitted_does_not_leak_and_two_token_form_works() {
    std::env::set_var("ZEL_SERVER_ENV_SECRET", "leaked_val");
    let w = make_ws("client-env");
    w.write(
        "p/BUILD",
        r#"
genrule(
    name = "g",
    outs = ["g.txt"],
    cmd = "echo S=$${ZEL_SERVER_ENV_SECRET:-unset} F=$${FOO:-unset} > $@",
)
"#,
    );
    // Without --client_env, server env must NOT leak when inherited via --action_env
    let r = w.run(&["build", "--action_env=ZEL_SERVER_ENV_SECRET", "//p:g"]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(
        std::fs::read_to_string(w.ws.join("bazel-bin/p/g.txt"))
            .unwrap()
            .trim(),
        "S=unset F=unset"
    );

    // Two-token --client_env FOO=bar works and does not break `version`
    let r2 = w.run(&[
        "build",
        "--client_env",
        "FOO=one",
        "--client_env=FOO=two",
        "--action_env=FOO",
        "--action_env=ZEL_SERVER_ENV_SECRET",
        "//p:g",
    ]);
    assert_eq!(r2.code, 0, "{}", r2.err);
    assert_eq!(
        std::fs::read_to_string(w.ws.join("bazel-bin/p/g.txt"))
            .unwrap()
            .trim(),
        "S=unset F=two"
    );

    let rv = w.run(&["version", "--client_env", "FOO=bar"]);
    assert_eq!(rv.code, 0, "{}", rv.err);
}

#[test]
fn shutdown_leaves_output_base_intact() {
    let w = make_ws("shutdown");
    let r = w.run(&["shutdown"]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(w.rt.shutdown_requested.load(Ordering::SeqCst));
    assert!(!w.rt.expunge_requested.load(Ordering::SeqCst));
    assert!(w.ob.exists(), "shutdown must not delete output_base");
}

#[cfg(unix)]
#[test]
fn run_command_parity_script_path_aliases_and_validation() {
    let w = make_ws("run-parity");
    w.write(
        "p/BUILD",
        r#"
load("@rules_shell//shell:sh_binary.bzl", "sh_binary")
load("@rules_shell//shell:sh_test.bzl", "sh_test")
sh_binary(name = "bin", srcs = ["bin.sh"], args = ["rule_arg"])
sh_test(name = "t", srcs = ["bin.sh"])
test_suite(name = "suite", tests = [":t"])
genrule(name = "g", outs = ["g.out"], cmd = "echo hi > $@")
alias(name = "bin_alias", actual = ":bin")
alias(name = "g_alias", actual = ":g")
"#,
    );
    w.write("p/bin.sh", "#!/bin/sh\necho \"BIN:$*\"\n");

    // test_suite is not executable
    let r = w.run(&["run", "//p:suite"]);
    assert_eq!(r.code, 2, "{}", r.err);
    assert!(r.err.contains("Not executable"), "{}", r.err);

    // alias to binary is accepted and runs
    let r = w.run(&["run", "//p:bin_alias", "--", "res_arg"]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.exec.is_some());

    // alias to non-executable genrule passes early check and fails post-analysis
    let r = w.run(&["run", "//p:g_alias"]);
    assert_eq!(r.code, 2, "{}", r.err);
    assert!(r.err.contains("Not executable"), "{}", r.err);

    // --nobuild_runfile_manifests is rejected with exit 2
    let r = w.run(&["run", "--nobuild_runfile_manifests", "//p:bin"]);
    assert_eq!(r.code, 2, "{}", r.err);
    assert!(
        r.err
            .contains("--nobuild_runfile_manifests is incompatible with the \"run\" command"),
        "{}",
        r.err
    );

    // --script_path resolves relative to --client_cwd, writes script, and returns NO ExecRequest
    let sub_cwd = w.ws.join("sub");
    std::fs::create_dir_all(&sub_cwd).unwrap();
    let r = w.run(&[
        "run",
        &format!("--client_cwd={}", sub_cwd.display()),
        "--script_path=out_script.sh",
        "--run_under=env FOO=bar",
        "--",
        "//p:bin",
        "cli_arg",
    ]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.exec.is_none(), "--script_path must return no ExecRequest");
    let script_file = sub_cwd.join("out_script.sh");
    assert!(
        script_file.exists(),
        "script must be written under client_cwd"
    );
    let script = std::fs::read_to_string(&script_file).unwrap();
    assert!(script.contains("/bin/bash -c 'env FOO=bar "), "{script}");
    assert!(script.contains("rule_arg"), "{script}");
    assert!(script.contains("cli_arg"), "{script}");
    assert!(script.ends_with("\"$@\""), "{script}");

    // --emit_script_path_in_exec_request skips disk write and returns ExecRequest with should_exec=false
    let r2 = w.run(&[
        "run",
        &format!("--client_cwd={}", sub_cwd.display()),
        "--script_path=emitted.sh",
        "--emit_script_path_in_exec_request",
        "//p:bin",
    ]);
    assert_eq!(r2.code, 0, "{}", r2.err);
    assert!(
        !sub_cwd.join("emitted.sh").exists(),
        "must not write script on server when emit flag is set"
    );
    let req = r2.exec.expect("should return ExecRequest");
    assert!(!req.should_exec);
    assert!(req.script_path.is_some());
}

#[test]
fn test_command_abort_prints_couldnt_start_not_no_tests_found() {
    let w = make_ws("test-abort");
    w.write(
        "p/BUILD",
        r#"
load("@rules_shell//shell:sh_test.bzl", "sh_test")
genrule(name = "bad", outs = ["bad.sh"], cmd = "echo $(location //p:nosuch) > $@")
sh_test(name = "t", srcs = [":bad"])
"#,
    );

    let r = w.run(&["test", "//p:t"]);
    assert_eq!(r.code, 1, "{}", r.err);
    assert!(
        r.err
            .contains("ERROR: Couldn't start the build. Unable to run tests"),
        "{}",
        r.err
    );
    assert!(
        !r.err
            .contains("No test targets were found, yet testing was requested"),
        "{}",
        r.err
    );
}

#[cfg(unix)]
#[test]
fn target_platform_incompatibility_and_skipping() {
    let w = make_ws("incompat");
    let incompatible_os = if cfg!(target_os = "windows") {
        "@platforms//os:linux"
    } else {
        "@platforms//os:windows"
    };
    w.write(
        "p/BUILD",
        &format!(
            r#"
load("@rules_shell//shell:sh_test.bzl", "sh_test")
genrule(
    name = "bad_gen",
    outs = ["bad.out"],
    cmd = "exit 1",
    target_compatible_with = ["{incompatible_os}"],
)
genrule(
    name = "ok_gen",
    outs = ["ok.out"],
    cmd = "echo ok > $@",
)
sh_test(
    name = "bad_test",
    srcs = ["fail.sh"],
    target_compatible_with = ["{incompatible_os}"],
)
sh_test(
    name = "ok_test",
    srcs = ["ok.sh"],
)
"#
        ),
    );
    w.write("p/fail.sh", "#!/bin/sh\nexit 1\n");
    w.write("p/ok.sh", "#!/bin/sh\nexit 0\n");

    // Explicit incompatible target fails analysis
    let r = w.run(&["build", "//p:bad_gen"]);
    assert_eq!(r.code, 1, "{}", r.err);
    assert!(
        r.err
            .contains("is incompatible and cannot be built, but was explicitly requested"),
        "{}",
        r.err
    );

    // Wildcard skips incompatible targets, builds compatible targets
    let r = w.run(&["build", "//p:all"]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(!w.ws.join("bazel-bin/p/bad.out").exists());
    assert!(w.ws.join("bazel-bin/p/ok.out").exists());

    // --skip_incompatible_explicit_targets skips explicit incompatible target
    let r = w.run(&[
        "build",
        "--skip_incompatible_explicit_targets",
        "//p:bad_gen",
    ]);
    assert_eq!(r.code, 0, "{}", r.err);

    // Test wildcard skips incompatible test and succeeds when non-skipped tests pass
    let r = w.run(&["test", "//p:all"]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert!(
        r.out.contains("1 test passes and 1 was skipped."),
        "{}{}",
        r.out,
        r.err
    );
}

#[test]
fn status_files_write_latin1() {
    let dir = std::env::temp_dir().join(format!("zel-status-latin1-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let fs = LocalFileSystem::default();
    let server_dir = PathFragment::create(dir.to_str().unwrap());
    write_server_status_files(
        &fs,
        &server_dir,
        42,
        "127.0.0.1:9999",
        "req\u{2603}",
        "resp\u{00E9}",
    )
    .unwrap();
    let req_bytes = std::fs::read(dir.join("request_cookie")).unwrap();
    assert_eq!(req_bytes, b"req?");
    let resp_bytes = std::fs::read(dir.join("response_cookie")).unwrap();
    assert_eq!(resp_bytes, &[b'r', b'e', b's', b'p', 0xE9]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn aquery_skyframe_state_omits_var_ref() {
    let w = make_ws("aquery-var");
    w.write(
        "p/BUILD",
        "genrule(name = \"g\", outs = [\"g.out\"], cmd = \"touch $@\")\n",
    );
    // Build once so Skyframe has state
    let b = w.run(&["build", "//p:g"]);
    assert_eq!(b.code, 0, "{}", b.err);
    // `set($x)`: $x must not trigger exit 2 "Specifying build target(s) [$x] with --skyframe_state"
    let r = w.run(&[
        "aquery",
        "--skyframe_state",
        "--output=textproto",
        "set($x)",
    ]);
    assert!(
        !r.err
            .contains("with --skyframe_state is currently not supported"),
        "{}",
        r.err
    );
    assert_ne!(r.code, 2, "{}", r.err);
}
