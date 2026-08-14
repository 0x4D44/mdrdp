//! Credential-free measurement of an RDP server.
//!
//! Two things are observable without authenticating: which security protocol the server
//! demands (the X.224 negotiation happens before authentication), and the TCP handshake
//! round-trip, which is the latency floor everything else is measured against.

pub mod negotiation;
pub mod rtt;
pub mod stats;
pub mod wire;
