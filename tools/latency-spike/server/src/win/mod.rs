//! Everything that talks to Windows.
//!
//! Split by pipeline stage so the per-stage budget in the stats file maps one-to-one
//! onto a module: [`source`] defines where frames come from and [`dxgi`] and
//! [`idd_source`] are the two answers, [`convert`] does BGRA→NV12 on the GPU,
//! [`encode`] drives the Media Foundation H.264 MFT, [`send`] owns the video socket
//! and the stats file, and [`input`] owns the keystroke channel. [`pipeline`] wires
//! them together. [`pixel_diff`] sits beside the sources: it is the D3D11 half of
//! the Increment 3 pixel diff, whose portable comparison lives in [`crate::diff`].

pub mod agent_ops;
pub mod clipboard;
pub mod convert;
pub mod dxgi;
pub mod encode;
pub mod idd_source;
pub mod input;
pub mod pipeline;
pub mod pixel_diff;
pub mod qpc;
pub mod send;
pub mod source;

pub use pipeline::{list_outputs, run};

/// Errors cross thread boundaries here, so they must be `Send + Sync`.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Set per-monitor-v2 DPI awareness. Must run before any display or metrics call —
/// `main` calls this first, before `cli::parse` even looks at `--list-outputs` vs a
/// live run (HLD tranche 3 §5.2, review S-M5): a DPI-unaware process reads
/// *logical* (OS-scaled) virtual-screen metrics, so every mouse injection computed
/// against real pixels (`GetSystemMetrics(SM_*VIRTUALSCREEN)`, in the [`input`]
/// module's mouse-move injector) would land off by the scale factor.
///
/// Failure is reported, not treated as fatal here — the caller (currently `main`)
/// decides whether a degraded run is still worth starting.
pub fn init_dpi_awareness() -> Result<()> {
    // SAFETY: no pointers cross the FFI boundary; the context value is one of the
    // library's own constants.
    unsafe {
        windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        )
    }?;
    Ok(())
}

/// Read a fixed-size wide string field (`DXGI_ADAPTER_DESC1::Description` and
/// friends), stopping at the first NUL.
pub fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}
