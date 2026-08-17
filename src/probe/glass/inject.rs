//! Keystroke injection and the permission preflights, via CoreGraphics.
//!
//! The injection timestamp is read from the **CoreMedia host clock**, the same clock
//! ScreenCaptureKit stamps frames on. Taking it from `Instant::now` would be measuring
//! two clocks against each other and calling the difference latency.

use std::time::Duration;

use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{
    CGEvent, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGPreflightPostEventAccess,
    CGPreflightScreenCaptureAccess,
};
use objc2_core_media::CMClock;

use super::capture::cmtime_to_us;

/// How long the injected key is held down. Long enough that the far end sees a real key
/// press rather than a glitch, short enough not to trigger key repeat.
const KEY_HOLD: Duration = Duration::from_millis(20);

#[derive(Debug)]
pub struct Error(String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// Which grants are missing. Both are checked with the `Preflight` calls, never the
/// `Request` ones: a blocking consent dialog in the middle of a measurement harness
/// would be its own kind of latency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    pub screen_recording: bool,
    pub post_events: bool,
}

pub fn preflight() -> Permissions {
    Permissions {
        screen_recording: CGPreflightScreenCaptureAccess(),
        post_events: CGPreflightPostEventAccess(),
    }
}

impl Permissions {
    pub fn granted(&self) -> bool {
        self.screen_recording && self.post_events
    }

    /// Exactly what to click, naming the pane each grant lives in. "Permission denied"
    /// with no route to fixing it wastes more time than the measurement saves.
    pub fn instructions(&self) -> String {
        let mut out = String::from("probe glass needs two macOS privacy grants:\n");
        if !self.screen_recording {
            out.push_str(
                "  MISSING  Screen Recording — System Settings -> Privacy & Security -> \
                 Screen & System Audio Recording\n",
            );
        }
        if !self.post_events {
            out.push_str(
                "  MISSING  Accessibility (post events) — System Settings -> \
                 Privacy & Security -> Accessibility\n",
            );
        }
        out.push_str(
            "Grant them to the application running this command (Terminal, iTerm, or your \
             editor's shell), not to the probe binary — macOS attributes both to the \
             invoking app. Quit and relaunch that app afterwards; a running process does \
             not pick up a new grant.",
        );
        out
    }
}

/// The CoreMedia host clock, in microseconds. This is the timebase every number the
/// glass probe reports is expressed in.
pub struct HostClock(CFRetained<CMClock>);

impl HostClock {
    pub fn new() -> Self {
        Self(unsafe { CMClock::host_time_clock() })
    }

    pub fn now_us(&self) -> Option<u64> {
        cmtime_to_us(unsafe { self.0.time() })
    }
}

impl Default for HostClock {
    fn default() -> Self {
        Self::new()
    }
}

/// Posts synthetic keystrokes into the session's event stream.
pub struct Injector {
    source: CFRetained<CGEventSource>,
    clock: HostClock,
    /// Post straight to this process instead of to whatever window has focus.
    ///
    /// Focus-routed injection fails silently on a busy desktop: anything that steals
    /// focus mid-run — another app activating itself, the operator touching the machine —
    /// reroutes every subsequent keystroke into the wrong application, and the probe just
    /// records timeouts. Pinning the pid makes misrouting impossible. (The target window
    /// must still be visible and unoccluded, or its photons cannot be watched.)
    target_pid: Option<i32>,
}

impl Injector {
    pub fn new(target_pid: Option<i32>) -> Result<Self, Error> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .ok_or_else(|| Error("could not create a CGEventSource".to_owned()))?;
        Ok(Self {
            source,
            clock: HostClock::new(),
            target_pid,
        })
    }

    /// Press and release one key, returning the host-clock time taken immediately before
    /// the key-down was posted.
    ///
    /// That ordering is the whole point: the timestamp must precede the event, so the
    /// measured interval can only ever be too long, never too short.
    pub fn tap(&self, key_code: u16) -> Result<u64, Error> {
        let down = CGEvent::new_keyboard_event(Some(&self.source), key_code, true)
            .ok_or_else(|| Error(format!("could not create a key-down event for {key_code}")))?;
        let up = CGEvent::new_keyboard_event(Some(&self.source), key_code, false)
            .ok_or_else(|| Error(format!("could not create a key-up event for {key_code}")))?;

        let at = self
            .clock
            .now_us()
            .ok_or_else(|| Error("the CoreMedia host clock reported an invalid time".to_owned()))?;
        self.post(&down);
        std::thread::sleep(KEY_HOLD);
        self.post(&up);
        Ok(at)
    }

    fn post(&self, event: &CFRetained<CGEvent>) {
        match self.target_pid {
            Some(pid) => CGEvent::post_to_pid(pid, Some(event)),
            None => CGEvent::post(CGEventTapLocation::HIDEventTap, Some(event)),
        }
    }

    pub fn now_us(&self) -> Option<u64> {
        self.clock.now_us()
    }
}

/// The pointer's current position in global points.
///
/// `CGEventCreate(NULL)` builds an event stamped with the current mouse location, which
/// is the documented way to read it without an event tap.
pub fn mouse_location() -> Option<(f64, f64)> {
    let event = CGEvent::new(None)?;
    let point = CGEvent::location(Some(&event));
    Some((point.x, point.y))
}
