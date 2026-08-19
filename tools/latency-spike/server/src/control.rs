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
/// 2: `StatusReport.version` (defaulted on read, so a 2-client reads a 1-agent).
pub const SCHEMA: u32 = 2;

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
    /// The agent's crate version. Defaulted (empty) when reading a pre-schema-2
    /// agent, which is itself the signal a deploy verify needs: "" never equals
    /// the version just deployed.
    #[serde(default)]
    pub version: String,
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

/// Whether one sample reads fully green: everything present, right mode, server
/// up, no stuck step. One green sample is necessary but NOT sufficient — a
/// crash-looping server shows one green tick per cycle (spawn sets `running`
/// optimistically). Health claims go through [`stable_green`].
pub fn green(r: &StatusReport) -> bool {
    r.device_present && r.mode_ok && r.server.running && r.stuck.is_none()
}

/// The two-sample health rule: both samples green, taken far enough apart that a
/// crash cycle would show, with the server's death counters unchanged between
/// them. The *caller* owes the ≥3 s spacing; this predicate owes the counters.
pub fn stable_green(first: &StatusReport, second: &StatusReport) -> bool {
    green(first)
        && green(second)
        && first.server.restarts == second.server.restarts
        && first.server.last_exit_code == second.server.last_exit_code
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

/// Why a status query failed: the port not answering is a different fact (no
/// agent) from an answer that could not be understood (wrong peer, wire skew).
#[derive(Debug, PartialEq, Eq)]
pub enum QueryError {
    /// Connect/read failed — nothing is listening, or it hung up.
    NoAnswer(String),
    /// Something answered, but not with a status envelope we understand.
    Bad(String),
}

/// One blocking status query against an agent control port.
///
/// `timeout` bounds both the TCP connect and the read of the reply line — the read
/// timeout is derived from it rather than the previous hardcoded 5 s, which could
/// alone exceed a caller's overall budget (HLD tranche 3 §4.3/review S-M3: the
/// native-connect probe runs its whole four-step sequence under one 8 s deadline).
pub fn query_status(
    addr: (&str, u16),
    timeout: std::time::Duration,
) -> Result<StatusReport, QueryError> {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpStream, ToSocketAddrs};

    let sock_addr = addr
        .to_socket_addrs()
        .map_err(|e| QueryError::NoAnswer(format!("resolve {}:{}: {e}", addr.0, addr.1)))?
        .next()
        .ok_or_else(|| {
            QueryError::NoAnswer(format!("resolve {}:{}: no addresses", addr.0, addr.1))
        })?;
    let stream = TcpStream::connect_timeout(&sock_addr, timeout)
        .map_err(|e| QueryError::NoAnswer(format!("connect {}:{}: {e}", addr.0, addr.1)))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| QueryError::NoAnswer(e.to_string()))?;
    let mut writer = stream
        .try_clone()
        .map_err(|e| QueryError::NoAnswer(e.to_string()))?;
    writeln!(writer, r#"{{"cmd":"status"}}"#).map_err(|e| QueryError::NoAnswer(e.to_string()))?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|e| QueryError::NoAnswer(format!("read: {e}")))?;
    if line.trim().is_empty() {
        return Err(QueryError::NoAnswer("empty reply".to_owned()));
    }
    let value: serde_json::Value =
        serde_json::from_str(&line).map_err(|e| QueryError::Bad(format!("not JSON: {e}")))?;
    if value["ok"] != true {
        return Err(QueryError::Bad(format!("refused: {}", line.trim())));
    }
    serde_json::from_value(value["status"].clone())
        .map_err(|e| QueryError::Bad(format!("bad status shape: {e}")))
}

/// The outcome of waiting for stable green.
#[derive(Debug)]
pub enum WaitOutcome {
    /// The two-sample rule passed; here is the second sample.
    StableGreen(Box<StatusReport>),
    /// Deadline hit while the agent answered but never went (stably) green.
    NotGreen(Box<StatusReport>),
    /// Deadline hit with the port never usefully answering.
    NoAnswer(String),
}

/// Poll until [`stable_green`] passes or `deadline` runs out. `spacing` is the
/// gap between the two samples of the health rule (production passes ~3 s; tests
/// shrink it — the rule's power comes from the server's counters, the spacing
/// only has to exceed a crash-cycle's green window).
pub fn wait_stable_green(
    addr: (&str, u16),
    deadline: std::time::Duration,
    spacing: std::time::Duration,
    query_timeout: std::time::Duration,
) -> WaitOutcome {
    let start = std::time::Instant::now();
    loop {
        let latest = match query_status(addr, query_timeout) {
            Ok(first) if green(&first) => {
                std::thread::sleep(spacing);
                match query_status(addr, query_timeout) {
                    Ok(second) => {
                        if stable_green(&first, &second) {
                            return WaitOutcome::StableGreen(Box::new(second));
                        }
                        Ok(second)
                    }
                    // The port vanished between samples; report the green we had —
                    // the deadline arm below will surface it as not-stable.
                    Err(_) => Ok(first),
                }
            }
            other => other,
        };
        if start.elapsed() >= deadline {
            return match latest {
                Ok(report) => WaitOutcome::NotGreen(Box::new(report)),
                Err(QueryError::NoAnswer(e)) | Err(QueryError::Bad(e)) => WaitOutcome::NoAnswer(e),
            };
        }
        std::thread::sleep(spacing.min(std::time::Duration::from_secs(1)));
    }
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
            version: "9.9.9".to_owned(),
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
    fn a_schema_1_line_without_version_still_parses() {
        // A 2-client reading a 1-agent: version defaults to empty, which is the
        // "old agent" signal, never a parse failure.
        let mut old = serde_json::to_value(distinct_report()).unwrap();
        old.as_object_mut().unwrap().remove("version");
        let back: StatusReport = serde_json::from_value(old).unwrap();
        assert_eq!(back.version, "");
    }

    fn green_report() -> StatusReport {
        StatusReport {
            schema: SCHEMA,
            version: "9.9.9".to_owned(),
            uptime_s: 60,
            creator: ChildReport {
                running: true,
                restarts: 0,
                last_exit_code: None,
                cooldown_s: 0,
            },
            device_present: true,
            display_mode: Some(ModeReport {
                width: 1920,
                height: 1080,
                hz: 240,
            }),
            mode_ok: true,
            server: ChildReport {
                running: true,
                restarts: 0,
                last_exit_code: None,
                cooldown_s: 0,
            },
            stuck: None,
        }
    }

    #[test]
    fn stable_green_accepts_two_quiet_samples() {
        let a = green_report();
        let mut b = green_report();
        b.uptime_s = 70; // time passing alone must not break stability
        assert!(stable_green(&a, &b));
    }

    #[test]
    fn stable_green_rejects_a_crash_loop() {
        // The ops-review scenario: each crash cycle shows one green tick, but the
        // restart counter moves between samples.
        let a = green_report();
        let mut b = green_report();
        b.server.restarts = a.server.restarts + 1;
        assert!(!stable_green(&a, &b));

        // A death and clean respawn inside the window also shows in the exit code.
        let mut c = green_report();
        c.server.last_exit_code = Some(1);
        assert!(!stable_green(&a, &c));
    }

    #[test]
    fn stable_green_requires_green_on_both_ends() {
        let a = green_report();
        let mut not_yet = green_report();
        not_yet.server.running = false;
        not_yet.stuck = Some("server".to_owned());
        assert!(!stable_green(&not_yet, &a));
        assert!(!stable_green(&a, &not_yet));
        assert!(!green(&not_yet));
    }

    /// Serve one scripted status line per incoming connection, in order, then
    /// keep serving the last one. Returns the bound port.
    fn scripted_agent(lines: Vec<String>) -> u16 {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut served = 0usize;
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let mut writer = stream.try_clone().unwrap();
                let mut request = String::new();
                let _ = BufReader::new(stream).read_line(&mut request);
                let line = &lines[served.min(lines.len() - 1)];
                let _ = writeln!(writer, "{line}");
                served += 1;
            }
        });
        port
    }

    #[test]
    fn query_status_reads_a_real_socket() {
        use std::time::Duration;
        let port = scripted_agent(vec![status_line(&distinct_report())]);
        let report = query_status(("127.0.0.1", port), Duration::from_secs(2)).unwrap();
        assert_eq!(report, distinct_report());
    }

    #[test]
    fn query_status_distinguishes_no_answer_from_bad_answer() {
        use std::time::Duration;
        // Nothing listening: NoAnswer.
        let unused = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = unused.local_addr().unwrap().port();
        drop(unused);
        assert!(matches!(
            query_status(("127.0.0.1", port), Duration::from_secs(2)),
            Err(QueryError::NoAnswer(_))
        ));
        // Something answering garbage: Bad.
        let port = scripted_agent(vec!["not json".to_owned()]);
        assert!(matches!(
            query_status(("127.0.0.1", port), Duration::from_secs(2)),
            Err(QueryError::Bad(_))
        ));
    }

    #[test]
    fn query_status_honours_its_timeout_against_a_silent_peer() {
        use std::time::{Duration, Instant};
        // Accepts the connection, then never writes a reply line: the read timeout,
        // not the connect timeout, has to be the one that fires. A short timeout
        // here proves the parameter actually reaches `set_read_timeout` — the old
        // hardcoded-5s version would hang this test for 5 real seconds instead.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let _kept_alive = listener.accept();
            std::thread::sleep(Duration::from_secs(5));
        });
        let start = Instant::now();
        let result = query_status(("127.0.0.1", port), Duration::from_millis(150));
        assert!(matches!(result, Err(QueryError::NoAnswer(_))));
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "query_status took {:?}, expected it to time out near 150ms",
            start.elapsed()
        );
    }

    #[test]
    fn wait_stable_green_passes_a_stable_agent_and_fails_a_crash_loop() {
        use std::time::Duration;
        // Stable: same green twice.
        let port = scripted_agent(vec![status_line(&green_report())]);
        assert!(matches!(
            wait_stable_green(
                ("127.0.0.1", port),
                Duration::from_secs(2),
                Duration::from_millis(30),
                Duration::from_secs(2),
            ),
            WaitOutcome::StableGreen(_)
        ));
        // Crash loop: every sample green but restarts always climbing.
        let looping: Vec<String> = (0..200)
            .map(|i| {
                let mut r = green_report();
                r.server.restarts = i;
                status_line(&r)
            })
            .collect();
        let port = scripted_agent(looping);
        assert!(matches!(
            wait_stable_green(
                ("127.0.0.1", port),
                Duration::from_millis(300),
                Duration::from_millis(20),
                Duration::from_secs(2),
            ),
            WaitOutcome::NotGreen(_)
        ));
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
