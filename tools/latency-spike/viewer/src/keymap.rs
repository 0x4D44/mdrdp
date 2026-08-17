//! winit physical key → Win32 virtual-key code.
//!
//! Deliberately a small explicit table, not a port of `src/input.rs`. That module maps
//! to PS/2 set-1 **scancodes**, because that is what RDP's Input PDU carries; this
//! channel carries **virtual-key codes**, because the server injects with `SendInput`
//! and `KEYBDINPUT::wVk`. The two encodings are unrelated — set-1 `0x2D` and `VK_X`
//! `0x58` are the same key under different names — so sharing a table would be a bug
//! waiting to happen rather than a saving.
//!
//! Coverage is the measurement workload plus enough to type: letters, digits, space,
//! enter, backspace, delete, escape, tab and the four arrows. Anything else returns
//! `None` and is not sent; an unmapped key must never reach the wire, because
//! `input_proto::decode` refuses a virtual key outside `0x01..=0xFE` and closes the
//! connection over it.
//!
//! Physical keys, not logical ones: the workload is "hold a key down and count the
//! round trips", and a logical key is affected by the Mac's own layout and modifier
//! state on the way in. The physical key is the button that was pressed.

use winit::keyboard::KeyCode;

/// `VK_BACK` — one half of the measurement workload, so it is named rather than inlined.
pub const VK_BACK: u16 = 0x08;
/// `VK_X` — the other half.
pub const VK_X: u16 = 0x58;

/// The Win32 virtual-key code for a physical key, if this channel carries it.
pub fn virtual_key(code: KeyCode) -> Option<u16> {
    use KeyCode as K;
    // Win32 defines no VK_A..VK_Z / VK_0..VK_9 constants: the codes are the ASCII
    // values of the uppercase letter and the digit. Hence the literals.
    let vk = match code {
        K::KeyA => 0x41,
        K::KeyB => 0x42,
        K::KeyC => 0x43,
        K::KeyD => 0x44,
        K::KeyE => 0x45,
        K::KeyF => 0x46,
        K::KeyG => 0x47,
        K::KeyH => 0x48,
        K::KeyI => 0x49,
        K::KeyJ => 0x4A,
        K::KeyK => 0x4B,
        K::KeyL => 0x4C,
        K::KeyM => 0x4D,
        K::KeyN => 0x4E,
        K::KeyO => 0x4F,
        K::KeyP => 0x50,
        K::KeyQ => 0x51,
        K::KeyR => 0x52,
        K::KeyS => 0x53,
        K::KeyT => 0x54,
        K::KeyU => 0x55,
        K::KeyV => 0x56,
        K::KeyW => 0x57,
        K::KeyX => VK_X,
        K::KeyY => 0x59,
        K::KeyZ => 0x5A,

        K::Digit0 => 0x30,
        K::Digit1 => 0x31,
        K::Digit2 => 0x32,
        K::Digit3 => 0x33,
        K::Digit4 => 0x34,
        K::Digit5 => 0x35,
        K::Digit6 => 0x36,
        K::Digit7 => 0x37,
        K::Digit8 => 0x38,
        K::Digit9 => 0x39,

        K::Space => 0x20,        // VK_SPACE
        K::Enter => 0x0D,        // VK_RETURN
        K::NumpadEnter => 0x0D,  // VK_RETURN — the same virtual key, extended on the wire
        K::Backspace => VK_BACK, // VK_BACK
        K::Delete => 0x2E,       // VK_DELETE — forward delete, NOT backspace
        K::Escape => 0x1B,       // VK_ESCAPE
        K::Tab => 0x09,          // VK_TAB
        K::ArrowLeft => 0x25,    // VK_LEFT
        K::ArrowUp => 0x26,      // VK_UP
        K::ArrowRight => 0x27,   // VK_RIGHT
        K::ArrowDown => 0x28,    // VK_DOWN
        _ => return None,
    };
    Some(vk)
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::KeyCode as K;

    /// The two keys the whole measurement rests on. `probe glass` types `x` and then
    /// erases it, so a wrong code here does not look like a mapping bug — it looks
    /// like the server never responded.
    #[test]
    fn the_workload_keys_carry_their_documented_codes() {
        assert_eq!(virtual_key(K::KeyX), Some(0x58), "VK_X");
        assert_eq!(virtual_key(K::Backspace), Some(0x08), "VK_BACK");
    }

    /// Backspace (`VK_BACK`, 0x08) and Delete (`VK_DELETE`, 0x2E) are different keys.
    /// On a Mac keyboard the key *labelled* delete is backspace, which is exactly the
    /// confusion that puts 0x2E on the wire and erases the wrong character.
    #[test]
    fn backspace_and_delete_do_not_collide() {
        assert_eq!(virtual_key(K::Backspace), Some(0x08));
        assert_eq!(virtual_key(K::Delete), Some(0x2E));
    }

    #[test]
    fn letters_and_digits_are_their_ascii_uppercase_codes() {
        assert_eq!(virtual_key(K::KeyA), Some(u16::from(b'A')));
        assert_eq!(virtual_key(K::KeyZ), Some(u16::from(b'Z')));
        assert_eq!(virtual_key(K::Digit0), Some(u16::from(b'0')));
        assert_eq!(virtual_key(K::Digit9), Some(u16::from(b'9')));
    }

    #[test]
    fn the_arrows_are_in_the_win32_order() {
        // left, up, right, down = 0x25..0x28. Any rotation of that quartet still
        // "maps four keys to four codes", so the order is asserted explicitly.
        assert_eq!(virtual_key(K::ArrowLeft), Some(0x25));
        assert_eq!(virtual_key(K::ArrowUp), Some(0x26));
        assert_eq!(virtual_key(K::ArrowRight), Some(0x27));
        assert_eq!(virtual_key(K::ArrowDown), Some(0x28));
    }

    #[test]
    fn every_mapped_key_is_in_the_range_the_server_accepts() {
        // `input_proto::decode` closes the connection on a vk outside 0x01..=0xFE, so
        // an out-of-range entry would kill the input channel on first press.
        for code in COVERED {
            let vk = virtual_key(*code).expect("covered key");
            assert!((0x01..=0xFE).contains(&vk), "{code:?} → {vk:#06x}");
        }
    }

    #[test]
    fn distinct_keys_get_distinct_codes() {
        // NumpadEnter and Enter are deliberately the same virtual key; everything else
        // sharing a code would be a copy-paste error in the table.
        let mut seen: Vec<(KeyCode, u16)> = Vec::new();
        for code in COVERED {
            let vk = virtual_key(*code).unwrap();
            if matches!(code, K::NumpadEnter) {
                continue;
            }
            if let Some((other, _)) = seen.iter().find(|(_, v)| *v == vk) {
                panic!("{code:?} and {other:?} both map to {vk:#06x}");
            }
            seen.push((*code, vk));
        }
    }

    #[test]
    fn an_uncovered_key_is_refused_rather_than_guessed() {
        assert_eq!(virtual_key(K::F13), None);
        assert_eq!(virtual_key(K::ShiftLeft), None);
        assert_eq!(virtual_key(K::AudioVolumeUp), None);
    }

    /// Every key the table claims to cover. Kept beside the tests so adding a row to
    /// the table without adding it here is visible in review.
    const COVERED: &[KeyCode] = &[
        K::KeyA,
        K::KeyB,
        K::KeyC,
        K::KeyD,
        K::KeyE,
        K::KeyF,
        K::KeyG,
        K::KeyH,
        K::KeyI,
        K::KeyJ,
        K::KeyK,
        K::KeyL,
        K::KeyM,
        K::KeyN,
        K::KeyO,
        K::KeyP,
        K::KeyQ,
        K::KeyR,
        K::KeyS,
        K::KeyT,
        K::KeyU,
        K::KeyV,
        K::KeyW,
        K::KeyX,
        K::KeyY,
        K::KeyZ,
        K::Digit0,
        K::Digit1,
        K::Digit2,
        K::Digit3,
        K::Digit4,
        K::Digit5,
        K::Digit6,
        K::Digit7,
        K::Digit8,
        K::Digit9,
        K::Space,
        K::Enter,
        K::NumpadEnter,
        K::Backspace,
        K::Delete,
        K::Escape,
        K::Tab,
        K::ArrowLeft,
        K::ArrowUp,
        K::ArrowRight,
        K::ArrowDown,
    ];
}
