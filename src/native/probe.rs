//! The probe that decides the transport — on the session sockets themselves.
//!
//! The agent's `green` is optimistic by design (spawn sets `running` before the
//! server binds its port; `rhydra::control` documents one green tick per
//! crash-loop cycle), so a control answer alone must never commit the transport
//! (HLD §4.3, review S-C1). The sequence, all under one wall-clock deadline:
//!
//! 1. **Forward readiness** — poll the control forward; ECONNREFUSED means "ssh
//!    hasn't bound yet", never "no agent" (review S-C2).
//! 2. **Control pre-filter** — one status query, `green` required. No version
//!    check here: the agent's crate version says nothing about the server
//!    child's wire dialect (review S-C3).
//! 3. **Video connect + header gate** — the server's first message is its stats
//!    header; it carries `wire_version` (the single version gate) and the
//!    desktop size the window needs. Read synchronously, on this path.
//! 4. **Input connect** — brief retry; the server binds 9501 just after 9500.
//!
//! Only after all four does the caller commit to native; the sockets returned
//! here ARE the session sockets. Every failure is a classified [`ProbeFailure`]
//! the caller maps to fallback (Auto) or a remedy-naming error (`Always`).

use std::io::Read;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use rhydra::framing::{self, Reassembler};

use super::ssh::{self, ForwardPorts, ProbeFailure, Tunnel, TunnelSpec};

/// The wire dialects this client speaks. A range, not an equality, so the day a
/// compatible v4 exists the gate loosens without a format break (review S-m4).
pub const WIRE_VERSION_MIN: u32 = 3;
pub const WIRE_VERSION_MAX: u32 = 3;

/// The whole probe's wall-clock budget, ssh spawn to input connect.
pub const PROBE_DEADLINE: Duration = Duration::from_secs(8);

/// How much of the budget one control query may consume.
const CONTROL_BUDGET: Duration = Duration::from_secs(2);

/// How long the input connect retries after the video channel is up.
const INPUT_RETRY_WINDOW: Duration = Duration::from_secs(1);

/// The server's stats header, reduced to what the client gates and sizes on.
/// Unknown fields are ignored by serde's default, so the server may grow its
/// header freely.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ServerHeader {
    pub schema: u32,
    pub wire_version: u32,
    pub width: u32,
    pub height: u32,
}

/// What a successful probe hands the session: connected sockets, the parsed
/// header, and the reassembler already holding any bytes read past the header —
/// the session must keep using it, not start a fresh one.
pub struct ProbeSuccess {
    pub video: TcpStream,
    pub reassembler: Reassembler,
    pub header: ServerHeader,
    pub input: TcpStream,
}

impl std::fmt::Debug for ProbeSuccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProbeSuccess")
            .field("header", &self.header)
            .field("buffered", &self.reassembler.buffered())
            .finish_non_exhaustive()
    }
}

/// A committed native transport: the probe's sockets plus the tunnel that
/// carries them. Dropping this kills the ssh child.
pub struct ProbedTransport {
    pub conn: ProbeSuccess,
    pub tunnel: Tunnel,
    pub ports: ForwardPorts,
}

/// Spawn the tunnel and run the probe, retrying once with fresh ports if ssh
/// lost the local-port race (`ExitOnForwardFailure` → [`ProbeFailure::SshBind`]).
///
/// `stages` mirrors the RDP connect's live stage feed: the launcher's Connecting
/// dialog renders whatever arrives, and the first native stage name is what tells
/// it to swap to the native ladder. Elapsed times are measured from this call.
pub fn establish(
    host: &str,
    ssh_user: Option<&str>,
    stages: Option<&std::sync::mpsc::Sender<crate::connect::LiveStage>>,
) -> Result<ProbedTransport, ProbeFailure> {
    let started = Instant::now();
    let mut report = move |name: &'static str, qualifier: Option<String>| {
        if let Some(tx) = stages {
            let _ = tx.send(crate::connect::LiveStage {
                name: name.to_owned(),
                elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
                qualifier,
            });
        }
    };
    let mut attempt = 0;
    loop {
        let ports =
            ssh::allocate_ports().map_err(|e| ProbeFailure::Io(format!("port alloc: {e}")))?;
        let spec = TunnelSpec {
            host: host.to_owned(),
            ssh_user: ssh_user.map(str::to_owned),
            local: ports,
        };
        let mut tunnel =
            Tunnel::spawn(&spec).map_err(|e| ProbeFailure::Io(format!("spawn ssh: {e}")))?;
        report(STAGE_SSH_SPAWN, None);
        let deadline = Instant::now() + PROBE_DEADLINE;
        match probe_over(ports, deadline, || tunnel.poll_exit(), &mut report) {
            Ok(conn) => {
                return Ok(ProbedTransport {
                    conn,
                    tunnel,
                    ports,
                });
            }
            Err(ProbeFailure::SshBind) if attempt == 0 => {
                attempt += 1;
                continue; // fresh ports; the dropped tunnel is already killed
            }
            Err(e) => return Err(e),
        }
    }
}

/// The stage names the native connect feeds the launcher's Connecting dialog,
/// in order. `dialogs.rs` keys its native ladder off these exact strings.
pub const STAGE_SSH_SPAWN: &str = "ssh-spawn";
pub const STAGE_TUNNEL_UP: &str = "tunnel-up";
pub const STAGE_PROBE: &str = "probe";
pub const STAGE_HANDSHAKE: &str = "handshake";

/// The four probe steps against already-decided local ports. Split from
/// [`establish`] so tests drive it against local listeners with no ssh at all;
/// `tunnel_exited` reports the ssh child's death (with its stderr) and is polled
/// at every step boundary. `stage` fires as each visible milestone completes.
pub fn probe_over(
    ports: ForwardPorts,
    deadline: Instant,
    mut tunnel_exited: impl FnMut() -> Option<String>,
    mut stage: impl FnMut(&'static str, Option<String>),
) -> Result<ProbeSuccess, ProbeFailure> {
    // Step 1: the control forward accepting proves ssh has bound its listeners.
    let readiness = ssh::await_forward_ready(ports.control_addr(), deadline, &mut tunnel_exited)?;
    drop(readiness);
    stage(STAGE_TUNNEL_UP, None);

    // Step 2: the control pre-filter. A refused-at-the-remote-end forward shows
    // up as an accepted-then-closed local connection — query_status reads EOF and
    // reports NoAnswer, which at this point means "no agent", not "ssh not ready".
    let budget = remaining(deadline, "control query")?.min(CONTROL_BUDGET);
    let report = match rhydra::control::query_status(("127.0.0.1", ports.control), budget) {
        Ok(report) => report,
        Err(rhydra::control::QueryError::NoAnswer(_)) => return Err(ProbeFailure::NoAgent),
        Err(rhydra::control::QueryError::Bad(m)) => {
            return Err(ProbeFailure::Io(format!("control answered strangely: {m}")));
        }
    };
    if !rhydra::control::green(&report) {
        return Err(ProbeFailure::NotGreen(
            report.stuck.unwrap_or_else(|| "unknown".to_owned()),
        ));
    }
    if let Some(stderr) = tunnel_exited() {
        return Err(ssh::classify_ssh_stderr(&stderr));
    }
    stage(STAGE_PROBE, None);

    // Step 3: the video channel and the header gate. `green` said the server was
    // spawned; only this connect proves it is listening (the S-C1 window shows up
    // here as refusal or instant EOF → NoServer).
    let video = TcpStream::connect_timeout(&ports.video_addr(), remaining(deadline, "video")?)
        .map_err(|_| ProbeFailure::NoServer)?;
    video
        .set_nodelay(true)
        .map_err(|e| ProbeFailure::Io(e.to_string()))?;
    let (header, reassembler) = read_header(&video, deadline)?;
    if header.wire_version < WIRE_VERSION_MIN || header.wire_version > WIRE_VERSION_MAX {
        return Err(ProbeFailure::VersionMismatch {
            host: header.wire_version,
            client: WIRE_VERSION_MAX,
        });
    }

    // Step 4: the input channel, which the server binds moments after video.
    let input = connect_input(ports, deadline)?;
    stage(
        STAGE_HANDSHAKE,
        Some(format!("wire v{}", header.wire_version)),
    );

    Ok(ProbeSuccess {
        video,
        reassembler,
        header,
        input,
    })
}

fn remaining(deadline: Instant, phase: &'static str) -> Result<Duration, ProbeFailure> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or(ProbeFailure::Deadline(phase))
}

/// Read framed messages until the first one completes; it must be the stats
/// header. Bytes past the header stay buffered in the returned reassembler.
fn read_header(
    mut video: &TcpStream,
    deadline: Instant,
) -> Result<(ServerHeader, Reassembler), ProbeFailure> {
    let mut reassembler = Reassembler::new(framing::DEFAULT_MAX_PAYLOAD);
    let mut buf = [0u8; 64 * 1024];
    loop {
        video
            .set_read_timeout(Some(remaining(deadline, "handshake")?))
            .map_err(|e| ProbeFailure::Io(e.to_string()))?;
        let n = match video.read(&mut buf) {
            Ok(0) => return Err(ProbeFailure::NoServer),
            Ok(n) => n,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Err(ProbeFailure::Deadline("handshake"));
            }
            Err(e) => return Err(ProbeFailure::Io(format!("header read: {e}"))),
        };
        reassembler.push(&buf[..n]);
        match reassembler.next_message() {
            Ok(Some(message)) => {
                if message.msg_type != framing::MSG_STATS {
                    return Err(ProbeFailure::Io(format!(
                        "first message was type {}, expected the stats header",
                        message.msg_type
                    )));
                }
                let header: ServerHeader = serde_json::from_slice(&message.payload)
                    .map_err(|e| ProbeFailure::Io(format!("bad stats header: {e}")))?;
                video
                    .set_read_timeout(None)
                    .map_err(|e| ProbeFailure::Io(e.to_string()))?;
                return Ok((header, reassembler));
            }
            Ok(None) => continue,
            Err(e) => return Err(ProbeFailure::Io(format!("framing: {e:?}"))),
        }
    }
}

fn connect_input(ports: ForwardPorts, deadline: Instant) -> Result<TcpStream, ProbeFailure> {
    let window_end = (Instant::now() + INPUT_RETRY_WINDOW).min(deadline);
    loop {
        match TcpStream::connect_timeout(&ports.input_addr(), Duration::from_millis(250)) {
            Ok(stream) => {
                stream
                    .set_nodelay(true)
                    .map_err(|e| ProbeFailure::Io(e.to_string()))?;
                return Ok(stream);
            }
            Err(_) if Instant::now() < window_end => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return Err(ProbeFailure::Io(format!(
                    "input channel refused after the video channel was up: {e}"
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhydra::control::{ChildReport, ModeReport, StatusReport, status_line};
    use std::io::{BufRead, BufReader, Write};
    use std::net::{Ipv4Addr, TcpListener};

    fn green_report() -> StatusReport {
        StatusReport {
            schema: rhydra::control::SCHEMA,
            version: "0.3.0".to_owned(),
            uptime_s: 42,
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

    fn header_bytes(wire_version: u32) -> Vec<u8> {
        let json = serde_json::json!({
            "schema": 5,
            "wire_version": wire_version,
            "width": 1920,
            "height": 1080,
            "encoder": "quicksync-h264",
        })
        .to_string();
        let mut framed = Vec::new();
        framing::encode(framing::MSG_STATS, json.as_bytes(), &mut framed);
        framed
    }

    /// Bind the three roles on loopback and return the ports plus join guards.
    /// `agent_reply`: what the fake agent answers a status request with, or None
    /// to hang up without answering. `video_payload`: what the fake server
    /// writes on accept, or None for accept-then-close.
    fn fake_host(
        agent_reply: Option<String>,
        video_payload: Option<Vec<u8>>,
    ) -> (ForwardPorts, Vec<std::thread::JoinHandle<()>>) {
        let control = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let video = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let input = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let ports = ForwardPorts {
            video: video.local_addr().unwrap().port(),
            input: input.local_addr().unwrap().port(),
            control: control.local_addr().unwrap().port(),
        };
        let mut joins = Vec::new();
        joins.push(std::thread::spawn(move || {
            // First accept: the readiness poke (dropped unread). Then the query.
            for _ in 0..2 {
                let Ok((stream, _)) = control.accept() else {
                    return;
                };
                let Some(reply) = agent_reply.clone() else {
                    continue; // hang up: NoAgent at the query step
                };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() && line.contains("status") {
                    let mut w = stream;
                    let _ = writeln!(w, "{reply}");
                    return;
                }
            }
        }));
        joins.push(std::thread::spawn(move || {
            if let Ok((mut stream, _)) = video.accept()
                && let Some(payload) = video_payload
            {
                let _ = stream.write_all(&payload);
                // Keep the socket open long enough for the client to finish
                // reading; dropping immediately can race the read.
                std::thread::sleep(Duration::from_millis(500));
            }
        }));
        joins.push(std::thread::spawn(move || {
            let _ = input.accept();
        }));
        (ports, joins)
    }

    fn far_deadline() -> Instant {
        Instant::now() + Duration::from_secs(10)
    }

    #[test]
    fn a_green_host_probes_through_to_connected_sockets_and_a_parsed_header() {
        // Extra bytes after the header must survive inside the reassembler.
        let mut payload = header_bytes(3);
        let mut second = Vec::new();
        framing::encode(framing::MSG_VIDEO_SEQ, &[0u8; 12], &mut second);
        payload.extend_from_slice(&second[..7]); // a partial second message
        let (ports, _joins) = fake_host(Some(status_line(&green_report())), Some(payload));

        let mut stages: Vec<(&'static str, Option<String>)> = Vec::new();
        let ok = probe_over(
            ports,
            far_deadline(),
            || None,
            |name, q| {
                stages.push((name, q));
            },
        )
        .expect("probe should succeed");
        assert_eq!(ok.header.wire_version, 3);
        assert_eq!((ok.header.width, ok.header.height), (1920, 1080));
        assert!(
            ok.reassembler.buffered() > 0,
            "bytes past the header must stay buffered for the session"
        );
        // The launcher ladder climbs these in this order; handshake names the wire.
        assert_eq!(
            stages,
            vec![
                (STAGE_TUNNEL_UP, None),
                (STAGE_PROBE, None),
                (STAGE_HANDSHAKE, Some("wire v3".to_owned())),
            ]
        );
    }

    #[test]
    fn a_wire_v2_host_is_refused_with_the_versions_named() {
        let (ports, _joins) = fake_host(Some(status_line(&green_report())), Some(header_bytes(2)));
        let err = probe_over(ports, far_deadline(), || None, |_, _| {}).unwrap_err();
        assert_eq!(err, ProbeFailure::VersionMismatch { host: 2, client: 3 });
    }

    #[test]
    fn a_stuck_agent_reports_not_green_with_what_it_waits_on() {
        let mut report = green_report();
        report.server.running = false;
        report.stuck = Some("server".to_owned());
        let (ports, _joins) = fake_host(Some(status_line(&report)), None);
        let err = probe_over(ports, far_deadline(), || None, |_, _| {}).unwrap_err();
        assert_eq!(err, ProbeFailure::NotGreen("server".to_owned()));
    }

    #[test]
    fn a_forward_that_hangs_up_without_answering_means_no_agent() {
        // The control listener accepts and closes without a reply — exactly what
        // ssh does locally when the remote end refuses the forward.
        let (ports, _joins) = fake_host(None, None);
        let err = probe_over(ports, far_deadline(), || None, |_, _| {}).unwrap_err();
        assert_eq!(err, ProbeFailure::NoAgent);
    }

    #[test]
    fn a_green_agent_with_no_listening_server_is_no_server_not_no_agent() {
        // Green control, but nothing ever bound the video port: the S-C1 window.
        let control = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let dead_video = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let video_port = dead_video.local_addr().unwrap().port();
        drop(dead_video);
        let ports = ForwardPorts {
            video: video_port,
            input: video_port,
            control: control.local_addr().unwrap().port(),
        };
        let reply = status_line(&green_report());
        let _agent = std::thread::spawn(move || {
            for _ in 0..2 {
                let Ok((stream, _)) = control.accept() else {
                    return;
                };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() && line.contains("status") {
                    let mut w = stream;
                    let _ = writeln!(w, "{reply}");
                    return;
                }
            }
        });
        let err = probe_over(ports, far_deadline(), || None, |_, _| {}).unwrap_err();
        assert_eq!(err, ProbeFailure::NoServer);
    }

    #[test]
    fn a_first_message_that_is_not_the_header_is_rejected() {
        let mut payload = Vec::new();
        framing::encode(framing::MSG_VIDEO_SEQ, &[0u8; 12], &mut payload);
        let (ports, _joins) = fake_host(Some(status_line(&green_report())), Some(payload));
        let err = probe_over(ports, far_deadline(), || None, |_, _| {}).unwrap_err();
        assert!(
            matches!(err, ProbeFailure::Io(ref m) if m.contains("expected the stats header")),
            "got {err:?}"
        );
    }
}
