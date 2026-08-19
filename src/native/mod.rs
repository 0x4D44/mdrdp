//! The native (rhydra) transport: SSH tunnel, probe, wire session.
//!
//! `mdrdp <host> --native` speaks the spike's wire protocol to a rhydra host the
//! user has deployed to, in place of IronRDP. SSH is the security boundary — the
//! host's listeners are loopback-only and reached through `-L` forwards owned by
//! this module. Design: `wrk_docs/2026.08.18 - HLD - rhydra tranche 3 - mdrdp
//! native MVP.md`.

pub mod ssh;
