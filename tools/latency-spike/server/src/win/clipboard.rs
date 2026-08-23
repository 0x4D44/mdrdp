//! The Windows clipboard, as the auxiliary channel's [`TextClipboard`].
//!
//! Text only, `CF_UNICODETEXT` only. Everything above this — suppression, the
//! size ceiling, the direction gates — is shared with the Mac end in
//! [`crate::clipboard`]; this file is the platform, and nothing else.
//!
//! # An idle host must not open the clipboard four times a second
//!
//! The poll runs every 250 ms for the life of a session, and opening the
//! clipboard locks it for the whole desktop while we hold it. Doing that
//! constantly would make every other application on the box contend with us for
//! no reason, on a host whose whole purpose is to be a desktop someone else is
//! using.
//!
//! [`GetClipboardSequenceNumber`] is the answer: it reports the clipboard's
//! change counter **without opening it**, so an unchanged clipboard costs one
//! cheap call and nothing else.
//!
//! **This is a deliberate departure from HLD tranche 5 §6**, which specifies a
//! message-only window with `AddClipboardFormatListener`. That design is
//! correct — the HLD checked that a `HWND_MESSAGE` window really does receive
//! `WM_CLIPBOARDUPDATE`, because it is posted rather than sent — but it needs a
//! window, a message loop, and a thread that owns both, and it buys nothing the
//! sequence number does not. Recorded here rather than silently swapped.
//!
//! # The wedge is `EmptyClipboard`, not `OpenClipboard`
//!
//! `OpenClipboard` failing because another process holds the clipboard is
//! ordinary and bounded retry is the right answer. It is **not** the hazard.
//!
//! The hazard is one call later: [`EmptyClipboard`] sends `WM_DESTROYCLIPBOARD`
//! to the previous owner, and it *sends* rather than posts — so a hung previous
//! owner blocks us **while we hold the clipboard open**, locking it for the
//! whole desktop. No retry policy on the open touches this, because by then we
//! are past it. What this module does about it:
//!
//! - the write sequence runs on whatever thread the caller gives it, which owns
//!   nothing else, so a block there costs the clipboard and nothing more;
//! - a watchdog **reports** a sequence that has not returned, and never kills —
//!   killing a thread mid-`EmptyClipboard` would leave the clipboard open
//!   forever, which is strictly worse than the block it was trying to escape;
//! - [`ClipboardOwner::stuck_since`] exposes it, so it can become a rung rather
//!   than a mystery.
//!
//! # Two rules the reference documentation makes easy to get wrong
//!
//! - **After [`SetClipboardData`] succeeds the handle belongs to the system.**
//!   Freeing it then is a double free. Before it succeeds — or if it fails — it
//!   is ours and must be freed, or every failed write leaks.
//! - **Delayed rendering is not used, and must not be.** Real data goes to
//!   `SetClipboardData`; passing null would make us responsible for rendering on
//!   demand, from a process that may be busy encoding video.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// `GlobalFree` lives in `Foundation` rather than `System::Memory`, unlike the
// rest of the Global* family it belongs to.
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};

use crate::clipboard::TextClipboard;

/// `CF_UNICODETEXT`. Declared here rather than imported because the constant
/// moves between modules across `windows` crate versions, and its value is
/// fixed by the SDK.
const CF_UNICODETEXT: u32 = 13;

/// How many times to retry a clipboard open before giving up on this lap.
///
/// Small and bounded on purpose. Another process holding the clipboard is
/// ordinary; the next poll is 250 ms away, so there is nothing to gain by
/// waiting longer than the gap between attempts and everything to lose by
/// holding a thread.
const OPEN_ATTEMPTS: u32 = 5;

/// Gap between open attempts.
const OPEN_RETRY_GAP: Duration = Duration::from_millis(20);

/// How long a write sequence may run before the watchdog says so.
///
/// Generous: this is not a deadline, it is the point at which "slow" becomes
/// worth reporting. Nothing is cancelled when it passes.
const WRITE_WATCHDOG: Duration = Duration::from_secs(2);

/// The host's clipboard.
pub struct ClipboardOwner {
    /// The sequence number as of the last successful read, so an unchanged
    /// clipboard is answered without opening it.
    last_seq: u64,
    /// What the clipboard held at `last_seq`, in canonical LF form.
    cached: Option<String>,
    /// When the in-flight write sequence started, if one is in flight.
    ///
    /// Shared so a watchdog thread — or a health rung — can see it without
    /// touching the clipboard itself.
    writing_since: Arc<Mutex<Option<Instant>>>,
    /// How many write sequences have outlived [`WRITE_WATCHDOG`].
    slow_writes: Arc<AtomicU64>,
}

impl Default for ClipboardOwner {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipboardOwner {
    pub fn new() -> Self {
        Self {
            // 0 is never a live sequence number, so the first read always opens.
            last_seq: 0,
            cached: None,
            writing_since: Arc::new(Mutex::new(None)),
            slow_writes: Arc::new(AtomicU64::new(0)),
        }
    }

    /// How long the current write sequence has been outstanding, if any.
    ///
    /// `Some` here means the clipboard is locked for the whole desktop and we
    /// are the ones holding it — the state HLD §6 asks `--doctor` to be able to
    /// show. Never used to cancel anything.
    pub fn stuck_since(&self) -> Option<Duration> {
        (*lock(&self.writing_since)).map(|started| started.elapsed())
    }

    /// How many write sequences have run longer than [`WRITE_WATCHDOG`].
    pub fn slow_writes(&self) -> u64 {
        self.slow_writes.load(Ordering::Relaxed)
    }
}

impl TextClipboard for ClipboardOwner {
    fn read_text(&mut self) -> Result<Option<String>, String> {
        // The whole point of the sequence number: an unchanged clipboard never
        // opens, so an idle session contends with nothing.
        //
        // SAFETY: no arguments, no pointers; the call only reads a counter.
        let seq = unsafe { GetClipboardSequenceNumber() } as u64;
        if seq != 0 && seq == self.last_seq {
            return Ok(self.cached.clone());
        }

        let _open = ClipboardGuard::open()?;

        // SAFETY: the format id is a constant; the call only tests availability.
        if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) }.is_err() {
            // Something is on the clipboard, but not text we can carry. That is
            // an answer, not a failure — the bridge leaves its slot alone for an
            // error and ignores a `None`, and those must stay different.
            self.last_seq = seq;
            self.cached = None;
            return Ok(None);
        }

        // SAFETY: the format was just reported available. The handle belongs to
        // the clipboard and is only valid until CloseClipboard, which is why the
        // text is copied out before `_open` drops.
        let handle = unsafe { GetClipboardData(CF_UNICODETEXT) }
            .map_err(|e| format!("could not read the clipboard: {e}"))?;
        let text = unsafe { read_utf16_handle(handle) }?;

        // Canonical LF on the wire; CRLF is Windows' business and stops here.
        let text = crate::aux_proto::to_wire_newlines(&text);
        self.last_seq = seq;
        self.cached = Some(text.clone());
        Ok(Some(text))
    }

    fn write_text(&mut self, text: &str) -> Result<(), String> {
        // The wire is LF; Windows wants CRLF. This is the one place either
        // conversion happens on this side.
        let wide: Vec<u16> = crate::aux_proto::to_crlf(text)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        *lock(&self.writing_since) = Some(Instant::now());
        let outcome = self.write_sequence(&wide);
        let elapsed = lock(&self.writing_since).take().map(|s| s.elapsed());
        if elapsed.is_some_and(|e| e > WRITE_WATCHDOG) {
            self.slow_writes.fetch_add(1, Ordering::Relaxed);
            // Names the duration, never the content.
            eprintln!(
                "aux: clipboard write sequence took {:?} — a previous owner was slow to \
                 release WM_DESTROYCLIPBOARD",
                elapsed.unwrap_or_default()
            );
        }
        outcome
    }
}

impl ClipboardOwner {
    /// Open, empty, set, close — the sequence the module docs are about.
    fn write_sequence(&mut self, wide: &[u16]) -> Result<(), String> {
        let _open = ClipboardGuard::open()?;

        // This is the call that can block: it SENDS WM_DESTROYCLIPBOARD to the
        // previous owner while we hold the clipboard open.
        //
        // SAFETY: the clipboard is open on this thread, which is EmptyClipboard's
        // only precondition.
        unsafe { EmptyClipboard() }.map_err(|e| format!("could not empty the clipboard: {e}"))?;

        let bytes = std::mem::size_of_val(wide);
        // SAFETY: a plain allocation; GMEM_MOVEABLE is what SetClipboardData
        // requires of a handle it will take ownership of.
        let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) }
            .map_err(|e| format!("could not allocate for the clipboard: {e}"))?;

        // Everything from here until SetClipboardData succeeds must free the
        // handle on the way out, or a failed write leaks it.
        let copied = unsafe { copy_into_handle(handle, wide) };
        if let Err(e) = copied {
            // SAFETY: SetClipboardData has not been called, so the handle is
            // still ours.
            let _ = unsafe { GlobalFree(Some(handle)) };
            return Err(e);
        }

        // SAFETY: the clipboard is open and empty and the handle is a
        // GMEM_MOVEABLE allocation holding a NUL-terminated UTF-16 string.
        match unsafe { SetClipboardData(CF_UNICODETEXT, Some(HANDLE(handle.0))) } {
            Ok(_) => {
                // The system owns the handle now. Freeing it here would be a
                // double free — this `Ok` arm deliberately does nothing.
                //
                // The sequence number has moved; recording it now would make the
                // next read serve a cache that predates our own write, so leave
                // it alone and let that read refresh from the OS.
                self.last_seq = 0;
                self.cached = None;
                Ok(())
            }
            Err(e) => {
                // SAFETY: SetClipboardData failed, so ownership never transferred.
                let _ = unsafe { GlobalFree(Some(handle)) };
                Err(format!("could not set the clipboard: {e}"))
            }
        }
    }
}

/// Holds the clipboard open, and closes it however the caller leaves.
///
/// A `Drop` guard rather than paired calls because every early return between
/// the open and the close — and there are several, one of them a `?` — would
/// otherwise leave the clipboard locked for the whole desktop until the process
/// exits.
struct ClipboardGuard;

impl ClipboardGuard {
    fn open() -> Result<Self, String> {
        let mut last = String::new();
        for attempt in 0..OPEN_ATTEMPTS {
            // SAFETY: a null owner associates the clipboard with the current
            // task, which is what we want — this process owns no window.
            match unsafe { OpenClipboard(Some(HWND(std::ptr::null_mut()))) } {
                Ok(()) => return Ok(Self),
                Err(e) => {
                    last = e.to_string();
                    if attempt + 1 < OPEN_ATTEMPTS {
                        std::thread::sleep(OPEN_RETRY_GAP);
                    }
                }
            }
        }
        // Ordinary and transient: another process had it. The caller treats this
        // as an error, which leaves the suppression slot untouched, so the copy
        // is picked up on the next lap rather than lost.
        Err(format!(
            "the clipboard was held by another process ({OPEN_ATTEMPTS} attempts): {last}"
        ))
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        // SAFETY: this type exists only while the clipboard is open.
        let _ = unsafe { CloseClipboard() };
    }
}

/// Read a NUL-terminated UTF-16 string out of a clipboard handle.
///
/// # Safety
///
/// `handle` must be a live `CF_UNICODETEXT` clipboard handle and the clipboard
/// must still be open.
unsafe fn read_utf16_handle(handle: HANDLE) -> Result<String, String> {
    let global = HGLOBAL(handle.0);
    // GlobalSize is the only trustworthy bound: the system may round a movable
    // allocation up, but bytes beyond the first NUL are slack, not text.
    let units = crate::clipboard::utf16_units_for_allocation(unsafe { GlobalSize(global) })?;
    let ptr = unsafe { GlobalLock(global) } as *const u16;
    if ptr.is_null() {
        return Err("the clipboard handle could not be locked".to_owned());
    }
    let _unlock = GlobalLockGuard(global);
    // SAFETY: GlobalSize supplied the exact allocation bound, and GlobalLock
    // returned a pointer to that allocation.
    let slice = unsafe { std::slice::from_raw_parts(ptr, units) };
    crate::clipboard::decode_utf16_allocation(slice)
}

/// Unlock a successful GlobalLock on every return path, including malformed data.
struct GlobalLockGuard(HGLOBAL);

impl Drop for GlobalLockGuard {
    fn drop(&mut self) {
        // SAFETY: this guard is created only after GlobalLock returned a pointer.
        let _ = unsafe { GlobalUnlock(self.0) };
    }
}

/// Copy a UTF-16 string into a freshly allocated moveable handle.
///
/// # Safety
///
/// `handle` must be a live `GlobalAlloc` handle with room for `wide`.
unsafe fn copy_into_handle(handle: HGLOBAL, wide: &[u16]) -> Result<(), String> {
    let ptr = unsafe { GlobalLock(handle) } as *mut u16;
    if ptr.is_null() {
        return Err("the clipboard allocation could not be locked".to_owned());
    }
    let _unlock = GlobalLockGuard(handle);
    // SAFETY: the allocation was sized from this same slice.
    unsafe { std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len()) };
    Ok(())
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
