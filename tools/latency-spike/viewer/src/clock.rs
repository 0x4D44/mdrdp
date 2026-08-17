//! The one clock every client-side stamp in the stats file comes from.
//!
//! On macOS it is **the CoreMedia host clock in microseconds** — literally
//! `mdrdp::probe::glass::inject::HostClock` (`src/probe/glass/inject.rs:82`), the same
//! timebase `probe glass` stamps its keystrokes and its ScreenCaptureKit presentation
//! timestamps with. Sharing it means a client stats file and a `probe glass` run taken
//! during the same session can be laid over each other without a conversion, and a
//! conversion is exactly where a cross-clock measurement goes wrong.
//!
//! Nothing here is comparable to the server's `*_qpc_us` fields. The two machines' clocks
//! are never synchronised — the HLD's design — so a server stamp minus a client stamp is
//! meaningless. Differences *within* one file are the measurement.
//!
//! One [`Clock`] per thread, deliberately: the underlying `CMClock` handle is not
//! declared thread-safe, and two instances read the same global timebase anyway, so
//! per-thread construction costs nothing and needs no `Send` claim to be true.

use std::cell::Cell;

/// A monotonic microsecond clock.
pub struct Clock {
    #[cfg(target_os = "macos")]
    host: mdrdp::probe::glass::inject::HostClock,
    #[cfg(not(target_os = "macos"))]
    base: std::time::Instant,
    /// Last value handed out. A clock read that fails must not produce a stamp that
    /// jumps backwards (or to zero) in the middle of a file, because a reader would
    /// take that at face value and compute a negative stage.
    last: Cell<u64>,
    warned: Cell<bool>,
}

impl Clock {
    pub fn new() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            host: mdrdp::probe::glass::inject::HostClock::new(),
            #[cfg(not(target_os = "macos"))]
            base: std::time::Instant::now(),
            last: Cell::new(0),
            warned: Cell::new(false),
        }
    }

    /// The name recorded in the stats header, so an archived file says which clock it is on.
    pub fn name(&self) -> &'static str {
        if cfg!(target_os = "macos") {
            "coremedia-host-clock-us"
        } else {
            "monotonic-instant-us"
        }
    }

    pub fn now_us(&self) -> u64 {
        let raw = self.read();
        match raw {
            Some(us) => {
                self.last.set(us);
                us
            }
            None => {
                if !self.warned.replace(true) {
                    eprintln!(
                        "clock: the host clock reported an invalid time; \
                         reusing the previous stamp (stage durations will read as 0)"
                    );
                }
                self.last.get()
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn read(&self) -> Option<u64> {
        self.host.now_us()
    }

    #[cfg(not(target_os = "macos"))]
    fn read(&self) -> Option<u64> {
        // Non-macOS builds exist only so `cargo check`/`cargo test` stay useful off
        // this Mac; the binary refuses to run there.
        u64::try_from(self.base.elapsed().as_micros()).ok()
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_advances_and_never_goes_backwards() {
        let clock = Clock::new();
        let mut previous = clock.now_us();
        let start = std::time::Instant::now();
        // Busy-wait rather than sleep: this asserts the clock moves, and a sleep would
        // pass even against a clock that only ticks on the scheduler's schedule.
        while start.elapsed() < std::time::Duration::from_millis(5) {
            let now = clock.now_us();
            assert!(now >= previous, "{now} < {previous}");
            previous = now;
        }
        assert!(previous > 0, "the clock never produced a non-zero stamp");
    }

    #[test]
    fn the_clock_names_itself_for_the_archive() {
        assert!(!Clock::new().name().is_empty());
    }
}
