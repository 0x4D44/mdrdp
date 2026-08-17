//! `spike-server` — the instrumented capture/encode/send half of the mdrdp latency
//! spike.
//!
//! The library half is deliberately portable: framing, Annex B handling, the input
//! record, the colour-space bit packing and the stats shapes all build and test on
//! macOS, because that is where they are written and because the viewer reuses them.
//! Everything that talks to Windows lives under [`win`] behind `#[cfg(windows)]`.

pub mod annexb;
pub mod cli;
pub mod colorspace;
pub mod framing;
pub mod input_proto;
pub mod rects;
pub mod stats;

#[cfg(windows)]
pub mod win;
