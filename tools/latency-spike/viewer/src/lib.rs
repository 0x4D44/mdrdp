//! `spike-viewer` — the macOS receiving half of the mdrdp latency spike.
//!
//! The measurement only means anything if the client side is held constant, so this
//! viewer does not have a decoder or a presenter of its own. It calls
//! [`mdrdp::h264::hardware_decoder`] and [`mdrdp::window::present_into`] — the same
//! two functions an mdrdp session runs — and drives them through `winit` and
//! `softbuffer` at the versions the client pins. What changes between a spike run and
//! an mdrdp run is the *server* and the *transport*, which is the whole point.
//!
//! The wire format is not re-implemented either: [`rhydra::framing`] and
//! [`rhydra::input_proto`] are the same modules the server encodes with.
//!
//! Layering, and why it is split this way: everything above [`app`] is free of any
//! window. The socket reader ([`net`]), the decode-and-store step ([`sink`]) and the
//! stats file ([`stats`]) are driven by `tests/loopback.rs` against a local listener,
//! with no event loop and no display — a viewer whose network layer can only be
//! tested by pointing it at a real server is a viewer nobody tests.

pub mod app;
pub mod cli;
pub mod clock;
pub mod input_link;
pub mod interrupt;
pub mod keymap;
pub mod net;
pub mod present;
pub mod sink;
pub mod stats;
