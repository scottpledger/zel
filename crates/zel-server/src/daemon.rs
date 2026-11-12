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

//! Persistent `CommandServer` daemon speaking the real Bazel `command_server.CommandServer` gRPC
//! protocol (`Run` / `Ping` / `Cancel`) over HTTP/2 (h2c) on a loopback TCP port, using the pure
//! Rust stack in `zel-remote`.  Mirrors `GrpcServerImpl.java` / `CommandManager.java` /
//! `BlazeCommandDispatcher` command locking.

use crate::BlazeRuntime;
use std::collections::HashMap;
use std::io;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use zel_proto::command_server::{
    CancelRequest, CancelResponse, PingRequest, PingResponse, RunRequest, RunResponse,
};
use zel_proto::failure_details::{Category, CommandCode, InterruptedCode, FailureDetail, GrpcServerCode};
use zel_remote::grpc::{frame_message, percent_encode, MessageParser};
use zel_remote::hpack::Header;
use zel_remote::http2::{Conn, Event, LocalSettings, Stream};
use zel_remote::transport::Transport;
use zel_util::ExitCode;

pub const SERVICE: &str = "/command_server.CommandServer/";

/// Chunk size for streamed stdout/stderr (`RunResponse.standard_output`).
const OUTPUT_CHUNK: usize = 8192;

/// Daemon configuration (from the server startup options).
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub request_cookie: String,
    pub response_cookie: String,
    /// `--max_idle_secs`; 0 means never shut down.
    pub max_idle_secs: u64,
    /// `--shutdown_on_low_sys_mem`: exit when idle and the system is low on free memory.
    pub shutdown_on_low_sys_mem: bool,
    /// `--idle_server_tasks`: housekeeping while idle (dropping the in-memory action cache
    /// once the server has been idle for a while, the native analog of the idle GC).
    pub idle_server_tasks: bool,
}

struct Daemon {
    rt: Arc<BlazeRuntime>,
    cfg: DaemonConfig,
    /// Description of the client currently holding the command lock (`pid=N`).
    current: Mutex<Option<String>>,
    cv: Condvar,
    cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
    next_id: AtomicU64,
    last_activity: Mutex<Instant>,
    running: Mutex<usize>,
    shutdown: AtomicBool,
}

impl Daemon {
    fn touch(&self) {
        *self.last_activity.lock().unwrap() = Instant::now();
    }
}

fn header<'a>(h: &'a [Header], name: &str) -> Option<&'a str> {
    h.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

fn send_ok_headers(st: &Stream) -> io::Result<()> {
    st.send_headers(
        &[
            (":status".into(), "200".into()),
            ("content-type".into(), "application/grpc".into()),
        ],
        false,
    )
}

fn finish_ok(st: &Stream) -> io::Result<()> {
    st.send_headers(&[("grpc-status".into(), "0".into())], true)
}

fn finish_err(st: &Stream, code: i32, msg: &str) -> io::Result<()> {
    st.send_headers(
        &[
            (":status".into(), "200".into()),
            ("content-type".into(), "application/grpc".into()),
            ("grpc-status".into(), code.to_string()),
            ("grpc-message".into(), percent_encode(msg)),
        ],
        true,
    )
}

fn send_msg(st: &Stream, bytes: &[u8]) -> io::Result<()> {
    st.send_data(&frame_message(bytes), false, None)
}

/// Serves one accepted transport on a background thread.
fn serve_transport(d: &Arc<Daemon>, t: Transport) {
    let d = Arc::clone(d);
    let _ = std::thread::Builder::new().name("zel-grpc-conn".into()).spawn(move || {
        let Ok((conn, rx)) = Conn::server_from(t, LocalSettings::default()) else {
            return;
        };
        while let Ok(stream) = rx.recv() {
            let d = Arc::clone(&d);
            let _ = std::thread::spawn(move || {
                let _ = handle_stream(&d, stream);
            });
        }
        // Peer went away: release the socket instead of leaving it in CLOSE_WAIT.
        conn.close();
    });
}

fn handle_stream(d: &Arc<Daemon>, st: Stream) -> io::Result<()> {
    let mut headers: Vec<Header> = Vec::new();
    let mut parser = MessageParser::default();
    let mut msgs: Vec<Vec<u8>> = Vec::new();
    loop {
        match st.recv(None)? {
            Event::Headers(h) => headers = h,
            Event::Data(data) => {
                parser.push(&data);
                while let Ok(Some(m)) = parser.next() {
                    msgs.push(m);
                }
            }
            Event::End => break,
            Event::Trailers(_) => {}
            Event::Reset(_) => return Ok(()),
        }
    }
    let path = header(&headers, ":path").unwrap_or("").to_string();
    let first = msgs.first().cloned().unwrap_or_default();
    d.touch();
    let r = match path.strip_prefix(SERVICE) {
        Some("Ping") => handle_ping(d, &st, &first),
        Some("Cancel") => handle_cancel(d, &st, &first),
        Some("Run") => handle_run(d, &st, &first),
        _ => finish_err(&st, 12, &format!("unknown method {path}")),
    };
    d.touch();
    r
}

fn handle_ping(d: &Daemon, st: &Stream, payload: &[u8]) -> io::Result<()> {
    let Ok(req) = PingRequest::decode(payload) else {
        return finish_err(st, 3, "bad PingRequest");
    };
    if req.cookie != d.cfg.request_cookie {
        return finish_err(st, 7, "bad cookie");
    }
    send_ok_headers(st)?;
    let resp = PingResponse { cookie: d.cfg.response_cookie.clone() };
    send_msg(st, &resp.encode_to_vec())?;
    finish_ok(st)
}

fn handle_cancel(d: &Daemon, st: &Stream, payload: &[u8]) -> io::Result<()> {
    let Ok(req) = CancelRequest::decode(payload) else {
        return finish_err(st, 3, "bad CancelRequest");
    };
    if req.cookie != d.cfg.request_cookie {
        return finish_err(st, 7, "bad cookie");
    }
    if let Some(flag) = d.cancels.lock().unwrap().get(&req.command_id) {
        flag.store(true, Ordering::SeqCst);
        // `BlazeCommandDispatcher`: the running command's thread is interrupted; spawns in flight die.
        zel_sandbox::process_wrapper::request_global_cancel();
    }
    d.cv.notify_all();
    send_ok_headers(st)?;
    send_msg(st, &CancelResponse { cookie: d.cfg.response_cookie.clone() }.encode_to_vec())?;
    finish_ok(st)
}

fn run_response(d: &Daemon, id: &str) -> RunResponse {
    RunResponse {
        cookie: d.cfg.response_cookie.clone(),
        command_id: id.to_string(),
        ..Default::default()
    }
}

fn final_failure(d: &Daemon, id: &str, code: ExitCode, msg: &str, cat: Category) -> RunResponse {
    RunResponse {
        finished: true,
        exit_code: code.numeric_exit_code(),
        failure_detail: Some(FailureDetail::new(msg, cat)),
        ..run_response(d, id)
    }
}

fn handle_run(d: &Arc<Daemon>, st: &Stream, payload: &[u8]) -> io::Result<()> {
    let req = match RunRequest::decode(payload) {
        Ok(r) => r,
        Err(e) => return finish_err(st, 3, &format!("bad RunRequest: {e}")),
    };
    send_ok_headers(st)?;
    if req.cookie != d.cfg.request_cookie {
        let bad = RunResponse {
            cookie: d.cfg.response_cookie.clone(),
            finished: true,
            exit_code: ExitCode::LOCAL_ENVIRONMENTAL_ERROR.numeric_exit_code(),
            failure_detail: Some(FailureDetail::new(
                "Invalid RunRequest: bad cookie",
                Category::GrpcServer(GrpcServerCode::BadCookie),
            )),
            ..Default::default()
        };
        send_msg(st, &bad.encode_to_vec())?;
        return finish_ok(st);
    }

    let id = format!("{}-{}", d.rt.server_pid, d.next_id.fetch_add(1, Ordering::SeqCst));
    let cancel = Arc::new(AtomicBool::new(false));
    d.cancels.lock().unwrap().insert(id.clone(), Arc::clone(&cancel));
    // First message announces the command id (`GrpcServerImpl.run`).
    send_msg(st, &run_response(d, &id).encode_to_vec())?;

    *d.running.lock().unwrap() += 1;
    let result = run_locked(d, st, &req, &id, &cancel);
    d.cancels.lock().unwrap().remove(&id);
    *d.running.lock().unwrap() -= 1;
    let (final_resp, shut) = result?;
    send_msg(st, &final_resp.encode_to_vec())?;
    if shut {
        d.shutdown.store(true, Ordering::SeqCst);
    }
    finish_ok(st)
}

/// Takes the server-wide command lock exactly like `BlazeCommandDispatcher.execExclusively`, runs
/// the command on a worker thread (so Cancel / RST_STREAM can interrupt the wait) and streams
/// the output back.  Returns the final response and whether the server must shut down.
fn run_locked(
    d: &Arc<Daemon>,
    st: &Stream,
    req: &RunRequest,
    id: &str,
    cancel: &Arc<AtomicBool>,
) -> io::Result<(RunResponse, bool)> {
    let desc = if req.client_description.is_empty() {
        "unknown".to_string()
    } else {
        req.client_description.clone()
    };
    let interrupted = |d: &Daemon| {
        let mut r = final_failure(
            d,
            id,
            ExitCode::INTERRUPTED,
            "Build interrupted",
            Category::Interrupted(InterruptedCode::Interrupted),
        );
        r.standard_error = b"Build interrupted\n".to_vec();
        r
    };
    // ---- lock acquisition
    {
        let timeout: Option<Duration> = if req.block_for_lock { None } else { Some(Duration::ZERO) };
        let mut announced = String::new();
        let mut cur = d.current.lock().unwrap();
        while let Some(other) = cur.clone() {
            if cancel.load(Ordering::SeqCst) {
                return Ok((interrupted(d), false));
            }
            if timeout == Some(Duration::ZERO) {
                let msg = format!("Another command ({other}) is running. Exiting immediately.");
                let mut r = final_failure(
                    d,
                    id,
                    ExitCode::LOCK_HELD_NOBLOCK_FOR_LOCK,
                    &msg,
                    Category::Command(CommandCode::AnotherCommandRunning),
                );
                r.standard_error = format!("{msg}\n").into_bytes();
                return Ok((r, false));
            }
            let wait = Duration::from_millis(500);
            if announced != other {
                let m = format!(
                    "Another command ({other}) is running. Waiting for it to complete on the \
                     server (server_pid={})...\n",
                    d.rt.server_pid
                );
                let mut r = run_response(d, id);
                r.standard_error = m.into_bytes();
                send_msg(st, &r.encode_to_vec())?;
                announced = other;
            }
            cur = d.cv.wait_timeout(cur, wait).unwrap().0;
        }
        *cur = Some(desc);
        zel_sandbox::process_wrapper::clear_global_cancel();
    }

    // ---- run
    struct CommandLockGuard(Arc<Daemon>);
    impl Drop for CommandLockGuard {
        fn drop(&mut self) {
            *self.0.current.lock().unwrap() = None;
            self.0.cv.notify_all();
        }
    }

    let (tx, rx) = mpsc::channel::<RunResponse>();
    let sink = Arc::new(crate::live::LiveSink::default());
    {
        let sink = Arc::clone(&sink);
        let d2 = Arc::clone(d);
        let req2 = req.clone();
        // Starlark analysis (e.g. `@rules_shell` under `--instrument_test_targets`) recurses deeply: the command
        // thread gets a large (lazily committed) stack instead of the 2 MiB default.
        std::thread::Builder::new().name("zel-command".into()).stack_size(512 << 20).spawn(move || {
            let _guard = CommandLockGuard(Arc::clone(&d2));
            crate::live::set_current(Some(sink));
            let panic_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if req2.arg.iter().any(|a| a == b"--test_force_command_panic")
                    || std::env::var("ZEL_TEST_FORCE_PANIC").is_ok()
                {
                    panic!("test forced command panic");
                }
                d2.rt.dispatch_command(&req2, &d2.cfg.response_cookie)
            }));
            crate::live::set_current(None);
            match panic_res {
                Ok(resp) => {
                    let _ = tx.send(resp);
                }
                Err(err) => {
                    let msg = if let Some(s) = err.downcast_ref::<&str>() {
                        format!("command panicked: {s}")
                    } else if let Some(s) = err.downcast_ref::<String>() {
                        format!("command panicked: {s}")
                    } else {
                        "command thread panicked".to_string()
                    };
                    let mut resp = final_failure(
                        &d2,
                        "panic",
                        ExitCode::BLAZE_INTERNAL_ERROR,
                        &msg,
                        Category::Command(CommandCode::CommandFailureUnknown),
                    );
                    resp.standard_error = format!("INTERNAL ERROR: {msg}\n").into_bytes();
                    let _ = tx.send(resp);
                }
            }
        })?;
    }
    let mut cancelled_at: Option<Instant> = None;
    let mut out = loop {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(r) => break r,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Ok((
                    final_failure(
                        d,
                        id,
                        ExitCode::BLAZE_INTERNAL_ERROR,
                        "command thread panicked",
                        Category::Command(CommandCode::CommandFailureUnknown),
                    ),
                    false,
                ));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        // Live streaming of whatever the command's UI produced so far.
        let (so, se) = sink.drain();
        for (c, is_err) in [(se, true), (so, false)] {
            for chunk in c.chunks(OUTPUT_CHUNK) {
                let mut r = run_response(d, id);
                if is_err {
                    r.standard_error = chunk.to_vec();
                } else {
                    r.standard_output = chunk.to_vec();
                }
                send_msg(st, &r.encode_to_vec())?;
            }
        }
        if cancel.load(Ordering::SeqCst) {
            // `Cancel`: the spawns in flight were killed; give the command a moment to unwind and
            // report its (interrupted) result like Bazel does, instead of answering right away while the
            // command keeps holding the server lock.
            let since = *cancelled_at.get_or_insert_with(Instant::now);
            if since.elapsed() > Duration::from_secs(10) {
                return Ok((interrupted(d), false));
            }
            continue;
        }
        // A client that dies / cancels resets the Run stream.
        match st.recv(Some(Instant::now())) {
            Ok(Event::Reset(_)) => {
                cancel.store(true, Ordering::SeqCst);
                return Ok((interrupted(d), false));
            }
            // No event within the (immediate) deadline: the client is still waiting.
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
            // Connection gone: the client died, treat like cancellation.
            Err(_) => {
                cancel.store(true, Ordering::SeqCst);
                return Ok((interrupted(d), false));
            }
            _ => {}
        }
    };
    d.touch();
    if cancel.load(Ordering::SeqCst) || cancelled_at.is_some() {
        out.exit_code = ExitCode::INTERRUPTED.numeric_exit_code();
        // The killed spawn's error is not reported by Bazel (`build interrupted` is the only error).
        let text = String::from_utf8_lossy(&out.standard_error).into_owned();
        out.standard_error = text
            .split_inclusive('\n')
            .filter(|l| l.trim_end() != "ERROR: spawn cancelled")
            .collect::<String>()
            .into_bytes();
    }

    // ---- stream output in chunks, then the terminal message
    let stdout = std::mem::take(&mut out.standard_output);
    let stderr = std::mem::take(&mut out.standard_error);
    for c in stderr.chunks(OUTPUT_CHUNK) {
        let mut r = run_response(d, id);
        r.standard_error = c.to_vec();
        send_msg(st, &r.encode_to_vec())?;
    }
    for c in stdout.chunks(OUTPUT_CHUNK) {
        let mut r = run_response(d, id);
        r.standard_output = c.to_vec();
        send_msg(st, &r.encode_to_vec())?;
    }
    out.command_id = id.to_string();
    out.finished = true;
    let shut = out.termination_expected || d.rt.shutdown_requested.load(Ordering::SeqCst);
    Ok((out, shut))
}

/// Writes `server/jvm.out`-style status files, then serves the gRPC `CommandServer` until
/// shutdown (the `shutdown`/`clean` commands), idle timeout, or SIGTERM-driven removal of the
/// pid file.  Returns when the server should exit.
pub fn serve_grpc(
    listener: TcpListener,
    runtime: Arc<BlazeRuntime>,
    cfg: DaemonConfig,
) -> io::Result<()> {
    let addr = listener.local_addr()?.to_string();
    let server_dir = runtime.dirs.server_dir();
    crate::write_server_status_files(
        runtime.fs.as_ref(),
        &server_dir,
        runtime.server_pid as i32,
        &addr,
        &cfg.request_cookie,
        &cfg.response_cookie,
    )?;
    let d = Arc::new(Daemon {
        rt: runtime,
        cfg,
        current: Mutex::new(None),
        cv: Condvar::new(),
        cancels: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(1),
        last_activity: Mutex::new(Instant::now()),
        running: Mutex::new(0),
        shutdown: AtomicBool::new(false),
    });
    listener.set_nonblocking(true)?;
    crate::sys::install_handler(crate::sys::SIGTERM);
    let pid_file = server_dir.get_relative_str("server.pid.txt");
    let mut last_mem_check = Instant::now();
    let mut idle_tasks_done = false;
    loop {
        match listener.accept() {
            Ok((s, _)) => {
                let _ = s.set_nonblocking(false);
                let _ = s.set_nodelay(true);
                if let (Ok(r), Ok(w)) = (s.try_clone(), s.try_clone()) {
                    serve_transport(
                        &d,
                        Transport {
                            reader: Box::new(r),
                            writer: Box::new(w),
                            shutdown: Box::new(move || {
                                let _ = s.shutdown(std::net::Shutdown::Both);
                            }),
                        },
                    );
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {}
        }
        if crate::sys::LAST_SIGNAL.load(Ordering::SeqCst) == crate::sys::SIGTERM {
            let _ = zel_exec::layout::append_server_log(
                std::path::Path::new(d.rt.dirs.output_base.path_string()),
                d.rt.server_pid,
                zel_exec::layout::server_start_secs(),
                "I Received SIGTERM; shutting down.",
            );
            break;
        }
        if d.shutdown.load(Ordering::SeqCst) {
            // Let the final response flush to the client.
            std::thread::sleep(Duration::from_millis(100));
            break;
        }
        if d.cfg.max_idle_secs > 0
            && *d.running.lock().unwrap() == 0
            && d.last_activity.lock().unwrap().elapsed() >= Duration::from_secs(d.cfg.max_idle_secs)
        {
            let _ = zel_exec::layout::append_server_log(
                std::path::Path::new(d.rt.dirs.output_base.path_string()),
                d.rt.server_pid,
                zel_exec::layout::server_start_secs(),
                "I Shutting down due to idleness.",
            );
            break;
        }
        // `--shutdown_on_low_sys_mem`: only when nothing is running and the server has been idle.
        if d.cfg.shutdown_on_low_sys_mem
            && *d.running.lock().unwrap() == 0
            && d.last_activity.lock().unwrap().elapsed() >= Duration::from_secs(10)
            && last_mem_check.elapsed() >= Duration::from_secs(5)
        {
            last_mem_check = Instant::now();
            if crate::sys::free_memory_fraction().map(|f| f < 0.05).unwrap_or(false) {
                let _ = zel_exec::layout::append_server_log(
                    std::path::Path::new(d.rt.dirs.output_base.path_string()),
                    d.rt.server_pid,
                    zel_exec::layout::server_start_secs(),
                    "I Shutting down: the system is low on memory and the server is idle.",
                );
                break;
            }
        }
        // `--idle_server_tasks`: persist the action cache once, after 10s of idleness.
        if d.cfg.idle_server_tasks
            && !idle_tasks_done
            && *d.running.lock().unwrap() == 0
            && d.last_activity.lock().unwrap().elapsed() >= Duration::from_secs(10)
        {
            idle_tasks_done = true;
            if let Ok(cache) = d.rt.action_cache.lock() {
                let _ = cache.save_to_disk(d.rt.fs.as_ref(), &d.rt.dirs.action_cache_file());
            }
        }
        if d.last_activity.lock().unwrap().elapsed() < Duration::from_secs(1) {
            idle_tasks_done = false;
        }
        // If someone removed our pid file (e.g. `clean --expunge`), stop when no command is running.
        if *d.running.lock().unwrap() == 0 && !d.rt.fs.exists(&pid_file, true) {
            break;
        }
    }
    crate::cleanup_server_status_files(d.rt.fs.as_ref(), &server_dir);
    if d.rt.shutdown_requested.load(Ordering::SeqCst) {
        let _ = std::fs::remove_dir_all(d.rt.dirs.output_base.path_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_panic_releases_command_lock() {
        let current = Arc::new(Mutex::new(Some("test-panic".to_string())));
        let cv = Arc::new(Condvar::new());

        struct TestLockGuard {
            current: Arc<Mutex<Option<String>>>,
            cv: Arc<Condvar>,
        }
        impl Drop for TestLockGuard {
            fn drop(&mut self) {
                *self.current.lock().unwrap() = None;
                self.cv.notify_all();
            }
        }

        let cur_clone = Arc::clone(&current);
        let cv_clone = Arc::clone(&cv);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = TestLockGuard { current: cur_clone, cv: cv_clone };
            panic!("intentional test panic inside command");
        }));

        assert_eq!(*current.lock().unwrap(), None, "command lock must be released after panic");
    }
}

