//! `rhydra` — mdrdp's native Windows host half: the capture/encode/send server
//! (`rhydra-server`) and the session agent that supervises it (`rhydra-agent`).
//! Grew out of the latency spike; the name is the 1997 Hydra project, in Rust.
//!
//! The library half is deliberately portable: framing, Annex B handling, the input
//! record, the colour-space bit packing, the stats shapes, and the agent's control
//! *client* (`control::query_status` and friends) all build and test on macOS,
//! because that is where they are written and because mdrdp's client build and the
//! viewer reuse them. Everything host-only — the reconcile loop, the IddCx shared
//! section, and everything that talks to Windows — sits behind the default-on
//! `host` feature (composed with `#[cfg(windows)]` for [`win`], which is Windows-only
//! regardless of the feature): a client build takes `default-features = false` and
//! gets only the portable half (HLD tranche 3 §3).

pub mod annexb;
pub mod audio_source;
pub mod aux_proto;
pub mod aux_server;
pub mod auxchan;
#[cfg(any(feature = "host", test))]
pub(crate) mod bootstrap;
#[cfg(any(feature = "host", test))]
pub(crate) mod channel_listeners;
pub mod cli;
pub mod clipboard;
pub mod colorspace;
pub mod control;
pub mod diff;
pub mod framing;
pub mod input_proto;
#[cfg(any(feature = "host", test))]
pub(crate) mod input_state;
#[cfg(any(all(feature = "host", windows), test))]
pub(crate) mod input_stream;
#[cfg(any(feature = "host", test))]
pub(crate) mod logical_frame;
pub mod rects;
#[cfg(any(feature = "host", test))]
pub(crate) mod send_schedule;
pub mod stats;
#[cfg(any(all(feature = "host", windows), test))]
pub(crate) mod surface_pool;

#[cfg(feature = "host")]
pub mod agent;
#[cfg(feature = "host")]
pub mod idd_section;

#[cfg(all(feature = "host", windows))]
pub mod win;
