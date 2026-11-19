// Copyright 2026 The Bazel Authors. All rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//    http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#[cfg(unix)]
#[test]
fn standalone_process_wrapper_reraises_sigterm_and_reaps_subreaper_orphans() {
    use std::os::unix::process::ExitStatusExt;
    use std::process::Command;
    use std::time::{Duration, Instant};

    let pw = env!("CARGO_BIN_EXE_zel-process-wrapper");
    let dir = std::env::temp_dir().join(format!("zel-pw-bin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 1. Double-forked grandchild escaping via a subshell must still be killed before wrapper exits.
    let stats = dir.join("stats.pb");
    let started = Instant::now();
    let out = Command::new(pw)
        .args([
            "-s",
            stats.to_str().unwrap(),
            "--",
            "/bin/sh",
            "-c",
            "(sleep 30 >/dev/null 2>&1 &) ; echo hello_live ; exit 0",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(out.stdout, b"hello_live\n");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(stats.exists());

    // 2. Graceful SIGTERM (`-g`): even if child catches TERM and exits 0, wrapper re-raises SIGTERM.
    let ready = dir.join("ready");
    let mut child = Command::new(pw)
        .args([
            "-g",
            "-k",
            "5",
            "--",
            "/bin/sh",
            "-c",
            &format!(
                "trap 'exit 0' TERM; echo ok > {}; while :; do sleep 0.05; done",
                ready.display()
            ),
        ])
        .spawn()
        .unwrap();
    let wait_ready = Instant::now();
    while !ready.exists() && wait_ready.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(ready.exists());
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe {
        let _ = kill(child.id() as i32, 15);
    }
    let st = child.wait().unwrap();
    assert_eq!(st.signal(), Some(15), "expected death by SIGTERM: {st:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(target_os = "linux")]
#[test]
fn standalone_linux_sandbox_probe_and_isolation() {
    use std::process::Command;

    let ls = env!("CARGO_BIN_EXE_zel-linux-sandbox");
    let dir = std::env::temp_dir().join(format!("zel-ls-bin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    std::fs::create_dir_all(dir.join("tmp_mnt")).unwrap();

    // If user namespaces are disabled by host sysctl/secpolicy in container, probe gracefully reports it.
    if let Err(_unsupported) = zel_sandbox::linux::probe_linux_sandbox(ls, Some(&dir), 5) {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    let out_file = dir.join("stdout.txt");
    let stats_file = dir.join("stats.pb");
    let st = Command::new(ls)
        .args([
            "-W",
            dir.join("work").to_str().unwrap(),
            "-T",
            "10",
            "-t",
            "2",
            "-l",
            out_file.to_str().unwrap(),
            "-e",
            dir.join("tmp_mnt").to_str().unwrap(),
            "-S",
            stats_file.to_str().unwrap(),
            "-H",
            "-N",
            "-U",
            "--",
            "/bin/sh",
            "-c",
            "echo host=$(hostname) uid=$(id -u) pid=$$ pwd=$(pwd)",
        ])
        .status()
        .unwrap();
    assert!(st.success(), "{st:?}");
    let captured = std::fs::read_to_string(&out_file).unwrap();
    assert!(captured.contains("host=localhost"), "{captured}");
    assert!(captured.contains("uid=65534"), "{captured}");
    assert!(stats_file.exists());

    let _ = std::fs::remove_dir_all(&dir);
}
