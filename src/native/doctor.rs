//! `mdrdp <host> --doctor` — the health ladder, read-only, from the Mac.
//!
//! What `probe stages` is to an RDP host, this is to a native one: it tells you
//! which rung is broken **without** taking a session, and it is safe to run
//! against a host that is already refusing sessions — which is exactly when you
//! want it.
//!
//! Two properties are load-bearing, and both are about not lying:
//!
//! - **It only ever sends `status`.** It never connects the video port, because
//!   the capture server accepts exactly one viewer: a diagnostic connect would
//!   take the slot a real session needs and unpark the capture loop, disturbing
//!   the very thing it measures.
//! - **A rung the agent does not report reads `unknown`, never green.** The
//!   vocabulary is held here, statically, so an older agent's silence is visible
//!   as silence rather than as health (HLD tranche 4 §7/§8).
//!
//! It is not free of side effects and does not claim to be: it performs an ssh
//! key logon, and that is named in the output.

use std::time::Duration;

use rhydra::control::{Rung, RungState, StatusReport};

use super::ssh::{self, ProbeFailure, Tunnel, TunnelSpec};

/// How long the doctor waits for the tunnel and the one status query.
const DOCTOR_DEADLINE: Duration = Duration::from_secs(8);
const CONTROL_BUDGET: Duration = Duration::from_secs(2);

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub state: RowState,
    pub detail: Option<String>,
}

/// What a row has to say. A superset of [`RungState`] because the doctor also
/// reports steps that are not rungs at all (ssh, the tunnel, the agent itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    Ok,
    Fail,
    /// Could not be judged — including "this agent is too old to say".
    Unknown,
    /// Not applicable right now.
    Untested,
}

impl RowState {
    /// The fixed-width word the report prints.
    pub fn word(self) -> &'static str {
        match self {
            RowState::Ok => "ok",
            RowState::Fail => "FAIL",
            RowState::Unknown => "unknown",
            RowState::Untested => "untested",
        }
    }

    fn from_rung(state: RungState) -> Self {
        match state {
            RungState::Ok => RowState::Ok,
            RungState::Fail => RowState::Fail,
            RungState::Unknown => RowState::Unknown,
            RungState::Untested => RowState::Untested,
        }
    }
}

impl Row {
    fn new(name: &str, state: RowState, detail: Option<String>) -> Self {
        Self {
            name: name.to_owned(),
            state,
            detail,
        }
    }
}

/// The ladder rows for a status report.
///
/// Rendered from [`Rung::ALL`], **not** from what the agent happened to send, so
/// a rung an older agent omits appears as `unknown (this agent does not report
/// it)` rather than vanishing from the report entirely. A missing row and a
/// healthy row must never look the same.
pub fn ladder_rows(report: &StatusReport) -> Vec<Row> {
    Rung::ALL
        .iter()
        .map(|rung| match report.rungs.iter().find(|r| r.rung == *rung) {
            Some(r) => Row::new(rung.name(), RowState::from_rung(r.state), r.detail.clone()),
            None => Row::new(
                rung.name(),
                RowState::Unknown,
                Some(format!(
                    "this agent does not report it (schema {})",
                    report.schema
                )),
            ),
        })
        .collect()
}

/// The rows above the ladder: how we reached the agent, and what it is.
pub fn preamble_rows(host: &str, ssh_ms: f64, report: &StatusReport) -> Vec<Row> {
    let mut rows = vec![
        Row::new(
            "ssh",
            RowState::Ok,
            Some(format!("key auth (BatchMode) to {host}, {ssh_ms:.0} ms")),
        ),
        Row::new(
            "tunnel",
            RowState::Ok,
            Some("control forward up".to_owned()),
        ),
        Row::new(
            "agent",
            RowState::Ok,
            Some(format!(
                "v{}, up {}, schema {}",
                if report.version.is_empty() {
                    "?"
                } else {
                    &report.version
                },
                human_uptime(report.uptime_s),
                report.schema
            )),
        ),
    ];
    if report.cycling {
        // Everything below is deliberately torn down mid-cycle, so saying so
        // first stops a reader diagnosing a recovering host as a broken one.
        rows.push(Row::new(
            "cycle",
            RowState::Untested,
            Some("a device cycle is in progress; the rungs below are mid-rebuild".to_owned()),
        ));
    }
    rows
}

/// The trailing rows: facts that are not rungs but change how the rungs read.
pub fn epilogue_rows(report: &StatusReport) -> Vec<Row> {
    let viewer = match report.viewer_connected {
        Some(true) => Row::new(
            "viewer",
            RowState::Ok,
            Some("a session holds the capture server's single slot".to_owned()),
        ),
        Some(false) => Row::new("viewer", RowState::Ok, Some("none".to_owned())),
        None => Row::new(
            "viewer",
            RowState::Unknown,
            Some("this agent does not report it".to_owned()),
        ),
    };
    vec![viewer]
}

fn human_uptime(secs: u64) -> String {
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else {
        format!("{m}m")
    }
}

/// Lay the rows out, one per line, aligned.
pub fn render(host: &str, rows: &[Row]) -> String {
    let width = rows.iter().map(|r| r.name.len()).max().unwrap_or(0);
    let state_width = rows.iter().map(|r| r.state.word().len()).max().unwrap_or(0);
    let mut out = format!("doctor {host}\n");
    for row in rows {
        out.push_str(&format!(
            "  {:<width$}  {:<state_width$}",
            row.name,
            row.state.word(),
            width = width,
            state_width = state_width
        ));
        if let Some(detail) = &row.detail {
            out.push_str("  ");
            out.push_str(detail);
        }
        out.push('\n');
    }
    out
}

/// Whether any row is a demonstrable failure — the process exit code's basis.
pub fn any_failure(rows: &[Row]) -> bool {
    rows.iter().any(|r| r.state == RowState::Fail)
}

/// Run the doctor against `host`, returning the rendered report.
///
/// The only request it ever sends is `status`. That is the safety property, and
/// it is why this is a separate function rather than a flag on the probe: the
/// probe connects the video port, which would take the capture server's only
/// viewer slot.
pub fn run(host: &str, ssh_user: Option<&str>) -> Result<(String, bool), String> {
    let started = std::time::Instant::now();
    let ports = ssh::allocate_ports().map_err(|e| format!("port alloc: {e}"))?;
    let spec = TunnelSpec {
        host: host.to_owned(),
        ssh_user: ssh_user.map(str::to_owned),
        local: ports,
    };
    let mut tunnel = Tunnel::spawn(&spec).map_err(|e| format!("spawn ssh: {e}"))?;
    let deadline = std::time::Instant::now() + DOCTOR_DEADLINE;

    // Reaching the forward at all is the ssh + tunnel verdict.
    if let Err(e) = ssh::await_forward_ready(ports.control_addr(), deadline, || tunnel.poll_exit())
    {
        let rows = vec![
            Row::new("ssh", RowState::Fail, Some(e.remedy(host))),
            Row::new("tunnel", RowState::Unknown, Some("not reached".to_owned())),
            Row::new("agent", RowState::Unknown, Some("not reached".to_owned())),
        ];
        return Ok((render(host, &rows), true));
    }
    let ssh_ms = started.elapsed().as_secs_f64() * 1000.0;

    let report = match rhydra::control::query_status(("127.0.0.1", ports.control), CONTROL_BUDGET) {
        Ok(r) => r,
        Err(e) => {
            let detail = match e {
                rhydra::control::QueryError::NoAnswer(_) => ProbeFailure::NoAgent.remedy(host),
                rhydra::control::QueryError::Bad(m) => {
                    format!("the agent answered strangely: {m}")
                }
            };
            let rows = vec![
                Row::new(
                    "ssh",
                    RowState::Ok,
                    Some(format!("key auth (BatchMode) to {host}, {ssh_ms:.0} ms")),
                ),
                Row::new(
                    "tunnel",
                    RowState::Ok,
                    Some("control forward up".to_owned()),
                ),
                Row::new("agent", RowState::Fail, Some(detail)),
            ];
            return Ok((render(host, &rows), true));
        }
    };

    let mut rows = preamble_rows(host, ssh_ms, &report);
    rows.extend(ladder_rows(&report));
    rows.extend(epilogue_rows(&report));
    let failed = any_failure(&rows);
    Ok((render(host, &rows), failed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhydra::control::{ChildReport, PoolReport, RungReport};

    fn report_with(rungs: Vec<RungReport>, schema: u32) -> StatusReport {
        StatusReport {
            schema,
            version: "0.4.0".to_owned(),
            uptime_s: 3 * 3600 + 25 * 60,
            creator: ChildReport {
                running: true,
                restarts: 0,
                last_exit_code: None,
                cooldown_s: 0,
            },
            device_present: true,
            display_mode: None,
            mode_ok: true,
            server: ChildReport {
                running: true,
                restarts: 0,
                last_exit_code: None,
                cooldown_s: 0,
            },
            stuck: None,
            rungs,
            pool: Some(PoolReport {
                generation: 7,
                frame_seq: 42,
                server_generation: Some(7),
            }),
            viewer_connected: Some(false),
            cycling: false,
        }
    }

    fn rung(rung: Rung, state: RungState) -> RungReport {
        RungReport {
            rung,
            state,
            detail: None,
        }
    }

    #[test]
    fn a_schema_2_agent_yields_every_rung_as_unknown_never_green() {
        // THE property of this module. A schema-2 agent sends no ladder at all.
        // If a missing rung rendered as ok — or vanished — the doctor would
        // report a host it knows nothing about as healthy, which is worse than
        // not running it.
        let rows = ladder_rows(&report_with(Vec::new(), 2));
        assert_eq!(rows.len(), Rung::ALL.len(), "every rung is still listed");
        for row in &rows {
            assert_eq!(
                row.state,
                RowState::Unknown,
                "{} must be unknown against a schema-2 agent",
                row.name
            );
            assert!(
                row.detail.as_deref().unwrap_or("").contains("schema 2"),
                "{} must say why it could not be judged",
                row.name
            );
        }
        assert!(!any_failure(&rows), "unknown is not a failure");
    }

    #[test]
    fn a_partial_ladder_fills_only_the_gaps() {
        // A newer agent that reports some rungs and not others: the reported ones
        // keep their state, the rest are unknown. Built with a rung DELIBERATELY
        // out of ladder order to prove the rendering follows the vocabulary and
        // not the wire order.
        let rows = ladder_rows(&report_with(
            vec![
                rung(Rung::Server, RungState::Fail),
                rung(Rung::Creator, RungState::Ok),
            ],
            3,
        ));
        let by_name = |n: &str| {
            rows.iter()
                .find(|r| r.name == n)
                .unwrap_or_else(|| panic!("{n} is listed"))
                .state
        };
        assert_eq!(by_name("creator"), RowState::Ok);
        assert_eq!(by_name("server"), RowState::Fail);
        assert_eq!(by_name("pool"), RowState::Unknown);
        assert_eq!(by_name("liveness"), RowState::Unknown);
        // Order follows Rung::ALL, not the order the agent sent.
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names[0], "creator");
        assert_eq!(names[names.len() - 1], "liveness");
        assert!(any_failure(&rows), "a Fail rung is a failure");
    }

    #[test]
    fn a_cycle_in_progress_is_announced_before_the_rungs() {
        // Mid-cycle everything below is torn down on purpose. Without this line a
        // reader would diagnose a recovering host as a broken one.
        let mut report = report_with(vec![rung(Rung::Creator, RungState::Fail)], 3);
        report.cycling = true;
        let rows = preamble_rows("quench", 141.0, &report);
        assert!(
            rows.iter().any(|r| r.name == "cycle"),
            "a running cycle must be announced: {rows:?}"
        );
        // …and it is not itself a failure.
        assert!(!any_failure(&rows));
    }

    #[test]
    fn the_viewer_row_distinguishes_none_from_unreported() {
        // "No viewer" and "this agent cannot tell me" are different facts: the
        // first explains why liveness is untested, the second explains nothing.
        let mut report = report_with(Vec::new(), 3);
        report.viewer_connected = Some(false);
        assert_eq!(epilogue_rows(&report)[0].state, RowState::Ok);
        report.viewer_connected = None;
        assert_eq!(epilogue_rows(&report)[0].state, RowState::Unknown);
    }

    #[test]
    fn render_lists_every_row_and_names_the_host() {
        let report = report_with(vec![rung(Rung::Pool, RungState::Fail)], 3);
        let mut rows = preamble_rows("quench.lan.example", 141.0, &report);
        rows.extend(ladder_rows(&report));
        let text = render("quench.lan.example", &rows);
        assert!(text.starts_with("doctor quench.lan.example\n"));
        for rung in Rung::ALL {
            assert!(
                text.contains(rung.name()),
                "{} is missing from the report:\n{text}",
                rung.name()
            );
        }
        assert!(text.contains("FAIL"), "a failing rung must stand out");
    }
}
