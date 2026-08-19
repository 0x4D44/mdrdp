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
/// 3: the health ladder — `rungs`, `pool`, `viewer_connected`, all defaulted on
///    read so a schema-3 client still parses a schema-2 agent, and `CycleDevice`.
///    Nothing gates on this number: `query_status` never reads it and the probe
///    deliberately declines to version-check the control port, which is why a
///    schema-2 *client* reading a schema-3 agent is also safe (serde ignores the
///    fields it does not know).
pub const SCHEMA: u32 = 3;

/// A parsed control request: `{"cmd":"status"}` and friends.
///
/// Fields other than `cmd` are ignored, deliberately: a future client may add
/// arguments an older agent does not know, and the command name alone decides
/// what happens. (serde's `deny_unknown_fields` is a no-op on internally tagged
/// enums anyway — the tolerance is documented rather than accidental.)
// Not `Copy`: `CycleDevice` carries its confirmation token.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Request {
    /// Report the reconcile loop's view of the stack.
    Status,
    /// Kill the capture server; supervision respawns it.
    RestartServer,
    /// Kill all children and exit the agent.
    Shutdown,
    /// Cycle the virtual display device, then rebuild the stack on it (HLD §6
    /// rung 7). **Destructive**: it recreates the display, renumbers it, and
    /// drops every session on the box.
    ///
    /// `confirm` must echo the challenge from the *current* status
    /// ([`StatusReport::cycle_challenge`]). Loopback is not an authorisation
    /// boundary — the probe opens a control forward on every Auto connect, and
    /// any script on the host can reach the port — so the guard lives here, in
    /// the agent, rather than in a client-side convention. Echoing a challenge
    /// derived from live state also means a blind retry after a read timeout
    /// cannot cycle the display a second time.
    CycleDevice {
        #[serde(default)]
        confirm: String,
    },
}

/// Parse one request line. The error string is sent back to the client verbatim,
/// so it names what was wrong rather than echoing serde internals wholesale.
pub fn parse_request(line: &str) -> Result<Request, String> {
    serde_json::from_str(line).map_err(|e| format!("unrecognised request: {e}"))
}

/// The health ladder's rungs, in bring-up order (HLD tranche 4 §6).
///
/// The order is the ladder: a rung is only meaningful once the ones before it
/// are satisfied. **Rungs 0–4 are the bring-up ladder** and are the only ones
/// [`StatusReport::stuck`] may name — see [`Rung::gates_bring_up`], which is
/// load-bearing rather than cosmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rung {
    /// The creator process that owns the virtual device's lifetime.
    Creator,
    /// The virtual display device is present.
    Device,
    /// The driver publishes a pool, and the running server is attached to the
    /// generation it publishes *now* (HLD §6 rung 2 — the rung that catches the
    /// capture server stranded on a section nothing writes to any more).
    Pool,
    /// The display is in the mode the agent wants.
    DisplayMode,
    /// The capture server is supervised *and* accepting on its video port.
    Server,
    /// The input desktop is the one injected input would land on.
    InputDesktop,
    /// Frames are actually being presented to the virtual display.
    Liveness,
}

impl Rung {
    /// Every rung, in ladder order. The client renders from this list, so a rung
    /// an older agent omits shows as [`RungState::Unknown`] rather than vanishing
    /// — an absent rung must never read as a green one.
    pub const ALL: [Rung; 7] = [
        Rung::Creator,
        Rung::Device,
        Rung::Pool,
        Rung::DisplayMode,
        Rung::Server,
        Rung::InputDesktop,
        Rung::Liveness,
    ];

    /// Whether this rung may appear in [`StatusReport::stuck`].
    ///
    /// **Only the bring-up rungs may.** `stuck` feeds [`green`], `green` gates the
    /// client's native connect, and `deploy.rs` carries a second hand-rolled copy
    /// of the same rule — so a rung that stops no pixel (a locked console) must
    /// never reach it, or every client on the fleet refuses to open a session on a
    /// host whose console merely happens to be locked (HLD §8).
    pub fn gates_bring_up(self) -> bool {
        match self {
            Rung::Creator | Rung::Device | Rung::Pool | Rung::DisplayMode | Rung::Server => true,
            Rung::InputDesktop | Rung::Liveness => false,
        }
    }

    /// The wire/display name, matching the serde rename.
    pub fn name(self) -> &'static str {
        match self {
            Rung::Creator => "creator",
            Rung::Device => "device",
            Rung::Pool => "pool",
            Rung::DisplayMode => "display-mode",
            Rung::Server => "server",
            Rung::InputDesktop => "input-desktop",
            Rung::Liveness => "liveness",
        }
    }
}

/// What a rung has to say. Four-way on purpose: "I could not judge this" and "this
/// does not apply right now" are different from "this is broken", and reporting
/// either as `Fail` is how a diagnostic tells a confident wrong story.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RungState {
    Ok,
    Fail,
    /// Could not be judged — e.g. `OpenInputDesktop` returning access-denied,
    /// which is also what a different session or window station returns.
    Unknown,
    /// Not applicable right now — e.g. liveness with nothing drawing.
    Untested,
}

/// One rung's verdict, with the detail a human needs to act on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RungReport {
    pub rung: Rung,
    pub state: RungState,
    /// Free text for the doctor to print. Never a credential, never a payload.
    #[serde(default)]
    pub detail: Option<String>,
}

/// The first bring-up rung that is demonstrably **broken** — what `stuck` reports.
///
/// This is a **gate**, not a summary: `stuck` feeds [`green`], `green` gates the
/// client's native connect, and `deploy.rs` carries a second hand-rolled copy of
/// the same rule. So it keys on [`RungState::Fail`] alone.
///
/// `Unknown` deliberately does NOT gate. Ignorance must not read as health — but
/// in a gate it must not read as failure either, or a section that happens to be
/// mid-write on the tick a client probes would refuse a session that would have
/// worked perfectly. Use [`first_unsatisfied_rung`] when the question is "what
/// should a human look at?" rather than "may a client connect?".
///
/// Rungs that do not gate bring-up are skipped entirely, whatever their state:
/// this is the single place that rule is enforced, so it is the single place to
/// test it.
pub fn stuck_from_rungs(rungs: &[RungReport]) -> Option<String> {
    Rung::ALL
        .iter()
        .filter(|rung| rung.gates_bring_up())
        .find(|rung| {
            rungs
                .iter()
                .any(|r| r.rung == **rung && r.state == RungState::Fail)
        })
        .map(|rung| rung.name().to_owned())
}

/// The first bring-up rung that is not demonstrably `Ok` — including `Unknown`,
/// `Untested`, and a rung the report omits entirely.
///
/// The reporting counterpart of [`stuck_from_rungs`]: this is what the agent log
/// and `--doctor` want, because "I could not tell" is exactly what a human needs
/// to see. A missing rung counts as unsatisfied, so a partial report can never
/// manufacture health.
pub fn first_unsatisfied_rung(rungs: &[RungReport]) -> Option<String> {
    Rung::ALL
        .iter()
        .filter(|rung| rung.gates_bring_up())
        .find(|rung| {
            rungs
                .iter()
                .find(|r| r.rung == **rung)
                .map(|r| r.state != RungState::Ok)
                .unwrap_or(true)
        })
        .map(|rung| rung.name().to_owned())
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
    ///
    /// **Only bring-up rungs ever appear here** — see [`Rung::gates_bring_up`].
    /// Its meaning is unchanged from schema 2 on purpose: it feeds [`green`],
    /// which gates the client's native connect.
    pub stuck: Option<String>,
    /// The full ladder (schema 3). `#[serde(default)]` is load-bearing, not
    /// tidiness: `query_status` deserialises with `serde_json::from_value`, so a
    /// field a schema-2 agent does not send would otherwise be a hard parse
    /// error, and the client turns that into a failed connect. Without this, the
    /// day this lands every not-yet-redeployed host loses the native transport.
    /// An empty vec means "this agent predates the ladder", which the client
    /// renders as `unknown` per rung — never as green.
    #[serde(default)]
    pub rungs: Vec<RungReport>,
    /// What the driver's shared section says (schema 3). Absent on a schema-2
    /// agent, and absent when the section could not be read at all.
    #[serde(default)]
    pub pool: Option<PoolReport>,
    /// Whether a viewer currently holds the capture server's single slot
    /// (schema 3) — the doctor needs this to explain *why* liveness is untested,
    /// and to refuse `--live` rather than time out against a held slot.
    #[serde(default)]
    pub viewer_connected: Option<bool>,
    /// Whether a device cycle is in progress (schema 3). Everything else in the
    /// report is deliberately torn down while this is true, so a doctor that did
    /// not know would report a healthy host as broken.
    #[serde(default)]
    pub cycling: bool,
}

/// What the IDD shared section publishes, read by the agent every tick.
///
/// This is the tranche's central observation: the section name is fixed, the
/// header's `generation` is 0 exactly when the driver publishes no pool, and
/// `frame_seq` is advanced by the driver as the compositor presents — none of
/// which needs a viewer, or even a capture server, to be true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolReport {
    /// 0 means the driver has published no pool.
    pub generation: u32,
    /// The driver's presented-frame counter for this generation.
    pub frame_seq: u64,
    /// The generation the running capture server attached to, when known. A
    /// mismatch against `generation` is the stranded-server signature.
    #[serde(default)]
    pub server_generation: Option<u32>,
}

impl StatusReport {
    /// The token a [`Request::CycleDevice`] must echo to be honoured.
    ///
    /// Derived from state that a completed cycle necessarily changes: the pool
    /// generation and the two children's respawn counts. That gives the guard
    /// both properties it needs. A caller must have *read* current status to
    /// produce it, so a stray loopback peer firing `cycle-device` blind is
    /// refused; and a blind retry after a read timeout — the hazard the fleet
    /// rules call out for any state-changing op — carries a token the cycle it
    /// may already have performed has just invalidated.
    pub fn cycle_challenge(&self) -> String {
        format!(
            "g{}-c{}-s{}",
            self.pool.map(|p| p.generation).unwrap_or(0),
            self.creator.restarts,
            self.server.restarts
        )
    }
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
            rungs: vec![RungReport {
                rung: Rung::DisplayMode,
                state: RungState::Fail,
                detail: Some("60 Hz, wanted 240".to_owned()),
            }],
            pool: Some(PoolReport {
                generation: 7,
                frame_seq: 12345,
                server_generation: Some(6),
            }),
            viewer_connected: Some(true),
            cycling: false,
        }
    }

    /// A ladder with every rung `Ok`, as the base for single-rung mutations.
    fn all_ok() -> Vec<RungReport> {
        Rung::ALL
            .iter()
            .map(|&rung| RungReport {
                rung,
                state: RungState::Ok,
                detail: None,
            })
            .collect()
    }

    fn with_state(rung: Rung, state: RungState) -> Vec<RungReport> {
        let mut rungs = all_ok();
        rungs.iter_mut().find(|r| r.rung == rung).unwrap().state = state;
        rungs
    }

    // --- the ladder ----------------------------------------------------------

    #[test]
    fn stuck_names_the_first_failing_bring_up_rung_in_ladder_order() {
        // Two bring-up rungs red at once: the EARLIER one is the answer, because
        // the ladder is an order, not a set. A fixture that reddened only one
        // rung could not tell a correct implementation from one that returns
        // whichever it happens to find first.
        //
        // The vec is built DELIBERATELY OUT OF LADDER ORDER, with the later rung
        // first. An `all_ok()`-ordered fixture cannot distinguish "ladder order"
        // from "input order" — it agrees with both — so it would pass against an
        // implementation that simply trusts the order the agent happened to send.
        // Proven: iterating the input order leaves this test green until the vec
        // disagrees with the ladder.
        let mut rungs = vec![
            RungReport {
                rung: Rung::Server,
                state: RungState::Fail,
                detail: None,
            },
            RungReport {
                rung: Rung::Device,
                state: RungState::Fail,
                detail: None,
            },
        ];
        rungs.extend(
            all_ok()
                .into_iter()
                .filter(|r| r.rung != Rung::Server && r.rung != Rung::Device),
        );
        assert_eq!(stuck_from_rungs(&rungs).as_deref(), Some("device"));
    }

    #[test]
    fn every_bring_up_rung_can_be_named_by_stuck() {
        // The expected set is written out LONGHAND rather than derived from
        // `Rung::ALL` / `gates_bring_up`. A test that derives its expectation
        // from the constant under test can only ever agree with it: dropping a
        // rung from `ALL` left the derived version green, because it then simply
        // stopped checking that rung. This list is the independent oracle.
        let expected: [(Rung, &str); 5] = [
            (Rung::Creator, "creator"),
            (Rung::Device, "device"),
            (Rung::Pool, "pool"),
            (Rung::DisplayMode, "display-mode"),
            (Rung::Server, "server"),
        ];
        for (rung, name) in expected {
            let rungs = with_state(rung, RungState::Fail);
            assert_eq!(
                stuck_from_rungs(&rungs).as_deref(),
                Some(name),
                "{name} gates bring-up so stuck must name it"
            );
        }
        // …and the ladder holds exactly these five, so a rung that quietly stops
        // gating bring-up is caught here too.
        let gating: Vec<&str> = Rung::ALL
            .iter()
            .filter(|r| r.gates_bring_up())
            .map(|r| r.name())
            .collect();
        assert_eq!(
            gating,
            expected.iter().map(|(_, n)| *n).collect::<Vec<_>>(),
            "the set of bring-up rungs changed"
        );
    }

    #[test]
    fn a_red_non_bring_up_rung_never_reaches_stuck_or_green() {
        // THE load-bearing test of this tranche. `stuck` feeds `green`, `green`
        // gates the client's native connect, and deploy.rs carries a second copy
        // of that rule in PowerShell. A locked console (input-desktop) stops no
        // pixel arriving, so if it could reach `stuck` every client on the fleet
        // would refuse to open a session on a host whose console merely locked.
        for rung in [Rung::InputDesktop, Rung::Liveness] {
            for state in [RungState::Fail, RungState::Unknown, RungState::Untested] {
                let rungs = with_state(rung, state);
                assert_eq!(
                    stuck_from_rungs(&rungs),
                    None,
                    "{} in state {state:?} must not reach stuck",
                    rung.name()
                );
            }
        }
    }

    #[test]
    fn an_unknown_bring_up_rung_is_reported_but_does_not_gate_the_connect() {
        // The two questions have different answers, and conflating them costs
        // real sessions either way round.
        //
        // "What should a human look at?" — ignorance counts: `Unknown` must not
        // read as health, or a partial report manufactures a green stack.
        //
        // "May a client connect?" — ignorance must NOT block: a section that is
        // merely mid-write on the tick a client happens to probe would otherwise
        // refuse a session that would have worked perfectly.
        for state in [RungState::Unknown, RungState::Untested] {
            let rungs = with_state(Rung::Pool, state);
            assert_eq!(
                first_unsatisfied_rung(&rungs).as_deref(),
                Some("pool"),
                "{state:?} must be reported as unsatisfied"
            );
            assert_eq!(
                stuck_from_rungs(&rungs),
                None,
                "{state:?} must not gate the connect"
            );
        }

        // A demonstrable failure gates both.
        let failed = with_state(Rung::Pool, RungState::Fail);
        assert_eq!(first_unsatisfied_rung(&failed).as_deref(), Some("pool"));
        assert_eq!(stuck_from_rungs(&failed).as_deref(), Some("pool"));
    }

    #[test]
    fn a_bring_up_rung_missing_from_the_ladder_is_reported_but_does_not_gate() {
        // Absence is ignorance. It must not read as health in a report — a
        // partial ladder that omits `pool` must not look like a working pool —
        // and it must not gate a connect either, because a schema-2 agent sends
        // no ladder at all and has to stay usable.
        let rungs: Vec<RungReport> = all_ok()
            .into_iter()
            .filter(|r| r.rung != Rung::Pool)
            .collect();
        assert_eq!(first_unsatisfied_rung(&rungs).as_deref(), Some("pool"));
        assert_eq!(stuck_from_rungs(&rungs), None);

        // An empty ladder is maximal ignorance: the FIRST bring-up rung.
        assert_eq!(first_unsatisfied_rung(&[]).as_deref(), Some("creator"));
        assert_eq!(stuck_from_rungs(&[]), None);
    }

    #[test]
    fn a_schema_2_status_line_still_parses_and_reports_no_rungs() {
        // The compatibility guarantee the whole schema bump rests on: a schema-2
        // agent sends none of the new fields. Built by REMOVING them from a
        // serialised schema-3 report, so the test breaks if a future field is
        // added without #[serde(default)] — which would take the native
        // transport down against every host not yet redeployed.
        let mut value = serde_json::to_value(distinct_report()).unwrap();
        let object = value.as_object_mut().unwrap();
        for field in ["rungs", "pool", "viewer_connected", "cycling"] {
            object.remove(field);
        }
        let back: StatusReport = serde_json::from_value(value).expect("schema-2 report must parse");
        assert!(back.rungs.is_empty(), "no ladder from a schema-2 agent");
        assert_eq!(back.pool, None);
        assert_eq!(back.viewer_connected, None);
        assert!(!back.cycling, "a schema-2 agent is never mid-cycle");
        // And it must still be judgeable by the unchanged bring-up rule.
        assert_eq!(back.stuck.as_deref(), Some("display-mode"));
    }

    #[test]
    fn rung_all_covers_every_variant_and_keeps_ladder_order() {
        // ALL is what the client renders from, so a rung missing here would be
        // invisible in the doctor rather than reported as unknown.
        assert_eq!(Rung::ALL.len(), 7);
        let mut sorted = Rung::ALL;
        sorted.sort();
        assert_eq!(sorted, Rung::ALL, "ALL must already be in ladder order");
        assert_eq!(Rung::ALL[0], Rung::Creator);
        assert_eq!(Rung::ALL[Rung::ALL.len() - 1], Rung::Liveness);
    }

    #[test]
    fn cycle_device_parses_and_carries_its_confirmation() {
        assert_eq!(
            parse_request(r#"{"cmd":"cycle-device","confirm":"g7-c3-s5"}"#),
            Ok(Request::CycleDevice {
                confirm: "g7-c3-s5".to_owned()
            })
        );
        // Absent confirmation parses to empty rather than failing, so the AGENT
        // decides — a parse error would report "unrecognised request" for what is
        // really a refused one.
        assert_eq!(
            parse_request(r#"{"cmd":"cycle-device"}"#),
            Ok(Request::CycleDevice {
                confirm: String::new()
            })
        );
    }

    #[test]
    fn the_cycle_challenge_changes_when_a_cycle_would_have_changed_it() {
        let base = distinct_report();
        let token = base.cycle_challenge();
        assert_eq!(token, "g7-c3-s5");

        // A completed cycle bumps the generation and respawns both children. Any
        // of those alone must invalidate a replayed confirmation — that is what
        // makes a blind retry after a read timeout safe.
        let mut bumped = base.clone();
        bumped.pool = Some(PoolReport {
            generation: 8,
            frame_seq: 0,
            server_generation: Some(8),
        });
        assert_ne!(bumped.cycle_challenge(), token);

        let mut respawned = base.clone();
        respawned.creator.restarts += 1;
        assert_ne!(respawned.cycle_challenge(), token);

        let mut server_respawned = base;
        server_respawned.server.restarts += 1;
        assert_ne!(server_respawned.cycle_challenge(), token);
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
            rungs: all_ok(),
            pool: Some(PoolReport {
                generation: 7,
                frame_seq: 900,
                server_generation: Some(7),
            }),
            viewer_connected: Some(false),
            cycling: false,
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
