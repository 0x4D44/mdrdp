//! X.224 connection request/confirm — the RDP security-protocol negotiation.
//!
//! This exchange happens before any authentication, so it can be probed with no
//! credential at all. It tells us which security protocol a server demands.
//!
//! Everything here parses data from an unauthenticated peer, so it is bounds-checked
//! throughout and returns `Result`. A panic in this module is a crash the far end
//! controls.

use crate::probe::wire::TPKT_HEADER_LEN;
use serde::Serialize;
use std::fmt;

/// Security protocols a client may request or a server may select.
/// `[MS-RDPBCGR]` 2.2.1.1.1 / 2.2.1.2.1.
pub const PROTOCOL_RDP: u32 = 0x0000_0000;
pub const PROTOCOL_SSL: u32 = 0x0000_0001;
pub const PROTOCOL_HYBRID: u32 = 0x0000_0002;
pub const PROTOCOL_HYBRID_EX: u32 = 0x0000_0008;

/// What a modern client offers: TLS, CredSSP, and CredSSP with Early User Authorization.
pub const PROTOCOL_ALL_MODERN: u32 = PROTOCOL_SSL | PROTOCOL_HYBRID | PROTOCOL_HYBRID_EX;

const TPKT_VERSION: u8 = 0x03;
const X224_CONNECTION_REQUEST: u8 = 0xE0;
const X224_CONNECTION_CONFIRM: u8 = 0xD0;
const NEG_TYPE_REQUEST: u8 = 0x01;
const NEG_TYPE_RESPONSE: u8 = 0x02;
const NEG_TYPE_FAILURE: u8 = 0x03;
const NEG_STRUCT_LEN: u16 = 8;

/// Byte offset of the negotiation structure: TPKT header (4) + X.224 CC header (7).
const NEG_OFFSET: usize = 11;

/// Bytes in a class-0 connection confirm after the length indicator and before any
/// variable part: code, dst-ref, src-ref, class.
const X224_CC_FIXED_LEN: usize = 6;

pub fn protocol_name(protocol: u32) -> &'static str {
    match protocol {
        PROTOCOL_RDP => "RDP (legacy, no TLS)",
        PROTOCOL_SSL => "SSL/TLS",
        PROTOCOL_HYBRID => "HYBRID (CredSSP/NLA)",
        PROTOCOL_HYBRID_EX => "HYBRID_EX (CredSSP + Early User Auth)",
        _ => "unknown",
    }
}

pub fn failure_name(code: u32) -> &'static str {
    match code {
        1 => "SSL_REQUIRED_BY_SERVER",
        2 => "SSL_NOT_ALLOWED_BY_SERVER",
        3 => "SSL_CERT_NOT_ON_SERVER",
        4 => "INCONSISTENT_FLAGS",
        5 => "HYBRID_REQUIRED_BY_SERVER",
        6 => "SSL_WITH_USER_AUTH_REQUIRED_BY_SERVER",
        _ => "unknown",
    }
}

/// The server's answer to a connection request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum NegotiationOutcome {
    /// The server selected a security protocol.
    Selected {
        protocol: u32,
        name: &'static str,
        flags: u8,
    },
    /// The server refused every protocol offered.
    Failed { code: u32, name: &'static str },
    /// A well-formed confirm carrying no negotiation structure. Older servers answer
    /// this way, and it means standard RDP security — no TLS.
    Absent,
}

impl NegotiationOutcome {
    /// True when the server demands CredSSP, in either form.
    pub fn requires_nla(&self) -> bool {
        match self {
            NegotiationOutcome::Selected { protocol, .. } => {
                *protocol == PROTOCOL_HYBRID || *protocol == PROTOCOL_HYBRID_EX
            }
            NegotiationOutcome::Failed { code, .. } => *code == 5,
            NegotiationOutcome::Absent => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    TooShort {
        need: usize,
        got: usize,
    },
    BadTpktVersion(u8),
    /// The TPKT header declares more bytes than we actually received.
    Truncated {
        declared: usize,
        got: usize,
    },
    NotConnectionConfirm(u8),
    UnknownNegotiationType(u8),
    BadNegotiationLength(u16),
    /// The X.224 length indicator is too small to describe a connection confirm.
    BadX224Length(u8),
    /// The X.224 length indicator and the TPKT length contradict each other.
    InconsistentFraming {
        li: u8,
        declared: usize,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::TooShort { need, got } => {
                write!(f, "response too short: need {need} bytes, got {got}")
            }
            ParseError::BadTpktVersion(v) => write!(f, "bad TPKT version {v:#04x}, expected 0x03"),
            ParseError::Truncated { declared, got } => {
                write!(f, "TPKT declares {declared} bytes, got {got}")
            }
            ParseError::NotConnectionConfirm(c) => {
                write!(f, "not an X.224 connection confirm: code {c:#04x}")
            }
            ParseError::UnknownNegotiationType(t) => {
                write!(f, "unknown negotiation type {t:#04x}")
            }
            ParseError::BadNegotiationLength(l) => {
                write!(f, "negotiation structure declares length {l}, expected 8")
            }
            ParseError::InconsistentFraming { li, declared } => {
                write!(
                    f,
                    "X.224 length indicator {li} overruns the {declared}-byte TPKT length"
                )
            }
            ParseError::BadX224Length(l) => {
                write!(
                    f,
                    "X.224 length indicator {l} is below the {X224_CC_FIXED_LEN}-byte \
                     connection-confirm minimum"
                )
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Build the X.224 connection request that opens every RDP session.
///
/// TPKT header, X.224 CR TPDU, then an RDP negotiation request naming the protocols
/// we are willing to speak.
pub fn connection_request(requested_protocols: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(19);

    // TPKT header: version, reserved, total length (big-endian).
    buf.push(TPKT_VERSION);
    buf.push(0x00);
    buf.extend_from_slice(&19u16.to_be_bytes());

    // X.224 CR TPDU: length indicator, code, dst-ref, src-ref, class.
    buf.push(14); // everything after this byte
    buf.push(X224_CONNECTION_REQUEST);
    buf.extend_from_slice(&0u16.to_be_bytes()); // dst-ref
    buf.extend_from_slice(&0u16.to_be_bytes()); // src-ref
    buf.push(0x00); // class 0

    // RDP_NEG_REQ: type, flags, length (little-endian), requested protocols.
    buf.push(NEG_TYPE_REQUEST);
    buf.push(0x00);
    buf.extend_from_slice(&NEG_STRUCT_LEN.to_le_bytes());
    buf.extend_from_slice(&requested_protocols.to_le_bytes());

    buf
}

/// Parse an X.224 connection confirm and extract the server's negotiation verdict.
pub fn parse_connection_confirm(buf: &[u8]) -> Result<NegotiationOutcome, ParseError> {
    // Minimum: TPKT header (4) + X.224 CC header (7).
    if buf.len() < NEG_OFFSET {
        return Err(ParseError::TooShort {
            need: NEG_OFFSET,
            got: buf.len(),
        });
    }
    if buf[0] != TPKT_VERSION {
        return Err(ParseError::BadTpktVersion(buf[0]));
    }

    let declared = u16::from_be_bytes([buf[2], buf[3]]) as usize;
    if declared > buf.len() {
        return Err(ParseError::Truncated {
            declared,
            got: buf.len(),
        });
    }

    if buf[5] != X224_CONNECTION_CONFIRM {
        return Err(ParseError::NotConnectionConfirm(buf[5]));
    }

    // The X.224 length indicator counts the bytes after itself. A class-0 connection
    // confirm has a 6-byte fixed part; anything beyond that is the variable part, which
    // is where the negotiation structure lives. Deriving the presence of that structure
    // from the length indicator — rather than assuming a fixed offset — is what keeps
    // this correct against a confirm carrying other class-specific fields.
    let li = buf[4] as usize;
    if li < X224_CC_FIXED_LEN {
        return Err(ParseError::BadX224Length(buf[4]));
    }
    // The TPDU occupies buf[4..5+li], so it cannot extend past what TPKT declared.
    // Without this, a confirm claiming li=255 alongside a well-formed 19-byte trailer
    // parses happily while its two length fields flatly contradict each other.
    if TPKT_HEADER_LEN + 1 + li > declared {
        return Err(ParseError::InconsistentFraming {
            li: buf[4],
            declared,
        });
    }
    let variable_len = li - X224_CC_FIXED_LEN;
    if variable_len == 0 {
        return Ok(NegotiationOutcome::Absent);
    }

    // The negotiation structure is fixed at 8 bytes: type, flags, length, payload.
    let end = NEG_OFFSET + NEG_STRUCT_LEN as usize;
    if variable_len < NEG_STRUCT_LEN as usize || declared < end || buf.len() < end {
        return Err(ParseError::TooShort {
            need: end,
            got: declared.min(buf.len()),
        });
    }

    let neg_type = buf[NEG_OFFSET];
    let flags = buf[NEG_OFFSET + 1];
    let neg_len = u16::from_le_bytes([buf[NEG_OFFSET + 2], buf[NEG_OFFSET + 3]]);
    if neg_len != NEG_STRUCT_LEN {
        return Err(ParseError::BadNegotiationLength(neg_len));
    }

    let payload = u32::from_le_bytes([
        buf[NEG_OFFSET + 4],
        buf[NEG_OFFSET + 5],
        buf[NEG_OFFSET + 6],
        buf[NEG_OFFSET + 7],
    ]);

    match neg_type {
        NEG_TYPE_RESPONSE => Ok(NegotiationOutcome::Selected {
            protocol: payload,
            name: protocol_name(payload),
            flags,
        }),
        NEG_TYPE_FAILURE => Ok(NegotiationOutcome::Failed {
            code: payload,
            name: failure_name(payload),
        }),
        other => Err(ParseError::UnknownNegotiationType(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from `temper` (Windows 11 Pro) on 2026-08-14 when offered
    /// TLS | HYBRID | HYBRID_EX. Real wire bytes, not constructed by this code.
    const CONFIRM_HYBRID_EX: [u8; 19] = [
        0x03, 0x00, 0x00, 0x13, 0x0e, 0xd0, 0x00, 0x00, 0x12, 0x34, 0x00, 0x02, 0x2f, 0x08, 0x00,
        0x08, 0x00, 0x00, 0x00,
    ];

    /// Captured from `temper` when offered legacy RDP security only.
    const CONFIRM_FAILURE_HYBRID_REQUIRED: [u8; 19] = [
        0x03, 0x00, 0x00, 0x13, 0x0e, 0xd0, 0x00, 0x00, 0x12, 0x34, 0x00, 0x03, 0x00, 0x08, 0x00,
        0x05, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn request_matches_the_bytes_the_spike_sent() {
        // Byte-for-byte the request that produced the captured responses above.
        let expected: [u8; 19] = [
            0x03, 0x00, 0x00, 0x13, 0x0e, 0xe0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x08,
            0x00, 0x0b, 0x00, 0x00, 0x00,
        ];
        assert_eq!(connection_request(PROTOCOL_ALL_MODERN), expected);
    }

    #[test]
    fn request_encodes_legacy_offer() {
        let req = connection_request(PROTOCOL_RDP);
        assert_eq!(&req[15..19], &[0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn parses_real_hybrid_ex_selection() {
        let outcome = parse_connection_confirm(&CONFIRM_HYBRID_EX).expect("should parse");
        assert_eq!(
            outcome,
            NegotiationOutcome::Selected {
                protocol: PROTOCOL_HYBRID_EX,
                name: "HYBRID_EX (CredSSP + Early User Auth)",
                flags: 0x2f,
            }
        );
        assert!(outcome.requires_nla());
    }

    #[test]
    fn parses_real_hybrid_required_failure() {
        let outcome =
            parse_connection_confirm(&CONFIRM_FAILURE_HYBRID_REQUIRED).expect("should parse");
        assert_eq!(
            outcome,
            NegotiationOutcome::Failed {
                code: 5,
                name: "HYBRID_REQUIRED_BY_SERVER",
            }
        );
        assert!(outcome.requires_nla());
    }

    #[test]
    fn confirm_without_negotiation_structure_is_absent() {
        // An 11-byte confirm: TPKT + X.224 CC and nothing more.
        let buf: [u8; 11] = [
            0x03, 0x00, 0x00, 0x0b, 0x06, 0xd0, 0x00, 0x00, 0x12, 0x34, 0x00,
        ];
        let outcome = parse_connection_confirm(&buf).expect("should parse");
        assert_eq!(outcome, NegotiationOutcome::Absent);
        assert!(!outcome.requires_nla());
    }

    #[test]
    fn rejects_short_input_without_panicking() {
        for len in 0..NEG_OFFSET {
            let buf = vec![0x03u8; len];
            assert!(
                matches!(
                    parse_connection_confirm(&buf),
                    Err(ParseError::TooShort { .. })
                ),
                "length {len} should be rejected as too short"
            );
        }
    }

    #[test]
    fn rejects_bad_tpkt_version() {
        let mut buf = CONFIRM_HYBRID_EX;
        buf[0] = 0x04;
        assert_eq!(
            parse_connection_confirm(&buf),
            Err(ParseError::BadTpktVersion(0x04))
        );
    }

    #[test]
    fn rejects_declared_length_beyond_buffer() {
        let mut buf = CONFIRM_HYBRID_EX;
        buf[3] = 0xff; // claim 255 bytes, supply 19
        assert_eq!(
            parse_connection_confirm(&buf),
            Err(ParseError::Truncated {
                declared: 255,
                got: 19
            })
        );
    }

    #[test]
    fn rejects_non_confirm_tpdu() {
        let mut buf = CONFIRM_HYBRID_EX;
        buf[5] = X224_CONNECTION_REQUEST;
        assert_eq!(
            parse_connection_confirm(&buf),
            Err(ParseError::NotConnectionConfirm(0xe0))
        );
    }

    #[test]
    fn rejects_unknown_negotiation_type() {
        let mut buf = CONFIRM_HYBRID_EX;
        buf[NEG_OFFSET] = 0x09;
        assert_eq!(
            parse_connection_confirm(&buf),
            Err(ParseError::UnknownNegotiationType(0x09))
        );
    }

    #[test]
    fn rejects_length_indicator_that_overruns_the_tpkt_length() {
        // A confirm whose two length fields contradict each other: li claims 255 bytes
        // follow, TPKT declares 19 in total. Previously this parsed as a normal
        // selection, silently ignoring the inconsistency.
        let mut buf = CONFIRM_HYBRID_EX;
        buf[4] = 0xff;
        assert_eq!(
            parse_connection_confirm(&buf),
            Err(ParseError::InconsistentFraming {
                li: 0xff,
                declared: 19
            })
        );
    }

    #[test]
    fn rejects_wrong_negotiation_length() {
        let mut buf = CONFIRM_HYBRID_EX;
        buf[NEG_OFFSET + 2] = 0x10; // declare 16 instead of 8
        assert_eq!(
            parse_connection_confirm(&buf),
            Err(ParseError::BadNegotiationLength(16))
        );
    }

    #[test]
    fn never_panics_on_arbitrary_input() {
        // Walk a byte through every position of an otherwise-valid response.
        for pos in 0..CONFIRM_HYBRID_EX.len() {
            for byte in [0x00u8, 0x01, 0x7f, 0x80, 0xff] {
                let mut buf = CONFIRM_HYBRID_EX;
                buf[pos] = byte;
                let _ = parse_connection_confirm(&buf);
            }
        }
    }
}
