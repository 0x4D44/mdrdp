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
    let scancode = scancode_for(code)?;
    Some(InputEvent::Key {
        scancode,
        down: event.state == ElementState::Pressed,
    })
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

/// Translate a scroll. A diagonal trackpad gesture legitimately yields two events.
///
/// Zero-unit axes are dropped: a wheel PDU carrying no rotation is pure wire noise, and
/// trackpads produce a great deal of it.
pub fn from_mouse_wheel(delta: MouseScrollDelta, at: (u16, u16)) -> Vec<InputEvent> {
    let (horizontal, vertical) = match delta {
        MouseScrollDelta::LineDelta(x, y) => (
            f64::from(x) * f64::from(WHEEL_UNITS_PER_NOTCH),
            f64::from(y) * f64::from(WHEEL_UNITS_PER_NOTCH),
        ),
        MouseScrollDelta::PixelDelta(pos) => (
            pos.x / PIXELS_PER_NOTCH * f64::from(WHEEL_UNITS_PER_NOTCH),
            pos.y / PIXELS_PER_NOTCH * f64::from(WHEEL_UNITS_PER_NOTCH),
        ),
    };

    let mut out = Vec::new();
    for (axis, units) in [
        (ScrollAxis::Vertical, vertical),
        (ScrollAxis::Horizontal, horizontal),
    ] {
        let units = clamp_wheel_units(units);
        if units != 0 {
            out.push(InputEvent::Scroll {
                axis,
                units,
                x: at.0,
                y: at.1,
            });
        }
    }
    out
}

/// Round and clamp wheel rotation into the 9-bit two's-complement wire range.
///
/// Clamping is not cosmetic: `MousePdu::encode` debug-asserts this range, so an
/// unclamped three-notch flick panics a debug build.
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

    #[test]
    fn one_wheel_line_is_one_notch_in_each_direction() {
        let up = from_mouse_wheel(MouseScrollDelta::LineDelta(0.0, 1.0), (7, 8));
        assert_eq!(
            up,
            vec![InputEvent::Scroll {
                axis: ScrollAxis::Vertical,
                units: 120,
                x: 7,
                y: 8,
            }]
        );

        let down = from_mouse_wheel(MouseScrollDelta::LineDelta(0.0, -1.0), (7, 8));
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
    fn a_fast_flick_is_clamped_into_the_wire_range() {
        // Three notches is 360 units, which does not fit a 9-bit field. Unclamped, the
        // encoder's debug_assert fires and a release build silently sends garbage.
        let events = from_mouse_wheel(MouseScrollDelta::LineDelta(0.0, 3.0), (0, 0));
        assert_eq!(
            events,
            vec![InputEvent::Scroll {
                axis: ScrollAxis::Vertical,
                units: 255,
                x: 0,
                y: 0,
            }]
        );
        assert_eq!(clamp_wheel_units(-100_000.0), -256);
        assert_eq!(clamp_wheel_units(f64::NAN), 0);
    }

    #[test]
    fn a_diagonal_gesture_produces_one_event_per_axis_and_drops_the_still_ones() {
        let both = from_mouse_wheel(MouseScrollDelta::LineDelta(1.0, 1.0), (0, 0));
        assert_eq!(both.len(), 2, "one event per moving axis");

        let neither = from_mouse_wheel(MouseScrollDelta::LineDelta(0.0, 0.0), (0, 0));
        assert!(neither.is_empty(), "a zero scroll is pure wire noise");

        // Sub-threshold trackpad jitter rounds to zero units and must be dropped too.
        let jitter = from_mouse_wheel(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 0.1)),
            (0, 0),
        );
        assert!(jitter.is_empty());
    }

    #[test]
    fn a_full_notch_of_trackpad_travel_is_a_full_notch_of_rotation() {
        let ev = from_mouse_wheel(
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
