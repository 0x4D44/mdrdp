//! UI building blocks shared by the launcher shell and the session process.
//!
//! The original hand-rolled toolkit ("no egui or any UI framework") is gone — the
//! 2026-08-16 design handoff superseded that position, and the launcher/dialog
//! surfaces are egui now (see `crate::shell` and the HLD in `wrk_docs/`). What
//! remains here is:
//!
//! - [`font`] — the 8×16 bitmap font, still rendering the Ctrl+Alt+S overlay and
//!   the transient toasts straight into the softbuffer frame;
//! - [`theme`] — the handoff's design tokens, egui fonts and visuals;
//! - [`egui_host`] — auxiliary egui windows on an existing winit loop;
//! - [`end_dialog`] — the Session-ended/lost epilogue dialogs;
//! - [`help`] — the About facts and shortcut tables both Help menus draw from.

pub mod egui_host;
pub mod end_dialog;
pub mod font;
pub mod help;
pub mod theme;
