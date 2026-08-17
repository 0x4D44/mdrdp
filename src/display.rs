//! Pre-window display probe: what the primary monitor looks like, before winit runs.
//!
//! A session that opens fullscreen wants to *connect* at the resolution and scale the
//! window will end up asking for — advertising the scale in the GCC core data at logon
//! means the server renders at that DPI from the first frame, instead of switching
//! mid-session (a switch DWM answers by bitmap-stretching every window whose process
//! is not per-monitor-DPI-aware — server-side blur no client can undo). winit can only
//! enumerate monitors from inside a running event loop, and the loop's one-and-only
//! `Resumed` is what creates the session window, so this asks the OS directly.
//!
//! Platform split per the architecture rule: the macOS implementation talks to
//! CoreGraphics; other platforms return `None`, which keeps today's behaviour there
//! (connect at the configured size, renegotiate once fullscreen). Best-effort by
//! design — `None` must always be a safe answer.

/// The primary monitor's mode, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayInfo {
    pub width: u32,
    pub height: u32,
    /// The OS UI scale as a percentage (200 on a Retina "looks like half" mode).
    pub scale_percent: u32,
}

/// The primary display's current mode, if the OS will say.
///
/// `None` on platforms without an implementation, on a headless machine, or on any
/// CoreGraphics refusal — callers fall back to the configured session size.
pub fn primary_display() -> Option<DisplayInfo> {
    #[cfg(target_os = "macos")]
    {
        macos::primary_display()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::DisplayInfo;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGSize {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }

    // CGDisplayModeRef, opaque.
    type CGDisplayModeRef = *mut core::ffi::c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGMainDisplayID() -> u32;
        fn CGDisplayCopyDisplayMode(display: u32) -> CGDisplayModeRef;
        fn CGDisplayModeGetPixelWidth(mode: CGDisplayModeRef) -> usize;
        fn CGDisplayModeGetPixelHeight(mode: CGDisplayModeRef) -> usize;
        fn CGDisplayModeRelease(mode: CGDisplayModeRef);
        /// Bounds in the global display coordinate space — *points*, not pixels.
        fn CGDisplayBounds(display: u32) -> CGRect;
    }

    pub(super) fn primary_display() -> Option<DisplayInfo> {
        // SAFETY: plain C calls; the mode ref is released on every path that owns it.
        unsafe {
            let id = CGMainDisplayID();
            let mode = CGDisplayCopyDisplayMode(id);
            if mode.is_null() {
                return None; // Headless, or no WindowServer session.
            }
            let width = CGDisplayModeGetPixelWidth(mode);
            let height = CGDisplayModeGetPixelHeight(mode);
            CGDisplayModeRelease(mode);
            if width == 0 || height == 0 {
                return None;
            }
            let points = CGDisplayBounds(id).size.width;
            // Pixels per point, as a percentage. A non-positive point width would be a
            // CoreGraphics bug; claim 100% rather than dividing by it.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let scale_percent = if points > 0.0 {
                ((width as f64 / points) * 100.0).round() as u32
            } else {
                100
            };
            #[allow(clippy::cast_possible_truncation)]
            Some(DisplayInfo {
                width: width as u32,
                height: height as u32,
                scale_percent,
            })
        }
    }
}
