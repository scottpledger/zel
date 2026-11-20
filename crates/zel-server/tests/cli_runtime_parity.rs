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
fn client_env_home_expands_tilde_in_early_repository_option_checks() {
    let w = make_ws("client-env-tilde");
    // Normalize through `PathFragment` so Windows tests strip `\\?\` identically to `workspace_dir`.
    let ws_norm = PathBuf::from(PathFragment::create(w.ws.to_str().unwrap()).path_string());
    let root_norm = ws_norm.parent().unwrap().to_path_buf();
    let custom_home = root_norm.join("custom_home");
    std::fs::create_dir_all(&custom_home).unwrap();
    std::fs::write(custom_home.join("valid_dl.cfg"), "rewrite (.*) $1\n").unwrap();
    std::fs::write(custom_home.join("bad_dl.cfg"), "invalid_directive foo\n").unwrap();

    let home_custom_arg = format!("--client_env=HOME={}", custom_home.display());
    let ok_dl = w.run(&[
        "info",
        &home_custom_arg,
        "--downloader_config=~/valid_dl.cfg",
        "workspace",
    ]);
    assert_eq!(ok_dl.code, 0, "{}", ok_dl.err);

    let bad_dl = w.run(&[
        "info",
        &home_custom_arg,
        "--downloader_config=~/bad_dl.cfg",
        "workspace",
    ]);
    assert_eq!(bad_dl.code, 2, "{}", bad_dl.err);
    assert!(
        bad_dl.err.contains("Failed to parse downloader config"),
        "{}",
        bad_dl.err
    );

    let home_root_arg = format!("--client_env=HOME={}", root_norm.display());
    let bad_cache = w.run(&[
        "info",
        &home_root_arg,
        "--repo_contents_cache=~/ws/inside_cache",
        "workspace",
    ]);
    assert_eq!(bad_cache.code, 2, "{}", bad_cache.err);
    assert!(
        bad_cache.err.contains("is inside the main repo"),
        "{}",
        bad_cache.err
    );

    let ok_cache = w.run(&[
        "info",
        &home_custom_arg,
        "--repo_contents_cache=~/outside_cache",
        "workspace",
    ]);
    assert_eq!(ok_cache.code, 0, "{}", ok_cache.err);
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

#[test]
fn canonicalize_flags_for_command_aliases_and_policy_bool() {
    let w = make_ws("canon-flags");
    w.write(
        "p/defs.bzl",
        "def _impl(ctx):\n    return []\nstr_flag = rule(implementation = _impl, build_setting = config.string(flag = True))\n",
    );
    w.write(
        "p/BUILD",
        "load(\":defs.bzl\", \"str_flag\")\nstr_flag(name = \"my_flag\", build_setting_default = \"d\")\n",
    );
    let r = w.run(&[
        "canonicalize-flags",
        "--for_command",
        "test",
        "--canonicalize_policy=false",
        "--flag_alias=mf=//p:my_flag",
        "--",
        "--mf=val",
        "--test_output=errors",
    ]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(
        r.out.contains("--//p:my_flag=val") && r.out.contains("--test_output=errors"),
        "out={} err={}",
        r.out,
        r.err
    );

    let r_pol = w.run(&[
        "canonicalize-flags",
        "--for_command",
        "test",
        "--canonicalize_policy=true",
        "--invocation_policy=flag_policies { flag_name: 'test_output' set_value { flag_value: 'all' behavior: FINAL_VALUE_IGNORE_OVERRIDES } }",
        "--",
        "--test_output=errors",
    ]);
    assert_eq!(r_pol.code, 0, "{}", r_pol.err);
    assert!(r_pol.out.contains("test_output"), "{}", r_pol.out);

    // `.bazelrc` overrides (`--rc_source` + `--default_override`) configure `canonicalize-flags`
    // (`--for_command`, `--experimental_include_default_values`, `--flag_alias`, and pre-`--`
    // `--config`) without leaking unconditional `build` options into stdout.
    let r_rc_default = w.run(&[
        "canonicalize-flags",
        "--rc_source=/ws/.bazelrc",
        "--default_override=0:canonicalize-flags=--for_command=test",
        "--default_override=0:common=--experimental_include_default_values=false",
        "--default_override=0:build=--flag_alias=mf=//p:my_flag",
        "--default_override=0:build=--jobs=8",
        "--",
        "--mf=d",
        "--test_output=errors",
    ]);
    assert_eq!(r_rc_default.code, 0, "{}", r_rc_default.err);
    assert_eq!(
        r_rc_default.out.lines().collect::<Vec<_>>(),
        vec!["--test_output=errors"]
    );

    let r_rc_nondefault = w.run(&[
        "canonicalize-flags",
        "--rc_source=/ws/.bazelrc",
        "--default_override=0:canonicalize-flags:t=--for_command=test",
        "--default_override=0:build=--flag_alias=mf=//p:my_flag",
        "--config=t",
        "--",
        "--mf=custom",
        "--test_output=errors",
        "--config=unexpanded",
    ]);
    assert_eq!(r_rc_nondefault.code, 0, "{}", r_rc_nondefault.err);
    assert!(
        r_rc_nondefault.out.contains("--//p:my_flag=custom")
            && r_rc_nondefault.out.contains("--test_output=errors")
            && r_rc_nondefault.out.contains("--config=unexpanded"),
        "out={} err={}",
        r_rc_nondefault.out,
        r_rc_nondefault.err
    );

    let r_bad_alias = w.run(&[
        "canonicalize-flags",
        "--rc_source=/ws/.bazelrc",
        "--default_override=0:test=--flag_alias=mf=//p:my_flag",
        "--",
        "--mf=custom",
    ]);
    assert_eq!(r_bad_alias.code, 2, "{}", r_bad_alias.err);
    assert!(
        r_bad_alias.err.contains(
            "disallowed. --flag_alias only supports these commands: build, common, always"
        ),
        "{}",
        r_bad_alias.err
    );
}

#[test]
fn info_client_env_make_env_and_unknown_key() {
    let w = make_ws("info-parity");
    let r = w.run(&[
        "info",
        "--action_env=KEEP_ME",
        "--action_env=FIXED=1",
        "--action_env=DROP_ME",
        "--action_env==DROP_ME",
        "--client_env=KEEP_ME=hello",
        "--client_env=FIXED=ignored",
        "--client_env=DROP_ME=bye",
        "client-env",
    ]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.out.contains("--action_env=KEEP_ME=hello"), "{}", r.out);
    assert!(!r.out.contains("FIXED"), "{}", r.out);
    assert!(!r.out.contains("DROP_ME"), "{}", r.out);

    let m = w.run(&["info", "--show_make_env", "--define=CUSTOM_VAR=42"]);
    assert_eq!(m.code, 0, "{}", m.err);
    assert!(m.out.contains("CUSTOM_VAR: 42"), "{}", m.out);
    assert!(
        m.out.contains("execution_root:") && m.out.contains("/_main"),
        "{}",
        m.out
    );

    let bad = w.run(&["info", "definitely-unknown-info-key"]);
    assert_eq!(bad.code, 2, "{}", bad.err);
}

#[cfg(unix)]
#[test]
fn config_json_case_insensitive_and_user_defined_defines() {
    let w = make_ws("config-parity");
    w.write(
        "p/BUILD",
        "genrule(name = \"g\", outs = [\"g.out\"], cmd = \"touch $@\")\n",
    );
    let b = w.run(&["build", "--define=FOO=bar", "//p:g"]);
    assert_eq!(b.code, 0, "{}", b.err);

    let c = w.run(&["config", "--dump_all", "--output=JSON"]);
    assert_eq!(c.code, 0, "{}", c.err);
    assert!(c.out.contains("user-defined"), "{}", c.out);
    assert!(c.out.contains("--define:FOO"), "{}", c.out);
}

#[cfg(unix)]
#[test]
fn build_noanalyze_and_nobuild_convenience_symlinks_bep() {
    let w = make_ws("build-phases");
    w.write(
        "p/BUILD",
        "genrule(name = \"g\", outs = [\"g.out\"], cmd = \"echo hi > $@\")\n",
    );

    let na = w.run(&["build", "--noanalyze", "//p:g"]);
    assert_eq!(na.code, 0, "{}", na.err);
    assert!(!w.ws.join("bazel-bin/p/g.out").exists());

    let bep_on = w.ws.join("bep-on.json");
    let nb = w.run(&[
        "build",
        "--nobuild",
        &format!("--build_event_json_file={}", bep_on.display()),
        "//p:g",
    ]);
    assert_eq!(nb.code, 0, "{}", nb.err);
    // `--nobuild` still creates convenience symlinks whose target directories exist (not dangling).
    assert!(w.ws.join("bazel-bin").exists());
    let bep_on_txt = std::fs::read_to_string(&bep_on).unwrap();
    assert!(
        bep_on_txt.contains("convenienceSymlinksIdentified") && bep_on_txt.contains("bazel-bin"),
        "{bep_on_txt}"
    );

    let bep_off = w.ws.join("bep-off.json");
    let nb2 = w.run(&[
        "build",
        "--nobuild",
        "--noexperimental_convenience_symlinks_bep_event",
        &format!("--build_event_json_file={}", bep_off.display()),
        "//p:g",
    ]);
    assert_eq!(nb2.code, 0, "{}", nb2.err);
    let bep_off_txt = std::fs::read_to_string(&bep_off).unwrap();
    assert!(
        !bep_off_txt.contains("convenienceSymlinksIdentified"),
        "{bep_off_txt}"
    );
}

#[cfg(unix)]
#[test]
fn run_norun_exec_request_and_script_path_equals_escaping() {
    let w = make_ws("run-norun-parity");
    w.write(
        "p/BUILD",
        "load(\"@rules_shell//shell:sh_binary.bzl\", \"sh_binary\")\nsh_binary(name = \"app\", srcs = [\"app.sh\"])\n",
    );
    w.write("p/app.sh", "#!/bin/sh\necho \"$@\"\n");

    let nr = w.run(&[
        "run",
        "--norun",
        "--invocation_id=11111111-2222-3333-4444-555555555555",
        "//p:app",
        "--",
        "FOO=bar",
    ]);
    assert_eq!(nr.code, 0, "{}", nr.err);
    let exec = nr
        .exec
        .expect("ExecRequest must be populated under --norun");
    assert!(!exec.should_exec);
    let build_id = exec
        .environment_variable
        .iter()
        .find(|ev| ev.name == b"BUILD_ID")
        .map(|ev| String::from_utf8_lossy(&ev.value).into_owned());
    assert_eq!(
        build_id.as_deref(),
        Some("11111111-2222-3333-4444-555555555555")
    );

    let script = w.ws.join("run.sh");
    let sp = w.run(&[
        "run",
        &format!("--script_path={}", script.display()),
        "//p:app",
        "--",
        "FOO=bar",
        "~tilde",
    ]);
    assert_eq!(sp.code, 0, "{}", sp.err);
    let content = std::fs::read_to_string(&script).unwrap();
    assert!(content.contains("'FOO=bar'"), "{content}");
    assert!(content.contains("'~tilde'"), "{content}");
}

#[cfg(unix)]
#[test]
fn print_action_empty_match_and_query_diagnostics() {
    let w = make_ws("diag-parity");
    w.write(
        "p/defs.bzl",
        "def _impl(ctx):\n    out = ctx.actions.declare_file(ctx.label.name + \".out\")\n    ctx.actions.run_shell(outputs = [out], command = \"touch \" + out.path, mnemonic = \"MyCustomAction\")\n    return [DefaultInfo(files = depset([out]))]\ncustom_rule = rule(implementation = _impl)\n",
    );
    w.write(
        "p/BUILD",
        "load(\":defs.bzl\", \"custom_rule\")\ngenrule(name = \"g\", outs = [\"g.out\"], cmd = \"touch $@\")\ncustom_rule(name = \"c\")\n",
    );

    let pa = w.run(&[
        "print_action",
        "--print_action_mnemonics=NonExistentMnemonic",
        "//p:c",
    ]);
    assert_ne!(pa.code, 0);
    assert!(
        pa.err.contains("no actions to print were found"),
        "{}",
        pa.err
    );

    let qf = w.run(&["query", "--output_file=missing_dir/out.txt", "//p:all"]);
    assert_eq!(qf.code, 36, "{}", qf.err);

    let aq_bad = w.run(&["aquery", "--skyframe_state", "deps("]);
    assert_eq!(aq_bad.code, 2, "{}", aq_bad.err);
    assert!(aq_bad.err.contains("Error while parsing"), "{}", aq_bad.err);

    let aq_univ = w.run(&["aquery", "--universe_scope=//p:all"]);
    assert_eq!(aq_univ.code, 0, "{}", aq_univ.err);
    assert!(aq_univ.out.contains("Genrule"), "{}", aq_univ.out);
}

#[test]
fn dispatch_validates_client_cwd_downloader_and_invocation_policy() {
    let w = make_ws("dispatch-val");

    let cwd_bad = w.run(&["info", "--client_cwd=.."]);
    assert_eq!(cwd_bad.code, 2, "{}", cwd_bad.err);

    let dl_bad = w.run(&["build", "--http_max_parallel_downloads=0"]);
    assert_eq!(dl_bad.code, 2, "{}", dl_bad.err);

    let req = RunRequest {
        cookie: String::new(),
        arg: vec![b"info".to_vec()],
        block_for_lock: true,
        client_description: "test".into(),
        invocation_policy: "not_valid_proto {{{".into(),
        startup_options: Vec::new(),
        preemptible: false,
        command_extensions: Vec::new(),
        quiet: true,
    };
    let resp = w.rt.dispatch_command(&req, "");
    assert_eq!(resp.exit_code, 2);
}

#[cfg(unix)]
#[test]
fn fetch_vendor_client_cwd_pattern_file_and_empty_to_null_path_override() {
    let w = make_ws("cwd-pattern-and-void-path");
    w.write("MODULE.bazel", "module(name = \"ws\")\n");
    w.write("BUILD", "filegroup(name = \"root_fg\")\n");
    w.write("patterns.txt", "//:missing_root_target\n");
    w.write(
        "sub/BUILD",
        "genrule(name = \"app\", outs = [\"app.txt\"], cmd = \"echo ok > $@\")\n",
    );
    w.write("sub/patterns.txt", ":app\n");

    let fetch_res = w.run(&[
        "fetch",
        "--client_cwd=sub",
        "--target_pattern_file=patterns.txt",
    ]);
    assert_eq!(fetch_res.code, 0, "{}", fetch_res.err);

    let vendor_dir = w.ws.join("vendored_deps");
    let vendor_flag = format!("--vendor_dir={}", vendor_dir.display());
    let vendor_res = w.run(&[
        "vendor",
        &vendor_flag,
        "--client_cwd=sub",
        "--target_pattern_file=patterns.txt",
    ]);
    assert_eq!(vendor_res.code, 0, "{}", vendor_res.err);

    let exec_json = w.ws.join("should_not_be_created.json");
    let set_flag = format!("--execution_log_json_file={}", exec_json.display());
    let build_res = w.run(&[
        "build",
        &set_flag,
        "--execution_log_json_file=",
        "//sub:app",
    ]);
    assert_eq!(build_res.code, 0, "{}", build_res.err);
    assert!(
        !exec_json.exists(),
        "empty --execution_log_json_file= override must clear earlier path to null"
    );
}

#[cfg(unix)]
#[test]
fn test_bep_undeclared_outputs_zip_and_nozip_names() {
    let w = make_ws("test-bep-undeclared-names");
    w.write(
        "p/BUILD",
        "load(\"@rules_shell//shell:sh_test.bzl\", \"sh_test\")\nsh_test(name = \"u\", srcs = [\"u.sh\"])\n",
    );
    w.write(
        "p/u.sh",
        concat!(
            "#!/bin/sh\n",
            "set -eu\n",
            "mkdir -p \"$TEST_UNDECLARED_OUTPUTS_DIR/sub\"\n",
            "printf 'top' > \"$TEST_UNDECLARED_OUTPUTS_DIR/top.txt\"\n",
            "printf 'nested' > \"$TEST_UNDECLARED_OUTPUTS_DIR/sub/nested.txt\"\n",
            "printf 'annot' > \"$TEST_UNDECLARED_OUTPUTS_ANNOTATIONS_DIR/a.part\"\n",
        ),
    );

    let bep_nozip = w.ws.join("bep-nozip.json");
    let r_nozip = w.run(&[
        "test",
        "--nocache_test_results",
        "--nozip_undeclared_test_outputs",
        &format!("--build_event_json_file={}", bep_nozip.display()),
        "//p:u",
    ]);
    assert_eq!(r_nozip.code, 0, "{}", r_nozip.err);
    let nozip_txt = std::fs::read_to_string(&bep_nozip).unwrap();
    assert!(
        nozip_txt.contains("\"name\":\"test.outputs/top.txt\"")
            || nozip_txt.contains("\"name\": \"test.outputs/top.txt\""),
        "expected test.outputs/top.txt in BEP JSON, got:\n{nozip_txt}"
    );
    assert!(
        nozip_txt.contains("\"name\":\"test.outputs/sub/nested.txt\"")
            || nozip_txt.contains("\"name\": \"test.outputs/sub/nested.txt\""),
        "expected test.outputs/sub/nested.txt in BEP JSON, got:\n{nozip_txt}"
    );
    assert!(
        nozip_txt.contains("test.outputs_manifest__MANIFEST"),
        "expected test.outputs_manifest__MANIFEST in BEP JSON, got:\n{nozip_txt}"
    );
    assert!(
        nozip_txt.contains("test.outputs_manifest__ANNOTATIONS"),
        "expected test.outputs_manifest__ANNOTATIONS in BEP JSON, got:\n{nozip_txt}"
    );
    assert!(
        !nozip_txt.contains("test.outputs__top.txt")
            && !nozip_txt.contains("test.outputs__sub/nested.txt"),
        "unzipped undeclared outputs must not use double underscores:\n{nozip_txt}"
    );

    let bep_zip = w.ws.join("bep-zip.json");
    let r_zip = w.run(&[
        "test",
        "--nocache_test_results",
        "--zip_undeclared_test_outputs",
        &format!("--build_event_json_file={}", bep_zip.display()),
        "//p:u",
    ]);
    assert_eq!(r_zip.code, 0, "{}", r_zip.err);
    let zip_txt = std::fs::read_to_string(&bep_zip).unwrap();
    assert!(
        zip_txt.contains("test.outputs__outputs.zip"),
        "expected test.outputs__outputs.zip in BEP JSON, got:\n{zip_txt}"
    );
    assert!(
        zip_txt.contains("test.outputs_manifest__MANIFEST"),
        "expected test.outputs_manifest__MANIFEST in BEP JSON, got:\n{zip_txt}"
    );
    assert!(
        zip_txt.contains("test.outputs_manifest__ANNOTATIONS"),
        "expected test.outputs_manifest__ANNOTATIONS in BEP JSON, got:\n{zip_txt}"
    );
    assert!(
        !zip_txt.contains("\"name\":\"test.outputs/")
            && !zip_txt.contains("\"name\": \"test.outputs/"),
        "zipped undeclared outputs must not emit individual test.outputs/* entries:\n{zip_txt}"
    );

    // 3. Flaky retry: failed attempt 1 writes extras, passing attempt 2 writes none; BEP
    // `testResult` for attempt 2 must not inherit stale extras from attempt 1.
    let retry_marker = w.ws.join("flaky_bep_once");
    let _ = std::fs::remove_file(&retry_marker);
    w.write(
        "p/BUILD",
        "load(\"@rules_shell//shell:sh_test.bzl\", \"sh_test\")\nsh_test(name = \"u\", srcs = [\"u.sh\"])\nsh_test(name = \"flaky_u\", srcs = [\"flaky_u.sh\"], tags = [\"local\"])\n",
    );
    w.write(
        "p/flaky_u.sh",
        &format!(
            concat!(
                "#!/bin/sh\n",
                "set -eu\n",
                "if [ ! -e \"{marker}\" ]; then\n",
                "  touch \"{marker}\"\n",
                "  printf 'warn1\\n' > \"$TEST_WARNINGS_OUTPUT_FILE\"\n",
                "  printf 'infra1\\n' > \"$TEST_INFRASTRUCTURE_FAILURE_FILE\"\n",
                "  printf 'only1' > \"$TEST_UNDECLARED_OUTPUTS_DIR/fail_only.txt\"\n",
                "  exit 1\n",
                "fi\n",
                "exit 0\n",
            ),
            marker = retry_marker.display()
        ),
    );
    let bep_flaky = w.ws.join("bep-flaky.json");
    let r_flaky = w.run(&[
        "test",
        "--nocache_test_results",
        "--flaky_test_attempts=2",
        "--nozip_undeclared_test_outputs",
        &format!("--build_event_json_file={}", bep_flaky.display()),
        "//p:flaky_u",
    ]);
    assert_eq!(r_flaky.code, 0, "{}", r_flaky.err);
    let flaky_lines: Vec<String> = std::fs::read_to_string(&bep_flaky)
        .unwrap()
        .lines()
        .filter(|l| l.contains("\"testActionOutput\""))
        .map(str::to_string)
        .collect();
    assert_eq!(
        flaky_lines.len(),
        2,
        "expected 2 testResult events: {flaky_lines:?}"
    );
    let att1 = &flaky_lines[0];
    let att2 = &flaky_lines[1];
    assert!(att1.contains("test.warnings"), "{att1}");
    assert!(att1.contains("test.infrastructure_failure"), "{att1}");
    assert!(att1.contains("test.outputs/fail_only.txt"), "{att1}");
    assert!(att1.contains("test.outputs_manifest__MANIFEST"), "{att1}");
    assert!(
        !att2.contains("test.warnings")
            && !att2.contains("test.infrastructure_failure")
            && !att2.contains("test.outputs/fail_only.txt")
            && !att2.contains("test.outputs_manifest__MANIFEST"),
        "passing retry attempt 2 leaked stale extras from attempt 1:\n{att2}"
    );
}

#[cfg(unix)]
#[test]
fn blazel_utils_workspace_root_sibling_layout_and_unmapped_repos_e2e() {
    let w = make_ws("blazel-utils");
    // Override MODULE.bazel so `platforms` is NOT declared as a dep of the root module,
    // while `rules_shell` IS declared under apparent name `my_shell`.
    w.write(
        "MODULE.bazel",
        "module(name = \"w\")\nbazel_dep(name = \"rules_shell\", version = \"0.6.1\", repo_name = \"my_shell\")\n",
    );
    w.write(
        "blazel_utils.bzl",
        r#"
def _to_label(repo_name):
    if not repo_name.startswith("@"):
        repo_name = "@" + repo_name
    return Label(repo_name + "//:unused")

def get_canonical_repo_name(apparent_repo_name):
    return _to_label(apparent_repo_name).workspace_name

def get_repo_root(apparent_repo_name):
    return _to_label(apparent_repo_name).workspace_root

def _probe_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.run_shell(
        inputs = [ctx.file.ext_src],
        outputs = [out],
        command = "test -f \"$1\" && printf '%s' \"$2\" > \"$3\"",
        arguments = [
            ctx.file.ext_src.path,
            "|".join([
                str(ctx.configuration.is_sibling_repository_layout()),
                ctx.file.ext_src.path,
                ctx.attr.load_val,
                get_canonical_repo_name("my_shell"),
                get_repo_root("my_shell"),
                get_canonical_repo_name(""),
                get_repo_root(""),
                get_repo_root("w"),
            ]),
            out.path,
        ],
    )
    return [DefaultInfo(files = depset([out]))]

probe_rule = rule(
    implementation = _probe_impl,
    attrs = {
        "load_val": attr.string(),
        "ext_src": attr.label(allow_single_file = True, mandatory = True),
    },
)
"#,
    );
    w.write(
        "p/BUILD",
        r#"
load("//:blazel_utils.bzl", "get_canonical_repo_name", "get_repo_root", "probe_rule")

probe_rule(
    name = "probe",
    load_val = get_canonical_repo_name("my_shell") + ":" + get_repo_root("my_shell") + ":" + get_repo_root(""),
    ext_src = "@bazel_tools//tools/genrule:genrule-setup.sh",
)
"#,
    );

    // 1. Default layout: external/<canonical>, empty for main repo, is_sibling_repository_layout() == False.
    let b1 = w.run(&["build", "//p:probe"]);
    assert_eq!(b1.code, 0, "{}", b1.err);
    let out_path = w.ws.join("bazel-bin/p/probe.txt");
    let content1 = std::fs::read_to_string(&out_path).unwrap();
    assert_eq!(
        content1,
        "False|external/bazel_tools/tools/genrule/genrule-setup.sh|rules_shell+:external/rules_shell+:|rules_shell+|external/rules_shell+|||"
    );

    // 2. Toggle --experimental_sibling_repository_layout on the SAME daemon instance:
    //    must re-evaluate package loading, configuration.is_sibling_repository_layout(),
    //    external source artifact exec paths, and sibling repo links to ../<canonical>, with no
    //    bogus deprecation warning.
    let b2 = w.run(&[
        "build",
        "--experimental_sibling_repository_layout",
        "//p:probe",
    ]);
    assert_eq!(b2.code, 0, "{}", b2.err);
    assert!(
        !b2.err.contains("experimental_sibling_repository_layout"),
        "unexpected warning: {}",
        b2.err
    );
    let content2 = std::fs::read_to_string(&out_path).unwrap();
    assert_eq!(
        content2,
        "True|../bazel_tools/tools/genrule/genrule-setup.sh|rules_shell+:../rules_shell+:|rules_shell+|../rules_shell+|||"
    );

    // 3. `cquery --output=starlark` respects --experimental_sibling_repository_layout too.
    let cq_sib = w.run(&[
        "cquery",
        "--experimental_sibling_repository_layout",
        "--output=starlark",
        "--starlark:expr=target.label.workspace_root",
        "@my_shell//shell:sh_binary.bzl",
    ]);
    assert_eq!(cq_sib.code, 0, "{}", cq_sib.err);
    assert_eq!(cq_sib.out.trim(), "../rules_shell+");

    let cq_def = w.run(&[
        "cquery",
        "--output=starlark",
        "--starlark:expr=target.label.workspace_root",
        "@my_shell//shell:sh_binary.bzl",
    ]);
    assert_eq!(cq_def.code, 0, "{}", cq_def.err);
    assert_eq!(cq_def.out.trim(), "external/rules_shell+");

    // `cquery --output=files` and `aquery` also reflect --experimental_sibling_repository_layout.
    let cqf_sib = w.run(&[
        "cquery",
        "--experimental_sibling_repository_layout",
        "--output=files",
        "@my_shell//shell:sh_binary.bzl",
    ]);
    assert_eq!(cqf_sib.code, 0, "{}", cqf_sib.err);
    assert_eq!(cqf_sib.out.trim(), "../rules_shell+/shell/sh_binary.bzl");

    let cqf_def = w.run(&["cquery", "--output=files", "@my_shell//shell:sh_binary.bzl"]);
    assert_eq!(cqf_def.code, 0, "{}", cqf_def.err);
    assert_eq!(
        cqf_def.out.trim(),
        "external/rules_shell+/shell/sh_binary.bzl"
    );

    let aq_sib = w.run(&[
        "aquery",
        "--experimental_sibling_repository_layout",
        "//p:probe",
    ]);
    assert_eq!(aq_sib.code, 0, "{}", aq_sib.err);
    assert!(
        aq_sib
            .out
            .contains("../bazel_tools/tools/genrule/genrule-setup.sh"),
        "expected sibling external input in aquery output:\n{}",
        aq_sib.out
    );
    assert!(
        !aq_sib
            .out
            .contains("external/bazel_tools/tools/genrule/genrule-setup.sh"),
        "unexpected legacy external input in aquery output:\n{}",
        aq_sib.out
    );

    // 4. Unmapped `platforms` (absent from root MODULE.bazel) and typo `my_shel` must fail with
    //    invalid Label errors rather than returning `[unknown repo ...]`.
    w.write(
        "bad_plat/BUILD",
        "load('//:blazel_utils.bzl', 'get_repo_root')\nx = get_repo_root('platforms')\n",
    );
    let bad1 = w.run(&["build", "//bad_plat:all"]);
    assert_ne!(bad1.code, 0);
    assert!(
        bad1.err.contains(
            "'workspace_root' is not allowed on invalid Label @@[unknown repo 'platforms' requested from @@]//:unused"
        ),
        "{}",
        bad1.err
    );

    w.write(
        "bad_typo/BUILD",
        "load('//:blazel_utils.bzl', 'get_canonical_repo_name')\nx = get_canonical_repo_name('my_shel')\n",
    );
    let bad2 = w.run(&["build", "//bad_typo:all"]);
    assert_ne!(bad2.code, 0);
    assert!(
        bad2.err.contains(
            "'workspace_name' is not allowed on invalid Label @@[unknown repo 'my_shel' requested from @@ (did you mean 'my_shell'?)]//:unused"
        ),
        "{}",
        bad2.err
    );
}
