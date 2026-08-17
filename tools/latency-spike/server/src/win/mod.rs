//! Everything that talks to Windows.
//!
//! Split by pipeline stage so the per-stage budget in the stats file maps one-to-one
//! onto a module: [`dxgi`] captures, [`convert`] does BGRA→NV12 on the GPU,
//! [`encode`] drives the Media Foundation H.264 MFT, [`send`] owns the video socket
//! and the stats file, and [`input`] owns the keystroke channel. [`pipeline`] wires
//! them together.

pub mod convert;
pub mod dxgi;
pub mod encode;
pub mod input;
pub mod pipeline;
pub mod qpc;
pub mod send;

pub use pipeline::{list_outputs, run};

/// Errors cross thread boundaries here, so they must be `Send + Sync`.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Read a fixed-size wide string field (`DXGI_ADAPTER_DESC1::Description` and
/// friends), stopping at the first NUL.
pub fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}
