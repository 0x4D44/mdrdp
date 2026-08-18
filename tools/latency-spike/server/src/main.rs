//! `rhydra-server` — the Windows half of mdrdp's native transport.
//!
//! See `README.md` beside this file for the wire protocol, the stage-timestamp
//! semantics and how to run it on the host.

use std::process::ExitCode;

#[cfg(windows)]
fn main() -> ExitCode {
    use rhydra::cli;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = match cli::parse(&args) {
        Ok(cfg) => cfg,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    let outcome = if cfg.list_outputs {
        rhydra::win::list_outputs()
    } else {
        rhydra::win::run(&cfg)
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The server captures a Windows desktop and injects Windows input; there is no
/// meaningful degraded mode elsewhere. Exiting 2 (the usage code) rather than
/// panicking keeps `cargo test` and `cargo check` useful on the Mac this is written
/// on, which is the whole reason the pure-logic modules are portable.
#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("rhydra-server: windows only");
    ExitCode::from(2)
}
