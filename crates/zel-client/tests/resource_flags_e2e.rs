//! End-to-end tests of Bazel's `ResourceConverter` flag forms through the real `zel` binary:
//! `--jobs=auto`, `--jobs=HOST_CPUS*.5`, `--local_test_jobs=auto`,
//! `--local_resources=cpu=HOST_CPUS*.5`, ... must parse *and* execute, and invalid values must be
//! rejected with Bazel's exact converter messages (exit code 2).

use std::path::PathBuf;
use std::process::Command;

fn zel() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_zel"))
}

struct Ws {
    root: PathBuf,
    ws: PathBuf,
    ob: PathBuf,
}

fn ws(name: &str) -> Ws {
    let root = std::env::temp_dir().join(format!("zel-resflags-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("ws/pkg")).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let w = Ws { ws: root.join("ws"), ob: root.join("ob"), root };
    std::fs::write(w.ws.join("MODULE.bazel"), "module(name = \"resflags\")\n").unwrap();
    std::fs::write(
        w.ws.join("pkg/BUILD"),
        "genrule(name = \"ok\", outs = [\"ok.txt\"], cmd = \"echo ok > $@\")\n",
    )
    .unwrap();
    w
}

impl Ws {
    fn build(&self, args: &[&str]) -> (i32, String) {
        let o = Command::new(zel())
            .current_dir(&self.ws)
            .arg(format!("--output_base={}", self.ob.display()))
            .arg("build")
            .args(args)
            .env("HOME", &self.root)
            .output()
            .unwrap();
        let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&o.stderr));
        (o.status.code().unwrap_or(-1), text)
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
fn resource_keyword_forms_parse_and_execute() {
    let w = ws("ok");
    for flag in [
        "--jobs=auto",
        "--jobs=HOST_CPUS",
        "--jobs=HOST_CPUS*.5",
        "--jobs=HOST_CPUS*0.5",
        "--jobs=HOST_RAM*.0001",
        "--local_test_jobs=auto",
        "--local_test_jobs=0",
        "--local_test_jobs=HOST_CPUS*.5",
        "--local_resources=cpu=HOST_CPUS*.5",
        "--local_resources=cpu=2",
        "--local_resources=memory=HOST_RAM*.5",
        "--local_resources=gpu=1.5",
        "--loading_phase_threads=auto",
        "--loading_phase_threads=HOST_CPUS*.5",
    ] {
        let (code, out) = w.build(&["//pkg:ok", flag]);
        assert_eq!(code, 0, "{flag}: {out}");
    }
    // Several forms together, and the `-j` abbreviation with a separate value.
    let (code, out) = w.build(&[
        "//pkg:ok",
        "-j",
        "HOST_CPUS*.5",
        "--local_resources=cpu=HOST_CPUS",
        "--local_resources=memory=HOST_RAM*.5",
        "--local_test_jobs=auto",
    ]);
    assert_eq!(code, 0, "{out}");
}

#[test]
fn invalid_resource_values_use_bazel_messages() {
    let w = ws("bad");
    let syntax = "an integer, or a keyword (\"auto\", \"HOST_CPUS\", \"HOST_RAM\"), optionally followed by an operation ([-|*]<float>) eg. \"auto\", \"HOST_CPUS*.5\"";
    let cases: Vec<(Vec<&str>, String)> = vec![
        (
            vec!["--jobs=lots"],
            format!("ERROR: While parsing option --jobs=lots: Parameter 'lots' does not follow correct syntax. This flag takes {syntax}."),
        ),
        (vec!["--jobs=0"], "ERROR: While parsing option --jobs=0: Value '(0)' must be at least 1.".to_string()),
        (
            vec!["-j", "HOST_CPUS/2"],
            format!("ERROR: While parsing option -j HOST_CPUS/2: Parameter 'HOST_CPUS/2' does not follow correct syntax. This flag takes {syntax}."),
        ),
        (
            vec!["--local_test_jobs=-1"],
            "ERROR: While parsing option --local_test_jobs=-1: Value '(-1.000000)' must be at least 0.000000.".to_string(),
        ),
        (
            vec!["--local_resources=cpu"],
            "ERROR: While parsing option --local_resources=cpu: Variable definitions must be in the form of a 'name=value' assignment".to_string(),
        ),
        (
            vec!["--local_resources=cpu=auto"],
            "ERROR: While parsing option --local_resources=cpu=auto: Parameter 'auto' does not follow correct syntax. This flag takes an integer, or a keyword (\"HOST_CPUS\", \"HOST_RAM\"), optionally followed by an operation ([-|*]<float>) eg. \"HOST_CPUS\", \"HOST_CPUS*.5\".".to_string(),
        ),
        // These flags do not exist in this Bazel version.
        (vec!["--local_cpu_resources=2"], "ERROR: --local_cpu_resources=2 :: Unrecognized option: --local_cpu_resources=2".to_string()),
        (vec!["--nojobs"], "ERROR: --nojobs :: Illegal use of 'no' prefix on non-boolean option: --nojobs".to_string()),
        (vec!["--jobs"], "ERROR: Expected value after --jobs".to_string()),
    ];
    for (flags, expected) in cases {
        let mut args = vec!["//pkg:ok"];
        args.extend(flags.iter().copied());
        let (code, out) = w.build(&args);
        assert_eq!(code, 2, "{flags:?}: {out}");
        assert!(out.lines().any(|l| l.starts_with(&expected)), "{flags:?}: expected {expected:?} in {out}");
    }
}
