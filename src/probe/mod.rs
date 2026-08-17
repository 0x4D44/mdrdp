//! Credential-free measurement of an RDP server.
//!
//! Two things are observable without authenticating: which security protocol the server
//! demands (the X.224 negotiation happens before authentication), and the TCP handshake
//! round-trip, which is the latency floor everything else is measured against.

/// Keypress-to-photon latency. macOS-only: it is built on ScreenCaptureKit, CoreMedia's
/// host clock and CGEvent injection, none of which have a portable equivalent, and the
/// measurement is only meaningful with all three on the same timebase.
#[cfg(target_os = "macos")]
pub mod glass;
pub mod negotiation;
pub mod rtt;
pub mod stats;
pub mod wire;
