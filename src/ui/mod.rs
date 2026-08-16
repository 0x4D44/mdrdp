//! A tiny software-rendered UI toolkit for the favourites launcher.
//!
//! `mdrdp` deliberately does not pull in egui or any UI framework for what is, in the
//! end, a list of rows the user double-clicks — the dependency and binary-size cost
//! buys nothing a hand-rolled font + list widget doesn't already cover. This module is
//! that hand-rolled minimum: [`font`] renders text into a plain `&mut [u32]` pixel
//! buffer, and [`list`] builds a scrollable, selectable row list on top of it.
//!
//! Neither submodule depends on `winit` or `softbuffer` — callers pass the framebuffer,
//! its dimensions, and plain coordinates. That is what makes the whole thing testable
//! without opening a window.

pub mod egui_host;
pub mod end_dialog;
pub mod font;
pub mod form;
pub mod list;
pub mod theme;
