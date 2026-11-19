//! `process-wrapper` as a standalone executable (port of `src/main/tools/process-wrapper.cc`).

use zel_sandbox::process_wrapper::{parse_args, run_standalone, Termination};

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let program = argv
        .first()
        .cloned()
        .unwrap_or_else(|| "process-wrapper".to_string());
    let opt = match parse_args(&program, &argv[1..]) {
        Ok(o) => o,
        Err(msg) => {
            eprint!("{msg}");
            std::process::exit(1);
        }
    };
    let (termination, wrapper_sig) = match run_standalone(&opt) {
        Ok(res) => res,
        Err(e) => {
            eprintln!("process-wrapper: execvp({}, ...): {e}", opt.args[0]);
            std::process::exit(1);
        }
    };
    if let Some(sig) = wrapper_sig {
        #[cfg(unix)]
        unsafe {
            zel_sandbox::process_wrapper::reraise_signal_to_self(sig);
        }
        std::process::exit(128 + sig);
    }
    match termination {
        Termination::Signaled(sig) => {
            #[cfg(unix)]
            unsafe {
                zel_sandbox::process_wrapper::reraise_signal_to_self(sig);
            }
            std::process::exit(128 + sig);
        }
        Termination::Exited(c) => std::process::exit(c),
    }
}
