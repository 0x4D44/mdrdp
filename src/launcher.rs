//! The favourites launcher: pick a saved connection and go.
//!
//! The requirement this exists for is *"a favourite connect list on the main UI — so I
//! just double click an icon and get connected to that server"*. So the launcher is what
//! `mdrdp` shows when it is started with no arguments, and its entire job is to return one
//! [`Favourite`] to the caller. It knows nothing about RDP: it opens no socket, holds no
//! credential, and hands back a choice.
//!
//! It is a separate window from the session, opened and closed before the session window
//! exists. Running both at once would mean two event loops on one thread, which winit does
//! not allow — and a launcher that lingers behind a full-screen desktop serves nobody.
//!
//! Everything drawn here goes through [`crate::ui`], which renders into a plain pixel
//! buffer. The layout arithmetic is pulled out into [`Layout`] so it can be tested without
//! opening a window; only the winit plumbing is untested, and it is deliberately thin.

use std::num::NonZeroU32;
use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::platform::run_on_demand::EventLoopExtRunOnDemand;
use winit::window::{Window, WindowId};

use crate::favourites::{Favourite, Favourites};
use crate::ui::font;
use crate::ui::list::{DoubleClick, KeyAction, ListView, Row};
use crate::window::{SessionEvent, WindowError};

/// Milliseconds within which two clicks on one row count as a double-click.
const DOUBLE_CLICK_MS: u64 = 400;

const BG: u32 = 0x0018_1818;
const HEADING: u32 = 0x00ff_ffff;
const HINT: u32 = 0x0090_9090;

/// Where each part of the launcher window sits, in pixels.
///
/// Split out so the arithmetic — which is the only part that can be wrong in a way a
/// human would not immediately see — is testable without a display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub width: i32,
    pub height: i32,
    pub pad: i32,
    pub header_h: i32,
    pub footer_h: i32,
    pub row_h: i32,
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            width: 560,
            height: 640,
            pad: 16,
            // Title line plus a hint line, each a glyph tall, plus breathing room.
            header_h: font::CHAR_H * 2 + 24,
            footer_h: font::CHAR_H + 16,
            // Two lines per row: the name, and the host underneath it.
            row_h: font::CHAR_H * 2 + 12,
        }
    }
}

impl Layout {
    /// The rectangle the list occupies: `(x, y, width, height)`.
    ///
    /// Height is clamped at zero so a window shrunk below the chrome yields an empty
    /// list rather than a negative height that would wrap around when cast.
    pub fn list_rect(&self, height: i32) -> (i32, i32, i32, i32) {
        let y = self.header_h;
        let available = height - self.header_h - self.footer_h;
        (self.pad, y, self.width - self.pad * 2, available.max(0))
    }
}

/// Turn saved favourites into list rows.
///
/// The subtitle carries the detail that distinguishes two similarly named entries —
/// which is exactly `user@host:port` — because a list of names alone is ambiguous the
/// moment you have the same machine saved twice with different accounts.
pub fn rows_for(favourites: &Favourites) -> Vec<Row> {
    favourites
        .iter()
        .map(|f| Row::with_subtitle(f.name.clone(), subtitle_for(f)))
        .collect()
}

fn subtitle_for(f: &Favourite) -> String {
    let host = if f.port == crate::favourites::DEFAULT_PORT {
        f.host.clone()
    } else {
        format!("{}:{}", f.host, f.port)
    };
    match (&f.domain, &f.username) {
        (Some(d), Some(u)) => format!("{d}\\{u} @ {host}"),
        (None, Some(u)) => format!("{u} @ {host}"),
        _ => host,
    }
}

/// Show the launcher and return the chosen favourite, or `None` if the user closed it.
///
/// Borrows the process's single event loop rather than making one. winit permits exactly
/// one `EventLoop` per process — `build` sets a global flag and every later call returns
/// `RecreationAttempt` — so the launcher cannot make its own each time it is shown.
/// `run_app_on_demand` is the supported way to re-run one loop for successive, orthogonal
/// windows, which is what lets the caller show the picker again after each session is
/// launched.
pub fn pick(
    event_loop: &mut EventLoop<SessionEvent>,
    favourites: &Favourites,
    config_path: &str,
) -> Result<Option<Favourite>, WindowError> {
    event_loop.set_control_flow(ControlFlow::Wait);

    let layout = Layout::default();
    let mut list = {
        let (x, y, w, h) = layout.list_rect(layout.height);
        let mut l = ListView::new(x, y, w, h, layout.row_h);
        l.set_rows(rows_for(favourites));
        if !favourites.is_empty() {
            l.selected = Some(0);
        }
        l
    };
    list.colours.bg_normal = BG;

    let mut app = LauncherApp {
        layout,
        list,
        window: None,
        _context: None,
        surface: None,
        double: DoubleClick::new(DOUBLE_CLICK_MS),
        started: std::time::Instant::now(),
        cursor: PhysicalPosition::new(0.0, 0.0),
        chosen: None,
        failure: None,
        config_path: config_path.to_owned(),
        empty: favourites.is_empty(),
    };

    event_loop
        .run_app_on_demand(&mut app)
        .map_err(|e| WindowError::EventLoop(e.to_string()))?;

    if let Some(e) = app.failure.take() {
        return Err(e);
    }
    Ok(app.chosen.and_then(|i| favourites.iter().nth(i).cloned()))
}

struct LauncherApp {
    layout: Layout,
    list: ListView,
    window: Option<Arc<Window>>,
    _context: Option<softbuffer::Context<Arc<Window>>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    double: DoubleClick,
    started: std::time::Instant,
    cursor: PhysicalPosition<f64>,
    chosen: Option<usize>,
    failure: Option<WindowError>,
    config_path: String,
    empty: bool,
}

impl LauncherApp {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Accept the current selection and close.
    fn confirm(&mut self, event_loop: &ActiveEventLoop, index: usize) {
        self.chosen = Some(index);
        event_loop.exit();
    }

    fn redraw(&mut self) {
        let (Some(window), Some(surface)) = (self.window.clone(), self.surface.as_mut()) else {
            return;
        };
        let size = window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return; // Minimised.
        };
        if surface.resize(w, h).is_err() {
            return;
        }
        let Ok(mut buf) = surface.buffer_mut() else {
            return;
        };

        let (bw, bh) = (size.width as usize, size.height as usize);
        buf.fill(BG);

        font::draw_text(&mut buf, bw, bh, self.layout.pad, 12, "mdrdp", HEADING);
        let hint = if self.empty {
            "no saved connections yet"
        } else {
            "double-click to connect  ·  enter to open  ·  esc to quit"
        };
        font::draw_text(
            &mut buf,
            bw,
            bh,
            self.layout.pad,
            12 + font::CHAR_H + 4,
            hint,
            HINT,
        );

        if self.empty {
            // An empty list with no explanation looks like a bug. Say where the file is
            // so the fix is obvious without reading any documentation.
            let y = self.layout.header_h + 8;
            font::draw_text(
                &mut buf,
                bw,
                bh,
                self.layout.pad,
                y,
                "add one by editing:",
                HINT,
            );
            font::draw_text(
                &mut buf,
                bw,
                bh,
                self.layout.pad,
                y + font::CHAR_H + 4,
                &self.config_path,
                HEADING,
            );
        } else {
            self.list.draw(&mut buf, bw, bh);
        }

        let footer_y = size.height as i32 - self.layout.footer_h + 4;
        font::draw_text(
            &mut buf,
            bw,
            bh,
            self.layout.pad,
            footer_y,
            &self.config_path,
            HINT,
        );

        window.pre_present_notify();
        let _ = buf.present();
    }
}

impl ApplicationHandler<SessionEvent> for LauncherApp {
    /// The launcher shares the session's event loop type but has no producer thread
    /// behind it, so nothing can arrive here. Ignoring it is correct, not a stub.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: SessionEvent) {}

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("mdrdp — favourites")
            .with_inner_size(LogicalSize::new(
                self.layout.width as f64,
                self.layout.height as f64,
            ));
        let window = match event_loop.create_window(attributes) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.failure = Some(WindowError::Os(e.to_string()));
                event_loop.exit();
                return;
            }
        };
        let context = match softbuffer::Context::new(window.clone()) {
            Ok(c) => c,
            Err(e) => {
                self.failure = Some(WindowError::Os(e.to_string()));
                event_loop.exit();
                return;
            }
        };
        let surface = match softbuffer::Surface::new(&context, window.clone()) {
            Ok(s) => s,
            Err(e) => {
                self.failure = Some(WindowError::Os(e.to_string()));
                event_loop.exit();
                return;
            }
        };
        window.request_redraw();
        self.window = Some(window);
        self._context = Some(context);
        self.surface = Some(surface);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested | WindowEvent::Destroyed => event_loop.exit(),
            WindowEvent::RedrawRequested => self.redraw(),

            WindowEvent::Resized(size) => {
                let (x, y, w, h) = self.layout.list_rect(size.height as i32);
                self.list.x = x;
                self.list.y = y;
                self.list.width = w;
                self.list.visible_height = h;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = position;
                let before = self.list.hover;
                self.list.update_hover(position.x as i32, position.y as i32);
                // Only repaint when the highlight actually moved: the cursor generates a
                // lot of these and each one otherwise costs a full window repaint.
                if before != self.list.hover
                    && let Some(window) = &self.window
                {
                    window.request_redraw();
                }
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let (x, y) = (self.cursor.x as i32, self.cursor.y as i32);
                if let Some(row) = self.list.row_at(x, y) {
                    self.list.selected = Some(row);
                    let now = self.now_ms();
                    if self.double.click(now, row) {
                        self.confirm(event_loop, row);
                        return;
                    }
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }

            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let action = match event.logical_key {
                    Key::Named(NamedKey::ArrowUp) => Some(KeyAction::Up),
                    Key::Named(NamedKey::ArrowDown) => Some(KeyAction::Down),
                    Key::Named(NamedKey::Enter) => Some(KeyAction::Enter),
                    Key::Named(NamedKey::Escape) => {
                        event_loop.exit();
                        return;
                    }
                    _ => None,
                };
                if let Some(action) = action {
                    if let Some(chosen) = self.list.handle_key(action) {
                        self.confirm(event_loop, chosen);
                        return;
                    }
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }

            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::favourites::Favourite;

    fn favourites() -> Favourites {
        let mut f = Favourites::default();
        f.add(Favourite {
            username: Some("martin".into()),
            ..Favourite::new("Temper", "temper.local")
        })
        .unwrap();
        let mut odd = Favourite::new("Odd port", "box.example");
        odd.port = 3390;
        f.add(odd).unwrap();
        f
    }

    #[test]
    fn rows_carry_the_name_and_a_distinguishing_subtitle() {
        let rows = rows_for(&favourites());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "Temper");
        assert_eq!(
            rows[0].subtitle.as_deref(),
            Some("martin @ temper.local"),
            "the account is what tells two entries for one host apart"
        );
    }

    #[test]
    fn a_default_port_is_not_shown_but_a_custom_one_is() {
        let rows = rows_for(&favourites());
        assert_eq!(
            rows[0].subtitle.as_deref(),
            Some("martin @ temper.local"),
            "3389 is noise"
        );
        assert_eq!(
            rows[1].subtitle.as_deref(),
            Some("box.example:3390"),
            "a non-default port is the whole reason the entry exists"
        );
    }

    #[test]
    fn a_domain_qualifies_the_account() {
        let mut f = Favourites::default();
        f.add(Favourite {
            username: Some("mgd".into()),
            domain: Some("CORP".into()),
            ..Favourite::new("Work", "desk.corp")
        })
        .unwrap();
        assert_eq!(
            rows_for(&f)[0].subtitle.as_deref(),
            Some("CORP\\mgd @ desk.corp")
        );
    }

    #[test]
    fn an_empty_list_yields_no_rows_rather_than_a_placeholder_row() {
        assert!(rows_for(&Favourites::default()).is_empty());
    }

    /// Render the real list widget with real favourites and check something legible came
    /// out, dumping it as ASCII art for a human to read under `--nocapture`.
    ///
    /// The launcher's own `redraw` needs a window, so it cannot be tested. This exercises
    /// everything inside it that can be wrong silently — row layout, the font, the
    /// selected-row highlight — without one.
    #[test]
    fn the_favourites_actually_render_into_a_buffer() {
        let favourites = favourites();
        let mut list = ListView::new(0, 0, 320, 120, 40);
        list.set_rows(rows_for(&favourites));
        list.selected = Some(0);

        let (w, h) = (320usize, 120usize);
        let mut buf = vec![0u32; w * h];
        list.draw(&mut buf, w, h);

        // "Ink" means a *text* pixel, not merely a non-black one: every row paints a
        // background, so counting non-black pixels would report a full buffer even if not
        // a single glyph were drawn.
        let colours = list.colours;
        let is_text = |p: u32| p == colours.text || p == colours.subtitle_text;

        let lit = buf.iter().filter(|&&p| is_text(p)).count();
        assert!(
            lit > 200,
            "only {lit} text pixels — the labels are not reaching the buffer"
        );

        // Two rows, so the second row's band must carry text too: a bug that draws only
        // the first row would still pass a whole-buffer count.
        let second_row_lit = (40..80)
            .flat_map(|y| (0..w).map(move |x| y * w + x))
            .filter(|&i| is_text(buf[i]))
            .count();
        assert!(
            second_row_lit > 50,
            "the second row drew no text ({second_row_lit} pixels)"
        );

        for y in 0..h {
            let line: String = (0..w)
                .map(|x| if is_text(buf[y * w + x]) { '#' } else { '.' })
                .collect();
            if line.contains('#') {
                println!("{}", line.trim_end_matches('.'));
            }
        }
    }

    #[test]
    fn the_list_sits_between_the_header_and_the_footer() {
        let l = Layout::default();
        let (x, y, w, h) = l.list_rect(l.height);
        assert_eq!(x, l.pad);
        assert_eq!(y, l.header_h);
        assert_eq!(w, l.width - l.pad * 2);
        assert_eq!(h, l.height - l.header_h - l.footer_h);
    }

    #[test]
    fn a_window_shorter_than_its_own_chrome_yields_an_empty_list_not_a_negative_one() {
        let l = Layout::default();
        // Squashed far below the header+footer height.
        let (_, _, _, h) = l.list_rect(10);
        assert_eq!(h, 0, "a negative height would wrap around when cast to u32");
    }

    #[test]
    fn growing_the_window_grows_the_list_by_the_same_amount() {
        let l = Layout::default();
        let (_, _, _, before) = l.list_rect(l.height);
        let (_, _, _, after) = l.list_rect(l.height + 200);
        assert_eq!(after - before, 200);
    }
}
