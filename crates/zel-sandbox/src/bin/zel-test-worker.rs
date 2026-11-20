//! A tiny persistent worker used by the end-to-end worker tests. Speaks the proto
//! (varint-delimited) and JSON protocols; supports multiplexing by request id and request
//! cancellation via [`WorkerCancellationRegistry`].
//!
//! Request arguments (after flagfile expansion): `--out=PATH`, `--text=STRING`, `--exit=N`,
//! `--sleep_ms=N`, `--barrier=N`, `--barrier_timeout_ms=N`, `--ready_file=PATH`.
//! The output file receives `<pid> <request count> <text> inputs=<n>`.

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use zel_proto::worker_protocol::{WorkRequest, WorkResponse};
use zel_sandbox::worker::*;

struct CancelSignal {
    reg: WorkerCancellationRegistry,
    lock: Mutex<()>,
    cv: Condvar,
}

impl CancelSignal {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            reg: WorkerCancellationRegistry::new(),
            lock: Mutex::new(()),
            cv: Condvar::new(),
        })
    }

    fn cancel_request(&self, request_id: i32) {
        let _guard = self.lock.lock().unwrap();
        self.reg.register_request(request_id);
        self.cv.notify_all();
    }
}

#[derive(Default)]
struct BarrierTable {
    groups: HashMap<usize, BarrierGroup>,
}

struct BarrierGroup {
    arrived: usize,
    generation: u64,
}

struct BarrierSync {
    table: Mutex<BarrierTable>,
    cv: Condvar,
}

impl BarrierSync {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            table: Mutex::new(BarrierTable::default()),
            cv: Condvar::new(),
        })
    }

    /// Blocks until `target` requests have entered this barrier concurrently within the same
    /// worker process, or returns `Err` on timeout / cancellation.
    fn arrive_and_wait(
        &self,
        target: usize,
        timeout: Duration,
        request_id: i32,
        cancel_signal: &CancelSignal,
    ) -> Result<(), Option<String>> {
        if target <= 1 {
            return Ok(());
        }
        let deadline = Instant::now() + timeout;
        let mut guard = self.table.lock().unwrap();
        let my_gen = {
            let group = guard.groups.entry(target).or_insert(BarrierGroup {
                arrived: 0,
                generation: 0,
            });
            let g = group.generation;
            group.arrived += 1;
            if group.arrived >= target {
                group.arrived = 0;
                group.generation += 1;
                self.cv.notify_all();
                return Ok(());
            }
            g
        };

        loop {
            if cancel_signal.reg.check_if_request_is_cancelled(request_id) {
                if let Some(group) = guard.groups.get_mut(&target) {
                    if group.generation == my_gen {
                        group.arrived = group.arrived.saturating_sub(1);
                    }
                }
                return Err(None);
            }
            let now = Instant::now();
            if now >= deadline {
                let arrived = if let Some(group) = guard.groups.get_mut(&target) {
                    let a = group.arrived;
                    if group.generation == my_gen {
                        group.arrived = group.arrived.saturating_sub(1);
                    }
                    a
                } else {
                    0
                };
                return Err(Some(format!(
                    "barrier timeout: expected {target} concurrent requests in pid {}, only {arrived} arrived\n",
                    std::process::id()
                )));
            }
            let slice = (deadline - now).min(Duration::from_millis(20));
            let (next_guard, _) = self.cv.wait_timeout(guard, slice).unwrap();
            guard = next_guard;
            if guard
                .groups
                .get(&target)
                .is_some_and(|group| group.generation != my_gen)
            {
                return Ok(());
            }
        }
    }
}

fn cancelled_response(request_id: i32) -> WorkResponse {
    WorkResponse {
        exit_code: 1,
        output: String::new(),
        request_id,
        was_cancelled: true,
    }
}

fn resolve_worker_path(sandbox_dir: &str, rel: &str) -> String {
    if sandbox_dir.is_empty() {
        rel.to_string()
    } else {
        format!("{sandbox_dir}/{rel}")
    }
}

fn handle(
    req: &WorkRequest,
    count: u64,
    cancel_signal: &CancelSignal,
    barriers: &BarrierSync,
) -> WorkResponse {
    let mut out = None;
    let mut text = String::new();
    let mut exit = 0;
    let mut sleep = 0u64;
    let mut barrier = None::<usize>;
    let mut barrier_timeout_ms = 10_000u64;
    let mut ready_file = None::<String>;
    for a in &req.arguments {
        if let Some(v) = a.strip_prefix("--out=") {
            out = Some(v.to_string());
        } else if let Some(v) = a.strip_prefix("--text=") {
            text = v.to_string();
        } else if let Some(v) = a.strip_prefix("--exit=") {
            exit = v.parse().unwrap_or(0);
        } else if let Some(v) = a.strip_prefix("--sleep_ms=") {
            sleep = v.parse().unwrap_or(0);
        } else if let Some(v) = a.strip_prefix("--barrier=") {
            barrier = v.parse().ok();
        } else if let Some(v) = a.strip_prefix("--barrier_timeout_ms=") {
            barrier_timeout_ms = v.parse().unwrap_or(10_000);
        } else if let Some(v) = a.strip_prefix("--ready_file=") {
            ready_file = Some(v.to_string());
        }
    }
    if cancel_signal
        .reg
        .check_if_request_is_cancelled(req.request_id)
    {
        cancel_signal.reg.unregister_request(req.request_id);
        return cancelled_response(req.request_id);
    }
    if let Some(rf) = ready_file {
        let _ = std::fs::write(resolve_worker_path(&req.sandbox_dir, &rf), b"ready\n");
    }
    if let Some(target) = barrier {
        if let Err(reason) = barriers.arrive_and_wait(
            target,
            Duration::from_millis(barrier_timeout_ms),
            req.request_id,
            cancel_signal,
        ) {
            cancel_signal.reg.unregister_request(req.request_id);
            return match reason {
                None => cancelled_response(req.request_id),
                Some(msg) => WorkResponse {
                    exit_code: 1,
                    output: msg,
                    request_id: req.request_id,
                    was_cancelled: false,
                },
            };
        }
    }
    if sleep > 0 {
        let deadline = Instant::now() + Duration::from_millis(sleep);
        let mut guard = cancel_signal.lock.lock().unwrap();
        loop {
            if cancel_signal
                .reg
                .check_if_request_is_cancelled(req.request_id)
            {
                drop(guard);
                cancel_signal.reg.unregister_request(req.request_id);
                return cancelled_response(req.request_id);
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (next_guard, _) = cancel_signal
                .cv
                .wait_timeout(guard, deadline - now)
                .unwrap();
            guard = next_guard;
        }
    }
    if cancel_signal
        .reg
        .check_if_request_is_cancelled(req.request_id)
    {
        cancel_signal.reg.unregister_request(req.request_id);
        return cancelled_response(req.request_id);
    }
    if let Some(o) = out {
        let base = resolve_worker_path(&req.sandbox_dir, &o);
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
    cancel_signal.reg.unregister_request(req.request_id);
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
    let cancel_signal = CancelSignal::new();
    let barriers = BarrierSync::new();
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
                cancel_signal.cancel_request(req.request_id);
            }
            continue;
        }
        active_ids.lock().unwrap().insert(req.request_id);
        let n = count.fetch_add(1, Ordering::SeqCst) + 1;
        let stdout = stdout.clone();
        let cancel_sig = cancel_signal.clone();
        let bar = barriers.clone();
        let active = active_ids.clone();
        let gate = singleplex_gate.clone();
        std::thread::spawn(move || {
            let _seq = if req.request_id == 0 {
                Some(gate.lock().unwrap())
            } else {
                None
            };
            let resp = handle(&req, n, &cancel_sig, &bar);
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
