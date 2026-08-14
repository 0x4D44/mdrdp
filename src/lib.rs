//! mdrdp — a portable RDP client for macOS and Windows.
//!
//! The library half exists so the client and the measurement harness share one
//! implementation of the things both need to agree on.

pub mod connect;
pub mod creds;
pub mod egfx;
pub mod gfx;
pub mod input;
pub mod probe;
pub mod session;
pub mod stagelog;
pub mod surface;
pub mod trust;
pub mod window;
