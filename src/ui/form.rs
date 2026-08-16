//! The launcher's dependency-free "New Connection" form.
//!
//! The form is intentionally a plain model plus a software renderer.  It does not know
//! about windows, event loops, keychains, or persistence; the launcher owns those edges
//! and can feed this module keyboard and mouse events.  Keeping the state here makes the
//! validation and the layout hit-tests deterministic, and lets the tests exercise the
//! screen without opening a window.

use super::font;
use crate::favourites::{Favourite, WindowSize};
use zeroize::Zeroize;

/// The values used for a newly created connection.
pub const DEFAULT_PORT_TEXT: &str = "3389";
pub const DEFAULT_WIDTH_TEXT: &str = "1920";
pub const DEFAULT_HEIGHT_TEXT: &str = "1080";

/// The two choices offered for the session window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionMode {
    /// Let the launcher/session use its full-screen/default-size behaviour.
    #[default]
    Fullscreen,
    /// Use the explicit width and height fields.
    Explicit,
}

impl SessionMode {
    fn label(self) -> &'static str {
        match self {
            Self::Fullscreen => "Fullscreen",
            Self::Explicit => "Explicit",
        }
    }
}

/// A focusable item in the form.
///
/// Width and height remain clickable even while the mode is fullscreen.  That makes
/// mouse hit-testing stable across a mode toggle and lets a user prepare a size before
/// selecting `Explicit`; keyboard traversal skips them while they are inactive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    DisplayName,
    Host,
    Port,
    Username,
    Password,
    SessionMode,
    Width,
    Height,
    Save,
    Cancel,
}

impl FormField {
    fn is_text(self) -> bool {
        matches!(
            self,
            Self::DisplayName
                | Self::Host
                | Self::Port
                | Self::Username
                | Self::Password
                | Self::Width
                | Self::Height
        )
    }

    fn label(self) -> &'static str {
        match self {
            Self::DisplayName => "Display name",
            Self::Host => "Host",
            Self::Port => "Port",
            Self::Username => "Username",
            Self::Password => "Password",
            Self::SessionMode => "Session mode",
            Self::Width => "Width",
            Self::Height => "Height",
            Self::Save => "Save",
            Self::Cancel => "Cancel",
        }
    }
}

/// An alias that reads naturally at call sites that talk about focus rather than fields.
pub type FormFocus = FormField;

/// Keyboard operations understood by [`NewConnectionForm::handle_key`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    /// Move focus forward, wrapping at the end.
    Tab,
    /// Move focus backwards, wrapping at the beginning.
    ShiftTab,
    /// Insert one character into the selected text field.
    Character(char),
    /// Remove the last character from the selected text field.
    Backspace,
    /// Toggle fullscreen/explicit session mode.
    ToggleMode,
    /// Validate and submit the form.
    Submit,
    /// Close the form without saving.
    Cancel,
}

/// The result of applying one form action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormAction {
    /// The action changed state but did not submit or cancel.
    None,
    /// The form produced a validated favourite.
    Submitted(Favourite),
    /// The caller should close the form without saving.
    Cancelled,
}

/// Why submission failed.  The form also retains the rendered text of this error in
/// [`NewConnectionForm::error`] until the next edit or mode change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormError {
    BlankDisplayName,
    BlankHost,
    BlankUsername,
    BlankPassword,
    InvalidPort,
    PortOutOfRange,
    InvalidWidth,
    WidthOutOfRange,
    InvalidHeight,
    HeightOutOfRange,
}

impl FormError {
    /// The field that needs attention for this error.
    pub const fn field(self) -> FormField {
        match self {
            Self::BlankDisplayName => FormField::DisplayName,
            Self::BlankHost => FormField::Host,
            Self::BlankUsername => FormField::Username,
            Self::BlankPassword => FormField::Password,
            Self::InvalidPort | Self::PortOutOfRange => FormField::Port,
            Self::InvalidWidth | Self::WidthOutOfRange => FormField::Width,
            Self::InvalidHeight | Self::HeightOutOfRange => FormField::Height,
        }
    }
}

impl std::fmt::Display for FormError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::BlankDisplayName => "display name must not be blank",
            Self::BlankHost => "host must not be blank",
            Self::BlankUsername => "username must not be blank",
            Self::BlankPassword => "password must not be blank",
            Self::InvalidPort => "port must be a whole number from 1 to 65535",
            Self::PortOutOfRange => "port must be between 1 and 65535",
            Self::InvalidWidth => "width must be a whole number from 1 to 65535",
            Self::WidthOutOfRange => "width must be between 1 and 65535",
            Self::InvalidHeight => "height must be a whole number from 1 to 65535",
            Self::HeightOutOfRange => "height must be between 1 and 65535",
        };
        f.write_str(text)
    }
}

impl std::error::Error for FormError {}

/// An integer-coordinate rectangle used for deterministic mouse hit-testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Test a point against the half-open rectangle `[x, x + width) × [y, y + height)`.
    pub fn contains(self, x: i32, y: i32) -> bool {
        self.width > 0
            && self.height > 0
            && x >= self.x
            && y >= self.y
            && x < self.x.saturating_add(self.width)
            && y < self.y.saturating_add(self.height)
    }
}

/// The form's fixed layout.  Coordinates are window pixels, with `(x, y)` at the
/// top-left of the form's content area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormLayout {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub padding: i32,
    pub label_width: i32,
    pub field_height: i32,
    pub row_gap: i32,
    pub button_width: i32,
    pub button_height: i32,
}

impl Default for FormLayout {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            width: 560,
            height: 580,
            padding: 24,
            label_width: 128,
            field_height: 32,
            row_gap: 20,
            button_width: 96,
            button_height: 36,
        }
    }
}

impl FormLayout {
    /// Build a layout inside a window of the supplied size.
    pub fn for_window(width: i32, height: i32) -> Self {
        Self {
            width,
            height,
            ..Self::default()
        }
    }

    fn content_x(self) -> i32 {
        self.x + self.padding
    }

    fn content_width(self) -> i32 {
        (self.width - self.padding * 2).max(0)
    }

    fn row_y(self, index: i32) -> i32 {
        self.y + self.padding + 36 + index * (self.field_height + self.row_gap)
    }

    /// The input rectangle for one field.  Width and height are returned even when the
    /// current mode is fullscreen so a mouse target never moves as mode changes.
    pub fn field_rect(self, field: FormField) -> Rect {
        let index = match field {
            FormField::DisplayName => Some(0),
            FormField::Host => Some(1),
            FormField::Port => Some(2),
            FormField::Username => Some(3),
            FormField::Password => Some(4),
            FormField::SessionMode => Some(5),
            FormField::Width => Some(6),
            FormField::Height => Some(7),
            FormField::Save | FormField::Cancel => None,
        };
        if let Some(index) = index {
            Rect::new(
                self.content_x() + self.label_width,
                self.row_y(index),
                (self.content_width() - self.label_width).max(0),
                self.field_height,
            )
        } else {
            self.button_rect(field)
        }
    }

    /// The rectangle for the Save or Cancel button.
    pub fn button_rect(self, button: FormField) -> Rect {
        let y = self.y + self.height - self.padding - self.button_height;
        let right = self.x + self.width - self.padding;
        match button {
            FormField::Save => Rect::new(
                right - self.button_width * 2 - 12,
                y,
                self.button_width,
                self.button_height,
            ),
            FormField::Cancel => Rect::new(
                right - self.button_width,
                y,
                self.button_width,
                self.button_height,
            ),
            _ => Rect::new(0, 0, 0, 0),
        }
    }

    /// Return the form item under a point, if any.
    pub fn hit_test(self, x: i32, y: i32) -> Option<FormField> {
        // Buttons are checked first because their y position may overlap the last row in
        // a caller-supplied very short layout.
        for field in [FormField::Save, FormField::Cancel] {
            if self.button_rect(field).contains(x, y) {
                return Some(field);
            }
        }
        [
            FormField::DisplayName,
            FormField::Host,
            FormField::Port,
            FormField::Username,
            FormField::Password,
            FormField::SessionMode,
            FormField::Width,
            FormField::Height,
        ]
        .into_iter()
        .find(|&field| self.field_rect(field).contains(x, y))
    }

    /// Alias for callers that name this operation after the mouse event.
    pub fn field_at(self, x: i32, y: i32) -> Option<FormField> {
        self.hit_test(x, y)
    }
}

/// Colours used by [`NewConnectionForm::draw`].  Colours are `0x00RRGGBB`, matching
/// softbuffer and the existing list/font widgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormColours {
    pub background: u32,
    pub label: u32,
    pub text: u32,
    pub muted_text: u32,
    pub field_background: u32,
    pub selected_background: u32,
    pub border: u32,
    pub selected_border: u32,
    pub button_background: u32,
    pub button_text: u32,
    pub error: u32,
}

impl Default for FormColours {
    fn default() -> Self {
        Self {
            background: 0x0018_1818,
            label: 0x0090_9090,
            text: 0x00ff_ffff,
            muted_text: 0x0060_6060,
            field_background: 0x0020_2020,
            selected_background: 0x0030_4d70,
            border: 0x0050_5050,
            selected_border: 0x006f_b7ff,
            button_background: 0x0035_5a8f,
            button_text: 0x00ff_ffff,
            error: 0x00ff_7070,
        }
    }
}

/// Mutable password text owned by the form.
///
/// This deliberately does not implement `Clone`; duplicating a password while copying a
/// form would create another plaintext allocation with an independent lifetime. `Debug`
/// is redacted for the same reason, and `Drop` wipes the allocation before releasing it.
struct PasswordBuffer(String);

impl PasswordBuffer {
    fn new() -> Self {
        Self(String::new())
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    fn replace(&mut self, value: String) {
        self.0.zeroize();
        self.0 = value;
    }

    fn push(&mut self, ch: char) {
        self.0.push(ch);
    }

    fn pop(&mut self) {
        self.0.pop();
    }
}

impl std::fmt::Debug for PasswordBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PasswordBuffer(<redacted>)")
    }
}

impl Drop for PasswordBuffer {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// State and renderer for the launcher's New Connection screen.
#[derive(Debug)]
pub struct NewConnectionForm {
    /// Display name shown in the favourites list.
    pub display_name: String,
    /// Hostname or address to connect to.
    pub host: String,
    /// Port as editable text, defaulting to [`DEFAULT_PORT_TEXT`].
    pub port: String,
    /// Account name used for RDP and as part of the keychain account key.
    pub username: String,
    /// Password entered for this session. It never enters a favourite or debug output.
    password: PasswordBuffer,
    /// Fullscreen or explicit size choice.
    pub session_mode: SessionMode,
    /// Explicit width text. Ignored while [`SessionMode::Fullscreen`] is selected.
    pub width: String,
    /// Explicit height text. Ignored while [`SessionMode::Fullscreen`] is selected.
    pub height: String,
    /// The currently selected input or button.
    pub focus: FormField,
    /// Human-readable validation feedback, retained for the renderer after a failed
    /// submit. It contains no credential text.
    pub error: Option<String>,
    /// Geometry used by hit-testing and rendering.
    pub layout: FormLayout,
    /// Palette used by rendering.
    pub colours: FormColours,
}

impl Default for NewConnectionForm {
    fn default() -> Self {
        Self::new()
    }
}

impl NewConnectionForm {
    /// Construct a blank form with the standard port, fullscreen mode, and 1080p values
    /// ready if the user switches to explicit mode.
    pub fn new() -> Self {
        Self {
            display_name: String::new(),
            host: String::new(),
            port: DEFAULT_PORT_TEXT.to_owned(),
            username: String::new(),
            password: PasswordBuffer::new(),
            session_mode: SessionMode::Fullscreen,
            width: DEFAULT_WIDTH_TEXT.to_owned(),
            height: DEFAULT_HEIGHT_TEXT.to_owned(),
            focus: FormField::DisplayName,
            error: None,
            layout: FormLayout::default(),
            colours: FormColours::default(),
        }
    }

    /// Set the layout used for the next draw/hit-test.
    pub fn with_layout(mut self, layout: FormLayout) -> Self {
        self.layout = layout;
        self
    }

    /// Select the field under a mouse coordinate. Returns the selected field, or `None`
    /// if the point is outside the form.
    pub fn mouse_focus(&mut self, x: i32, y: i32) -> Option<FormField> {
        let field = self.layout.hit_test(x, y)?;
        self.focus = field;
        Some(field)
    }

    /// Alias for [`Self::mouse_focus`] used by event-loop code that calls this a hit-test.
    pub fn focus_at(&mut self, x: i32, y: i32) -> Option<FormField> {
        self.mouse_focus(x, y)
    }

    /// Borrow the entered password for the launcher-to-keychain hand-off.
    pub(crate) fn password(&self) -> &str {
        self.password.as_str()
    }

    /// The current text or mode label for a field. Buttons have no value.
    pub fn value(&self, field: FormField) -> Option<&str> {
        match field {
            FormField::DisplayName => Some(&self.display_name),
            FormField::Host => Some(&self.host),
            FormField::Port => Some(&self.port),
            FormField::Username => Some(&self.username),
            FormField::Password => Some("<redacted>"),
            FormField::SessionMode => Some(self.session_mode.label()),
            FormField::Width => Some(&self.width),
            FormField::Height => Some(&self.height),
            FormField::Save | FormField::Cancel => None,
        }
    }

    /// Set one editable value.  This is mainly useful when a launcher restores draft
    /// state; normal input should use [`Self::handle_key`].
    pub fn set_value(&mut self, field: FormField, value: impl Into<String>) -> bool {
        let value = value.into();
        let target = match field {
            FormField::DisplayName => &mut self.display_name,
            FormField::Host => &mut self.host,
            FormField::Port => &mut self.port,
            FormField::Username => &mut self.username,
            FormField::Width => &mut self.width,
            FormField::Height => &mut self.height,
            FormField::Password => {
                self.password.replace(value);
                self.error = None;
                return true;
            }
            FormField::SessionMode | FormField::Save | FormField::Cancel => return false,
        };
        *target = value;
        self.error = None;
        true
    }

    /// Handle one keyboard operation.
    pub fn handle_key(&mut self, action: KeyAction) -> FormAction {
        match action {
            KeyAction::Tab => {
                self.move_focus(1);
                FormAction::None
            }
            KeyAction::ShiftTab => {
                self.move_focus(-1);
                FormAction::None
            }
            KeyAction::Character(ch) => {
                if self.focus.is_text() {
                    self.insert_character(ch);
                }
                FormAction::None
            }
            KeyAction::Backspace => {
                if self.focus.is_text() {
                    self.backspace();
                }
                FormAction::None
            }
            KeyAction::ToggleMode => {
                self.session_mode = match self.session_mode {
                    SessionMode::Fullscreen => SessionMode::Explicit,
                    SessionMode::Explicit => SessionMode::Fullscreen,
                };
                self.error = None;
                FormAction::None
            }
            KeyAction::Submit => match self.submit() {
                Ok(favourite) => FormAction::Submitted(favourite),
                Err(_) => FormAction::None,
            },
            KeyAction::Cancel => FormAction::Cancelled,
        }
    }

    /// Alias for [`Self::handle_key`].
    pub fn key(&mut self, action: KeyAction) -> FormAction {
        self.handle_key(action)
    }

    /// Submit the current values after validation, producing a password-free favourite.
    pub fn submit(&mut self) -> Result<Favourite, FormError> {
        let name = self.display_name.trim();
        if name.is_empty() {
            return self.reject(FormError::BlankDisplayName);
        }
        let host = self.host.trim();
        if host.is_empty() {
            return self.reject(FormError::BlankHost);
        }
        let username = self.username.trim();
        if username.is_empty() {
            return self.reject(FormError::BlankUsername);
        }
        let password = self.password.as_str();
        if password.is_empty() {
            return self.reject(FormError::BlankPassword);
        }
        let port = match parse_port(&self.port, FormField::Port) {
            Ok(value) => value,
            Err(error) => return self.reject(error),
        };

        let window_size = match self.session_mode {
            SessionMode::Fullscreen => WindowSize::Fullscreen,
            SessionMode::Explicit => {
                let width = match parse_port(&self.width, FormField::Width) {
                    Ok(value) => value,
                    Err(error) => return self.reject(error),
                };
                let height = match parse_port(&self.height, FormField::Height) {
                    Ok(value) => value,
                    Err(error) => return self.reject(error),
                };
                WindowSize::Explicit { width, height }
            }
        };

        self.error = None;
        let keychain_account = format!("{username}@{host}:{port}");
        Ok(Favourite {
            name: name.to_owned(),
            host: host.to_owned(),
            port,
            username: Some(username.to_owned()),
            domain: None,
            window_size,
            keychain_account: Some(keychain_account),
            last_used: None,
        })
    }

    /// The last validation error as text, if any.
    pub fn error_message(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Render the complete form into a software framebuffer.
    ///
    /// The input buffer is clipped just like [`font::draw_text`].  A short or zero-sized
    /// buffer is therefore harmless, which matters while a resizable window is between
    /// surface sizes.
    pub fn draw(&self, buf: &mut [u32], buf_w: usize, buf_h: usize) {
        buf.fill(self.colours.background);
        let layout = self.layout;

        draw_text_clipped(
            buf,
            buf_w,
            buf_h,
            layout.content_x(),
            layout.y + layout.padding,
            "New connection",
            self.colours.text,
        );

        for field in [
            FormField::DisplayName,
            FormField::Host,
            FormField::Port,
            FormField::Username,
            FormField::Password,
            FormField::SessionMode,
            FormField::Width,
            FormField::Height,
        ] {
            let rect = layout.field_rect(field);
            let active = field == self.focus;
            let disabled = matches!(field, FormField::Width | FormField::Height)
                && self.session_mode == SessionMode::Fullscreen;
            fill_rect(
                buf,
                buf_w,
                buf_h,
                rect,
                if active {
                    self.colours.selected_background
                } else {
                    self.colours.field_background
                },
            );
            stroke_rect(
                buf,
                buf_w,
                buf_h,
                rect,
                if active {
                    self.colours.selected_border
                } else {
                    self.colours.border
                },
            );

            let label_y = rect.y + (layout.field_height - font::CHAR_H) / 2;
            draw_text_clipped(
                buf,
                buf_w,
                buf_h,
                layout.content_x(),
                label_y,
                field.label(),
                self.colours.label,
            );
            let value = self.rendered_value(field);
            draw_text_clipped(
                buf,
                buf_w,
                buf_h,
                rect.x + 8,
                label_y,
                &value,
                if disabled {
                    self.colours.muted_text
                } else {
                    self.colours.text
                },
            );
        }

        if let Some(error) = &self.error {
            // Leave one glyph cell between the explicit-height row and the buttons so a
            // long validation message cannot paint over the button labels.
            let y = layout.row_y(7) - font::CHAR_H / 2;
            draw_text_clipped(
                buf,
                buf_w,
                buf_h,
                layout.content_x(),
                y,
                error,
                self.colours.error,
            );
        }

        for field in [FormField::Save, FormField::Cancel] {
            let rect = layout.button_rect(field);
            fill_rect(
                buf,
                buf_w,
                buf_h,
                rect,
                if self.focus == field {
                    self.colours.selected_background
                } else {
                    self.colours.button_background
                },
            );
            stroke_rect(
                buf,
                buf_w,
                buf_h,
                rect,
                if self.focus == field {
                    self.colours.selected_border
                } else {
                    self.colours.border
                },
            );
            let text_width = font::text_width(field.label());
            draw_text_clipped(
                buf,
                buf_w,
                buf_h,
                rect.x + (rect.width - text_width) / 2,
                rect.y + (rect.height - font::CHAR_H) / 2,
                field.label(),
                self.colours.button_text,
            );
        }
    }

    /// Alias for [`Self::draw`].
    pub fn render(&self, buf: &mut [u32], buf_w: usize, buf_h: usize) {
        self.draw(buf, buf_w, buf_h);
    }

    fn focus_order(&self) -> Vec<FormField> {
        let mut fields = vec![
            FormField::DisplayName,
            FormField::Host,
            FormField::Port,
            FormField::Username,
            FormField::Password,
            FormField::SessionMode,
        ];
        if self.session_mode == SessionMode::Explicit {
            fields.extend([FormField::Width, FormField::Height]);
        }
        fields.extend([FormField::Save, FormField::Cancel]);
        fields
    }

    fn move_focus(&mut self, delta: i32) {
        let order = self.focus_order();
        let current = order.iter().position(|&field| field == self.focus);
        let current = current.unwrap_or(0) as i32;
        let len = order.len() as i32;
        let next = (current + delta).rem_euclid(len) as usize;
        self.focus = order[next];
    }

    fn insert_character(&mut self, ch: char) {
        if self.focus == FormField::Password {
            self.password.push(ch);
        } else if let Some(target) = self.text_mut(self.focus) {
            target.push(ch);
        } else {
            return;
        }
        self.error = None;
    }

    fn backspace(&mut self) {
        if self.focus == FormField::Password {
            self.password.pop();
        } else if let Some(target) = self.text_mut(self.focus) {
            target.pop();
        } else {
            return;
        }
        self.error = None;
    }

    fn text_mut(&mut self, field: FormField) -> Option<&mut String> {
        match field {
            FormField::DisplayName => Some(&mut self.display_name),
            FormField::Host => Some(&mut self.host),
            FormField::Port => Some(&mut self.port),
            FormField::Username => Some(&mut self.username),
            FormField::Width => Some(&mut self.width),
            FormField::Height => Some(&mut self.height),
            FormField::Password | FormField::SessionMode | FormField::Save | FormField::Cancel => {
                None
            }
        }
    }

    fn rendered_value(&self, field: FormField) -> String {
        if field == FormField::Password {
            return "*".repeat(self.password.as_str().chars().count());
        }
        self.value(field).unwrap_or_default().to_owned()
    }

    fn reject<T>(&mut self, error: FormError) -> Result<T, FormError> {
        self.focus = error.field();
        self.error = Some(error.to_string());
        Err(error)
    }
}

fn parse_port(text: &str, field: FormField) -> Result<u16, FormError> {
    let parsed = match text.trim().parse::<u32>() {
        Ok(value) => value,
        Err(_) => {
            return Err(match field {
                FormField::Port => FormError::InvalidPort,
                FormField::Width => FormError::InvalidWidth,
                FormField::Height => FormError::InvalidHeight,
                _ => unreachable!("parse_port only accepts numeric form fields"),
            });
        }
    };
    if !(1..=u32::from(u16::MAX)).contains(&parsed) {
        return Err(match field {
            FormField::Port => FormError::PortOutOfRange,
            FormField::Width => FormError::WidthOutOfRange,
            FormField::Height => FormError::HeightOutOfRange,
            _ => unreachable!("parse_port only accepts numeric form fields"),
        });
    }
    Ok(parsed as u16)
}

fn draw_text_clipped(
    buf: &mut [u32],
    buf_w: usize,
    buf_h: usize,
    x: i32,
    y: i32,
    text: &str,
    colour: u32,
) {
    font::draw_text(buf, buf_w, buf_h, x, y, text, colour);
}

fn fill_rect(buf: &mut [u32], buf_w: usize, buf_h: usize, rect: Rect, colour: u32) {
    if rect.width <= 0 || rect.height <= 0 {
        return;
    }
    let x0 = rect.x.max(0);
    let y0 = rect.y.max(0);
    let x1 = rect.x.saturating_add(rect.width).min(buf_w as i32);
    let y1 = rect.y.saturating_add(rect.height).min(buf_h as i32);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    for y in y0..y1 {
        let Some(row) = (y as usize).checked_mul(buf_w) else {
            continue;
        };
        for x in x0..x1 {
            if let Some(pixel) = row
                .checked_add(x as usize)
                .and_then(|index| buf.get_mut(index))
            {
                *pixel = colour;
            }
        }
    }
}

fn stroke_rect(buf: &mut [u32], buf_w: usize, buf_h: usize, rect: Rect, colour: u32) {
    if rect.width <= 0 || rect.height <= 0 {
        return;
    }
    fill_rect(
        buf,
        buf_w,
        buf_h,
        Rect::new(rect.x, rect.y, rect.width, 1),
        colour,
    );
    fill_rect(
        buf,
        buf_w,
        buf_h,
        Rect::new(rect.x, rect.y + rect.height - 1, rect.width, 1),
        colour,
    );
    fill_rect(
        buf,
        buf_w,
        buf_h,
        Rect::new(rect.x, rect.y, 1, rect.height),
        colour,
    );
    fill_rect(
        buf,
        buf_w,
        buf_h,
        Rect::new(rect.x + rect.width - 1, rect.y, 1, rect.height),
        colour,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled_form() -> NewConnectionForm {
        let mut form = NewConnectionForm::new();
        form.display_name = "Alpha Desk".to_owned();
        form.host = "rdp.example.test".to_owned();
        form.port = "3391".to_owned();
        form.username = "alice@example.test".to_owned();
        form.set_value(FormField::Password, "s3cret");
        form
    }

    #[test]
    fn defaults_are_safe_and_contain_no_credential_field() {
        let form = NewConnectionForm::new();
        assert_eq!(form.port, "3389");
        assert_eq!(form.session_mode, SessionMode::Fullscreen);
        assert_eq!(form.width, "1920");
        assert_eq!(form.height, "1080");
        assert_eq!(form.focus, FormField::DisplayName);
        assert_eq!(form.password(), "");
    }

    #[test]
    fn submit_preserves_distinct_values_and_derives_keychain_account() {
        let mut form = filled_form();
        let favourite = form.submit().expect("filled form should validate");
        assert_eq!(favourite.name, "Alpha Desk");
        assert_eq!(favourite.host, "rdp.example.test");
        assert_eq!(favourite.port, 3391);
        assert_eq!(favourite.username.as_deref(), Some("alice@example.test"));
        assert_eq!(favourite.domain, None);
        assert_eq!(
            favourite.keychain_account.as_deref(),
            Some("alice@example.test@rdp.example.test:3391")
        );
        assert_eq!(favourite.window_size, WindowSize::Fullscreen);
    }

    #[test]
    fn explicit_mode_submits_distinct_dimensions() {
        let mut form = filled_form();
        form.session_mode = SessionMode::Explicit;
        form.width = "1366".to_owned();
        form.height = "768".to_owned();
        let favourite = form.submit().expect("explicit dimensions should validate");
        assert_eq!(
            favourite.window_size,
            WindowSize::Explicit {
                width: 1366,
                height: 768
            }
        );
    }

    #[test]
    fn invalid_port_is_rejected_and_retained_for_rendering() {
        let mut form = filled_form();
        form.port = "not-a-port".to_owned();
        assert_eq!(form.submit(), Err(FormError::InvalidPort));
        assert_eq!(
            form.error_message(),
            Some("port must be a whole number from 1 to 65535")
        );
        assert_eq!(form.focus, FormField::Port);
    }

    #[test]
    fn zero_and_large_numeric_values_are_rejected() {
        let mut form = filled_form();
        for (value, error) in [
            ("0", FormError::PortOutOfRange),
            ("65536", FormError::PortOutOfRange),
        ] {
            form.port = value.to_owned();
            assert_eq!(form.submit(), Err(error));
            assert_eq!(
                form.error_message(),
                Some("port must be between 1 and 65535")
            );
        }

        form.session_mode = SessionMode::Explicit;
        form.port = "3389".to_owned();
        form.width = "0".to_owned();
        assert_eq!(form.submit(), Err(FormError::WidthOutOfRange));
        form.width = "65536".to_owned();
        assert_eq!(form.submit(), Err(FormError::WidthOutOfRange));
        form.width = "1".to_owned();
        form.height = "65536".to_owned();
        assert_eq!(form.submit(), Err(FormError::HeightOutOfRange));
    }

    #[test]
    fn blank_required_fields_are_rejected_in_order() {
        let mut form = NewConnectionForm::new();
        assert_eq!(form.submit(), Err(FormError::BlankDisplayName));
        form.display_name = "Desk".to_owned();
        assert_eq!(form.submit(), Err(FormError::BlankHost));
        form.host = "host".to_owned();
        assert_eq!(form.submit(), Err(FormError::BlankUsername));
        form.username = "alice".to_owned();
        assert_eq!(form.submit(), Err(FormError::BlankPassword));
    }

    #[test]
    fn tab_and_shift_tab_traverse_active_fields_and_buttons() {
        let mut form = NewConnectionForm::new();
        let mut seen = Vec::new();
        for _ in 0..8 {
            form.handle_key(KeyAction::Tab);
            seen.push(form.focus);
        }
        assert_eq!(
            seen,
            vec![
                FormField::Host,
                FormField::Port,
                FormField::Username,
                FormField::Password,
                FormField::SessionMode,
                FormField::Save,
                FormField::Cancel,
                FormField::DisplayName,
            ]
        );
        form.handle_key(KeyAction::ShiftTab);
        assert_eq!(form.focus, FormField::Cancel);

        form.session_mode = SessionMode::Explicit;
        form.focus = FormField::SessionMode;
        form.handle_key(KeyAction::Tab);
        assert_eq!(form.focus, FormField::Width);
        form.handle_key(KeyAction::ShiftTab);
        assert_eq!(form.focus, FormField::SessionMode);
    }

    #[test]
    fn character_and_backspace_edit_the_selected_value() {
        let mut form = NewConnectionForm::new();
        form.focus = FormField::Host;
        form.handle_key(KeyAction::Character('r'));
        form.handle_key(KeyAction::Character('d'));
        form.handle_key(KeyAction::Backspace);
        assert_eq!(form.host, "r");
        form.focus = FormField::Password;
        form.handle_key(KeyAction::Character('s'));
        form.handle_key(KeyAction::Character('3'));
        form.handle_key(KeyAction::Backspace);
        assert_eq!(form.password(), "s");
        form.focus = FormField::SessionMode;
        form.handle_key(KeyAction::ToggleMode);
        assert_eq!(form.session_mode, SessionMode::Explicit);
        form.handle_key(KeyAction::ToggleMode);
        assert_eq!(form.session_mode, SessionMode::Fullscreen);
    }

    #[test]
    fn mouse_targets_are_deterministic_and_include_buttons() {
        let form = NewConnectionForm::new();
        let layout = form.layout;
        for field in [
            FormField::DisplayName,
            FormField::Host,
            FormField::Port,
            FormField::Username,
            FormField::Password,
            FormField::SessionMode,
            FormField::Width,
            FormField::Height,
        ] {
            let rect = layout.field_rect(field);
            assert_eq!(layout.hit_test(rect.x + 1, rect.y + 1), Some(field));
            assert_eq!(layout.hit_test(rect.x + rect.width, rect.y + 1), None);
        }
        for field in [FormField::Save, FormField::Cancel] {
            let rect = layout.button_rect(field);
            assert_eq!(layout.field_at(rect.x + 1, rect.y + 1), Some(field));
        }
        assert_eq!(layout.hit_test(-1, -1), None);
    }

    #[test]
    fn draw_uses_text_colour_for_values_and_selected_border_for_focus() {
        let mut form = filled_form();
        form.focus = FormField::Host;
        let (w, h) = (560usize, 520usize);
        let mut buf = vec![0u32; w * h];
        form.draw(&mut buf, w, h);
        assert!(
            buf.contains(&form.colours.text),
            "at least one label or value must render with the value text colour"
        );
        let host = form.layout.field_rect(FormField::Host);
        assert!(
            buf.contains(&form.colours.selected_border),
            "selected field must render a selected border"
        );
        assert_eq!(
            buf[host.y as usize * w + host.x as usize],
            form.colours.selected_border
        );

        form.error = Some("host must not be blank".to_owned());
        form.draw(&mut buf, w, h);
        assert!(buf.contains(&form.colours.error));
    }

    #[test]
    fn draw_clips_short_buffer_without_panicking() {
        let form = NewConnectionForm::new();
        let mut buf = vec![0u32; 12];
        form.draw(&mut buf, 560, 520);
        assert_eq!(buf.len(), 12);
    }

    #[test]
    fn password_is_required_and_never_appears_in_public_values_or_debug() {
        let mut form = filled_form();
        let secret = form.password().to_owned();
        assert_eq!(form.value(FormField::Password), Some("<redacted>"));
        assert!(
            !form
                .value(FormField::Password)
                .unwrap_or_default()
                .contains(&secret)
        );
        let debug = format!("{form:?}");
        assert!(!debug.contains(&secret));
        form.set_value(FormField::Password, "");
        assert_eq!(form.submit(), Err(FormError::BlankPassword));
        assert_eq!(form.error_message(), Some("password must not be blank"));
    }

    #[test]
    fn password_renderer_uses_one_mask_glyph_per_character() {
        let mut form = NewConnectionForm::new();
        form.set_value(FormField::Password, "sëcret");
        assert_eq!(form.rendered_value(FormField::Password), "******");
        assert!(!form.rendered_value(FormField::Password).contains("sëcret"));
    }

    #[test]
    fn submit_action_returns_favourite_and_cancel_action_returns_cancelled() {
        let mut form = filled_form();
        assert!(matches!(
            form.handle_key(KeyAction::Submit),
            FormAction::Submitted(Favourite { .. })
        ));
        assert_eq!(form.handle_key(KeyAction::Cancel), FormAction::Cancelled);
    }

    #[test]
    fn default_port_constant_matches_favourite_model() {
        assert_eq!(
            DEFAULT_PORT_TEXT.parse::<u16>(),
            Ok(crate::favourites::DEFAULT_PORT)
        );
    }
}
