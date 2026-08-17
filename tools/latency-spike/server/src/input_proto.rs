//! The keystroke channel record.
//!
//! Fixed 8 bytes, no framing of its own — the record *is* the frame:
//!
//! ```text
//! [u8 kind][u8 reserved][u16 le vk][u32 le seq]
//! ```
//!
//! Fixed width is deliberate. The whole point of this channel is to time an
//! injection, so it must not spend a byte on anything the reader has to parse.

pub const RECORD_LEN: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    Down = 1,
    Up = 2,
}

impl KeyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyKind::Down => "down",
            KeyKind::Up => "up",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputRecord {
    pub kind: KeyKind,
    pub vk: u16,
    pub seq: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputProtoError {
    /// `kind` was neither 1 nor 2.
    BadKind(u8),
    /// The reserved byte was non-zero. It is reserved so that a future field can be
    /// added without a version negotiation; a sender that fills it is out of sync.
    ReservedNotZero(u8),
    /// Virtual-key codes occupy 0x01..=0xFE. 0 and anything above 0xFE is a bug in
    /// the sender, and `SendInput` would silently do nothing with it.
    BadVirtualKey(u16),
}

impl std::fmt::Display for InputProtoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InputProtoError::BadKind(k) => write!(f, "input: kind {k} is not 1 (down) or 2 (up)"),
            InputProtoError::ReservedNotZero(b) => {
                write!(f, "input: reserved byte is {b:#04x}, expected 0")
            }
            InputProtoError::BadVirtualKey(v) => {
                write!(f, "input: virtual-key {v:#06x} outside 0x01..=0xFE")
            }
        }
    }
}

impl std::error::Error for InputProtoError {}

pub fn encode(rec: InputRecord) -> [u8; RECORD_LEN] {
    let mut out = [0u8; RECORD_LEN];
    out[0] = rec.kind as u8;
    out[1] = 0;
    out[2..4].copy_from_slice(&rec.vk.to_le_bytes());
    out[4..8].copy_from_slice(&rec.seq.to_le_bytes());
    out
}

pub fn decode(bytes: &[u8; RECORD_LEN]) -> Result<InputRecord, InputProtoError> {
    let kind = match bytes[0] {
        1 => KeyKind::Down,
        2 => KeyKind::Up,
        other => return Err(InputProtoError::BadKind(other)),
    };
    if bytes[1] != 0 {
        return Err(InputProtoError::ReservedNotZero(bytes[1]));
    }
    let vk = u16::from_le_bytes([bytes[2], bytes[3]]);
    if vk == 0 || vk > 0xFE {
        return Err(InputProtoError::BadVirtualKey(vk));
    }
    let seq = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    Ok(InputRecord { kind, vk, seq })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_is_eight_bytes_in_the_documented_order() {
        // Every field a different value: a fixture where vk and seq agree could not
        // catch the two being swapped.
        let rec = InputRecord {
            kind: KeyKind::Up,
            vk: 0x41,
            seq: 0x0A0B0C0D,
        };
        let bytes = encode(rec);
        assert_eq!(bytes, [0x02, 0x00, 0x41, 0x00, 0x0D, 0x0C, 0x0B, 0x0A]);
        assert_eq!(bytes.len(), RECORD_LEN);
    }

    #[test]
    fn encode_decode_round_trips() {
        for (kind, vk, seq) in [
            (KeyKind::Down, 1u16, 0u32),
            (KeyKind::Up, 0xFE, u32::MAX),
            (KeyKind::Down, 0x5A, 12345),
        ] {
            let rec = InputRecord { kind, vk, seq };
            assert_eq!(decode(&encode(rec)), Ok(rec));
        }
    }

    #[test]
    fn an_unknown_kind_is_refused() {
        let bytes = [0x03, 0x00, 0x41, 0x00, 0x01, 0, 0, 0];
        assert_eq!(decode(&bytes), Err(InputProtoError::BadKind(3)));
        let bytes = [0x00, 0x00, 0x41, 0x00, 0x01, 0, 0, 0];
        assert_eq!(decode(&bytes), Err(InputProtoError::BadKind(0)));
    }

    #[test]
    fn a_dirty_reserved_byte_is_refused() {
        let bytes = [0x01, 0x7F, 0x41, 0x00, 0x01, 0, 0, 0];
        assert_eq!(decode(&bytes), Err(InputProtoError::ReservedNotZero(0x7F)));
    }

    #[test]
    fn an_out_of_range_virtual_key_is_refused() {
        let zero = [0x01, 0x00, 0x00, 0x00, 0x01, 0, 0, 0];
        assert_eq!(decode(&zero), Err(InputProtoError::BadVirtualKey(0)));
        let high = [0x01, 0x00, 0xFF, 0x00, 0x01, 0, 0, 0];
        assert_eq!(decode(&high), Err(InputProtoError::BadVirtualKey(0xFF)));
        let very_high = [0x01, 0x00, 0x00, 0x01, 0x01, 0, 0, 0];
        assert_eq!(
            decode(&very_high),
            Err(InputProtoError::BadVirtualKey(0x100))
        );
    }

    #[test]
    fn the_vk_field_is_little_endian() {
        // 0x00FE little-endian is FE 00. Reading it big-endian would give 0xFE00,
        // which the range check rejects — so this test distinguishes the two.
        let bytes = [0x01, 0x00, 0xFE, 0x00, 0, 0, 0, 0];
        assert_eq!(decode(&bytes).unwrap().vk, 0xFE);
    }
}
