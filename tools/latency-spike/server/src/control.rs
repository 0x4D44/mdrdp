//! The session agent's loopback control protocol.
//!
//! One JSON object per line in each direction, on 127.0.0.1 only — SSH is the
//! security boundary, exactly as for the video and input ports. The agent serves
//! one client at a time with a short read timeout, so a connected-but-silent
//! client cannot wedge the listener.
//!
//! `shutdown` is the sanctioned way to stop the agent: it kills the supervised
//! children before exiting. Ending the scheduled task instead (`schtasks /end`)
//! provably orphans them (2026-08-18 post-mortem), leaving the device and ports
//! held by processes nothing supervises.

use serde::{Deserialize, Serialize};

/// Where the agent's control listener binds, next to video (9500) and input (9501).
pub const CONTROL_PORT: u16 = 9502;

/// Bumped on any incompatible change to the request or response shapes.
pub const SCHEMA: u32 = 1;

/// A parsed control request: `{"cmd":"status"}` and friends.
///
/// Fields other than `cmd` are ignored, deliberately: a future client may add
/// arguments an older agent does not know, and the command name alone decides
/// what happens. (serde's `deny_unknown_fields` is a no-op on internally tagged
/// enums anyway — the tolerance is documented rather than accidental.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Request {
    /// Report the reconcile loop's view of the stack.
    Status,
    /// Kill the capture server; supervision respawns it.
    RestartServer,
    /// Kill all children and exit the agent.
    Shutdown,
}

/// Parse one request line. The error string is sent back to the client verbatim,
/// so it names what was wrong rather than echoing serde internals wholesale.
pub fn parse_request(line: &str) -> Result<Request, String> {
    serde_json::from_str(line).map_err(|e| format!("unrecognised request: {e}"))
}

/// One supervised child, as the status report describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildReport {
    pub running: bool,
    /// Respawns after the first start — 0 means it has never died.
    pub restarts: u32,
    /// Exit code of the most recent death, if any.
    pub last_exit_code: Option<i32>,
    /// Seconds until the next respawn attempt (0 when running or due now).
    pub cooldown_s: u32,
}

/// A display mode, as reported. Distinct from the reconciler's internal type so
/// the wire shape cannot drift by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeReport {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
}

/// The agent's whole view, one JSON object. Designed to distinguish every failure
/// mode the 2026-08-18 wedge post-mortem catalogued: creator dead vs device absent
/// vs wrong mode vs server dead vs server crash-looping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusReport {
    pub schema: u32,
    pub uptime_s: u64,
    pub creator: ChildReport,
    pub device_present: bool,
    /// The mode the display actually has right now, if it could be read.
    pub display_mode: Option<ModeReport>,
    /// Whether that mode matches the desired one.
    pub mode_ok: bool,
    pub server: ChildReport,
    /// The first unsatisfied step in bring-up order, or absent when green.
    pub stuck: Option<String>,
}

#[derive(Serialize)]
struct StatusEnvelope<'a> {
    ok: bool,
    schema: u32,
    status: &'a StatusReport,
}

#[derive(Serialize)]
struct AckEnvelope<'a> {
    ok: bool,
    schema: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

/// The response line for a `status` request.
pub fn status_line(report: &StatusReport) -> String {
    serde_json::to_string(&StatusEnvelope {
        ok: true,
        schema: SCHEMA,
        status: report,
    })
    .expect("a StatusReport always serialises")
}

/// The response line for an acknowledged command.
pub fn ok_line() -> String {
    serde_json::to_string(&AckEnvelope {
        ok: true,
        schema: SCHEMA,
        error: None,
    })
    .expect("an ack always serialises")
}

/// The response line for a refused or unparseable request. The connection
/// survives — one bad line is not a reason to hang up on the operator.
pub fn error_line(message: &str) -> String {
    serde_json::to_string(&AckEnvelope {
        ok: false,
        schema: SCHEMA,
        error: Some(message),
    })
    .expect("an error always serialises")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_command_parses() {
        assert_eq!(parse_request(r#"{"cmd":"status"}"#), Ok(Request::Status));
        assert_eq!(
            parse_request(r#"{"cmd":"restart-server"}"#),
            Ok(Request::RestartServer)
        );
        assert_eq!(
            parse_request(r#"{"cmd":"shutdown"}"#),
            Ok(Request::Shutdown)
        );
    }

    #[test]
    fn unknown_command_and_garbage_are_errors_not_panics() {
        assert!(parse_request(r#"{"cmd":"reboot-the-host"}"#).is_err());
        assert!(parse_request("not json at all").is_err());
    }

    #[test]
    fn extra_fields_are_tolerated_by_contract() {
        // The command name alone decides; unknown arguments from a newer client
        // are ignored rather than refused.
        assert_eq!(
            parse_request(r#"{"cmd":"status","force":true}"#),
            Ok(Request::Status)
        );
    }

    /// Every field distinct, so a swapped pair cannot round-trip unnoticed.
    fn distinct_report() -> StatusReport {
        StatusReport {
            schema: SCHEMA,
            uptime_s: 101,
            creator: ChildReport {
                running: true,
                restarts: 3,
                last_exit_code: Some(7),
                cooldown_s: 0,
            },
            device_present: true,
            display_mode: Some(ModeReport {
                width: 1920,
                height: 1080,
                hz: 240,
            }),
            mode_ok: false,
            server: ChildReport {
                running: false,
                restarts: 5,
                last_exit_code: Some(9),
                cooldown_s: 4,
            },
            stuck: Some("display-mode".to_owned()),
        }
    }

    #[test]
    fn status_round_trips_with_distinct_fields() {
        let report = distinct_report();
        let line = status_line(&report);
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["schema"], SCHEMA);
        let back: StatusReport = serde_json::from_value(value["status"].clone()).unwrap();
        assert_eq!(back, report);
        // Spot-check the two ChildReports landed on the right children.
        assert_eq!(value["status"]["creator"]["restarts"], 3);
        assert_eq!(value["status"]["server"]["restarts"], 5);
        assert_eq!(value["status"]["server"]["cooldown_s"], 4);
    }

    #[test]
    fn ack_and_error_lines_carry_ok_and_schema() {
        let ok: serde_json::Value = serde_json::from_str(&ok_line()).unwrap();
        assert_eq!(ok["ok"], true);
        assert_eq!(ok["schema"], SCHEMA);
        let err: serde_json::Value =
            serde_json::from_str(&error_line("unrecognised request")).unwrap();
        assert_eq!(err["ok"], false);
        assert_eq!(err["error"], "unrecognised request");
    }
}
