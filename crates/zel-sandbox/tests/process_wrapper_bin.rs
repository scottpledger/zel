//! The standalone `zel-process-wrapper` executable (port of `process-wrapper.cc`).

use std::os::unix::process::ExitStatusExt;
use std::process::Command;

fn pw() -> Command {
    Command::new(env!("CARGO_BIN_EXE_zel-process-wrapper"))
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("zel-pwbin-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn propagates_exit_code_redirects_and_writes_stats() {
    let d = tmp("basic");
    let st = pw()
        .arg(format!("--stdout={}", d.join("o").display()))
        .arg(format!("--stderr={}", d.join("e").display()))
        .arg(format!("--stats={}", d.join("s").display()))
        .args(["--", "/bin/sh", "-c", "echo out; echo err >&2; exit 5"])
        .status()
        .unwrap();
    assert_eq!(st.code(), Some(5));
    assert_eq!(std::fs::read_to_string(d.join("o")).unwrap(), "out\n");
    assert_eq!(std::fs::read_to_string(d.join("e")).unwrap(), "err\n");
    let stats = zel_sandbox::process_wrapper::ResourceUsage::decode_execution_statistics(&std::fs::read(d.join("s")).unwrap()).unwrap();
    assert!(stats.maxrss > 0);
}

#[test]
fn timeout_kills_the_process_group_and_dies_by_sigalrm() {
    let t = std::time::Instant::now();
    let st = pw()
        .args(["--timeout=0.3", "--kill_delay=0.3", "/bin/sh", "-c", "trap '' TERM; sleep 30 & sleep 30"])
        .status()
        .unwrap();
    assert_eq!(st.signal(), Some(14), "{st:?}");
    assert!(t.elapsed().as_secs() < 10);
}

#[test]
fn child_death_by_signal_is_reraised() {
    let st = pw().args(["/bin/sh", "-c", "kill -TERM $$"]).status().unwrap();
    assert_eq!(st.signal(), Some(15), "{st:?}");
}

#[test]
fn usage_errors_exit_1_with_usage_text() {
    let o = pw().args(["-t", "abc", "--", "true"]).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    let e = String::from_utf8_lossy(&o.stderr);
    assert!(e.starts_with("Invalid timeout (-t) value: abc\n\nUsage:"), "{e}");
    let o = pw().output().unwrap();
    assert!(String::from_utf8_lossy(&o.stderr).starts_with("No command specified."));
}
