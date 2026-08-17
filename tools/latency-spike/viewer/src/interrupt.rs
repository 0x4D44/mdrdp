//! Ctrl-C that closes the run rather than killing it.
//!
//! The stats file is flushed per line, so a hard kill loses no measurement — but it
//! also leaves the server holding a video connection and never sends the input socket
//! a FIN. The handler here does the only thing a signal handler may safely do: set a
//! flag on a `static` atomic. The event loop reads it every 200 ms and exits normally,
//! which runs `exiting` and closes both sockets.

use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Has an interrupt arrived?
pub fn is_set() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}

/// Catch SIGINT and SIGTERM. Idempotent; a second call re-installs the same handler.
#[cfg(unix)]
pub fn arm() {
    // SAFETY: `signal` with a plain `extern "C"` handler that touches nothing but a
    // `static` atomic — the one operation POSIX guarantees is async-signal-safe. The
    // handler takes no locks, allocates nothing, and calls back into no library.
    unsafe {
        libc::signal(libc::SIGINT, handler as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handler as *const () as libc::sighandler_t);
    }
}

#[cfg(unix)]
extern "C" fn handler(_signal: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}

/// No signal handling off unix. The binary refuses to run there anyway.
#[cfg(not(unix))]
pub fn arm() {}
