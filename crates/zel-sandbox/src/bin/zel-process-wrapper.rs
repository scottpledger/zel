//! `process-wrapper` as a standalone executable (port of `src/main/tools/process-wrapper.cc`).

use zel_sandbox::process_wrapper::{parse_args, record_wrapper_signal, run, Termination, SIGTERM};

#[cfg(unix)]
const SIGINT: i32 = 2;

#[cfg(unix)]
extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
    fn kill(pid: i32, sig: i32) -> i32;
    fn getpid() -> i32;
}

#[cfg(target_os = "linux")]
extern "C" {
    fn prctl(option: i32, arg2: usize, arg3: usize, arg4: usize, arg5: usize) -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}

#[cfg(target_os = "linux")]
const PR_SET_CHILD_SUBREAPER: i32 = 36;
#[cfg(target_os = "linux")]
const WNOHANG: i32 = 1;

#[cfg(unix)]
extern "C" fn on_signal(sig: i32) {
    record_wrapper_signal(sig);
}

fn main() {
    #[cfg(target_os = "linux")]
    unsafe {
        // Become a child subreaper so double-forked descendants are reparented to us.
        let _ = prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
    }
    #[cfg(unix)]
    unsafe {
        signal(SIGINT, on_signal as *const () as usize);
        signal(SIGTERM, on_signal as *const () as usize);
    }
    let argv: Vec<String> = std::env::args().collect();
    let program = argv[0].clone();
    let opt = match parse_args(&program, &argv[1..]) {
        Ok(o) => o,
        Err(msg) => {
            eprint!("{msg}");
            std::process::exit(1);
        }
    };
    let outcome = match run(&opt, None, None) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("process-wrapper: execvp({}, ...): {e}", opt.args[0]);
            std::process::exit(1);
        }
    };
    use std::io::Write;
    #[cfg(target_os = "linux")]
    unsafe {
        // Reap any orphaned descendants reparented to this standalone subreaper process.
        loop {
            let mut st: i32 = 0;
            if waitpid(-1, &mut st, WNOHANG) <= 0 {
                break;
            }
        }
    }
    let _ = std::io::stdout().write_all(&outcome.stdout);
    let _ = std::io::stderr().write_all(&outcome.stderr);
    match outcome.termination {
        // The C++ wrapper re-raises the child's (or the timeout's) signal with the default
        // disposition so that its own parent observes death by signal.
        Termination::Signaled(sig) => {
            #[cfg(unix)]
            unsafe {
                signal(sig, 0);
                kill(getpid(), sig);
            }
            std::process::exit(128 + sig);
        }
        Termination::Exited(c) => std::process::exit(c),
    }
}
