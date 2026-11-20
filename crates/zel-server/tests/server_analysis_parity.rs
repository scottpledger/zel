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

#[test]
fn build_info_translation_reads_execution_time_status_and_preserves_constant_metadata() {
    let w = Ws::new("build-info");
    let status_script = if cfg!(windows) {
        w.write(
            "status.bat",
            "@echo off\r\necho STABLE_CUSTOM v1.2.3 with spaces\r\necho BUILD_SCM_REVISION deadbeef\r\necho BUILD_SCM_STATUS clean\r\n",
        );
        "status.bat"
    } else {
        w.write(
            "status.sh",
            "#!/bin/sh\necho 'STABLE_CUSTOM v1.2.3 with spaces'\necho 'BUILD_SCM_REVISION deadbeef'\necho 'BUILD_SCM_STATUS clean'\n",
        );
        "status.sh"
    };
    w.write("tmpl_v.txt", "ts={TS};date={DATE};rev={REV}\n");
    w.write("tmpl_s.txt", "label={LBL};custom={CUSTOM}\n");
    let combine_snippet = if cfg!(windows) {
        let comspec = std::env::var("ComSpec")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| r"C:\Windows\System32\cmd.exe".to_string())
            .replace('\\', "/");
        format!(
            r#"    ctx.actions.run(
        executable = "{comspec}",
        arguments = ["/d", "/s", "/c", "copy /y /b %s+%s %s >nul" % (vf.path.replace("/", "\\"), sf.path.replace("/", "\\"), combined.path.replace("/", "\\"))],
        inputs = [vf, sf],
        outputs = [combined],
        mnemonic = "CombineBuildInfo",
    )"#
        )
    } else {
        r#"    ctx.actions.run_shell(
        inputs = [vf, sf],
        outputs = [combined],
        command = "cat '%s' '%s' > '%s'" % (vf.path, sf.path, combined.path),
        mnemonic = "CombineBuildInfo",
    )"#
        .to_string()
    };
    w.write(
        "defs.bzl",
        &format!(
            r#"
def _tr_v(d):
    return {{
        "{{TS}}": d.get("BUILD_TIMESTAMP", "?"),
        "{{DATE}}": d.get("FORMATTED_DATE", "?"),
        "{{REV}}": d.get("BUILD_SCM_REVISION", "?"),
    }}

def _tr_s(d):
    return {{
        "{{LBL}}": d.get("BUILD_EMBED_LABEL", "?"),
        "{{CUSTOM}}": d.get("STABLE_CUSTOM", "?"),
    }}

def _impl(ctx):
    vf = ctx.actions.transform_version_file(
        transform_func = _tr_v,
        template = ctx.file.tv,
        output_file_name = "volatile_out.txt",
    )
    sf = ctx.actions.transform_info_file(
        transform_func = _tr_s,
        template = ctx.file.ts,
        output_file_name = "stable_out.txt",
    )
    combined = ctx.actions.declare_file("combined.txt")
{combine_snippet}
    return [DefaultInfo(files = depset([vf, sf, combined]))]

stamp_probe = rule(
    implementation = _impl,
    attrs = {{
        "tv": attr.label(default = ":tmpl_v.txt", allow_single_file = True),
        "ts": attr.label(default = ":tmpl_s.txt", allow_single_file = True),
    }},
)

def _cc_java_probe_impl(ctx):
    cc_p = ctx.attr.cc_bi[OutputGroupInfo]
    java_p = ctx.attr.java_bi[OutputGroupInfo]
    files = depset(transitive = [cc_p.non_redacted_build_info_files, java_p.non_redacted_build_info_files])
    return [DefaultInfo(files = files)]

cc_java_probe = rule(
    implementation = _cc_java_probe_impl,
    attrs = {{
        "cc_bi": attr.label(default = "@bazel_tools//tools/build_defs/build_info:cc_build_info"),
        "java_bi": attr.label(default = "@bazel_tools//tools/build_defs/build_info:java_build_info"),
    }},
)
"#
        ),
    );
    w.write(
        "BUILD",
        r#"
load(":defs.bzl", "stamp_probe", "cc_java_probe")
stamp_probe(name = "probe")
cc_java_probe(name = "builtin_bi")
"#,
    );

    let status_flag = format!(
        "--workspace_status_command={}",
        PathFragment::create(w.ws.join(status_script).to_str().unwrap())
    );

    let (code1, _, err1, _) = w.run(&[
        "build",
        "--stamp",
        "--embed_label=rel-9.3.2",
        "--client_env=SOURCE_DATE_EPOCH=1700000000",
        &status_flag,
        "//:probe",
        "//:builtin_bi",
    ]);
    assert_eq!(code1, 0, "{err1}");

    let execroot = w.rt.dirs.execroot();
    let find_out = |name: &str| -> PathBuf {
        fn walk(dir: &std::path::Path, name: &str) -> Option<PathBuf> {
            for entry in std::fs::read_dir(dir).ok()?.flatten() {
                let p = entry.path();
                if p.file_name().is_some_and(|n| n == name) && p.is_file() {
                    return Some(p);
                }
                if p.is_dir() {
                    if let Some(found) = walk(&p, name) {
                        return Some(found);
                    }
                }
            }
            None
        }
        walk(std::path::Path::new(execroot.path_string()), name)
            .unwrap_or_else(|| panic!("missing output {name}"))
    };

    let v1 = std::fs::read_to_string(find_out("volatile_out.txt")).unwrap();
    let s1 = std::fs::read_to_string(find_out("stable_out.txt")).unwrap();
    let c1 = std::fs::read_to_string(find_out("combined.txt")).unwrap();
    assert_eq!(
        v1,
        "ts=1700000000;date=2023 Nov 14 22 13 20 Tue;rev=deadbeef\n"
    );
    assert_eq!(s1, "label=rel-9.3.2;custom=v1.2.3 with spaces\n");
    assert_eq!(c1, format!("{v1}{s1}"));

    let java_bi_props = std::fs::read_to_string(find_out("non_volatile_file.properties")).unwrap();
    assert!(
        java_bi_props.contains("build.label=rel-9.3.2"),
        "{java_bi_props}"
    );
    let cc_vol_hdr = std::fs::read_to_string(find_out("volatile_file.h")).unwrap();
    assert!(cc_vol_hdr.contains("1700000000"), "{cc_vol_hdr}");
    assert!(cc_vol_hdr.contains("deadbeef"), "{cc_vol_hdr}");

    // Rebuild with only volatile timestamp changed: `volatile_out.txt` updates unconditionally,
    // while `CombineBuildInfo` stays cached because `volatile_out.txt` has constant metadata.
    let explain_path = PathBuf::from(
        PathFragment::create(w.root.join("explain2.txt").to_str().unwrap()).path_string(),
    );
    let explain_flag = format!("--explain={}", explain_path.display());
    let (code2, _, err2, _) = w.run(&[
        "build",
        "--stamp",
        "--embed_label=rel-9.3.2",
        "--client_env=SOURCE_DATE_EPOCH=1700000500",
        &status_flag,
        &explain_flag,
        "//:probe",
    ]);
    assert_eq!(code2, 0, "{err2}");
    let v2 = std::fs::read_to_string(find_out("volatile_out.txt")).unwrap();
    let c2 = std::fs::read_to_string(find_out("combined.txt")).unwrap();
    assert!(v2.contains("ts=1700000500;"), "{v2}");
    assert!(
        c2.contains("ts=1700000000;"),
        "downstream action must not be invalidated solely by volatile constant-metadata change: {c2}"
    );
    let explain2 = std::fs::read_to_string(&explain_path).unwrap_or_default();
    assert!(!explain2.contains("CombineBuildInfo"), "{explain2}");

    // Rebuild with stable `--embed_label` changed (also exercising `~/` expansion via `--client_env=HOME`):
    // stable file changes, invalidating `CombineBuildInfo`, which then consumes both the new stable
    // output and the updated volatile output.
    let home_root_flag = format!(
        "--client_env=HOME={}",
        PathFragment::create(w.root.to_str().unwrap()).path_string()
    );
    let tilde_status_flag = format!("--workspace_status_command=~/ws/{status_script}");
    let (code3, _, err3, _) = w.run(&[
        "build",
        "--stamp",
        "--embed_label=rel-9.3.3",
        "--client_env=SOURCE_DATE_EPOCH=1700000900",
        &home_root_flag,
        &tilde_status_flag,
        "//:probe",
    ]);
    assert_eq!(code3, 0, "{err3}");
    let c3 = std::fs::read_to_string(find_out("combined.txt")).unwrap();
    assert!(c3.contains("ts=1700000900;"), "{c3}");
    assert!(c3.contains("label=rel-9.3.3;"), "{c3}");
}

#[test]
fn launcher_flag_alias_forwards_cc_and_default_providers_for_target_and_exec() {
    let w = Ws::new("java-launcher-alias");
    let rc_dir = w.root.join("rules_cc");
    std::fs::create_dir_all(rc_dir.join("cc/common")).unwrap();
    std::fs::write(
        rc_dir.join("MODULE.bazel"),
        "module(name = \"rules_cc\", version = \"0.2.25\")\n",
    )
    .unwrap();
    std::fs::write(rc_dir.join("cc/common/BUILD"), "").unwrap();
    std::fs::write(
        rc_dir.join("cc/common/cc_info.bzl"),
        "CcInfo = provider(fields = [\"tag\"])\n",
    )
    .unwrap();
    std::fs::write(
        rc_dir.join("cc/common/cc_common.bzl"),
        "CcLauncherInfo = provider(fields = [\"tag\"])\ncc_common = struct(launcher_provider = CcLauncherInfo)\n",
    )
    .unwrap();
    let rc_dir_starlark = PathFragment::create(rc_dir.to_str().unwrap())
        .path_string()
        .to_string();
    w.write(
        "MODULE.bazel",
        &format!(
            "module(name = \"w\")\nbazel_dep(name = \"rules_cc\", version = \"0.2.25\")\nlocal_path_override(module_name = \"rules_cc\", path = \"{rc_dir_starlark}\")\n"
        ),
    );
    w.write("data_t.txt", "target-data\n");
    w.write("data_h.txt", "host-data\n");
    w.write(
        "defs.bzl",
        r#"
load("@rules_cc//cc/common:cc_common.bzl", "cc_common")
load("@rules_cc//cc/common:cc_info.bzl", "CcInfo")

CcLauncherInfo = cc_common.launcher_provider

def _fake_launcher_impl(ctx):
    exe = ctx.actions.declare_file(ctx.label.name + ".bin")
    ctx.actions.write(exe, ctx.attr.tag, is_executable = True)
    rf = ctx.runfiles(files = ctx.files.data)
    return [
        DefaultInfo(executable = exe, files = depset([exe]), runfiles = rf),
        CcInfo(tag = ctx.attr.tag),
        CcLauncherInfo(tag = ctx.attr.tag + "_LI"),
    ]

fake_launcher = rule(
    implementation = _fake_launcher_impl,
    executable = True,
    attrs = {
        "tag": attr.string(mandatory = True),
        "data": attr.label_list(allow_files = True),
    },
)

def _consumer_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".summary")
    t_has_cc = CcInfo in ctx.attr.target_alias
    t_has_li = CcLauncherInfo in ctx.attr.target_alias
    h_has_cc = CcInfo in ctx.attr.exec_alias
    h_has_li = CcLauncherInfo in ctx.attr.exec_alias
    t_tag = ctx.attr.target_alias[CcInfo].tag if t_has_cc else ""
    t_li = ctx.attr.target_alias[CcLauncherInfo].tag if t_has_li else ""
    h_tag = ctx.attr.exec_alias[CcInfo].tag if h_has_cc else ""
    h_li = ctx.attr.exec_alias[CcLauncherInfo].tag if h_has_li else ""
    t_files = [f.basename for f in ctx.attr.target_alias[DefaultInfo].files.to_list()]
    h_files = [f.basename for f in ctx.attr.exec_alias[DefaultInfo].files.to_list()]
    t_rf = [f.basename for f in ctx.attr.target_alias[DefaultInfo].default_runfiles.files.to_list()]
    h_rf = [f.basename for f in ctx.attr.exec_alias[DefaultInfo].default_runfiles.files.to_list()]
    ctx.actions.write(
        out,
        "t=%s,%s,%s,%s,%s,%s\nh=%s,%s,%s,%s,%s,%s\n" % (
            t_has_cc, t_has_li, t_tag, t_li, t_files, t_rf,
            h_has_cc, h_has_li, h_tag, h_li, h_files, h_rf,
        ),
    )
    return [DefaultInfo(files = depset([out]))]

consumer = rule(
    implementation = _consumer_impl,
    attrs = {
        "target_alias": attr.label(default = ":alias"),
        "exec_alias": attr.label(default = ":alias", cfg = "exec"),
    },
)
"#,
    );
    w.write(
        "BUILD",
        r#"
load("@bazel_tools//tools/jdk:launcher_flag_alias.bzl", "launcher_flag_alias")
load(":defs.bzl", "fake_launcher", "consumer")
launcher_flag_alias(name = "alias")
fake_launcher(name = "tgt_launcher", tag = "TGT_L", data = ["data_t.txt"])
fake_launcher(name = "exec_launcher", tag = "EXEC_L", data = ["data_h.txt"])
consumer(name = "check")
"#,
    );

    // 1. Unset flags: `launcher_flag_alias` returns empty DefaultInfo without CcInfo/CcLauncherInfo.
    let (code0, _, err0, _) = w.run(&["build", "//:check"]);
    assert_eq!(code0, 0, "{err0}");

    let execroot = w.rt.dirs.execroot();
    let find_summary = || -> String {
        fn walk(dir: &std::path::Path) -> Option<PathBuf> {
            for entry in std::fs::read_dir(dir).ok()?.flatten() {
                let p = entry.path();
                if p.file_name().is_some_and(|n| n == "check.summary") && p.is_file() {
                    return Some(p);
                }
                if p.is_dir() {
                    if let Some(found) = walk(&p) {
                        return Some(found);
                    }
                }
            }
            None
        }
        let p = walk(std::path::Path::new(execroot.path_string())).expect("check.summary");
        std::fs::read_to_string(p).unwrap()
    };
    let s0 = find_summary();
    assert_eq!(s0, "t=False,False,,,[],[]\nh=False,False,,,[],[]\n");

    // 2. `--java_launcher` (relative `:tgt_launcher`) and `--host_java_launcher` (`//:exec_launcher`):
    // target alias forwards `tgt_launcher`, exec alias forwards `exec_launcher`.
    let (code1, _, err1, _) = w.run(&[
        "build",
        "--java_launcher=:tgt_launcher",
        "--host_java_launcher=//:exec_launcher",
        "//:check",
    ]);
    assert_eq!(code1, 0, "{err1}");
    let s1 = find_summary();
    assert_eq!(
        s1,
        "t=True,True,TGT_L,TGT_L_LI,[\"tgt_launcher.bin\"],[\"tgt_launcher.bin\", \"data_t.txt\"]\nh=True,True,EXEC_L,EXEC_L_LI,[\"exec_launcher.bin\"],[\"exec_launcher.bin\", \"data_h.txt\"]\n"
    );
}

#[test]
fn wave5_coverage_build_tags_invocation_id_bep_residue_and_workspace_status() {
    let w = Ws::new("w5b");
    let bin_dir = w.root.join(format!(
        "ob/execroot/_main/bazel-out/{}-fastbuild/bin",
        zel_analysis::build_options::auto_host_cpu()
    ));
    w.write(
        "defs.bzl",
        r##"
def _cov_check(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".info")
    # Also verify allow_unresolved_symlinks defaults to True on attr.label_list:
    ctx.actions.write(
        out,
        "cov=%s\n" % ctx.coverage_instrumented(),
    )
    return [DefaultInfo(files = depset([out]))]

cov_rule = rule(
    implementation = _cov_check,
    attrs = {"deps": attr.label_list(allow_files = True)},
)

def _sym(ctx):
    lnk = ctx.actions.declare_symlink(ctx.label.name + ".lnk")
    ctx.actions.symlink(output = lnk, target_path = "dangling/target")
    return [DefaultInfo(files = depset([lnk]))]

sym_rule = rule(implementation = _sym)

def _write_out(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".out")
    ctx.actions.write(out, ctx.attr.val + "\n")
    return [DefaultInfo(files = depset([out]))]

write_rule = rule(
    implementation = _write_out,
    attrs = {"val": attr.string(default = "x")},
)

def _bin(ctx):
    exe = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.write(exe, "#!/bin/sh\nexit 0\n", is_executable = True)
    rf = ctx.runfiles(files = ctx.files.data)
    return [DefaultInfo(executable = exe, files = depset([exe]), runfiles = rf)]

my_bin = rule(implementation = _bin, executable = True, attrs = {"data": attr.label_list(allow_files = True)})
my_test = rule(implementation = _bin, test = True, attrs = {"data": attr.label_list(allow_files = True)})
"##,
    );
    w.write(
        "pkg/BUILD",
        r#"
load("//:defs.bzl", "cov_rule", "sym_rule", "write_rule", "my_bin", "my_test")
sym_rule(name = "unresolved")
cov_rule(name = "c", deps = [":unresolved"])
write_rule(name = "g_a", val = "a", tags = ["alpha"])
my_test(name = "g_b", local = True)
my_bin(name = "runner")
my_test(name = "my_sh_test", data = ["d.txt"])
"#,
    );
    w.write("pkg/runner.sh", "#!/bin/sh\nexit 0\n");
    w.write("pkg/d.txt", "hello");
    w.write("status.sh", "#!/bin/sh\necho STABLE_FOO $MY_STATUS_VAR\n");

    // 1. allow_unresolved_symlinks defaults to True + coverage_instrumented honors default filter.
    let (code, _, err, _) = w.run(&["coverage", "//pkg:c"]);
    assert_eq!(code, 4, "{err}");
    assert!(
        err.contains("Using default value for --instrumentation_filter: \"^//pkg[/:]\"."),
        "{err}"
    );
    let cov_txt = std::fs::read_to_string(bin_dir.join("pkg/c.info")).unwrap();
    assert_eq!(cov_txt.trim(), "cov=True");

    // 2. --build_tag_filters uses OR for positive filters and recognizes `local = True`/`1` as `local` tag.
    let (code, _, err, _) = w.run(&[
        "build",
        "--build_tag_filters=alpha,local",
        "//pkg:g_a",
        "//pkg:g_b",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(bin_dir.join("pkg/g_a.out").exists());
    assert!(bin_dir.join("pkg/g_b").exists());

    // 3. --invocation_id honored in BEP & command-<uuid>.profile.gz symlinked by command.profile.gz,
    //    and `bazel build` materializes .runfiles for sh_test when --build_runfile_links=true.
    let bep_json = w.root.join("run_bep.json");
    let custom_inv = "11111111-2222-4333-8444-555555555555";
    let (code, _, err, _) = w.run(&[
        "build",
        &format!("--invocation_id={custom_inv}"),
        "//pkg:my_sh_test",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(
        w.root
            .join(format!("ob/command-{custom_inv}.profile.gz"))
            .exists(),
        "per-invocation profile should exist"
    );
    assert!(w.root.join("ob/command.profile.gz").exists());
    assert!(
        bin_dir.join("pkg/my_sh_test.runfiles").is_dir(),
        "bazel build should materialize test runfiles tree"
    );

    // 4. `bazel run` BEP redacts post-`--` arguments to REDACTED by default.
    let (code, _, err, req) = w.run(&[
        "run",
        &format!("--build_event_json_file={}", bep_json.display()),
        "//pkg:runner",
        "--",
        "--secret_token=xyz",
        "arg2",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(req.is_some());
    let bep_str = std::fs::read_to_string(&bep_json).unwrap();
    assert!(bep_str.contains("REDACTED"), "{bep_str}");
    assert!(!bep_str.contains("secret_token"), "{bep_str}");

    // 5. workspace_status_command receives --client_env and honors SOURCE_DATE_EPOCH without
    //    clobbering unchanged stable-status.txt (including absolute paths containing spaces and
    //    platform separators).
    w.write(
        "status dir/status.sh",
        "#!/bin/sh\necho STABLE_FOO $MY_STATUS_VAR\n",
    );
    let status_cmd = if cfg!(windows) {
        w.write(
            "status dir/status.bat",
            "@echo off\r\necho STABLE_FOO %MY_STATUS_VAR%\r\n",
        );
        w.ws.join("status dir")
            .join("status.bat")
            .display()
            .to_string()
    } else {
        w.ws.join("status dir")
            .join("status.sh")
            .display()
            .to_string()
    };
    let (code, _, err, _) = w.run(&[
        "build",
        "--stamp",
        &format!("--workspace_status_command={status_cmd}"),
        "--client_env=MY_STATUS_VAR=from_client",
        "--client_env=SOURCE_DATE_EPOCH=1700000000",
        "//pkg:g_a",
    ]);
    assert_eq!(code, 0, "{err}");
    let stable =
        std::fs::read_to_string(w.root.join("ob/execroot/_main/bazel-out/stable-status.txt"))
            .unwrap();
    assert!(stable.contains("STABLE_FOO from_client"), "{stable}");
    let volatile = std::fs::read_to_string(
        w.root
            .join("ob/execroot/_main/bazel-out/volatile-status.txt"),
    )
    .unwrap();
    assert!(
        volatile.contains("BUILD_TIMESTAMP 1700000000"),
        "{volatile}"
    );

    #[cfg(windows)]
    {
        let has_git_bash = [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files\Git\usr\bin\bash.exe",
            r"C:\msys64\usr\bin\bash.exe",
        ]
        .iter()
        .any(|p| std::path::Path::new(p).is_file());
        if has_git_bash {
            let win_sh_cmd =
                w.ws.join("status dir")
                    .join("status.sh")
                    .display()
                    .to_string();
            let (code_sh, _, err_sh, _) = w.run(&[
                "build",
                "--stamp",
                &format!("--workspace_status_command={win_sh_cmd}"),
                "--client_env=MY_STATUS_VAR=from_win_sh",
                "--client_env=SOURCE_DATE_EPOCH=1700000000",
                "//pkg:g_a",
            ]);
            assert_eq!(code_sh, 0, "{err_sh}");
            let stable_sh = std::fs::read_to_string(
                w.root.join("ob/execroot/_main/bazel-out/stable-status.txt"),
            )
            .unwrap();
            assert!(stable_sh.contains("STABLE_FOO from_win_sh"), "{stable_sh}");
        }
    }

    // 6. --query_file decodes ISO-8859-1 bytes cleanly.
    let qfile = w.root.join("latin1.q");
    std::fs::write(&qfile, b"//pkg:g_a\n").unwrap();
    let (code, out, err, _) = w.run(&["query", &format!("--query_file={}", qfile.display())]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.trim(), "//pkg:g_a");
}

#[test]
fn actions_write_expands_tree_artifact_args_via_in_memory_fs() {
    use zel_analysis::BuildConfigurationKey;
    use zel_cmdline::Label;
    use zel_events::Reporter;
    use zel_exec::{execute_action_with, CompactPersistentActionCache, ExecOptions};
    use zel_packages::{LoaderConfig, PackageLoader, SimpleRepoResolver};
    use zel_rules::{AnalysisConfig, TargetResolver};
    use zel_vfs::InMemoryFileSystem;

    let fs = Arc::new(InMemoryFileSystem::default());
    let ws = PathFragment::create("/virtual/ws");
    fs.create_directory_and_parents(&ws).unwrap();
    fs.write_file(
        &ws.get_relative_str("MODULE.bazel"),
        b"module(name = \"m\")\n",
    )
    .unwrap();
    fs.write_file(
        &ws.get_relative_str("defs.bzl"),
        br#"
def _impl(ctx):
    tree = ctx.actions.declare_directory(ctx.label.name + "_tree")
    ctx.actions.run_shell(
        outputs = [tree],
        command = "mkdir -p %s" % tree.path,
    )
    out_shell = ctx.actions.declare_file(ctx.label.name + ".shell.params")
    a1 = ctx.actions.args()
    a1.add_all([tree])
    ctx.actions.write(out_shell, a1)

    out_flags = ctx.actions.declare_file(ctx.label.name + ".flags.params")
    a2 = ctx.actions.args()
    a2.set_param_file_format("flag_per_line")
    a2.add_all("--srcs", [tree])
    ctx.actions.write(out_flags, a2)
    return [DefaultInfo(files = depset([out_shell, out_flags]))]

tree_writer = rule(implementation = _impl)
"#,
    )
    .unwrap();
    fs.write_file(
        &ws.get_relative_str("BUILD"),
        b"load(\":defs.bzl\", \"tree_writer\")\ntree_writer(name = \"tw\")\n",
    )
    .unwrap();

    let lc = LoaderConfig::new(
        fs.clone() as Arc<dyn FileSystem>,
        Arc::new(SimpleRepoResolver::new(ws)),
    );
    let resolver = TargetResolver::new(PackageLoader::new(lc), AnalysisConfig::new());
    let cfg = BuildConfigurationKey::target("k8", "fastbuild");
    let ct = resolver
        .analyze(&Label::parse_canonical("//:tw").unwrap(), &cfg)
        .unwrap();

    // Populate the tree artifact strictly inside `exec_fs` (never touching host disk).
    let exec_fs = InMemoryFileSystem::default();
    let execroot = PathFragment::create("/virtual/execroot/_main");
    let tree_art = ct
        .typed_actions
        .iter()
        .flat_map(|a| a.outputs.iter())
        .find(|o| o.is_tree_artifact)
        .unwrap()
        .clone();
    let tree_dir = execroot.get_relative(&tree_art.exec_path);
    exec_fs
        .create_directory_and_parents(&tree_dir.get_relative_str("sub"))
        .unwrap();
    exec_fs
        .write_file(&tree_dir.get_relative_str("z.cc"), b"int z;")
        .unwrap();
    exec_fs
        .write_file(&tree_dir.get_relative_str("sub/a space.cc"), b"int a;")
        .unwrap();

    let cache = CompactPersistentActionCache::new();
    let reporter = Reporter::new();
    let opts = ExecOptions::default();
    for act in &ct.actions {
        if act.mnemonic() == "FileWrite" {
            let res =
                execute_action_with(act, &execroot, &exec_fs, &cache, &reporter, &opts).unwrap();
            assert!(res.succeeded(), "{:?}", res.failure);
        }
    }

    let tree_prefix = tree_art.exec_path.path_string();
    let shell_out = ct
        .default_info
        .files
        .iter()
        .find(|f| f.exec_path.path_string().ends_with(".shell.params"))
        .unwrap();
    let flags_out = ct
        .default_info
        .files
        .iter()
        .find(|f| f.exec_path.path_string().ends_with(".flags.params"))
        .unwrap();

    let shell_text = String::from_utf8(
        exec_fs
            .read_file(&execroot.get_relative(&shell_out.exec_path))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        shell_text,
        format!("'{tree_prefix}/sub/a space.cc'\n{tree_prefix}/z.cc\n")
    );

    let flags_text = String::from_utf8(
        exec_fs
            .read_file(&execroot.get_relative(&flags_out.exec_path))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        flags_text,
        format!("--srcs={tree_prefix}/sub/a space.cc {tree_prefix}/z.cc\n")
    );
}

#[test]
fn flag_per_line_spilled_param_file_expands_tree_artifacts_end_to_end() {
    use zel_analysis::BuildConfigurationKey;
    use zel_cmdline::Label;
    use zel_events::Reporter;
    use zel_exec::{execute_action_with, CompactPersistentActionCache, ExecOptions};
    use zel_packages::{LoaderConfig, PackageLoader, SimpleRepoResolver};
    use zel_rules::{AnalysisConfig, TargetResolver};
    use zel_vfs::InMemoryFileSystem;

    let fs = Arc::new(InMemoryFileSystem::default());
    let ws = PathFragment::create("/virtual/ws");
    fs.create_directory_and_parents(&ws).unwrap();
    fs.write_file(
        &ws.get_relative_str("MODULE.bazel"),
        b"module(name = \"m\")\n",
    )
    .unwrap();
    fs.write_file(
        &ws.get_relative_str("defs.bzl"),
        br##"
def _impl(ctx):
    tree_before = ctx.actions.declare_directory("tree_before")
    tree_fmt = ctx.actions.declare_directory("tree_fmt")
    tree_pos_single = ctx.actions.declare_directory("tree_pos_single")
    tree_pos_multi = ctx.actions.declare_directory("tree_pos_multi")
    for d in [tree_before, tree_fmt, tree_pos_single, tree_pos_multi]:
        ctx.actions.run_shell(outputs = [d], command = "mkdir -p " + d.path)

    tool = ctx.actions.declare_file("tool.sh")
    ctx.actions.write(tool, "#!/bin/sh\n", is_executable = True)
    out = ctx.actions.declare_file("result.out")

    args = ctx.actions.args()
    args.set_param_file_format("flag_per_line")
    args.use_param_file("@%s", use_always = True)
    args.add("--header", "val")
    args.add("static_pos")
    args.add_all([tree_before], before_each = "--input")
    args.add_all([tree_fmt], format_each = "--input=%s")
    args.add_all([tree_pos_single])
    args.add_all([tree_pos_multi])

    ctx.actions.run(
        executable = tool,
        inputs = [tree_before, tree_fmt, tree_pos_single, tree_pos_multi],
        outputs = [out],
        arguments = [args],
    )
    return [DefaultInfo(files = depset([out]))]

tree_flag_rule = rule(implementation = _impl)
"##,
    )
    .unwrap();
    fs.write_file(
        &ws.get_relative_str("BUILD"),
        b"load(\":defs.bzl\", \"tree_flag_rule\")\ntree_flag_rule(name = \"tf\")\n",
    )
    .unwrap();

    let lc = LoaderConfig::new(
        fs.clone() as Arc<dyn FileSystem>,
        Arc::new(SimpleRepoResolver::new(ws)),
    );
    let resolver = TargetResolver::new(PackageLoader::new(lc), AnalysisConfig::new());
    let cfg = BuildConfigurationKey::target("k8", "fastbuild");
    let ct = resolver
        .analyze(&Label::parse_canonical("//:tf").unwrap(), &cfg)
        .unwrap();

    let exec_fs = InMemoryFileSystem::default();
    let execroot = PathFragment::create("/virtual/execroot/_main");
    let find_tree = |suffix: &str| {
        ct.typed_actions
            .iter()
            .flat_map(|a| a.outputs.iter())
            .find(|o| o.is_tree_artifact && o.exec_path.path_string().ends_with(suffix))
            .unwrap()
            .exec_path
            .clone()
    };
    let tb = find_tree("/tree_before");
    let tf = find_tree("/tree_fmt");
    let tps = find_tree("/tree_pos_single");
    let tpm = find_tree("/tree_pos_multi");
    for t in [&tb, &tf, &tps, &tpm] {
        exec_fs
            .create_directory_and_parents(&execroot.get_relative(t))
            .unwrap();
    }

    exec_fs
        .write_file(&execroot.get_relative(&tb).get_relative_str("a.in"), b"a")
        .unwrap();
    exec_fs
        .write_file(&execroot.get_relative(&tb).get_relative_str("b.in"), b"b")
        .unwrap();
    exec_fs
        .write_file(&execroot.get_relative(&tf).get_relative_str("c.in"), b"c")
        .unwrap();
    exec_fs
        .write_file(&execroot.get_relative(&tf).get_relative_str("d.in"), b"d")
        .unwrap();
    exec_fs
        .write_file(
            &execroot.get_relative(&tps).get_relative_str("only.in"),
            b"o",
        )
        .unwrap();
    exec_fs
        .write_file(
            &execroot.get_relative(&tpm).get_relative_str("first.in"),
            b"1",
        )
        .unwrap();
    exec_fs
        .write_file(
            &execroot.get_relative(&tpm).get_relative_str("second.in"),
            b"2",
        )
        .unwrap();

    let cache = CompactPersistentActionCache::new();
    let reporter = Reporter::new();
    let opts = ExecOptions::default();
    for act in &ct.actions {
        if act.mnemonic() == "FileWrite" {
            let res =
                execute_action_with(act, &execroot, &exec_fs, &cache, &reporter, &opts).unwrap();
            assert!(res.succeeded(), "{:?}", res.failure);
        }
    }

    let out_art = ct
        .default_info
        .files
        .iter()
        .find(|f| f.exec_path.path_string().ends_with("/result.out"))
        .unwrap();
    let params_rel = PathFragment::create(&format!("{}-0.params", out_art.exec_path.path_string()));
    let params_text = String::from_utf8(
        exec_fs
            .read_file(&execroot.get_relative(&params_rel))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        params_text,
        format!(
            "--header=val\n--input={tb}/a.in --input {tb}/b.in\n--input={tf}/c.in=--input={tf}/d.in\n",
            tb = tb.path_string(),
            tf = tf.path_string(),
        )
    );

    let spawn_meta = ct
        .actions
        .iter()
        .find(|a| {
            a.outputs()
                .iter()
                .any(|o| o.exec_path.path_string().ends_with("/result.out"))
        })
        .expect("spawn action for result.out");
    let zel_analysis::ActionAnalysisMetadata::SpawnAction { ref argv, .. } = *spawn_meta else {
        panic!("expected SpawnAction");
    };
    let expanded_argv = zel_analysis::expand_tree_artifact_args(argv, &|dir| {
        let root = execroot.get_relative(&PathFragment::create(dir));
        let mut entries: Vec<String> = exec_fs
            .readdir(&root)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|d| d.name)
            .collect();
        entries.sort();
        Ok(entries
            .into_iter()
            .map(|name| format!("{dir}/{name}"))
            .collect())
    })
    .unwrap();
    assert_eq!(
        &expanded_argv[1..],
        &[
            format!("@{}", params_rel.path_string()),
            "static_pos".to_string(),
            format!("{}/only.in", tps.path_string()),
            format!("{tpm}/first.in={tpm}/second.in", tpm = tpm.path_string()),
        ]
    );

    #[cfg(unix)]
    {
        let w = Ws::new("flag-per-line-trees");
        w.write(
            "defs.bzl",
            r##"
def _impl(ctx):
    tree_before = ctx.actions.declare_directory("tree_before")
    tree_fmt = ctx.actions.declare_directory("tree_fmt")
    tree_pos = ctx.actions.declare_directory("tree_pos")
    ctx.actions.run_shell(
        outputs = [tree_before, tree_fmt, tree_pos],
        command = (
            "mkdir -p {tb} {tf} {tp} && "
            + ": > {tb}/a.in && : > {tb}/b.in && "
            + ": > {tf}/c.in && : > {tp}/only.in"
        ).format(tb = tree_before.path, tf = tree_fmt.path, tp = tree_pos.path),
    )

    tool = ctx.actions.declare_file("capture.sh")
    out = ctx.actions.declare_file("captured.txt")
    ctx.actions.write(
        tool,
        "#!/bin/sh\nset -eu\npfile=\"${1#@}\"\n{\n  printf 'ARGV:'\n  printf ' <%s>' \"$@\"\n  printf '\\nPARAMS:\\n'\n  cat \"$pfile\"\n} > " + out.path + "\n",
        is_executable = True,
    )

    args = ctx.actions.args()
    args.set_param_file_format("flag_per_line")
    args.use_param_file("@%s", use_always = True)
    args.add_all([tree_before], before_each = "--input")
    args.add_all([tree_fmt], format_each = "--input=%s")
    args.add_all([tree_pos])

    ctx.actions.run(
        executable = tool,
        inputs = [tree_before, tree_fmt, tree_pos],
        outputs = [out],
        arguments = [args],
    )
    return [DefaultInfo(files = depset([out]))]

capture_rule = rule(implementation = _impl)
"##,
        );
        w.write(
            "BUILD",
            "load(':defs.bzl', 'capture_rule')\ncapture_rule(name = 'cap')\n",
        );
        let (code, _stdout, stderr, _) = w.run(&["build", "//:cap"]);
        assert_eq!(code, 0, "build failed: {stderr}");
        let captured_path = w.ws.join("bazel-bin/captured.txt");
        let captured = std::fs::read_to_string(&captured_path).unwrap();
        assert!(
            captured.contains("/tree_pos/only.in>\nPARAMS:\n"),
            "positional tree path missing from argv: {captured}"
        );
        assert!(
            captured.contains("--input=")
                && captured.contains("/tree_before/a.in --input ")
                && captured.contains("/tree_before/b.in\n"),
            "before_each tree values missing from param file: {captured}"
        );
        assert!(
            captured.contains("/tree_fmt/c.in\n"),
            "format_each tree value missing from param file: {captured}"
        );
    }
}
