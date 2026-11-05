//! End-to-end tests of [`RemoteSpawnRunner`] against the in-process fake REAPI server, using
//! real local processes (`sh -c`) for the local runner.

use crate::fake_server::{ExecContext, FakeReapiServer};
use crate::options::{options_from_args, RemoteOptions};
use crate::spawn_runner::*;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use zel_proto::reapi::*;
use zel_sandbox::{Spawn, SpawnResult, SpawnStatus};
use zel_vfs::{FileSystem, LocalFileSystem, PathFragment};

struct Counting {
    n: AtomicUsize,
    inner: LocalSpawnRunner,
}

impl SpawnRunner for Counting {
    fn name(&self) -> &str {
        "counting-local"
    }
    fn can_exec(&self, s: &Spawn) -> bool {
        self.inner.can_exec(s)
    }
    fn exec(&self, s: &Spawn, r: &PathFragment, fs: &dyn FileSystem) -> io::Result<SpawnResult> {
        self.n.fetch_add(1, Ordering::SeqCst);
        self.inner.exec(s, r, fs)
    }
}

fn counting() -> Arc<Counting> {
    Arc::new(Counting { n: AtomicUsize::new(0), inner: LocalSpawnRunner })
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("zel-sr-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn spawn(argv: &[&str], inputs: &[&str], outputs: &[&str]) -> Spawn {
    Spawn {
        mnemonic: "Gen".into(),
        owner_label: "//t:t".into(),
        argv: argv.iter().map(|s| s.to_string()).collect(),
        env: BTreeMap::from([("LANG".to_string(), "C".to_string())]),
        inputs: inputs.iter().map(|s| PathFragment::create(s)).collect(),
        outputs: outputs.iter().map(|s| PathFragment::create(s)).collect(),
        ..Default::default()
    }
}

/// Finds a file by path inside the input root of an action stored in the fake CAS.
fn read_input(ctx: &ExecContext<'_>, path: &str) -> Option<Vec<u8>> {
    let mut dir = Directory::decode(&ctx.server.get_blob(ctx.action.input_root_digest.as_ref()?)?).ok()?;
    let segs: Vec<&str> = path.split('/').collect();
    for (i, seg) in segs.iter().enumerate() {
        if i + 1 == segs.len() {
            let f = dir.files.iter().find(|f| f.name == *seg)?;
            return ctx.server.get_blob(f.digest.as_ref()?);
        }
        let d = dir.directories.iter().find(|d| d.name == *seg)?;
        dir = Directory::decode(&ctx.server.get_blob(d.digest.as_ref()?)?).ok()?;
    }
    None
}

/// `upper IN OUT`: writes the upper-cased contents of IN as OUT.
fn install_upper(server: &FakeReapiServer) {
    server.set_executor(|ctx| {
        let a = &ctx.command.arguments;
        assert_eq!(a[0], "upper");
        let input = read_input(ctx, &a[1]).ok_or_else(|| crate::grpc::GrpcStatus::new(3, format!("input {} not in tree", a[1])))?;
        let out = ctx.server.put_blob(&input.to_ascii_uppercase());
        Ok(ActionResult {
            output_files: vec![OutputFile { path: a[2].clone(), digest: Some(out), ..Default::default() }],
            stdout_raw: b"did-upper\n".to_vec(),
            ..Default::default()
        })
    });
}

fn runner(server: &FakeReapiServer, exec: bool, args: &[&str], fb: Arc<Counting>) -> RemoteSpawnRunner {
    let opts = RemoteOptions::from_source(&options_from_args(args)).unwrap();
    RemoteSpawnRunner::new(opts, Some(Arc::new(server.client(exec))), fb).unwrap()
}

fn root(p: &Path) -> PathFragment {
    PathFragment::create(p.to_str().unwrap())
}

fn run(r: &RemoteSpawnRunner, s: &Spawn, execroot: &Path) -> SpawnResult {
    r.exec(s, &root(execroot), &LocalFileSystem::default()).unwrap()
}

#[test]
fn remote_execution_then_remote_cache_hit() {
    let server = FakeReapiServer::new();
    install_upper(&server);
    let ws = tmp("rexec");
    std::fs::create_dir_all(ws.join("src")).unwrap();
    std::fs::write(ws.join("src/a.txt"), "hello").unwrap();
    let fb = counting();
    let r = runner(&server, true, &["--remote_download_outputs=all"], fb.clone());
    let s = spawn(&["upper", "src/a.txt", "out/a.txt"], &["src/a.txt"], &["out/a.txt"]);
    let res = run(&r, &s, &ws);
    assert_eq!(res.status, SpawnStatus::Success, "{}", String::from_utf8_lossy(&res.stderr));
    assert_eq!(res.runner_name, "remote");
    assert_eq!(std::fs::read_to_string(ws.join("out/a.txt")).unwrap(), "HELLO");
    assert_eq!(res.stdout, b"did-upper\n");
    assert_eq!(server.executions(), 1);
    assert_eq!(fb.n.load(Ordering::SeqCst), 0, "nothing ran locally");

    std::fs::remove_file(ws.join("out/a.txt")).unwrap();
    let res = run(&r, &s, &ws);
    assert_eq!(res.runner_name, "remote cache hit");
    assert_eq!(std::fs::read_to_string(ws.join("out/a.txt")).unwrap(), "HELLO");
    assert_eq!(server.executions(), 1);

    // Changing the input changes the action digest → a new execution.
    std::fs::write(ws.join("src/a.txt"), "other").unwrap();
    let res = run(&r, &s, &ws);
    assert_eq!(res.runner_name, "remote");
    assert_eq!(std::fs::read_to_string(ws.join("out/a.txt")).unwrap(), "OTHER");
    assert_eq!(server.executions(), 2);
    assert_eq!(r.stats.summary(), "3 processes: 1 remote cache hit, 2 remote");
}

#[test]
fn local_execution_uploads_and_other_runner_hits_cache() {
    let server = FakeReapiServer::new();
    let ws1 = tmp("up1");
    let ws2 = tmp("up2");
    for ws in [&ws1, &ws2] {
        std::fs::write(ws.join("in.txt"), "data").unwrap();
    }
    let s = spawn(&["sh", "-c", "tr a-z A-Z < in.txt > out.txt; echo hi"], &["in.txt"], &["out.txt"]);
    let fb1 = counting();
    let r1 = runner(&server, false, &["--remote_download_outputs=all"], fb1.clone());
    let res = run(&r1, &s, &ws1);
    assert_eq!(res.runner_name, "local");
    assert!(res.is_ok(), "{}", String::from_utf8_lossy(&res.stderr));
    assert_eq!(fb1.n.load(Ordering::SeqCst), 1);
    assert_eq!(server.ac_count(), 1, "result uploaded to the action cache");
    assert!(server.blob_count() >= 2);

    // A different machine (workspace) with the same inputs gets a remote cache hit.
    let fb2 = counting();
    let r2 = runner(&server, false, &["--remote_download_outputs=all"], fb2.clone());
    let res = run(&r2, &s, &ws2);
    assert_eq!(res.runner_name, "remote cache hit");
    assert_eq!(fb2.n.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read_to_string(ws2.join("out.txt")).unwrap(), "DATA");
    assert_eq!(res.stdout, b"hi\n");
}

#[test]
fn upload_and_accept_flags_and_execution_requirements() {
    let server = FakeReapiServer::new();
    let ws = tmp("flags");
    std::fs::write(ws.join("i"), "x").unwrap();
    let s = spawn(&["sh", "-c", "echo r > o"], &["i"], &["o"]);

    let fb = counting();
    let r = runner(&server, false, &["--noremote_upload_local_results"], fb.clone());
    run(&r, &s, &ws);
    assert_eq!(server.ac_count(), 0, "--noremote_upload_local_results");

    let r = runner(&server, false, &[], fb.clone());
    let lookups_before_tagged = server.request_count("GetActionResult");
    let mut tagged = s.clone();
    tagged.execution_info.insert("no-remote-cache".into(), "".into());
    run(&r, &tagged, &ws);
    assert_eq!(server.ac_count(), 0, "no-remote-cache must not upload");
    let mut tagged = s.clone();
    tagged.execution_info.insert("no-cache".into(), "".into());
    run(&r, &tagged, &ws);
    assert_eq!(server.ac_count(), 0);
    assert_eq!(server.request_count("GetActionResult"), lookups_before_tagged, "no-cache spawns do no lookup");
    run(&r, &s, &ws);
    assert_eq!(server.ac_count(), 1);

    // --noremote_accept_cached: lookup is skipped even though the entry exists.
    let before = server.request_count("GetActionResult");
    let fb2 = counting();
    let r = runner(&server, false, &["--noremote_accept_cached"], fb2.clone());
    let res = run(&r, &s, &ws);
    assert_eq!(res.runner_name, "local");
    assert_eq!(server.request_count("GetActionResult"), before);
    assert_eq!(fb2.n.load(Ordering::SeqCst), 1);
}

#[test]
fn disk_cache_hit_and_combined_population() {
    let server = FakeReapiServer::new();
    install_upper(&server);
    let ws = tmp("disk");
    let dc = tmp("disk-cache");
    std::fs::write(ws.join("a.txt"), "abc").unwrap();
    let s = spawn(&["upper", "a.txt", "o.txt"], &["a.txt"], &["o.txt"]);
    let dcs = format!("--disk_cache={}", dc.display());

    // Remote execution with a disk cache: result is also stored on disk.
    let r = runner(&server, true, &["--remote_download_outputs=all", &dcs], counting());
    assert_eq!(run(&r, &s, &ws).runner_name, "remote");
    assert!(dc.join("ac").is_dir() && dc.join("cas").is_dir());

    // A disk-only runner (no remote) now hits the disk cache, without touching the server.
    std::fs::remove_file(ws.join("o.txt")).unwrap();
    let opts = RemoteOptions::from_source(&options_from_args(&["--remote_download_outputs=all", &dcs])).unwrap();
    let fb = counting();
    let r2 = RemoteSpawnRunner::new(opts, None, fb.clone()).unwrap();
    let res = run(&r2, &s, &ws);
    assert_eq!(res.runner_name, "disk cache hit");
    assert_eq!(std::fs::read_to_string(ws.join("o.txt")).unwrap(), "ABC");
    assert_eq!(fb.n.load(Ordering::SeqCst), 0);
    assert_eq!(server.executions(), 1);

    // Local execution populates the disk cache; a second run hits it.
    let ws2 = tmp("disk2");
    let dc2 = tmp("disk-cache2");
    std::fs::write(ws2.join("i"), "z").unwrap();
    let s2 = spawn(&["sh", "-c", "echo built > out"], &["i"], &["out"]);
    let opts = RemoteOptions::from_source(&options_from_args(&["--remote_download_outputs=all", &format!("--disk_cache={}", dc2.display())])).unwrap();
    let fb = counting();
    let r3 = RemoteSpawnRunner::new(opts, None, fb.clone()).unwrap();
    assert_eq!(run(&r3, &s2, &ws2).runner_name, "local");
    std::fs::remove_file(ws2.join("out")).unwrap();
    assert_eq!(run(&r3, &s2, &ws2).runner_name, "disk cache hit");
    assert_eq!(std::fs::read_to_string(ws2.join("out")).unwrap(), "built\n");
    assert_eq!(fb.n.load(Ordering::SeqCst), 1);
    assert_eq!(r3.stats.summary(), "2 processes: 1 disk cache hit, 1 local");
}

#[test]
fn remote_cache_hit_populates_disk_cache_on_download() {
    let server = FakeReapiServer::new();
    let ws = tmp("pop");
    let dc = tmp("pop-dc");
    std::fs::write(ws.join("i"), "q").unwrap();
    let s = spawn(&["sh", "-c", "echo remote-made > o"], &["i"], &["o"]);
    let r = runner(&server, false, &[], counting());
    run(&r, &s, &ws); // uploads
    std::fs::remove_file(ws.join("o")).unwrap();
    let dcs = format!("--disk_cache={}", dc.display());
    let r2 = runner(&server, false, &["--remote_download_outputs=all", &dcs], counting());
    assert_eq!(run(&r2, &s, &ws).runner_name, "remote cache hit");
    let d = crate::digest::DigestFn::Sha256.digest(b"remote-made\n");
    assert!(dc.join("cas").join(&d.hash[..2]).join(&d.hash).is_file(), "downloaded blob lands in disk cache");
}

#[test]
fn lazy_outputs_toplevel_mode_and_materialization() {
    let server = FakeReapiServer::new();
    install_upper(&server);
    let ws = tmp("lazy");
    std::fs::write(ws.join("a.txt"), "lazy").unwrap();
    let r = runner(&server, true, &["--remote_download_outputs=toplevel"], counting());
    let s1 = spawn(&["upper", "a.txt", "mid.txt"], &["a.txt"], &["mid.txt"]);
    let res = run(&r, &s1, &ws);
    assert_eq!(res.status, SpawnStatus::Success);
    assert!(!ws.join("mid.txt").exists(), "intermediate output must not be downloaded");
    assert_eq!(r.pending_remote_outputs(), 1);
    // A dependent remote action consumes it by digest only.
    let s2 = spawn(&["upper", "mid.txt", "final.txt"], &["mid.txt"], &["final.txt"]);
    let res = run(&r, &s2, &ws);
    assert_eq!(res.status, SpawnStatus::Success, "{}", String::from_utf8_lossy(&res.stderr));
    assert!(!ws.join("mid.txt").exists());
    assert!(!ws.join("final.txt").exists());
    // Top-level target: download on demand.
    assert_eq!(r.materialize(&["final.txt".to_string()], &ws).unwrap(), 1);
    assert_eq!(std::fs::read_to_string(ws.join("final.txt")).unwrap(), "LAZY");
    assert_eq!(r.pending_remote_outputs(), 1);
    // A local spawn needing the lazy intermediate triggers materialization first.
    let fb = counting();
    let r_local = runner(&server, false, &["--remote_download_outputs=toplevel"], fb.clone());
    let _ = r_local; // (lazy state is per-runner; use the same runner with a no-exec spawn instead)
    let mut s3 = spawn(&["sh", "-c", "cat mid.txt > copy.txt"], &["mid.txt"], &["copy.txt"]);
    s3.execution_info.insert("local".into(), "".into());
    let res = run(&r, &s3, &ws);
    assert_eq!(res.status, SpawnStatus::Success, "{}", String::from_utf8_lossy(&res.stderr));
    assert_eq!(std::fs::read_to_string(ws.join("copy.txt")).unwrap(), "LAZY");
    assert_eq!(r.pending_remote_outputs(), 0);
}

#[test]
fn download_all_mode_downloads_everything_minimal_does_not() {
    let server = FakeReapiServer::new();
    install_upper(&server);
    let ws = tmp("modes");
    std::fs::write(ws.join("a"), "m").unwrap();
    let s = spawn(&["upper", "a", "o"], &["a"], &["o"]);
    let r = runner(&server, true, &["--remote_download_outputs=minimal"], counting());
    run(&r, &s, &ws);
    assert!(!ws.join("o").exists());
    // Test actions always get their outputs.
    let mut t = spawn(&["upper", "a", "t.log"], &["a"], &["t.log"]);
    t.mnemonic = "TestRunner".into();
    run(&r, &t, &ws);
    assert!(ws.join("t.log").exists());
}

#[test]
fn remote_failures_fallback_and_errors() {
    let server = FakeReapiServer::new();
    server.set_executor(|_| Err(crate::grpc::GrpcStatus::new(13, "boom")));
    let ws = tmp("fail");
    std::fs::write(ws.join("i"), "x").unwrap();
    let s = spawn(&["sh", "-c", "echo local > o"], &["i"], &["o"]);

    let fb = counting();
    let r = runner(&server, true, &[], fb.clone());
    let res = run(&r, &s, &ws);
    assert_eq!(res.status, SpawnStatus::ExecutionFailed);
    assert!(String::from_utf8_lossy(&res.stderr).contains("Remote execution failed"));
    assert_eq!(fb.n.load(Ordering::SeqCst), 0);

    let fb = counting();
    let r = runner(&server, true, &["--remote_local_fallback"], fb.clone());
    let res = run(&r, &s, &ws);
    assert!(res.is_ok());
    assert_eq!(res.runner_name, "local");
    assert_eq!(fb.n.load(Ordering::SeqCst), 1);
    assert!(r.take_warnings().iter().any(|w| w.contains("falling back")));

    // Deadline exceeded is reported as a timeout, not retried locally.
    server.set_executor(|_| Err(crate::grpc::GrpcStatus::new(4, "too slow")));
    let fb = counting();
    let r = runner(&server, true, &["--remote_local_fallback"], fb.clone());
    let s_slow = spawn(&["sh", "-c", "echo slow > o"], &["i"], &["o"]);
    let res = run(&r, &s_slow, &ws);
    assert_eq!(res.status, SpawnStatus::Timeout);
    assert_eq!(fb.n.load(Ordering::SeqCst), 0);
}

#[test]
fn nonzero_remote_exit_code_is_reported() {
    let server = FakeReapiServer::new();
    server.set_executor(|_| Ok(ActionResult { exit_code: 3, stderr_raw: b"bad\n".to_vec(), ..Default::default() }));
    let ws = tmp("exit3");
    std::fs::write(ws.join("i"), "x").unwrap();
    let s = spawn(&["tool"], &["i"], &["o"]);
    let r = runner(&server, true, &["--remote_download_outputs=all"], counting());
    let res = run(&r, &s, &ws);
    assert_eq!((res.status, res.exit_code), (SpawnStatus::NonZeroExit, 3));
    assert_eq!(res.stderr, b"bad\n");
    assert_eq!(server.ac_count(), 0, "failed results are never cached");
}

#[test]
fn missing_declared_output_is_an_error() {
    let server = FakeReapiServer::new();
    server.set_executor(|_| Ok(ActionResult::default()));
    let ws = tmp("noout");
    std::fs::write(ws.join("i"), "x").unwrap();
    let s = spawn(&["tool"], &["i"], &["expected"]);
    let r = runner(&server, true, &["--remote_download_outputs=all"], counting());
    let res = run(&r, &s, &ws);
    assert_eq!(res.status, SpawnStatus::ExecutionFailed);
    assert!(String::from_utf8_lossy(&res.stderr).contains("was not created"));
}

#[test]
fn lost_cas_blob_triggers_reupload_and_retry() {
    let server = FakeReapiServer::new();
    install_upper(&server);
    let ws = tmp("lost");
    std::fs::write(ws.join("a"), "reupload-me").unwrap();
    let r = runner(&server, true, &["--remote_download_outputs=all"], counting());
    let s = spawn(&["upper", "a", "o"], &["a"], &["o"]);
    let d = crate::digest::DigestFn::Sha256.digest(b"reupload-me");
    // Server claims to have the input, then loses it.
    server.put_blob(b"reupload-me");
    server.forget_after_next_find_missing(&d.hash);
    let res = run(&r, &s, &ws);
    assert_eq!(res.status, SpawnStatus::Success, "{}", String::from_utf8_lossy(&res.stderr));
    assert_eq!(std::fs::read_to_string(ws.join("o")).unwrap(), "REUPLOAD-ME");
    assert_eq!(server.request_count("Execute"), 2, "execute was retried after re-upload");
}

#[test]
fn output_directories_roundtrip_through_tree_blobs() {
    let server = FakeReapiServer::new();
    let ws1 = tmp("tree1");
    let ws2 = tmp("tree2");
    std::fs::write(ws1.join("i"), "x").unwrap();
    std::fs::write(ws2.join("i"), "x").unwrap();
    let mut s = spawn(
        &["sh", "-c", "mkdir -p outdir/sub && echo 1 > outdir/a && echo 2 > outdir/sub/b && ln -s a outdir/lnk"],
        &["i"],
        &["outdir"],
    );
    mark_tree_output(&mut s, "outdir");
    let r1 = runner(&server, false, &[], counting());
    let res = run(&r1, &s, &ws1);
    assert!(res.is_ok(), "{}", String::from_utf8_lossy(&res.stderr));
    let r2 = runner(&server, false, &["--remote_download_outputs=all"], counting());
    let res = run(&r2, &s, &ws2);
    assert_eq!(res.runner_name, "remote cache hit");
    assert_eq!(std::fs::read_to_string(ws2.join("outdir/a")).unwrap(), "1\n");
    assert_eq!(std::fs::read_to_string(ws2.join("outdir/sub/b")).unwrap(), "2\n");
    #[cfg(unix)]
    assert_eq!(std::fs::read_link(ws2.join("outdir/lnk")).unwrap(), Path::new("a"));
}

#[test]
fn command_proto_matches_bazel_conventions() {
    let server = FakeReapiServer::new();
    let seen: Arc<std::sync::Mutex<Option<(Command, Action)>>> = Arc::new(std::sync::Mutex::new(None));
    let seen2 = seen.clone();
    server.set_executor(move |ctx| {
        *seen2.lock().unwrap() = Some((ctx.command.clone(), ctx.action.clone()));
        Ok(ActionResult::default())
    });
    let ws = tmp("cmdproto");
    std::fs::write(ws.join("i"), "x").unwrap();
    let mut s = spawn(&["tool", "--flag"], &["i"], &["z.out", "a.out"]);
    s.env.insert("B".into(), "2".into());
    s.env.insert("A".into(), "1".into());
    s.execution_info.insert("timeout".into(), "30".into());
    let r = runner(&server, true, &["--remote_default_exec_properties=OSFamily=linux", "--remote_default_exec_properties=Arch=x86"], counting());
    let _ = run(&r, &s, &ws);
    let (cmd, action) = seen.lock().unwrap().clone().unwrap();
    assert_eq!(cmd.arguments, ["tool", "--flag"]);
    let env: Vec<_> = cmd.environment_variables.iter().map(|e| (e.name.as_str(), e.value.as_str())).collect();
    assert_eq!(env, [("A", "1"), ("B", "2"), ("LANG", "C")], "env sorted by name");
    assert_eq!(cmd.output_paths, ["a.out", "z.out"], "REAPI 2.1+ output_paths sorted");
    assert!(cmd.output_files.is_empty());
    let props: Vec<_> = cmd.platform.unwrap().properties.iter().map(|p| (p.name.clone(), p.value.clone())).collect();
    assert_eq!(props, [("Arch".to_string(), "x86".to_string()), ("OSFamily".to_string(), "linux".to_string())]);
    assert_eq!(action.timeout.unwrap().seconds, 30);
    assert!(!action.do_not_cache);
    let salt = CacheSalt::decode(&action.salt).unwrap();
    assert!(salt.may_be_executed_remotely);

    // Older servers (< 2.1) get output_files / output_directories instead.
    server.configure(|c| c.high_api_version = SemVer { major: 2, minor: 0, ..Default::default() });
    let r = runner(&server, true, &[], counting());
    let mut s2 = s.clone();
    s2.env.insert("C".into(), "3".into());
    let _ = run(&r, &s2, &ws);
    let (cmd, _) = seen.lock().unwrap().clone().unwrap();
    assert_eq!(cmd.output_files, ["a.out", "z.out"]);
    assert!(cmd.output_paths.is_empty());
}

#[test]
fn no_cache_tag_sets_do_not_cache_and_missing_input_goes_to_local() {
    let server = FakeReapiServer::new();
    let seen: Arc<std::sync::Mutex<Vec<bool>>> = Arc::new(std::sync::Mutex::new(vec![]));
    let seen2 = seen.clone();
    server.set_executor(move |ctx| {
        seen2.lock().unwrap().push(ctx.action.do_not_cache);
        Ok(ActionResult::default())
    });
    let ws = tmp("nocache");
    std::fs::write(ws.join("i"), "x").unwrap();
    let r = runner(&server, true, &[], counting());
    let mut s = spawn(&["t"], &["i"], &[]);
    s.execution_info.insert("no-cache".into(), "".into());
    run(&r, &s, &ws);
    assert_eq!(*seen.lock().unwrap(), vec![true]);
    assert_eq!(server.ac_count(), 0);

    // A missing declared input: the local runner produces the canonical diagnostics.
    let fb = counting();
    let r = runner(&server, true, &[], fb.clone());
    let res = run(&r, &spawn(&["t"], &["does-not-exist"], &[]), &ws);
    assert_eq!(res.status, SpawnStatus::MissingInputs);
    assert_eq!(fb.n.load(Ordering::SeqCst), 1);
}
