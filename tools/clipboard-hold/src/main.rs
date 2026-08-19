//! Hold the Windows clipboard open for a bounded time, and say so precisely.
//!
//! One job: make another process's `OpenClipboard` fail, on purpose, for a
//! known interval. rhydra's clipboard path retries a refused open a bounded
//! number of times and then reports; AC4 asks for that to be observed live, and
//! it cannot be observed unless something reliably refuses.
//!
//! # Why this is a binary and not a PowerShell script
//!
//! It was a PowerShell script first, and the script did **not** actually hold
//! the clipboard: its own `CloseClipboard` returned `FALSE`, meaning the handle
//! had already been lost, and the process under test wrote to the clipboard
//! happily throughout. A test whose refusal never happens measures nothing —
//! and looks exactly like a passing one.
//!
//! The likely cause is PowerShell's threading: `OpenClipboard` associates the
//! clipboard with the *calling thread*, and a runspace is free to marshal the
//! next statement elsewhere. Rather than argue with semantics we do not
//! control, this holds it on a thread that plainly does nothing else.
//!
//! # It reports both calls, and that matters
//!
//! `CloseClipboard` returning `FALSE` is the signal that the hold was never
//! real. Printing only "held for 10s" would hide exactly the failure that made
//! this program necessary.

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::DataExchange::{CloseClipboard, OpenClipboard};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };

    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(10);

    // SAFETY: a null owner associates the clipboard with the current task,
    // which is what we want — this process owns no window.
    let opened = unsafe { OpenClipboard(Some(HWND(std::ptr::null_mut()))) };
    match &opened {
        Ok(()) => println!("opened=true"),
        Err(e) => {
            // Someone else already has it. Say so rather than pretending to
            // hold something we do not.
            println!("opened=false error={e}");
            return std::process::ExitCode::from(2);
        }
    }

    // **Pump messages while holding, rather than sleeping.**
    //
    // A plain sleep loses the clipboard: measured on quench, a 30 s sleeping
    // hold ended with `CloseClipboard` reporting 0x8007058A, "thread does not
    // have a clipboard open". A thread that owns the clipboard is expected to
    // answer messages — `WM_DESTROYCLIPBOARD` and `WM_RENDERFORMAT` are *sent*
    // to it — and one that never does is not behaving like an owner at all.
    // Pumping is what a real application does between Open and Close.
    let started = Instant::now();
    let deadline = Duration::from_secs(seconds);
    let mut message = MSG::default();
    while started.elapsed() < deadline {
        // SAFETY: a peek with no window filter, removing what it finds. Both
        // out-params are ours.
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let held = started.elapsed();

    // SAFETY: the clipboard is open on this thread.
    let closed = unsafe { CloseClipboard() };
    match &closed {
        Ok(()) => {
            println!("held_secs={:.2}", held.as_secs_f64());
            println!("closed=true");
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            // The whole reason this program exists. If the close fails, we did
            // not hold it, and any conclusion drawn from the run is void.
            println!("held_secs={:.2}", held.as_secs_f64());
            println!("closed=false error={e}");
            println!("VOID: the clipboard was not held for the whole interval");
            std::process::ExitCode::from(3)
        }
    }
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    // Kept compiling everywhere so `cargo check` on the Mac is a useful gate,
    // and saying why rather than silently doing nothing.
    eprintln!("clipboard-hold: there is no Windows clipboard to hold on this platform");
    std::process::ExitCode::from(1)
}
