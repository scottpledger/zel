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

    let host_supports_userns = Command::new("unshare")
        .args(["-Urpf", "/bin/true"])
        .status()
        .is_ok_and(|s| s.success());
    // If user namespaces are disabled by host sysctl/secpolicy in container, probe gracefully reports it.
    if let Err(err) = zel_sandbox::linux::probe_linux_sandbox(ls, Some(&dir), 5) {
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            !host_supports_userns,
            "host supports unprivileged userns (`unshare -Urpf /bin/true` succeeded), so `probe_linux_sandbox` must succeed too, got: {err}"
        );
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

    // 1. Non-hermetic read-only enforcement: non-`-w` host paths and `-M/-m` bind mounts reject
    // writes with EROFS, while `-W`, `-w`, and `-e` accept writes, and `-C` writes `cgroup.procs`.
    let ro_sibling = dir.join("ro_sibling");
    let ro_bind_src = dir.join("ro_bind_src");
    let ro_bind_dst = dir.join("ro_bind_dst");
    let rw_extra = dir.join("rw_extra");
    let cg_dir = dir.join("fake_cgroup");
    std::fs::create_dir_all(&ro_sibling).unwrap();
    std::fs::create_dir_all(&ro_bind_src).unwrap();
    std::fs::create_dir_all(&ro_bind_dst).unwrap();
    std::fs::create_dir_all(&rw_extra).unwrap();
    std::fs::create_dir_all(&cg_dir).unwrap();
    std::fs::write(ro_bind_src.join("in.txt"), "immutable\n").unwrap();
    std::fs::write(cg_dir.join("memory.peak"), "16777216\n").unwrap(); // 16 MiB = 16384 KiB

    let out_ro = dir.join("ro_check.txt");
    let stats_cg = dir.join("stats_cg.pb");
    let script = format!(
        "set -e; \
         echo work_ok > work_file; \
         echo rw_ok > {rw}/rw_file; \
         echo tmp_ok > {tmp}/tmp_file; \
         if touch {sib}/nope 2>/dev/null; then echo SIBLING_WRITABLE; exit 11; fi; \
         if touch {mnt}/nope 2>/dev/null; then echo BIND_WRITABLE; exit 12; fi; \
         if touch /dev/pts/nope 2>/dev/null; then echo PTS_WRITABLE_WITHOUT_P; exit 13; fi; \
         cat {mnt}/in.txt",
        rw = rw_extra.display(),
        tmp = dir.join("tmp_mnt").display(),
        sib = ro_sibling.display(),
        mnt = ro_bind_dst.display(),
    );
    let st = Command::new(ls)
        .args([
            "-W",
            dir.join("work").to_str().unwrap(),
            "-w",
            rw_extra.to_str().unwrap(),
            "-e",
            dir.join("tmp_mnt").to_str().unwrap(),
            "-M",
            ro_bind_src.to_str().unwrap(),
            "-m",
            ro_bind_dst.to_str().unwrap(),
            "-C",
            cg_dir.to_str().unwrap(),
            "-l",
            out_ro.to_str().unwrap(),
            "-S",
            stats_cg.to_str().unwrap(),
            "--",
            "/bin/sh",
            "-c",
            &script,
        ])
        .status()
        .unwrap();
    assert!(st.success(), "{st:?}");
    assert_eq!(std::fs::read_to_string(&out_ro).unwrap(), "immutable\n");
    assert_eq!(
        std::fs::read_to_string(dir.join("work/work_file")).unwrap(),
        "work_ok\n"
    );
    assert_eq!(
        std::fs::read_to_string(rw_extra.join("rw_file")).unwrap(),
        "rw_ok\n"
    );
    assert!(!ro_sibling.join("nope").exists());
    assert!(!ro_bind_src.join("nope").exists());

    let procs_written = std::fs::read_to_string(cg_dir.join("cgroup.procs")).unwrap();
    assert!(
        procs_written.trim().parse::<u32>().is_ok_and(|p| p > 0),
        "expected valid pid in cgroup.procs, got {procs_written:?}"
    );
    let decoded_stats = zel_sandbox::process_wrapper::ResourceUsage::decode_execution_statistics(
        &std::fs::read(&stats_cg).unwrap(),
    )
    .unwrap();
    assert_eq!(decoded_stats.maxrss, 16384);

    // Invalid `-C` cgroup directory fails fast before running the command.
    let bad_cg = Command::new(ls)
        .args([
            "-W",
            dir.join("work").to_str().unwrap(),
            "-C",
            dir.join("missing_cgroup_dir").to_str().unwrap(),
            "--",
            "/bin/true",
        ])
        .output()
        .unwrap();
    assert!(!bad_cg.status.success(), "{bad_cg:?}");

    // 2. Hermetic (`-h`) read-only enforcement: sandbox root `/` and `-M/-m` bind mounts are
    // remounted read-only (`EROFS`), while `-W` and `-w` remain writable.
    let sbx = dir.join("hermetic_root");
    let sbx_work = sbx.join("work");
    let sbx_rw = sbx.join("rw_out");
    let sbx_ro_inside = sbx.join("ro_inside");
    std::fs::create_dir_all(&sbx_work).unwrap();
    std::fs::create_dir_all(&sbx_rw).unwrap();
    std::fs::create_dir_all(&sbx_ro_inside).unwrap();

    let mut hermetic_args: Vec<String> = vec![
        "-h".into(),
        sbx.to_str().unwrap().into(),
        "-W".into(),
        sbx_work.to_str().unwrap().into(),
        "-w".into(),
        sbx_rw.to_str().unwrap().into(),
        "-M".into(),
        ro_bind_src.to_str().unwrap().into(),
        "-m".into(),
        "/mounted_ro".into(),
    ];
    for sys_dir in ["/bin", "/usr", "/lib", "/lib64"] {
        if std::path::Path::new(sys_dir).exists() {
            hermetic_args.push("-M".into());
            hermetic_args.push(sys_dir.into());
        }
    }
    hermetic_args.extend([
        "--".into(),
        "/bin/sh".into(),
        "-c".into(),
        "set -e; \
         echo h_work > h_file; \
         echo h_rw > /rw_out/out_file; \
         if touch /root_nope 2>/dev/null; then echo ROOT_WRITABLE; exit 21; fi; \
         if touch /ro_inside/nope 2>/dev/null; then echo RO_INSIDE_WRITABLE; exit 22; fi; \
         if touch /mounted_ro/nope 2>/dev/null; then echo MOUNTED_RO_WRITABLE; exit 23; fi; \
         test \"$(cat /mounted_ro/in.txt)\" = immutable"
            .into(),
    ]);
    let h_out = Command::new(ls).args(&hermetic_args).output().unwrap();
    assert!(
        h_out.status.success(),
        "hermetic failed: status={:?} stderr={}",
        h_out.status,
        String::from_utf8_lossy(&h_out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(sbx_work.join("h_file")).unwrap(),
        "h_work\n"
    );
    assert_eq!(
        std::fs::read_to_string(sbx_rw.join("out_file")).unwrap(),
        "h_rw\n"
    );
    assert!(!sbx.join("root_nope").exists());
    assert!(!sbx_ro_inside.join("nope").exists());
    assert!(!ro_bind_src.join("nope").exists());

    // 3. `-P` (`enable_pty`): maps inner gid to `tty`, keeps `/dev/pts` writable, preserves
    // stdout/stderr without EIO/SIGHUP, and allows pseudoterminals to be opened and used.
    let expected_tty_gid = zel_sandbox::linux_sandbox::lookup_tty_gid();
    let pty_out = Command::new(ls)
        .args([
            "-W",
            dir.join("work").to_str().unwrap(),
            "-U",
            "-P",
            "--",
            "/bin/sh",
            "-c",
            "echo gid=$(id -g); \
             if command -v python3 >/dev/null 2>&1; then \
               python3 -c 'import os, pty; m, s = pty.openpty(); assert os.isatty(s); os.write(s, b\"pty_ping\\n\"); data = os.read(m, 64); os.close(s); os.close(m); print(\"pty_ok=\" + data.decode().strip())'; \
             fi",
        ])
        .output()
        .unwrap();
    assert!(
        pty_out.status.success(),
        "pty failed: status={:?} stderr={}",
        pty_out.status,
        String::from_utf8_lossy(&pty_out.stderr)
    );
    let pty_stdout = String::from_utf8_lossy(&pty_out.stdout);
    assert!(
        pty_stdout.contains(&format!("gid={expected_tty_gid}")),
        "{pty_stdout}"
    );
    if pty_stdout.contains("pty_ok=") {
        assert!(pty_stdout.contains("pty_ok=pty_ping"), "{pty_stdout}");
    }

    let _ = std::fs::remove_dir_all(&dir);
}
