//! Bazel's environment semantics end to end through `BlazeRuntime::dispatch_command`: the client
//! environment arrives as `--client_env=NAME=VALUE` arguments (like the C++ client sends it) and
//! must be the only source of inherited variables (never the server process environment), for
//! `--action_env`, `--host_action_env`, `--incompatible_strict_action_env`, `--test_env`,
//! `RunEnvironmentInfo`, `env` / `env_inherit`, `bazel run` (`--run_env`) and the action cache.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use zel_proto::command_server::{ExecRequest, RunRequest};
use zel_server::{BlazeDirectories, BlazeRuntime};
use zel_vfs::{FileSystem, LocalFileSystem, PathFragment};

/// A variable that only exists in this (the "server") process; it must never reach an action.
const SERVER_ONLY: &str = "ZEL_ENV_TEST_SERVER_ONLY";


/// A hermetic local registry built once per test process from the vendored closure under
/// `third_party` (`scripts/make_vendored_registry.py`); no network is ever used.
fn local_registry() -> PathBuf {
    static REG: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    REG.get_or_init(|| {
        let root = env!("CARGO_MANIFEST_DIR").strip_suffix("/crates/zel-server").unwrap_or("/Users/pledger/Development/zel");
        let dir = std::env::temp_dir().join(format!("zel-envsem-registry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let st = std::process::Command::new("python3")
            .arg(format!("{root}/scripts/make_vendored_registry.py"))
            .arg(&dir)
            .status()
            .expect("python3 is required to build the local test registry");
        assert!(st.success());
        dir
    })
    .clone()
}

/// Bazel 9 has no native `sh_*` rules: BUILD files load them from `@rules_shell`.
fn shell_loads(rel: &str, content: &str) -> String {
    if !(rel == "BUILD" || rel.ends_with("/BUILD")) {
        return content.to_string();
    }
    let mut out = String::new();
    for name in ["sh_binary", "sh_test", "sh_library"] {
        if content.contains(&format!("{name}(")) && !content.contains(&format!("\"{name}\")")) {
            out.push_str(&format!("load(\"@rules_shell//shell:{name}.bzl\", \"{name}\")\n"));
        }
    }
    out.push_str(content);
    out
}

struct Ws {
    ws: PathBuf,
    rt: BlazeRuntime,
}

fn ws(name: &str) -> Ws {
    // Safe: set once per process before any command runs (all tests set the same value).
    std::env::set_var(SERVER_ONLY, "leak");
    let root = std::env::temp_dir().join(format!("zel-env-sem-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let ws = root.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let ws = root.join("ws");
    std::fs::write(ws.join("MODULE.bazel"), "module(name = \"w\")\nbazel_dep(name = \"rules_shell\", version = \"0.6.1\")\n").unwrap();
    std::fs::write(ws.join("BUILD"), "").unwrap();
    let fs: Arc<dyn FileSystem> = Arc::new(LocalFileSystem::default());
    let p = |s: &str| PathFragment::create(root.join(s).to_str().unwrap());
    let dirs = BlazeDirectories {
        workspace_dir: PathFragment::create(ws.to_str().unwrap()),
        output_base: p("ob"),
        install_base: p("ib"),
        output_user_root: p("ur"),
    };
    Ws { ws, rt: BlazeRuntime::new(dirs, fs, 1) }
}

struct Out {
    code: i32,
    #[allow(dead_code)]
    out: String,
    err: String,
    exec: Option<ExecRequest>,
}

impl Ws {
    fn write(&self, rel: &str, content: &str) {
        let p = self.ws.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, shell_loads(rel, content)).unwrap();
    }

    /// Runs a command with the given client environment.
    fn run(&self, client_env: &[(&str, &str)], args: &[&str]) -> Out {
        let mut arg: Vec<Vec<u8>> = Vec::new();
        arg.push(args[0].as_bytes().to_vec());
        if matches!(args[0], "build" | "test" | "run" | "coverage" | "query" | "cquery" | "aquery") {
            arg.push(format!("--registry=file://{}", local_registry().display()).into_bytes());
        }
        for (k, v) in client_env {
            arg.push(format!("--client_env={k}={v}").into_bytes());
        }
        arg.extend(args[1..].iter().map(|a| a.as_bytes().to_vec()));
        let req = RunRequest { cookie: "c".into(), arg, client_description: "t".into(), ..Default::default() };
        let r = self.rt.dispatch_command(&req, "r");
        Out {
            code: r.exit_code,
            out: String::from_utf8_lossy(&r.standard_output).into_owned(),
            err: String::from_utf8_lossy(&r.standard_error).into_owned(),
            exec: r.exec_request,
        }
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.ws.join(rel)).unwrap()
    }
}

/// `NAME=value` lines (`<unset>` for variables that are not set) into a map.
fn parse(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("ENV "))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

const NAMES: &str = "PATH LC_CTYPE TZ HOME LD_LIBRARY_PATH FOO BAR BAZ CLIENT_ONLY ZEL_ENV_TEST_SERVER_ONLY \
                     RE_FIXED RE_INHERIT E1 EI RUN1 ACT";

/// Shell snippet printing `ENV NAME=value` for every interesting variable.
fn dump_script() -> String {
    format!(
        "for v in {NAMES}; do eval \"val=\\${{$v-<unset>}}\"; echo \"ENV $v=$val\"; done\n\
         [ \"${{HOME-}}\" = \"${{TEST_TMPDIR-x}}\" ] && echo \"ENV HOME_IS_TEST_TMPDIR=yes\"\n\
         echo \"ENV BUILD_WORKSPACE_DIRECTORY=${{BUILD_WORKSPACE_DIRECTORY-<unset>}}\"\n\
         echo \"ENV RUNFILES_DIR_SET=${{RUNFILES_DIR+yes}}\"\n"
    )
}

const GENRULE_DUMP: &str = r#"
genrule(
    name = "dump",
    outs = ["dump.txt"],
    cmd = "for v in PATH LC_CTYPE HOME LD_LIBRARY_PATH FOO BAR BAZ CLIENT_ONLY ZEL_ENV_TEST_SERVER_ONLY; do echo \"ENV $$v=$${!v-<unset>}\"; done > $@",
)
genrule(
    name = "tool_dump",
    outs = ["tool.txt"],
    cmd = "for v in PATH FOO BAR; do echo \"ENV $$v=$${!v-<unset>}\"; done > $@",
)
genrule(
    name = "user_dump",
    outs = ["user.txt"],
    tools = [":tool_dump"],
    cmd = "(for v in FOO BAR; do echo \"ENV $$v=$${!v-<unset>}\"; done; cat $(location :tool_dump) | sed 's/^ENV /ENV TOOL_/') > $@",
)
"#;

const STRICT_PATH: &str = "/bin:/usr/bin:/usr/local/bin";

#[test]
fn strict_action_env_is_the_default() {
    let w = ws("strict");
    w.write("p/BUILD", GENRULE_DUMP);
    let client = [("FOO", "clientfoo"), ("CLIENT_ONLY", "x"), ("LD_LIBRARY_PATH", "/ld"), ("HOME", "/h"), ("PATH", "/opt/z:/bin")];
    let r = w.run(&client, &["build", "//p:dump"]);
    assert_eq!(r.code, 0, "{}", r.err);
    let e = parse(&w.read("bazel-bin/p/dump.txt"));
    assert_eq!(e["PATH"], STRICT_PATH);
    assert_eq!(e["LC_CTYPE"], "<unset>"); // 9.3.2 sets no LC_CTYPE
    for unset in ["HOME", "LD_LIBRARY_PATH", "FOO", "BAR", "CLIENT_ONLY", SERVER_ONLY] {
        assert_eq!(e[unset], "<unset>", "{unset} leaked: {e:?}");
    }
}

#[test]
fn non_strict_inherits_path_and_ld_library_path_from_the_client() {
    let w = ws("nonstrict");
    w.write("p/BUILD", GENRULE_DUMP);
    let client = [("PATH", "/opt/z:/bin:/usr/bin"), ("LD_LIBRARY_PATH", "/ld"), ("FOO", "clientfoo")];
    let r = w.run(&client, &["build", "//p:dump", "--noincompatible_strict_action_env"]);
    assert_eq!(r.code, 0, "{}", r.err);
    let e = parse(&w.read("bazel-bin/p/dump.txt"));
    assert_eq!(e["PATH"], "/opt/z:/bin:/usr/bin");
    assert_eq!(e["LD_LIBRARY_PATH"], "/ld");
    assert_eq!(e["LC_CTYPE"], "<unset>"); // 9.3.2 sets no LC_CTYPE
    assert_eq!(e["FOO"], "<unset>");
    assert_eq!(e[SERVER_ONLY], "<unset>");
}

#[test]
fn action_env_set_inherit_unset_and_last_wins() {
    let w = ws("actionenv");
    w.write("p/BUILD", GENRULE_DUMP);
    let client = [("HOME", "/client/home"), ("BAR", "clientbar"), ("FOO", "clientfoo")];
    let r = w.run(
        &client,
        &[
            "build",
            "//p:dump",
            "--action_env=FOO=bar",
            "--action_env=HOME",
            "--action_env=BAR=1",
            "--action_env==BAR",
            "--action_env=FOO=baz",
            "--action_env=BAZ=a=b",
            "--action_env=CLIENT_ONLY",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.err);
    let e = parse(&w.read("bazel-bin/p/dump.txt"));
    assert_eq!(e["FOO"], "baz", "last occurrence wins, client FOO is ignored for a fixed value");
    assert_eq!(e["HOME"], "/client/home");
    assert_eq!(e["BAR"], "<unset>", "=BAR unsets even though the client has BAR");
    assert_eq!(e["BAZ"], "a=b");
    assert_eq!(e["CLIENT_ONLY"], "<unset>", "inherited but absent from the client environment");
    assert_eq!(e["PATH"], STRICT_PATH);
}

#[test]
fn action_env_can_override_and_unset_the_default_path() {
    let w = ws("actionpath");
    w.write("p/BUILD", GENRULE_DUMP);
    let r = w.run(&[], &["build", "//p:dump", "--action_env=PATH=/custom:/bin"]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(parse(&w.read("bazel-bin/p/dump.txt"))["PATH"], "/custom:/bin");
    let r = w.run(&[], &["build", "//p:dump", "--action_env==PATH", "--action_env==LC_CTYPE"]);
    assert_eq!(r.code, 0, "{}", r.err);
    let e = parse(&w.read("bazel-bin/p/dump.txt"));
    // bash invents its own PATH when none is exported; what matters is that it is not ours.
    assert_ne!(e["PATH"], STRICT_PATH);
    assert_ne!(e["PATH"], "/custom:/bin");
    assert_eq!(e["LC_CTYPE"], "<unset>");
}

#[test]
fn inherited_values_are_part_of_the_action_cache_key() {
    let w = ws("cachekey");
    w.write("p/BUILD", GENRULE_DUMP);
    let args = ["build", "//p:dump", "--action_env=FOO"];
    let r = w.run(&[("FOO", "one")], &args);
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(parse(&w.read("bazel-bin/p/dump.txt"))["FOO"], "one");
    assert!(r.err.contains("2 total actions"), "{}", r.err);
    // Same client value: up to date.
    let r = w.run(&[("FOO", "one")], &args);
    assert!(r.err.contains("1 total action\n"), "{}", r.err);
    // Different client value: the action re-executes with the new value.
    let r = w.run(&[("FOO", "two")], &args);
    assert!(r.err.contains("2 total actions"), "{}", r.err);
    assert_eq!(parse(&w.read("bazel-bin/p/dump.txt"))["FOO"], "two");
    // Variable removed from the client environment: executes again, now unset.
    let r = w.run(&[], &args);
    assert!(r.err.contains("2 total actions"), "{}", r.err);
    assert_eq!(parse(&w.read("bazel-bin/p/dump.txt"))["FOO"], "<unset>");
    // Unrelated client variables do not invalidate anything.
    let r = w.run(&[("OTHER", "x")], &args);
    assert!(r.err.contains("1 total action\n"), "{}", r.err);
}

#[test]
fn fixed_action_env_values_change_the_action_key() {
    let w = ws("fixedkey");
    w.write("p/BUILD", GENRULE_DUMP);
    assert_eq!(w.run(&[], &["build", "//p:dump", "--action_env=FOO=1"]).code, 0);
    let r = w.run(&[], &["build", "//p:dump", "--action_env=FOO=1"]);
    assert!(r.err.contains("1 total action\n"), "{}", r.err);
    let r = w.run(&[], &["build", "//p:dump", "--action_env=FOO=2"]);
    assert!(r.err.contains("2 total actions"), "{}", r.err);
    assert_eq!(parse(&w.read("bazel-bin/p/dump.txt"))["FOO"], "2");
}

#[test]
fn host_action_env_applies_to_exec_configuration_actions_only() {
    let w = ws("hostenv");
    w.write("p/BUILD", GENRULE_DUMP);
    let client = [("BAR", "clientbar")];
    let r = w.run(
        &client,
        &["build", "//p:user_dump", "--action_env=FOO=target", "--host_action_env=FOO=host", "--host_action_env=BAR"],
    );
    assert_eq!(r.code, 0, "{}", r.err);
    let e = parse(&w.read("bazel-bin/p/user.txt"));
    // The genrule itself runs in the target configuration (--action_env) ...
    assert_eq!(e["FOO"], "target");
    assert_eq!(e["BAR"], "<unset>");
    // ... its tool in the exec configuration (--host_action_env replaces --action_env).
    assert_eq!(e["TOOL_FOO"], "host");
    assert_eq!(e["TOOL_BAR"], "clientbar");
    assert_eq!(e["TOOL_PATH"], STRICT_PATH);
}

const DEFS: &str = r#"
def _impl(ctx):
    exe = ctx.actions.declare_file(ctx.label.name + ".sh")
    ctx.actions.write(exe, ctx.attr.script, is_executable = True)
    return [
        DefaultInfo(executable = exe, runfiles = ctx.runfiles(files = [exe])),
        RunEnvironmentInfo(
            environment = {"RE_FIXED": "fixed", "FOO": "from_rule"},
            inherited_environment = ["RE_INHERIT"],
        ),
    ]

re_bin = rule(implementation = _impl, executable = True, attrs = {"script": attr.string()})
re_test = rule(implementation = _impl, test = True, attrs = {"script": attr.string()})

def _plain(ctx):
    exe = ctx.actions.declare_file(ctx.label.name + ".sh")
    ctx.actions.write(exe, ctx.attr.script, is_executable = True)
    return [DefaultInfo(executable = exe, runfiles = ctx.runfiles(files = [exe]))]

plain_test = rule(implementation = _plain, test = True, attrs = {"script": attr.string(), "env": attr.string_dict()})

def _default_env(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    keys = sorted(ctx.configuration.default_shell_env.keys())
    ctx.actions.write(out, "\n".join(["ENV %s=%s" % (k, ctx.configuration.default_shell_env[k]) for k in keys]) + "\n")
    shell = ctx.actions.declare_file(ctx.label.name + "_shell.txt")
    ctx.actions.run_shell(
        outputs = [shell],
        command = "for v in PATH FOO BAZ LC_CTYPE; do echo \"ENV $v=${!v-<unset>}\"; done > " + shell.path,
        use_default_shell_env = True,
        env = {"BAZ": "explicit"},
    )
    bare = ctx.actions.declare_file(ctx.label.name + "_bare.txt")
    ctx.actions.run_shell(
        outputs = [bare],
        command = "for v in PATH FOO BAZ LC_CTYPE; do echo \"ENV $v=${!v-<unset>}\"; done > " + bare.path,
        env = {"BAZ": "explicit"},
    )
    return [DefaultInfo(files = depset([out, shell, bare]))]

default_env = rule(implementation = _default_env)
"#;

fn script_attr() -> String {
    let s = dump_script().replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
    format!("\"{s}\"")
}

fn test_ws(name: &str) -> Ws {
    let w = ws(name);
    w.write("defs.bzl", DEFS);
    w.write(
        "t/BUILD",
        &format!(
            r#"load("//:defs.bzl", "default_env", "plain_test", "re_bin", "re_test")
re_bin(name = "rebin", script = {s})
re_test(name = "retest", script = {s})
plain_test(name = "plain", script = {s})
default_env(name = "defenv")
plain_test(name = "withenv", script = {s}, env = {{"E1": "v1", "FOO": "rule"}})
sh_test(name = "sh", srcs = ["sh.sh"], env = {{"E1": "v1", "FOO": "rule"}}, env_inherit = ["EI", "E1"])
"#,
            s = script_attr()
        ),
    );
    w.write("t/sh.sh", &format!("#!/bin/sh\n{}", dump_script()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(w.ws.join("t/sh.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    w
}

fn test_env_of(w: &Ws, client: &[(&str, &str)], target: &str, flags: &[&str]) -> BTreeMap<String, String> {
    let mut args = vec!["test", target, "--test_output=all", "--cache_test_results=no"];
    args.extend_from_slice(flags);
    let r = w.run(client, &args);
    assert_eq!(r.code, 0, "{}", r.err);
    parse(&r.out)
}

#[test]
fn test_default_environment() {
    let w = test_ws("testdef");
    let client = [("FOO", "clientfoo"), ("CLIENT_ONLY", "x"), ("HOME", "/h"), ("PATH", "/opt/z:/bin")];
    let e = test_env_of(&w, &client, "//t:plain", &[]);
    // test-setup.sh prepends '.' to the PATH of the action environment.
    assert_eq!(e["PATH"], format!(".:{STRICT_PATH}"));
    assert_eq!(e["TZ"], "UTC");
    assert_eq!(e["LC_CTYPE"], "<unset>"); // 9.3.2 sets no LC_CTYPE
    assert_eq!(e.get("HOME_IS_TEST_TMPDIR").map(String::as_str), Some("yes"), "HOME defaults to TEST_TMPDIR");
    assert_eq!(e["RUNFILES_DIR_SET"], "yes");
    for unset in ["LD_LIBRARY_PATH", "CLIENT_ONLY", SERVER_ONLY, "ACT"] {
        assert_eq!(e[unset], "<unset>", "{unset}: {e:?}");
    }
    // `FOO` is a plain client variable, not part of the test environment.
    assert_eq!(e["FOO"], "<unset>");
}

#[test]
fn test_env_set_inherit_override_and_unset() {
    let w = test_ws("testenv");
    let client = [("BAR", "clientbar"), ("FOO", "clientfoo"), ("HOME", "/h")];
    let e = test_env_of(
        &w,
        &client,
        "//t:plain",
        &[
            "--test_env=FOO=baz",
            "--test_env=BAR",
            "--test_env=HOME=/explicit",
            "--action_env=FOO=bar",
            "--action_env=BAZ=from_action_env",
            "--test_env=CLIENT_ONLY",
        ],
    );
    assert_eq!(e["FOO"], "baz", "--test_env overrides --action_env");
    assert_eq!(e["BAR"], "clientbar", "--test_env=NAME inherits from the client environment");
    assert_eq!(e["HOME"], "/explicit", "--test_env overrides the default HOME");
    assert_eq!(e["BAZ"], "from_action_env", "--action_env reaches tests");
    assert_eq!(e["CLIENT_ONLY"], "<unset>");
    assert_eq!(e["PATH"], format!(".:{STRICT_PATH}"));

    // `--test_env=HOME` inherits the client HOME; `--test_env==HOME` removes even the default.
    let e = test_env_of(&w, &client, "//t:plain", &["--test_env=HOME"]);
    assert_eq!(e["HOME"], "/h");
    let e = test_env_of(&w, &client, "//t:plain", &["--test_env==HOME", "--test_env==TZ"]);
    assert_eq!(e["HOME"], "<unset>");
    assert_eq!(e["TZ"], "<unset>");
    // Last one wins: HOME set after the unset.
    let e = test_env_of(&w, &client, "//t:plain", &["--test_env==HOME", "--test_env=HOME=/again"]);
    assert_eq!(e["HOME"], "/again");
    // The action environment's PATH can be replaced / non-strict inherits the client PATH.
    let e = test_env_of(&w, &[("PATH", "/opt/q:/bin:/usr/bin")], "//t:plain", &["--noincompatible_strict_action_env"]);
    assert_eq!(e["PATH"], ".:/opt/q:/bin:/usr/bin");
    let e = test_env_of(&w, &client, "//t:plain", &["--action_env=PATH=/custom:/bin:/usr/bin"]);
    assert_eq!(e["PATH"], ".:/custom:/bin:/usr/bin");
}

#[test]
fn test_env_inherits_use_the_client_env_and_invalidate_the_test_cache() {
    let w = test_ws("testcache");
    let run = |foo: &str| {
        let r = w.run(&[("FOO", foo)], &["test", "//t:plain", "--test_output=all", "--test_env=FOO"]);
        assert_eq!(r.code, 0, "{}", r.err);
        r.out
    };
    let first = run("one");
    assert_eq!(parse(&first)["FOO"], "one");
    assert!(!first.contains("(cached)"), "{first}");
    assert!(run("one").contains("(cached)"));
    let changed = run("two");
    assert!(!changed.contains("(cached) PASSED"), "{changed}");
    assert_eq!(parse(&changed)["FOO"], "two");
}

#[test]
fn rule_environment_wins_over_test_env() {
    let w = test_ws("ruleenv");
    let client = [("EI", "client_ei"), ("E1", "client_e1"), ("FOO", "clientfoo")];
    // `@rules_shell`'s `sh_test` (`RunEnvironmentInfo`): `env` beats `--test_env`; an `env_inherit` entry (taken
    // from the client environment) beats an `env` entry of the same name (verified on real Bazel 9.3.2).
    let e = test_env_of(&w, &client, "//t:sh", &["--test_env=FOO=cli", "--test_env=E1=cli"]);
    assert_eq!(e["FOO"], "rule");
    assert_eq!(e["E1"], "client_e1");
    assert_eq!(e["EI"], "client_ei");
    // A Starlark rule's `env` attribute is *not* applied unless it returns RunEnvironmentInfo.
    let e = test_env_of(&w, &client, "//t:withenv", &["--test_env=FOO=cli"]);
    assert_eq!(e["FOO"], "cli");
    assert_eq!(e["E1"], "<unset>", "a Starlark rule's own `env` attribute is not the native one");
}

#[test]
fn run_environment_info_in_tests() {
    let w = test_ws("rei-test");
    let client = [("RE_INHERIT", "client_value"), ("FOO", "clientfoo")];
    let e = test_env_of(&w, &client, "//t:retest", &["--test_env=RE_FIXED=cli", "--test_env=FOO=cli"]);
    assert_eq!(e["RE_FIXED"], "fixed", "RunEnvironmentInfo.environment overrides --test_env");
    assert_eq!(e["FOO"], "from_rule");
    assert_eq!(e["RE_INHERIT"], "client_value", "inherited_environment comes from the client environment");
    let e = test_env_of(&w, &[], "//t:retest", &[]);
    assert_eq!(e["RE_INHERIT"], "<unset>");
}

fn exec_env(x: &ExecRequest) -> (BTreeMap<String, String>, Vec<String>) {
    let env = x
        .environment_variable
        .iter()
        .map(|v| (String::from_utf8_lossy(&v.name).into_owned(), String::from_utf8_lossy(&v.value).into_owned()))
        .collect();
    let mut clear: Vec<String> = x.environment_variable_to_clear.iter().map(|n| String::from_utf8_lossy(n).into_owned()).collect();
    clear.sort();
    (env, clear)
}

#[test]
fn bazel_run_environment() {
    let w = test_ws("run");
    let client = [("RE_INHERIT", "client_value"), ("FOO", "clientfoo"), ("CLIENT_ONLY", "x"), ("RUN1", "clientrun1")];
    let r = w.run(
        &client,
        &[
            "run",
            "//t:rebin",
            "--run_env=RUN1",
            "--run_env=FOO=cli",
            "--run_env=RE_FIXED=cli",
            "--run_env==CLIENT_ONLY",
            "--run_env=MISSING_IN_CLIENT",
            "--action_env=ACT=1",
            "--action_env=RUN1=fromactionenv",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.err);
    let (env, clear) = exec_env(r.exec.as_ref().expect("exec request"));
    assert_eq!(env["RE_FIXED"], "fixed", "RunEnvironmentInfo overrides --run_env");
    assert_eq!(env["FOO"], "from_rule");
    assert_eq!(env["RE_INHERIT"], "client_value");
    assert_eq!(env["RUN1"], "clientrun1", "--run_env=NAME inherits; --action_env does not apply to bazel run");
    assert!(!env.contains_key("ACT"), "{env:?}");
    assert!(!env.contains_key("MISSING_IN_CLIENT"));
    assert!(!env.contains_key("CLIENT_ONLY"));
    assert!(!env.contains_key("PATH"), "the client's own PATH is kept by the client, not rewritten");
    assert!(!env.contains_key(SERVER_ONLY));
    assert_eq!(env["BUILD_WORKSPACE_DIRECTORY"], w.ws.to_str().unwrap());
    assert!(env.contains_key("BUILD_WORKING_DIRECTORY") && env.contains_key("BUILD_EXECROOT") && env.contains_key("BUILD_ID"));
    assert_eq!(
        clear,
        ["CLIENT_ONLY", "JAVA_RUNFILES", "RUNFILES_DIR", "RUNFILES_MANIFEST_FILE", "RUNFILES_MANIFEST_ONLY", "TEST_SRCDIR"]
    );
    // Without `RunEnvironmentInfo`-declared inherited variable in the client env nothing is added.
    let r = w.run(&[], &["run", "//t:rebin"]);
    let (env, _) = exec_env(r.exec.as_ref().unwrap());
    assert!(!env.contains_key("RE_INHERIT"));
    assert_eq!(env["RE_FIXED"], "fixed");
}

#[test]
fn ctx_configuration_default_shell_env_and_use_default_shell_env() {
    let w = test_ws("defshell");
    let client = [("FOO", "clientfoo"), ("BAZ", "clientbaz")];
    let r = w.run(&client, &["build", "//t:defenv", "--action_env=FOO", "--action_env=BAZ=cfg", "--action_env=FIXED=1"]);
    assert_eq!(r.code, 0, "{}", r.err);
    // `ctx.configuration.default_shell_env` is the fixed part: inherited FOO is not in it.
    let fixed = parse(&w.read("bazel-bin/t/defenv.txt"));
    assert_eq!(fixed["BAZ"], "cfg");
    assert_eq!(fixed["FIXED"], "1");
    assert!(!fixed.contains_key("LC_CTYPE")); // 9.3.2 sets no LC_CTYPE
    assert_eq!(fixed["PATH"], STRICT_PATH);
    assert!(!fixed.contains_key("FOO"));
    // With use_default_shell_env the inherited FOO is there and the action's `env` overrides BAZ.
    let with = parse(&w.read("bazel-bin/t/defenv_shell.txt"));
    assert_eq!(with["FOO"], "clientfoo");
    assert_eq!(with["BAZ"], "explicit");
    assert_eq!(with["PATH"], STRICT_PATH);
    assert_eq!(with["LC_CTYPE"], "<unset>"); // 9.3.2 sets no LC_CTYPE
    // Without it: only the action's own env.
    let bare = parse(&w.read("bazel-bin/t/defenv_bare.txt"));
    assert_eq!(bare["BAZ"], "explicit");
    assert_eq!(bare["FOO"], "<unset>");
    assert_eq!(bare["LC_CTYPE"], "<unset>");
    // (bash invents a PATH when none is exported; it must not be ours.)
    assert_ne!(bare["PATH"], STRICT_PATH);
}

#[test]
fn client_env_args_are_not_user_arguments() {
    // `--client_env` must not reach option parsing of the command (it is hidden client plumbing).
    let w = ws("plumbing");
    w.write("p/BUILD", GENRULE_DUMP);
    let r = w.run(&[("A", "b=c"), ("EMPTY", "")], &["build", "//p:dump"]);
    assert_eq!(r.code, 0, "{}", r.err);
}
