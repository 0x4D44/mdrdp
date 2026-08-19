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
}
