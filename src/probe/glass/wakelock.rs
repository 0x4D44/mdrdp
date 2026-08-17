//! Keep the display awake for the duration of a measurement run.
//!
//! A glass trial needs photons, and macOS turns the display off after its idle timeout —
//! which a probe run only partially resets: the injected CGEvents count as user activity,
//! but the quiet gaps between runs do not, and a locked screen ends every measurement
//! until a human types a password. This was observed live: the display slept eight
//! minutes into a measurement session and the lock screen ate the rest of the phase.
//!
//! The assertion is the same one `caffeinate -d` takes. It prevents *idle* display
//! sleep only — it does not stop the user locking the screen, closing the lid, or the
//! machine sleeping on battery, and it evaporates if the probe exits or crashes.

use objc2_core_foundation::CFString;

/// `kIOPMAssertionLevelOn`.
const LEVEL_ON: u32 = 255;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMAssertionCreateWithName(
        assertion_type: &CFString,
        assertion_level: u32,
        assertion_name: &CFString,
        assertion_id: *mut u32,
    ) -> i32;
    fn IOPMAssertionRelease(assertion_id: u32) -> i32;
}

/// An RAII hold on `PreventUserIdleDisplaySleep`. Dropped, it releases the assertion;
/// leaked by a crash, the kernel releases it with the process.
pub struct DisplayWake {
    id: Option<u32>,
}

impl DisplayWake {
    /// Take the assertion. Failure is reported, not fatal: a run with a sleeping-display
    /// risk still beats no run, and the operator was told.
    pub fn acquire() -> Self {
        let kind = CFString::from_str("PreventUserIdleDisplaySleep");
        let name = CFString::from_str("mdrdp probe glass measurement");
        let mut id: u32 = 0;
        // SAFETY: both CFStrings outlive the call; id is a valid out-pointer.
        let status = unsafe { IOPMAssertionCreateWithName(&kind, LEVEL_ON, &name, &mut id) };
        if status == 0 {
            Self { id: Some(id) }
        } else {
            eprintln!(
                "  warning   could not prevent display sleep (IOPMAssertion status {status}); \
                 a long run may go dark mid-measurement"
            );
            Self { id: None }
        }
    }

    pub fn held(&self) -> bool {
        self.id.is_some()
    }
}

impl Drop for DisplayWake {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            // SAFETY: id came from IOPMAssertionCreateWithName and is released once.
            unsafe { IOPMAssertionRelease(id) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_assertion_is_acquired_and_released() {
        // IOPMAssertionCreateWithName works from any session state (even a locked
        // screen), so this exercises the real create/release pair, not a mock. A held
        // assertion is visible in `pmset -g assertions` while the test runs.
        let wake = DisplayWake::acquire();
        assert!(
            wake.held(),
            "the power-management assertion must be granted"
        );
        drop(wake); // releases; a leak would show up as a stuck assertion in pmset
    }
}
