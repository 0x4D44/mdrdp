//! Write a predictable nonce to the Windows clipboard on a fixed period.
//!
//! ```text
//! clipboard-cycle.exe [period_secs] [minutes]     # defaults: 60, 600
//! ```
//!
//! Exists so the **host → client** direction can be soaked for hours without
//! anybody typing on the host's desktop. An ssh session on Windows is a
//! different window station, so a clipboard written there is one nobody is
//! using; this has to run in the console session, and is launched once at the
//! start of a soak rather than driven per transfer.
//!
//! The client watches its own pasteboard for `SOAKHOST-NNNNNN` and records
//! which numbers arrived. A number that never appears is a non-arrival in that
//! direction — which is the whole point, because without this the soak covers
//! one direction and AC8 asks for both.
//!
//! # What it deliberately does not do
//!
//! **It does not stamp a wall-clock time into the payload.** That would let the
//! client compute a latency, and the figure would be wrong: the two machines'
//! clocks are only as aligned as NTP has left them, and a skew of a few hundred
//! milliseconds would sit inside the range being measured. Arrival is reported;
//! latency for this direction is not claimed. A number that is wrong is worse
//! than a number that is absent.
//!
//! # Staggering matters
//!
//! Run this on a period the client's own copies avoid. Simultaneous copies at
//! both ends inside one poll interval leave the two clipboards disagreeing —
//! HLD §5 states that case and does not solve it — so a soak that collided them
//! would report known, accepted behaviour as a wedge.

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{GlobalFree, HANDLE, HWND};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

    const CF_UNICODETEXT: u32 = 13;

    let mut args = std::env::args().skip(1);
    let period = Duration::from_secs(args.next().and_then(|a| a.parse().ok()).unwrap_or(60));
    let minutes: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(600);
    let until = Duration::from_secs(minutes * 60);

    println!(
        "clipboard-cycle: every {}s for {}min",
        period.as_secs(),
        minutes
    );
    let began = Instant::now();
    let mut seq: u64 = 0;
    let mut written = 0u64;
    let mut refused = 0u64;

    while began.elapsed() < until {
        seq += 1;
        // Synthetic by construction — a prefix and a counter, never anything
        // read from a clipboard. That is what makes it safe for the client to
        // log a payload that did not match.
        let text = format!("SOAKHOST-{seq:06}");
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();

        // Open / empty / set / close, with the same ownership rule the product
        // follows: after SetClipboardData succeeds the handle belongs to the
        // system, and freeing it then is a double free.
        let mut ok = false;
        // SAFETY: a null owner associates the clipboard with this task.
        if unsafe { OpenClipboard(Some(HWND(std::ptr::null_mut()))) }.is_ok() {
            // SAFETY: the clipboard is open on this thread.
            if unsafe { EmptyClipboard() }.is_ok() {
                let bytes = std::mem::size_of_val(&wide[..]);
                // SAFETY: a plain moveable allocation, as SetClipboardData requires.
                if let Ok(handle) = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) } {
                    // SAFETY: the allocation was sized from this slice.
                    let ptr = unsafe { GlobalLock(handle) } as *mut u16;
                    if !ptr.is_null() {
                        unsafe {
                            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
                            let _ = GlobalUnlock(handle);
                        }
                        // SAFETY: open, empty, and a NUL-terminated UTF-16 buffer.
                        match unsafe { SetClipboardData(CF_UNICODETEXT, Some(HANDLE(handle.0))) } {
                            // The system owns it now; this arm frees nothing.
                            Ok(_) => ok = true,
                            // SAFETY: ownership never transferred, so it is ours.
                            Err(_) => unsafe {
                                let _ = GlobalFree(Some(handle));
                            },
                        }
                    } else {
                        // SAFETY: SetClipboardData was never called.
                        unsafe {
                            let _ = GlobalFree(Some(handle));
                        }
                    }
                }
            }
            // SAFETY: this thread has the clipboard open.
            unsafe {
                let _ = CloseClipboard();
            }
        }

        if ok {
            written += 1;
        } else {
            // Refusals are ordinary — something else held the clipboard for a
            // moment — and they are reported so a gap in the client's sequence
            // can be told apart from a lost transfer. A silent skip here would
            // show up at the other end as a clipboard failure.
            refused += 1;
            println!("clipboard-cycle: REFUSED seq={seq}");
        }
        std::thread::sleep(period);
    }

    println!("clipboard-cycle: wrote {written}, refused {refused}, of {seq} attempts");
    std::process::ExitCode::SUCCESS
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("clipboard-cycle: there is no Windows clipboard to write on this platform");
    std::process::ExitCode::from(1)
}
