//! Drive an RDP connection through to capability exchange.
//!
//! Sequence: TCP → X.224 security negotiation → TLS (pinned) → CredSSP/NLA → MCS →
//! licensing → Demand Active / Confirm Active → finalization.
//!
//! Records a **payload-free** stage trace as it goes. Under HYBRID_EX everything after
//! negotiation is inside TLS, so a packet capture proves nothing and decrypting one
//! would manufacture exactly the material we are forbidden to keep. Stage name, outcome,
//! elapsed time and negotiated parameters are the evidence; bytes are not.

use crate::audio::DynamicRdpsndListener;
use crate::creds::Secret;
use crate::egfx::{EgfxObservations, EgfxProbe};
use crate::stagelog::{StageEvent, StageLog};
use crate::trust::{Fingerprint, KnownHosts, TofuVerifier, TrustOutcome};
use ironrdp::connector::{ClientConnector, Config, Credentials, DesktopSize};
use ironrdp_blocking::{Framed, connect_begin, connect_finalize, mark_as_upgraded};
use rustls::pki_types::ServerName;
use rustls::{ClientConnection, StreamOwned};
use serde::Serialize;
use std::fmt;
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing_subscriber::layer::SubscriberExt as _;

/// A peer that stops reading must not hold the sole session thread forever.
///
/// Every logical outbound batch gets one deadline. A partial frame is terminal because
/// retrying a later frame on the same stream would make the RDP byte stream ambiguous.
pub(crate) const RDP_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

pub struct ConnectOptions {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub domain: Option<String>,
    pub desktop_size: DesktopSize,
    /// Desktop scale percent (100–500) to advertise in the GCC core data, so the server
    /// renders the session at that DPI from logon. `None` advertises nothing (the wire
    /// carries 0, which servers ignore). Advertising at connect matters because a
    /// mid-session DPI change is answered by DWM bitmap-stretching every window whose
    /// process is not per-monitor-DPI-aware — blur no client can undo.
    pub desktop_scale_percent: Option<u32>,
    pub known_hosts: PathBuf,
    /// Open the graphics channel and observe what the server negotiates, for how long.
    /// `None` connects and disconnects without touching EGFX (P2 behaviour).
    pub observe_egfx: Option<Duration>,
    /// Dump the first few raw AVC444 payloads here, for offline replay of a decode
    /// anomaly. **Session content** — carried by the operator's `--capture` opt-in.
    pub avc_capture: Option<PathBuf>,
    /// Stream each connect stage as it happens, for a UI that shows live progress.
    /// `None` costs nothing. The receiver disappearing is not an error — progress
    /// display must never be able to fail a connect.
    pub live_stages: Option<std::sync::mpsc::Sender<LiveStage>>,
    /// Consulted on a first-sight certificate, blocking the connect until a decision
    /// arrives. `None` keeps the CLI's pin-on-first-sight behaviour.
    pub trust_prompt: Option<crate::trust::TrustPrompt>,
}

/// One live progress event, streamed while connecting.
///
/// Deliberately its own type rather than [`StageEvent`]: the qualifier may carry a
/// peer address, which belongs in an interactive progress row but must never enter
/// the redacted metrics report that `StageEvent` feeds.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveStage {
    pub name: String,
    pub elapsed_ms: f64,
    pub qualifier: Option<String>,
}

/// One step of the connection sequence.
#[derive(Debug, Clone, Serialize)]
pub struct Stage {
    pub name: &'static str,
    pub elapsed_ms: f64,
    pub detail: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ConnectReport {
    pub target: String,
    /// Coarse phases measured by this crate.
    pub stages: Vec<Stage>,
    /// The connector's own state sequence, captured from IronRDP's instrumentation.
    /// This is what shows CredSSP, licensing and capability exchange as distinct legs.
    pub connector_stages: Vec<StageEvent>,
    pub tls_version: Option<String>,
    pub tls_cipher_suite: Option<String>,
    pub certificate_fingerprint: String,
    pub trust: &'static str,
    pub desktop_width: u16,
    pub desktop_height: u16,
    /// Whether a Shutdown Request was sent. False means a session may be left behind.
    pub graceful_shutdown: bool,
    /// What the graphics pipeline negotiated, when observation was requested.
    pub egfx: Option<EgfxObservations>,
    /// Static channels the server actually joined. If DRDYNVC is absent, no dynamic
    /// channel can ever open and an empty EGFX result means the channel was never
    /// available — not that our observation loop failed.
    pub joined_static_channels: Vec<String>,
    pub total_ms: f64,
}

#[derive(Debug)]
pub enum ConnectError {
    Io(std::io::Error),
    Tls(String),
    Trust(String),
    /// The connector refused. Carries IronRDP's own message, which names the stage.
    Protocol(String),
    NoPeerCertificate,
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectError::Io(e) => write!(f, "network error: {e}"),
            ConnectError::Tls(m) => write!(f, "TLS error: {m}"),
            ConnectError::Trust(m) => write!(f, "certificate trust failure: {m}"),
            ConnectError::Protocol(m) => write!(f, "RDP connection failed: {m}"),
            ConnectError::NoPeerCertificate => write!(f, "server presented no certificate"),
        }
    }
}

impl std::error::Error for ConnectError {}

impl From<std::io::Error> for ConnectError {
    fn from(e: std::io::Error) -> Self {
        ConnectError::Io(e)
    }
}

/// Render an error together with its `source()` chain.
///
/// IronRDP's own `Display` for a connector error prints only the outermost layer, which
/// for a rejected login reads `[CredSSP @ connector.rs:113] CredSSP` — true, and useless
/// to whoever has to fix it. The cause naming *why* authentication failed is one or more
/// links down the chain.
pub fn describe(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !out.contains(&text) {
            out.push_str(": ");
            out.push_str(&text);
        }
        source = cause.source();
    }
    out
}

/// Kerberos is out of scope; `temper` authenticates with NTLM over CredSSP.
///
/// This exists because the connector requires a network client for KDC traffic. If it is
/// ever called, that is a real finding — say so loudly rather than appearing to work.
#[derive(Debug, Default)]
struct NoKerberos;

impl ironrdp::connector::sspi::network_client::NetworkClient for NoKerberos {
    fn send(
        &self,
        _request: &ironrdp::connector::sspi::generator::NetworkRequest,
    ) -> ironrdp::connector::sspi::Result<Vec<u8>> {
        Err(ironrdp::connector::sspi::Error::new(
            ironrdp::connector::sspi::ErrorKind::UnsupportedFunction,
            "Kerberos KDC traffic is not supported by mdrdp; expected NTLM over CredSSP",
        ))
    }
}

struct Trace {
    started: Instant,
    last: Instant,
    stages: Vec<Stage>,
    /// Mirror of each mark, streamed to a listening UI. Send failures are ignored:
    /// a closed progress display must never fail a connect.
    live: Option<std::sync::mpsc::Sender<LiveStage>>,
}

impl Trace {
    fn new(live: Option<std::sync::mpsc::Sender<LiveStage>>) -> Self {
        let now = Instant::now();
        Trace {
            started: now,
            last: now,
            stages: Vec::new(),
            live,
        }
    }

    fn mark(&mut self, name: &'static str, detail: Option<String>) {
        let now = Instant::now();
        let elapsed_ms = ms(now - self.last);
        if let Some(live) = &self.live {
            let _ = live.send(LiveStage {
                name: name.to_owned(),
                elapsed_ms,
                qualifier: detail.clone(),
            });
        }
        self.stages.push(Stage {
            name,
            elapsed_ms,
            detail,
        });
        self.last = now;
    }
}

fn ms(d: Duration) -> f64 {
    (d.as_micros() as f64) / 1000.0
}

/// Static channels worth naming in a report.
///
/// `ChannelName` keeps its bytes private and implements no `Display`, so a name cannot be
/// read back out of one — only compared. This is the list we compare against.
const KNOWN_CHANNELS: &[&str] = &[
    "drdynvc", "cliprdr", "rdpsnd", "rdpdr", "ainput", "echo", "rail",
];

/// Render a channel name for humans, and for `contains`-style checks.
///
/// The obvious `format!("{:?}", name)` yields `ChannelName { inner: [100, 114, ...] }`,
/// which is unreadable *and* silently defeats any comparison against a plain name — a
/// check for "cliprdr" against that string can never match, so the client reports the
/// clipboard as unavailable on a server that joined it perfectly well.
pub fn channel_label(name: &ironrdp_svc::pdu::gcc::ChannelName) -> String {
    KNOWN_CHANNELS
        .iter()
        .find(|known| ironrdp_svc::pdu::gcc::ChannelName::from_utf8(known).as_ref() == Some(name))
        .map(|known| (*known).to_owned())
        .unwrap_or_else(|| format!("{name:?}"))
}

/// Build the active stage once. It owns the static channel set, so it cannot be built
/// twice from one `ConnectionResult` — and both observation and shutdown need it.
fn build_active_stage(
    result: ironrdp::connector::ConnectionResult,
) -> ironrdp::session::ActiveStage {
    ironrdp::session::ActiveStageBuilder {
        static_channels: result.static_channels,
        user_channel_id: result.user_channel_id,
        io_channel_id: result.io_channel_id,
        message_channel_id: result.message_channel_id,
        share_id: result.share_id,
        compression_type: result.compression_type,
        enable_server_pointer: result.enable_server_pointer,
        pointer_software_rendering: result.pointer_software_rendering,
    }
    .build()
}

/// Send an [MS-RDPBCGR] Shutdown Request so the server tears the session down instead of
/// leaving it disconnected-but-alive.
///
/// Not optional politeness. Abandoning the socket leaves a session the host does not
/// reclaim promptly; an evening of test connects wedged `temper` until it stopped
/// completing logons at all.
pub fn send_shutdown<S: std::io::Read + std::io::Write>(
    stage: &ironrdp::session::ActiveStage,
    framed: &mut Framed<S>,
) -> Result<(), ConnectError> {
    send_shutdown_with(stage, |frame| {
        framed.write_all(frame)?;
        let (stream, _) = framed.get_inner_mut();
        std::io::Write::flush(stream)
    })
}

/// The production transport variant. Unlike the public generic compatibility helper,
/// this can toggle the live socket and therefore enforce one deadline across every frame.
pub(crate) fn send_shutdown_bounded<S: std::io::Read + std::io::Write + SetNonblocking>(
    stage: &ironrdp::session::ActiveStage,
    framed: &mut Framed<S>,
) -> Result<(), ConnectError> {
    let deadline = Instant::now() + RDP_WRITE_TIMEOUT;
    send_shutdown_with(stage, |frame| write_framed(framed, frame, deadline))
}

fn send_shutdown_with(
    stage: &ironrdp::session::ActiveStage,
    mut send: impl FnMut(&[u8]) -> std::io::Result<()>,
) -> Result<(), ConnectError> {
    let outputs = stage
        .graceful_shutdown()
        .map_err(|e| ConnectError::Protocol(describe(&e)))?;

    for output in outputs {
        if let ironrdp::session::ActiveStageOutput::ResponseFrame(frame) = output {
            send(&frame).map_err(ConnectError::Io)?;
        }
    }
    Ok(())
}

/// Toggle the nonblocking mode of the transport owned by the live Framed stream.
pub(crate) trait SetNonblocking {
    fn set_nonblocking(&mut self, nonblocking: bool) -> std::io::Result<()>;
    fn wait_writable(&mut self, timeout: Duration) -> std::io::Result<bool>;

    fn wait_write_progress(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<crate::wake::WriteReadiness> {
        Ok(crate::wake::WriteReadiness {
            readable: false,
            writable: self.wait_writable(timeout)?,
        })
    }

    fn buffer_inbound(
        &mut self,
        _append_plaintext: &mut dyn FnMut(&[u8]),
    ) -> std::io::Result<bool> {
        Ok(false)
    }
}

impl SetNonblocking for StreamOwned<ClientConnection, TcpStream> {
    fn set_nonblocking(&mut self, nonblocking: bool) -> std::io::Result<()> {
        self.sock.set_nonblocking(nonblocking)
    }

    fn wait_writable(&mut self, timeout: Duration) -> std::io::Result<bool> {
        crate::wake::wait_writable(&self.sock, timeout)
    }

    fn wait_write_progress(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<crate::wake::WriteReadiness> {
        crate::wake::wait_write_progress(&self.sock, timeout)
    }

    fn buffer_inbound(&mut self, append_plaintext: &mut dyn FnMut(&[u8])) -> std::io::Result<bool> {
        let mut plaintext = [0u8; 16 * 1024];
        let mut drain_plaintext = |conn: &mut ClientConnection| -> std::io::Result<usize> {
            let mut total = 0;
            loop {
                match conn.reader().read(&mut plaintext) {
                    Ok(0) => break,
                    Ok(read) => {
                        append_plaintext(&plaintext[..read]);
                        total += read;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e),
                }
            }
            Ok(total)
        };

        // Move already-authenticated bytes into Framed's own read buffer first. This
        // preserves stream order without dispatching a newer RDP PDU while the older
        // outbound frame is incomplete, and releases rustls's one-record backpressure.
        let drained = drain_plaintext(&mut self.conn)?;
        if !self.conn.wants_read() {
            if drained > 0 {
                return Ok(true);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "RDP peer closed while an outbound write was blocked",
            ));
        }

        let read = loop {
            match self.conn.read_tls(&mut self.sock) {
                Ok(read) => break read,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(drained > 0),
                Err(e) => return Err(e),
            }
        };
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "RDP peer closed while an outbound write was blocked",
            ));
        }
        self.conn.process_new_packets().map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("TLS inbound record while writing: {e}"),
            )
        })?;
        drain_plaintext(&mut self.conn)?;
        Ok(true)
    }
}

/// Queue one frame and force the underlying transport to report deferred write errors.
///
/// Rustls may accept plaintext while its best-effort socket write fails. `flush` is what
/// surfaces that failure and makes the socket timeout observable to the session. The
/// transport is nonblocking while this helper loops, so each syscall can be checked against
/// one absolute deadline rather than resetting a socket timeout per partial write.
pub(crate) fn write_framed<S: std::io::Read + std::io::Write + SetNonblocking>(
    framed: &mut Framed<S>,
    bytes: &[u8],
    deadline: Instant,
) -> std::io::Result<()> {
    framed.get_inner_mut().0.set_nonblocking(true)?;
    let result = write_framed_with_clock(framed, bytes, deadline, Instant::now);
    let restore = framed.get_inner_mut().0.set_nonblocking(false);
    match restore {
        Ok(()) => result,
        Err(e) => Err(e),
    }
}

fn write_framed_with_clock<
    S: std::io::Read + std::io::Write + SetNonblocking,
    N: FnMut() -> Instant,
>(
    framed: &mut Framed<S>,
    bytes: &[u8],
    deadline: Instant,
    mut now: N,
) -> std::io::Result<()> {
    let mut written = 0;
    while written < bytes.len() {
        if now() >= deadline {
            return Err(write_deadline_error());
        }
        let result = {
            let (stream, _) = framed.get_inner_mut();
            std::io::Write::write(stream, &bytes[written..])
        };
        match result {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "RDP framed write made no progress",
                ));
            }
            Ok(count) => written += count,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                wait_for_write_progress(framed, deadline, &mut now)?;
            }
            Err(e) => return Err(e),
        }
    }

    loop {
        if now() >= deadline {
            return Err(write_deadline_error());
        }
        let result = {
            let (stream, _) = framed.get_inner_mut();
            std::io::Write::flush(stream)
        };
        match result {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                wait_for_write_progress(framed, deadline, &mut now)?;
            }
            Err(e) => return Err(e),
        }
    }
}

fn wait_for_write_progress<
    S: std::io::Read + std::io::Write + SetNonblocking,
    N: FnMut() -> Instant,
>(
    framed: &mut Framed<S>,
    deadline: Instant,
    now: &mut N,
) -> std::io::Result<()> {
    let remaining = deadline.saturating_duration_since(now());
    if remaining.is_zero() {
        return Err(write_deadline_error());
    }
    let readiness = {
        let (stream, _) = framed.get_inner_mut();
        stream.wait_write_progress(remaining)?
    };
    if readiness.readable {
        let buffered = {
            let (stream, buffer) = framed.get_inner_mut();
            stream.buffer_inbound(&mut |plaintext| buffer.extend_from_slice(plaintext))?
        };
        if buffered {
            return Ok(());
        }
    }
    if readiness.writable {
        return Ok(());
    }
    Err(write_deadline_error())
}

fn write_deadline_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "RDP outbound write deadline expired",
    )
}

/// Pump the session so the graphics channel can open, for a bounded time.
///
/// Bounded two ways: a wall-clock budget, and a socket read timeout so a server that
/// simply says nothing cannot hold us. Both matter — if the server declines to open the
/// graphics channel (see `crate::egfx` on the missing DYNVC_GFX flag) the correct
/// outcome is a report saying so, not a hang.
fn observe_egfx<S: std::io::Read + std::io::Write + SetNonblocking>(
    stage: &mut ironrdp::session::ActiveStage,
    framed: &mut Framed<S>,
    socket: &TcpStream,
    desktop: DesktopSize,
    budget: Duration,
) -> Result<(), ConnectError> {
    use ironrdp::session::{ActiveStageOutput, image::DecodedImage};

    // Short read timeout: quiet is an expected outcome here, not an error.
    socket.set_read_timeout(Some(Duration::from_millis(500)))?;

    let mut image = DecodedImage::new(
        ironrdp_graphics::image_processing::PixelFormat::RgbA32,
        desktop.width,
        desktop.height,
    );

    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        let (action, payload) = match framed.read_pdu() {
            Ok(pdu) => pdu,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(e) => return Err(ConnectError::Io(e)),
        };

        let outputs = stage
            .process(&mut image, action, &payload)
            .map_err(|e| ConnectError::Protocol(describe(&e)))?;

        // Every response frame produced for one inbound PDU is one logical outbound
        // batch. A slow peer gets one bounded chance for the batch, not a fresh timeout
        // for every response frame.
        let mut response_deadline = None;
        for out in outputs {
            match out {
                ActiveStageOutput::ResponseFrame(frame) => {
                    let write_deadline = *response_deadline
                        .get_or_insert_with(|| (Instant::now() + RDP_WRITE_TIMEOUT).min(deadline));
                    write_framed(framed, &frame, write_deadline).map_err(ConnectError::Io)?;
                }
                ActiveStageOutput::Terminate(_) => return Ok(()),
                _ => {}
            }
        }
    }
    Ok(())
}

/// A connection that is still open, handed back for a session to drive.
///
/// `connect()` below is the probe form: establish, look, disconnect. This is the form
/// the actual client needs — the framed stream and the active stage stay alive.
pub struct Established {
    pub framed: Framed<StreamOwned<ClientConnection, TcpStream>>,
    pub stage: ironrdp::session::ActiveStage,
    pub socket: TcpStream,
    pub desktop_size: DesktopSize,
    /// The desktop scale the server was last told to use: the connect-time
    /// advertisement, then updated by each Display Control resize that goes out.
    /// `None` = never advertised.
    pub desktop_scale_percent: Option<u32>,
    pub report: ConnectReport,
    /// Kept so the caller can snapshot AFTER observing, not before.
    pub probe: Option<EgfxProbe>,
    /// Produces a fresh Deactivation-Reactivation sequence when the server sends
    /// Deactivate All — which is how a Display Control resolution change completes.
    pub activation_factory: ironrdp::connector::connection_activation::ConnectionActivationFactory,
}

/// Optional channel handlers to negotiate with the server.
///
/// Bundled because every one of these must be registered *before* the MCS channel join —
/// none can be added to a live session — so they are all decided at the same moment, and
/// a growing positional parameter list for them reads badly at every call site.
#[derive(Default)]
pub struct Channels {
    /// EGFX, over the dynamic virtual channel.
    pub gfx: Option<Box<dyn ironrdp_egfx::client::GraphicsPipelineHandler>>,
    /// Clipboard sharing.
    pub cliprdr: Option<Box<dyn ironrdp_cliprdr::backend::CliprdrBackend>>,
    /// Remote audio playback over both classic static RDPSND and the modern
    /// `AUDIO_PLAYBACK_DVC` transport. Left `None` when no output device could be opened:
    /// a client that joins either channel and discards every wave produces silence that
    /// looks exactly like working audio.
    pub rdpsnd: Option<RdpsndHandlers>,
    /// Join the Display Control channel (MS-RDPEDISP), which lets the session ask the
    /// server for a new resolution mid-session. Off by default so the probe paths keep
    /// measuring the channel set they always measured.
    pub display_control: bool,
}

/// Two handlers backed by the same playback ring, one per RDPSND transport.
///
/// Current Windows hosts prefer `AUDIO_PLAYBACK_DVC`; older hosts may only offer the
/// classic static channel. The protocol PDUs are identical, but each transport owns an
/// independent IronRDP state machine.
pub struct RdpsndHandlers {
    static_channel: Box<dyn ironrdp_rdpsnd::client::RdpsndClientHandler>,
    dynamic_channel: DynamicRdpsndListener,
}

impl RdpsndHandlers {
    pub fn new(
        static_channel: Box<dyn ironrdp_rdpsnd::client::RdpsndClientHandler>,
        dynamic_channel: DynamicRdpsndListener,
    ) -> Self {
        Self {
            static_channel,
            dynamic_channel,
        }
    }
}

/// The experience settings sent in the Client Info PDU.
///
/// Explicit rather than `PerformanceFlags::default()` because the upstream default sets
/// `DISABLE_FULLWINDOWDRAG`, which makes dragging a window on the remote desktop show a
/// bare outline instead of its contents. On the LAN this client targets, full-window drag
/// is affordable and the outline looks broken. Menu animations stay off (they are pure
/// extra frames to decode) and font smoothing stays on.
pub fn performance_flags() -> ironrdp::pdu::rdp::client_info::PerformanceFlags {
    use ironrdp::pdu::rdp::client_info::PerformanceFlags;
    PerformanceFlags::DISABLE_MENUANIMATIONS | PerformanceFlags::ENABLE_FONT_SMOOTHING
}

/// Connect and hand back the live session.
///
/// The caller owns the disconnect from here on — see `send_shutdown`. Dropping the
/// stream without it leaves a session alive on the Windows host.
pub fn establish(
    opts: &ConnectOptions,
    secret: &Secret,
    channels: Channels,
) -> Result<Established, ConnectError> {
    let Channels {
        gfx: handler,
        cliprdr,
        rdpsnd,
        display_control,
    } = channels;
    // Read before `rdpsnd` is moved into the channel set below.
    //
    // This drives the Client Info PDU's INFO_NOAUDIOPLAYBACK flag, and the polarity is
    // easy to get backwards: the connector sets NO_AUDIO_PLAYBACK when this is *false*,
    // and MS-RDPBCGR 2.2.1.11.1.1 defines that flag as "audio redirection MUST NOT take
    // place". Leaving it false while registering RDPSND joins the channel and then tells
    // the server never to use it — the server obliges, no Wave PDU ever arrives, and the
    // feature looks present while producing permanent silence.
    let wants_audio = rdpsnd.is_some();
    let mut trace = Trace::new(opts.live_stages.clone());
    let target = format!("{}:{}", opts.host, opts.port);

    // --- TCP -------------------------------------------------------------------
    let tcp = TcpStream::connect((opts.host.as_str(), opts.port))?;
    tcp.set_nodelay(true)?;
    trace.mark("tcp_connect", Some(tcp.peer_addr()?.to_string()));

    let config = Config {
        desktop_size: opts.desktop_size,
        enable_tls: false,
        enable_credssp: true,
        credentials: Credentials::UsernamePassword {
            username: opts.username.clone(),
            password: secret.expose().to_owned(),
        },
        domain: opts.domain.clone(),
        client_build: 0,
        client_name: "mdrdp".to_owned(),
        keyboard_type: ironrdp::pdu::gcc::KeyboardType::IbmEnhanced,
        keyboard_subtype: 0,
        keyboard_functional_keys_count: 12,
        keyboard_layout: 0,
        ime_file_name: String::new(),
        bitmap: None,
        dig_product_id: String::new(),
        client_dir: "C:\\Windows\\System32\\mstscax.dll".to_owned(),
        platform: ironrdp::pdu::rdp::capability_sets::MajorPlatformType::MACINTOSH,
        hardware_id: None,
        request_data: None,
        alternate_shell: String::new(),
        work_dir: String::new(),
        autologon: false,
        enable_audio_playback: wants_audio,
        performance_flags: performance_flags(),
        desktop_scale_factor: opts.desktop_scale_percent.unwrap_or(0),
        license_cache: None,
        timezone_info: Default::default(),
        compression_type: None,
        // The server sends pointer shapes as data and the session thread mirrors them
        // onto the local window as native OS cursors. Software rendering stays off:
        // burning the pointer into the frame would smear it across the scaled viewport
        // and put it a network round-trip behind the real mouse.
        enable_server_pointer: true,
        pointer_software_rendering: false,
        multitransport_flags: None,
    };

    let client_addr = tcp.local_addr()?;
    // A second handle to the same socket, so the observation phase can bound its reads
    // without imposing a timeout on the CredSSP exchange.
    let socket = tcp.try_clone()?;

    let mut connector = ClientConnector::new(config, client_addr);

    // The dynamic virtual channel must be registered BEFORE connecting: it is announced
    // in the MCS Connect Initial GCC network block and cannot be added afterwards.
    let probe = if handler.is_none() {
        opts.observe_egfx.map(|_| EgfxProbe::new())
    } else {
        None
    };
    let gfx_handler: Option<Box<dyn ironrdp_egfx::client::GraphicsPipelineHandler>> =
        match (handler, probe.clone()) {
            (Some(h), _) => Some(h),
            (None, Some(p)) => Some(Box::new(p)),
            (None, None) => None,
        };
    let mut drdynvc = ironrdp_dvc::DrdynvcClient::new();
    let mut has_dynamic_channel = false;
    if let Some(h) = gfx_handler {
        // The hardware H.264 decoder, where this platform has one. Its presence must
        // match the handler's capability advertisement (main.rs keys both off
        // `h264::hardware_decode_available`), or AVC frames arrive with nothing to
        // decode them — the vendored client drops AVC capability sets itself when the
        // decoder is absent, so the failure mode is a silent downgrade, not a crash.
        let decoder_factory = crate::h264::hardware_decoder_factory();
        let mut graphics = ironrdp_egfx::client::GraphicsPipelineClient::new_with_decoder_factory(
            h,
            decoder_factory,
        );
        if let Some(dir) = &opts.avc_capture {
            graphics = graphics.capturing_avc_payloads_to(dir);
        }
        drdynvc.attach_dynamic_channel(graphics);
        has_dynamic_channel = true;
    }

    // Display Control (MS-RDPEDISP): joined so the session can ask for a new resolution
    // later — e.g. going fullscreen renegotiating to the monitor's native pixels. The
    // capabilities callback sends nothing: a layout is only ever sent when the user
    // actually changes something, via `ActiveStage::encode_resize`.
    if display_control {
        drdynvc.attach_dynamic_channel(ironrdp::displaycontrol::client::DisplayControlClient::new(
            |_caps| Ok(Vec::new()),
        ));
        has_dynamic_channel = true;
    }

    // CLIPRDR is a *static* channel, so it must be registered before the MCS channel
    // join — there is no way to add one to a live session. A client with no clipboard
    // backend simply never joins the channel, and the server sees a peer that does not
    // do clipboard rather than one that accepts and then ignores it.
    if let Some(backend) = cliprdr {
        connector = connector.with_static_channel(ironrdp_cliprdr::CliprdrClient::new(backend));
    }

    // Offer both transports. Windows 11 opens AUDIO_PLAYBACK_DVC; the static channel is
    // retained as a compatibility fallback for older hosts.
    if let Some(handlers) = rdpsnd {
        drdynvc.attach_listener(handlers.dynamic_channel);
        has_dynamic_channel = true;
        connector = connector
            .with_static_channel(ironrdp_rdpsnd::client::Rdpsnd::new(handlers.static_channel));
        // Windows gates AUDIO_PLAYBACK_DVC on the presence of RDPDR, even when the client
        // redirects no devices. A no-op backend completes the companion-channel handshake
        // without advertising drives, printers, ports, or smart cards.
        connector = connector.with_static_channel(ironrdp_rdpdr::Rdpdr::new(
            Box::new(ironrdp_rdpdr::NoopRdpdrBackend),
            "mdrdp".to_owned(),
        ));
    }
    if has_dynamic_channel {
        connector = connector.with_static_channel(drdynvc);
    }

    // --- X.224 security negotiation --------------------------------------------
    let mut framed = Framed::new(tcp);
    let should_upgrade = connect_begin(&mut framed, &mut connector)
        .map_err(|e| ConnectError::Protocol(describe(&e)))?;
    trace.mark("x224_negotiation", Some("HYBRID_EX requested".to_owned()));

    let stream = framed.into_inner_no_leftover();

    // --- TLS with trust-on-first-use pinning ------------------------------------
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let store =
        KnownHosts::load(&opts.known_hosts).map_err(|e| ConnectError::Trust(e.to_string()))?;
    let verifier = TofuVerifier::new(
        &target,
        opts.known_hosts.clone(),
        store,
        Arc::clone(&provider),
    );
    let verifier = Arc::new(match &opts.trust_prompt {
        Some(prompt) => verifier.with_prompt(prompt.clone()),
        None => verifier,
    });

    let mut tls_config = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| ConnectError::Tls(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::clone(&verifier) as Arc<_>)
        .with_no_client_auth();

    // CredSSP does not support TLS session resumption.
    // <https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-cssp/385a7489-d46b-464c-b224-f7340e308a5c>
    tls_config.resumption = rustls::client::Resumption::disabled();

    let server_name = ServerName::try_from(opts.host.clone())
        .map_err(|e| ConnectError::Tls(format!("invalid server name: {e}")))?;
    let tls_conn = ClientConnection::new(Arc::new(tls_config), server_name.clone())
        .map_err(|e| ConnectError::Tls(e.to_string()))?;
    let mut tls = StreamOwned::new(tls_conn, stream);

    // Drive the handshake to completion so the certificate is available.
    tls.flush()?;

    let (fingerprint, public_key) = {
        let cert = tls
            .conn
            .peer_certificates()
            .and_then(|c| c.first())
            .ok_or(ConnectError::NoPeerCertificate)?;
        let fingerprint = Fingerprint::of_der(cert.as_ref());

        use x509_cert::der::Decode as _;
        let parsed = x509_cert::Certificate::from_der(cert.as_ref())
            .map_err(|e| ConnectError::Tls(format!("certificate is not valid DER: {e}")))?;
        let key = parsed
            .tbs_certificate
            .subject_public_key_info
            .subject_public_key
            .as_bytes()
            .ok_or_else(|| ConnectError::Tls("certificate has no public key bits".to_owned()))?
            .to_vec();
        (fingerprint, key)
    };

    let trust = match verifier.outcome() {
        Some(TrustOutcome::Pinned) => "pinned (matched stored fingerprint)",
        Some(TrustOutcome::PinnedOnFirstSight) => "pinned on first sight (newly recorded)",
        Some(TrustOutcome::AcceptedOnce) => "accepted once (not stored)",
        None => "unknown",
    };
    let tls_version = tls.conn.protocol_version().map(|v| format!("{v:?}"));
    let tls_cipher_suite = tls
        .conn
        .negotiated_cipher_suite()
        .map(|s| format!("{:?}", s.suite()));
    trace.mark("tls_handshake", Some(trust.to_owned()));

    // --- CredSSP and the rest of the connection sequence ------------------------
    let upgraded = mark_as_upgraded(should_upgrade, &mut connector);
    let mut framed = Framed::new(tls);
    let mut network_client = NoKerberos;

    // IronRDP reports each connector state as it steps; capture that rather than
    // reimplementing its loop. Scoped to this call, so nothing is installed globally.
    let stage_log = match &opts.live_stages {
        Some(live) => StageLog::with_sender(live.clone()),
        None => StageLog::new(),
    };
    let subscriber = tracing_subscriber::registry().with(stage_log.clone());
    stage_log.start();
    let result = tracing::subscriber::with_default(subscriber, || {
        connect_finalize(
            upgraded,
            connector,
            &mut framed,
            &mut network_client,
            ironrdp::connector::ServerName::new(opts.host.clone()),
            public_key,
            None,
        )
    })
    .map_err(|e| ConnectError::Protocol(describe(&e)))?;
    // Named for what it actually covers: CredSSP, MCS, licensing, capability exchange
    // and finalization together. connector_stages breaks it down.
    trace.mark("post_tls_sequence", None);
    let connector_stages = stage_log.take();

    // --- Active stage ------------------------------------------------------------
    let desktop_size = result.desktop_size;
    let joined_static_channels: Vec<String> = result
        .static_channels
        .iter()
        .map(|(_id, channel)| channel_label(&channel.channel_name()))
        .collect();
    let activation_factory = result.activation_factory.clone();
    let stage = build_active_stage(result);

    let total_ms = ms(trace.started.elapsed());
    let report = ConnectReport {
        target,
        stages: trace.stages,
        connector_stages,
        tls_version,
        tls_cipher_suite,
        certificate_fingerprint: fingerprint.to_string(),
        trust,
        desktop_width: desktop_size.width,
        desktop_height: desktop_size.height,
        graceful_shutdown: false,
        egfx: None,
        joined_static_channels,
        total_ms,
    };

    Ok(Established {
        framed,
        stage,
        socket,
        desktop_size,
        desktop_scale_percent: opts.desktop_scale_percent,
        report,
        probe,
        activation_factory,
    })
}

/// Probe form: connect, optionally observe EGFX for a while, then disconnect cleanly.
pub fn connect(opts: &ConnectOptions, secret: &Secret) -> Result<ConnectReport, ConnectError> {
    let mut established = establish(opts, secret, Channels::default())?;
    let mut report = established.report;

    if let Some(budget) = opts.observe_egfx {
        let outcome = observe_egfx(
            &mut established.stage,
            &mut established.framed,
            &established.socket,
            established.desktop_size,
            budget,
        );
        if let Err(e) = &outcome {
            report.stages.push(Stage {
                name: "egfx_observation",
                elapsed_ms: 0.0,
                detail: Some(format!("stopped: {e}")),
            });
        }
    }

    // Snapshot AFTER observing — snapshotting at establish time would always be empty.
    report.egfx = established.probe.as_ref().map(|p| p.snapshot());

    let shutdown = send_shutdown_bounded(&established.stage, &mut established.framed);
    report.graceful_shutdown = shutdown.is_ok();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Read, Write};
    use std::time::{Duration, Instant};

    struct DribblingWriter {
        bytes: Vec<u8>,
        max_per_write: usize,
        writes: usize,
    }

    struct BlockedWriter {
        writes: usize,
        waits: usize,
    }

    struct DuplexDeadlockWriter {
        bytes: Vec<u8>,
        pending: Vec<u8>,
        inbound_buffered: bool,
        can_buffer_inbound: bool,
        inbound_records: usize,
        write_ready_on_progress: bool,
        unblock_on_writable_wait: bool,
        inbound_drains: usize,
        writable_waits: usize,
    }

    impl Read for DuplexDeadlockWriter {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::WouldBlock, "quiet"))
        }
    }

    impl Write for DuplexDeadlockWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.pending.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            if !self.inbound_buffered {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "peer cannot read until its pending output is drained",
                ));
            }
            self.bytes.append(&mut self.pending);
            Ok(())
        }
    }

    impl SetNonblocking for DuplexDeadlockWriter {
        fn set_nonblocking(&mut self, _nonblocking: bool) -> io::Result<()> {
            Ok(())
        }

        fn wait_writable(&mut self, _timeout: Duration) -> io::Result<bool> {
            self.writable_waits += 1;
            if self.unblock_on_writable_wait {
                self.inbound_buffered = true;
                Ok(true)
            } else {
                Ok(false)
            }
        }

        fn wait_write_progress(
            &mut self,
            _timeout: Duration,
        ) -> io::Result<crate::wake::WriteReadiness> {
            if self.write_ready_on_progress {
                self.inbound_buffered = true;
            }
            Ok(crate::wake::WriteReadiness {
                readable: true,
                writable: self.write_ready_on_progress,
            })
        }

        fn buffer_inbound(&mut self, append_plaintext: &mut dyn FnMut(&[u8])) -> io::Result<bool> {
            if !self.can_buffer_inbound {
                return Ok(false);
            }
            append_plaintext(b"server frame");
            self.inbound_drains += 1;
            self.inbound_buffered = self.inbound_drains == self.inbound_records;
            Ok(true)
        }
    }

    impl Read for BlockedWriter {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::WouldBlock, "quiet"))
        }
    }

    impl Write for BlockedWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            Err(io::Error::new(io::ErrorKind::WouldBlock, "blocked"))
        }

        fn flush(&mut self) -> io::Result<()> {
            panic!("flush must not follow an unwritten frame")
        }
    }

    impl SetNonblocking for BlockedWriter {
        fn set_nonblocking(&mut self, _nonblocking: bool) -> io::Result<()> {
            Ok(())
        }

        fn wait_writable(&mut self, _timeout: Duration) -> io::Result<bool> {
            self.waits += 1;
            Ok(false)
        }
    }

    impl Read for DribblingWriter {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::WouldBlock, "quiet"))
        }
    }

    impl Write for DribblingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let count = buf.len().min(self.max_per_write);
            self.bytes.extend_from_slice(&buf[..count]);
            self.writes += 1;
            Ok(count)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl SetNonblocking for DribblingWriter {
        fn set_nonblocking(&mut self, _nonblocking: bool) -> io::Result<()> {
            Ok(())
        }

        fn wait_writable(&mut self, _timeout: Duration) -> io::Result<bool> {
            Ok(true)
        }
    }

    #[test]
    fn dribbling_writer_honours_one_absolute_deadline() {
        let base = Instant::now();
        let mut clock = base;
        let mut framed = Framed::new(DribblingWriter {
            bytes: Vec::new(),
            max_per_write: 1,
            writes: 0,
        });

        let error = write_framed_with_clock(
            &mut framed,
            b"0123456789",
            base + Duration::from_millis(5),
            || {
                clock += Duration::from_millis(1);
                clock
            },
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        let (writer, _) = framed.get_inner_mut();
        assert_eq!(writer.bytes.len(), 4);
        assert_eq!(writer.writes, 4);
    }

    #[test]
    fn blocked_writer_waits_for_writability_instead_of_retrying_each_millisecond() {
        let base = Instant::now();
        let mut clock = base;
        let mut framed = Framed::new(BlockedWriter {
            writes: 0,
            waits: 0,
        });

        let error = write_framed_with_clock(
            &mut framed,
            b"frame",
            base + Duration::from_millis(5),
            || {
                clock += Duration::from_millis(1);
                clock
            },
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            framed.get_inner().0.writes,
            1,
            "a blocked socket must sleep in OS readiness, not retry at 1 kHz"
        );
        assert_eq!(framed.get_inner().0.waits, 1);
    }

    #[test]
    fn blocked_write_drains_inbound_tls_to_break_full_duplex_deadlock() {
        let mut framed = Framed::new(DuplexDeadlockWriter {
            bytes: Vec::new(),
            pending: Vec::new(),
            inbound_buffered: false,
            can_buffer_inbound: true,
            inbound_records: 1,
            write_ready_on_progress: false,
            unblock_on_writable_wait: false,
            inbound_drains: 0,
            writable_waits: 0,
        });

        write_framed(
            &mut framed,
            b"frame",
            Instant::now() + Duration::from_secs(1),
        )
        .expect("reading the peer's pending TLS record must unblock its receive path");

        let writer = &framed.get_inner().0;
        assert_eq!(writer.bytes, b"frame");
        assert_eq!(writer.inbound_drains, 1);
    }

    #[test]
    fn simultaneous_read_and_write_readiness_retries_the_write() {
        let mut framed = Framed::new(DuplexDeadlockWriter {
            bytes: Vec::new(),
            pending: Vec::new(),
            inbound_buffered: false,
            can_buffer_inbound: false,
            inbound_records: 1,
            write_ready_on_progress: true,
            unblock_on_writable_wait: false,
            inbound_drains: 0,
            writable_waits: 0,
        });

        write_framed(
            &mut framed,
            b"frame",
            Instant::now() + Duration::from_secs(1),
        )
        .expect("writable readiness must win when TLS cannot buffer more inbound data");

        assert_eq!(framed.get_inner().0.bytes, b"frame");
    }

    #[test]
    fn blocked_write_drains_more_than_one_inbound_tls_record() {
        let mut framed = Framed::new(DuplexDeadlockWriter {
            bytes: Vec::new(),
            pending: Vec::new(),
            inbound_buffered: false,
            can_buffer_inbound: true,
            inbound_records: 2,
            write_ready_on_progress: false,
            unblock_on_writable_wait: false,
            inbound_drains: 0,
            writable_waits: 0,
        });

        write_framed(
            &mut framed,
            b"frame",
            Instant::now() + Duration::from_secs(1),
        )
        .expect("TLS plaintext must move into the framer so another record can be read");

        let writer = &framed.get_inner().0;
        assert_eq!(writer.bytes, b"frame");
        assert_eq!(writer.inbound_drains, 2);
    }

    #[test]
    fn window_drag_shows_contents_not_an_outline() {
        use ironrdp::pdu::rdp::client_info::PerformanceFlags;
        let flags = super::performance_flags();
        assert!(
            !flags.contains(PerformanceFlags::DISABLE_FULLWINDOWDRAG),
            "full-window drag must be enabled; the upstream default disables it"
        );
        assert!(flags.contains(PerformanceFlags::DISABLE_MENUANIMATIONS));
        assert!(flags.contains(PerformanceFlags::ENABLE_FONT_SMOOTHING));
    }

    #[test]
    fn a_known_channel_renders_as_its_name_not_its_bytes() {
        let cliprdr = ironrdp_svc::pdu::gcc::ChannelName::from_utf8("cliprdr").unwrap();
        let label = super::channel_label(&cliprdr);
        assert_eq!(label, "cliprdr");
        assert!(!label.contains("inner"), "got {label}");
    }

    #[test]
    fn an_unknown_channel_still_renders_something_rather_than_nothing() {
        let odd = ironrdp_svc::pdu::gcc::ChannelName::from_utf8("weird").unwrap();
        let label = super::channel_label(&odd);
        assert!(!label.is_empty());
        assert_ne!(label, "cliprdr");
    }
}
