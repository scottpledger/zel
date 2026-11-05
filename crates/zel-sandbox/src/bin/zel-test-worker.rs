//! A tiny persistent worker used by the end-to-end worker tests. Speaks the proto
//! (varint-delimited) and JSON protocols; supports multiplexing by request id.
//!
//! Request arguments (after flagfile expansion): `--out=PATH`, `--text=STRING`, `--exit=N`,
//! `--sleep_ms=N`. The output file receives `<pid> <request count> <text> <sandbox_dir>`.

use std::io::{BufReader, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use zel_proto::worker_protocol::{WorkRequest, WorkResponse};
use zel_sandbox::worker::*;

fn handle(req: &WorkRequest, count: u64) -> WorkResponse {
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
    if sleep > 0 {
        std::thread::sleep(std::time::Duration::from_millis(sleep));
    }
    if let Some(o) = out {
        let base = if req.sandbox_dir.is_empty() { o.clone() } else { format!("{}/{}", req.sandbox_dir, o) };
        let _ = std::fs::write(
            base,
            format!("{} {} {} inputs={}\n", std::process::id(), count, text, req.inputs.len()),
        );
    }
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
            continue;
        }
        let n = count.fetch_add(1, Ordering::SeqCst) + 1;
        let stdout = stdout.clone();
        let respond = move |resp: WorkResponse| {
            let mut o = stdout.lock().unwrap();
            if json {
                let s = format!(
                    "{{\"exitCode\":{},\"output\":{},\"requestId\":{}}}",
                    resp.exit_code,
                    json_escape(&resp.output),
                    resp.request_id
                );
                o.write_all(s.as_bytes()).unwrap();
                o.flush().unwrap();
            } else {
                write_delimited(&mut *o, &resp.encode_to_vec()).unwrap();
            }
        };
        if req.request_id != 0 {
            // Multiplex: handle concurrently.
            std::thread::spawn(move || respond(handle(&req, n)));
        } else {
            respond(handle(&req, n));
        }
    }
}
