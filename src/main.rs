//! `mdrdp` — connect to an RDP host and show its desktop.
//!
//!     mdrdp                          pick from the favourites launcher
//!     mdrdp <favourite>              connect to a saved favourite by name
//!     mdrdp <host> --user <account>  connect to a host directly
//!
//! The password comes from the OS keychain (service `mdrdp`; direct CLI account `<user>`,
//! GUI account `<user>@<host>:<port>`); it is never an argument, an environment variable,
//! or a log line.
//!
//! With no target named, this process is the launcher: it shows the favourites picker
//! and spawns a *child process* per chosen favourite rather than connecting itself. One
//! OS process per session is the decided architecture — it buys crash isolation and
//! independent clipboard state, so one wedged session cannot take down another.
//!
//! In a session process the ordering is not arbitrary:
//!
//! 1. Build the event loop first — winit allows exactly one per process, and building it
//!    up front means a headless machine fails before a logon is spent.
//! 2. Connect — the server reports the desktop size and the window is built to it.
//! 3. Build the session window on the **main thread**; winit requires that.
//! 4. Only then spawn the session thread, which needs the window's waker.

use ironrdp::connector::DesktopSize;
use mdrdp::audio::{
    AudioPlayback, AudioRing, AudioStatsHandle, DynamicRdpsndListener, RdpsndBackend,
};
use mdrdp::clipboard::{ArboardClipboard, clipboard_channel};
use mdrdp::connect::{Channels, ConnectOptions, RdpsndHandlers, establish};
use mdrdp::favourites::{Favourite, Favourites, WindowSize};
use mdrdp::gfx::{GfxHandler, GfxStatsHandle};
use mdrdp::input::InputEvent;
use mdrdp::metrics::{ResourceMetrics, SessionMetricsReport};
use mdrdp::process_metrics::{ProcessSnapshot, snapshot as process_snapshot};
use mdrdp::session::{self, SessionServices};
use mdrdp::stats::StatsHandle;
use mdrdp::surface::SurfaceStore;
use mdrdp::trust::KnownHosts;
use mdrdp::window::{SessionWindow, WindowConfig};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

fn usage() -> &'static str {
    "usage:\n  \
     mdrdp                            pick from the favourites launcher\n  \
     mdrdp <favourite>                connect to a saved favourite by name\n  \
     mdrdp <host> --user <account>    connect to a host directly\n\n\
     options:\n  \
     --user <account>       keychain account holding the password\n  \
     --port <n>             default 3389\n  \
     --domain <d>           Windows domain\n  \
     --size WxH             session resolution, e.g. 1920x1080\n  \
     --fullscreen           open fullscreen (and renegotiate to the native resolution)\n  \
     --list                 print saved favourites and exit\n  \
     --duration <secs>      disconnect cleanly after N seconds (for scripted runs)\n  \
     --password-stdin       read the password from stdin instead of the keychain\n  \
     --screenshot <file>    write the final frame to a BMP (session pixels on disk)\n  \
     --metrics-json <file>  write a redacted session metrics report as JSON\n  \
     --input-script <file>  inject scripted keystrokes into the session (for tests)\n  \
     --stage-json           print machine-readable connect progress on stdout\n  \
     --capture-failures DIR dump undecodable tiles AND the first raw AVC444 frames\n                            (screen content!) for offline debugging\n\n\
     Flags override whatever the chosen favourite specifies. A [defaults] username in\n\
     favourites.toml is used when neither a flag nor a favourite names an account."
}

/// The size a session gets when nothing asks for a specific one.
///
/// A favourite marked "fullscreen" cannot be resolved to pixels before a window exists,
/// so it lands here too. 1080p is a reasonable desktop on every machine this runs on, and
/// the window is freely resizable afterwards — the session resolution is what is fixed.
const DEFAULT_SIZE: (u16, u16) = (1920, 1080);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Everything the connection needs, after favourites and flags have been reconciled.
#[derive(Debug)]
struct Target {
    host: String,
    port: u16,
    user: String,
    /// Which keychain entry holds the password.
    ///
    /// Separate from `user` because they are genuinely different strings: `temper` wants
    /// the bare UPN `user@example.com` as the logon name — IronRDP rejects
    /// `MicrosoftAccount\\user@example.com` outright as a "mixed username
    /// format" — while the keychain entry may be filed under either. Forcing one to equal
    /// the other makes a working credential unreachable.
    keychain_account: String,
    domain: Option<String>,
    size: (u16, u16),
    /// Whether the session window should open borderless fullscreen.
    fullscreen: bool,
}

fn session_end_state(end: &session::SessionEnd) -> &'static str {
    match end {
        session::SessionEnd::Graceful => "graceful",
        session::SessionEnd::WindowClosed => "window_closed",
        session::SessionEnd::Failed(_) => "failed",
    }
}

fn resource_metrics(
    start: ProcessSnapshot,
    end: ProcessSnapshot,
    elapsed_ms: u64,
) -> Option<ResourceMetrics> {
    Some(ResourceMetrics::new(
        end.user_cpu_ms?.saturating_sub(start.user_cpu_ms?),
        end.system_cpu_ms?.saturating_sub(start.system_cpu_ms?),
        end.peak_resident_bytes?,
        elapsed_ms,
    ))
}

#[allow(clippy::too_many_arguments)]
fn write_session_metrics(
    path: &str,
    started: std::time::Instant,
    resource_start: ProcessSnapshot,
    end: &session::SessionEnd,
    stats: &StatsHandle,
    gfx: &GfxStatsHandle,
    audio: &AudioStatsHandle,
    slots: &mdrdp::stats::SlotStatsHandle,
    joined_channels: &[String],
) -> std::io::Result<()> {
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let resources = resource_metrics(resource_start, process_snapshot(), elapsed_ms);
    let report = SessionMetricsReport::new(
        elapsed_ms,
        session_end_state(end),
        &stats.snapshot(),
        &gfx.snapshot(),
        &audio.snapshot(),
        joined_channels.iter().cloned(),
        resources,
    )
    .with_slots(&slots.snapshot());
    mdrdp::metrics::write_report(std::path::Path::new(path), &report)
}

/// Install a global stderr tracing subscriber when `MDRDP_LOG` names a filter.
///
/// Session-thread instrumentation (EGFX frame/ack traffic, DVC dispatch, clipboard
/// warnings) is written against `tracing`, but a normal run installs no global
/// subscriber, so all of it is invisible. Setting e.g. `MDRDP_LOG=ironrdp_egfx=trace`
/// makes that stream observable without touching a default run.
///
/// `MDRDP_LOG` rather than `RUST_LOG` on purpose: an ambient `RUST_LOG` from the
/// caller's shell must not silently turn a session into a diagnostic one. The connect
/// sequence is unaffected either way — it runs under its own scoped subscriber, which
/// also keeps credential-bearing connector events away from this one.
fn install_diagnostics_subscriber() {
    let Ok(filter) = std::env::var("MDRDP_LOG") else {
        return;
    };
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(filter))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(false),
        )
        .init();
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    install_diagnostics_subscriber();

    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", usage());
        return Ok(());
    }

    // `--` ends the options, so a favourite whose name looks like a flag is still usable
    // as a target. `spawn_session` always passes it, because a name is user data and may
    // legitimately begin with a dash.
    let (positional, args): (Option<String>, Vec<String>) =
        if args.first().is_some_and(|a| a == "--") {
            (args.get(1).cloned(), args.iter().skip(2).cloned().collect())
        } else {
            let p = args.first().filter(|h| !h.starts_with("--")).cloned();
            let rest: Vec<String> = if p.is_some() {
                args.iter().skip(1).cloned().collect()
            } else {
                args.clone()
            };
            (p, rest)
        };

    let mut user: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut domain: Option<String> = None;
    let mut size: Option<(u16, u16)> = None;
    let mut capture: Option<String> = None;
    let mut duration: Option<u64> = None;
    let mut screenshot: Option<String> = None;
    let mut metrics_json: Option<String> = None;
    let mut input_script: Option<String> = None;
    let mut password_stdin = false;
    let mut stage_json = false;
    let mut list_only = false;
    let mut force_fullscreen = false;

    let mut i = 0usize;
    while i < args.len() {
        let value = || -> Result<&String, String> {
            args.get(i + 1)
                .ok_or_else(|| format!("{} needs a value", args[i]))
        };
        match args[i].as_str() {
            "--list" => {
                list_only = true;
                i += 1;
                continue;
            }
            "--password-stdin" => {
                password_stdin = true;
                i += 1;
                continue;
            }
            "--stage-json" => {
                stage_json = true;
                i += 1;
                continue;
            }
            "--fullscreen" => {
                force_fullscreen = true;
                i += 1;
                continue;
            }
            "--user" => user = Some(value()?.clone()),
            "--port" => port = Some(value()?.parse()?),
            "--domain" => domain = Some(value()?.clone()),
            "--capture-failures" => capture = Some(value()?.clone()),
            "--duration" => duration = Some(value()?.parse()?),
            "--screenshot" => screenshot = Some(value()?.clone()),
            "--metrics-json" => metrics_json = Some(value()?.clone()),
            "--input-script" => input_script = Some(value()?.clone()),
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').ok_or("--size wants WxH, e.g. 1280x800")?;
                size = Some((w.parse()?, h.parse()?));
            }
            other => return Err(format!("unknown flag {other}\n{}", usage()).into()),
        }
        i += 2;
    }

    let config_path = Favourites::default_path();
    let config_path_display = config_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(no config directory)".to_owned());

    // A malformed favourites file is reported, never silently treated as empty — the
    // list is the user's data and quietly losing it is worse than refusing to start.
    // With an explicit target on the command line we can still proceed without it.
    let favourites_result = match &config_path {
        Ok(path) => Favourites::load_from(path),
        Err(_) => Favourites::load(),
    };
    let favourites = match favourites_result {
        Ok(f) => f,
        Err(e) if positional.is_some() => {
            eprintln!("warning: could not read favourites ({e}); treating the argument as a host");
            Favourites::default()
        }
        Err(e) => return Err(e.into()),
    };

    // Settings live beside the favourites; the legacy [defaults] username in
    // favourites.toml migrates into settings.toml on first sight and is only read
    // as a fallback after that.
    let settings = match mdrdp::settings::Settings::default_path()
        .map_err(|e| e.to_string())
        .and_then(|p| {
            mdrdp::settings::Settings::load_from(&p)
                .map(|s| (p, s))
                .map_err(|e| e.to_string())
        }) {
        Ok((path, mut s)) => {
            if s.adopt_username(favourites.default_username())
                && let Err(e) = s.save_to(&path)
            {
                eprintln!("warning: could not write settings.toml: {e}");
            }
            s
        }
        Err(e) => {
            eprintln!("warning: {e}; using default settings");
            mdrdp::settings::Settings::default()
        }
    };

    if list_only {
        if favourites.is_empty() {
            println!("no favourites yet — open mdrdp to add one ({config_path_display})");
        } else {
            for f in favourites.iter() {
                let account = f.username.as_deref().unwrap_or("(no account)");
                println!("{:<24} {}@{}:{}", f.name, account, f.host, f.port);
            }
        }
        return Ok(());
    }

    // With no target named, this process is the launcher and never connects: it spawns
    // one child process per chosen favourite. That is the decided architecture (see
    // CLAUDE.md) and it buys crash isolation and independent clipboard state per session
    // for free — a wedged channel in one session cannot take down another.
    //
    // The launcher stays open so several sessions can be started, which `run_app_on_demand`
    // supports directly: each `pick` is an orthogonal run of the same event loop.
    if positional.is_none() {
        let config_path = config_path?;
        let settings_path = mdrdp::settings::Settings::default_path().ok();
        mdrdp::shell::run(favourites, config_path, settings, settings_path)?;
        return Ok(());
    }

    let positional = positional.expect("the launcher path returned above");
    let chosen = favourites.resolve(&positional).cloned();
    // The title names what the user asked for: the favourite if one matched, else the
    // host they typed.
    let display_name = chosen
        .as_ref()
        .map(|f| f.name.clone())
        .unwrap_or_else(|| positional.clone());
    let default_user = settings
        .defaults
        .username
        .clone()
        .or_else(|| favourites.default_username().map(str::to_owned));
    let explicit_size = size.is_some();
    let target = reconcile(
        Some(positional),
        chosen,
        user,
        default_user,
        port,
        domain,
        size,
    )?;

    // Whether the last session against this target closed fullscreen. Remembered state
    // beats the favourite's setting — "reopen how I left it" is the point — but an
    // explicit --size flag beats both and means windowed.
    let state_key = mdrdp::state::target_key(&target.host, target.port);
    let state_path = mdrdp::state::SessionState::default_path();
    let remembered = state_path
        .as_ref()
        .map(|p| mdrdp::state::SessionState::load_from(p))
        .and_then(|s| s.fullscreen_for(&state_key));
    let fullscreen =
        force_fullscreen || (!explicit_size && remembered.unwrap_or(target.fullscreen));

    // Built before connecting so a headless machine fails here, with a clear message,
    // rather than after a connection has been established and a logon spent.
    let event_loop = SessionWindow::event_loop()?;

    // Stdin beats the keychain when asked for: a scripted run must not depend on a
    // keychain that can prompt, and must never be answered by an interactive prompt
    // nobody is there to see.
    let secret = if password_stdin {
        mdrdp::creds::from_stdin()?
    } else if stage_json {
        // A launcher is driving: a missing password is its dialog, not a tty prompt.
        // The child asks over the pipe and blocks on one answer line from stdin;
        // EOF or garbage aborts the connect. The password itself is never echoed,
        // logged, or carried in any event this process emits.
        match mdrdp::creds::lookup(&target.keychain_account) {
            Ok(secret) => secret,
            Err(e) => {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "password_prompt",
                        "account": target.keychain_account,
                        // The reason names the store failure kind, never a secret.
                        "reason": e.to_string(),
                    })
                );
                let mut line = String::new();
                std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line)
                    .map_err(|e| format!("reading the password answer: {e}"))?;
                let parsed: serde_json::Value = serde_json::from_str(&line)
                    .map_err(|_| "the password answer was not understood")?;
                let password = parsed["password"]
                    .as_str()
                    .ok_or("no password was provided")?
                    .to_owned();
                zeroize::Zeroize::zeroize(&mut line);
                mdrdp::creds::secret_from_password(password)?
            }
        }
    } else {
        mdrdp::creds::lookup_or_prompt(&target.keychain_account)?
    };

    let store = Arc::new(Mutex::new(SurfaceStore::new()));
    let (input_tx, input_rx) = mpsc::channel::<InputEvent>();
    let (command_tx, command_rx) = mpsc::channel::<session::SessionCommand>();

    // The stats handle must be taken before the handler is boxed away.
    let handler = GfxHandler::new(Arc::clone(&store));
    // Advertise AVC (AVC444 via V10.7, AVC420 via V8.1) only where connect() will
    // actually configure a decoder, or the server sends H.264 into a void and every
    // video region goes black.
    let handler = if mdrdp::h264::hardware_decode_available() {
        handler.advertising_avc()
    } else {
        handler
    };
    let handler = match &capture {
        Some(dir) => {
            eprintln!("capturing undecodable tiles to {dir} (session content — your call)");
            handler.capturing_failures_to(dir)
        }
        None => handler,
    };
    let gfx_stats = handler.stats();
    let slot_stats = handler.slot_stats();

    // The backend goes into the connection (CLIPRDR is static, so it must be registered
    // before the channel join); the bridge stays here and is driven by the session loop.
    let (clipboard_backend, clipboard_bridge) =
        clipboard_channel(Box::new(ArboardClipboard::new()));

    // Audio. The output stream is opened here and deliberately kept on the main thread for
    // the life of the window: `cpal::Stream` has thread affinity on some platforms, and
    // dropping it stops playback. Only the ring and the counters cross to the session
    // thread, and both are built to be shared.
    //
    // The ring is sized from a nominal 48kHz stereo rather than the device's real format,
    // which is not known until the stream is open. That affects only how many milliseconds
    // of slack it holds; the conversion below uses the device's actual format.
    let audio_stats = AudioStatsHandle::new();
    let audio_ring = AudioRing::for_device(48_000, 2, audio_stats.clone());
    let playback = AudioPlayback::start(audio_ring.clone(), audio_stats.clone());
    let rdpsnd = if playback.is_active() {
        let fmt = playback.format();
        eprintln!("audio: {} Hz, {} channel(s)", fmt.sample_rate, fmt.channels);
        let static_channel = Box::new(RdpsndBackend::new(
            audio_ring.clone(),
            audio_stats.clone(),
            fmt,
        ));
        let dynamic_channel = DynamicRdpsndListener::new(audio_ring, audio_stats.clone(), fmt);
        Some(RdpsndHandlers::new(static_channel, dynamic_channel))
    } else {
        // Joining the channel and then discarding every wave would give the server every
        // reason to believe audio works. Better not to claim it.
        eprintln!("audio: no output device available; continuing without sound");
        None
    };

    // With --stage-json each connect stage goes to stdout as one JSON line, live, so
    // a launcher can drive a progress display. The forwarder thread ends when the
    // channel does; stdout carries only this protocol, human chatter stays on stderr.
    let live_stages = stage_json.then(|| {
        let (tx, rx) = mpsc::channel::<mdrdp::connect::LiveStage>();
        std::thread::spawn(move || {
            while let Ok(stage) = rx.recv() {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "stage",
                        "state": stage.name,
                        "elapsed_ms": stage.elapsed_ms,
                        "qualifier": stage.qualifier,
                    })
                );
            }
        });
        tx
    });

    let opts = ConnectOptions {
        host: target.host.clone(),
        port: target.port,
        username: target.user.clone(),
        domain: target.domain.clone(),
        desktop_size: DesktopSize {
            width: target.size.0,
            height: target.size.1,
        },
        known_hosts: KnownHosts::default_path()?,
        observe_egfx: None,
        // The same --capture opt-in that dumps undecodable ClearCodec tiles also
        // dumps the first few AVC444 payloads for offline replay.
        avc_capture: capture.as_ref().map(std::path::PathBuf::from),
        live_stages,
        // With --stage-json, a first-sight certificate is the launcher's question:
        // emit a cert_prompt event and block on one decision line from stdin. EOF or
        // garbage is a rejection — trust fails closed, never open.
        trust_prompt: stage_json.then(|| {
            std::sync::Arc::new(|sight: &mdrdp::trust::FirstSight| {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "cert_prompt",
                        "host": sight.host,
                        "fingerprint": sight.fingerprint.to_hex(),
                        "store_path": sight.store_path.display().to_string(),
                    })
                );
                let mut line = String::new();
                if std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line).is_err() {
                    return mdrdp::trust::TrustDecision::Reject;
                }
                match serde_json::from_str::<serde_json::Value>(&line)
                    .ok()
                    .and_then(|v| v["decision"].as_str().map(str::to_owned))
                    .as_deref()
                {
                    Some("pin") => mdrdp::trust::TrustDecision::PinAndConnect,
                    Some("once") => mdrdp::trust::TrustDecision::ConnectOnce,
                    _ => mdrdp::trust::TrustDecision::Reject,
                }
            }) as mdrdp::trust::TrustPrompt
        }),
    };

    eprintln!("connecting to {}:{} …", target.host, target.port);
    let established = match establish(
        &opts,
        &secret,
        Channels {
            gfx: Some(Box::new(handler)),
            cliprdr: Some(Box::new(clipboard_backend)),
            rdpsnd,
            // Lets the fullscreen toggle renegotiate the session resolution.
            display_control: true,
        },
    ) {
        Ok(e) => e,
        Err(e) => {
            if stage_json {
                println!(
                    "{}",
                    serde_json::json!({ "event": "failed", "error": e.to_string() })
                );
            }
            return Err(e.into());
        }
    };
    let desktop = established.desktop_size;
    if stage_json {
        println!(
            "{}",
            serde_json::json!({
                "event": "connected",
                "width": desktop.width,
                "height": desktop.height,
            })
        );
    }
    eprintln!(
        "connected: {}x{}, {}, {}",
        desktop.width,
        desktop.height,
        established.report.trust,
        established
            .report
            .tls_version
            .clone()
            .unwrap_or_else(|| "?".to_owned())
    );

    // Window on the main thread, before the session thread that needs its waker.
    let mut window_config = WindowConfig::new(
        format!("mdrdp — {display_name}"),
        desktop.width,
        desktop.height,
    )
    .with_fullscreen(fullscreen);
    if explicit_size {
        // Flags always win: --size names the session resolution, fullscreen or not.
        window_config = window_config.keeping_stated_resolution();
    }
    // Parse and arm the input script before the sender moves into the window. A parse
    // error aborts the run: a scripted test whose script silently did nothing has
    // already produced hours of misleading "healthy" evidence via window-system paths.
    if let Some(path) = &input_script {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("input script {path}: {e}"))?;
        let script = mdrdp::autoinput::Script::parse(&text)
            .map_err(|e| format!("input script {path}: {e}"))?;
        script.spawn(input_tx.clone());
    }
    let window = SessionWindow::new(event_loop, window_config, Arc::clone(&store), input_tx)?;
    let session_stats = StatsHandle::new();
    let session_started = std::time::Instant::now();
    let resource_start = process_snapshot();
    let window = window
        .with_stats(session_stats.clone())
        .with_commands(command_tx)
        .with_diagnostics(mdrdp::window::DiagnosticsUis {
            cache: {
                let stats = session_stats.clone();
                let slots = slot_stats.clone();
                let name = display_name.clone();
                let detail = format!(
                    "{}@{}:{} · pid {}",
                    target.user,
                    target.host,
                    target.port,
                    std::process::id()
                );
                let metrics_dir = settings.diagnostics.metrics_dir.clone();
                let mut window_state = mdrdp::diag::cache::CacheWindow::new();
                Box::new(move |ui: &mut egui::Ui| {
                    let slots_snapshot = slots.snapshot();
                    let cache = stats.snapshot().cache;
                    let action = window_state.ui(
                        ui,
                        &slots_snapshot,
                        &cache,
                        Some((name.as_str(), detail.as_str())),
                        std::time::Instant::now(),
                    );
                    if action == mdrdp::diag::cache::CacheAction::WriteMetrics {
                        match write_cache_metrics(&metrics_dir, &slots_snapshot, &cache) {
                            Ok(path) => eprintln!("metrics: written to {}", path.display()),
                            Err(e) => eprintln!("metrics: could not write: {e}"),
                        }
                    }
                })
            },
            latency: {
                let stats = session_stats.clone();
                let name = display_name.clone();
                let detail = format!(
                    "{}@{}:{} · pid {}",
                    target.user,
                    target.host,
                    target.port,
                    std::process::id()
                );
                let metrics_dir = settings.diagnostics.metrics_dir.clone();
                Box::new(move |ui: &mut egui::Ui| {
                    let snapshot = stats.snapshot();
                    let action = mdrdp::diag::latency::ui_with_session(
                        ui,
                        &snapshot,
                        Some((name.as_str(), detail.as_str())),
                    );
                    if action == mdrdp::diag::latency::LatencyAction::WriteMetrics {
                        match write_diag_metrics(&metrics_dir, &snapshot) {
                            Ok(path) => eprintln!("metrics: written to {}", path.display()),
                            Err(e) => eprintln!("metrics: could not write: {e}"),
                        }
                    }
                })
            },
            channels: {
                let stats = session_stats.clone();
                let gfx = gfx_stats.clone();
                let audio = audio_stats.clone();
                let name = display_name.clone();
                let detail = format!(
                    "{}@{}:{} · pid {}",
                    target.user,
                    target.host,
                    target.port,
                    std::process::id()
                );
                let joined = established.report.joined_static_channels.clone();
                let timeline: Vec<mdrdp::diag::channels::TimelineEntry> = {
                    let mut entries = Vec::new();
                    for stage in &established.report.stages {
                        if stage.name == "post_tls_sequence" {
                            // The connector's own legs break this blob down; insert
                            // them first, then the blob total, mirroring the mock.
                            for leg in &established.report.connector_stages {
                                entries.push(mdrdp::diag::channels::TimelineEntry {
                                    stage: leg.state.clone(),
                                    elapsed_ms: leg.elapsed_ms,
                                });
                            }
                        }
                        entries.push(mdrdp::diag::channels::TimelineEntry {
                            stage: stage.name.to_owned(),
                            elapsed_ms: stage.elapsed_ms,
                        });
                    }
                    entries
                };
                let total_ms = established.report.total_ms;
                Box::new(move |ui: &mut egui::Ui| {
                    let elapsed_ms =
                        u64::try_from(session_started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    let snapshot = build_channels_snapshot(
                        &stats.snapshot(),
                        &gfx.snapshot(),
                        &audio.snapshot(),
                        &joined,
                        timeline.clone(),
                        total_ms,
                        resource_start,
                        elapsed_ms,
                        (name.clone(), detail.clone()),
                    );
                    let _ = mdrdp::diag::channels::ui(ui, &snapshot);
                })
            },
        });
    let fullscreen_at_exit = window.fullscreen_state();

    // The session handle is shared with the window's exit hook so the disconnect happens
    // exactly once, whichever way the loop ends. On macOS a Cmd+Q makes AppKit call
    // `exit(0)` from inside `run()`, so a disconnect written after `run()` returns would
    // simply never happen and the session would be abandoned on the host.
    let session_slot: Arc<Mutex<Option<session::SessionHandle>>> = Arc::new(Mutex::new(None));
    let slot_for_exit = Arc::clone(&session_slot);
    let metrics_path_for_exit = metrics_json.clone();
    let stats_for_exit = session_stats.clone();
    let gfx_for_exit = gfx_stats.clone();
    let audio_for_exit = audio_stats.clone();
    let slots_for_exit = slot_stats.clone();
    let joined_channels_for_exit = established.report.joined_static_channels.clone();
    let state_key_for_exit = state_key.clone();
    let state_path_for_exit = state_path.clone();
    let fullscreen_for_exit = Arc::clone(&fullscreen_at_exit);
    let window = window.on_exit(move || {
        // Remember how the window closed, fullscreen-wise, so the next launch of this
        // target can open the same way. Best-effort: a failed save costs one toggle.
        if let Some(path) = &state_path_for_exit {
            let mut state = mdrdp::state::SessionState::load_from(path);
            state.set_fullscreen(
                &state_key_for_exit,
                fullscreen_for_exit.load(std::sync::atomic::Ordering::Relaxed),
            );
            if let Err(e) = state.save_to(path) {
                eprintln!("warning: could not remember the window state: {e}");
            }
        }
        let handle = slot_for_exit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(handle) = handle {
            let end = handle.shutdown();
            eprintln!("session ended: {end:?}");
            if let Some(path) = &metrics_path_for_exit {
                match write_session_metrics(
                    path,
                    session_started,
                    resource_start,
                    &end,
                    &stats_for_exit,
                    &gfx_for_exit,
                    &audio_for_exit,
                    &slots_for_exit,
                    &joined_channels_for_exit,
                ) {
                    Ok(()) => eprintln!("  metrics: redacted report written to {path}"),
                    Err(error) => eprintln!("  metrics: could not write {path}: {error}"),
                }
            }
        }
    });
    let waker = window.waker();

    // Which redirections are actually live. Worth printing every time: "the clipboard
    // isn't working" and "the clipboard channel never joined" are different problems with
    // the same symptom, and this is the one line that tells them apart.
    eprintln!(
        "channels: {}",
        if established.report.joined_static_channels.is_empty() {
            "(none)".to_owned()
        } else {
            established.report.joined_static_channels.join(", ")
        }
    );
    if !established
        .report
        .joined_static_channels
        .iter()
        .any(|c| c.eq_ignore_ascii_case("cliprdr"))
    {
        eprintln!("note: the server did not join CLIPRDR — clipboard sharing is unavailable");
    }

    let session = session::spawn(
        established,
        Arc::clone(&store),
        input_rx,
        command_rx,
        waker,
        SessionServices {
            clipboard: Some(clipboard_bridge),
            stats: session_stats.clone(),
            gfx: Some(gfx_stats.clone()),
        },
    );

    // A scripted run must end the way a user closing the window does — through the
    // waker, so the session thread still sends a Shutdown Request. Killing the process
    // instead abandons the socket, and abandoned sessions accumulate on the Windows host
    // until it stops accepting logons.
    if let Some(secs) = duration {
        let closer = window.waker();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(secs));
            closer.close();
        });
    }

    *session_slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(session);

    let window_result = window.run();

    // Stop playback before tearing the session down, so the device is released even if the
    // disconnect below takes a moment. Explicit because the drop is otherwise invisible,
    // and an audio stream outliving its session is a confusing thing to debug.
    drop(playback);

    // Window gone: disconnect properly rather than dropping the socket, which would
    // leave a session alive on the host.
    // Before tearing anything down: what was actually on screen? Every other signal this
    // client emits can look healthy while the surface holds garbage, so a frame on disk is
    // the only evidence that the decode path produced a picture rather than a plausible
    // set of counters.
    if let Some(path) = &screenshot {
        let captured = store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .output_surface()
            .map(|s| (s.width, s.height, s.pixels().to_vec()));
        match captured {
            Some((w, h, pixels)) => {
                match mdrdp::screenshot::write_bmp(std::path::Path::new(path), w, h, &pixels) {
                    Ok(()) => eprintln!("  screenshot: {w}x{h} written to {path}"),
                    Err(e) => eprintln!("  screenshot: could not write {path}: {e}"),
                }
            }
            None => eprintln!("  screenshot: no surface was ever mapped to output"),
        }
    }

    // `None` here means the exit hook already disconnected — the Cmd+Q path.
    let end = session_slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .map(|h| h.shutdown());
    let s = gfx_stats.snapshot();
    let cache = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .cache_stats();
    if let Some(end) = end {
        eprintln!("session ended: {end:?}");
    }
    eprintln!(
        "  frames {}  decode errors {}  undecoded regions {}  surface errors {}\n  \
         surfaces +{} -{}  reset {:?}  unhandled pdus {}\n  codecs {:?}",
        s.frames_completed,
        s.decode_errors,
        s.undecoded_regions,
        s.surface_errors,
        s.surfaces_created,
        s.surfaces_deleted,
        s.reset_graphics,
        s.unhandled_pdus,
        s.codec_ids_seen
    );
    match (cache.hit_rate(), cache.byte_savings()) {
        (Some(hit), Some(saved)) => eprintln!(
            "  bitmap cache: {:.0}% of {} lookups hit, saving {:.0}% of painted pixels \
             ({} evictions)",
            hit * 100.0,
            cache.hits + cache.misses,
            saved * 100.0,
            cache.evictions
        ),
        _ => eprintln!("  bitmap cache: never used by this server"),
    }
    let audio = audio_stats.snapshot();
    // Printed unconditionally. Reporting only when packets arrived hides the single most
    // important case — the channel was joined and the server sent nothing — which is
    // exactly what a wrong NO_AUDIO_PLAYBACK flag looks like, and it looks identical to
    // "nothing was playing" if the line is suppressed.
    // Three outcomes, never merged: no channel was ever opened, one was opened and stayed
    // silent, or audio actually played. Only `negotiated_formats` can tell the first two
    // apart — `current_format` alone is `None` for both, and calling that "negotiated no
    // format" blamed a stage that had not been measured.
    match (audio.current_format, audio.negotiated_formats) {
        (Some(fmt), _) => eprintln!(
            "  audio: {} packets at {} Hz/{}ch, {} dropped to overrun, {} underruns",
            audio.packets_received, fmt.sample_rate, fmt.channels, audio.overruns, audio.underruns
        ),
        (None, None) => {
            eprintln!("  audio: no audio channel was opened by the server this session")
        }
        (None, Some(0)) => eprintln!(
            "  audio: formats exchanged, but the server shared none of the formats we offer"
        ),
        (None, Some(n)) => {
            eprintln!("  audio: {n} format(s) negotiated; the server sent no audio this session")
        }
    }
    if !s.decode_error_reasons.is_empty() {
        eprintln!("  decode failures by reason:");
        let mut reasons: Vec<_> = s.decode_error_reasons.iter().collect();
        reasons.sort_by(|a, b| b.1.cmp(a.1));
        for (reason, count) in reasons.iter().take(5) {
            eprintln!("    {count:>5}  {reason}");
        }
    }
    if !s.surface_error_reasons.is_empty() {
        eprintln!("  surface failures by reason:");
        for (reason, count) in &s.surface_error_reasons {
            eprintln!("    {count:>5}  {reason}");
        }
    }
    if s.decode_errors > 0 && capture.is_none() {
        eprintln!(
            "  {} tiles failed to decode. Re-run with --capture-failures <dir> to keep \
             the bytes for offline debugging.",
            s.decode_errors
        );
    }
    if s.undecoded_regions > 0 {
        eprintln!(
            "  {} regions arrived in a surface codec without a decoder; \
             those parts of the desktop will be stale (see the codec list above).",
            s.undecoded_regions
        );
    }

    window_result?;
    Ok(())
}

/// Write a redacted stats-only report from a diagnostics window's button.
///
/// Mid-session, so it carries the session stats alone ("running"); the full report
/// with gfx/audio/channel detail still lands at session end via --metrics-json.
fn write_diag_metrics(
    dir: &str,
    stats: &mdrdp::stats::SessionStats,
) -> Result<std::path::PathBuf, String> {
    let dir = expand_home(dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("mdrdp-latency-{stamp}.json"));
    let payload = serde_json::json!({
        "kind": "latency_snapshot",
        "written_unix": stamp,
        "latency": {
            "count": stats.latency.count(),
            "min_us": stats.latency.min(),
            "max_us": stats.latency.max(),
            "recent": stats.latency.recent().map(|p| {
                serde_json::json!({"p50": p.p50, "p95": p.p95, "p99": p.p99})
            }),
            "baseline": stats.latency.baseline().map(|p| {
                serde_json::json!({"p50": p.p50, "p95": p.p95, "p99": p.p99})
            }),
            "drift_us": stats.latency.drift_us(),
        },
    });
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&payload).expect("plain json"),
    )
    .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

fn expand_home(dir: &str) -> std::path::PathBuf {
    if let Some(rest) = dir.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return std::path::PathBuf::from(home).join(rest);
    }
    std::path::PathBuf::from(dir)
}

/// Assemble the channels-window snapshot from the live handles.
#[allow(clippy::too_many_arguments)]
fn build_channels_snapshot(
    stats: &mdrdp::stats::SessionStats,
    gfx: &mdrdp::gfx::GfxStats,
    audio: &mdrdp::audio::AudioStats,
    joined: &[String],
    timeline: Vec<mdrdp::diag::channels::TimelineEntry>,
    total_ms: f64,
    resource_start: ProcessSnapshot,
    elapsed_ms: u64,
    session: (String, String),
) -> mdrdp::diag::channels::ChannelsSnapshot {
    use mdrdp::diag::channels::{
        ChannelRow, ChannelState, ChannelsSnapshot, CodecBar, ProcessRows,
    };
    let is_joined = |name: &str| joined.iter().any(|c| c.eq_ignore_ascii_case(name));
    let state_for = |name: &str, idle: bool| {
        if !is_joined(name) {
            ChannelState::NotRequested
        } else if idle {
            ChannelState::JoinedIdle
        } else {
            ChannelState::Joined
        }
    };
    let audio_idle = audio.packets_received == 0;
    let channels = vec![
        ChannelRow {
            name: "DRDYNVC",
            description: "dynamic channel transport · carries EGFX and audio".to_owned(),
            state: state_for("drdynvc", false),
            counter: None,
        },
        ChannelRow {
            name: "CLIPRDR",
            description: "clipboard sharing".to_owned(),
            state: state_for("cliprdr", false),
            counter: None,
        },
        ChannelRow {
            name: "RDPSND",
            description: "audio output".to_owned(),
            state: state_for("rdpsnd", audio_idle),
            counter: is_joined("rdpsnd").then(|| format!("{} packets", audio.packets_received)),
        },
        ChannelRow {
            name: "RDPDR",
            description: "device redirection · attached so audio can open".to_owned(),
            state: state_for("rdpdr", true),
            counter: is_joined("rdpdr").then(|| "idle".to_owned()),
        },
        ChannelRow {
            name: "AINPUT",
            description: "not requested · also ECHO, RAIL".to_owned(),
            state: ChannelState::NotRequested,
            counter: None,
        },
    ];
    let codecs: Vec<CodecBar> = gfx
        .codec_ids_seen
        .iter()
        .map(|(name, updates)| CodecBar {
            label: name.clone(),
            updates: *updates,
            painted_bytes: gfx.codec_bytes_painted.get(name).copied().unwrap_or(0),
        })
        .collect();
    let resources = resource_metrics(resource_start, process_snapshot(), elapsed_ms);
    let process = ProcessRows {
        cpu_average_percent: resources.as_ref().and_then(|r| r.average_cpu_percent),
        peak_resident_bytes: resources.as_ref().map(|r| r.peak_resident_bytes),
        frames: stats.frames,
        bytes_in: stats.bytes_in,
    };
    ChannelsSnapshot {
        decode_errors: gfx.decode_errors,
        undecoded_regions: gfx.undecoded_regions,
        unhandled_pdus: gfx.unhandled_pdus,
        channels,
        codecs,
        timeline,
        total_to_first_frame_ms: Some(total_ms),
        process,
        session: Some(session),
    }
}

/// Write a redacted per-slot cache report from the cache window's button.
fn write_cache_metrics(
    dir: &str,
    slots: &mdrdp::stats::SlotStats,
    cache: &mdrdp::stats::CacheStats,
) -> Result<std::path::PathBuf, String> {
    let dir = expand_home(dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("mdrdp-cache-{stamp}.json"));
    let now = std::time::Instant::now();
    let slot_rows: Vec<mdrdp::metrics::SlotMetrics> = slots
        .iter()
        .map(|s| mdrdp::metrics::SlotMetrics::from_slot(s, now))
        .collect();
    let payload = serde_json::json!({
        "kind": "cache_snapshot",
        "written_unix": stamp,
        "aggregate": {
            "hits": cache.hits,
            "misses": cache.misses,
            "evictions": cache.evictions,
            "bytes_served": cache.bytes_served,
            "bytes_from_wire": cache.bytes_from_wire,
        },
        "slots": slot_rows,
    });
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&payload).expect("plain json"),
    )
    .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Reconcile a favourite with command-line flags. Flags always win.
///
/// Pulled out of `run` because this precedence is the one piece of the argument handling
/// that can be silently wrong — connecting as the wrong account, or to the favourite's
/// host when one was typed explicitly — and it is worth a test.
///
/// The account precedence is flag > favourite > `[defaults]` username: an explicit flag
/// is the user speaking now, a favourite is what they saved for this host, and the
/// default is what they use everywhere else.
fn reconcile(
    positional: Option<String>,
    chosen: Option<Favourite>,
    user: Option<String>,
    default_user: Option<String>,
    port: Option<u16>,
    domain: Option<String>,
    size: Option<(u16, u16)>,
) -> Result<Target, String> {
    let host = match (&chosen, &positional) {
        // A resolved favourite names its own host; the argument was its *name*.
        (Some(f), _) => f.host.clone(),
        (None, Some(p)) => p.clone(),
        (None, None) => return Err(usage().to_owned()),
    };

    let user = user
        .or_else(|| chosen.as_ref().and_then(|f| f.username.clone()))
        .or(default_user)
        .ok_or_else(|| {
            format!(
                "no account for {host}: pass --user <account>, set one on the favourite, \
                 or add a [defaults] username to favourites.toml\n{}",
                usage()
            )
        })?;

    let fullscreen = size.is_none()
        && matches!(
            chosen.as_ref().map(|f| f.window_size),
            Some(WindowSize::Fullscreen)
        );
    let size = size.unwrap_or_else(|| match chosen.as_ref().map(|f| f.window_size) {
        Some(WindowSize::Explicit { width, height }) => (width, height),
        _ => DEFAULT_SIZE,
    });

    let keychain_account = chosen
        .as_ref()
        .and_then(|f| f.keychain_account.clone())
        .unwrap_or_else(|| user.clone());

    Ok(Target {
        host,
        keychain_account,
        port: port
            .or_else(|| chosen.as_ref().map(|f| f.port))
            .unwrap_or(mdrdp::favourites::DEFAULT_PORT),
        user,
        domain: domain.or_else(|| chosen.as_ref().and_then(|f| f.domain.clone())),
        size,
        fullscreen,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_documents_the_redacted_metrics_export() {
        let text = usage();
        assert!(text.contains("--metrics-json <file>"));
        assert!(text.contains("redacted"));
    }

    #[test]
    fn resource_report_uses_session_cpu_delta_and_process_peak_memory() {
        let start = ProcessSnapshot {
            user_cpu_ms: Some(100),
            system_cpu_ms: Some(40),
            peak_resident_bytes: Some(1_000),
        };
        let end = ProcessSnapshot {
            user_cpu_ms: Some(260),
            system_cpu_ms: Some(80),
            peak_resident_bytes: Some(2_000),
        };
        let resource = resource_metrics(start, end, 1_000).expect("all counters available");
        assert_eq!(resource.user_cpu_ms, 160);
        assert_eq!(resource.system_cpu_ms, 40);
        assert_eq!(resource.peak_resident_bytes, 2_000);
        assert_eq!(resource.average_cpu_percent, Some(20.0));
    }

    #[test]
    fn session_end_state_never_serialises_an_error_detail() {
        assert_eq!(
            session_end_state(&session::SessionEnd::Graceful),
            "graceful"
        );
        assert_eq!(
            session_end_state(&session::SessionEnd::WindowClosed),
            "window_closed"
        );
    }

    fn temper() -> Favourite {
        Favourite {
            username: Some("saved-user".into()),
            domain: Some("SAVED".into()),
            port: 4000,
            window_size: WindowSize::Explicit {
                width: 1280,
                height: 800,
            },
            ..Favourite::new("Temper", "temper.local")
        }
    }

    #[test]
    fn a_favourite_supplies_every_field_it_knows() {
        let t = reconcile(
            Some("Temper".into()),
            Some(temper()),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            t.host, "temper.local",
            "the argument was a name, not a host"
        );
        assert_eq!(t.user, "saved-user");
        assert_eq!(t.domain.as_deref(), Some("SAVED"));
        assert_eq!(t.port, 4000);
        assert_eq!(t.size, (1280, 800));
        assert!(!t.fullscreen, "an explicit favourite size stays windowed");
    }

    #[test]
    fn every_flag_overrides_the_favourite_it_clashes_with() {
        let t = reconcile(
            Some("Temper".into()),
            Some(temper()),
            Some("flag-user".into()),
            None,
            Some(3389),
            Some("FLAG".into()),
            Some((800, 600)),
        )
        .unwrap();
        // Distinct values throughout, so a field copied from the wrong source shows up.
        assert_eq!(t.user, "flag-user");
        assert_eq!(t.port, 3389);
        assert_eq!(t.domain.as_deref(), Some("FLAG"));
        assert_eq!(t.size, (800, 600));
        assert_eq!(
            t.host, "temper.local",
            "but the host still comes from the favourite"
        );
    }

    #[test]
    fn an_unmatched_argument_is_treated_as_a_bare_host() {
        let t = reconcile(
            Some("192.0.2.50".into()),
            None,
            Some("martin".into()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(t.host, "192.0.2.50");
        assert_eq!(t.port, mdrdp::favourites::DEFAULT_PORT);
        assert_eq!(t.size, DEFAULT_SIZE);
        assert!(!t.fullscreen, "direct hosts stay windowed");
        assert_eq!(t.domain, None);
    }

    #[test]
    fn a_host_with_no_account_anywhere_is_an_error_not_a_guess() {
        let err = reconcile(Some("box".into()), None, None, None, None, None, None).unwrap_err();
        assert!(err.contains("no account for box"), "got: {err}");
    }

    #[test]
    fn the_default_username_covers_a_bare_host() {
        let t = reconcile(
            Some("box".into()),
            None,
            None,
            Some("default-user".into()),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(t.user, "default-user");
        assert_eq!(
            t.keychain_account, "default-user",
            "the default account also names the keychain entry"
        );
    }

    #[test]
    fn a_favourite_account_beats_the_default_and_a_flag_beats_both() {
        let with_favourite = reconcile(
            Some("Temper".into()),
            Some(temper()),
            None,
            Some("default-user".into()),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(with_favourite.user, "saved-user");

        let with_flag = reconcile(
            Some("Temper".into()),
            Some(temper()),
            Some("flag-user".into()),
            Some("default-user".into()),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(with_flag.user, "flag-user");
    }

    #[test]
    fn the_keychain_account_can_differ_from_the_logon_name() {
        // These are genuinely different strings on temper: IronRDP rejects a username
        // that mixes a Domain\\ prefix with a UPN suffix ("mixed username format"), so the
        // logon name must be the bare UPN — while the keychain entry may be filed under
        // the prefixed form. Tying them together makes a working credential unreachable.
        let f = Favourite {
            username: Some("user@example.com".into()),
            keychain_account: Some("MicrosoftAccount\\user@example.com".into()),
            ..Favourite::new("Temper", "temper")
        };
        let t = reconcile(Some("Temper".into()), Some(f), None, None, None, None, None).unwrap();
        assert_eq!(t.user, "user@example.com", "what we log on as");
        assert_eq!(
            t.keychain_account, "MicrosoftAccount\\user@example.com",
            "where the password is stored"
        );
    }

    #[test]
    fn the_keychain_account_defaults_to_the_logon_name() {
        let f = Favourite {
            username: Some("someone@example.com".into()),
            keychain_account: None,
            ..Favourite::new("Plain", "host")
        };
        let t = reconcile(Some("Plain".into()), Some(f), None, None, None, None, None).unwrap();
        assert_eq!(t.keychain_account, t.user, "one name unless told otherwise");
    }

    #[test]
    fn nothing_at_all_is_a_usage_error() {
        assert!(reconcile(None, None, None, None, None, None, None).is_err());
    }

    #[test]
    fn a_fullscreen_favourite_falls_back_to_the_default_size() {
        let f = Favourite {
            username: Some("u".into()),
            window_size: WindowSize::Fullscreen,
            ..Favourite::new("FS", "fs.local")
        };
        let t = reconcile(Some("FS".into()), Some(f), None, None, None, None, None).unwrap();
        assert_eq!(
            t.size, DEFAULT_SIZE,
            "fullscreen cannot be resolved to pixels before a window exists"
        );
        assert!(
            t.fullscreen,
            "fullscreen intent must reach the session window"
        );
    }

    #[test]
    fn a_size_override_forces_a_fullscreen_favourite_to_stay_windowed() {
        let f = Favourite {
            username: Some("fullscreen-user".into()),
            window_size: WindowSize::Fullscreen,
            ..Favourite::new("Fullscreen", "fullscreen.local")
        };
        let t = reconcile(
            Some("Fullscreen".into()),
            Some(f),
            None,
            None,
            None,
            None,
            Some((1366, 768)),
        )
        .unwrap();
        assert_eq!(
            t.size,
            (1366, 768),
            "the flag still controls session resolution"
        );
        assert!(!t.fullscreen, "an explicit size means a windowed session");
    }
}
