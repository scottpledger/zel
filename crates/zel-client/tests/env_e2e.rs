//! The client environment reaches actions through the real `zel` binary: the client sends it as
//! `--client_env` arguments with every command, so a persistent daemon started under one
//! environment still sees each command's own environment (and never its own).

use std::path::PathBuf;
use std::process::Command;

fn zel() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_zel"))
}


/// A hermetic local registry built once per test process from the vendored closure under
/// `third_party` (`scripts/make_vendored_registry.py`); no network is ever used.
fn local_registry() -> PathBuf {
    static REG: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    REG.get_or_init(|| {
        let root = env!("CARGO_MANIFEST_DIR").strip_suffix("/crates/zel-client").unwrap_or("/Users/pledger/Development/zel");
        let dir = std::env::temp_dir().join(format!("zel-client-registry-{}", std::process::id()));
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

fn registry_flag(cmd: &str) -> Option<String> {
    matches!(cmd, "build" | "test" | "run" | "coverage" | "query" | "cquery" | "aquery" | "fetch")
        .then(|| format!("--registry=file://{}", local_registry().display()))
}

/// Bazel 9 has no native `sh_*` rules: BUILD files load them from `@rules_shell`.
fn shell_loads(rel: &str, content: &str) -> String {
    if !(rel == "BUILD" || rel.ends_with("/BUILD")) {
        return content.to_string();
    }
    let mut out = String::new();
    for name in ["sh_binary", "sh_test", "sh_library"] {
        if content.contains(&format!("{name}(")) {
            out.push_str(&format!("load(\"@rules_shell//shell:{name}.bzl\", \"{name}\")\n"));
        }
    }
    out.push_str(content);
    out
}

struct Ws {
    ws: PathBuf,
    ob: PathBuf,
    root: PathBuf,
}

fn ws(name: &str) -> Ws {
    let root = std::env::temp_dir().join(format!("zel-env-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("ws/p")).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let w = Ws { ws: root.join("ws"), ob: root.join("ob"), root };
    std::fs::write(w.ws.join("MODULE.bazel"), "module(name = \"e2e\")\nbazel_dep(name = \"rules_shell\", version = \"0.6.1\")\n").unwrap();
    std::fs::write(w.ws.join("BUILD"), "").unwrap();
    std::fs::write(
        w.ws.join("p/BUILD"),
        shell_loads("p/BUILD", r#"
genrule(
    name = "dump",
    outs = ["dump.txt"],
    cmd = "for v in PATH FOO CLIENT_ONLY; do echo \"ENV $$v=$${!v-<unset>}\"; done > $@",
)
sh_binary(name = "bin", srcs = ["bin.sh"])
"#),
    )
    .unwrap();
    std::fs::write(
        w.ws.join("p/bin.sh"),
        "#!/bin/sh\nfor v in FOO CLIENT_ONLY RUNFILES_DIR TEST_SRCDIR RUN1 ACT BUILD_WORKSPACE_DIRECTORY; do eval \"val=\\${$v-<unset>}\"; echo \"ENV $v=$val\"; done\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(w.ws.join("p/bin.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    w
}

impl Ws {
    fn zel(&self, env: &[(&str, &str)], args: &[&str]) -> (i32, String) {
        let mut c = Command::new(zel());
        c.current_dir(&self.ws).arg(format!("--output_base={}", self.ob.display())).arg(args[0]).args(registry_flag(args[0])).args(&args[1..]).env("HOME", &self.root);
        for (k, v) in env {
            c.env(k, v);
        }
        let o = c.output().unwrap();
        let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&o.stderr));
        (o.status.code().unwrap_or(-1), text)
    }

    fn dump(&self) -> String {
        std::fs::read_to_string(self.ws.join("bazel-bin/p/dump.txt")).unwrap()
    }
}

/// Leak guard: every e2e workspace shuts its daemon down (and kills it if that fails) when dropped.
impl Drop for Ws {
    fn drop(&mut self) {
        let _ = Command::new(zel())
            .current_dir(&self.ws)
            .arg(format!("--output_base={}", self.ob.display()))
            .arg("shutdown")
            .env("HOME", &self.root)
            .output();
        if let Ok(pid) = std::fs::read_to_string(self.ob.join("server/server.pid.txt")) {
            let _ = Command::new("kill").args(["-9", pid.trim()]).status();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn each_command_sees_its_own_client_environment() {
    let w = ws("percmd");
    // The first command starts the daemon under FOO=daemon-start.
    let (code, out) = w.zel(&[("FOO", "daemon-start"), ("CLIENT_ONLY", "x")], &["build", "//p:dump", "--action_env=FOO"]);
    assert_eq!(code, 0, "{out}");
    assert!(w.dump().contains("ENV FOO=daemon-start\n"), "{}", w.dump());
    // A later command with another value: the daemon's own environment is stale.
    let (code, out) = w.zel(&[("FOO", "second")], &["build", "//p:dump", "--action_env=FOO"]);
    assert_eq!(code, 0, "{out}");
    let d = w.dump();
    assert!(d.contains("ENV FOO=second\n"), "{d}");
    assert!(d.contains("ENV PATH=/bin:/usr/bin:/usr/local/bin\n"), "{d}");
    assert!(d.contains("ENV CLIENT_ONLY=<unset>\n"), "{d}");
    // Not set by the client at all: unset, even though the daemon started with it.
    let (code, out) = w.zel(&[], &["build", "//p:dump", "--action_env=FOO"]);
    assert_eq!(code, 0, "{out}");
    assert!(w.dump().contains("ENV FOO=<unset>\n"), "{}", w.dump());
}

#[test]
fn bazel_run_executes_with_run_env_and_clears_runfiles_variables() {
    let w = ws("run");
    let (code, out) = w.zel(
        &[("FOO", "clientfoo"), ("CLIENT_ONLY", "x"), ("RUN1", "clientrun1"), ("RUNFILES_DIR", "/stale"), ("TEST_SRCDIR", "/stale")],
        &["run", "//p:bin", "--run_env=RUN1", "--run_env=FOO=cli", "--run_env==CLIENT_ONLY", "--action_env=ACT=1"],
    );
    assert_eq!(code, 0, "{out}");
    for line in [
        "ENV FOO=cli",
        "ENV CLIENT_ONLY=<unset>",
        "ENV RUN1=clientrun1",
        "ENV RUNFILES_DIR=<unset>",
        "ENV TEST_SRCDIR=<unset>",
        "ENV ACT=<unset>",
    ] {
        assert!(out.lines().any(|l| l == line), "missing `{line}` in:\n{out}");
    }
    assert!(out.contains(&format!("ENV BUILD_WORKSPACE_DIRECTORY={}", w.ws.display())), "{out}");
}
