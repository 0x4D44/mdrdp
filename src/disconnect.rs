//! Reading the server's parting message.
//!
//! A Windows host that is shutting down, rebooting, or handing your session to
//! somebody else says so first, in a Set Error Info PDU ([MS-RDPBCGR] 2.2.5.1.1).
//! The code is the only machine-readable statement of *why* a session ended, so it
//! is worth turning into a sentence rather than dropping on the floor.
//!
//! [MS-RDPBCGR]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/a21a1bd9-2303-49c1-90ec-3932435c248c

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
        // Everything else keeps the protocol's own description. It is written for a
        // reader ("Another user connected to the server, forcing the disconnection of
        // the current connection") and there is nothing we can add to it.
        other => ("ended the session", other.description()),
    };
    Some(ServerFarewell { headline, detail })
}

#[cfg(test)]
mod tests {
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
    }

    /// Closing the window is not a server farewell, so the ordinary clean-close path
    /// must stay exactly as it was.
    #[test]
    fn an_ordinary_disconnect_carries_no_farewell() {
        assert_eq!(classify(&GracefulDisconnectReason::UserInitiated), None);
        assert_eq!(classify(&GracefulDisconnectReason::ServerInitiated), None);
    }
}
