//! `process-wrapper` as a standalone executable (port of `src/main/tools/process-wrapper.cc`).

use zel_sandbox::process_wrapper::{parse_args, run, Termination};

extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
    fn kill(pid: i32, sig: i32) -> i32;
    fn getpid() -> i32;
}

fn main() {
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
    let _ = std::io::stdout().write_all(&outcome.stdout);
    let _ = std::io::stderr().write_all(&outcome.stderr);
    match outcome.termination {
        // The C++ wrapper re-raises the child's (or the timeout's) signal with the default
        // disposition so that its own parent observes death by signal.
        Termination::Signaled(sig) => unsafe {
            signal(sig, 0);
            kill(getpid(), sig);
            std::process::exit(128 + sig);
        },
        Termination::Exited(c) => std::process::exit(c),
    }
}
