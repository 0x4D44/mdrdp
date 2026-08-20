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
/// compatible v5 exists the gate loosens without a format break (review S-m4).
pub const WIRE_VERSION_MIN: u32 = 5;
pub const WIRE_VERSION_MAX: u32 = 5;

/// The IDD backing-pixel mode and Windows UI scale selected by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayRequest {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    pub scale_percent: u32,
}

impl DisplayRequest {
    fn control_line(self) -> String {
        serde_json::json!({
            "cmd": "prepare-display",
            "width": self.width,
            "height": self.height,
            "hz": self.hz,
            "scale_percent": self.scale_percent,
        })
        .to_string()
    }

    fn mode(self) -> rhydra::control::ModeReport {
        rhydra::control::ModeReport {
            width: self.width,
            height: self.height,
            hz: self.hz,
        }
    }
}

fn display_ready(status: &rhydra::control::StatusReport, request: DisplayRequest) -> bool {
    status.mode_ok
        && status.display_mode == Some(request.mode())
        && status.desired_display_mode == request.mode()
        && status.desktop_scale_percent == request.scale_percent
        && status.desired_desktop_scale_percent == request.scale_percent
        && rhydra::control::green(status)
}

/// The whole probe's wall-clock budget, ssh spawn to input connect.
pub const PROBE_DEADLINE: Duration = Duration::from_secs(8);

/// How much of the budget one control query may consume.
const CONTROL_BUDGET: Duration = Duration::from_secs(2);

/// How long the input connect retries after the video channel is up.
const INPUT_RETRY_WINDOW: Duration = Duration::from_secs(1);

/// How much of the budget the optional auxiliary connect may consume. Small,
/// because it is the least important step and must never be what makes a
/// connect miss its deadline.
const AUX_CONNECT_BUDGET: Duration = Duration::from_millis(500);

/// The server's stats header, reduced to what the client gates and sizes on.
/// Unknown fields are ignored by serde's default, so the server may grow its
/// header freely.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ServerHeader {
    pub schema: u32,
    pub wire_version: u32,
    pub width: u32,
    pub height: u32,
    pub codec: String,
    pub tiles: Vec<TileHeader>,
    /// Whether the host is listening on the auxiliary channel (clipboard now,
    /// audio in tranche 6).
    ///
    /// **Defaulted false on purpose, and the default is the safety property.**
    /// A host that predates the channel omits the field entirely, and this is
    /// the only signal that tells the client not to try — there is nothing
    /// listening on 9503 there. It gates opening that socket at all, not merely
    /// what is sent on it.
    #[serde(default)]
    pub clipboard: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub struct TileHeader {
    pub id: u8,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

fn validate_video_contract(header: &ServerHeader) -> Result<(), String> {
    let geometry = (header.width, header.height);
    if !matches!(geometry, (5120, 2880) | (2560, 1440)) {
        return Err(format!(
            "unsupported native desktop {}x{}",
            geometry.0, geometry.1
        ));
    }
    let expected: &[(u8, u32, u32, u32, u32)] = match (header.codec.as_str(), geometry) {
        ("h264-420", (5120, 2880)) => &[(0, 0, 0, 2560, 2880), (1, 2560, 0, 2560, 2880)],
        ("h264-420", (2560, 1440)) | ("hevc-420", (2560, 1440)) => &[(0, 0, 0, 2560, 1440)],
        ("hevc-420", (5120, 2880)) => &[(0, 0, 0, 5120, 2880)],
        (codec, _) => return Err(format!("unsupported native codec {codec:?}")),
    };
    let actual: Vec<_> = header
        .tiles
        .iter()
        .map(|t| (t.id, t.x, t.y, t.width, t.height))
        .collect();
    if actual != expected {
        return Err(format!("invalid native tile layout {actual:?}"));
    }
    Ok(())
}

/// What a successful probe hands the session: connected sockets, the parsed
/// header, and the reassembler already holding any bytes read past the header —
/// the session must keep using it, not start a fresh one.
pub struct ProbeSuccess {
    pub video: TcpStream,
    pub reassembler: Reassembler,
    pub header: ServerHeader,
    pub input: TcpStream,
    /// The auxiliary channel, or `None` when the host does not advertise it or
    /// the connect failed. **Never a session failure** — a session without a
    /// clipboard is a working session, and degrading to one is the whole point
    /// of putting this traffic on its own socket.
    pub aux: Option<TcpStream>,
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
    display: DisplayRequest,
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
        match probe_over_with_display(
            ports,
            deadline,
            Some(display),
            || tunnel.poll_exit(),
            &mut report,
        ) {
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
    tunnel_exited: impl FnMut() -> Option<String>,
    stage: impl FnMut(&'static str, Option<String>),
) -> Result<ProbeSuccess, ProbeFailure> {
    probe_over_with_display(ports, deadline, None, tunnel_exited, stage)
}

fn probe_over_with_display(
    ports: ForwardPorts,
    deadline: Instant,
    display: Option<DisplayRequest>,
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
    let mut report = match rhydra::control::query_status(("127.0.0.1", ports.control), budget) {
        Ok(report) => report,
        Err(rhydra::control::QueryError::NoAnswer(_)) => return Err(ProbeFailure::NoAgent),
        Err(rhydra::control::QueryError::Bad(m)) => {
            return Err(ProbeFailure::Io(format!("control answered strangely: {m}")));
        }
    };
    if let Some(request) = display {
        rhydra::control::send_request(
            ("127.0.0.1", ports.control),
            &request.control_line(),
            remaining(deadline, "display request")?.min(CONTROL_BUDGET),
        )
        .map_err(|e| ProbeFailure::Io(format!("display request failed: {e:?}")))?;

        // The agent ticks every two seconds. It first applies the mode, then starts
        // a fresh server whose converters and encoders inherit that geometry.
        loop {
            if let Some(stderr) = tunnel_exited() {
                return Err(ssh::classify_ssh_stderr(&stderr));
            }
            let left = remaining(deadline, "display prepare")?;
            std::thread::sleep(left.min(Duration::from_millis(100)));
            let budget = remaining(deadline, "display status")?.min(CONTROL_BUDGET);
            report = rhydra::control::query_status(("127.0.0.1", ports.control), budget)
                .map_err(|e| ProbeFailure::Io(format!("display status failed: {e:?}")))?;
            if display_ready(&report, request) {
                break;
            }
        }
    }
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
    validate_video_contract(&header)
        .map_err(|message| ProbeFailure::Io(format!("video contract: {message}")))?;
    if let Some(request) = display
        && (header.width, header.height) != (request.width, request.height)
    {
        return Err(ProbeFailure::Io(format!(
            "video header is {}x{}, but the client prepared {}x{}",
            header.width, header.height, request.width, request.height
        )));
    }

    // Step 4: the input channel, which the server binds moments after video.
    let input = connect_input(ports, deadline)?;

    // Step 5: the auxiliary channel, gated on the host advertising it.
    //
    // The gate is a safety property, not an optimisation. Against a host that
    // predates the channel nothing is listening on 9503, and this flag is the
    // only thing that tells the client so. Connecting regardless would merely
    // waste a round trip — but the failure this guards is the one *behind* it:
    // with no aux socket there is no upstream channel at all, and the tempting
    // alternative of sending clipboard on the input channel is terminal for the
    // session, because an unknown input record kind closes the connection.
    //
    // Non-fatal in both directions: no flag, or a flag with nothing behind it,
    // both yield a working session that simply has no clipboard.
    let aux = if header.clipboard {
        connect_aux(ports, deadline)
    } else {
        None
    };

    stage(
        STAGE_HANDSHAKE,
        Some(format!("wire v{}", header.wire_version)),
    );

    Ok(ProbeSuccess {
        video,
        reassembler,
        header,
        input,
        aux,
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

/// Connect the auxiliary channel, or give up quietly.
///
/// Bounded by [`AUX_CONNECT_BUDGET`] as well as the probe deadline: this is the
/// last step and the least important one, so it may not be what makes a
/// connect miss its budget. Every failure returns `None`; none is fatal.
fn connect_aux(ports: ForwardPorts, deadline: Instant) -> Option<TcpStream> {
    let budget = remaining(deadline, "aux").ok()?.min(AUX_CONNECT_BUDGET);
    let stream = TcpStream::connect_timeout(&ports.aux_addr(), budget).ok()?;
    // Clipboard messages are small and bursty; Nagle would add up to 40 ms to
    // one for no gain.
    stream.set_nodelay(true).ok()?;
    Some(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhydra::control::{ChildReport, ModeReport, StatusReport, status_line};
    use std::io::{BufRead, BufReader, Write};
    use std::net::{Ipv4Addr, TcpListener};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
                width: 2560,
                height: 1440,
                hz: 240,
            }),
            desired_display_mode: ModeReport {
                width: 2560,
                height: 1440,
                hz: 240,
            },
            desktop_scale_percent: 100,
            desired_desktop_scale_percent: 100,
            mode_ok: true,
            server: ChildReport {
                running: true,
                restarts: 0,
                last_exit_code: None,
                cooldown_s: 0,
            },
            stuck: None,
            // A schema-2 agent sends none of these, and the client must connect
            // against one exactly as it does against a schema-3 one — the whole
            // point of their serde defaults. The dedicated compatibility test
            // lives beside the wire type in rhydra's `control`.
            rungs: Vec::new(),
            pool: None,
            viewer_connected: None,
            cycling: false,
        }
    }

    /// A header from a host that predates the auxiliary channel: the
    /// `clipboard` field is **absent**, not false. That is the shape a deployed
    /// 0.4.0 host actually sends, and the one the safety gate must survive.
    fn header_bytes(wire_version: u32) -> Vec<u8> {
        header_json(serde_json::json!({
            "schema": 5,
            "wire_version": wire_version,
            "width": 2560,
            "height": 1440,
            "codec": "h264-420",
            "tiles": [{"id":0,"x":0,"y":0,"width":2560,"height":1440}],
            "encoder": "quicksync-h264",
        }))
    }

    /// A header from a host that states its auxiliary-channel capability either
    /// way.
    fn header_bytes_advertising(wire_version: u32, clipboard: bool) -> Vec<u8> {
        header_json(serde_json::json!({
            "schema": rhydra::stats::SCHEMA,
            "wire_version": wire_version,
            "width": 2560,
            "height": 1440,
            "codec": "h264-420",
            "tiles": [{"id":0,"x":0,"y":0,"width":2560,"height":1440}],
            "encoder": "quicksync-h264",
            "clipboard": clipboard,
        }))
    }

    fn header_json(json: serde_json::Value) -> Vec<u8> {
        let mut framed = Vec::new();
        framing::encode(framing::MSG_STATS, json.to_string().as_bytes(), &mut framed);
        framed
    }

    /// Whether the fake host has anything listening on the auxiliary port.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum AuxHost {
        /// Bound and accepting, like a tranche-5 host.
        Listening,
        /// Bound then dropped, so the port is refused — a host that advertises
        /// the channel but whose listener died.
        Dead,
    }

    /// Bind the four roles on loopback and return the ports, a counter of
    /// auxiliary-port connections, and join guards.
    ///
    /// `agent_reply`: what the fake agent answers a status request with, or None
    /// to hang up without answering. `video_payload`: what the fake server
    /// writes on accept, or None for accept-then-close.
    ///
    /// The auxiliary counter is the oracle for the safety gate: "the client did
    /// not open the socket" has to be observed at the listener, because a client
    /// that connected and immediately closed would look identical from the
    /// client side.
    fn fake_host(
        agent_reply: Option<String>,
        video_payload: Option<Vec<u8>>,
    ) -> (
        ForwardPorts,
        Arc<AtomicUsize>,
        Vec<std::thread::JoinHandle<()>>,
    ) {
        fake_host_with(agent_reply, video_payload, AuxHost::Listening)
    }

    fn fake_host_with(
        agent_reply: Option<String>,
        video_payload: Option<Vec<u8>>,
        aux_host: AuxHost,
    ) -> (
        ForwardPorts,
        Arc<AtomicUsize>,
        Vec<std::thread::JoinHandle<()>>,
    ) {
        let control = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let video = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let input = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let aux = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let ports = ForwardPorts {
            video: video.local_addr().unwrap().port(),
            input: input.local_addr().unwrap().port(),
            control: control.local_addr().unwrap().port(),
            aux: aux.local_addr().unwrap().port(),
        };
        let aux_accepts = Arc::new(AtomicUsize::new(0));
        let mut joins = Vec::new();
        match aux_host {
            AuxHost::Listening => {
                let counter = Arc::clone(&aux_accepts);
                joins.push(std::thread::spawn(move || {
                    while let Ok((stream, _)) = aux.accept() {
                        counter.fetch_add(1, Ordering::SeqCst);
                        // Hold it open; dropping would race the client's own
                        // view of a successful connect.
                        std::thread::sleep(Duration::from_millis(500));
                        drop(stream);
                    }
                }));
            }
            AuxHost::Dead => drop(aux),
        }
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
        (ports, aux_accepts, joins)
    }

    fn far_deadline() -> Instant {
        Instant::now() + Duration::from_secs(10)
    }

    #[test]
    fn a_green_host_probes_through_to_connected_sockets_and_a_parsed_header() {
        // Extra bytes after the header must survive inside the reassembler.
        let mut payload = header_bytes(5);
        let mut second = Vec::new();
        framing::encode(framing::MSG_VIDEO_SEQ, &[0u8; 12], &mut second);
        payload.extend_from_slice(&second[..7]); // a partial second message
        let (ports, _aux, _joins) = fake_host(Some(status_line(&green_report())), Some(payload));

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
        assert_eq!(ok.header.wire_version, 5);
        assert_eq!((ok.header.width, ok.header.height), (2560, 1440));
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
                (STAGE_HANDSHAKE, Some("wire v5".to_owned())),
            ]
        );
    }

    #[test]
    fn display_readiness_requires_the_exact_client_mode_and_scale() {
        let request = DisplayRequest {
            width: 5120,
            height: 2880,
            hz: 240,
            scale_percent: 200,
        };
        let mut status = green_report();
        status.display_mode = Some(ModeReport {
            width: 5120,
            height: 2880,
            hz: 240,
        });
        status.desired_display_mode = status.display_mode.unwrap();
        status.desktop_scale_percent = 200;
        status.desired_desktop_scale_percent = 200;
        assert!(display_ready(&status, request));

        status.desktop_scale_percent = 175;
        assert!(!display_ready(&status, request));
        status.desktop_scale_percent = 200;
        status.display_mode.as_mut().unwrap().width = 2560;
        assert!(!display_ready(&status, request));
    }

    #[test]
    fn a_single_full_frame_hevc_stream_is_an_explicitly_supported_fallback() {
        let header: ServerHeader = serde_json::from_value(serde_json::json!({
            "schema": rhydra::stats::SCHEMA,
            "wire_version": 5,
            "width": 5120,
            "height": 2880,
            "codec": "hevc-420",
            "tiles": [{"id":0,"x":0,"y":0,"width":5120,"height":2880}],
            "encoder": "quicksync-hevc"
        }))
        .unwrap();
        validate_video_contract(&header).expect("HEVC remains the 5K fallback");
    }

    #[test]
    fn a_wire_v3_host_is_refused_with_the_versions_named() {
        let (ports, _aux, _joins) =
            fake_host(Some(status_line(&green_report())), Some(header_bytes(3)));
        let err = probe_over(ports, far_deadline(), || None, |_, _| {}).unwrap_err();
        assert_eq!(err, ProbeFailure::VersionMismatch { host: 3, client: 5 });
    }

    /// Poll the counter rather than sleeping a guessed interval.
    fn wait_for_accepts(counter: &AtomicUsize, want: usize, timeout: Duration) -> usize {
        let end = Instant::now() + timeout;
        loop {
            let seen = counter.load(Ordering::SeqCst);
            if seen >= want || Instant::now() >= end {
                return seen;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// How long to let a forbidden connect happen before declaring it did not.
    /// The client's own connect is synchronous and already complete when
    /// `probe_over` returns, so this only covers the listener thread's accept.
    const SETTLE: Duration = Duration::from_millis(250);

    #[test]
    fn a_host_that_never_heard_of_the_channel_is_never_connected_to() {
        // The AC6 safety gate in unit form. The auxiliary port here is bound and
        // accepting, so a client that tried would succeed — the counter stays at
        // zero only because the absent `clipboard` field held the gate shut.
        // That is the shape a deployed 0.4.0 host sends: the field is missing,
        // not false.
        let (ports, aux_accepts, _joins) =
            fake_host(Some(status_line(&green_report())), Some(header_bytes(5)));
        let ok =
            probe_over(ports, far_deadline(), || None, |_, _| {}).expect("probe should succeed");
        assert!(!ok.header.clipboard, "an absent flag must read as false");
        assert!(
            ok.aux.is_none(),
            "no auxiliary socket may be handed to the session"
        );
        std::thread::sleep(SETTLE);
        assert_eq!(
            aux_accepts.load(Ordering::SeqCst),
            0,
            "the client opened the auxiliary socket against a host that never advertised it"
        );
    }

    #[test]
    fn an_explicit_false_binds_exactly_as_hard_as_an_absent_flag() {
        // Absent and false must be indistinguishable, or the serde default is
        // covering for a gate that only works by accident.
        let (ports, aux_accepts, _joins) = fake_host(
            Some(status_line(&green_report())),
            Some(header_bytes_advertising(5, false)),
        );
        let ok =
            probe_over(ports, far_deadline(), || None, |_, _| {}).expect("probe should succeed");
        assert!(ok.aux.is_none());
        std::thread::sleep(SETTLE);
        assert_eq!(aux_accepts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_host_that_advertises_the_channel_is_connected_to_exactly_once() {
        let (ports, aux_accepts, _joins) = fake_host(
            Some(status_line(&green_report())),
            Some(header_bytes_advertising(5, true)),
        );
        let ok =
            probe_over(ports, far_deadline(), || None, |_, _| {}).expect("probe should succeed");
        assert!(ok.header.clipboard);
        assert!(ok.aux.is_some(), "an advertised channel must be connected");
        assert_eq!(
            wait_for_accepts(&aux_accepts, 1, Duration::from_secs(2)),
            1,
            "exactly one auxiliary connection per session"
        );
    }

    #[test]
    fn an_advertised_channel_with_nothing_behind_it_is_not_a_session_failure() {
        // A host may advertise the channel and have its listener die. Degrading
        // to a session without a clipboard is the designed outcome; failing the
        // connect would make an optional feature able to break the product.
        let (ports, _aux_accepts, _joins) = fake_host_with(
            Some(status_line(&green_report())),
            Some(header_bytes_advertising(5, true)),
            AuxHost::Dead,
        );
        let ok = probe_over(ports, far_deadline(), || None, |_, _| {})
            .expect("a refused auxiliary connect must not fail the probe");
        assert!(ok.aux.is_none());
    }

    #[test]
    fn a_stuck_agent_reports_not_green_with_what_it_waits_on() {
        let mut report = green_report();
        report.server.running = false;
        report.stuck = Some("server".to_owned());
        let (ports, _aux, _joins) = fake_host(Some(status_line(&report)), None);
        let err = probe_over(ports, far_deadline(), || None, |_, _| {}).unwrap_err();
        assert_eq!(err, ProbeFailure::NotGreen("server".to_owned()));
    }

    #[test]
    fn a_forward_that_hangs_up_without_answering_means_no_agent() {
        // The control listener accepts and closes without a reply — exactly what
        // ssh does locally when the remote end refuses the forward.
        let (ports, _aux, _joins) = fake_host(None, None);
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
            aux: video_port,
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
        let (ports, _aux, _joins) = fake_host(Some(status_line(&green_report())), Some(payload));
        let err = probe_over(ports, far_deadline(), || None, |_, _| {}).unwrap_err();
        assert!(
            matches!(err, ProbeFailure::Io(ref m) if m.contains("expected the stats header")),
            "got {err:?}"
        );
    }
}
