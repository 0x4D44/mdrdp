//! The auxiliary session channel's message dialect (HLD tranche 5 §4).
//!
//! One framed, bidirectional TCP connection per session on port 9503, carrying
//! the traffic that must never delay video: clipboard now, audio in tranche 6.
//!
//! # Why this channel exists at all
//!
//! Video latency is the product's whole premise, so nothing lower-priority may
//! back it up. Before this, the native transport had **no client→server message
//! channel**: the video socket is server→client by construction (its reader
//! peeks for EOF and never consumes), and the input channel is frameless, where
//! an unknown record kind is *terminal* — sending anything unexpected there
//! kills keyboard, mouse and the session. This channel is the answer, and the
//! parent HLD §6 records the correction.
//!
//! # Priority
//!
//! Three tiers, and traffic never crosses upward: video (9500) is critical;
//! audio is continuous and latency-sensitive; clipboard is bursty bulk that can
//! wait. Audio and clipboard share this socket with a **strict-priority sender —
//! any pending audio drains before any pending clipboard**. The residual
//! head-of-line hazard is one large clipboard message delaying audio, which the
//! clipboard size ceiling bounds (§7) and tranche 6 must measure rather than
//! assume.
//!
//! # Namespace
//!
//! These type bytes are deliberately **disjoint from `input_proto`'s record
//! kinds** (1..=7). A stray byte on the input channel is a valid `MouseMove`, so
//! a framing mistake there injects mouse motion rather than erroring cleanly;
//! nothing here may share that hazard, and starting at 0x20 makes a
//! cross-wired byte obviously invalid on either side.
//!
//! Framing is [`crate::framing`] — the same length-prefixed encoding as the
//! video socket, so an unknown type skips safely in **both** directions, which
//! was only ever true one way before.

use crate::framing;

/// Clipboard content, either direction.
///
/// Deliberately not `1` and not in `input_proto`'s range: see the module docs.
pub const MSG_CLIPBOARD: u8 = 0x20;

/// The clipboard payload's format byte. Text is the only format this tranche
/// carries; the byte exists so images can be added later without a wire break.
pub const CLIPBOARD_FORMAT_TEXT_UTF8: u8 = 1;

/// The largest clipboard payload this channel will carry, in bytes.
///
/// **Not a tuning constant — a session-safety one.** Above the framing layer's
/// own limit an oversize message is a terminal `FramingError`, which the client
/// turns into a dropped session: a large copy would kill the remote desktop,
/// which is worse than the wedge this tranche exists to prevent. So the ceiling
/// sits far below that, and is enforced on **both** send and receive because a
/// peer may ignore it.
///
/// 256 KiB is chosen to match the clipboard fingerprint's prefix cap. Above that
/// cap the fingerprint hashes a prefix plus the total length, so a same-length
/// edit past the prefix is *guaranteed* to go undetected — capping here removes
/// that whole class by construction rather than documenting it as accepted risk.
pub const MAX_CLIPBOARD_BYTES: usize = 256 * 1024;

/// Host → client audio (HLD tranche 6 §3).
pub const MSG_AUDIO: u8 = 0x21;

/// Client → host audio control: one byte, 0 = stop capturing, 1 = start.
///
/// Audio is **not** server-push-always-on. Without this a client that does not
/// want audio still pays the bandwidth on a channel the parent HLD says must
/// never back up video. One byte is not a handshake with states to wedge in.
pub const MSG_AUDIO_CONTROL: u8 = 0x22;

/// The only audio format this build speaks: interleaved 16-bit LE PCM.
///
/// A tag rather than padding, declared now so a later codec is purely additive
/// rather than a wire break.
pub const AUDIO_FORMAT_PCM16: u8 = 0;

/// `u32` rate + `u8` channels + `u8` format + `u64` capture position.
pub const AUDIO_HEADER_BYTES: usize = 14;

/// The largest audio payload this channel will carry, in bytes.
///
/// **Session-safety, not tuning** — the same class as [`MAX_CLIPBOARD_BYTES`],
/// and enforced on both send and receive because a peer may ignore it.
///
/// Without a ceiling here the client's reassembler permits `DEFAULT_MAX_PAYLOAD`
/// (64 MiB). One oversize frame would decode to a 128 MiB `Vec<f32>` and then be
/// pushed into the playback ring one sample at a time **while holding the lock
/// the cpal realtime callback needs** — a guaranteed device xrun and a
/// multi-second stall, from a single bad length field.
///
/// 16 KiB is ~85 ms at 48 kHz stereo, comfortably above the 10 ms frame this
/// tranche sends and far below anything that could stall the device.
pub const MAX_AUDIO_BYTES: usize = 16 * 1024;

/// Sample rates we will accept from a peer.
///
/// Unvalidated, `sample_rate` feeds the resampler directly: a rate of 1 with a
/// 480-frame payload asks for `480 * 48000 / 1` output frames and allocates
/// ~184 MB from a four-byte header field.
pub const MIN_SAMPLE_RATE: u32 = 8_000;
/// See [`MIN_SAMPLE_RATE`].
pub const MAX_SAMPLE_RATE: u32 = 192_000;

/// A decoded auxiliary-channel message.
///
/// `Debug` is **hand-written** for the clipboard variant: deriving it would put
/// clipboard content — passwords, one-time codes, whole documents — into any
/// `debug!(?msg)` a future maintainer writes. That is the exact shape of the
/// tranche-3 hazard where a derived `Debug` would have printed a password, and
/// the server half of this path writes to a log file on the host that is read
/// and quoted during diagnosis.
#[derive(Clone, PartialEq, Eq)]
pub enum AuxMessage {
    /// UTF-8 clipboard text. Line endings are canonical **LF on the wire**; the
    /// Windows end converts to and from CRLF at exactly one place, because a
    /// round trip that is not byte-identical ping-pongs forever at poll cadence.
    ClipboardText(String),
    /// One block of interleaved PCM from the host.
    Audio(AudioFrame),
    /// The client asking the host to start or stop capturing.
    AudioControl { enable: bool },
}

/// One block of interleaved 16-bit PCM, with the timing reference that makes it
/// interpretable.
///
/// `Debug` is **hand-written**, like every other payload-carrying type on this
/// path. Session audio is user content — a voice call is as sensitive as a
/// clipboard — and the repo rule is that session contents are never logged.
#[derive(Clone, PartialEq, Eq)]
pub struct AudioFrame {
    pub sample_rate: u32,
    pub channels: u8,
    /// The host's capture sample position of this frame's first sample.
    ///
    /// **This is the field that makes the stream interpretable, and revision 1
    /// of the HLD did not have it.** It advances across silence the host elided,
    /// so the client can tell "the desktop was quiet" from "frames were dropped"
    /// from "the host's clock is running fast" — three causes that otherwise
    /// present identically as a change in ring depth, which is exactly what both
    /// correction mechanisms key off. Drift becomes a measurable slope rather
    /// than an inference.
    pub capture_pos: u64,
    /// Interleaved 16-bit LE samples, still as bytes.
    ///
    /// Empty PCM is the one quiet-window boundary. It resets client continuity
    /// and is never offered to the audio device; subsequent quiet stays wire-idle.
    ///
    /// Kept as bytes rather than `Vec<i16>` so the client hands them straight to
    /// the already-tested `pcm16_le_to_f32` without an intermediate conversion.
    pub pcm: Vec<u8>,
}

impl AudioFrame {
    /// Bytes per whole frame of audio (one sample for each channel).
    fn frame_stride(channels: u8) -> usize {
        channels as usize * 2
    }
}

impl std::fmt::Debug for AudioFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Shape and timing only. Never the samples.
        f.debug_struct("AudioFrame")
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("capture_pos", &self.capture_pos)
            .field("pcm_bytes", &self.pcm.len())
            .finish()
    }
}

impl std::fmt::Debug for AuxMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Length only. Never the bytes, and never a raw fingerprint either:
            // the fingerprint is an unsalted hash, so for a 6-digit code or a
            // PIN it is an offline verifier, not an opaque token.
            AuxMessage::ClipboardText(text) => f
                .debug_struct("ClipboardText")
                .field("bytes", &text.len())
                .finish(),
            // Delegates to AudioFrame's own hand-written Debug, which prints
            // shape and never samples.
            AuxMessage::Audio(frame) => f.debug_tuple("Audio").field(frame).finish(),
            AuxMessage::AudioControl { enable } => f
                .debug_struct("AudioControl")
                .field("enable", enable)
                .finish(),
        }
    }
}

/// Why a message could not be decoded.
///
/// No variant carries payload bytes or any slice of them — an error string is a
/// log line you did not write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuxProtoError {
    /// The payload was empty, so it carries no format byte.
    Empty,
    /// A format this build does not speak. Skipped, never fatal.
    UnknownFormat(u8),
    /// The text was not valid UTF-8.
    NotUtf8,
    /// Larger than [`MAX_CLIPBOARD_BYTES`]. Refused, never truncated: silently
    /// pasting half a document is a data-integrity bug.
    TooLarge { bytes: usize, limit: usize },
    /// An audio payload too short to hold its own header.
    AudioShortHeader { bytes: usize },
    /// Larger than [`MAX_AUDIO_BYTES`].
    AudioTooLarge { bytes: usize, limit: usize },
    /// A sample rate outside [`MIN_SAMPLE_RATE`]..=[`MAX_SAMPLE_RATE`].
    AudioBadRate(u32),
    /// A channel count this build does not carry. Only mono and stereo.
    AudioBadChannels(u8),
    /// An audio format tag this build does not speak.
    AudioBadFormat(u8),
    /// A PCM length that is not a whole number of interleaved frames.
    ///
    /// Refused rather than trimmed. The playback ring is a flat buffer of
    /// samples with no frame alignment of its own, so a single odd-length frame
    /// flips its parity and swaps left and right **for the rest of the
    /// session** — silently, with no counter and nothing in the audio that looks
    /// wrong enough to investigate.
    AudioMisaligned { bytes: usize, channels: u8 },
    /// An audio-control payload that was not exactly one byte.
    AudioControlMalformed { bytes: usize },
}

impl std::fmt::Display for AuxProtoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuxProtoError::Empty => write!(f, "empty clipboard payload"),
            AuxProtoError::UnknownFormat(id) => write!(f, "unknown clipboard format {id}"),
            AuxProtoError::NotUtf8 => write!(f, "clipboard text was not valid UTF-8"),
            AuxProtoError::TooLarge { bytes, limit } => write!(
                f,
                "clipboard payload is {bytes} bytes, over the {limit}-byte share limit"
            ),
            AuxProtoError::AudioShortHeader { bytes } => {
                write!(
                    f,
                    "audio payload is {bytes} bytes, too short for its header"
                )
            }
            AuxProtoError::AudioTooLarge { bytes, limit } => write!(
                f,
                "audio payload is {bytes} bytes, over the {limit}-byte frame limit"
            ),
            AuxProtoError::AudioBadRate(rate) => write!(f, "audio sample rate {rate} out of range"),
            AuxProtoError::AudioBadChannels(ch) => {
                write!(f, "audio channel count {ch} unsupported")
            }
            AuxProtoError::AudioBadFormat(id) => write!(f, "unknown audio format {id}"),
            AuxProtoError::AudioMisaligned { bytes, channels } => write!(
                f,
                "audio payload of {bytes} bytes is not a whole number of {channels}-channel frames"
            ),
            AuxProtoError::AudioControlMalformed { bytes } => {
                write!(f, "audio control payload is {bytes} bytes, expected 1")
            }
        }
    }
}

/// Encode clipboard text onto the wire, refusing anything over the ceiling.
///
/// Returns the framed bytes ready to write. The ceiling is checked here, on the
/// send side, so an oversize payload never reaches the peer's reassembler.
pub fn encode_clipboard_text(text: &str, out: &mut Vec<u8>) -> Result<(), AuxProtoError> {
    if text.len() > MAX_CLIPBOARD_BYTES {
        return Err(AuxProtoError::TooLarge {
            bytes: text.len(),
            limit: MAX_CLIPBOARD_BYTES,
        });
    }
    let mut payload = Vec::with_capacity(1 + text.len());
    payload.push(CLIPBOARD_FORMAT_TEXT_UTF8);
    payload.extend_from_slice(text.as_bytes());
    framing::encode(MSG_CLIPBOARD, &payload, out);
    Ok(())
}

/// Decode one clipboard payload (the framed message's body, type byte already
/// consumed by the framing layer).
///
/// The size check runs **before** the UTF-8 conversion so an oversize payload is
/// refused without being materialised as a `String`.
pub fn decode_clipboard(payload: &[u8]) -> Result<AuxMessage, AuxProtoError> {
    let (&format, body) = payload.split_first().ok_or(AuxProtoError::Empty)?;
    if format != CLIPBOARD_FORMAT_TEXT_UTF8 {
        return Err(AuxProtoError::UnknownFormat(format));
    }
    if body.len() > MAX_CLIPBOARD_BYTES {
        return Err(AuxProtoError::TooLarge {
            bytes: body.len(),
            limit: MAX_CLIPBOARD_BYTES,
        });
    }
    let text = std::str::from_utf8(body).map_err(|_| AuxProtoError::NotUtf8)?;
    Ok(AuxMessage::ClipboardText(text.to_owned()))
}

/// Encode one audio frame onto the wire, refusing anything malformed here rather
/// than letting the peer's reassembler meet it.
pub fn encode_audio(frame: &AudioFrame, out: &mut Vec<u8>) -> Result<(), AuxProtoError> {
    validate_audio_shape(frame.sample_rate, frame.channels, frame.pcm.len())?;
    let mut payload = Vec::with_capacity(AUDIO_HEADER_BYTES + frame.pcm.len());
    payload.extend_from_slice(&frame.sample_rate.to_le_bytes());
    payload.push(frame.channels);
    payload.push(AUDIO_FORMAT_PCM16);
    payload.extend_from_slice(&frame.capture_pos.to_le_bytes());
    payload.extend_from_slice(&frame.pcm);
    framing::encode(MSG_AUDIO, &payload, out);
    Ok(())
}

/// Decode one audio payload (type byte already consumed by the framing layer).
///
/// **Every header field is validated before anything is materialised.** An
/// unvalidated header is not a self-describing format, it is an instruction to
/// allocate whatever a peer says.
pub fn decode_audio(payload: &[u8]) -> Result<AuxMessage, AuxProtoError> {
    if payload.len() < AUDIO_HEADER_BYTES {
        return Err(AuxProtoError::AudioShortHeader {
            bytes: payload.len(),
        });
    }
    let (header, pcm) = payload.split_at(AUDIO_HEADER_BYTES);
    let sample_rate = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let channels = header[4];
    let format = header[5];
    let capture_pos = u64::from_le_bytes([
        header[6], header[7], header[8], header[9], header[10], header[11], header[12], header[13],
    ]);
    if format != AUDIO_FORMAT_PCM16 {
        return Err(AuxProtoError::AudioBadFormat(format));
    }
    validate_audio_shape(sample_rate, channels, pcm.len())?;
    Ok(AuxMessage::Audio(AudioFrame {
        sample_rate,
        channels,
        capture_pos,
        pcm: pcm.to_vec(),
    }))
}

/// The shape rules both directions enforce, in one place so send and receive
/// cannot drift apart.
fn validate_audio_shape(
    sample_rate: u32,
    channels: u8,
    pcm_bytes: usize,
) -> Result<(), AuxProtoError> {
    if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&sample_rate) {
        return Err(AuxProtoError::AudioBadRate(sample_rate));
    }
    if channels != 1 && channels != 2 {
        return Err(AuxProtoError::AudioBadChannels(channels));
    }
    if pcm_bytes > MAX_AUDIO_BYTES {
        return Err(AuxProtoError::AudioTooLarge {
            bytes: pcm_bytes,
            limit: MAX_AUDIO_BYTES,
        });
    }
    if !pcm_bytes.is_multiple_of(AudioFrame::frame_stride(channels)) {
        return Err(AuxProtoError::AudioMisaligned {
            bytes: pcm_bytes,
            channels,
        });
    }
    Ok(())
}

/// Encode the client's start/stop request.
pub fn encode_audio_control(enable: bool, out: &mut Vec<u8>) {
    framing::encode(MSG_AUDIO_CONTROL, &[u8::from(enable)], out);
}

/// Decode the client's start/stop request.
pub fn decode_audio_control(payload: &[u8]) -> Result<AuxMessage, AuxProtoError> {
    match payload {
        [byte] => Ok(AuxMessage::AudioControl { enable: *byte != 0 }),
        other => Err(AuxProtoError::AudioControlMalformed { bytes: other.len() }),
    }
}

/// Normalise line endings for the wire: CRLF and lone CR both become LF.
///
/// The wire is canonically LF and the conversion happens at exactly one place on
/// each side. Without this a multi-line copy loops forever: the Mac sends
/// `a\nb`, Windows stores `a\r\nb`, its listener sees a change and sends it back,
/// AppKit normalises to `a\nb`, and round it goes at poll cadence. Fingerprint
/// suppression cannot break that loop, because the round trip is not
/// byte-identical.
pub fn to_wire_newlines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                // CRLF collapses to one LF; a lone CR becomes one too.
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            other => out.push(other),
        }
    }
    out
}

/// Expand canonical LF to CRLF for Windows consumers.
///
/// Idempotent with respect to text that already contains CRLF, so applying it to
/// a payload that somehow arrived with CRLF cannot produce CRCRLF.
pub fn to_crlf(text: &str) -> String {
    to_wire_newlines(text).replace('\n', "\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_type_cannot_be_confused_with_an_input_record_kind() {
        // A stray byte on the input channel is a valid MouseMove, so a
        // cross-wiring mistake there injects mouse motion instead of erroring.
        // Keeping this namespace disjoint means the same mistake is loudly
        // invalid on both channels. Asserted against the input protocol's own
        // range rather than a copied literal.
        assert!(
            crate::input_proto::kind_len(MSG_CLIPBOARD).is_none(),
            "MSG_CLIPBOARD {MSG_CLIPBOARD:#x} is a valid input record kind"
        );
        // …and the reverse: no input kind may collide with ours.
        for kind in 0..=u8::MAX {
            if crate::input_proto::kind_len(kind).is_some() {
                assert_ne!(kind, MSG_CLIPBOARD, "input kind {kind} collides");
            }
        }
    }

    #[test]
    fn clipboard_text_round_trips_through_the_framing() {
        let mut wire = Vec::new();
        encode_clipboard_text("hello\nworld", &mut wire).expect("under the ceiling");
        let mut reassembler = framing::Reassembler::new(framing::DEFAULT_MAX_PAYLOAD);
        reassembler.push(&wire);
        let msg = reassembler
            .next_message()
            .expect("well formed")
            .expect("one whole message");
        assert_eq!(msg.msg_type, MSG_CLIPBOARD);
        assert_eq!(
            decode_clipboard(&msg.payload),
            Ok(AuxMessage::ClipboardText("hello\nworld".to_owned()))
        );
    }

    #[test]
    fn non_ascii_survives_the_round_trip_byte_exactly() {
        // UTF-8 correctness is not assumable, and a clipboard that mangles
        // accents is a clipboard people stop trusting.
        let payload = "café — naïve 日本語 🎉";
        let mut wire = Vec::new();
        encode_clipboard_text(payload, &mut wire).expect("under the ceiling");
        let mut reassembler = framing::Reassembler::new(framing::DEFAULT_MAX_PAYLOAD);
        reassembler.push(&wire);
        let msg = reassembler.next_message().unwrap().unwrap();
        assert_eq!(
            decode_clipboard(&msg.payload),
            Ok(AuxMessage::ClipboardText(payload.to_owned()))
        );
    }

    #[test]
    fn an_oversize_payload_is_refused_on_send_and_never_framed() {
        // The session-safety property: an oversize message must never reach the
        // peer's reassembler, where it would be a terminal framing error and
        // drop the whole session.
        let huge = "x".repeat(MAX_CLIPBOARD_BYTES + 1);
        let mut wire = Vec::new();
        let outcome = encode_clipboard_text(&huge, &mut wire);
        assert_eq!(
            outcome,
            Err(AuxProtoError::TooLarge {
                bytes: MAX_CLIPBOARD_BYTES + 1,
                limit: MAX_CLIPBOARD_BYTES,
            })
        );
        assert!(
            wire.is_empty(),
            "nothing may be written for a refused payload"
        );
    }

    #[test]
    fn an_oversize_payload_is_also_refused_on_receive() {
        // A peer may ignore the rule; the receiver must not depend on it.
        let mut payload = vec![CLIPBOARD_FORMAT_TEXT_UTF8];
        payload.extend(std::iter::repeat_n(b'x', MAX_CLIPBOARD_BYTES + 1));
        assert!(matches!(
            decode_clipboard(&payload),
            Err(AuxProtoError::TooLarge { .. })
        ));
    }

    #[test]
    fn exactly_the_ceiling_is_allowed() {
        // Off-by-one at a safety boundary is worth pinning in both directions.
        let at_limit = "x".repeat(MAX_CLIPBOARD_BYTES);
        let mut wire = Vec::new();
        assert!(encode_clipboard_text(&at_limit, &mut wire).is_ok());
        assert!(!wire.is_empty());
    }

    #[test]
    fn a_malformed_payload_is_an_error_not_a_panic() {
        assert_eq!(decode_clipboard(&[]), Err(AuxProtoError::Empty));
        assert_eq!(
            decode_clipboard(&[99, b'h', b'i']),
            Err(AuxProtoError::UnknownFormat(99))
        );
        assert_eq!(
            decode_clipboard(&[CLIPBOARD_FORMAT_TEXT_UTF8, 0xFF, 0xFE]),
            Err(AuxProtoError::NotUtf8)
        );
    }

    #[test]
    fn newline_conversion_round_trips_byte_exactly() {
        // THE anti-ping-pong property. If LF -> CRLF -> LF is not the identity,
        // every multi-line copy loops forever at poll cadence.
        for original in [
            "a\nb",
            "a\nb\n",
            "\n",
            "no newlines at all",
            "trailing\n\n",
            "café\nnaïve\n",
        ] {
            let windows = to_crlf(original);
            assert_eq!(
                to_wire_newlines(&windows),
                original,
                "LF -> CRLF -> LF must be the identity for {original:?}"
            );
        }
    }

    #[test]
    fn text_that_already_contains_crlf_normalises_rather_than_doubling() {
        // Content copied from a Windows app can already be CRLF. Expanding it
        // again would produce CRCRLF and a payload that differs on every hop.
        assert_eq!(to_wire_newlines("a\r\nb"), "a\nb");
        assert_eq!(to_crlf("a\r\nb"), "a\r\nb");
        // A lone CR (classic Mac line ending) is also canonicalised.
        assert_eq!(to_wire_newlines("a\rb"), "a\nb");
    }

    #[test]
    fn debug_never_prints_clipboard_content() {
        // The tranche-3 pattern: a derived Debug on a content-carrying type puts
        // a password into any debug!(?msg) someone later writes — and the server
        // half logs to a file on the host that gets quoted in journals.
        let msg = AuxMessage::ClipboardText("hunter2-the-actual-secret".to_owned());
        let rendered = format!("{msg:?}");
        assert!(
            !rendered.contains("hunter2"),
            "Debug leaked clipboard content: {rendered}"
        );
        assert!(
            rendered.contains("25"),
            "Debug should still report the length: {rendered}"
        );
    }

    #[test]
    fn an_error_message_never_quotes_the_content() {
        // Third-party error text and our own both end up in logs.
        let err = AuxProtoError::TooLarge {
            bytes: 999_999,
            limit: MAX_CLIPBOARD_BYTES,
        };
        let rendered = err.to_string();
        assert!(rendered.contains("999999") || rendered.contains("999_999"));
        assert!(
            !rendered.contains('x'),
            "no payload bytes in an error: {rendered}"
        );
    }

    // -- audio (tranche 6) ------------------------------------------------------

    /// A frame with a distinguishable pattern, so a test can tell a correct
    /// round trip from a plausible-looking one.
    fn stereo_frame(frames: usize) -> AudioFrame {
        let mut pcm = Vec::with_capacity(frames * 4);
        for i in 0..frames {
            // Left and right deliberately DIFFER, and both vary with i. A fixture
            // whose fields all hold the same value cannot tell a channel swap
            // from a correct decode, nor a reversed buffer from an intact one.
            let left = (i as i16).wrapping_mul(3);
            let right = (i as i16).wrapping_mul(3).wrapping_add(1);
            pcm.extend_from_slice(&left.to_le_bytes());
            pcm.extend_from_slice(&right.to_le_bytes());
        }
        AudioFrame {
            sample_rate: 48_000,
            channels: 2,
            capture_pos: 123_456,
            pcm,
        }
    }

    #[test]
    fn an_audio_frame_round_trips_byte_for_byte() {
        let frame = stereo_frame(64);
        let mut wire = Vec::new();
        encode_audio(&frame, &mut wire).expect("a well-formed frame encodes");

        // Skip the framing header the encoder added.
        let payload = &wire[framing::HEADER_LEN..];
        let decoded = decode_audio(payload).expect("what we encoded must decode");
        match decoded {
            AuxMessage::Audio(got) => {
                assert_eq!(got.sample_rate, 48_000);
                assert_eq!(got.channels, 2);
                assert_eq!(
                    got.capture_pos, 123_456,
                    "the timing reference must survive"
                );
                assert_eq!(got.pcm, frame.pcm, "samples must be byte-identical");
            }
            other => panic!("expected audio, got {other:?}"),
        }
    }

    #[test]
    fn capture_position_survives_a_value_that_needs_all_eight_bytes() {
        // At 48 kHz a u32 position wraps after ~25 hours. This product is meant
        // to run all day, so the field is u64 and the test uses a value that
        // would be truncated by anything narrower.
        let mut frame = stereo_frame(4);
        frame.capture_pos = 0x0123_4567_89AB_CDEF;
        let mut wire = Vec::new();
        encode_audio(&frame, &mut wire).unwrap();
        let decoded = decode_audio(&wire[framing::HEADER_LEN..]).unwrap();
        match decoded {
            AuxMessage::Audio(got) => assert_eq!(got.capture_pos, 0x0123_4567_89AB_CDEF),
            other => panic!("expected audio, got {other:?}"),
        }
    }

    #[test]
    fn an_oversize_frame_is_refused_by_the_sender() {
        let frame = AudioFrame {
            sample_rate: 48_000,
            channels: 2,
            capture_pos: 0,
            pcm: vec![0u8; MAX_AUDIO_BYTES + 4],
        };
        let mut wire = Vec::new();
        assert_eq!(
            encode_audio(&frame, &mut wire),
            Err(AuxProtoError::AudioTooLarge {
                bytes: MAX_AUDIO_BYTES + 4,
                limit: MAX_AUDIO_BYTES,
            })
        );
        assert!(wire.is_empty(), "nothing may reach the wire");
    }

    #[test]
    fn an_oversize_frame_is_refused_by_the_receiver_too() {
        // The send-side check is not enough: a peer may ignore the ceiling, and
        // the receiver is where an oversize payload does its damage.
        let mut payload = Vec::new();
        payload.extend_from_slice(&48_000u32.to_le_bytes());
        payload.push(2);
        payload.push(AUDIO_FORMAT_PCM16);
        payload.extend_from_slice(&0u64.to_le_bytes());
        payload.extend_from_slice(&vec![0u8; MAX_AUDIO_BYTES + 4]);
        assert_eq!(
            decode_audio(&payload),
            Err(AuxProtoError::AudioTooLarge {
                bytes: MAX_AUDIO_BYTES + 4,
                limit: MAX_AUDIO_BYTES,
            })
        );
    }

    #[test]
    fn a_misaligned_payload_is_refused_rather_than_trimmed() {
        // One odd-length frame flips the playback ring's parity and swaps L/R
        // for the rest of the session. Trimming would hide it; refusing counts it.
        let mut payload = Vec::new();
        payload.extend_from_slice(&48_000u32.to_le_bytes());
        payload.push(2);
        payload.push(AUDIO_FORMAT_PCM16);
        payload.extend_from_slice(&0u64.to_le_bytes());
        payload.extend_from_slice(&[1, 2, 3, 4, 5, 6]); // 6 bytes = 1.5 stereo frames
        assert_eq!(
            decode_audio(&payload),
            Err(AuxProtoError::AudioMisaligned {
                bytes: 6,
                channels: 2,
            })
        );
    }

    #[test]
    fn an_absurd_sample_rate_is_refused_before_anything_is_allocated() {
        // rate=1 with a 480-frame payload would ask the resampler for
        // 480 * 48000 / 1 output frames: ~184 MB from a four-byte field.
        for bad in [0u32, 1, 7_999, 192_001, u32::MAX] {
            let mut payload = Vec::new();
            payload.extend_from_slice(&bad.to_le_bytes());
            payload.push(2);
            payload.push(AUDIO_FORMAT_PCM16);
            payload.extend_from_slice(&0u64.to_le_bytes());
            payload.extend_from_slice(&[0, 0, 0, 0]);
            assert_eq!(
                decode_audio(&payload),
                Err(AuxProtoError::AudioBadRate(bad)),
                "rate {bad} must be refused"
            );
        }
    }

    #[test]
    fn an_unsupported_channel_count_is_refused() {
        // channels=0 would divide by zero computing the stride; the catch-all in
        // the client's channel remapper would otherwise push garbage interleaving.
        for bad in [0u8, 3, 6, 255] {
            let mut payload = Vec::new();
            payload.extend_from_slice(&48_000u32.to_le_bytes());
            payload.push(bad);
            payload.push(AUDIO_FORMAT_PCM16);
            payload.extend_from_slice(&0u64.to_le_bytes());
            payload.extend_from_slice(&[0, 0, 0, 0]);
            assert_eq!(
                decode_audio(&payload),
                Err(AuxProtoError::AudioBadChannels(bad)),
                "channel count {bad} must be refused"
            );
        }
    }

    #[test]
    fn an_unknown_format_tag_is_refused_and_not_treated_as_pcm() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&48_000u32.to_le_bytes());
        payload.push(2);
        payload.push(9); // a codec this build does not speak
        payload.extend_from_slice(&0u64.to_le_bytes());
        payload.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(
            decode_audio(&payload),
            Err(AuxProtoError::AudioBadFormat(9))
        );
    }

    #[test]
    fn a_payload_too_short_for_its_header_is_refused_without_indexing_past_it() {
        // The obvious hostile input. Anything that reads the header before
        // checking the length panics here instead of erroring.
        for len in 0..AUDIO_HEADER_BYTES {
            let payload = vec![0u8; len];
            assert_eq!(
                decode_audio(&payload),
                Err(AuxProtoError::AudioShortHeader { bytes: len }),
                "a {len}-byte payload must be refused"
            );
        }
    }

    #[test]
    fn audio_control_round_trips_both_ways() {
        for enable in [true, false] {
            let mut wire = Vec::new();
            encode_audio_control(enable, &mut wire);
            let decoded = decode_audio_control(&wire[framing::HEADER_LEN..]).unwrap();
            assert_eq!(decoded, AuxMessage::AudioControl { enable });
        }
    }

    #[test]
    fn a_malformed_audio_control_payload_is_refused() {
        assert_eq!(
            decode_audio_control(&[]),
            Err(AuxProtoError::AudioControlMalformed { bytes: 0 })
        );
        assert_eq!(
            decode_audio_control(&[1, 2]),
            Err(AuxProtoError::AudioControlMalformed { bytes: 2 })
        );
    }

    #[test]
    fn audio_kinds_stay_disjoint_from_every_other_user_of_this_wire() {
        // input_proto's record kinds are 1..=7 and a stray byte there is a valid
        // MouseMove, so a cross-wired byte must be obviously invalid instead.
        let aux = [MSG_CLIPBOARD, MSG_AUDIO, MSG_AUDIO_CONTROL];
        for kind in aux {
            assert!(
                kind >= 0x20,
                "0x{kind:02x} collides with input record kinds"
            );
        }
        let mut seen = std::collections::HashSet::new();
        for kind in aux {
            assert!(seen.insert(kind), "0x{kind:02x} is used twice");
        }
    }

    #[test]
    fn debug_on_an_audio_frame_reports_shape_but_never_samples() {
        // Session audio is user content. A voice call is as sensitive as a
        // clipboard, and this path writes to a log file on the host.
        let mut frame = stereo_frame(3);
        // A byte pattern that would be unmistakable if it leaked.
        frame.pcm = vec![0xAB, 0xCD, 0xAB, 0xCD];
        let rendered = format!("{frame:?}");
        assert!(
            !rendered.contains("171") && !rendered.contains("205") && !rendered.contains("ab"),
            "sample bytes must never render: {rendered}"
        );
        assert!(
            rendered.contains('4'),
            "the length should still show: {rendered}"
        );
        // And through the enum, which is what a debug!(?msg) would actually print.
        let wrapped = format!("{:?}", AuxMessage::Audio(frame));
        assert!(
            !wrapped.contains("171") && !wrapped.contains("205"),
            "sample bytes must not leak through AuxMessage either: {wrapped}"
        );
    }
}
