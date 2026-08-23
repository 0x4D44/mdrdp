//! Input translation: winit events in, RDP fast-path input PDUs out.
//!
//! Two stages, deliberately separated so both are testable without a window and without
//! a server:
//!
//! 1. **winit -> [`InputEvent`]** — an RDP-shaped enum. Scancodes, session pixels,
//!    button identities. Nothing winit-specific survives this stage.
//! 2. **[`InputEvent`] -> [`FastPathInputEvent`]** — the wire types from `ironrdp-pdu`.
//!
//! The split matters because stage 1 is where platform quirks live (macOS Command is
//! `SuperLeft`, trackpads send pixel deltas rather than lines) and stage 2 is where
//! protocol quirks live (9-bit two's-complement wheel units, the extended-key prefix).
//! Mixing them produces a function nobody can test.
//!
//! **Security:** an [`InputEvent`] *is* session content — a scancode stream is what the
//! user typed, including passwords. `Debug` is derived because tests need it. Never log,
//! print, or serialise one.

use ironrdp::core::encode_vec;
use ironrdp::pdu::input::fast_path::{FastPathInput, FastPathInputEvent, KeyboardFlags};
use ironrdp::pdu::input::mouse::PointerFlags;
use ironrdp::pdu::input::mouse_x::PointerXFlags;
use ironrdp::pdu::input::{MousePdu, MouseXPdu};
use std::sync::Mutex;
use winit::event::{ElementState, KeyEvent, MouseScrollDelta};
use winit::keyboard::{KeyCode, PhysicalKey};

/// One wheel notch, in the units MS-RDPBCGR 2.2.8.1.1.3.1.1.3 counts.
pub const WHEEL_UNITS_PER_NOTCH: i32 = 120;

/// The wire field is 9-bit two's complement, so this is its whole representable range.
///
/// `MousePdu::encode` has a `debug_assert!` on exactly this range — an unclamped
/// multi-notch scroll would panic a debug build and silently truncate a release one.
pub const WHEEL_UNITS_MIN: i32 = -256;
pub const WHEEL_UNITS_MAX: i32 = 255;

/// Pixels of trackpad travel treated as one wheel notch.
///
/// A tuning constant, not a protocol one: winit reports trackpad scrolling as
/// [`MouseScrollDelta::PixelDelta`], and RDP has no pixel-scroll concept, so something
/// has to pick the ratio.
pub const PIXELS_PER_NOTCH: f64 = 40.0;

/// Most notches one winit event may spend. Beyond this the rest of that event's travel
/// is dropped rather than held, so the remote can never keep scrolling after the user's
/// hand has stopped.
pub const MAX_NOTCHES_PER_EVENT: i32 = 32;

/// A PS/2 Set 1 make code, plus whether it is reached through the `E0` prefix.
///
/// Fast-path carries the code as a single byte and the prefix as a flag, so those are
/// the two things worth storing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Scancode {
    pub code: u8,
    pub extended: bool,
}

impl Scancode {
    pub const fn plain(code: u8) -> Self {
        Scancode {
            code,
            extended: false,
        }
    }

    pub const fn extended(code: u8) -> Self {
        Scancode {
            code,
            extended: true,
        }
    }
}

/// The buttons RDP can name. `X1`/`X2` ride a different PDU from the other three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollAxis {
    Vertical,
    Horizontal,
}

/// An input event in RDP's terms: scancodes and session pixels, never winit types.
///
/// **This is session content.** Never log or serialise it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    Key {
        scancode: Scancode,
        down: bool,
    },
    MouseMove {
        x: u16,
        y: u16,
    },
    MouseButton {
        button: MouseButton,
        down: bool,
        x: u16,
        y: u16,
    },
    Scroll {
        axis: ScrollAxis,
        /// Already clamped to the 9-bit wire range.
        units: i16,
        x: u16,
        y: u16,
    },
}

/// Window-system pointer motion waiting for the transport input pump.
///
/// Kept separate from reliable keys, buttons, wheels, and scripted input so physical
/// mouse motion cannot put an arbitrary FIFO ahead of a keystroke on either transport.
struct LatestMouseMoveState {
    position: Option<(u16, u16)>,
    receiver_alive: bool,
}

pub struct LatestMouseMove(Mutex<LatestMouseMoveState>);

impl Default for LatestMouseMove {
    fn default() -> Self {
        Self(Mutex::new(LatestMouseMoveState {
            position: None,
            receiver_alive: true,
        }))
    }
}

impl LatestMouseMove {
    pub fn replace(&self, x: u16, y: u16) -> bool {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.receiver_alive {
            return false;
        }
        state.position = Some((x, y));
        true
    }

    pub fn clear(&self) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .position
            .take();
    }

    pub fn close(&self) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.receiver_alive = false;
        state.position = None;
    }

    pub fn take(&self) -> Option<InputEvent> {
        let (x, y) = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .position
            .take()?;
        Some(InputEvent::MouseMove { x, y })
    }
}

/// How a window position becomes a session pixel.
///
/// The window is scaled and letterboxed, so the presenter owns this mapping and input
/// borrows it. Input must never assume 1:1 — that assumption is invisible until someone
/// resizes the window and every click lands in the wrong place.
pub trait PointerMap {
    /// Map a window-relative physical-pixel position to a session pixel.
    ///
    /// Returns `None` only when there is nothing to map onto (a zero-area viewport).
    /// Positions outside the image are clamped to its edge rather than dropped, so a
    /// drag that leaves the image still tracks.
    fn to_session(&self, x: f64, y: f64) -> Option<(u16, u16)>;
}

/// A 1:1 mapping, for an unscaled window and for tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityMap {
    pub width: u16,
    pub height: u16,
}

impl IdentityMap {
    pub fn new(width: u16, height: u16) -> Self {
        IdentityMap { width, height }
    }
}

impl PointerMap for IdentityMap {
    fn to_session(&self, x: f64, y: f64) -> Option<(u16, u16)> {
        clamp_to_session(x, y, self.width, self.height)
    }
}

/// Clamp a floating-point position into `[0, width) x [0, height)`.
///
/// Shared with the presenter's letterbox mapping so both round the same way.
pub fn clamp_to_session(x: f64, y: f64, width: u16, height: u16) -> Option<(u16, u16)> {
    if width == 0 || height == 0 {
        return None;
    }
    let cx = x.max(0.0).min(f64::from(width - 1));
    let cy = y.max(0.0).min(f64::from(height - 1));
    Some((cx as u16, cy as u16))
}

// --- stage 1: winit -> InputEvent ------------------------------------------------

/// Map a physical key to its PS/2 Set 1 scancode.
///
/// Physical, not logical: RDP scancodes describe *where* the key is, and the server
/// applies its own layout. Using the logical key would double-apply the client layout.
///
/// Unmapped by design: `PrintScreen` and `Pause` (multi-byte E0/E1 sequences that do not
/// fit the one-byte-plus-flag fast-path shape), F13 and above, the media and browser
/// keys, and the numpad's calculator-style extras.
pub fn scancode_for(key: KeyCode) -> Option<Scancode> {
    use KeyCode as K;

    let sc = match key {
        // Number row.
        K::Backquote => Scancode::plain(0x29),
        K::Digit1 => Scancode::plain(0x02),
        K::Digit2 => Scancode::plain(0x03),
        K::Digit3 => Scancode::plain(0x04),
        K::Digit4 => Scancode::plain(0x05),
        K::Digit5 => Scancode::plain(0x06),
        K::Digit6 => Scancode::plain(0x07),
        K::Digit7 => Scancode::plain(0x08),
        K::Digit8 => Scancode::plain(0x09),
        K::Digit9 => Scancode::plain(0x0A),
        K::Digit0 => Scancode::plain(0x0B),
        K::Minus => Scancode::plain(0x0C),
        K::Equal => Scancode::plain(0x0D),
        K::Backspace => Scancode::plain(0x0E),

        // Top letter row.
        K::Tab => Scancode::plain(0x0F),
        K::KeyQ => Scancode::plain(0x10),
        K::KeyW => Scancode::plain(0x11),
        K::KeyE => Scancode::plain(0x12),
        K::KeyR => Scancode::plain(0x13),
        K::KeyT => Scancode::plain(0x14),
        K::KeyY => Scancode::plain(0x15),
        K::KeyU => Scancode::plain(0x16),
        K::KeyI => Scancode::plain(0x17),
        K::KeyO => Scancode::plain(0x18),
        K::KeyP => Scancode::plain(0x19),
        K::BracketLeft => Scancode::plain(0x1A),
        K::BracketRight => Scancode::plain(0x1B),
        K::Backslash => Scancode::plain(0x2B),

        // Home row.
        K::CapsLock => Scancode::plain(0x3A),
        K::KeyA => Scancode::plain(0x1E),
        K::KeyS => Scancode::plain(0x1F),
        K::KeyD => Scancode::plain(0x20),
        K::KeyF => Scancode::plain(0x21),
        K::KeyG => Scancode::plain(0x22),
        K::KeyH => Scancode::plain(0x23),
        K::KeyJ => Scancode::plain(0x24),
        K::KeyK => Scancode::plain(0x25),
        K::KeyL => Scancode::plain(0x26),
        K::Semicolon => Scancode::plain(0x27),
        K::Quote => Scancode::plain(0x28),
        K::Enter => Scancode::plain(0x1C),

        // Bottom letter row.
        K::ShiftLeft => Scancode::plain(0x2A),
        K::IntlBackslash => Scancode::plain(0x56),
        K::KeyZ => Scancode::plain(0x2C),
        K::KeyX => Scancode::plain(0x2D),
        K::KeyC => Scancode::plain(0x2E),
        K::KeyV => Scancode::plain(0x2F),
        K::KeyB => Scancode::plain(0x30),
        K::KeyN => Scancode::plain(0x31),
        K::KeyM => Scancode::plain(0x32),
        K::Comma => Scancode::plain(0x33),
        K::Period => Scancode::plain(0x34),
        K::Slash => Scancode::plain(0x35),
        K::ShiftRight => Scancode::plain(0x36),

        // Modifiers and space. macOS Command arrives as Super, which is the Windows key
        // — exactly the mapping we want, and the reason this needs no `cfg`.
        K::ControlLeft => Scancode::plain(0x1D),
        K::SuperLeft => Scancode::extended(0x5B),
        K::AltLeft => Scancode::plain(0x38),
        K::Space => Scancode::plain(0x39),
        K::AltRight => Scancode::extended(0x38),
        K::SuperRight => Scancode::extended(0x5C),
        K::ContextMenu => Scancode::extended(0x5D),
        K::ControlRight => Scancode::extended(0x1D),
        K::Escape => Scancode::plain(0x01),

        // Function row.
        K::F1 => Scancode::plain(0x3B),
        K::F2 => Scancode::plain(0x3C),
        K::F3 => Scancode::plain(0x3D),
        K::F4 => Scancode::plain(0x3E),
        K::F5 => Scancode::plain(0x3F),
        K::F6 => Scancode::plain(0x40),
        K::F7 => Scancode::plain(0x41),
        K::F8 => Scancode::plain(0x42),
        K::F9 => Scancode::plain(0x43),
        K::F10 => Scancode::plain(0x44),
        K::F11 => Scancode::plain(0x57),
        K::F12 => Scancode::plain(0x58),

        // Navigation cluster — all E0-prefixed.
        K::Insert => Scancode::extended(0x52),
        K::Delete => Scancode::extended(0x53),
        K::Home => Scancode::extended(0x47),
        K::End => Scancode::extended(0x4F),
        K::PageUp => Scancode::extended(0x49),
        K::PageDown => Scancode::extended(0x51),
        K::ArrowUp => Scancode::extended(0x48),
        K::ArrowDown => Scancode::extended(0x50),
        K::ArrowLeft => Scancode::extended(0x4B),
        K::ArrowRight => Scancode::extended(0x4D),

        // Numpad. Only Enter and Divide take the E0 prefix.
        K::NumLock => Scancode::plain(0x45),
        K::ScrollLock => Scancode::plain(0x46),
        K::NumpadDivide => Scancode::extended(0x35),
        K::NumpadMultiply => Scancode::plain(0x37),
        K::NumpadSubtract => Scancode::plain(0x4A),
        K::NumpadAdd => Scancode::plain(0x4E),
        K::NumpadEnter => Scancode::extended(0x1C),
        K::NumpadDecimal => Scancode::plain(0x53),
        K::Numpad0 => Scancode::plain(0x52),
        K::Numpad1 => Scancode::plain(0x4F),
        K::Numpad2 => Scancode::plain(0x50),
        K::Numpad3 => Scancode::plain(0x51),
        K::Numpad4 => Scancode::plain(0x4B),
        K::Numpad5 => Scancode::plain(0x4C),
        K::Numpad6 => Scancode::plain(0x4D),
        K::Numpad7 => Scancode::plain(0x47),
        K::Numpad8 => Scancode::plain(0x48),
        K::Numpad9 => Scancode::plain(0x49),

        // Japanese 106/109 keys, which cost nothing to support.
        K::IntlRo => Scancode::plain(0x73),
        K::IntlYen => Scancode::plain(0x7D),
        K::Convert => Scancode::plain(0x79),
        K::NonConvert => Scancode::plain(0x7B),
        K::KanaMode => Scancode::plain(0x70),

        _ => return None,
    };
    Some(sc)
}

/// Translate a winit key event. Returns `None` for keys this client does not map.
///
/// Auto-repeat is forwarded: RDP expects the client to send the repeats.
pub fn from_key_event(event: &KeyEvent) -> Option<InputEvent> {
    let PhysicalKey::Code(code) = event.physical_key else {
        return None;
    };
    #[cfg(target_os = "macos")]
    let scancode = if code == KeyCode::Backquote {
        macos_backquote_scancode(event)
    } else {
        scancode_for(code)?
    };
    #[cfg(not(target_os = "macos"))]
    let scancode = scancode_for(code)?;
    Some(InputEvent::Key {
        scancode,
        down: event.state == ElementState::Pressed,
    })
}

/// Recover the ISO 102nd key that winit's macOS backend loses.
///
/// winit 0.30 maps both macOS corner keycodes — `kVK_ISO_Section` (0x0A) and
/// `kVK_ANSI_Grave` (0x32) — to `KeyCode::Backquote`, and never emits
/// `IntlBackslash` on macOS. On an ISO keyboard that folds the 102nd key (right
/// of left Shift; `\|` on a UK PC layout) into the backquote scancode, so it
/// types ` on the server instead of \.
///
/// The unmodified character still tells the two keys apart: winit derives
/// `key_without_modifiers` from the *raw* native keycode via UCKeyTranslate
/// before the collapse, so the top-left key and the 102nd key carry distinct
/// characters even though their `KeyCode` is the same.
#[cfg(target_os = "macos")]
fn macos_backquote_scancode(event: &KeyEvent) -> Scancode {
    use winit::keyboard::Key;
    use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

    let unmodified = match event.key_without_modifiers() {
        Key::Character(s) => s.chars().next(),
        Key::Dead(c) => c,
        _ => None,
    };
    backquote_scancode(unmodified, keyboard_is_iso())
}

/// Classify a key macOS reported as `Backquote`: top-left of the main block
/// (scancode 0x29) or the ISO 102nd key next to left Shift (0x56).
///
/// Layout survey of what the two keys type unmodified:
///
/// | layout               | top-left | 102nd |
/// |----------------------|----------|-------|
/// | British, US (on ISO) | §        | `     |
/// | British – PC         | `        | \     |
/// | German, Nordic       | ^ / §    | <     |
///
/// `` ` `` therefore reads as the 102nd key on ISO hardware: that is where
/// Apple's own layouts put it. The one loser is a PC-emulating layout's
/// top-left backtick, which arrives as \ — accepted until Mac-faithful Unicode
/// input exists. ANSI and JIS keyboards have no 102nd key, so everything stays
/// at 0x29 there.
// Compiled on every platform so the classifier stays unit-tested and the
// Windows type-check guards it against drift; only macOS calls it at runtime.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn backquote_scancode(unmodified: Option<char>, iso_keyboard: bool) -> Scancode {
    if !iso_keyboard {
        return Scancode::plain(0x29);
    }
    match unmodified {
        Some('\\' | '|' | '<' | '>' | '`' | '~') => Scancode::plain(0x56),
        _ => Scancode::plain(0x29),
    }
}

/// Whether the physical keyboard is ISO (has the 102nd key next to left Shift).
///
/// Asked fresh on every corner-key press — two cheap Carbon calls — so swapping
/// keyboards mid-session is picked up. Both functions are the same ones winit
/// itself links from Carbon for its key translation.
#[cfg(target_os = "macos")]
fn keyboard_is_iso() -> bool {
    // kKeyboardISO, a four-char code (HIToolbox Events.h).
    const K_KEYBOARD_ISO: u32 = u32::from_be_bytes(*b"ISO ");

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn LMGetKbdType() -> u8;
        fn KBGetLayoutType(kbd_type: i16) -> u32;
    }
    unsafe { KBGetLayoutType(i16::from(LMGetKbdType())) == K_KEYBOARD_ISO }
}

/// Translate a winit mouse button. `at` is the session pixel the pointer is on.
pub fn from_mouse_button(
    button: winit::event::MouseButton,
    state: ElementState,
    at: (u16, u16),
) -> Option<InputEvent> {
    let button = match button {
        winit::event::MouseButton::Left => MouseButton::Left,
        winit::event::MouseButton::Right => MouseButton::Right,
        winit::event::MouseButton::Middle => MouseButton::Middle,
        winit::event::MouseButton::Back => MouseButton::X1,
        winit::event::MouseButton::Forward => MouseButton::X2,
        winit::event::MouseButton::Other(_) => return None,
    };
    Some(InputEvent::MouseButton {
        button,
        down: state == ElementState::Pressed,
        x: at.0,
        y: at.1,
    })
}

/// Translate a cursor position through the presenter's mapping.
pub fn from_cursor_moved<M: PointerMap + ?Sized>(x: f64, y: f64, map: &M) -> Option<InputEvent> {
    let (x, y) = map.to_session(x, y)?;
    Some(InputEvent::MouseMove { x, y })
}

/// The keys currently held down on the server, so focus loss can release them.
///
/// The stuck-modifier bug this exists for: press Ctrl, then hit an OS chord that steals
/// focus — Cmd+Tab, Ctrl+Arrow switching Spaces, or a click on another app. The Ctrl
/// *down* was already forwarded; the *up* is delivered to whatever has focus afterwards,
/// never to us (macOS sends no key event at all for it — winit's `windowDidResignKey`
/// documents the case). The server then holds Ctrl until the same physical key is
/// pressed and released inside the session again, which the user experiences as a stuck
/// modifier. mstsc and Windows App release every held key when their window deactivates;
/// the ledger is what lets us do the same.
///
/// It also settles what to do with winit's *synthetic* key events (Windows replays key
/// state across focus changes): a synthetic press re-asserts keys that went down while
/// we were unfocused — forwarding it would type into the session — but a synthetic
/// release of a key *we* forwarded down is the only notification that the key came up
/// while unfocused, and dropping it is exactly the stuck-key bug. Filtering releases by
/// "is the key actually held" answers both without caring which platform sent what.
#[derive(Debug, Default)]
pub struct KeyLedger {
    held: std::collections::HashSet<Scancode>,
}

impl KeyLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decide whether a key event should be forwarded, updating the ledger.
    ///
    /// Presses are forwarded unless synthetic — auto-repeat included, since RDP expects
    /// the client to send repeats. A release is forwarded only if the key is actually
    /// held, synthetic or not: a release for a key the server never saw go down is wire
    /// noise (the local hotkeys claim presses, so their releases can otherwise leak
    /// through unpaired).
    pub fn on_key(&mut self, scancode: Scancode, down: bool, synthetic: bool) -> bool {
        if down {
            if synthetic {
                return false;
            }
            self.held.insert(scancode);
            true
        } else {
            self.held.remove(&scancode)
        }
    }

    /// Release everything held. Call on focus loss and send each returned event.
    pub fn release_all(&mut self) -> Vec<InputEvent> {
        self.held
            .drain()
            .map(|scancode| InputEvent::Key {
                scancode,
                down: false,
            })
            .collect()
    }
}

/// Sub-notch scroll travel, carried across events so a slow gesture still reaches the
/// remote.
///
/// RDP counts rotation in notches of [`WHEEL_UNITS_PER_NOTCH`], and Windows applications
/// overwhelmingly divide the arriving rotation by that constant — so anything under one
/// notch scrolls nothing at all. macOS hands us far finer input than that, and the two
/// shapes it arrives in need different treatment:
///
/// - A **detented wheel** reports whole physical clicks, but macOS's acceleration curve
///   shrinks a slowly-turned click to a *fraction* of a line (~0.1). On a native Windows
///   box every click scrolls, however slowly the wheel turns, so each [`LineDelta`]
///   event is floored at one whole notch ([`detent_floor`]). Accumulating those
///   fractions instead — the first version of this fix — left slow scrolling ten
///   physical clicks per remote notch, which reads as dead.
/// - A **precise device** (trackpad, Magic Mouse) reports a continuous pixel stream
///   with no physical click to honour. Its travel accumulates, and whole notches are
///   spent as they are earned: slow scrolling still moves, fast scrolling stays
///   proportional, and sub-notch jitter never reaches the wire.
///
/// [`LineDelta`]: MouseScrollDelta::LineDelta
#[derive(Debug, Default, Clone, Copy)]
pub struct WheelAccumulator {
    vertical: f64,
    horizontal: f64,
}

impl WheelAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Translate a scroll into whole wheel notches, one PDU each — the shape a detented
    /// wheel produces, and the only shape the remote reliably acts on.
    ///
    /// A diagonal trackpad gesture legitimately yields events on both axes. Axes with no
    /// whole notch to spend yet are dropped: a wheel PDU carrying no rotation is pure
    /// wire noise, and trackpads produce a great deal of it.
    pub fn translate(&mut self, delta: MouseScrollDelta, at: (u16, u16)) -> Vec<InputEvent> {
        let (horizontal, vertical) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (
                detent_floor(f64::from(x)) * f64::from(WHEEL_UNITS_PER_NOTCH),
                detent_floor(f64::from(y)) * f64::from(WHEEL_UNITS_PER_NOTCH),
            ),
            MouseScrollDelta::PixelDelta(pos) => (
                pos.x / PIXELS_PER_NOTCH * f64::from(WHEEL_UNITS_PER_NOTCH),
                pos.y / PIXELS_PER_NOTCH * f64::from(WHEEL_UNITS_PER_NOTCH),
            ),
        };

        let mut out = Vec::new();
        for (axis, units, held) in [
            (ScrollAxis::Vertical, vertical, &mut self.vertical),
            (ScrollAxis::Horizontal, horizontal, &mut self.horizontal),
        ] {
            spend(axis, units, held, at, &mut out);
        }
        out
    }
}

/// Floor one axis of a discrete wheel event at a whole line, keeping its direction.
///
/// A [`MouseScrollDelta::LineDelta`] event is a physical detent click — winit only emits
/// it for non-precise devices — but macOS's scroll acceleration reports a slowly-turned
/// click as a fraction of a line. The click happened; the user is owed a notch for it.
/// Values of a line or more pass through untouched, so a fast spin keeps the
/// acceleration curve and its fractional part still accumulates.
///
/// The trade-off, accepted deliberately: a driver that splits one detent into several
/// sub-line events (some high-resolution wheels) will over-scroll here. That shape has
/// not been seen from macOS, and a dead slow scroll is the worse failure.
fn detent_floor(lines: f64) -> f64 {
    if lines != 0.0 && lines.abs() < 1.0 {
        lines.signum()
    } else {
        lines
    }
}

/// Add one axis of travel to its held remainder and emit whatever whole notches that buys.
fn spend(axis: ScrollAxis, units: f64, held: &mut f64, at: (u16, u16), out: &mut Vec<InputEvent>) {
    if !units.is_finite() || units == 0.0 {
        return;
    }
    // Travel held from the other direction is stale the moment the user reverses:
    // spending it would eat the start of the new gesture.
    if units.signum() != held.signum() {
        *held = 0.0;
    }
    *held += units;

    let per_notch = f64::from(WHEEL_UNITS_PER_NOTCH);
    let notches = (*held / per_notch).trunc().clamp(
        f64::from(-MAX_NOTCHES_PER_EVENT),
        f64::from(MAX_NOTCHES_PER_EVENT),
    ) as i32;
    if notches == 0 {
        return;
    }
    *held -= f64::from(notches) * per_notch;
    // A whole notch can only survive the subtraction when the cap bit. Drop it: banking
    // it would keep the remote scrolling after the gesture ended.
    if held.abs() >= per_notch {
        *held = 0.0;
    }

    let step = clamp_wheel_units(per_notch * f64::from(notches.signum()));
    for _ in 0..notches.abs() {
        out.push(InputEvent::Scroll {
            axis,
            units: step,
            x: at.0,
            y: at.1,
        });
    }
}

/// Round and clamp wheel rotation into the 9-bit two's-complement wire range.
///
/// Clamping is not cosmetic: `MousePdu::encode` debug-asserts this range, so a rotation
/// past it panics a debug build and silently truncates a release one. Every
/// [`InputEvent::Scroll`] is built through here so no path can produce one.
pub fn clamp_wheel_units(units: f64) -> i16 {
    if !units.is_finite() {
        return 0;
    }
    let rounded = units.round();
    let clamped = rounded
        .max(WHEEL_UNITS_MIN as f64)
        .min(WHEEL_UNITS_MAX as f64);
    clamped as i16
}

// --- stage 2: InputEvent -> fast-path PDU ----------------------------------------

/// Convert to the wire event. Total: every [`InputEvent`] has a fast-path form.
pub fn to_fastpath(event: InputEvent) -> FastPathInputEvent {
    match event {
        InputEvent::Key { scancode, down } => {
            let mut flags = KeyboardFlags::empty();
            if !down {
                flags |= KeyboardFlags::RELEASE;
            }
            if scancode.extended {
                flags |= KeyboardFlags::EXTENDED;
            }
            FastPathInputEvent::KeyboardEvent(flags, scancode.code)
        }
        InputEvent::MouseMove { x, y } => FastPathInputEvent::MouseEvent(MousePdu {
            flags: PointerFlags::MOVE,
            number_of_wheel_rotation_units: 0,
            x_position: x,
            y_position: y,
        }),
        InputEvent::MouseButton { button, down, x, y } => match button {
            MouseButton::Left | MouseButton::Right | MouseButton::Middle => {
                let mut flags = match button {
                    MouseButton::Left => PointerFlags::LEFT_BUTTON,
                    MouseButton::Right => PointerFlags::RIGHT_BUTTON,
                    _ => PointerFlags::MIDDLE_BUTTON_OR_WHEEL,
                };
                if down {
                    flags |= PointerFlags::DOWN;
                }
                FastPathInputEvent::MouseEvent(MousePdu {
                    flags,
                    number_of_wheel_rotation_units: 0,
                    x_position: x,
                    y_position: y,
                })
            }
            MouseButton::X1 | MouseButton::X2 => {
                let mut flags = if button == MouseButton::X1 {
                    PointerXFlags::BUTTON1
                } else {
                    PointerXFlags::BUTTON2
                };
                if down {
                    flags |= PointerXFlags::DOWN;
                }
                FastPathInputEvent::MouseEventEx(MouseXPdu {
                    flags,
                    x_position: x,
                    y_position: y,
                })
            }
        },
        InputEvent::Scroll { axis, units, x, y } => {
            let flags = match axis {
                ScrollAxis::Vertical => PointerFlags::VERTICAL_WHEEL,
                ScrollAxis::Horizontal => PointerFlags::HORIZONTAL_WHEEL,
            };
            FastPathInputEvent::MouseEvent(MousePdu {
                flags,
                number_of_wheel_rotation_units: units,
                x_position: x,
                y_position: y,
            })
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputError {
    /// Fast-path carries an 8-bit event count, so a batch is 1..=255 events.
    BadBatchSize(usize),
    /// Encoding failed. The message never carries event payload — see the module note.
    Encode(String),
}

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InputError::BadBatchSize(n) => {
                write!(f, "fast-path batch must hold 1..=255 events, got {n}")
            }
            InputError::Encode(msg) => write!(f, "fast-path input encode failed: {msg}"),
        }
    }
}

impl std::error::Error for InputError {}

/// Encode a batch of fast-path events into the bytes to write to the session stream.
///
/// Batching is what keeps a fast drag from costing one TCP write per pixel, so the
/// caller is expected to drain its channel and hand over what it found.
pub fn encode_fastpath_input(events: Vec<FastPathInputEvent>) -> Result<Vec<u8>, InputError> {
    let count = events.len();
    let pdu = FastPathInput::new(events).map_err(|_| InputError::BadBatchSize(count))?;
    encode_vec(&pdu).map_err(|e| InputError::Encode(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_thousand_window_moves_collapse_to_the_latest_position() {
        let moves = LatestMouseMove::default();
        for x in 0..10_000u16 {
            assert!(moves.replace(x, x + 1));
        }

        let mut emitted = Vec::new();
        while let Some(event) = moves.take() {
            emitted.push(event);
        }
        assert_eq!(
            emitted,
            [InputEvent::MouseMove {
                x: 9_999,
                y: 10_000
            }]
        );
    }

    #[test]
    fn a_closed_receiver_refuses_more_window_motion_on_either_transport() {
        let moves = LatestMouseMove::default();
        assert!(moves.replace(1, 2));
        moves.close();

        assert!(!moves.replace(3, 4));
        assert_eq!(moves.take(), None);
    }

    use winit::event::MouseButton as WButton;

    /// The ISO corner-key classifier: winit macOS folds both `kVK_ISO_Section` and
    /// `kVK_ANSI_Grave` into `Backquote`, so the unmodified character is the only
    /// signal separating the top-left key (0x29) from the 102nd key (0x56). A wrong
    /// verdict types ` where the user meant \ — the exact bug this classifier fixes.
    #[test]
    fn iso_corner_keys_classify_by_unmodified_character() {
        // 102nd key next to left Shift, per layout: Apple British / US-on-ISO (` ~),
        // British – PC (\ |), German and Nordic (< >).
        for ch in ['`', '~', '\\', '|', '<', '>'] {
            assert_eq!(
                backquote_scancode(Some(ch), true),
                Scancode::plain(0x56),
                "{ch:?} on ISO must be the 102nd key"
            );
        }
        // Top-left section key: British (§ ±), German (^), French (@), and the
        // no-character fallbacks.
        for ch in ['§', '±', '^', '@'] {
            assert_eq!(
                backquote_scancode(Some(ch), true),
                Scancode::plain(0x29),
                "{ch:?} on ISO must stay the backquote position"
            );
        }
        assert_eq!(backquote_scancode(None, true), Scancode::plain(0x29));
    }

    /// ANSI and JIS keyboards have no 102nd key: everything stays at 0x29, even the
    /// backtick a US layout puts top-left. Regressing this would make every US-Mac
    /// user's ` key type \.
    #[test]
    fn non_iso_keyboards_never_produce_the_102nd_key() {
        for ch in ['`', '~', '\\', '|', '<', '>', '§'] {
            assert_eq!(backquote_scancode(Some(ch), false), Scancode::plain(0x29));
        }
        assert_eq!(backquote_scancode(None, false), Scancode::plain(0x29));
    }

    /// Every winit button maps to the right RDP button.
    ///
    /// Untested until now, and a swapped Left/Right survived mutation — which would ship
    /// a client where right-click opens nothing and left-click opens context menus.
    #[test]
    fn every_mouse_button_maps_to_its_own_rdp_button() {
        let cases = [
            (WButton::Left, MouseButton::Left),
            (WButton::Right, MouseButton::Right),
            (WButton::Middle, MouseButton::Middle),
            (WButton::Back, MouseButton::X1),
            (WButton::Forward, MouseButton::X2),
        ];
        for (winit_button, expected) in cases {
            let event = from_mouse_button(winit_button, ElementState::Pressed, (7, 9))
                .unwrap_or_else(|| panic!("{winit_button:?} should map"));
            match event {
                InputEvent::MouseButton { button, x, y, .. } => {
                    assert_eq!(button, expected, "{winit_button:?} mapped to {button:?}");
                    assert_eq!((x, y), (7, 9), "coordinates must pass through");
                }
                other => panic!("expected MouseButton, got {other:?}"),
            }
        }
    }

    #[test]
    fn press_and_release_are_not_confused() {
        // An inverted `down` makes every click a release: the desktop looks alive but
        // nothing can be clicked. Cheap to get wrong, invisible in a smoke test.
        let pressed = from_mouse_button(WButton::Left, ElementState::Pressed, (0, 0)).unwrap();
        let released = from_mouse_button(WButton::Left, ElementState::Released, (0, 0)).unwrap();
        match (pressed, released) {
            (
                InputEvent::MouseButton { down: true, .. },
                InputEvent::MouseButton { down: false, .. },
            ) => {}
            other => panic!("press/release inverted or malformed: {other:?}"),
        }
    }

    #[test]
    fn an_unnamed_button_is_dropped_rather_than_guessed() {
        assert!(from_mouse_button(WButton::Other(9), ElementState::Pressed, (0, 0)).is_none());
    }
    use ironrdp::core::decode;
    use winit::dpi::PhysicalPosition;

    /// Round-trip a single event through the real wire encoder and decoder.
    ///
    /// This is the oracle: it proves our flags survive `ironrdp`'s bit packing, rather
    /// than merely proving our struct equals itself.
    fn round_trip(event: InputEvent) -> FastPathInputEvent {
        let wire = encode_fastpath_input(vec![to_fastpath(event)]).expect("encode");
        let decoded: FastPathInput = decode(&wire).expect("decode");
        assert_eq!(decoded.input_events().len(), 1);
        decoded.input_events()[0]
    }

    // --- scancodes ---------------------------------------------------------------

    #[test]
    fn common_keys_carry_their_ps2_set1_codes() {
        // Spot values checked against the Set 1 make-code table; if the match arms are
        // shuffled or a digit is off by one, these fail.
        assert_eq!(scancode_for(KeyCode::Escape), Some(Scancode::plain(0x01)));
        assert_eq!(scancode_for(KeyCode::Digit1), Some(Scancode::plain(0x02)));
        assert_eq!(scancode_for(KeyCode::KeyQ), Some(Scancode::plain(0x10)));
        assert_eq!(scancode_for(KeyCode::KeyA), Some(Scancode::plain(0x1E)));
        assert_eq!(scancode_for(KeyCode::KeyZ), Some(Scancode::plain(0x2C)));
        assert_eq!(scancode_for(KeyCode::Enter), Some(Scancode::plain(0x1C)));
        assert_eq!(scancode_for(KeyCode::Space), Some(Scancode::plain(0x39)));
        assert_eq!(scancode_for(KeyCode::Tab), Some(Scancode::plain(0x0F)));
        assert_eq!(
            scancode_for(KeyCode::Backspace),
            Some(Scancode::plain(0x0E))
        );
        assert_eq!(scancode_for(KeyCode::Minus), Some(Scancode::plain(0x0C)));
        assert_eq!(scancode_for(KeyCode::Slash), Some(Scancode::plain(0x35)));
        assert_eq!(scancode_for(KeyCode::F1), Some(Scancode::plain(0x3B)));
        assert_eq!(scancode_for(KeyCode::F12), Some(Scancode::plain(0x58)));
    }

    #[test]
    fn macos_command_maps_to_the_windows_key() {
        // winit reports macOS Command as Super. The Windows key is E0 5B / E0 5C.
        assert_eq!(
            scancode_for(KeyCode::SuperLeft),
            Some(Scancode::extended(0x5B))
        );
        assert_eq!(
            scancode_for(KeyCode::SuperRight),
            Some(Scancode::extended(0x5C))
        );
    }

    #[test]
    fn the_navigation_cluster_is_extended_and_the_numpad_twins_are_not() {
        // The classic mix-up: Home and Numpad7 share code 0x47, and only Home is
        // E0-prefixed. Getting this wrong turns arrow keys into numpad digits.
        assert_eq!(scancode_for(KeyCode::Home), Some(Scancode::extended(0x47)));
        assert_eq!(scancode_for(KeyCode::Numpad7), Some(Scancode::plain(0x47)));
        assert_eq!(
            scancode_for(KeyCode::ArrowUp),
            Some(Scancode::extended(0x48))
        );
        assert_eq!(scancode_for(KeyCode::Numpad8), Some(Scancode::plain(0x48)));
        assert_eq!(
            scancode_for(KeyCode::NumpadEnter),
            Some(Scancode::extended(0x1C))
        );
        assert_eq!(scancode_for(KeyCode::Enter), Some(Scancode::plain(0x1C)));
        assert_eq!(
            scancode_for(KeyCode::ControlRight),
            Some(Scancode::extended(0x1D))
        );
        assert_eq!(
            scancode_for(KeyCode::ControlLeft),
            Some(Scancode::plain(0x1D))
        );
    }

    #[test]
    fn every_letter_and_digit_is_mapped() {
        let letters = [
            KeyCode::KeyA,
            KeyCode::KeyB,
            KeyCode::KeyC,
            KeyCode::KeyD,
            KeyCode::KeyE,
            KeyCode::KeyF,
            KeyCode::KeyG,
            KeyCode::KeyH,
            KeyCode::KeyI,
            KeyCode::KeyJ,
            KeyCode::KeyK,
            KeyCode::KeyL,
            KeyCode::KeyM,
            KeyCode::KeyN,
            KeyCode::KeyO,
            KeyCode::KeyP,
            KeyCode::KeyQ,
            KeyCode::KeyR,
            KeyCode::KeyS,
            KeyCode::KeyT,
            KeyCode::KeyU,
            KeyCode::KeyV,
            KeyCode::KeyW,
            KeyCode::KeyX,
            KeyCode::KeyY,
            KeyCode::KeyZ,
        ];
        for key in letters {
            assert!(scancode_for(key).is_some(), "{key:?} must be mapped");
        }
        let digits = [
            KeyCode::Digit0,
            KeyCode::Digit1,
            KeyCode::Digit2,
            KeyCode::Digit3,
            KeyCode::Digit4,
            KeyCode::Digit5,
            KeyCode::Digit6,
            KeyCode::Digit7,
            KeyCode::Digit8,
            KeyCode::Digit9,
        ];
        for key in digits {
            assert!(scancode_for(key).is_some(), "{key:?} must be mapped");
        }
    }

    /// Every key this client claims to support, so the collision check below sees the
    /// whole table rather than a sample of it.
    fn all_mapped_keys() -> Vec<KeyCode> {
        use KeyCode as K;
        vec![
            K::Backquote,
            K::Digit1,
            K::Digit2,
            K::Digit3,
            K::Digit4,
            K::Digit5,
            K::Digit6,
            K::Digit7,
            K::Digit8,
            K::Digit9,
            K::Digit0,
            K::Minus,
            K::Equal,
            K::Backspace,
            K::Tab,
            K::KeyQ,
            K::KeyW,
            K::KeyE,
            K::KeyR,
            K::KeyT,
            K::KeyY,
            K::KeyU,
            K::KeyI,
            K::KeyO,
            K::KeyP,
            K::BracketLeft,
            K::BracketRight,
            K::Backslash,
            K::CapsLock,
            K::KeyA,
            K::KeyS,
            K::KeyD,
            K::KeyF,
            K::KeyG,
            K::KeyH,
            K::KeyJ,
            K::KeyK,
            K::KeyL,
            K::Semicolon,
            K::Quote,
            K::Enter,
            K::ShiftLeft,
            K::IntlBackslash,
            K::KeyZ,
            K::KeyX,
            K::KeyC,
            K::KeyV,
            K::KeyB,
            K::KeyN,
            K::KeyM,
            K::Comma,
            K::Period,
            K::Slash,
            K::ShiftRight,
            K::ControlLeft,
            K::SuperLeft,
            K::AltLeft,
            K::Space,
            K::AltRight,
            K::SuperRight,
            K::ContextMenu,
            K::ControlRight,
            K::Escape,
            K::F1,
            K::F2,
            K::F3,
            K::F4,
            K::F5,
            K::F6,
            K::F7,
            K::F8,
            K::F9,
            K::F10,
            K::F11,
            K::F12,
            K::Insert,
            K::Delete,
            K::Home,
            K::End,
            K::PageUp,
            K::PageDown,
            K::ArrowUp,
            K::ArrowDown,
            K::ArrowLeft,
            K::ArrowRight,
            K::NumLock,
            K::ScrollLock,
            K::NumpadDivide,
            K::NumpadMultiply,
            K::NumpadSubtract,
            K::NumpadAdd,
            K::NumpadEnter,
            K::NumpadDecimal,
            K::Numpad0,
            K::Numpad1,
            K::Numpad2,
            K::Numpad3,
            K::Numpad4,
            K::Numpad5,
            K::Numpad6,
            K::Numpad7,
            K::Numpad8,
            K::Numpad9,
            K::IntlRo,
            K::IntlYen,
            K::Convert,
            K::NonConvert,
            K::KanaMode,
        ]
    }

    #[test]
    fn no_two_keys_share_a_scancode() {
        // The oracle for a hand-written table: a duplicated arm (the copy-paste bug this
        // kind of code always has) makes two distinct keys produce identical wire bytes,
        // which no spot-check would notice.
        use std::collections::HashMap;
        let mut seen: HashMap<Scancode, KeyCode> = HashMap::new();
        for key in all_mapped_keys() {
            let sc = scancode_for(key).unwrap_or_else(|| panic!("{key:?} is in the table"));
            if let Some(other) = seen.insert(sc, key) {
                panic!("{key:?} and {other:?} both map to {sc:?}");
            }
        }
    }

    #[test]
    fn exotic_keys_are_unmapped_rather_than_wrong() {
        // Multi-byte sequences and media keys have no one-byte fast-path form. Silence
        // beats a plausible-looking wrong code.
        assert_eq!(scancode_for(KeyCode::PrintScreen), None);
        assert_eq!(scancode_for(KeyCode::Pause), None);
        assert_eq!(scancode_for(KeyCode::F13), None);
        assert_eq!(scancode_for(KeyCode::AudioVolumeUp), None);
    }

    // --- keys on the wire --------------------------------------------------------

    #[test]
    fn a_key_press_and_release_differ_only_by_the_release_flag() {
        let down = round_trip(InputEvent::Key {
            scancode: Scancode::plain(0x1E),
            down: true,
        });
        let up = round_trip(InputEvent::Key {
            scancode: Scancode::plain(0x1E),
            down: false,
        });
        assert_eq!(
            down,
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x1E)
        );
        assert_eq!(
            up,
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x1E)
        );
    }

    #[test]
    fn an_extended_key_sets_the_extended_flag_on_the_wire() {
        let ev = round_trip(InputEvent::Key {
            scancode: Scancode::extended(0x5B),
            down: true,
        });
        assert_eq!(
            ev,
            FastPathInputEvent::KeyboardEvent(KeyboardFlags::EXTENDED, 0x5B)
        );

        let ev = round_trip(InputEvent::Key {
            scancode: Scancode::extended(0x5B),
            down: false,
        });
        assert_eq!(
            ev,
            FastPathInputEvent::KeyboardEvent(
                KeyboardFlags::EXTENDED | KeyboardFlags::RELEASE,
                0x5B
            )
        );
    }

    // --- mouse on the wire -------------------------------------------------------

    #[test]
    fn a_mouse_move_carries_move_and_the_session_coordinates() {
        let ev = round_trip(InputEvent::MouseMove { x: 1234, y: 567 });
        let FastPathInputEvent::MouseEvent(pdu) = ev else {
            panic!("expected a mouse event");
        };
        assert!(pdu.flags.contains(PointerFlags::MOVE));
        assert_eq!((pdu.x_position, pdu.y_position), (1234, 567));
        assert_eq!(pdu.number_of_wheel_rotation_units, 0);
    }

    #[test]
    fn button_down_sets_down_and_button_up_clears_it() {
        for (button, flag) in [
            (MouseButton::Left, PointerFlags::LEFT_BUTTON),
            (MouseButton::Right, PointerFlags::RIGHT_BUTTON),
            (MouseButton::Middle, PointerFlags::MIDDLE_BUTTON_OR_WHEEL),
        ] {
            let down = round_trip(InputEvent::MouseButton {
                button,
                down: true,
                x: 10,
                y: 20,
            });
            let FastPathInputEvent::MouseEvent(pdu) = down else {
                panic!("expected a mouse event");
            };
            assert!(pdu.flags.contains(flag), "{button:?} flag");
            assert!(pdu.flags.contains(PointerFlags::DOWN), "{button:?} down");

            let up = round_trip(InputEvent::MouseButton {
                button,
                down: false,
                x: 10,
                y: 20,
            });
            let FastPathInputEvent::MouseEvent(pdu) = up else {
                panic!("expected a mouse event");
            };
            assert!(pdu.flags.contains(flag), "{button:?} flag on release");
            assert!(!pdu.flags.contains(PointerFlags::DOWN), "{button:?} up");
        }
    }

    #[test]
    fn the_extra_buttons_ride_the_extended_mouse_pdu() {
        let ev = round_trip(InputEvent::MouseButton {
            button: MouseButton::X1,
            down: true,
            x: 3,
            y: 4,
        });
        let FastPathInputEvent::MouseEventEx(pdu) = ev else {
            panic!("X buttons must use MouseXPdu, not MousePdu");
        };
        assert!(pdu.flags.contains(PointerXFlags::BUTTON1));
        assert!(pdu.flags.contains(PointerXFlags::DOWN));

        let ev = round_trip(InputEvent::MouseButton {
            button: MouseButton::X2,
            down: false,
            x: 3,
            y: 4,
        });
        let FastPathInputEvent::MouseEventEx(pdu) = ev else {
            panic!("X buttons must use MouseXPdu");
        };
        assert!(pdu.flags.contains(PointerXFlags::BUTTON2));
        assert!(!pdu.flags.contains(PointerXFlags::DOWN));
    }

    // --- scrolling ----------------------------------------------------------------

    /// A vertical scroll event of `units`, at the origin unless a test says otherwise.
    fn notch(units: i16) -> InputEvent {
        InputEvent::Scroll {
            axis: ScrollAxis::Vertical,
            units,
            x: 0,
            y: 0,
        }
    }

    #[test]
    fn one_wheel_line_is_one_notch_in_each_direction() {
        let mut wheel = WheelAccumulator::new();
        let up = wheel.translate(MouseScrollDelta::LineDelta(0.0, 1.0), (7, 8));
        assert_eq!(
            up,
            vec![InputEvent::Scroll {
                axis: ScrollAxis::Vertical,
                units: 120,
                x: 7,
                y: 8,
            }]
        );

        let down = wheel.translate(MouseScrollDelta::LineDelta(0.0, -1.0), (7, 8));
        assert_eq!(
            down,
            vec![InputEvent::Scroll {
                axis: ScrollAxis::Vertical,
                units: -120,
                x: 7,
                y: 8,
            }]
        );
    }

    #[test]
    fn a_negative_notch_survives_the_nine_bit_wire_field() {
        // The wire packs rotation as 9-bit two's complement inside the flags word. A
        // sign-magnitude reading of -120 comes back as a wildly different number.
        let ev = round_trip(InputEvent::Scroll {
            axis: ScrollAxis::Vertical,
            units: -120,
            x: 0,
            y: 0,
        });
        let FastPathInputEvent::MouseEvent(pdu) = ev else {
            panic!("expected a mouse event");
        };
        assert!(pdu.flags.contains(PointerFlags::VERTICAL_WHEEL));
        assert_eq!(pdu.number_of_wheel_rotation_units, -120);
    }

    #[test]
    fn a_horizontal_scroll_uses_the_horizontal_wheel_flag() {
        let ev = round_trip(InputEvent::Scroll {
            axis: ScrollAxis::Horizontal,
            units: 120,
            x: 0,
            y: 0,
        });
        let FastPathInputEvent::MouseEvent(pdu) = ev else {
            panic!("expected a mouse event");
        };
        assert!(pdu.flags.contains(PointerFlags::HORIZONTAL_WHEEL));
        assert!(!pdu.flags.contains(PointerFlags::VERTICAL_WHEEL));
    }

    #[test]
    fn a_multi_line_event_spends_every_notch_rather_than_clamping_them_away() {
        // Three lines is 360 units, which does not fit the 9-bit wire field. Sent as one
        // PDU it clamps to 255 — the user asked for three notches and the remote scrolls
        // two. One PDU per notch keeps the travel and stays inside the field.
        let mut wheel = WheelAccumulator::new();
        let events = wheel.translate(MouseScrollDelta::LineDelta(0.0, 3.0), (0, 0));
        assert_eq!(events, vec![notch(120), notch(120), notch(120)]);

        assert_eq!(clamp_wheel_units(-100_000.0), -256);
        assert_eq!(clamp_wheel_units(f64::NAN), 0);
    }

    #[test]
    fn a_slow_wheel_detent_is_a_whole_notch_however_small_macos_reports_it() {
        // The bug this guards: macOS's acceleration curve reports a slowly-turned
        // detent as ~0.1 of a line. Accumulating those fractions left slow scrolling
        // ten physical clicks per remote notch — which the hand reads as dead. On a
        // native Windows box every click scrolls; every click must scroll here too.
        let mut wheel = WheelAccumulator::new();
        for _ in 0..3 {
            let per_click = wheel.translate(MouseScrollDelta::LineDelta(0.0, 0.1), (0, 0));
            assert_eq!(
                per_click,
                vec![notch(120)],
                "every physical click scrolls exactly one notch, at any speed"
            );
        }

        let down = wheel.translate(MouseScrollDelta::LineDelta(0.0, -0.1), (0, 0));
        assert_eq!(down, vec![notch(-120)]);

        let sideways = wheel.translate(MouseScrollDelta::LineDelta(-0.3, 0.0), (0, 0));
        assert_eq!(
            sideways,
            vec![InputEvent::Scroll {
                axis: ScrollAxis::Horizontal,
                units: -120,
                x: 0,
                y: 0,
            }],
            "a tilt-wheel detent gets the same floor"
        );
    }

    #[test]
    fn a_fast_wheel_spin_keeps_its_fractional_acceleration() {
        // The floor only applies below one line: past it, the accelerated value and
        // its remainder still count, so a fast spin stays proportional.
        let mut wheel = WheelAccumulator::new();
        let first = wheel.translate(MouseScrollDelta::LineDelta(0.0, 1.4), (0, 0));
        assert_eq!(first, vec![notch(120)], "1.4 lines spends one, holds 0.4");

        let second = wheel.translate(MouseScrollDelta::LineDelta(0.0, 1.4), (0, 0));
        assert_eq!(second, vec![notch(120)], "2.8 lines spends two in total");

        let third = wheel.translate(MouseScrollDelta::LineDelta(0.0, 1.4), (0, 0));
        assert_eq!(
            third,
            vec![notch(120), notch(120)],
            "4.2 lines spends four in total — the fractions were not floored away"
        );
    }

    #[test]
    fn slow_trackpad_travel_accumulates_until_it_buys_a_whole_notch() {
        // Precise devices have no detent to honour, so their pixel stream accumulates:
        // eight events of 5px make one 40px notch, and nothing sub-notch hits the wire.
        let mut wheel = WheelAccumulator::new();
        let mut sent = Vec::new();
        for _ in 0..7 {
            sent.extend(wheel.translate(
                MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 5.0)),
                (0, 0),
            ));
        }
        assert!(sent.is_empty(), "35px is not yet a notch");

        sent.extend(wheel.translate(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 5.0)),
            (0, 0),
        ));
        assert_eq!(sent, vec![notch(120)], "the eighth 5px completes the notch");
    }

    #[test]
    fn reversing_direction_drops_the_travel_held_the_other_way() {
        // Otherwise the first half of the new gesture is spent cancelling the old one,
        // and a flick down then up feels dead in the up direction.
        let mut wheel = WheelAccumulator::new();
        assert!(
            wheel
                .translate(
                    MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 36.0)),
                    (0, 0)
                )
                .is_empty()
        );
        assert!(
            wheel
                .translate(
                    MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -20.0)),
                    (0, 0)
                )
                .is_empty()
        );
        let events = wheel.translate(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -20.0)),
            (0, 0),
        );
        assert_eq!(
            events,
            vec![notch(-120)],
            "a full notch of downward travel, held from zero"
        );
    }

    #[test]
    fn a_runaway_flick_is_capped_and_not_banked_for_later() {
        let mut wheel = WheelAccumulator::new();
        let huge = wheel.translate(
            MouseScrollDelta::LineDelta(0.0, MAX_NOTCHES_PER_EVENT as f32 * 10.0),
            (0, 0),
        );
        assert_eq!(huge.len(), MAX_NOTCHES_PER_EVENT as usize);

        // The dropped remainder must not reappear on the next tiny nudge.
        let next = wheel.translate(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 4.0)),
            (0, 0),
        );
        assert!(next.is_empty(), "a backlog would keep scrolling on its own");
    }

    #[test]
    fn each_axis_holds_its_own_remainder() {
        // A shared remainder would let horizontal jitter fund a vertical notch.
        let mut wheel = WheelAccumulator::new();
        let none = wheel.translate(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(25.0, 25.0)),
            (0, 0),
        );
        assert!(none.is_empty());

        let vertical_only = wheel.translate(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 15.0)),
            (0, 0),
        );
        assert_eq!(
            vertical_only,
            vec![notch(120)],
            "only the vertical axis reached a full notch"
        );
    }

    #[test]
    fn a_diagonal_gesture_produces_one_event_per_axis_and_drops_the_still_ones() {
        let mut wheel = WheelAccumulator::new();
        let both = wheel.translate(MouseScrollDelta::LineDelta(1.0, 1.0), (0, 0));
        assert_eq!(both.len(), 2, "one event per moving axis");

        let neither = wheel.translate(MouseScrollDelta::LineDelta(0.0, 0.0), (0, 0));
        assert!(neither.is_empty(), "a zero scroll is pure wire noise");

        // A single pixel of trackpad jitter buys no notch, so it stays off the wire.
        let jitter = wheel.translate(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 0.1)),
            (0, 0),
        );
        assert!(jitter.is_empty());
    }

    #[test]
    fn a_full_notch_of_trackpad_travel_is_a_full_notch_of_rotation() {
        let mut wheel = WheelAccumulator::new();
        let ev = wheel.translate(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, PIXELS_PER_NOTCH)),
            (0, 0),
        );
        assert_eq!(
            ev,
            vec![InputEvent::Scroll {
                axis: ScrollAxis::Vertical,
                units: 120,
                x: 0,
                y: 0,
            }]
        );
    }

    // --- the key ledger ------------------------------------------------------------

    #[test]
    fn focus_loss_releases_exactly_the_keys_still_held() {
        // The stuck-Ctrl bug: Ctrl down forwarded, then Ctrl+Arrow switches Spaces and
        // the release goes to another app. release_all is the server's only way out.
        let mut keys = KeyLedger::new();
        assert!(keys.on_key(Scancode::plain(0x1D), true, false)); // Ctrl down
        assert!(keys.on_key(Scancode::extended(0x5B), true, false)); // Win down
        assert!(keys.on_key(Scancode::plain(0x1E), true, false)); // A down
        assert!(keys.on_key(Scancode::plain(0x1E), false, false)); // A up

        let mut released: Vec<Scancode> = keys
            .release_all()
            .into_iter()
            .map(|ev| match ev {
                InputEvent::Key { scancode, down } => {
                    assert!(!down, "release_all must only ever release");
                    scancode
                }
                other => panic!("release_all produced a non-key event: {other:?}"),
            })
            .collect();
        released.sort_by_key(|sc| (sc.code, sc.extended));
        assert_eq!(
            released,
            vec![Scancode::plain(0x1D), Scancode::extended(0x5B)],
            "only the keys still held go up, the already-released A does not"
        );
        assert!(
            keys.release_all().is_empty(),
            "the ledger must be empty after a release-all"
        );
    }

    #[test]
    fn a_synthetic_press_is_dropped_but_a_synthetic_release_of_a_held_key_is_not() {
        let mut keys = KeyLedger::new();
        // Focus-gain replay of a key held elsewhere: forwarding it would type into the
        // session.
        assert!(!keys.on_key(Scancode::plain(0x1D), true, true));
        // But a synthetic release of a key we sent down (Windows, WM_KILLFOCUS) is the
        // only notification the key came up; dropping it is the stuck-key bug.
        assert!(keys.on_key(Scancode::plain(0x1D), true, false));
        assert!(keys.on_key(Scancode::plain(0x1D), false, true));
        assert!(keys.release_all().is_empty(), "nothing left held");
    }

    #[test]
    fn a_release_for_a_key_never_forwarded_down_is_dropped() {
        // The local hotkeys claim presses; without this filter their releases leak to
        // the server unpaired.
        let mut keys = KeyLedger::new();
        assert!(!keys.on_key(Scancode::plain(0x1F), false, false));
    }

    #[test]
    fn auto_repeat_presses_keep_flowing() {
        // RDP expects the client to send key repeats, so a second press of a held key
        // is not filtered as a duplicate.
        let mut keys = KeyLedger::new();
        assert!(keys.on_key(Scancode::plain(0x1E), true, false));
        assert!(keys.on_key(Scancode::plain(0x1E), true, false));
    }

    #[test]
    fn left_and_right_variants_are_distinct_in_the_ledger() {
        // ControlLeft is plain 0x1D and ControlRight is E0 1D. Conflating them would
        // release the wrong key — the server distinguishes them.
        let mut keys = KeyLedger::new();
        assert!(keys.on_key(Scancode::plain(0x1D), true, false));
        assert!(
            !keys.on_key(Scancode::extended(0x1D), false, false),
            "right-Ctrl release must not discharge a held left Ctrl"
        );
        assert_eq!(
            keys.release_all(),
            vec![InputEvent::Key {
                scancode: Scancode::plain(0x1D),
                down: false,
            }]
        );
    }

    // --- coordinate mapping -------------------------------------------------------

    #[test]
    fn an_identity_map_passes_coordinates_through_and_clamps_at_the_edges() {
        let map = IdentityMap::new(800, 600);
        assert_eq!(map.to_session(0.0, 0.0), Some((0, 0)));
        assert_eq!(map.to_session(123.7, 45.2), Some((123, 45)));
        // Outside the image clamps rather than dropping, so a drag keeps tracking.
        assert_eq!(map.to_session(-5.0, -5.0), Some((0, 0)));
        assert_eq!(map.to_session(9999.0, 9999.0), Some((799, 599)));
    }

    #[test]
    fn a_zero_sized_target_has_nothing_to_map_onto() {
        assert_eq!(IdentityMap::new(0, 600).to_session(1.0, 1.0), None);
        assert_eq!(IdentityMap::new(800, 0).to_session(1.0, 1.0), None);
    }

    #[test]
    fn a_cursor_move_is_mapped_not_assumed_to_be_one_to_one() {
        /// A deliberately non-identity mapping: halve both axes.
        struct HalfMap;
        impl PointerMap for HalfMap {
            fn to_session(&self, x: f64, y: f64) -> Option<(u16, u16)> {
                Some(((x / 2.0) as u16, (y / 2.0) as u16))
            }
        }
        assert_eq!(
            from_cursor_moved(400.0, 200.0, &HalfMap),
            Some(InputEvent::MouseMove { x: 200, y: 100 })
        );
    }

    // --- batching ------------------------------------------------------------------

    #[test]
    fn an_empty_batch_is_rejected_rather_than_sent() {
        assert_eq!(
            encode_fastpath_input(Vec::new()),
            Err(InputError::BadBatchSize(0))
        );
    }

    #[test]
    fn an_oversized_batch_is_rejected_rather_than_truncated() {
        let ev = to_fastpath(InputEvent::MouseMove { x: 0, y: 0 });
        let batch = vec![ev; 256];
        assert_eq!(
            encode_fastpath_input(batch),
            Err(InputError::BadBatchSize(256))
        );
    }

    #[test]
    fn a_batch_round_trips_with_its_events_in_order() {
        // Order matters: a click is down-then-up, and a reordered batch is a different
        // gesture entirely.
        let events = vec![
            to_fastpath(InputEvent::MouseMove { x: 5, y: 6 }),
            to_fastpath(InputEvent::MouseButton {
                button: MouseButton::Left,
                down: true,
                x: 5,
                y: 6,
            }),
            to_fastpath(InputEvent::MouseButton {
                button: MouseButton::Left,
                down: false,
                x: 5,
                y: 6,
            }),
        ];
        let wire = encode_fastpath_input(events.clone()).expect("encode");
        let decoded: FastPathInput = decode(&wire).expect("decode");
        assert_eq!(decoded.input_events(), events.as_slice());
    }
}
