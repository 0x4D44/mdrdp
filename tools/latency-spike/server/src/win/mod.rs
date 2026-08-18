//! Everything that talks to Windows.
//!
//! Split by pipeline stage so the per-stage budget in the stats file maps one-to-one
//! onto a module: [`source`] defines where frames come from and [`dxgi`] and
//! [`idd_source`] are the two answers, [`convert`] does BGRA→NV12 on the GPU,
//! [`encode`] drives the Media Foundation H.264 MFT, [`send`] owns the video socket
//! and the stats file, and [`input`] owns the keystroke channel. [`pipeline`] wires
//! them together. [`pixel_diff`] sits beside the sources: it is the D3D11 half of
//! the Increment 3 pixel diff, whose portable comparison lives in [`crate::diff`].

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

/// Read a fixed-size wide string field (`DXGI_ADAPTER_DESC1::Description` and
/// friends), stopping at the first NUL.
pub fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}
