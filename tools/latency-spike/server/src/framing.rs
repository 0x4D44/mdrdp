//! The spike wire format.
//!
//! One message on the video channel is
//!
//! ```text
//! [u32 le length][u8 type][payload ...]
//! ```
//!
//! `length` counts the bytes that *follow* it — the type byte plus the payload — so
//! the smallest legal message is 1 byte long and carries no payload. A reader that
//! has the 4-byte length has everything it needs to skip a message it does not
//! understand, which is the whole point of putting the length first.
//!
//! Portable on purpose: the macOS viewer decodes with this same module.

/// One legacy H.264 access unit, Annex B, exactly one per message (wire v1):
/// carries no sequence number. Kept decodable so an archived capture stays
/// readable; a v2 server sends [`MSG_VIDEO_SEQ`] instead.
pub const MSG_VIDEO: u8 = 1;
/// One server stats line (JSON, no trailing newline).
pub const MSG_STATS: u8 = 2;
/// Raw dirty rects for one captured frame — the hybrid wire's fast path. Payload
/// layout lives in `crate::rects`.
pub const MSG_RECTS: u8 = 3;
/// One Annex B access unit prefixed by its `u64` LE capture sequence number. Its
/// codec follows the wire version: H.264 through v3, HEVC in v4. A new type rather
/// than a changed `MSG_VIDEO` payload lets older readers skip it safely.
pub const MSG_VIDEO_SEQ: u8 = 4;
/// Header-selected codec access unit: tile id + capture sequence + Annex B bytes.
/// H.264 may advertise multiple tiles; the HEVC fallback advertises one full-frame tile.
pub const MSG_VIDEO_TILE: u8 = 5;
/// Host cursor visibility. The pointer pixels themselves stay out of the video
/// surfaces so the viewer can draw its local cursor without capture latency.
pub const MSG_CURSOR: u8 = 6;
/// One atomic regional/full H.264 update. The payload carries every selected
/// tile AU and the exact desktop coverage those decoded pixels may replace.
pub const MSG_VIDEO_UPDATE: u8 = 7;
/// Sparse-lane pixel-free move prelude. The declared raw/video remainder follows
/// as its ordinary message type with the same frame sequence.
pub const MSG_MOVE_UPDATE: u8 = 8;
/// Bulk-lane baseline barrier for a move prelude. It ensures every earlier bulk
/// update is visible before the screen-to-screen copy reads its source.
pub const MSG_MOVE_FENCE: u8 = 9;
/// Exact acknowledgement of one complete logical visual update on wire v10.
pub const MSG_FRAME_ACK: u8 = 10;

pub const FRAME_ACK_BYTES: usize = 8;

/// The logical lanes required to complete one visual update.
///
/// Wire-v10 permits a raw leg, a video leg, or both. Bits outside the two
/// defined lanes, and the empty mask, are invalid on the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UpdateParts(u8);

/// Descriptive alias used by the wire/protocol documentation.
pub type RequiredParts = UpdateParts;

impl UpdateParts {
    pub const RAW: Self = Self(0b01);
    pub const VIDEO: Self = Self(0b10);
    pub const BOTH: Self = Self(Self::RAW.0 | Self::VIDEO.0);
    pub const ALL: Self = Self::BOTH;

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn from_bits(bits: u8) -> Option<Self> {
        match bits {
            1..=3 => Some(Self(bits)),
            _ => None,
        }
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for UpdateParts {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl TryFrom<u8> for UpdateParts {
    type Error = u8;

    fn try_from(bits: u8) -> Result<Self, Self::Error> {
        Self::from_bits(bits).ok_or(bits)
    }
}

pub const MOVE_FENCE_BYTES: usize = 16;

pub fn encode_move_fence(baseline_seq: u64, frame_seq: u64) -> [u8; MOVE_FENCE_BYTES] {
    let mut payload = [0; MOVE_FENCE_BYTES];
    payload[..8].copy_from_slice(&baseline_seq.to_le_bytes());
    payload[8..].copy_from_slice(&frame_seq.to_le_bytes());
    payload
}

pub fn decode_move_fence(payload: &[u8]) -> Result<(u64, u64), &'static str> {
    if payload.len() != MOVE_FENCE_BYTES {
        return Err("move fence is not its 16-byte payload");
    }
    let baseline_seq = u64::from_le_bytes(payload[..8].try_into().expect("8 bytes"));
    let frame_seq = u64::from_le_bytes(payload[8..].try_into().expect("8 bytes"));
    if baseline_seq >= frame_seq {
        return Err("move fence baseline is not older than its update");
    }
    Ok((baseline_seq, frame_seq))
}

/// Encode the exact logical frame sequence acknowledged by the viewer.
pub fn encode_frame_ack(frame_seq: u64) -> [u8; FRAME_ACK_BYTES] {
    frame_seq.to_le_bytes()
}

/// Decode one strict wire-v10 frame acknowledgement.
pub fn decode_frame_ack(payload: &[u8]) -> Result<u64, &'static str> {
    if payload.len() != FRAME_ACK_BYTES {
        return Err("frame acknowledgement is not its 8-byte payload");
    }
    Ok(u64::from_le_bytes(
        payload.try_into().expect("checked 8-byte acknowledgement"),
    ))
}

/// `[hidden: u8][reserved for later cursor metadata: 11]`.
pub const CURSOR_PAYLOAD_BYTES: usize = 12;

pub fn encode_cursor(hidden: bool) -> [u8; CURSOR_PAYLOAD_BYTES] {
    let mut payload = [0; CURSOR_PAYLOAD_BYTES];
    payload[0] = u8::from(hidden);
    payload
}

pub fn decode_cursor(payload: &[u8]) -> Result<bool, &'static str> {
    if payload.len() != CURSOR_PAYLOAD_BYTES {
        return Err("cursor state is not its 12-byte payload");
    }
    match payload[0] {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err("cursor hidden flag is not 0 or 1"),
    }
}

/// `[tile_id: u8][reserved: 3][capture_seq: u64 LE]`.
pub const TILE_AU_PREFIX: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileAu<'a> {
    pub tile_id: u8,
    pub capture_seq: u64,
    pub au: &'a [u8],
}

pub fn encode_tile_au(tile_id: u8, capture_seq: u64, au: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(TILE_AU_PREFIX + au.len());
    payload.push(tile_id);
    payload.extend_from_slice(&[0; 3]);
    payload.extend_from_slice(&capture_seq.to_le_bytes());
    payload.extend_from_slice(au);
    payload
}

pub fn decode_tile_au(payload: &[u8]) -> Result<TileAu<'_>, &'static str> {
    if payload.len() < TILE_AU_PREFIX {
        return Err("tile access unit is shorter than its 12-byte prefix");
    }
    if payload[1..4] != [0; 3] {
        return Err("tile access unit reserved bytes are non-zero");
    }
    Ok(TileAu {
        tile_id: payload[0],
        capture_seq: u64::from_le_bytes(payload[4..12].try_into().expect("8 bytes")),
        au: &payload[TILE_AU_PREFIX..],
    })
}

/// Header bytes ahead of the payload: the length field plus the type byte.
pub const HEADER_LEN: usize = 5;

/// Refuse anything larger than this. A 4K keyframe is a few hundred KB; 64 MB is far
/// past any legitimate access unit and stops a corrupt length from asking for a
/// gigabyte allocation.
pub const DEFAULT_MAX_PAYLOAD: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub msg_type: u8,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FramingError {
    /// `length` was 0, so the message does not even carry a type byte.
    Empty,
    /// `length` exceeded the reassembler's ceiling.
    Oversize { declared: usize, limit: usize },
}

impl std::fmt::Display for FramingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FramingError::Empty => write!(f, "framing: zero-length message (no type byte)"),
            FramingError::Oversize { declared, limit } => {
                write!(
                    f,
                    "framing: message of {declared} bytes exceeds limit {limit}"
                )
            }
        }
    }
}

impl std::error::Error for FramingError {}

/// Append one framed message to `out`.
pub fn encode(msg_type: u8, payload: &[u8], out: &mut Vec<u8>) {
    let length = (payload.len() + 1) as u32;
    out.extend_from_slice(&length.to_le_bytes());
    out.push(msg_type);
    out.extend_from_slice(payload);
}

/// Bytes `encode` will append for a payload of `payload_len`.
pub fn encoded_len(payload_len: usize) -> usize {
    HEADER_LEN + payload_len
}

/// Reassembles messages from a byte stream that arrives in arbitrary chunks.
///
/// TCP splits and coalesces freely, so a reader that assumes one `read` yields one
/// message is wrong on the first busy frame. Feed everything here instead.
#[derive(Debug)]
pub struct Reassembler {
    buf: Vec<u8>,
    /// Read cursor into `buf`. Consumed bytes are not removed on every message —
    /// they are compacted away once the cursor passes `COMPACT_THRESHOLD`, so a
    /// steady stream costs one memmove per few messages rather than per message.
    pos: usize,
    max_payload: usize,
}

/// Compact once this many consumed bytes have accumulated at the front.
const COMPACT_THRESHOLD: usize = 256 * 1024;

impl Default for Reassembler {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_PAYLOAD)
    }
}

impl Reassembler {
    pub fn new(max_payload: usize) -> Self {
        Self {
            buf: Vec::new(),
            pos: 0,
            max_payload,
        }
    }

    /// Bytes received but not yet returned as a message.
    pub fn buffered(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Pop the next complete message, if one has arrived.
    ///
    /// Returns `Ok(None)` when more bytes are needed. An error is terminal for the
    /// connection: the stream is out of sync and there is no safe resynchronisation
    /// point in a bare length-prefixed format.
    pub fn next_message(&mut self) -> Result<Option<Message>, FramingError> {
        let available = self.buffered();
        if available < 4 {
            return Ok(None);
        }
        let mut len_bytes = [0u8; 4];
        len_bytes.copy_from_slice(&self.buf[self.pos..self.pos + 4]);
        let declared = u32::from_le_bytes(len_bytes) as usize;
        if declared == 0 {
            return Err(FramingError::Empty);
        }
        // `declared` includes the type byte, so the payload is one byte shorter.
        if declared - 1 > self.max_payload {
            return Err(FramingError::Oversize {
                declared: declared - 1,
                limit: self.max_payload,
            });
        }
        if available < 4 + declared {
            return Ok(None);
        }
        let msg_type = self.buf[self.pos + 4];
        let payload_start = self.pos + 5;
        let payload_end = self.pos + 4 + declared;
        let payload = self.buf[payload_start..payload_end].to_vec();
        self.pos = payload_end;
        self.compact();
        Ok(Some(Message { msg_type, payload }))
    }

    fn compact(&mut self) {
        if self.pos >= COMPACT_THRESHOLD || self.pos == self.buf.len() {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_one(msg_type: u8, payload: &[u8]) -> Message {
        let mut wire = Vec::new();
        encode(msg_type, payload, &mut wire);
        let mut r = Reassembler::default();
        r.push(&wire);
        r.next_message().unwrap().unwrap()
    }

    #[test]
    fn tile_access_unit_prefix_keeps_tile_and_capture_sequence_distinct() {
        let payload = encode_tile_au(1, 0x0102_0304_0506_0708, &[0x65, 0xaa]);
        let tile = decode_tile_au(&payload).expect("valid tile payload");
        assert_eq!(tile.tile_id, 1);
        assert_eq!(tile.capture_seq, 0x0102_0304_0506_0708);
        assert_eq!(tile.au, &[0x65, 0xaa]);
        assert!(decode_tile_au(&payload[..TILE_AU_PREFIX - 1]).is_err());
    }

    #[test]
    fn cursor_visibility_round_trips_without_claiming_a_remote_position() {
        let hidden = encode_cursor(true);
        assert_eq!(hidden[1..], [0; CURSOR_PAYLOAD_BYTES - 1]);
        assert!(decode_cursor(&hidden).expect("valid hidden cursor state"));
        assert!(!decode_cursor(&encode_cursor(false)).expect("valid visible cursor state"));
        assert!(decode_cursor(&hidden[..CURSOR_PAYLOAD_BYTES - 1]).is_err());

        let mut malformed = hidden;
        malformed[0] = 2;
        assert!(decode_cursor(&malformed).is_err());
    }

    #[test]
    fn move_fence_names_the_required_baseline_and_the_update_that_releases_it() {
        let payload = encode_move_fence(41, 44);
        assert_eq!(decode_move_fence(&payload).unwrap(), (41, 44));
        assert!(decode_move_fence(&payload[..15]).is_err());
        assert!(decode_move_fence(&encode_move_fence(44, 44)).is_err());
    }

    #[test]
    fn frame_ack_round_trips_as_exact_little_endian_u64() {
        let payload = encode_frame_ack(0x0102_0304_0506_0708);
        assert_eq!(payload, [0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]);
        assert_eq!(decode_frame_ack(&payload), Ok(0x0102_0304_0506_0708));
    }

    #[test]
    fn frame_ack_rejects_every_non_exact_payload_size() {
        let payload = encode_frame_ack(44);
        for size in 0..=FRAME_ACK_BYTES + 1 {
            if size > payload.len() {
                let mut too_long = payload.to_vec();
                too_long.push(0);
                assert!(decode_frame_ack(&too_long).is_err());
            } else if size != FRAME_ACK_BYTES {
                assert!(
                    decode_frame_ack(&payload[..size]).is_err(),
                    "accepted {size} bytes"
                );
            }
        }
    }

    #[test]
    fn required_parts_accepts_only_nonempty_raw_video_bits() {
        assert_eq!(UpdateParts::from_bits(0), None);
        assert_eq!(UpdateParts::from_bits(1), Some(UpdateParts::RAW));
        assert_eq!(UpdateParts::from_bits(2), Some(UpdateParts::VIDEO));
        assert_eq!(UpdateParts::from_bits(3), Some(UpdateParts::BOTH));
        assert_eq!(UpdateParts::from_bits(4), None);
        assert_eq!(UpdateParts::from_bits(0xff), None);
    }

    #[test]
    fn the_length_prefix_counts_the_type_byte_and_the_payload() {
        let mut wire = Vec::new();
        encode(MSG_VIDEO, &[0xAA, 0xBB, 0xCC], &mut wire);
        assert_eq!(&wire[..4], &4u32.to_le_bytes());
        assert_eq!(wire[4], MSG_VIDEO);
        assert_eq!(&wire[5..], &[0xAA, 0xBB, 0xCC]);
        assert_eq!(wire.len(), encoded_len(3));
    }

    #[test]
    fn a_message_survives_the_roundtrip() {
        // Distinct bytes, not a repeated fill: a fixture of identical values cannot
        // tell a correct offset from an off-by-one.
        let payload: Vec<u8> = (0u8..=200).collect();
        let msg = roundtrip_one(MSG_STATS, &payload);
        assert_eq!(msg.msg_type, MSG_STATS);
        assert_eq!(msg.payload, payload);
    }

    #[test]
    fn an_empty_payload_is_legal() {
        let msg = roundtrip_one(MSG_VIDEO, &[]);
        assert_eq!(msg.msg_type, MSG_VIDEO);
        assert!(msg.payload.is_empty());
    }

    #[test]
    fn a_byte_at_a_time_dribble_reassembles() {
        let payloads: Vec<Vec<u8>> = vec![
            (0u8..17).collect(),
            vec![],
            (100u8..250).rev().collect(),
            vec![0xFF],
        ];
        let mut wire = Vec::new();
        for (i, p) in payloads.iter().enumerate() {
            encode(if i % 2 == 0 { MSG_VIDEO } else { MSG_STATS }, p, &mut wire);
        }

        let mut r = Reassembler::default();
        let mut got: Vec<Message> = Vec::new();
        for b in &wire {
            r.push(std::slice::from_ref(b));
            while let Some(m) = r.next_message().unwrap() {
                got.push(m);
            }
        }
        assert_eq!(got.len(), payloads.len(), "one message per encode call");
        for (i, p) in payloads.iter().enumerate() {
            assert_eq!(&got[i].payload, p, "payload {i}");
            assert_eq!(
                got[i].msg_type,
                if i % 2 == 0 { MSG_VIDEO } else { MSG_STATS },
                "type {i}"
            );
        }
        assert_eq!(r.buffered(), 0, "nothing left over");
    }

    #[test]
    fn three_messages_delivered_in_one_chunk_all_come_back() {
        let mut wire = Vec::new();
        encode(1, &[1, 2, 3], &mut wire);
        encode(2, &[4, 5], &mut wire);
        encode(1, &[6], &mut wire);
        let mut r = Reassembler::default();
        r.push(&wire);
        let a = r.next_message().unwrap().unwrap();
        let b = r.next_message().unwrap().unwrap();
        let c = r.next_message().unwrap().unwrap();
        assert_eq!((a.msg_type, a.payload), (1, vec![1, 2, 3]));
        assert_eq!((b.msg_type, b.payload), (2, vec![4, 5]));
        assert_eq!((c.msg_type, c.payload), (1, vec![6]));
        assert_eq!(r.next_message().unwrap(), None);
    }

    #[test]
    fn a_partial_header_yields_none_rather_than_a_bogus_message() {
        let mut r = Reassembler::default();
        r.push(&[0x05, 0x00, 0x00]);
        assert_eq!(r.next_message().unwrap(), None);
        assert_eq!(r.buffered(), 3);
    }

    #[test]
    fn a_zero_length_message_is_rejected() {
        let mut r = Reassembler::default();
        r.push(&0u32.to_le_bytes());
        assert_eq!(r.next_message(), Err(FramingError::Empty));
    }

    #[test]
    fn an_oversize_length_is_rejected_before_the_bytes_arrive() {
        let mut r = Reassembler::new(16);
        // 100 payload bytes declared against a 16-byte ceiling: the guard must fire
        // on the header alone, without waiting for 100 bytes that may never come.
        r.push(&101u32.to_le_bytes());
        assert_eq!(
            r.next_message(),
            Err(FramingError::Oversize {
                declared: 100,
                limit: 16
            })
        );
    }

    #[test]
    fn compaction_does_not_lose_a_straddling_message() {
        // Drive past COMPACT_THRESHOLD with a partial message pending, so the
        // compaction runs while unread bytes are still in the buffer.
        let payload = vec![7u8; 64 * 1024];
        let mut r = Reassembler::default();
        for _ in 0..8 {
            let mut wire = Vec::new();
            encode(MSG_VIDEO, &payload, &mut wire);
            // Split each message across two pushes.
            let (head, tail) = wire.split_at(3);
            r.push(head);
            assert_eq!(r.next_message().unwrap(), None);
            r.push(tail);
            let m = r.next_message().unwrap().unwrap();
            assert_eq!(m.payload.len(), payload.len());
            assert_eq!(m.payload, payload);
        }
    }
}
