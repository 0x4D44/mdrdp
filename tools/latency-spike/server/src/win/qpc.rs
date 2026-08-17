//! `QueryPerformanceCounter`, the one clock every stamp in this server comes from.
//!
//! QPC is the right clock because DXGI hands us `LastPresentTime` in QPC ticks: any
//! other clock would need a conversion we could not check, and the whole point of
//! the budget is that the first stamp and the last stamp are on the same timebase.

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

/// Current QPC tick.
///
/// Panics only if the kernel fails, which it cannot on any Windows that can run
/// this binary — QPC has been infallible since Windows XP, and a server whose clock
/// stopped has nothing useful to report anyway.
pub fn now() -> i64 {
    let mut ticks = 0i64;
    // SAFETY: `ticks` is a live, writable i64 for the duration of the call, which is
    // the only requirement the API has.
    unsafe { QueryPerformanceCounter(&mut ticks) }.expect("QueryPerformanceCounter failed");
    ticks
}

/// Ticks per second. Fixed at boot, so this is read once and passed around.
pub fn frequency() -> i64 {
    let mut freq = 0i64;
    // SAFETY: as above — one live, writable i64.
    unsafe { QueryPerformanceFrequency(&mut freq) }.expect("QueryPerformanceFrequency failed");
    freq
}
