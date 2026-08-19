//! Why a session ended, in words for the person who lost it.
//!
//! Two routes in. A Windows host that is shutting down, rebooting, or handing your
//! session to somebody else says so first, in a Set Error Info PDU ([MS-RDPBCGR]
//! 2.2.5.1.1) — [`classify`] turns that code into a sentence. Everything else that
//! kills a live session arrives as a [`ConnectError`], and [`LostSession`] turns that
//! into one too.
//!
//! Both exist for the same reason: the machine-readable statement of *why* is worth
//! translating rather than dropping on the floor — or, worse, showing raw.
//!
//! [MS-RDPBCGR]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/a21a1bd9-2303-49c1-90ec-3932435c248c

use crate::connect::ConnectError;
use ironrdp::pdu::rdp::server_error_info::{ErrorInfo, ProtocolIndependentCode};
use ironrdp::session::GracefulDisconnectReason;

/// What the server said on its way out, ready for the end dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerFarewell {
    /// Completes `"<host> …"` in the dialog headline — "Kiln is restarting".
    pub headline: &'static str,
    /// The sentence underneath: ours for the cases we recognise, the protocol's own
    /// words otherwise.
    pub detail: String,
}

/// Turn a graceful-disconnect reason into something to show the user.
///
/// `None` for the reasons that carry no server explanation — a user-initiated
/// disconnect and a bare MCS teardown are the ordinary end of a session, and the
/// caller keeps treating those as a clean close.
pub fn classify(reason: &GracefulDisconnectReason) -> Option<ServerFarewell> {
    let GracefulDisconnectReason::ErrorInfo(info) = reason else {
        return None;
    };
    let (headline, detail) = match info {
        ErrorInfo::ProtocolIndependentCode(ProtocolIndependentCode::ServerReboot) => (
            "is restarting",
            "The host is rebooting. It will take connections again once it is back.".to_owned(),
        ),
        ErrorInfo::ProtocolIndependentCode(ProtocolIndependentCode::ServerShutdown) => (
            "is shutting down",
            "The host is powering off. Reconnecting will only work once it is on again.".to_owned(),
        ),
        // The commonest ending of all, and the spec's own sentence for it — "The
        // disconnection was initiated by the user logging off his or her session on
        // the server" — is three lines of dialog saying one thing.
        ErrorInfo::ProtocolIndependentCode(ProtocolIndependentCode::LogoffByUser) => (
            "ended the session",
            "User logged out of their session on the server".to_owned(),
        ),
        // Everything else keeps the protocol's own description. It is written for a
        // reader ("Another user connected to the server, forcing the disconnection of
        // the current connection") and there is nothing we can add to it.
        other => ("ended the session", plain_description(*other)),
    };
    Some(ServerFarewell { headline, detail })
}

/// The protocol's own sentence, without the error-class label `ErrorInfo::description`
/// prepends. "[Protocol independent error]" tells the reader which table of MS-RDPBCGR
/// 2.2.5.1.1 the code came from, which is a fact about the specification rather than
/// about their session.
fn plain_description(info: ErrorInfo) -> String {
    match info {
        ErrorInfo::ProtocolIndependentCode(c) => c.description().to_owned(),
        ErrorInfo::ProtocolIndependentLicensingCode(c) => c.description().to_owned(),
        ErrorInfo::ProtocolIndependentConnectionBrokerCode(c) => c.description().to_owned(),
        ErrorInfo::RdpSpecificCode(c) => c.description().to_owned(),
        // No sentence exists for a code we cannot name, so the number is all there is.
        ErrorInfo::Unknown(_) => info.description(),
    }
}

/// A session that died under us, split into what the user is told and what an engineer
/// needs (MDR-BUG-FLUX-00014).
///
/// The raw failure is IronRDP's nested error chain: a message per layer, each wrapped
/// in the trait path and source location it passed through. That is written for us. A
/// person who has just lost their desktop needs one sentence saying what happened and
/// whether reconnecting will help — with the chain still reachable, because it is what
/// a bug report is made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LostSession {
    /// The sentence under the "Session to `<host>` lost" headline.
    pub detail: String,
    /// The failure as engineering reads it, behind the dialog's Technical details
    /// disclosure. Call sites stripped ([`strip_call_sites`]); the untouched original
    /// still goes to stderr with the rest of the exit summary.
    pub technical: String,
}

impl LostSession {
    /// Classify an RDP-side failure.
    ///
    /// The sentences are deliberately keyed off the error's own variant rather than
    /// its text: matching on wording would break silently the next time IronRDP or the
    /// OS rephrases something, and the fallback here has to stay honest.
    pub fn from_connect_error(error: &ConnectError) -> Self {
        use std::io::ErrorKind;
        // "Still alive on the host" is the useful half of the message: an RDP session
        // outlives the client that was showing it, so reconnecting resumes the desktop
        // rather than starting a new one. It is only said where it is true.
        const RESUMES: &str = "The desktop is probably still alive on the host — \
                               reconnecting resumes it.";
        let detail = match error {
            ConnectError::Io(e) => match e.kind() {
                ErrorKind::TimedOut => format!("The host stopped answering. {RESUMES}"),
                ErrorKind::ConnectionReset
                | ErrorKind::ConnectionAborted
                | ErrorKind::NotConnected
                | ErrorKind::BrokenPipe
                | ErrorKind::UnexpectedEof => {
                    format!("The host closed the connection. {RESUMES}")
                }
                ErrorKind::ConnectionRefused => "The host refused the connection. Check that \
                     Remote Desktop is still enabled on it."
                    .to_owned(),
                _ => format!("The network connection to the host failed. {RESUMES}"),
            },
            ConnectError::Tls(_) => {
                format!("The encrypted channel to the host failed. {RESUMES}")
            }
            // A trust failure is the one case where reconnecting is the wrong advice:
            // it will fail the same way until the certificate is dealt with.
            ConnectError::Trust(_) => "The host's identity could not be verified, so the \
                 session was stopped."
                .to_owned(),
            ConnectError::NoPeerCertificate => "The host offered no certificate, so the \
                 session was stopped."
                .to_owned(),
            // Everything IronRDP rejects lands here — a PDU that would not parse, a
            // stage that refused. We cannot say which without reading the chain, and
            // the chain is one click away.
            ConnectError::Protocol(_) => {
                format!("The host sent something this client could not handle. {RESUMES}")
            }
        };
        LostSession {
            detail,
            technical: strip_call_sites(&error.to_string()),
        }
    }

    /// A native (rhydra) transport failure. Its own sentence: the tunnel is ours, not
    /// the protocol's, and calling it an RDP error sends the reader the wrong way.
    pub fn from_transport(reason: &str) -> Self {
        LostSession {
            detail: "The native transport to the host failed, so the session stopped.".to_owned(),
            technical: strip_call_sites(&format!("native transport: {reason}")),
        }
    }
}

/// Drop IronRDP's call-site brackets from an error chain, keeping the messages.
///
/// `ironrdp-error` wraps every layer as `[<context> @ <file>:<line>]`, so a routine
/// decode failure reads
/// `RDP connection failed: [payload error @ /rustc/<hash>/library/core/src/ops/function.rs:250]
/// PDU error: [<ironrdp_egfx::client::…>::process::{{closure}} @ …] decode error: […]
/// invalid `Type`: Unknown GFX PDU type` — 450 characters of which one phrase carries
/// the meaning, and two of the paths name the machine that compiled the binary rather
/// than anything on the user's. Dropping only the bracketed groups that carry a `@`
/// location leaves the real chain: `RDP connection failed: PDU error: decode error:
/// invalid `Type`: Unknown GFX PDU type`.
///
/// A bracket without a location is left alone — it is somebody's message, not a call
/// site — and so is a chain the stripping would empty, so an upstream format change
/// degrades to showing the original rather than to showing nothing.
pub fn strip_call_sites(chain: &str) -> String {
    let mut out = String::with_capacity(chain.len());
    let mut rest = chain;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find(']').map(|i| open + i) else {
            break; // Unbalanced: nothing more can be safely recognised.
        };
        if !rest[open..close].contains(" @ ") {
            out.push_str(&rest[..=close]);
            rest = &rest[close + 1..];
            continue;
        }
        out.push_str(&rest[..open]);
        // The bracket group is followed by a space before the next message; drop it
        // too, or the chain reads "failed:  PDU error".
        let tail = &rest[close + 1..];
        rest = tail.strip_prefix(' ').unwrap_or(tail);
    }
    out.push_str(rest);
    let trimmed = out.trim().trim_end_matches(':').trim().to_owned();
    if trimmed.is_empty() {
        chain.to_owned()
    } else {
        trimmed
    }
}

#[cfg(test)]
// Not private: the end dialog's own tests render this module's classification of the
// real 2026-08-19 failure, and one fixture beats two copies drifting apart.
pub(crate) mod tests {
    use super::*;
    use ironrdp::core::{Decode as _, ReadCursor};
    use ironrdp::pdu::rdp::server_error_info::ServerSetErrorInfoPdu;

    /// Decode the four bytes a Set Error Info PDU carries, exactly as they arrive.
    fn wire(code: u32) -> ErrorInfo {
        let bytes = code.to_le_bytes();
        let mut cursor = ReadCursor::new(&bytes);
        ServerSetErrorInfoPdu::decode(&mut cursor)
            .expect("a Set Error Info PDU must decode")
            .0
    }

    /// The bug behind the "unexpected info code" decode failure: a host that is on its
    /// way down says so with 0x19/0x1A, and IronRDP's table stopped at 0x18.
    #[test]
    fn a_restarting_host_decodes_instead_of_failing_the_pdu() {
        assert_eq!(
            wire(0x0000_001A),
            ErrorInfo::ProtocolIndependentCode(ProtocolIndependentCode::ServerReboot)
        );
        assert_eq!(
            wire(0x0000_0019),
            ErrorInfo::ProtocolIndependentCode(ProtocolIndependentCode::ServerShutdown)
        );
    }

    /// The same failure must not come back the next time Microsoft adds a code. An
    /// unknown one is carried verbatim, not rejected.
    #[test]
    fn an_unknown_code_is_carried_rather_than_rejected() {
        assert_eq!(wire(0x0000_00FE), ErrorInfo::Unknown(0x0000_00FE));
        assert!(wire(0x0000_00FE).description().contains("0x000000FE"));
    }

    #[test]
    fn a_reboot_is_reported_as_a_restart_not_as_an_error() {
        let farewell = classify(&GracefulDisconnectReason::ErrorInfo(wire(0x0000_001A)))
            .expect("a reboot is a server farewell");
        assert_eq!(farewell.headline, "is restarting");
        assert!(farewell.detail.contains("rebooting"), "{farewell:?}");
    }

    #[test]
    fn a_shutdown_says_the_host_is_going_away() {
        let farewell = classify(&GracefulDisconnectReason::ErrorInfo(wire(0x0000_0019)))
            .expect("a shutdown is a server farewell");
        assert_eq!(farewell.headline, "is shutting down");
        assert!(farewell.detail.contains("powering off"), "{farewell:?}");
    }

    /// Being kicked off keeps the protocol's own wording — it explains itself better
    /// than a headline can.
    #[test]
    fn another_code_keeps_the_protocols_own_words() {
        let farewell = classify(&GracefulDisconnectReason::ErrorInfo(wire(0x0000_0005)))
            .expect("a kick is a server farewell");
        assert_eq!(farewell.headline, "ended the session");
        assert!(
            farewell.detail.contains("Another user connected"),
            "{farewell:?}"
        );
        // …but not the label naming the spec table the code came from. The reader is
        // being told why their session ended, not which of MS-RDPBCGR's four code
        // tables Microsoft filed the reason under.
        assert!(
            !farewell.detail.contains("[Protocol independent error]"),
            "{farewell:?}"
        );
    }

    /// The commonest ending of all. The spec's own sentence — "The disconnection was
    /// initiated by the user logging off his or her session on the server" — is three
    /// wrapped dialog lines saying one thing, so this is the one code we reword.
    #[test]
    fn a_user_logoff_is_said_in_one_short_line() {
        let farewell = classify(&GracefulDisconnectReason::ErrorInfo(wire(0x0000_000C)))
            .expect("a logoff is a server farewell");
        assert_eq!(farewell.headline, "ended the session");
        assert_eq!(
            farewell.detail,
            "User logged out of their session on the server"
        );
    }

    /// A code with no sentence has only its number, and dropping the class label must
    /// not drop that too — leaving the detail line blank.
    #[test]
    fn an_unnameable_code_still_shows_its_number() {
        let farewell = classify(&GracefulDisconnectReason::ErrorInfo(wire(0x0000_00FE)))
            .expect("an unknown code is still a farewell");
        assert!(farewell.detail.contains("0x000000FE"), "{farewell:?}");
    }

    /// Closing the window is not a server farewell, so the ordinary clean-close path
    /// must stay exactly as it was.
    #[test]
    fn an_ordinary_disconnect_carries_no_farewell() {
        assert_eq!(classify(&GracefulDisconnectReason::UserInitiated), None);
        assert_eq!(classify(&GracefulDisconnectReason::ServerInitiated), None);
    }

    /// Verbatim from Arthur's 2026-08-19 session, as `connect::describe` hands it to
    /// `ConnectError::Protocol` — whose `Display` prefixes the "RDP connection failed:"
    /// the dialog showed. Together they were the whole user-facing explanation.
    pub(crate) const REAL_DECODE_CHAIN: &str = "[payload error @ \
        /rustc/ac68faa20c58cbccd01ee7208bf3b6e93a7d7f96/library/core/src/ops/function.rs:250] \
        PDU error: [<ironrdp_egfx::client::GraphicsPipelineClient as \
        ironrdp_dvc::DvcProcessor>::process::{{closure}} @ \
        vendor/ironrdp-egfx/src/client.rs:1230] decode error: [<ironrdp_egfx::pdu::cmd::GfxPdu \
        as ironrdp_core::decode::Decode<'_>>::decode @ \
        ~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/ironrdp-core-0.2.1/src/error.rs:101] \
        invalid `Type`: Unknown GFX PDU type";

    /// The failure as the live path builds it, so the tests below run over the string
    /// the user actually saw rather than a hand-assembled lookalike.
    pub(crate) fn real_decode_failure() -> ConnectError {
        ConnectError::Protocol(REAL_DECODE_CHAIN.to_owned())
    }

    #[test]
    fn the_call_site_brackets_go_and_the_message_chain_stays() {
        let stripped = strip_call_sites(&real_decode_failure().to_string());
        assert_eq!(
            stripped,
            "RDP connection failed: PDU error: decode error: invalid `Type`: \
             Unknown GFX PDU type"
        );
    }

    /// The half of the point that is about the reader, not the length: none of what is
    /// left names the machine that compiled the binary.
    #[test]
    fn no_build_machine_path_survives_the_strip() {
        let stripped = strip_call_sites(&real_decode_failure().to_string());
        for leak in ["/Users/", "/rustc/", ".cargo/registry", " @ ", "vendor/"] {
            assert!(
                !stripped.contains(leak),
                "{leak:?} survived in {stripped:?}"
            );
        }
    }

    /// A bracket that is somebody's message rather than a call site has no location in
    /// it, and must survive untouched.
    #[test]
    fn a_bracket_without_a_location_is_left_alone() {
        assert_eq!(
            strip_call_sites("clipboard failed: format [13] is not registered"),
            "clipboard failed: format [13] is not registered"
        );
    }

    /// If IronRDP ever changes the format such that stripping would leave nothing, the
    /// user gets the ugly original rather than an empty dialog.
    #[test]
    fn a_chain_that_is_all_call_sites_falls_back_to_the_original() {
        let all_sites = "[a @ x.rs:1] [b @ y.rs:2]";
        assert_eq!(strip_call_sites(all_sites), all_sites);
    }

    /// The classification keys off the variant, so a protocol rejection is never
    /// described as a dropped connection — the wrong sentence this replaced.
    #[test]
    fn a_protocol_rejection_is_not_called_a_dropped_connection() {
        let lost = LostSession::from_connect_error(&real_decode_failure());
        assert!(
            lost.detail
                .starts_with("The host sent something this client could not handle."),
            "{lost:?}"
        );
        assert!(lost.detail.contains("reconnecting resumes it"), "{lost:?}");
        assert!(!lost.detail.contains('['), "{lost:?}");
        assert!(lost.technical.ends_with("Unknown GFX PDU type"), "{lost:?}");
    }

    /// Each io kind we name gets its own sentence, and the tail of the enum still gets
    /// a true one rather than falling through to whichever arm came first.
    #[test]
    fn every_io_failure_gets_the_sentence_that_fits_it() {
        use std::io::{Error, ErrorKind};
        let sentence = |kind: ErrorKind| {
            LostSession::from_connect_error(&ConnectError::Io(Error::new(kind, "boom"))).detail
        };
        assert!(sentence(ErrorKind::TimedOut).starts_with("The host stopped answering."));
        assert!(
            sentence(ErrorKind::ConnectionReset).starts_with("The host closed the connection.")
        );
        assert!(
            sentence(ErrorKind::ConnectionRefused).starts_with("The host refused the connection.")
        );
        // Nothing to say about a resume for a refusal: there is no session to resume.
        assert!(!sentence(ErrorKind::ConnectionRefused).contains("reconnecting resumes"));
        assert!(
            sentence(ErrorKind::OutOfMemory)
                .starts_with("The network connection to the host failed.")
        );
    }

    /// A certificate the client would not accept is the one failure where "reconnect"
    /// is bad advice — it will fail identically until somebody deals with the cert.
    #[test]
    fn a_trust_failure_does_not_promise_that_reconnecting_helps() {
        let lost = LostSession::from_connect_error(&ConnectError::Trust("bad chain".to_owned()));
        assert!(!lost.detail.contains("reconnect"), "{lost:?}");
        assert!(lost.technical.contains("bad chain"), "{lost:?}");
    }

    /// A native-transport failure says "transport", not "RDP": the tunnel is ours.
    #[test]
    fn a_transport_failure_is_not_dressed_as_an_rdp_error() {
        let lost = LostSession::from_transport("video read: connection reset");
        assert!(lost.detail.contains("native transport"), "{lost:?}");
        assert!(!lost.detail.contains("RDP"), "{lost:?}");
        assert!(lost.technical.contains("video read"), "{lost:?}");
    }
}
