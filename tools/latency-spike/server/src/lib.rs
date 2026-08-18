//! `rhydra` — mdrdp's native Windows host half: the capture/encode/send server
//! (`rhydra-server`) and the session agent that supervises it (`rhydra-agent`).
//! Grew out of the latency spike; the name is the 1997 Hydra project, in Rust.
//!
//! The library half is deliberately portable: framing, Annex B handling, the input
//! record, the colour-space bit packing, the stats shapes, the agent's control
//! protocol and its reconcile loop all build and test on macOS, because that is
//! where they are written and because the viewer reuses them. Everything that
//! talks to Windows lives under [`win`] behind `#[cfg(windows)]`.

pub mod agent;
pub mod annexb;
pub mod cli;
pub mod colorspace;
pub mod control;
pub mod diff;
pub mod framing;
pub mod idd_section;
pub mod input_proto;
pub mod rects;
pub mod stats;

#[cfg(windows)]
pub mod win;
