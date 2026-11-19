//! `linux-sandbox` standalone executable (`src/main/tools/linux-sandbox.cc`).

use zel_sandbox::linux_sandbox::{parse_linux_sandbox_args, run_linux_sandbox};
use zel_sandbox::process_wrapper::Termination;

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let program = argv
        .first()
        .cloned()
        .unwrap_or_else(|| "linux-sandbox".to_string());
    let opt = match parse_linux_sandbox_args(&program, &argv[1..]) {
        Ok(o) => o,
        Err(msg) => {
            eprint!("{msg}");
            std::process::exit(1);
        }
    };
    let (termination, wrapper_sig) = match run_linux_sandbox(&opt) {
        Ok(res) => res,
        Err(e) => {
            eprintln!("linux-sandbox: {e}");
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
        Termination::Exited(code) => std::process::exit(code),
    }
}
