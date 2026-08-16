//! mdrdp — a portable RDP client for macOS and Windows.
//!
//! The library half exists so the client and the measurement harness share one
//! implementation of the things both need to agree on.

pub mod audio;
pub mod autoinput;
pub mod clipboard;
pub mod connect;
pub mod creds;
pub mod diag;
pub mod egfx;
pub mod favourites;
pub mod gfx;
pub mod h264;
pub mod input;
pub mod metrics;
pub mod probe;
pub mod process_metrics;
pub mod screenshot;
pub mod session;
pub mod settings;
pub mod shell;
pub mod stagelog;
pub mod state;
pub mod stats;
pub mod surface;
pub mod trust;
pub mod ui;
pub mod window;
pub mod window_policy;
