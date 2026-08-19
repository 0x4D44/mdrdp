//! The input channel record.
//!
//! No framing of its own — the record *is* the frame — but no longer one fixed
//! width either. v1 (kinds 1/2, the latency-rig's VK down/up) is 8 bytes:
//!
//! ```text
//! [u8 kind][u8 reserved][u16 le vk][u32 le seq]
//! ```
//!
//! v2 (input-channel v2, HLD tranche 3 §5.1: `mdrdp <host> --native`'s full
//! keyboard + mouse dialect) adds kinds 3-7, each still fixed width *for that
//! kind* — [`kind_len`] gives the reader the length before it decodes anything —
//! with `rsvd` at byte 1 in every one of them:
//!
//! | kind | name          | layout after kind (LE)                                | total |
//! |------|---------------|--------------------------------------------------------|-------|
//! | 1/2  | VkDown/VkUp   | `u8 rsvd, u16 vk, u32 seq`                             | 8     |
//! | 3/4  | ScanDown/Up   | `u8 rsvd, u16 scancode, u32 seq`                       | 8     |
//! | 5    | MouseMove     | `u8 rsvd, u16 x, u16 y, u32 seq`                       | 10    |
//! | 6    | MouseButton   | `u8 rsvd, u8 button, u8 down, u32 seq`                 | 8     |
//! | 7    | Wheel         | `u8 rsvd, u8 axis, u8 pad, i16 delta_x120, u32 seq`    | 10    |
//!
//! Fixed width per kind is deliberate: this is a frameless stream, so an unknown
//! kind cannot be skipped — it is terminal for the connection (`BadKind`, `decode`
//! and `decode_record`) — and the reader must always know how many bytes a known
//! kind needs before it reads them ([`kind_len`]).
//!
//! `scancode` wire convention: low byte is the PC set-1 code, high byte is `0xE0`
//! when extended (0x00 otherwise) — mapped from mdrdp's `Scancode { code, extended }`,
//! which has no high byte of its own.
//!
//! Kinds 1/2 keep their exact bytes and their original [`encode`]/[`decode`]
//! functions unchanged — the latency-rig viewer and the `win` input server both
//! call those two directly and must keep compiling. The v2 kinds live behind the
//! additive [`Record`]/[`encode_record`]/[`decode_record`] API instead.

/// Total length of a v1 (kind 1/2) record, including the kind byte.
pub const RECORD_LEN: usize = 8;

/// Longest v2 record, including the kind byte (kinds 5 and 7, at 10 bytes).
pub const MAX_RECORD_LEN: usize = 10;

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
    /// `kind` was not one of the seven known record kinds.
    BadKind(u8),
    /// The reserved byte was non-zero. It is reserved so that a future field can be
    /// added without a version negotiation; a sender that fills it is out of sync.
    ReservedNotZero(u8),
    /// Virtual-key codes occupy 0x01..=0xFE. 0 and anything above 0xFE is a bug in
    /// the sender, and `SendInput` would silently do nothing with it.
    BadVirtualKey(u16),
    /// A scancode's high byte was neither 0x00 nor 0xE0 — the only two the wire
    /// convention (§5.1) defines.
    BadScancode(u16),
    /// A `MouseButton` record's button byte was outside 1..=5.
    BadButton(u8),
    /// A `MouseButton` record's down byte was neither 0 nor 1.
    BadDown(u8),
    /// A `Wheel` record's axis byte was neither 0 (vertical) nor 1 (horizontal).
    BadAxis(u8),
    /// A `Wheel` record's pad byte was non-zero.
    PadNotZero(u8),
    /// A `Wheel` record's delta was zero, or not a multiple of 120 — `mdrdp`'s
    /// `WheelAccumulator` only ever emits whole ±120 notches.
    BadDelta(i16),
    /// The slice handed to `decode_record` was shorter than the record its own
    /// kind byte says it should be (or empty, so not even the kind byte was there).
    Short { need: usize, got: usize },
}

impl std::fmt::Display for InputProtoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InputProtoError::BadKind(k) => write!(f, "input: kind {k} is not 1..=7"),
            InputProtoError::ReservedNotZero(b) => {
                write!(f, "input: reserved byte is {b:#04x}, expected 0")
            }
            InputProtoError::BadVirtualKey(v) => {
                write!(f, "input: virtual-key {v:#06x} outside 0x01..=0xFE")
            }
            InputProtoError::BadScancode(s) => write!(
                f,
                "input: scancode {s:#06x} has a high byte other than 0x00 or 0xE0"
            ),
            InputProtoError::BadButton(b) => write!(f, "input: mouse button {b} outside 1..=5"),
            InputProtoError::BadDown(d) => write!(f, "input: button `down` byte {d} is not 0 or 1"),
            InputProtoError::BadAxis(a) => {
                write!(
                    f,
                    "input: wheel axis {a} is not 0 (vertical) or 1 (horizontal)"
                )
            }
            InputProtoError::PadNotZero(p) => {
                write!(f, "input: pad byte is {p:#04x}, expected 0")
            }
            InputProtoError::BadDelta(d) => {
                write!(f, "input: wheel delta {d} is not a nonzero multiple of 120")
            }
            InputProtoError::Short { need, got } => {
                write!(f, "input: record needs {need} bytes, got {got}")
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

/// Which physical mouse button a [`Record::MouseButton`] names. Discriminants are
/// the wire values (§5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left = 1,
    Right = 2,
    Middle = 3,
    X1 = 4,
    X2 = 5,
}

/// Which axis a [`Record::Wheel`] scrolled. Discriminants are the wire values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelAxis {
    Vertical = 0,
    Horizontal = 1,
}

/// One decoded input-channel record, any of the seven kinds (§5.1). Additive
/// beside [`InputRecord`]/[`KeyKind`] — existing callers of [`encode`]/[`decode`]
/// need not change; new callers that want the full v2 dialect use this plus
/// [`encode_record`]/[`decode_record`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Record {
    VkDown {
        vk: u16,
        seq: u32,
    },
    VkUp {
        vk: u16,
        seq: u32,
    },
    ScanDown {
        scancode: u16,
        seq: u32,
    },
    ScanUp {
        scancode: u16,
        seq: u32,
    },
    MouseMove {
        x: u16,
        y: u16,
        seq: u32,
    },
    MouseButton {
        button: MouseButton,
        down: bool,
        seq: u32,
    },
    Wheel {
        axis: WheelAxis,
        delta120: i16,
        seq: u32,
    },
}

/// The total length of a record of the given kind, including the kind byte, or
/// `None` for an unknown kind. Frameless streams cannot skip an unknown length,
/// so a reader must consult this before it knows how many more bytes to pull off
/// the wire (§5.1).
pub fn kind_len(kind: u8) -> Option<usize> {
    match kind {
        1 | 2 | 3 | 4 | 6 => Some(8),
        5 | 7 => Some(10),
        _ => None,
    }
}

/// Encode any v2 record into a fixed [`MAX_RECORD_LEN`]-byte buffer, returning the
/// buffer and the number of leading bytes that are actually the record (matches
/// [`kind_len`] for that record's kind) — the trailing bytes are zeroed padding,
/// not part of the wire record, and must not be sent.
pub fn encode_record(rec: Record) -> ([u8; MAX_RECORD_LEN], usize) {
    let mut out = [0u8; MAX_RECORD_LEN];
    let len = match rec {
        Record::VkDown { vk, seq } => encode_kind_field16_seq(&mut out, 1, vk, seq),
        Record::VkUp { vk, seq } => encode_kind_field16_seq(&mut out, 2, vk, seq),
        Record::ScanDown { scancode, seq } => encode_kind_field16_seq(&mut out, 3, scancode, seq),
        Record::ScanUp { scancode, seq } => encode_kind_field16_seq(&mut out, 4, scancode, seq),
        Record::MouseMove { x, y, seq } => {
            out[0] = 5;
            out[1] = 0;
            out[2..4].copy_from_slice(&x.to_le_bytes());
            out[4..6].copy_from_slice(&y.to_le_bytes());
            out[6..10].copy_from_slice(&seq.to_le_bytes());
            10
        }
        Record::MouseButton { button, down, seq } => {
            out[0] = 6;
            out[1] = 0;
            out[2] = button as u8;
            out[3] = down as u8;
            out[4..8].copy_from_slice(&seq.to_le_bytes());
            8
        }
        Record::Wheel {
            axis,
            delta120,
            seq,
        } => {
            out[0] = 7;
            out[1] = 0;
            out[2] = axis as u8;
            out[3] = 0;
            out[4..6].copy_from_slice(&delta120.to_le_bytes());
            out[6..10].copy_from_slice(&seq.to_le_bytes());
            10
        }
    };
    (out, len)
}

/// Shared encoder for the four kinds whose body is `rsvd, u16 field, u32 seq`
/// (VkDown/Up, ScanDown/Up) — same shape, different field name and kind byte.
fn encode_kind_field16_seq(
    out: &mut [u8; MAX_RECORD_LEN],
    kind: u8,
    field: u16,
    seq: u32,
) -> usize {
    out[0] = kind;
    out[1] = 0;
    out[2..4].copy_from_slice(&field.to_le_bytes());
    out[4..8].copy_from_slice(&seq.to_le_bytes());
    8
}

/// Decode one v2 record from the front of `bytes`. `bytes` must hold at least as
/// many bytes as [`kind_len`] of its own first byte demands — trailing bytes
/// beyond that are ignored, so the caller may pass a longer read-ahead buffer.
///
/// Kinds 1/2 decode here too (the v2 dialect is a superset), but this function is
/// additive: existing callers of [`decode`] are unaffected and unchanged.
pub fn decode_record(bytes: &[u8]) -> Result<Record, InputProtoError> {
    let kind = *bytes
        .first()
        .ok_or(InputProtoError::Short { need: 1, got: 0 })?;
    let len = kind_len(kind).ok_or(InputProtoError::BadKind(kind))?;
    if bytes.len() < len {
        return Err(InputProtoError::Short {
            need: len,
            got: bytes.len(),
        });
    }
    let rsvd = bytes[1];
    if rsvd != 0 {
        return Err(InputProtoError::ReservedNotZero(rsvd));
    }
    let seq = u32::from_le_bytes(bytes[len - 4..len].try_into().unwrap());
    match kind {
        1 | 2 => {
            let vk = u16::from_le_bytes([bytes[2], bytes[3]]);
            if vk == 0 || vk > 0xFE {
                return Err(InputProtoError::BadVirtualKey(vk));
            }
            Ok(if kind == 1 {
                Record::VkDown { vk, seq }
            } else {
                Record::VkUp { vk, seq }
            })
        }
        3 | 4 => {
            let scancode = u16::from_le_bytes([bytes[2], bytes[3]]);
            let high = (scancode >> 8) as u8;
            if high != 0x00 && high != 0xE0 {
                return Err(InputProtoError::BadScancode(scancode));
            }
            Ok(if kind == 3 {
                Record::ScanDown { scancode, seq }
            } else {
                Record::ScanUp { scancode, seq }
            })
        }
        5 => {
            let x = u16::from_le_bytes([bytes[2], bytes[3]]);
            let y = u16::from_le_bytes([bytes[4], bytes[5]]);
            Ok(Record::MouseMove { x, y, seq })
        }
        6 => {
            let button = match bytes[2] {
                1 => MouseButton::Left,
                2 => MouseButton::Right,
                3 => MouseButton::Middle,
                4 => MouseButton::X1,
                5 => MouseButton::X2,
                other => return Err(InputProtoError::BadButton(other)),
            };
            let down = match bytes[3] {
                0 => false,
                1 => true,
                other => return Err(InputProtoError::BadDown(other)),
            };
            Ok(Record::MouseButton { button, down, seq })
        }
        7 => {
            let axis = match bytes[2] {
                0 => WheelAxis::Vertical,
                1 => WheelAxis::Horizontal,
                other => return Err(InputProtoError::BadAxis(other)),
            };
            let pad = bytes[3];
            if pad != 0 {
                return Err(InputProtoError::PadNotZero(pad));
            }
            let delta120 = i16::from_le_bytes([bytes[4], bytes[5]]);
            if delta120 == 0 || delta120 % 120 != 0 {
                return Err(InputProtoError::BadDelta(delta120));
            }
            Ok(Record::Wheel {
                axis,
                delta120,
                seq,
            })
        }
        _ => unreachable!("kind_len already rejected any kind not in 1..=7"),
    }
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

    // --- v2: kind_len -----------------------------------------------------

    #[test]
    fn kind_len_covers_every_documented_kind() {
        assert_eq!(kind_len(1), Some(8)); // VkDown
        assert_eq!(kind_len(2), Some(8)); // VkUp
        assert_eq!(kind_len(3), Some(8)); // ScanDown
        assert_eq!(kind_len(4), Some(8)); // ScanUp
        assert_eq!(kind_len(5), Some(10)); // MouseMove
        assert_eq!(kind_len(6), Some(8)); // MouseButton
        assert_eq!(kind_len(7), Some(10)); // Wheel
    }

    #[test]
    fn kind_len_rejects_unknown_kinds() {
        assert_eq!(kind_len(0), None);
        assert_eq!(kind_len(8), None);
        assert_eq!(kind_len(255), None);
    }

    // --- v2: round trips, every field a distinct value ---------------------

    #[test]
    fn scan_records_round_trip() {
        for rec in [
            Record::ScanDown {
                scancode: 0x001E, // 'A', set-1, non-extended
                seq: 0x1122_3344,
            },
            Record::ScanUp {
                scancode: 0xE048, // up-arrow, set-1, extended (0xE0 high byte)
                seq: 0x5566_7788,
            },
        ] {
            let (buf, len) = encode_record(rec);
            assert_eq!(decode_record(&buf[..len]), Ok(rec));
        }
    }

    #[test]
    fn scan_down_up_kind_bytes_and_layout_are_exact() {
        let (buf, len) = encode_record(Record::ScanDown {
            scancode: 0xE05B,
            seq: 7,
        });
        assert_eq!(len, 8);
        assert_eq!(&buf[..8], &[0x03, 0x00, 0x5B, 0xE0, 0x07, 0x00, 0x00, 0x00]);
        let (buf, len) = encode_record(Record::ScanUp {
            scancode: 0x001C,
            seq: 9,
        });
        assert_eq!(len, 8);
        assert_eq!(&buf[..8], &[0x04, 0x00, 0x1C, 0x00, 0x09, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn mouse_move_round_trips_with_distinct_x_y_seq() {
        let rec = Record::MouseMove {
            x: 1234,
            y: 567,
            seq: 99,
        };
        let (buf, len) = encode_record(rec);
        assert_eq!(len, 10);
        assert_eq!(decode_record(&buf[..len]), Ok(rec));
    }

    #[test]
    fn mouse_button_round_trips_every_button_and_edge() {
        for rec in [
            Record::MouseButton {
                button: MouseButton::Left,
                down: true,
                seq: 1,
            },
            Record::MouseButton {
                button: MouseButton::Right,
                down: false,
                seq: 42,
            },
            Record::MouseButton {
                button: MouseButton::Middle,
                down: true,
                seq: 1000,
            },
            Record::MouseButton {
                button: MouseButton::X1,
                down: false,
                seq: 7,
            },
            Record::MouseButton {
                button: MouseButton::X2,
                down: true,
                seq: u32::MAX,
            },
        ] {
            let (buf, len) = encode_record(rec);
            assert_eq!(len, 8);
            assert_eq!(decode_record(&buf[..len]), Ok(rec));
        }
    }

    #[test]
    fn wheel_round_trips_both_axes_and_signs() {
        for rec in [
            Record::Wheel {
                axis: WheelAxis::Vertical,
                delta120: 120,
                seq: 5,
            },
            Record::Wheel {
                axis: WheelAxis::Horizontal,
                delta120: -240,
                seq: 1000,
            },
        ] {
            let (buf, len) = encode_record(rec);
            assert_eq!(len, 10);
            assert_eq!(decode_record(&buf[..len]), Ok(rec));
        }
    }

    #[test]
    fn v1_kinds_decode_through_decode_record_too() {
        // decode_record is a superset of decode — same bytes, same answer, so a
        // caller that switches wholesale to the v2 API does not regress kind 1/2.
        let rec = InputRecord {
            kind: KeyKind::Down,
            vk: 0x41,
            seq: 0x0102_0304,
        };
        let bytes = encode(rec);
        assert_eq!(
            decode_record(&bytes),
            Ok(Record::VkDown {
                vk: 0x41,
                seq: 0x0102_0304
            })
        );
    }

    // --- v2: rejections ------------------------------------------------------

    #[test]
    fn decode_record_rejects_an_unknown_kind() {
        assert_eq!(
            decode_record(&[8, 0, 0, 0, 0, 0, 0, 0]),
            Err(InputProtoError::BadKind(8))
        );
    }

    #[test]
    fn decode_record_rejects_a_dirty_reserved_byte_on_a_new_kind() {
        // ScanDown with rsvd = 0x01 instead of 0.
        let bytes = [0x03, 0x01, 0x1E, 0x00, 0x01, 0, 0, 0];
        assert_eq!(
            decode_record(&bytes),
            Err(InputProtoError::ReservedNotZero(0x01))
        );
    }

    #[test]
    fn decode_record_rejects_a_bad_scancode_high_byte() {
        // High byte 0x01 is neither 0x00 nor the 0xE0 extended marker.
        let bytes = [0x03, 0x00, 0x1E, 0x01, 0x01, 0, 0, 0];
        assert_eq!(
            decode_record(&bytes),
            Err(InputProtoError::BadScancode(0x011E))
        );
    }

    #[test]
    fn decode_record_rejects_a_bad_button() {
        let bytes = [0x06, 0x00, 0x06, 0x01, 0x01, 0, 0, 0];
        assert_eq!(decode_record(&bytes), Err(InputProtoError::BadButton(6)));
    }

    #[test]
    fn decode_record_rejects_a_bad_down_flag() {
        let bytes = [0x06, 0x00, 0x01, 0x02, 0x01, 0, 0, 0];
        assert_eq!(decode_record(&bytes), Err(InputProtoError::BadDown(2)));
    }

    #[test]
    fn decode_record_rejects_a_bad_axis() {
        let bytes = [0x07, 0x00, 0x02, 0x00, 0x78, 0x00, 0x01, 0, 0, 0];
        assert_eq!(decode_record(&bytes), Err(InputProtoError::BadAxis(2)));
    }

    #[test]
    fn decode_record_rejects_a_dirty_wheel_pad() {
        let bytes = [0x07, 0x00, 0x00, 0x05, 0x78, 0x00, 0x01, 0, 0, 0];
        assert_eq!(
            decode_record(&bytes),
            Err(InputProtoError::PadNotZero(0x05))
        );
    }

    #[test]
    fn decode_record_rejects_a_zero_or_non_multiple_of_120_delta() {
        let zero = [0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0, 0, 0];
        assert_eq!(decode_record(&zero), Err(InputProtoError::BadDelta(0)));
        // 100 little-endian: 0x64, 0x00.
        let not_multiple = [0x07, 0x00, 0x00, 0x00, 0x64, 0x00, 0x01, 0, 0, 0];
        assert_eq!(
            decode_record(&not_multiple),
            Err(InputProtoError::BadDelta(100))
        );
    }

    #[test]
    fn decode_record_rejects_an_empty_buffer() {
        assert_eq!(
            decode_record(&[]),
            Err(InputProtoError::Short { need: 1, got: 0 })
        );
    }

    #[test]
    fn decode_record_rejects_a_record_truncated_after_the_kind_byte() {
        // A Wheel record (needs 10 bytes) with only its kind byte plus 4 more —
        // the truncated-stream-boundary case: the kind byte is legible, the
        // remainder is not fully present yet.
        let bytes = [0x07u8, 0x00, 0x00, 0x00, 0x78];
        assert_eq!(
            decode_record(&bytes),
            Err(InputProtoError::Short { need: 10, got: 5 })
        );
    }
}
