//! The session agent's loopback control protocol.
//!
//! One JSON object per line in each direction, on 127.0.0.1 only — SSH is the
//! security boundary, exactly as for the video and input ports. The agent serves
//! one client at a time with a short read timeout, so a connected-but-silent
//! client cannot wedge the listener. Each line also has a strict byte ceiling, so
//! a peer that keeps dribbling without a newline cannot grow the agent indefinitely.
//!
//! `shutdown` is the sanctioned way to stop the agent: it kills the supervised
//! children before exiting. The Windows service uses it for graceful worker
//! shutdown and keeps a bounded process-termination fallback for a wedged child.

use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Read, Write};

/// Where the agent's control listener binds, next to video (9500) and input (9501).
pub const CONTROL_PORT: u16 = 9502;

/// Maximum bytes in one control request, including its line ending.
///
/// Ordinary requests are under 200 bytes. The allowance also fits the auxiliary
/// channel's largest legal clipboard value after worst-case JSON `\u00xx` escaping,
/// plus the request envelope, while still bounding an unterminated line strictly.
pub const MAX_REQUEST_LINE_BYTES: usize = crate::aux_proto::MAX_CLIPBOARD_BYTES * 6 + 256;

/// Maximum bytes in one control reply, including its line ending.
///
/// Replies contain status and acknowledgement metadata, never clipboard
/// content. 64 KiB leaves ample room for a status report while making a
/// newline-free reply a bounded allocation rather than a memory exhaustion
/// path.
pub const MAX_REPLY_LINE_BYTES: usize = 64 * 1024;

const CONTROL_READ_CHUNK_BYTES: usize = 8 * 1024;

/// Read one control request without ever growing `line` beyond the protocol bound.
pub fn read_request_line(reader: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut line = String::new();
    let mut bounded = reader.take(MAX_REQUEST_LINE_BYTES as u64 + 1);
    let read = bounded.read_line(&mut line)?;
    if read == 0 {
        return Ok(None);
    }
    if line.len() > MAX_REQUEST_LINE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("control request exceeds the {MAX_REQUEST_LINE_BYTES}-byte line limit"),
        ));
    }
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    Ok(Some(line))
}

/// Bumped on any incompatible change to the request or response shapes.
/// 2: `StatusReport.version` (defaulted on read, so a 2-client reads a 1-agent).
/// 3: the health ladder — `rungs`, `pool`, `viewer_connected`, all defaulted on
///    read so a schema-3 client still parses a schema-2 agent, and `CycleDevice`.
///    Nothing gates on this number: `query_status` never reads it and the probe
///    deliberately declines to version-check the control port, which is why a
///    schema-2 *client* reading a schema-3 agent is also safe (serde ignores the
///    fields it does not know).
/// 4: `ClipboardMatches`, the interactive-session clipboard comparison. Purely
///    additive — a schema-3 client never sends it, and a schema-3 agent answers
///    an unrecognised command with an error rather than misbehaving.
/// 5: `PrepareDisplay` plus requested mode/scale status fields.
/// 6: `ConfigureAudio`, which is handled by the interactive agent and refuses
///    while the capture process owns its named lease.
/// 7: `CheckAudio`, the read-only live-format check used by deploy preflight.
pub const SCHEMA: u32 = 7;

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
    /// Does the **interactive session's** clipboard hold exactly this text?
    ///
    /// Exists because the acceptance criteria cannot be checked any other way:
    /// an ssh session on Windows runs in a different session and window
    /// station, so `Get-Clipboard` over ssh reads a clipboard nobody is using.
    /// The agent is in the console session, so it can see the real one.
    ///
    /// **It compares rather than reads, and that is the whole design.** The
    /// obvious verb — "tell me what is on the clipboard" — would put arbitrary
    /// clipboard content on a control port that any process on the host can
    /// reach and that the probe forwards on every Auto connect. A caller
    /// checking an assertion already knows the text it expects, so sending the
    /// expectation *in* and getting a boolean *out* answers the same question
    /// and discloses nothing. The reply carries a byte count so a mismatch is
    /// still diagnosable.
    ///
    /// Callers must send synthetic text. Nothing here stops a caller sending a
    /// real secret as `expected`, and nothing can — but the acceptance criteria
    /// that use it generate nonces by construction.
    ClipboardMatches {
        #[serde(default)]
        expected: String,
    },
    /// Ask the reconcile loop to apply this IDD mode before video starts.
    PrepareDisplay {
        width: u32,
        height: u32,
        hz: u32,
        scale_percent: u32,
    },
    /// Pin the discovered VB-CABLE endpoints from the interactive session.
    /// The agent owns this operation because an SSH service session has a
    /// different per-user Core Audio policy context.
    ConfigureAudio,
    /// Read back both VB-CABLE formats in the interactive user's policy store.
    CheckAudio,
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
    /// Whether this host can capture audio at all.
    ///
    /// **Deliberately not a bring-up rung**: a host with no audio is a working
    /// host, and gating a session on sound would refuse a perfectly good remote
    /// desktop over a missing speaker. It exists because the alternative is worse
    /// than a red rung — audio that is *silently* absent is indistinguishable
    /// from audio that is broken, which is product requirement 4 (no visibility)
    /// reproduced inside the newest feature. Both fleet hosts report this red
    /// today, and that is the honest answer rather than a defect.
    Audio,
}

impl Rung {
    /// Every rung, in ladder order. The client renders from this list, so a rung
    /// an older agent omits shows as [`RungState::Unknown`] rather than vanishing
    /// — an absent rung must never read as a green one.
    pub const ALL: [Rung; 8] = [
        Rung::Creator,
        Rung::Device,
        Rung::Pool,
        Rung::DisplayMode,
        Rung::Server,
        Rung::InputDesktop,
        Rung::Liveness,
        Rung::Audio,
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
            Rung::InputDesktop | Rung::Liveness | Rung::Audio => false,
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
            Rung::Audio => "audio",
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
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
    /// The client-selected mode the agent is reconciling toward (schema 5).
    #[serde(default)]
    pub desired_display_mode: ModeReport,
    /// Windows UI scale actually in force on the IDD monitor (schema 5). Zero
    /// means the OS would not answer; it never means 0% scaling.
    #[serde(default = "default_scale_percent")]
    pub desktop_scale_percent: u32,
    /// Client-selected Windows UI scale the agent is reconciling toward.
    #[serde(default = "default_scale_percent")]
    pub desired_desktop_scale_percent: u32,
    /// Whether both the backing-pixel mode and UI scale match the request.
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

const fn default_scale_percent() -> u32 {
    100
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

/// The answer to [`Request::ClipboardMatches`].
///
/// Carries no clipboard content, by construction: a boolean and two lengths are
/// enough to assert equality and to say how a mismatch differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardMatchReport {
    pub ok: bool,
    pub schema: u32,
    /// Whether the interactive session's clipboard is byte-identical to the
    /// text sent, after both are put in canonical LF form — the host stores
    /// CRLF, so a raw comparison would fail on every multi-line payload.
    pub matches: bool,
    /// Length of what the clipboard holds, in canonical form. `None` when the
    /// clipboard holds something that is not text, which is a different answer
    /// from holding text that differs.
    pub actual_bytes: Option<usize>,
    /// Length of what the caller expected, for the same reason.
    pub expected_bytes: usize,
}

/// Decide whether a clipboard reading matches an expectation.
///
/// Both sides are put in canonical LF form first, and that is the whole point:
/// the host stores CRLF, so a raw byte comparison would report a mismatch on
/// every multi-line payload for a reason that has nothing to do with whether
/// the two clipboards agree.
///
/// Lives here rather than in the agent binary so it can be tested at all — the
/// binary is Windows-only and never compiles on the machine this is written on.
pub fn clipboard_verdict(actual: Option<&str>, expected: &str) -> (bool, Option<usize>, usize) {
    let expected_wire = crate::aux_proto::to_wire_newlines(expected);
    let actual_wire = actual.map(crate::aux_proto::to_wire_newlines);
    (
        actual_wire.as_deref() == Some(expected_wire.as_str()),
        actual_wire.as_ref().map(|a| a.len()),
        expected_wire.len(),
    )
}

/// Serialise a clipboard comparison as one line.
pub fn clipboard_match_line(
    matches: bool,
    actual_bytes: Option<usize>,
    expected_bytes: usize,
) -> String {
    serde_json::to_string(&ClipboardMatchReport {
        ok: true,
        schema: SCHEMA,
        matches,
        actual_bytes,
        expected_bytes,
    })
    .expect("a clipboard report always serialises")
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

fn remaining_until(deadline: std::time::Instant) -> io::Result<std::time::Duration> {
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "control exchange deadline expired",
        ))
    } else {
        Ok(remaining)
    }
}

/// Write a complete control line while refreshing the socket timeout before
/// every write syscall. A socket write may accept only a prefix, and retrying
/// the remainder under a fresh timeout would let a slow peer extend the
/// caller's budget indefinitely.
fn write_all_until(
    stream: &mut std::net::TcpStream,
    bytes: &[u8],
    deadline: std::time::Instant,
) -> io::Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        stream.set_write_timeout(Some(remaining_until(deadline)?))?;
        match stream.write(&bytes[offset..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "control peer closed while writing",
                ));
            }
            Ok(written) => offset += written,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Read one reply line with a byte ceiling and one absolute deadline.
///
/// Reading directly from the stream, rather than through `BufRead::read_line`,
/// lets us refresh the timeout before each underlying read. The final read is
/// capped at `MAX_REPLY_LINE_BYTES + 1`, so an unterminated oversized reply is
/// rejected after at most one byte beyond the protocol limit.
fn read_reply_line(
    stream: &mut std::net::TcpStream,
    deadline: std::time::Instant,
) -> io::Result<Option<String>> {
    let mut bytes = Vec::with_capacity(CONTROL_READ_CHUNK_BYTES.min(MAX_REPLY_LINE_BYTES + 1));
    let mut chunk = [0_u8; CONTROL_READ_CHUNK_BYTES];

    loop {
        let remaining_bytes = MAX_REPLY_LINE_BYTES + 1 - bytes.len();
        let read_len = remaining_bytes.min(chunk.len());
        stream.set_read_timeout(Some(remaining_until(deadline)?))?;
        let read = match stream.read(&mut chunk[..read_len]) {
            Ok(read) => read,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) =>
            {
                continue
            }
            Err(error) => return Err(error),
        };
        if read == 0 {
            if bytes.is_empty() {
                return Ok(None);
            }
            return String::from_utf8(bytes)
                .map(Some)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
        }

        let chunk_start = bytes.len();
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(offset) = chunk[..read].iter().position(|&byte| byte == b'\n') {
            let newline = chunk_start + offset;
            if newline + 1 > MAX_REPLY_LINE_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("control reply exceeds the {MAX_REPLY_LINE_BYTES}-byte line limit"),
                ));
            }
            bytes.truncate(newline + 1);
            return String::from_utf8(bytes)
                .map(Some)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
        }
        if bytes.len() > MAX_REPLY_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("control reply exceeds the {MAX_REPLY_LINE_BYTES}-byte line limit"),
            ));
        }
    }
}

/// Perform the request/write/reply exchange shared by status and command
/// calls. The standard-library hostname resolver is synchronous and cannot be
/// interrupted; it remains available for compatibility, but the absolute
/// deadline starts before resolution and covers every socket operation after
/// it. Native callers use numeric loopback addresses and therefore avoid that
/// resolver caveat.
fn control_exchange(
    addr: (&str, u16),
    line: &str,
    timeout: std::time::Duration,
) -> Result<String, QueryError> {
    use std::net::{TcpStream, ToSocketAddrs};

    let deadline = std::time::Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(std::time::Instant::now);
    let sock_addr = addr
        .to_socket_addrs()
        .map_err(|error| QueryError::NoAnswer(format!("resolve {}:{}: {error}", addr.0, addr.1)))?
        .next()
        .ok_or_else(|| {
            QueryError::NoAnswer(format!("resolve {}:{}: no addresses", addr.0, addr.1))
        })?;
    let connect_budget = remaining_until(deadline)
        .map_err(|error| QueryError::NoAnswer(format!("connect {}:{}: {error}", addr.0, addr.1)))?;
    let mut stream = TcpStream::connect_timeout(&sock_addr, connect_budget)
        .map_err(|error| QueryError::NoAnswer(format!("connect {}:{}: {error}", addr.0, addr.1)))?;
    stream.set_nodelay(true).map_err(|error| {
        QueryError::NoAnswer(format!("configure {}:{}: {error}", addr.0, addr.1))
    })?;
    write_all_until(&mut stream, line.trim().as_bytes(), deadline)
        .and_then(|_| write_all_until(&mut stream, b"\n", deadline))
        .map_err(|error| QueryError::NoAnswer(format!("write: {error}")))?;

    let reply = match read_reply_line(&mut stream, deadline) {
        Ok(Some(reply)) => reply,
        Ok(None) => return Err(QueryError::NoAnswer("empty reply".to_owned())),
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            return Err(QueryError::Bad(format!("invalid reply: {error}")));
        }
        Err(error) => return Err(QueryError::NoAnswer(format!("read: {error}"))),
    };
    if reply.trim().is_empty() {
        return Err(QueryError::NoAnswer("empty reply".to_owned()));
    }
    Ok(reply)
}

/// One blocking status query against an agent control port.
///
/// `timeout` is one absolute budget shared by TCP connect, request write, and
/// reply read — no individual socket operation receives a fresh full timeout.
/// The standard-library hostname resolver remains synchronous for compatibility;
/// numeric addresses avoid that uninterruptible lookup on native call paths.
pub fn query_status(
    addr: (&str, u16),
    timeout: std::time::Duration,
) -> Result<StatusReport, QueryError> {
    let line = control_exchange(addr, r#"{"cmd":"status"}"#, timeout)?;
    let value: serde_json::Value =
        serde_json::from_str(&line).map_err(|e| QueryError::Bad(format!("not JSON: {e}")))?;
    if value["ok"] != true {
        return Err(QueryError::Bad(format!("refused: {}", line.trim())));
    }
    serde_json::from_value(value["status"].clone())
        .map_err(|e| QueryError::Bad(format!("bad status shape: {e}")))
}

/// Send one command and read its acknowledgement.
///
/// The write counterpart of [`query_status`], and it carries the same hazards, so
/// it takes the same explicit `timeout` rather than hiding a constant: the caller
/// owns one absolute connect/write/read budget. `line` is the request JSON, already serialised by the caller —
/// the client crate builds these by hand rather than depending on a serialiser for
/// three shapes.
///
/// An agent that does not know the command answers `{"ok":false,...}`, which
/// arrives here as [`QueryError::Bad`] carrying the agent's own words. That is the
/// intended degrade for a schema-2 agent asked to cycle its device: refused with a
/// reason, never silently ignored.
pub fn send_request(
    addr: (&str, u16),
    line: &str,
    timeout: std::time::Duration,
) -> Result<String, QueryError> {
    let reply = control_exchange(addr, line, timeout)?;
    let value: serde_json::Value =
        serde_json::from_str(&reply).map_err(|e| QueryError::Bad(format!("not JSON: {e}")))?;
    if value["ok"] != true {
        // The agent's refusal text is the useful part — a rejected cycle-device
        // says why, and the operator needs to read it verbatim.
        let why = value["error"].as_str().unwrap_or(reply.trim()).to_owned();
        return Err(QueryError::Bad(why));
    }
    // The whole line, for callers whose reply carries more than the ack — the
    // clipboard comparison, and whatever comes next. Callers that only wanted
    // "did it work?" write `send_request(..)?;` and drop it.
    Ok(reply)
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
    fn an_unterminated_control_line_is_rejected_at_the_byte_ceiling() {
        let wire = vec![b'x'; MAX_REQUEST_LINE_BYTES * 2];
        let mut reader = std::io::Cursor::new(wire);

        let error = read_request_line(&mut reader).expect_err("the line is over the ceiling");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(reader.position(), MAX_REQUEST_LINE_BYTES as u64 + 1);
    }

    #[test]
    fn bounded_control_reader_preserves_complete_and_final_lines() {
        let mut reader = std::io::Cursor::new(b"first\nlast".as_slice());

        assert_eq!(
            read_request_line(&mut reader).unwrap().as_deref(),
            Some("first")
        );
        assert_eq!(
            read_request_line(&mut reader).unwrap().as_deref(),
            Some("last")
        );
        assert_eq!(read_request_line(&mut reader).unwrap(), None);
    }

    #[test]
    fn the_largest_legal_clipboard_expectation_still_fits_the_control_bound() {
        let expected = "\0".repeat(crate::aux_proto::MAX_CLIPBOARD_BYTES);
        let mut wire = serde_json::json!({
            "cmd": "clipboard-matches",
            "expected": expected,
        })
        .to_string();
        wire.push('\n');
        assert!(wire.len() <= MAX_REQUEST_LINE_BYTES);

        let mut reader = std::io::Cursor::new(wire);
        let line = read_request_line(&mut reader).unwrap().unwrap();
        let Request::ClipboardMatches { expected } = parse_request(&line).unwrap() else {
            panic!("the request changed kind");
        };
        assert_eq!(expected.len(), crate::aux_proto::MAX_CLIPBOARD_BYTES);
    }

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
        assert_eq!(
            parse_request(
                r#"{"cmd":"prepare-display","width":5120,"height":2880,"hz":240,"scale_percent":200}"#,
            ),
            Ok(Request::PrepareDisplay {
                width: 5120,
                height: 2880,
                hz: 240,
                scale_percent: 200,
            })
        );
        assert_eq!(
            parse_request(r#"{"cmd":"configure-audio"}"#),
            Ok(Request::ConfigureAudio)
        );
        assert_eq!(
            parse_request(r#"{"cmd":"check-audio"}"#),
            Ok(Request::CheckAudio)
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
            desired_display_mode: ModeReport {
                width: 5120,
                height: 2880,
                hz: 120,
            },
            desktop_scale_percent: 175,
            desired_desktop_scale_percent: 200,
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
        assert_eq!(Rung::ALL.len(), 8);
        let mut sorted = Rung::ALL;
        sorted.sort();
        assert_eq!(sorted, Rung::ALL, "ALL must already be in ladder order");
        assert_eq!(Rung::ALL[0], Rung::Creator);
        assert_eq!(Rung::ALL[Rung::ALL.len() - 1], Rung::Audio);
    }

    #[test]
    fn a_red_audio_rung_never_refuses_a_session() {
        // Both fleet hosts report audio red today, and a remote desktop without
        // sound is a working remote desktop. If this rung gated bring-up, every
        // client on the fleet would refuse to open a session over a missing
        // speaker — which is exactly the class of mistake `gates_bring_up`
        // exists to prevent, and why the rule is asserted rather than trusted.
        assert!(
            !Rung::Audio.gates_bring_up(),
            "audio must never gate a connect"
        );
        // Every rung green except audio, which is red — the state both fleet
        // hosts are actually in.
        let rungs: Vec<RungReport> = Rung::ALL
            .iter()
            .map(|&rung| RungReport {
                rung,
                state: if rung == Rung::Audio {
                    RungState::Fail
                } else {
                    RungState::Ok
                },
                detail: None,
            })
            .collect();
        assert_eq!(
            stuck_from_rungs(&rungs),
            None,
            "a red audio rung must not read as stuck"
        );
    }

    #[test]
    fn clipboard_matches_parses_and_carries_its_expectation() {
        assert_eq!(
            parse_request(r#"{"cmd":"clipboard-matches","expected":"hello"}"#),
            Ok(Request::ClipboardMatches {
                expected: "hello".to_owned()
            })
        );
    }

    #[test]
    fn a_crlf_host_clipboard_matches_an_lf_expectation() {
        // THE reason the comparison canonicalises. The host stores CRLF, so a
        // raw comparison would report a mismatch on every multi-line payload
        // and the acceptance criteria would fail for a reason unrelated to
        // whether the two clipboards actually agree.
        let (matches, actual, expected) = clipboard_verdict(Some("a\r\nb"), "a\nb");
        assert!(matches);
        assert_eq!((actual, expected), (Some(3), 3));
    }

    #[test]
    fn text_that_genuinely_differs_does_not_match_and_says_how_long_it_was() {
        let (matches, actual, expected) = clipboard_verdict(Some("something else"), "expected");
        assert!(!matches);
        assert_eq!((actual, expected), (Some(14), 8));
    }

    #[test]
    fn a_clipboard_holding_no_text_is_distinguishable_from_one_holding_the_wrong_text() {
        // Two different answers with two different remedies: "the copy did not
        // happen" versus "an image is on the clipboard". A bare false would
        // conflate them.
        let (matches, actual, expected) = clipboard_verdict(None, "expected");
        assert!(!matches);
        assert_eq!(actual, None);
        assert_eq!(expected, 8);
    }

    #[test]
    fn the_reply_carries_lengths_and_never_the_content() {
        let line = clipboard_match_line(false, Some(14), 8);
        assert!(line.contains("\"matches\":false"));
        assert!(line.contains("\"actual_bytes\":14"));
        assert!(
            !line.contains("something"),
            "a reply must never carry content: {line}"
        );
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
            desired_display_mode: ModeReport {
                width: 1920,
                height: 1080,
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

    /// Serve one line, reply with `reply`, and hand back the request we saw.
    fn one_shot_server(reply: &'static str) -> (u16, std::thread::JoinHandle<String>) {
        use std::io::{BufRead, BufReader, Write};
        let listener =
            std::net::TcpListener::bind(("127.0.0.1", 0)).expect("an ephemeral port is available");
        let port = listener.local_addr().expect("bound").port();
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("one client");
            let mut writer = stream.try_clone().expect("clone");
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).expect("read");
            writeln!(writer, "{reply}").expect("write");
            line.trim().to_owned()
        });
        (port, handle)
    }

    /// Serve one arbitrary reply after consuming the request line.
    fn raw_reply_server(reply: Vec<u8>) -> (u16, std::thread::JoinHandle<()>) {
        use std::io::{BufRead, BufReader, Write};
        let listener =
            std::net::TcpListener::bind(("127.0.0.1", 0)).expect("an ephemeral port is available");
        let port = listener.local_addr().expect("bound").port();
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("one client");
            let mut request = String::new();
            BufReader::new(stream.try_clone().expect("clone"))
                .read_line(&mut request)
                .expect("read request");
            let mut writer = stream;
            writer.write_all(&reply).expect("write reply");
        });
        (port, handle)
    }

    /// Drip one byte at a time without a newline. A per-read timeout lets this
    /// peer stay alive indefinitely; a shared exchange deadline must not.
    fn dripping_reply_server(
        bytes: usize,
        interval: std::time::Duration,
    ) -> (u16, std::thread::JoinHandle<()>) {
        use std::io::{BufRead, BufReader, Write};
        let listener =
            std::net::TcpListener::bind(("127.0.0.1", 0)).expect("an ephemeral port is available");
        let port = listener.local_addr().expect("bound").port();
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("one client");
            let mut request = String::new();
            BufReader::new(stream.try_clone().expect("clone"))
                .read_line(&mut request)
                .expect("read request");
            let mut writer = stream;
            writer.set_nodelay(true).expect("disable Nagle");
            for _ in 0..bytes {
                if writer.write_all(b"x").is_err() {
                    return;
                }
                writer.flush().expect("flush reply byte");
                std::thread::sleep(interval);
            }
        });
        (port, handle)
    }

    #[test]
    fn send_request_writes_the_line_and_accepts_an_ack() {
        let (port, handle) = one_shot_server(r#"{"ok":true,"schema":3}"#);
        let sent = send_request(
            ("127.0.0.1", port),
            r#"{"cmd":"cycle-device","confirm":"g7-c3-s5"}"#,
            std::time::Duration::from_secs(2),
        );
        assert!(sent.is_ok(), "{sent:?}");
        assert_eq!(
            handle.join().expect("thread"),
            r#"{"cmd":"cycle-device","confirm":"g7-c3-s5"}"#,
            "the agent must receive exactly what the caller asked for"
        );
    }

    #[test]
    fn send_request_surfaces_a_refusal_verbatim() {
        // The refusal text IS the useful part: a rejected cycle-device explains
        // what was wrong with the confirmation, and an operator has to read that
        // rather than a generic "failed".
        let (port, handle) = one_shot_server(
            r#"{"ok":false,"schema":3,"error":"cycle-device needs the confirmation token"}"#,
        );
        let sent = send_request(
            ("127.0.0.1", port),
            r#"{"cmd":"cycle-device"}"#,
            std::time::Duration::from_secs(2),
        );
        let _ = handle.join();
        match sent {
            Err(QueryError::Bad(why)) => {
                assert_eq!(why, "cycle-device needs the confirmation token");
            }
            other => panic!("expected the agent's own words, got {other:?}"),
        }
    }

    #[test]
    fn send_request_distinguishes_nothing_listening_from_a_refusal() {
        // A closed port is NoAnswer, never Bad: "the agent is not there" and "the
        // agent said no" lead an operator to completely different next steps.
        let listener =
            std::net::TcpListener::bind(("127.0.0.1", 0)).expect("an ephemeral port is available");
        let port = listener.local_addr().expect("bound").port();
        drop(listener);
        match send_request(
            ("127.0.0.1", port),
            r#"{"cmd":"status"}"#,
            std::time::Duration::from_millis(500),
        ) {
            Err(QueryError::NoAnswer(_)) => {}
            other => panic!("expected NoAnswer, got {other:?}"),
        }
    }

    #[test]
    fn send_request_honours_its_timeout_against_a_silent_peer() {
        // The hazard that forced query_status to take an explicit timeout: an
        // agent that accepts and never answers must not hold the caller past its
        // own budget.
        let listener =
            std::net::TcpListener::bind(("127.0.0.1", 0)).expect("an ephemeral port is available");
        let port = listener.local_addr().expect("bound").port();
        let _silent = std::thread::spawn(move || {
            let _held = listener.accept();
            std::thread::sleep(std::time::Duration::from_secs(5));
        });
        let started = std::time::Instant::now();
        let outcome = send_request(
            ("127.0.0.1", port),
            r#"{"cmd":"status"}"#,
            std::time::Duration::from_millis(300),
        );
        assert!(outcome.is_err(), "a silent peer must not read as success");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "took {:?}, so the timeout was not honoured",
            started.elapsed()
        );
    }

    #[test]
    fn send_request_rejects_an_oversized_unterminated_reply() {
        use std::time::Duration;
        let (port, handle) = raw_reply_server(vec![b'x'; MAX_REPLY_LINE_BYTES + 1]);
        let result = send_request(
            ("127.0.0.1", port),
            r#"{"cmd":"status"}"#,
            Duration::from_secs(2),
        );
        handle.join().expect("server");
        assert!(
            matches!(&result, Err(QueryError::Bad(message)) if message.contains("line limit")),
            "oversized replies must be rejected before JSON parsing: {result:?}"
        );
    }

    #[test]
    fn query_status_rejects_an_oversized_unterminated_reply() {
        use std::time::Duration;
        let (port, handle) = raw_reply_server(vec![b'x'; MAX_REPLY_LINE_BYTES + 1]);
        let result = query_status(("127.0.0.1", port), Duration::from_secs(2));
        handle.join().expect("server");
        assert!(
            matches!(&result, Err(QueryError::Bad(message)) if message.contains("line limit")),
            "oversized replies must be rejected before JSON parsing: {result:?}"
        );
    }

    #[test]
    fn send_request_does_not_renew_its_deadline_for_a_dripping_reply() {
        use std::time::{Duration, Instant};
        let (port, handle) = dripping_reply_server(32, Duration::from_millis(20));
        let start = Instant::now();
        let result = send_request(
            ("127.0.0.1", port),
            r#"{"cmd":"status"}"#,
            Duration::from_millis(120),
        );
        let elapsed = start.elapsed();
        handle.join().expect("server");
        assert!(
            matches!(&result, Err(QueryError::NoAnswer(_))),
            "got {result:?}"
        );
        assert!(
            elapsed < Duration::from_millis(400),
            "dripping reply renewed the timeout: {elapsed:?}"
        );
    }

    #[test]
    fn query_status_does_not_renew_its_deadline_for_a_dripping_reply() {
        use std::time::{Duration, Instant};
        let (port, handle) = dripping_reply_server(32, Duration::from_millis(20));
        let start = Instant::now();
        let result = query_status(("127.0.0.1", port), Duration::from_millis(120));
        let elapsed = start.elapsed();
        handle.join().expect("server");
        assert!(
            matches!(&result, Err(QueryError::NoAnswer(_))),
            "got {result:?}"
        );
        assert!(
            elapsed < Duration::from_millis(400),
            "dripping reply renewed the timeout: {elapsed:?}"
        );
    }

    #[test]
    fn send_request_applies_its_deadline_to_a_blocked_partial_write() {
        use std::time::{Duration, Instant};
        let listener =
            std::net::TcpListener::bind(("127.0.0.1", 0)).expect("an ephemeral port is available");
        let port = listener.local_addr().expect("bound").port();
        let handle = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().expect("one client");
            // Do not consume the request. The client must time out while its
            // large line fills the kernel send window, rather than waiting for
            // this peer to close the connection.
            std::thread::sleep(Duration::from_millis(750));
        });

        let request = "x".repeat(8 * 1024 * 1024);
        let start = Instant::now();
        let result = send_request(("127.0.0.1", port), &request, Duration::from_millis(120));
        let elapsed = start.elapsed();
        handle.join().expect("server");
        assert!(
            matches!(&result, Err(QueryError::NoAnswer(message)) if message.starts_with("write:")),
            "blocked write should fail during writing: {result:?}"
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "blocked write exceeded its absolute deadline: {elapsed:?}"
        );
    }

    #[test]
    fn expired_exchange_budget_rejects_write_and_read_before_transfer() {
        use std::io::Read;
        use std::net::TcpStream;
        use std::time::{Duration, Instant};
        let listener =
            std::net::TcpListener::bind(("127.0.0.1", 0)).expect("an ephemeral port is available");
        let port = listener.local_addr().expect("bound").port();
        let mut client = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        let (mut peer, _) = listener.accept().expect("accept");
        let expired = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("the test deadline is representable");

        let write_error = write_all_until(&mut client, b"x", expired).expect_err("expired write");
        assert_eq!(write_error.kind(), std::io::ErrorKind::TimedOut);
        peer.set_nonblocking(true).expect("nonblocking peer");
        let mut received = [0_u8; 1];
        match peer.read(&mut received) {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Ok(read) => panic!("expired write transferred {read} bytes"),
            Err(error) => panic!("unexpected peer read error: {error}"),
        }

        let read_error = read_reply_line(&mut client, expired).expect_err("expired read");
        assert_eq!(read_error.kind(), std::io::ErrorKind::TimedOut);
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
