//! Drive an RDP connection through to capability exchange.
//!
//! Sequence: TCP → X.224 security negotiation → TLS (pinned) → CredSSP/NLA → MCS →
//! licensing → Demand Active / Confirm Active → finalization.
//!
//! Records a **payload-free** stage trace as it goes. Under HYBRID_EX everything after
//! negotiation is inside TLS, so a packet capture proves nothing and decrypting one
//! would manufacture exactly the material we are forbidden to keep. Stage name, outcome,
//! elapsed time and negotiated parameters are the evidence; bytes are not.

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
use std::io::Write as _;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing_subscriber::layer::SubscriberExt as _;

pub struct ConnectOptions {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub domain: Option<String>,
    pub desktop_size: DesktopSize,
    pub known_hosts: PathBuf,
    /// Open the graphics channel and observe what the server negotiates, for how long.
    /// `None` connects and disconnects without touching EGFX (P2 behaviour).
    pub observe_egfx: Option<Duration>,
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
fn describe(e: &dyn std::error::Error) -> String {
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
}

impl Trace {
    fn new() -> Self {
        let now = Instant::now();
        Trace {
            started: now,
            last: now,
            stages: Vec::new(),
        }
    }

    fn mark(&mut self, name: &'static str, detail: Option<String>) {
        let now = Instant::now();
        self.stages.push(Stage {
            name,
            elapsed_ms: ms(now - self.last),
            detail,
        });
        self.last = now;
    }
}

fn ms(d: Duration) -> f64 {
    (d.as_micros() as f64) / 1000.0
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
fn send_shutdown<S: std::io::Read + std::io::Write>(
    stage: &ironrdp::session::ActiveStage,
    framed: &mut Framed<S>,
) -> Result<(), ConnectError> {
    let outputs = stage
        .graceful_shutdown()
        .map_err(|e| ConnectError::Protocol(describe(&e)))?;

    for output in outputs {
        if let ironrdp::session::ActiveStageOutput::ResponseFrame(frame) = output {
            framed.write_all(&frame).map_err(ConnectError::Io)?;
        }
    }
    Ok(())
}

/// Pump the session so the graphics channel can open, for a bounded time.
///
/// Bounded two ways: a wall-clock budget, and a socket read timeout so a server that
/// simply says nothing cannot hold us. Both matter — if the server declines to open the
/// graphics channel (see `crate::egfx` on the missing DYNVC_GFX flag) the correct
/// outcome is a report saying so, not a hang.
fn observe_egfx<S: std::io::Read + std::io::Write>(
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

        for out in outputs {
            match out {
                ActiveStageOutput::ResponseFrame(frame) => {
                    framed.write_all(&frame).map_err(ConnectError::Io)?
                }
                ActiveStageOutput::Terminate(_) => return Ok(()),
                _ => {}
            }
        }
    }
    Ok(())
}

pub fn connect(opts: &ConnectOptions, secret: &Secret) -> Result<ConnectReport, ConnectError> {
    let mut trace = Trace::new();
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
        enable_audio_playback: false,
        performance_flags: ironrdp::pdu::rdp::client_info::PerformanceFlags::default(),
        desktop_scale_factor: 0,
        license_cache: None,
        timezone_info: Default::default(),
        compression_type: None,
        // P2 does not render, so a server-drawn pointer would have nowhere to go.
        enable_server_pointer: false,
        pointer_software_rendering: false,
        multitransport_flags: None,
    };

    let client_addr = tcp.local_addr()?;
    // A second handle to the same socket, so the observation phase can bound its reads
    // without imposing a timeout on the CredSSP exchange.
    let socket = tcp.try_clone()?;

    let mut connector = ClientConnector::new(config, client_addr);

    // The dynamic virtual channel must be registered BEFORE connecting: it is announced
    // in the MCS Connect Initial GCC network block, so it cannot be added afterwards.
    let probe = opts.observe_egfx.map(|_| EgfxProbe::new());
    if let Some(probe) = probe.clone() {
        let graphics = ironrdp_egfx::client::GraphicsPipelineClient::new(Box::new(probe), None);
        connector = connector
            .with_static_channel(ironrdp_dvc::DrdynvcClient::new().with_dynamic_channel(graphics));
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
    let verifier = Arc::new(TofuVerifier::new(
        &target,
        opts.known_hosts.clone(),
        store,
        Arc::clone(&provider),
    ));

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
    let stage_log = StageLog::new();
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

    // --- Active stage, EGFX observation, and shutdown ----------------------------
    let desktop_size = result.desktop_size;
    let joined_static_channels: Vec<String> = result
        .static_channels
        .iter()
        .map(|(id, channel)| format!("{:?} (id {:?})", channel.channel_name(), id))
        .collect();
    let mut stage = build_active_stage(result);

    let egfx = match (opts.observe_egfx, probe) {
        (Some(budget), Some(probe)) => {
            let outcome = observe_egfx(&mut stage, &mut framed, &socket, desktop_size, budget);
            trace.mark(
                "egfx_observation",
                Some(match &outcome {
                    Ok(()) => "completed".to_owned(),
                    Err(e) => format!("stopped: {e}"),
                }),
            );
            Some(probe.snapshot())
        }
        _ => None,
    };

    let shutdown = send_shutdown(&stage, &mut framed);
    trace.mark(
        "graceful_shutdown",
        Some(match &shutdown {
            Ok(()) => "shutdown request sent".to_owned(),
            Err(e) => format!("failed: {e}"),
        }),
    );

    let total_ms = ms(trace.started.elapsed());
    Ok(ConnectReport {
        target,
        stages: trace.stages,
        connector_stages,
        tls_version,
        tls_cipher_suite,
        certificate_fingerprint: fingerprint.to_string(),
        trust,
        desktop_width: desktop_size.width,
        desktop_height: desktop_size.height,
        graceful_shutdown: shutdown.is_ok(),
        egfx,
        joined_static_channels,
        total_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Layer {
        message: &'static str,
        inner: Option<Box<Layer>>,
    }

    impl fmt::Display for Layer {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.message)
        }
    }

    impl std::error::Error for Layer {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.inner
                .as_ref()
                .map(|b| b.as_ref() as &(dyn std::error::Error + 'static))
        }
    }

    fn layer(message: &'static str, inner: Option<Layer>) -> Layer {
        Layer {
            message,
            inner: inner.map(Box::new),
        }
    }

    #[test]
    fn describe_walks_the_whole_cause_chain() {
        // The real case: IronRDP's outer message says only "CredSSP"; the useful part —
        // STATUS_LOGON_FAILURE — is two links down.
        let err = layer(
            "CredSSP",
            Some(layer(
                "InvalidToken",
                Some(layer("status is STATUS_LOGON_FAILURE [0xc000006d]", None)),
            )),
        );
        let rendered = describe(&err);
        assert!(rendered.contains("CredSSP"), "{rendered}");
        assert!(rendered.contains("InvalidToken"), "{rendered}");
        assert!(
            rendered.contains("STATUS_LOGON_FAILURE"),
            "the actionable cause must survive: {rendered}"
        );
    }

    #[test]
    fn describe_does_not_repeat_a_cause_already_quoted_by_its_parent() {
        // Many wrappers embed their source's text. Repeating it makes the message worse.
        let err = layer("outer: inner detail", Some(layer("inner detail", None)));
        assert_eq!(describe(&err), "outer: inner detail");
    }

    #[test]
    fn graceful_shutdown_produces_a_shutdown_request_frame() {
        // Exercises the shutdown path without a server. The bug this guards against is
        // silence: abandoning a session instead of ending it leaves a disconnected
        // session alive on the Windows host, and they accumulate until it stops
        // accepting logons.
        let stage = ironrdp::session::ActiveStageBuilder {
            static_channels: ironrdp::svc::StaticChannelSet::new(),
            user_channel_id: 1002,
            io_channel_id: 1003,
            message_channel_id: Some(1004),
            share_id: 0x0001_0001,
            compression_type: None,
            enable_server_pointer: false,
            pointer_software_rendering: false,
        }
        .build();

        let outputs = stage.graceful_shutdown().expect("shutdown must encode");
        let frames: Vec<&Vec<u8>> = outputs
            .iter()
            .filter_map(|o| match o {
                ironrdp::session::ActiveStageOutput::ResponseFrame(f) => Some(f),
                _ => None,
            })
            .collect();

        assert_eq!(frames.len(), 1, "expected exactly one shutdown frame");
        assert!(!frames[0].is_empty(), "shutdown frame must carry bytes");
        // TPKT-framed, like every other X.224-carried PDU on this connection.
        assert_eq!(
            frames[0][0],
            0x03,
            "should be TPKT version 3: {:02x?}",
            &frames[0][..4.min(frames[0].len())]
        );
    }

    #[test]
    fn describe_handles_a_lone_error() {
        assert_eq!(describe(&layer("just this", None)), "just this");
    }
}
