//! A tiny persistent worker used by the end-to-end worker tests. Speaks the proto
//! (varint-delimited) and JSON protocols; supports multiplexing by request id and request
//! cancellation via [`WorkerCancellationRegistry`].
//!
//! Request arguments (after flagfile expansion): `--out=PATH`, `--text=STRING`, `--exit=N`,
//! `--sleep_ms=N`. The output file receives `<pid> <request count> <text> inputs=<n>`.

use std::collections::HashSet;
use std::io::{BufReader, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use zel_proto::worker_protocol::{WorkRequest, WorkResponse};
use zel_sandbox::worker::*;

fn cancelled_response(request_id: i32) -> WorkResponse {
    WorkResponse {
        exit_code: 1,
        output: String::new(),
        request_id,
        was_cancelled: true,
    }
}

fn handle(
    req: &WorkRequest,
    count: u64,
    cancellations: &WorkerCancellationRegistry,
) -> WorkResponse {
    let mut out = None;
    let mut text = String::new();
    let mut exit = 0;
    let mut sleep = 0u64;
    for a in &req.arguments {
        if let Some(v) = a.strip_prefix("--out=") {
            out = Some(v.to_string());
        } else if let Some(v) = a.strip_prefix("--text=") {
            text = v.to_string();
        } else if let Some(v) = a.strip_prefix("--exit=") {
            exit = v.parse().unwrap_or(0);
        } else if let Some(v) = a.strip_prefix("--sleep_ms=") {
            sleep = v.parse().unwrap_or(0);
        }
    }
    if cancellations.check_if_request_is_cancelled(req.request_id) {
        cancellations.unregister_request(req.request_id);
        return cancelled_response(req.request_id);
    }
    if sleep > 0 {
        let mut remaining = sleep;
        while remaining > 0 {
            let slice = remaining.min(5);
            std::thread::sleep(std::time::Duration::from_millis(slice));
            remaining -= slice;
            if cancellations.check_if_request_is_cancelled(req.request_id) {
                cancellations.unregister_request(req.request_id);
                return cancelled_response(req.request_id);
            }
        }
    }
    if cancellations.check_if_request_is_cancelled(req.request_id) {
        cancellations.unregister_request(req.request_id);
        return cancelled_response(req.request_id);
    }
    if let Some(o) = out {
        let base = if req.sandbox_dir.is_empty() {
            o
        } else {
            format!("{}/{}", req.sandbox_dir, o)
        };
        let _ = std::fs::write(
            base,
            format!(
                "{} {} {} inputs={}\n",
                std::process::id(),
                count,
                text,
                req.inputs.len()
            ),
        );
    }
    cancellations.unregister_request(req.request_id);
    WorkResponse {
        exit_code: exit,
        output: format!("worker says: {text}\n"),
        request_id: req.request_id,
        was_cancelled: false,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == "--persistent_worker") {
        eprintln!("zel-test-worker: must be run with --persistent_worker");
        std::process::exit(2);
    }
    let json = args.iter().any(|a| a == "--json");
    eprintln!("worker started pid={}", std::process::id());
    let count = Arc::new(AtomicU64::new(0));
    let stdout = Arc::new(Mutex::new(std::io::stdout()));
    let cancellations = WorkerCancellationRegistry::new();
    let active_ids = Arc::new(Mutex::new(HashSet::<i32>::new()));
    // Singleplex requests (`request_id == 0`) execute sequentially on a background lock while the
    // main thread continues reading stdin so `cancel: true` requests arrive mid-execution.
    let singleplex_gate = Arc::new(Mutex::new(()));
    let mut stdin = BufReader::new(std::io::stdin());
    loop {
        let req = if json {
            match read_json_object(&mut stdin).unwrap() {
                None => return,
                Some(s) => work_request_from_json(&s).unwrap(),
            }
        } else {
            match read_delimited(&mut stdin).unwrap() {
                None => return,
                Some(b) => WorkRequest::decode(&b).unwrap(),
            }
        };
        if req.cancel {
            if active_ids.lock().unwrap().contains(&req.request_id) {
                cancellations.register_request(req.request_id);
            }
            continue;
        }
        active_ids.lock().unwrap().insert(req.request_id);
        let n = count.fetch_add(1, Ordering::SeqCst) + 1;
        let stdout = stdout.clone();
        let reg = cancellations.clone();
        let active = active_ids.clone();
        let gate = singleplex_gate.clone();
        std::thread::spawn(move || {
            let _seq = if req.request_id == 0 {
                Some(gate.lock().unwrap())
            } else {
                None
            };
            let resp = handle(&req, n, &reg);
            active.lock().unwrap().remove(&req.request_id);
            let mut o = stdout.lock().unwrap();
            if json {
                let cancelled_field = if resp.was_cancelled {
                    ",\"wasCancelled\":true"
                } else {
                    ""
                };
                let s = format!(
                    "{{\"exitCode\":{},\"output\":{},\"requestId\":{}{}}}\n",
                    resp.exit_code,
                    json_escape(&resp.output),
                    resp.request_id,
                    cancelled_field
                );
                o.write_all(s.as_bytes()).unwrap();
                o.flush().unwrap();
            } else {
                write_delimited(&mut *o, &resp.encode_to_vec()).unwrap();
            }
        });
    }
}
