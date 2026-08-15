//! The favourites launcher: pick a saved connection and go.
//!
//! The requirement this exists for is *"a favourite connect list on the main UI — so I
//! just double click an icon and get connected to that server"*. So the launcher is what
//! `mdrdp` shows when it is started with no arguments, and its entire job is to return one
//! [`Favourite`] to the caller. It opens no RDP socket. The one credential edge it owns is
//! moving a new form's password straight into the OS store before the password-free
//! favourite is published.
//!
//! It is a separate window from the session, opened and closed before the session window
//! exists. Running both at once would mean two event loops on one thread, which winit does
//! not allow — and a launcher that lingers behind a full-screen desktop serves nobody.
//!
//! Everything drawn here goes through [`crate::ui`], which renders into a plain pixel
//! buffer. The layout arithmetic is pulled out into [`Layout`] so it can be tested without
//! opening a window; only the winit plumbing is untested, and it is deliberately thin.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::run_on_demand::EventLoopExtRunOnDemand;
use winit::window::{Window, WindowId};

use crate::creds;
#[cfg(test)]
use crate::favourites::FavouritesError;
use crate::favourites::{Favourite, Favourites};
use crate::ui::font;
use crate::ui::form::{
    FormAction, FormField, FormLayout, KeyAction as FormKeyAction, NewConnectionForm, Rect,
};
use crate::ui::list::{DoubleClick, KeyAction, ListView, Row};
use crate::window::{SessionEvent, WindowError};

/// Milliseconds within which two clicks on one row count as a double-click.
const DOUBLE_CLICK_MS: u64 = 400;

const BG: u32 = 0x0018_1818;
const HEADING: u32 = 0x00ff_ffff;
const HINT: u32 = 0x0090_9090;
const NEW_BUTTON_WIDTH: i32 = 144;
const NEW_BUTTON_HEIGHT: i32 = 28;

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

    /// Mouse target for the visible New Connection action in the header.
    pub fn new_button_rect(&self, width: i32) -> Rect {
        Rect::new(
            (width - self.pad - NEW_BUTTON_WIDTH).max(self.pad),
            10,
            NEW_BUTTON_WIDTH,
            NEW_BUTTON_HEIGHT,
        )
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

/// Add and atomically persist one connection without changing the in-memory list unless
/// the on-disk replace succeeds. A full disk or unwritable directory therefore leaves the
/// launcher showing exactly the durable state it started with.
#[cfg(test)]
fn persist_new_connection(
    favourites: &mut Favourites,
    favourite: Favourite,
    path: &Path,
) -> Result<(), FavouritesError> {
    let mut updated = favourites.clone();
    updated.add(favourite)?;
    updated.save_to(path)?;
    *favourites = updated;
    Ok(())
}

/// Validate a new favourite, store its password in the OS credential store, then publish
/// the password-free favourite atomically.
///
/// Validation happens before the credential write, so a duplicate or malformed favourite
/// never touches the keychain. The in-memory list changes only after the durable TOML
/// replace succeeds. A disk failure can leave a harmless credential entry ready for retry,
/// but can never leave a visible favourite whose password was not stored.
fn persist_new_connection_with_credential(
    favourites: &mut Favourites,
    favourite: Favourite,
    password: &str,
    path: &Path,
    store: impl FnOnce(&str, &str) -> Result<(), String>,
) -> Result<(), String> {
    let mut updated = favourites.clone();
    updated.add(favourite.clone()).map_err(|e| e.to_string())?;
    let account = favourite
        .keychain_account
        .as_deref()
        .ok_or_else(|| "new connection has no credential-store account".to_owned())?;
    store(account, password).map_err(|e| format!("could not store the password securely: {e}"))?;
    updated.save_to(path).map_err(|e| e.to_string())?;
    *favourites = updated;
    Ok(())
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
    favourites: &mut Favourites,
    config_path: &Path,
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
        favourites: favourites.clone(),
        form: None,
        modifiers: ModifiersState::empty(),
    };

    event_loop
        .run_app_on_demand(&mut app)
        .map_err(|e| WindowError::EventLoop(e.to_string()))?;

    if let Some(e) = app.failure.take() {
        return Err(e);
    }
    *favourites = app.favourites;
    Ok(app.chosen)
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
    chosen: Option<Favourite>,
    failure: Option<WindowError>,
    config_path: PathBuf,
    favourites: Favourites,
    form: Option<NewConnectionForm>,
    modifiers: ModifiersState,
}

impl LauncherApp {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Accept the current selection and close.
    fn confirm(&mut self, event_loop: &ActiveEventLoop, index: usize) {
        if let Some(favourite) = self.favourites.iter().nth(index).cloned() {
            self.chosen = Some(favourite);
            event_loop.exit();
        }
    }

    fn open_form(&mut self) {
        self.form = Some(NewConnectionForm::new());
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn cancel_form(&mut self) {
        self.form = None;
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn apply_form_action(&mut self, action: FormAction) {
        match action {
            FormAction::None => {}
            FormAction::Cancelled => self.cancel_form(),
            FormAction::Submitted(favourite) => {
                let password = self
                    .form
                    .as_ref()
                    .map(NewConnectionForm::password)
                    .unwrap_or_default();
                match persist_new_connection_with_credential(
                    &mut self.favourites,
                    favourite,
                    password,
                    &self.config_path,
                    |account, password| {
                        creds::store(account, password).map_err(|error| error.to_string())
                    },
                ) {
                    Ok(()) => {
                        self.list.set_rows(rows_for(&self.favourites));
                        self.list.selected = self.favourites.len().checked_sub(1);
                        self.form = None;
                    }
                    Err(error) => {
                        if let Some(form) = self.form.as_mut() {
                            form.error = Some(error.to_string());
                        }
                    }
                }
            }
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn submit_form(&mut self) {
        let action = self
            .form
            .as_mut()
            .map(|form| form.handle_key(FormKeyAction::Submit))
            .unwrap_or(FormAction::None);
        self.apply_form_action(action);
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

        if let Some(form) = self.form.as_mut() {
            form.layout = FormLayout::for_window(size.width as i32, size.height as i32);
            form.draw(&mut buf, bw, bh);
            window.pre_present_notify();
            let _ = buf.present();
            return;
        }

        font::draw_text(&mut buf, bw, bh, self.layout.pad, 12, "mdrdp", HEADING);
        let hint = if self.favourites.is_empty() {
            "no saved connections yet  ·  choose New connection"
        } else {
            "double-click to connect  ·  enter to open  ·  N to add"
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

        let button = self.layout.new_button_rect(size.width as i32);
        font::draw_text(
            &mut buf,
            bw,
            bh,
            button.x + 8,
            button.y + 7,
            "New connection",
            HEADING,
        );

        if self.favourites.is_empty() {
            let y = self.layout.header_h + 8;
            font::draw_text(
                &mut buf,
                bw,
                bh,
                self.layout.pad,
                y,
                "Create a saved connection entirely in this window.",
                HINT,
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
            &self.config_path.display().to_string(),
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
                if let Some(form) = self.form.as_mut() {
                    form.layout = FormLayout::for_window(size.width as i32, size.height as i32);
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = position;
                if self.form.is_some() {
                    return;
                }
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
                if let Some(field) = self.form.as_mut().and_then(|form| form.focus_at(x, y)) {
                    match field {
                        FormField::SessionMode => {
                            if let Some(form) = self.form.as_mut() {
                                form.handle_key(FormKeyAction::ToggleMode);
                            }
                        }
                        FormField::Save => self.submit_form(),
                        FormField::Cancel => self.cancel_form(),
                        _ => {
                            if let Some(window) = &self.window {
                                window.request_redraw();
                            }
                        }
                    }
                    return;
                }

                let window_width = self
                    .window
                    .as_ref()
                    .map(|window| window.inner_size().width as i32)
                    .unwrap_or(self.layout.width);
                if self.layout.new_button_rect(window_width).contains(x, y) {
                    self.open_form();
                    return;
                }
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

            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),

            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if self.form.is_some() {
                    let focus = self.form.as_ref().map(|form| form.focus);
                    let action = match &event.logical_key {
                        Key::Named(NamedKey::Escape) => Some(FormKeyAction::Cancel),
                        Key::Named(NamedKey::Tab) if self.modifiers.shift_key() => {
                            Some(FormKeyAction::ShiftTab)
                        }
                        Key::Named(NamedKey::Tab) => Some(FormKeyAction::Tab),
                        Key::Named(NamedKey::Backspace) => Some(FormKeyAction::Backspace),
                        Key::Named(NamedKey::Enter) => match focus {
                            Some(FormField::SessionMode) => Some(FormKeyAction::ToggleMode),
                            Some(FormField::Cancel) => Some(FormKeyAction::Cancel),
                            _ => Some(FormKeyAction::Submit),
                        },
                        Key::Character(text)
                            if focus == Some(FormField::SessionMode) && text.as_str() == " " =>
                        {
                            Some(FormKeyAction::ToggleMode)
                        }
                        _ => None,
                    };
                    if let Some(action) = action {
                        let result = self
                            .form
                            .as_mut()
                            .map(|form| form.handle_key(action))
                            .unwrap_or(FormAction::None);
                        self.apply_form_action(result);
                        return;
                    }

                    if !self.modifiers.super_key()
                        && !self.modifiers.control_key()
                        && !self.modifiers.alt_key()
                        && let Key::Character(text) = &event.logical_key
                    {
                        for ch in text.chars().filter(|ch| !ch.is_control()) {
                            if let Some(form) = self.form.as_mut() {
                                form.handle_key(FormKeyAction::Character(ch));
                            }
                        }
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    }
                    return;
                }

                if matches!(&event.logical_key, Key::Character(text) if text.eq_ignore_ascii_case("n"))
                {
                    self.open_form();
                    return;
                }
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

    fn tempdir(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "mdrdp-launcher-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create launcher test directory");
        path
    }

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

    #[test]
    fn new_connection_is_durable_before_the_launcher_list_changes() {
        let dir = tempdir("persist");
        let path = dir.join("favourites.toml");
        let mut favourites = Favourites::default();
        let mut candidate = Favourite::new("Quench", "quench");
        candidate.username = Some("test-user".to_owned());

        persist_new_connection(&mut favourites, candidate.clone(), &path).expect("save");

        assert_eq!(favourites.iter().next(), Some(&candidate));
        assert_eq!(
            Favourites::load_from(&path)
                .expect("reload after a process restart")
                .iter()
                .next(),
            Some(&candidate)
        );

        std::fs::remove_file(&path).expect("remove test favourites");
        std::fs::remove_dir(&dir).expect("remove launcher test directory");
    }

    #[test]
    fn failed_persistence_does_not_show_an_entry_that_will_vanish_on_restart() {
        let dir = tempdir("failure");
        let blocker = dir.join("not-a-directory");
        std::fs::write(&blocker, b"block create_dir_all").expect("create blocker");
        let path = blocker.join("favourites.toml");
        let mut favourites = Favourites::default();

        let error =
            persist_new_connection(&mut favourites, Favourite::new("Unsaved", "host"), &path)
                .expect_err("a file cannot be used as a parent directory");

        assert!(matches!(error, FavouritesError::Io(_)));
        assert!(favourites.is_empty(), "memory must remain equal to disk");
        std::fs::remove_file(&blocker).expect("remove blocker");
        std::fs::remove_dir(&dir).expect("remove launcher test directory");
    }

    #[test]
    fn new_connection_stores_the_secret_out_of_band_and_never_in_toml() {
        let dir = tempdir("credential");
        let path = dir.join("favourites.toml");
        let mut favourites = Favourites::default();
        let mut candidate = Favourite::new("Quench", "quench");
        candidate.username = Some("test-user".to_owned());
        candidate.keychain_account = Some("test-user@quench:3389".to_owned());
        let mut stored = false;

        persist_new_connection_with_credential(
            &mut favourites,
            candidate,
            "never-write-this-secret",
            &path,
            |account, password| {
                assert_eq!(account, "test-user@quench:3389");
                assert_eq!(password, "never-write-this-secret");
                stored = true;
                Ok(())
            },
        )
        .expect("store and save");

        assert!(stored);
        let disk = std::fs::read_to_string(&path).expect("read persisted favourite");
        assert!(!disk.contains("never-write-this-secret"));
        assert_eq!(Favourites::load_from(&path).unwrap().len(), 1);
        std::fs::remove_file(&path).expect("remove test favourites");
        std::fs::remove_dir(&dir).expect("remove launcher test directory");
    }

    #[test]
    fn new_connection_mouse_target_is_half_open() {
        let layout = Layout::default();
        let button = layout.new_button_rect(layout.width);
        assert!(button.contains(button.x, button.y));
        assert!(button.contains(button.x + button.width - 1, button.y));
        assert!(!button.contains(button.x + button.width, button.y));
    }
}
