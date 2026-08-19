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
use mdrdp::stats::{SlotStatsHandle, StatsHandle};
use mdrdp::surface::SurfaceStore;
use mdrdp::trust::KnownHosts;
use mdrdp::wake::{WakingSender, doorbell};
use mdrdp::window::{SessionWindow, WindowConfig};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// The help screen as it goes into error messages: always plain, never coloured —
/// an error string ends up in pipes and logs, where ANSI codes are noise. The
/// interactive `--help` path colours it separately. The text itself lives in
/// `mdrdp::cli` so the plain and coloured forms cannot drift apart.
fn usage() -> String {
    mdrdp::cli::help_text(false)
}

/// The missing-RDP-account error, raised by `reconcile` for an RDP-only target
/// and by the RDP connect arm when a native-capable target lands on RDP after
/// all — the same words either way, so where it fires is invisible to the user.
fn no_account_error(host: &str) -> String {
    format!(
        "no account for {host}: pass --user <account>, set one on the favourite, \
         or add a [defaults] username to favourites.toml\n{}",
        usage()
    )
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

/// A connection whose window is not yet built: the server has told us the desktop
/// size, nothing is pumping yet. Boxed contents keep the enum pocket-sized.
enum Connected {
    Rdp(Box<mdrdp::connect::Established>),
    Native(Box<mdrdp::native::probe::ProbedTransport>),
}

/// The running session, whichever transport carries it. One slot, one shutdown —
/// the window's exit hook and the epilogue both go through here, so a disconnect
/// happens exactly once regardless of transport.
enum ActiveSession {
    Rdp(session::SessionHandle),
    Native(mdrdp::native::session::NativeHandle),
}

impl ActiveSession {
    fn shutdown(self) -> session::SessionEnd {
        match self {
            ActiveSession::Rdp(handle) => handle.shutdown(),
            ActiveSession::Native(handle) => handle.shutdown(),
        }
    }
}

/// Everything the connection needs, after favourites and flags have been reconciled.
#[derive(Debug)]
struct Target {
    host: String,
    port: u16,
    /// The RDP logon account. `None` is representable only for a native-mode
    /// target (the ssh identity is separate): the RDP connect arm re-imposes the
    /// requirement the moment the transport is RDP — moved out of `reconcile` so
    /// a native connect never demands an account it will not use (review P-M5).
    user: Option<String>,
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
    /// The resolved transport preference (flag > favourite > settings).
    native: mdrdp::favourites::NativeMode,
    /// SSH login for the native transport; `None` lets `~/.ssh/config` decide.
    ssh_user: Option<String>,
}

fn session_end_state(end: &session::SessionEnd) -> &'static str {
    match end {
        session::SessionEnd::Graceful => "graceful",
        session::SessionEnd::WindowClosed => "window_closed",
        session::SessionEnd::ServerEnded(_) => "server_ended",
        session::SessionEnd::Failed(_) => "failed",
        session::SessionEnd::TransportFailed(_) => "transport_failed",
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
/// Print the end-of-session diagnostics, and write the `--screenshot` frame.
///
/// Called from the window's exit hook, which runs at `LoopExiting` on EVERY close. This
/// must NOT live after `SessionWindow::run()`: on macOS a Cmd+Q — or the Quit item in
/// the application menu — makes AppKit call `exit(0)` from inside `run()`, so nothing
/// written after it ever executes. That is how every user-initiated close, the common
/// case, silently discarded the decode-failure reason tally, the surface counters and
/// the `--screenshot` file, while still logging `session ended: …` from this same hook
/// so the log looked complete (MDR-BUG-FLUX-00009). The reason tally is the only record
/// that ever names WHY a frame failed to decode; losing it cost the root cause of
/// MDR-BUG-FLUX-00008.
fn report_session_epilogue(
    store: &Mutex<SurfaceStore>,
    gfx_stats: &GfxStatsHandle,
    audio_stats: &AudioStatsHandle,
    session_stats: &StatsHandle,
    is_native: bool,
    screenshot: Option<&str>,
    capture_configured: bool,
) {
    // First: what was actually on screen? Every other signal this client emits can look
    // healthy while the surface holds garbage, so a frame on disk is the only evidence
    // that the decode path produced a picture rather than a plausible set of counters.
    if let Some(path) = screenshot {
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

    let s = gfx_stats.snapshot();
    let cache = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .cache_stats();

    // The RDP epilogue reads the EGFX pipeline; a native session never fed it, so
    // all-zero counters would be noise pretending to be a report. Its own summary
    // comes from the shared session stats the native sink does feed.
    if is_native {
        let stats = session_stats.snapshot();
        eprintln!(
            "  native: frames {}  bytes in {}  decode errors {}",
            stats.frames, stats.bytes_in, stats.decode_errors
        );
    } else {
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
    if !is_native {
        match (audio.current_format, audio.negotiated_formats) {
            (Some(fmt), _) => eprintln!(
                "  audio: {} packets at {} Hz/{}ch, {} dropped to overrun, {} underruns",
                audio.packets_received,
                fmt.sample_rate,
                fmt.channels,
                audio.overruns,
                audio.underruns
            ),
            (None, None) => {
                eprintln!("  audio: no audio channel was opened by the server this session")
            }
            (None, Some(0)) => eprintln!(
                "  audio: formats exchanged, but the server shared none of the formats we offer"
            ),
            (None, Some(n)) => {
                eprintln!(
                    "  audio: {n} format(s) negotiated; the server sent no audio this session"
                )
            }
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
    if s.decode_errors > 0 && !capture_configured {
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
}

/// `MDRDP_LOG` rather than `RUST_LOG` on purpose: an ambient `RUST_LOG` from the
/// caller's shell must not silently turn a session into a diagnostic one. The connect
/// sequence is unaffected either way — it runs under its own scoped subscriber, which
/// also keeps credential-bearing connector events away from this one.
fn install_diagnostics_subscriber(default_filter: Option<&str>) {
    let filter = match std::env::var("MDRDP_LOG") {
        Ok(filter) => filter,
        // Settings ▸ Diagnostics ▸ Stage log = Verbose supplies a default filter;
        // an explicit MDRDP_LOG always wins over it.
        Err(_) => match default_filter {
            Some(f) => f.to_owned(),
            None => return,
        },
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

/// Ask the driving launcher for a password over the `--stage-json` pipe.
///
/// Blocks on one answer line from stdin; EOF or garbage aborts the connect. The
/// password itself is never echoed, logged, or carried in any event this process
/// emits, and the reason is a failure description, never a secret.
fn ask_password_over_pipe(
    account: &str,
    reason: &str,
) -> Result<mdrdp::creds::Secret, Box<dyn std::error::Error>> {
    println!(
        "{}",
        serde_json::json!({
            "event": "password_prompt",
            "account": account,
            "reason": reason,
        })
    );
    let mut line = String::new();
    std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line)
        .map_err(|e| format!("reading the password answer: {e}"))?;
    let parsed: serde_json::Value =
        serde_json::from_str(&line).map_err(|_| "the password answer was not understood")?;
    let password = parsed["password"]
        .as_str()
        .ok_or("no password was provided")?
        .to_owned();
    zeroize::Zeroize::zeroize(&mut line);
    Ok(mdrdp::creds::secret_from_password(password)?)
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // `deploy` claims the first positional before ANYTHING else — before the
    // global --help scan (deploy has its own), and long before the detach logic
    // (a detached deploy would hand ssh a null stdin and log its prompts into a
    // file). A favourite genuinely named "deploy" stays reachable as
    // `mdrdp -- deploy`.
    if args.first().is_some_and(|a| a == "deploy") {
        std::process::exit(mdrdp::deploy::run(&args[1..]));
    }

    // `ssh-setup` claims its positional for the same reasons `deploy` does, and
    // additionally because it is deploy's prerequisite: a host with no SSH is a
    // host deploy cannot reach at all.
    if args.first().is_some_and(|a| a == "ssh-setup") {
        std::process::exit(mdrdp::sshsetup::run(&args[1..]));
    }

    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "{}",
            mdrdp::cli::help_text(mdrdp::cli::stdout_wants_color())
        );
        return Ok(());
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!(
            "{}",
            mdrdp::cli::version_banner(mdrdp::cli::stdout_wants_color())
        );
        return Ok(());
    }

    // The banner leads every run, on stderr with the rest of the status stream, so
    // stdout consumers (--list, --stage-json) never see it.
    eprintln!(
        "{}",
        mdrdp::cli::version_banner(mdrdp::cli::stderr_wants_color())
    );

    // `--` ends the options, so a favourite whose name looks like a flag is still usable
    // as a target. `spawn_session` always passes it, because a name is user data and may
    // legitimately begin with a dash.
    let (positional, args): (Option<String>, Vec<String>) =
        if args.first().is_some_and(|a| a == "--") {
            (args.get(1).cloned(), args.iter().skip(2).cloned().collect())
        } else {
            let p = args.first().filter(|h| !h.starts_with('-')).cloned();
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
    let mut no_avc = false;
    let mut password_stdin = false;
    let mut ask_password = false;
    let mut stage_json = false;
    let mut list_only = false;
    let mut sessions_only = false;
    let mut force_fullscreen = false;
    let mut foreground = false;
    let mut native_flag = false;
    let mut rdp_flag = false;
    let mut doctor = false;
    // Set by `--clipboard-check <text>`: ask the host whether its interactive
    // session's clipboard holds exactly this. Read-only, and nothing about the
    // host's clipboard comes back but a verdict and two lengths.
    let mut clipboard_check: Option<String> = None;
    let mut ssh_user: Option<String> = None;

    // Human-typed flags carry a single-letter short code as well; harness-facing ones
    // (--stage-json, --metrics-json, …) stay long-only — a script types them once.
    let mut i = 0usize;
    while i < args.len() {
        let value = || -> Result<&String, String> {
            args.get(i + 1)
                .ok_or_else(|| format!("{} needs a value", args[i]))
        };
        match args[i].as_str() {
            "--list" | "-l" => {
                list_only = true;
                i += 1;
                continue;
            }
            "--sessions" | "-S" => {
                sessions_only = true;
                i += 1;
                continue;
            }
            "--clipboard-check" => clipboard_check = Some(value()?.clone()),
            "--doctor" => {
                doctor = true;
                i += 1;
                continue;
            }
            "--password-stdin" => {
                password_stdin = true;
                i += 1;
                continue;
            }
            "--ask-password" => {
                ask_password = true;
                i += 1;
                continue;
            }
            "--stage-json" => {
                stage_json = true;
                i += 1;
                continue;
            }
            "--no-avc" => {
                no_avc = true;
                i += 1;
                continue;
            }
            "--fullscreen" | "-f" => {
                force_fullscreen = true;
                i += 1;
                continue;
            }
            "--foreground" | "-F" => {
                foreground = true;
                i += 1;
                continue;
            }
            "--native" => {
                native_flag = true;
                i += 1;
                continue;
            }
            "--rdp" => {
                rdp_flag = true;
                i += 1;
                continue;
            }
            "--ssh-user" => ssh_user = Some(value()?.clone()),
            "--user" | "-u" => user = Some(value()?.clone()),
            "--port" | "-p" => port = Some(value()?.parse()?),
            "--domain" | "-d" => domain = Some(value()?.clone()),
            "--capture-failures" => capture = Some(value()?.clone()),
            "--duration" | "-t" => duration = Some(value()?.parse()?),
            "--screenshot" => screenshot = Some(value()?.clone()),
            "--metrics-json" => metrics_json = Some(value()?.clone()),
            "--input-script" => input_script = Some(value()?.clone()),
            "--size" | "-s" => {
                let v = value()?;
                let (w, h) = v.split_once('x').ok_or("--size wants WxH, e.g. 1280x800")?;
                size = Some((w.parse()?, h.parse()?));
            }
            other => return Err(format!("unknown flag {other}\n{}", usage()).into()),
        }
        i += 2;
    }

    if native_flag && rdp_flag {
        return Err("--native and --rdp contradict each other; pass at most one".into());
    }

    // `--doctor` diagnoses the NATIVE stack, so pairing it with --rdp asks for
    // two different things at once. `probe stages <host>` is the RDP equivalent
    // and the error says so rather than leaving the reader to guess.
    if doctor && rdp_flag {
        return Err(
            "--doctor diagnoses the native (rhydra) stack, so it cannot be \
             combined with --rdp; for the RDP side use `probe stages <host>`"
                .into(),
        );
    }

    if ask_password && !stage_json {
        return Err("--ask-password needs --stage-json (a driving launcher); \
             for a scripted run use --password-stdin"
            .into());
    }

    // The sessions listing needs no favourites, no credential, and no window: read the
    // presence files, print, done. Handled before the detach decision so it always
    // stays on the invoking terminal.
    if sessions_only {
        let dir = mdrdp::presence::default_dir()
            .ok_or("no config directory, so no sessions can be recorded")?;
        let now = mdrdp::presence::unix_now();
        let sessions = mdrdp::presence::list_from(&dir, now);
        if sessions.is_empty() {
            println!("no active sessions");
        } else {
            print!(
                "{}",
                mdrdp::presence::render_table(&sessions, now, mdrdp::cli::stdout_wants_color())
            );
        }
        return Ok(());
    }

    // The doctor opens no window, reads no credential and takes no session: it
    // asks the agent one question and prints the answer. Handled beside the
    // sessions listing, before the detach decision, so it always stays on the
    // invoking terminal.
    //
    // It implies the native path whatever the favourite says — a favourite set to
    // `native = never` is a statement about how to CONNECT, not a reason to
    // refuse to diagnose — and it never falls back to RDP.
    // Before --doctor: both are read-only diagnostics, and a run that asked for
    // this one wants its verdict, not a health ladder.
    if let Some(expected) = &clipboard_check {
        let host = positional
            .as_deref()
            .ok_or("--clipboard-check needs a host: mdrdp <host> --clipboard-check <text>")?;
        let verdict =
            mdrdp::native::doctor::clipboard_matches(host, ssh_user.as_deref(), expected)?;
        println!("clipboard {host}: {verdict}");
        if !verdict.matches {
            std::process::exit(1);
        }
        return Ok(());
    }

    if doctor {
        let host = positional
            .as_deref()
            .ok_or("--doctor needs a host: mdrdp <host> --doctor")?;
        let (report, failed) = mdrdp::native::doctor::run(host, ssh_user.as_deref())?;
        print!("{report}");
        if failed {
            std::process::exit(1);
        }
        return Ok(());
    }

    // A GUI run started from a terminal gives the prompt back: re-spawn detached and
    // let the lingering parent report how the startup went. Anything that talks to its
    // invoker over stdio, or that a harness waits on, stays attached — see the module
    // docs for the full list.
    let launch_flags = mdrdp::detach::LaunchFlags {
        foreground,
        stage_json,
        password_stdin,
        list_only,
        scripted: duration.is_some()
            || screenshot.is_some()
            || metrics_json.is_some()
            || input_script.is_some()
            || capture.is_some(),
    };
    if mdrdp::detach::should_detach(
        &launch_flags,
        std::io::IsTerminal::is_terminal(&std::io::stderr()),
        mdrdp::detach::already_detached(),
    ) {
        match mdrdp::detach::respawn(positional.is_some()) {
            Ok(mdrdp::detach::StartupOutcome::Running) => return Ok(()),
            Ok(mdrdp::detach::StartupOutcome::Failed { code }) => {
                // The child's log has already been replayed above this line.
                return Err(format!(
                    "the background {} exited during startup{}",
                    if positional.is_some() {
                        "session"
                    } else {
                        "launcher"
                    },
                    code.map(|c| format!(" (exit code {c})"))
                        .unwrap_or_default()
                )
                .into());
            }
            Err(e) => {
                eprintln!("warning: could not detach from the terminal ({e}); continuing attached");
            }
        }
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
    install_diagnostics_subscriber(
        (settings.diagnostics.stage_log == mdrdp::settings::StageLogLevel::Verbose)
            .then_some("ironrdp=debug,mdrdp=debug"),
    );

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
    // Settings ▸ Defaults drive a bare `mdrdp <host>`: they sit between an explicit
    // flag (which always wins) and the built-in constants, and never override a
    // favourite's own choices.
    let port = port.or_else(|| chosen.is_none().then_some(settings.defaults.port));
    let size = size.or_else(|| {
        (chosen.is_none() && settings.defaults.window == mdrdp::settings::WindowMode::Explicit)
            .then_some((settings.defaults.width, settings.defaults.height))
    });
    let force_fullscreen = force_fullscreen
        || (chosen.is_none()
            && size.is_none()
            && settings.defaults.window == mdrdp::settings::WindowMode::Fullscreen);
    let explicit_size = size.is_some();
    let native_mode = mdrdp::native::resolve_mode(
        native_flag,
        rdp_flag,
        chosen.as_ref().map(|f| f.native),
        settings.defaults.native,
    );
    let target = reconcile(
        Some(positional),
        chosen,
        user,
        default_user,
        port,
        domain,
        size,
        native_mode,
        ssh_user,
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

    // With --stage-json each connect stage goes to stdout as one JSON line, live, so
    // a launcher can drive a progress display. The forwarder thread ends when the
    // channel does; stdout carries only this protocol, human chatter stays on stderr.
    // Built before the transport decision: a native probe feeds the same stream.
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

    // ---- The transport decision (native HLD §4) ----
    // `Never` costs nothing. `Auto` probes only hosts the local deploy record names,
    // so it never sprays SSH logon attempts at boxes that never opted in. `Always`
    // probes regardless and fails hard with the remedy rather than silently
    // connecting over RDP against the user's explicit word. Decided entirely before
    // any RDP credential is read: ssh is the native transport's boundary, and under
    // Auto the keychain is consulted only once the probe has settled on RDP.
    let native_conn: Option<mdrdp::native::probe::ProbedTransport> = 'native: {
        if target.native == mdrdp::favourites::NativeMode::Never {
            break 'native None;
        }
        let always = target.native == mdrdp::favourites::NativeMode::Always;
        if !mdrdp::h264::hardware_decode_available() {
            let why =
                "no hardware H.264 decoder is available, and the native transport is AVC-only";
            if always {
                return Err(format!("--native: {why}").into());
            }
            eprintln!("native auto-detect skipped: {why}");
            break 'native None;
        }
        if !always {
            let recorded = mdrdp::native::deployed::default_path()
                .map(|p| {
                    mdrdp::native::deployed::NativeHosts::load_from(&p).is_recorded(&target.host)
                })
                .unwrap_or(false);
            if !recorded {
                // Unrecorded host under Auto: straight RDP at zero probe cost.
                break 'native None;
            }
        }
        eprintln!("probing {} for a native (rhydra) session …", target.host);
        match mdrdp::native::probe::establish(
            &target.host,
            target.ssh_user.as_deref(),
            live_stages.as_ref(),
        ) {
            Ok(transport) => Some(transport),
            Err(failure) if always => {
                let remedy = failure.remedy(&target.host);
                if stage_json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "event": "failed",
                            "error": remedy,
                            "qualifier": failure.qualifier(),
                        })
                    );
                }
                return Err(remedy.into());
            }
            Err(failure) => {
                // Auto falls back to RDP — unless a live native session already holds
                // this host's console, in which case an RDP logon would zero-frame it.
                // An explicit --rdp is not guarded: the user asked.
                if let Some(dir) = mdrdp::presence::default_dir()
                    && let Some(live) = mdrdp::presence::live_native_for(
                        &dir,
                        &target.host,
                        mdrdp::presence::unix_now(),
                    )
                {
                    let refusal = format!(
                        "native probe failed ({}), and a native session to {} is already \
                         live (pid {}): an RDP logon would take the console out from \
                         under it. Close that session first, or connect with --rdp if \
                         that is what you want.",
                        failure.qualifier(),
                        target.host,
                        live.pid
                    );
                    if stage_json {
                        println!(
                            "{}",
                            serde_json::json!({ "event": "failed", "error": refusal })
                        );
                    }
                    return Err(refusal.into());
                }
                eprintln!(
                    "native probe failed ({}); falling back to RDP — {}",
                    failure.qualifier(),
                    failure.remedy(&target.host)
                );
                None
            }
        }
    };
    let is_native = native_conn.is_some();
    if is_native && explicit_size {
        eprintln!("note: --size is ignored on the native transport (the host owns the resolution)");
    }

    let store = Arc::new(Mutex::new(SurfaceStore::new()));
    // Every sender of input or commands rings this doorbell, so the session pump
    // acts immediately instead of on its next read timeout.
    let (session_bell, session_wake_rx) =
        doorbell().map_err(|e| format!("session doorbell: {e}"))?;
    let (raw_input_tx, input_rx) = mpsc::channel::<InputEvent>();
    let (raw_command_tx, command_rx) = mpsc::channel::<session::SessionCommand>();
    let input_tx = WakingSender::new(raw_input_tx, session_bell.clone());
    let command_tx = WakingSender::new(raw_command_tx, session_bell.clone());

    // The EGFX handler is RDP machinery; a native session keeps fresh (all-zero)
    // stats handles so the diagnostics windows and the exit report still have
    // something honest to read. The stats handles must be taken before the handler
    // is boxed away into the connection.
    let (handler, gfx_stats, slot_stats) = if is_native {
        (None, GfxStatsHandle::new(), SlotStatsHandle::new())
    } else {
        let handler = GfxHandler::new(Arc::clone(&store));
        // Advertise AVC (AVC444 via V10.7, AVC420 via V8.1) only where connect() will
        // actually configure a decoder, or the server sends H.264 into a void and every
        // video region goes black.
        // --no-avc withholds the AVC capability sets entirely, so the server falls
        // back to its non-AVC mix (ClearCodec / RFX Progressive). A diagnostic lever:
        // codec A/B comparisons on the same host, and triage when an AVC decode bug
        // is suspected. The decoder requirement is unchanged when it is off.
        let handler = if !no_avc && mdrdp::h264::hardware_decode_available() {
            handler.advertising_avc()
        } else {
            handler
        };
        if no_avc {
            eprintln!("AVC withheld (--no-avc): the server will fall back to its non-AVC codecs");
        }
        let handler = match &capture {
            Some(dir) => {
                eprintln!("capturing undecodable tiles to {dir} (session content — your call)");
                handler.capturing_failures_to(dir)
            }
            None => handler,
        };
        let gfx_stats = handler.stats();
        let slot_stats = handler.slot_stats();
        (Some(handler), gfx_stats, slot_stats)
    };

    // The direction gate is shared by both transports: the same Settings choice
    // decides what CLIPRDR may carry and what the native auxiliary channel may
    // carry, so the two cannot disagree about what the user asked for.
    let clipboard_to_remote = matches!(
        settings.clipboard.direction,
        mdrdp::settings::ClipboardDirection::Both | mdrdp::settings::ClipboardDirection::ToRemote
    );
    let clipboard_from_remote = matches!(
        settings.clipboard.direction,
        mdrdp::settings::ClipboardDirection::Both | mdrdp::settings::ClipboardDirection::FromRemote
    );

    // The backend goes into the connection (CLIPRDR is static, so it must be registered
    // before the channel join); the bridge stays here and is driven by the session loop.
    // The native transport carries its clipboard on the auxiliary channel instead, and
    // opens its own OS handle inside the session (tranche 5).
    let clipboard_enabled =
        !is_native && settings.clipboard.direction != mdrdp::settings::ClipboardDirection::Off;
    let clipboard = (!is_native).then(|| {
        let (backend, bridge) = clipboard_channel(Box::new(ArboardClipboard::new()));
        let bridge = bridge.with_policy(mdrdp::clipboard::ClipboardPolicy {
            to_remote: clipboard_to_remote,
            from_remote: clipboard_from_remote,
            max_image_bytes: settings.clipboard.max_image_bytes,
            paste_timeout_ms: settings.clipboard.timeout_secs.saturating_mul(1000),
        });
        (backend, bridge)
    });

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
    // Settings ▸ Audio: playback off means no device is opened and no RDPSND channel
    // is claimed — the honest form of "no sound", not a joined channel that discards.
    // Native has no audio yet (tranche 6): no device is opened and no channel claimed.
    let playback = if settings.audio.playback && !is_native {
        AudioPlayback::start(audio_ring.clone(), audio_stats.clone())
    } else {
        AudioPlayback::disabled(audio_ring.clone())
    };
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
        if !is_native {
            eprintln!("audio: no output device available; continuing without sound");
        }
        None
    };

    // The clipboard bridge stays out here (the session loop drives it); the backend
    // goes into the RDP connection below.
    let (clipboard_backend, clipboard_bridge) = match clipboard {
        Some((backend, bridge)) => (Some(backend), Some(bridge)),
        None => (None, None),
    };

    let connected = if let Some(transport) = native_conn {
        Connected::Native(Box::new(transport))
    } else {
        // The transport is RDP, so now — and only now — an RDP account is required.
        // Deferred from `reconcile` so a native connect never demands one (P-M5).
        if target.user.is_none() {
            return Err(no_account_error(&target.host).into());
        }

        // Stdin beats the keychain when asked for: a scripted run must not depend on a
        // keychain that can prompt, and must never be answered by an interactive prompt
        // nobody is there to see. This read happens only once the transport is RDP —
        // the native transport authenticates over ssh and reads no RDP credential.
        let secret = if password_stdin {
            mdrdp::creds::from_stdin()?
        } else if stage_json && ask_password {
            // The launcher saw a sign-in rejected with the stored password and wants a
            // fresh one; the keychain is deliberately not consulted.
            ask_password_over_pipe(
                &target.keychain_account,
                "the saved password was not accepted",
            )?
        } else if stage_json {
            // A launcher is driving: a missing password is its dialog, not a tty prompt.
            match mdrdp::creds::lookup(&target.keychain_account) {
                Ok(secret) => secret,
                // The reason names the store failure kind, never a secret.
                Err(e) => ask_password_over_pipe(&target.keychain_account, &e.to_string())?,
            }
        } else if mdrdp::detach::already_detached() {
            // No terminal behind us: reading the tty from a background process group would
            // stop the process on SIGTTIN, invisibly. Fail with a message in the log — the
            // lingering parent replays it — rather than hang where nobody can answer.
            mdrdp::creds::lookup(&target.keychain_account)?
        } else {
            mdrdp::creds::lookup_or_prompt(&target.keychain_account)?
        };
        // A session that will open fullscreen connects AT the resolution and scale the
        // window would otherwise renegotiate to. Two wins: the server renders at the right
        // DPI from logon (a mid-session DPI change leaves every non-DPI-aware remote app
        // DWM-stretched and blurry until relaunch), and there is no resize round at all —
        // the window's start-up request matches the session state and is skipped. Probe
        // failure (headless, non-macOS) falls back to today's connect-then-renegotiate.
        // The probe reads the *primary* monitor; if the window actually opens elsewhere,
        // the start-up renegotiation still corrects it.
        let fullscreen_plan =
            (fullscreen && !explicit_size && settings.graphics.dynamic_resolution)
                .then(mdrdp::display::primary_display)
                .flatten()
                .map(|d| {
                    mdrdp::session::fullscreen_request(
                        d.width,
                        d.height,
                        d.scale_percent,
                        settings.graphics.integer_fullscreen_fit,
                    )
                });

        #[allow(clippy::cast_possible_truncation)]
        let opts = ConnectOptions {
            host: target.host.clone(),
            port: target.port,
            username: target.user.clone().expect("checked at the top of this arm"),
            domain: target.domain.clone(),
            // fullscreen_request only returns encodable sizes (≤ 4096x2304), so u16 holds.
            desktop_size: match fullscreen_plan {
                Some((width, height, _)) => DesktopSize {
                    width: width as u16,
                    height: height as u16,
                },
                None => DesktopSize {
                    width: target.size.0,
                    height: target.size.1,
                },
            },
            desktop_scale_percent: fullscreen_plan.and_then(|(_, _, scale)| scale),
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
                    if std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line).is_err()
                    {
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
        let handler = handler.expect("the RDP path always builds a gfx handler");
        match establish(
            &opts,
            &secret,
            Channels {
                gfx: Some(Box::new(handler)),
                cliprdr: clipboard_backend
                    .filter(|_| clipboard_enabled)
                    .map(|b| Box::new(b) as Box<dyn ironrdp::cliprdr::backend::CliprdrBackend>),
                rdpsnd,
                // Lets the fullscreen toggle renegotiate the session resolution.
                display_control: true,
            },
        ) {
            Ok(e) => Connected::Rdp(Box::new(e)),
            Err(e) => {
                if stage_json {
                    println!(
                        "{}",
                        serde_json::json!({ "event": "failed", "error": e.to_string() })
                    );
                }
                return Err(e.into());
            }
        }
    };

    let desktop = match &connected {
        Connected::Rdp(established) => established.desktop_size,
        Connected::Native(transport) => {
            // The header's u32 dimensions size a real display; anything that does
            // not fit a session's u16 is the server talking nonsense.
            let width = u16::try_from(transport.conn.header.width).map_err(|_| {
                format!(
                    "host reports an impossible width {}",
                    transport.conn.header.width
                )
            })?;
            let height = u16::try_from(transport.conn.header.height).map_err(|_| {
                format!(
                    "host reports an impossible height {}",
                    transport.conn.header.height
                )
            })?;
            DesktopSize { width, height }
        }
    };
    let transport_label = match &connected {
        Connected::Rdp(_) => mdrdp::presence::TRANSPORT_RDP,
        Connected::Native(_) => mdrdp::presence::TRANSPORT_NATIVE,
    };
    if stage_json {
        println!(
            "{}",
            serde_json::json!({
                "event": "connected",
                "width": desktop.width,
                "height": desktop.height,
                "transport": transport_label,
            })
        );
    }
    // The prefix is the marker a detaching parent watches the log for; the tail
    // names the transport's trust story (TLS for RDP, the ssh tunnel's wire for native).
    match &connected {
        Connected::Rdp(established) => eprintln!(
            "{} {}x{}, {}, {}",
            mdrdp::detach::CONNECTED_MARKER,
            desktop.width,
            desktop.height,
            established.report.trust,
            established
                .report
                .tls_version
                .clone()
                .unwrap_or_else(|| "?".to_owned())
        ),
        Connected::Native(transport) => eprintln!(
            "{} {}x{}, native, wire v{}",
            mdrdp::detach::CONNECTED_MARKER,
            desktop.width,
            desktop.height,
            transport.conn.header.wire_version
        ),
    }

    // Transport visibility: the title base says which pipe carries the pixels, and
    // every diagnostics window's identity line repeats it.
    let title_base = match &connected {
        Connected::Rdp(_) => format!("mdrdp — {display_name}"),
        Connected::Native(_) => format!("mdrdp — {display_name} (native)"),
    };
    let diag_detail = match &connected {
        Connected::Rdp(_) => format!(
            "{}@{}:{} · pid {}",
            target.user.as_deref().unwrap_or("?"),
            target.host,
            target.port,
            std::process::id()
        ),
        Connected::Native(_) => format!("{} (native) · pid {}", target.host, std::process::id()),
    };

    // Window on the main thread, before the session thread that needs its waker.
    let mut window_config = WindowConfig::new(title_base, desktop.width, desktop.height)
        .with_dock_label(display_name.clone())
        .with_fullscreen(fullscreen)
        .with_overlay_on_start(settings.diagnostics.overlay_on_connect)
        .with_dynamic_resolution(settings.graphics.dynamic_resolution)
        .with_integer_fullscreen_fit(settings.graphics.integer_fullscreen_fit);
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
        .with_transients({
            // Watches counters that mean "something the user should know just broke".
            // Polled on damage and each 1 Hz tick; cheap by construction.
            let audio = audio_stats.clone();
            let mut seen_device_errors = 0u64;
            Box::new(move || {
                let snapshot = audio.snapshot();
                let mut report = mdrdp::window::TransientReport::default();
                if snapshot.device_errors > seen_device_errors {
                    seen_device_errors = snapshot.device_errors;
                    report.toasts.push(mdrdp::window::Toast {
                        warn: true,
                        title: "Audio device lost".to_owned(),
                        body: "playback stopped".to_owned(),
                    });
                }
                if snapshot.device_errors > 0 {
                    report.warn_line = Some("audio   device errors; playback degraded".to_owned());
                }
                report
            })
        })
        .with_diagnostics(mdrdp::window::DiagnosticsUis {
            cache: {
                let stats = session_stats.clone();
                let slots = slot_stats.clone();
                let name = display_name.clone();
                let detail = diag_detail.clone();
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
                    action == mdrdp::diag::cache::CacheAction::Close
                })
            },
            latency: {
                let stats = session_stats.clone();
                let name = display_name.clone();
                let detail = diag_detail.clone();
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
                    action == mdrdp::diag::latency::LatencyAction::Close
                })
            },
            channels: {
                let stats = session_stats.clone();
                let gfx = gfx_stats.clone();
                let audio = audio_stats.clone();
                let name = display_name.clone();
                let detail = diag_detail.clone();
                let joined = match &connected {
                    Connected::Rdp(established) => {
                        established.report.joined_static_channels.clone()
                    }
                    // Honest emptiness: a native session joins no static channels.
                    Connected::Native(_) => Vec::new(),
                };
                let timeline: Vec<mdrdp::diag::channels::TimelineEntry> = match &connected {
                    Connected::Rdp(established) => {
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
                    }
                    // The window still opens on a native session; one row says why
                    // there is no RDP connect timeline rather than looking broken.
                    Connected::Native(_) => vec![mdrdp::diag::channels::TimelineEntry {
                        stage: "n/a — native transport".to_owned(),
                        elapsed_ms: 0.0,
                    }],
                };
                let total_ms = match &connected {
                    Connected::Rdp(established) => established.report.total_ms,
                    Connected::Native(_) => 0.0,
                };
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
                    mdrdp::diag::channels::ui(ui, &snapshot)
                        == mdrdp::diag::channels::ChannelsAction::Close
                })
            },
        });
    let fullscreen_at_exit = window.fullscreen_state();

    // The session handle is shared with the window's exit hook so the disconnect happens
    // exactly once, whichever way the loop ends. On macOS a Cmd+Q makes AppKit call
    // `exit(0)` from inside `run()`, so a disconnect written after `run()` returns would
    // simply never happen and the session would be abandoned on the host.
    let session_slot: Arc<Mutex<Option<ActiveSession>>> = Arc::new(Mutex::new(None));
    // How the session ended, written by whichever path performed the shutdown. The
    // exit hook runs at LoopExiting on EVERY close (not only Cmd+Q), so by the time
    // the epilogue runs the handle is long gone — this slot is what survives.
    let session_end: Arc<Mutex<Option<session::SessionEnd>>> = Arc::new(Mutex::new(None));
    let end_for_exit = Arc::clone(&session_end);
    let slot_for_exit = Arc::clone(&session_slot);
    let metrics_path_for_exit = metrics_json.clone();
    let stats_for_exit = session_stats.clone();
    let gfx_for_exit = gfx_stats.clone();
    let audio_for_exit = audio_stats.clone();
    let slots_for_exit = slot_stats.clone();
    let joined_channels_for_exit = match &connected {
        Connected::Rdp(established) => established.report.joined_static_channels.clone(),
        Connected::Native(_) => Vec::new(),
    };
    let established_channels = joined_channels_for_exit.clone();
    let state_key_for_exit = state_key.clone();
    let state_path_for_exit = state_path.clone();
    let fullscreen_for_exit = Arc::clone(&fullscreen_at_exit);
    // The epilogue runs from the exit hook so a Cmd+Q cannot skip it, and the post-`run`
    // path only covers the case where the hook somehow did not fire. This flag is what
    // keeps it to exactly one report either way.
    let reported = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reported_for_exit = Arc::clone(&reported);
    let store_for_exit = Arc::clone(&store);
    let screenshot_for_exit = screenshot.clone();
    let capture_configured = capture.is_some();
    // Keep a presence file alive for `mdrdp --sessions` in other processes. The exit
    // hook both stops the writer and removes the file itself: on macOS Cmd+Q the
    // process exits without unwinding, so the writer thread may never see the flag.
    let presence_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let presence_dir = mdrdp::presence::default_dir();
    if let Some(dir) = presence_dir.clone() {
        let stats = session_stats.clone();
        let store = Arc::clone(&store);
        let name = display_name.clone();
        let host = target.host.clone();
        // A native session's identity is its ssh login (possibly ~/.ssh/config's
        // choice, hence empty), not the RDP account it never used.
        let user = match &connected {
            Connected::Rdp(_) => target.user.clone().unwrap_or_default(),
            Connected::Native(_) => target.ssh_user.clone().unwrap_or_default(),
        };
        let port = target.port;
        let pid = std::process::id();
        let started_unix = mdrdp::presence::unix_now();
        let (mut width, mut height) = (desktop.width, desktop.height);
        let mut codec = mdrdp::presence::CodecTracker::default();
        mdrdp::presence::spawn_writer(dir, Arc::clone(&presence_stop), move || {
            let s = stats.snapshot();
            if let Some(surface) = store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .output_surface()
            {
                (width, height) = (surface.width, surface.height);
            }
            mdrdp::presence::SessionPresence {
                pid,
                name: name.clone(),
                host: host.clone(),
                port,
                user: user.clone(),
                version: mdrdp::cli::VERSION.to_owned(),
                transport: transport_label.to_owned(),
                width,
                height,
                started_unix,
                updated_unix: mdrdp::presence::unix_now(),
                frames: s.frames,
                bytes_in: s.bytes_in,
                codec: codec.observe(&s.codec_painted),
                latency_p50_us: s.latency.recent().map(|p| p.p50),
                frame_gap_p50_us: s.frame_gap.recent().map(|p| p.p50),
                decode_errors: s.decode_errors,
                codecs: s.codecs.clone(),
            }
        });
    }
    let presence_stop_for_exit = Arc::clone(&presence_stop);
    let window = window.on_exit(move || {
        presence_stop_for_exit.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(dir) = &presence_dir {
            mdrdp::presence::remove_from(dir, std::process::id());
        }
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
            // Moved in whole (SessionEnd is not Clone); the epilogue takes it back out.
            *end_for_exit
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(end);
        }
        // Outside the handle check on purpose: the diagnostics describe the session that
        // ran, and are worth printing even when the handle was already taken elsewhere.
        if !reported_for_exit.swap(true, std::sync::atomic::Ordering::Relaxed) {
            report_session_epilogue(
                &store_for_exit,
                &gfx_for_exit,
                &audio_for_exit,
                &stats_for_exit,
                is_native,
                screenshot_for_exit.as_deref(),
                capture_configured,
            );
        }
    });
    let waker = window.waker();

    // Which redirections are actually live. Worth printing every time: "the clipboard
    // isn't working" and "the clipboard channel never joined" are different problems with
    // the same symptom, and this is the one line that tells them apart. A native session
    // says so explicitly rather than printing "(none)" and looking broken.
    match &connected {
        Connected::Rdp(established) => {
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
                eprintln!(
                    "note: the server did not join CLIPRDR — clipboard sharing is unavailable"
                );
            }
        }
        Connected::Native(_) => {
            eprintln!(
                "channels: n/a — native transport (clipboard and audio land in later tranches)"
            );
        }
    }

    let session = match connected {
        Connected::Rdp(established) => ActiveSession::Rdp(session::spawn(
            *established,
            Arc::clone(&store),
            input_rx,
            command_rx,
            waker,
            SessionServices {
                clipboard: clipboard_enabled.then_some(clipboard_bridge).flatten(),
                stats: session_stats.clone(),
                gfx: Some(gfx_stats.clone()),
            },
            session_bell,
            session_wake_rx,
        )),
        Connected::Native(transport) => ActiveSession::Native(
            mdrdp::native::session::spawn(
                *transport,
                mdrdp::h264::hardware_decoder(),
                Arc::clone(&store),
                input_rx,
                command_rx,
                waker,
                session_stats.clone(),
                session_wake_rx,
                mdrdp::native::clipboard::Policy {
                    to_remote: clipboard_to_remote,
                    from_remote: clipboard_from_remote,
                    // Text only this tranche, so the image ceiling does not
                    // apply; the wire ceiling is the binding one and the Policy
                    // default already carries it.
                    ..Default::default()
                },
            )
            .map_err(|e| format!("native session: {e}"))?,
        ),
    };

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

    // `run` hands the loop back so the Session-ended/lost epilogue dialog can run
    // one more on-demand cycle on it. On the Cmd+Q path it never returns at all.
    let window_result = window.run();

    // Stop playback before tearing the session down, so the device is released even if the
    // disconnect below takes a moment. Explicit because the drop is otherwise invisible,
    // and an audio stream outliving its session is a confusing thing to debug.
    drop(playback);

    // Window gone: disconnect properly rather than dropping the socket, which would
    // leave a session alive on the host.
    //
    // The screenshot and the diagnostics summary used to live here. They now run from
    // the exit hook (`report_session_epilogue`), because this point is never reached on
    // a macOS Cmd+Q — see MDR-BUG-FLUX-00009.

    // Usually the exit hook has already disconnected (it runs at LoopExiting for
    // every close); this direct take only matters if the hook somehow did not run.
    let direct_end = session_slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .map(|h| h.shutdown());
    if let Some(end) = &direct_end {
        // The hook prints its own line; this branch only runs when it did not.
        eprintln!("session ended: {end:?}");
    }
    let end = direct_end.or_else(|| {
        session_end
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    });
    // Normally the hook already reported; this covers the case where it did not run.
    if !reported.swap(true, std::sync::atomic::Ordering::Relaxed) {
        report_session_epilogue(
            &store,
            &gfx_stats,
            &audio_stats,
            &session_stats,
            is_native,
            screenshot.as_deref(),
            capture.is_some(),
        );
    }
    // Still needed below: the end dialog quotes the cache saving, and the cache report
    // writes the raw counters.
    let cache = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .cache_stats();

    // The §7 epilogue dialogs, for interactive sessions only: a scripted run has
    // nobody to click Done, and its exit code already says what happened.
    let scripted = duration.is_some() || screenshot.is_some() || input_script.is_some();
    match window_result {
        Ok(mut event_loop) => {
            if !scripted && let Some(end) = &end {
                use mdrdp::disconnect::LostSession;
                use mdrdp::ui::end_dialog::EndOutcome;
                let outcome = match end {
                    // Classified rather than printed raw: the chain is written for us
                    // and reaches the log above either way (MDR-BUG-FLUX-00014).
                    session::SessionEnd::Failed(reason) => {
                        EndOutcome::Lost(LostSession::from_connect_error(reason))
                    }
                    session::SessionEnd::TransportFailed(reason) => {
                        EndOutcome::Lost(LostSession::from_transport(reason))
                    }
                    session::SessionEnd::ServerEnded(farewell) => {
                        EndOutcome::ServerEnded(farewell.clone())
                    }
                    session::SessionEnd::Graceful | session::SessionEnd::WindowClosed => {
                        EndOutcome::Ended
                    }
                };
                let stats_snapshot = session_stats.snapshot();
                let info = mdrdp::ui::end_dialog::EndInfo {
                    outcome,
                    session_name: display_name.clone(),
                    duration_secs: session_started.elapsed().as_secs(),
                    drift_ms: stats_snapshot.latency.drift_us().map(|d| d as f64 / 1000.0),
                    cache_share: cache.byte_savings(),
                };
                match mdrdp::ui::end_dialog::show(&mut event_loop, info) {
                    Some(mdrdp::ui::end_dialog::EndChoice::SaveMetricsAndClose) => {
                        let dir = expand_home(&settings.diagnostics.metrics_dir);
                        if let Err(e) = std::fs::create_dir_all(&dir) {
                            eprintln!("metrics: could not create {}: {e}", dir.display());
                        } else {
                            let stamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            let path = dir.join(format!("mdrdp-session-{stamp}.json"));
                            let path_text = path.display().to_string();
                            match write_session_metrics(
                                &path_text,
                                session_started,
                                resource_start,
                                end,
                                &session_stats,
                                &gfx_stats,
                                &audio_stats,
                                &slot_stats,
                                &established_channels,
                            ) {
                                Ok(()) => eprintln!("metrics: written to {path_text}"),
                                Err(e) => eprintln!("metrics: could not write {path_text}: {e}"),
                            }
                        }
                    }
                    Some(mdrdp::ui::end_dialog::EndChoice::Reconnect) => {
                        // A fresh process, same target: re-exec ourselves with the
                        // original arguments. The new session does its own logon.
                        if let Ok(exe) = std::env::current_exe() {
                            let args: Vec<String> = std::env::args().skip(1).collect();
                            if let Err(e) = std::process::Command::new(exe).args(args).spawn() {
                                eprintln!("could not reconnect: {e}");
                            }
                        }
                    }
                    Some(mdrdp::ui::end_dialog::EndChoice::Done) | None => {}
                }
            }
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
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
#[allow(clippy::too_many_arguments)]
fn reconcile(
    positional: Option<String>,
    chosen: Option<Favourite>,
    user: Option<String>,
    default_user: Option<String>,
    port: Option<u16>,
    domain: Option<String>,
    size: Option<(u16, u16)>,
    native: mdrdp::favourites::NativeMode,
    ssh_user: Option<String>,
) -> Result<Target, String> {
    let host = match (&chosen, &positional) {
        // A resolved favourite names its own host; the argument was its *name*.
        (Some(f), _) => f.host.clone(),
        (None, Some(p)) => p.clone(),
        (None, None) => return Err(usage()),
    };

    let user = user
        .or_else(|| chosen.as_ref().and_then(|f| f.username.clone()))
        .or(default_user);
    // The account is an RDP credential, so only a target that can end up on RDP
    // needs one here. A native-capable target defers the requirement to the RDP
    // connect arm — demanded then with this same message — so `--native` against
    // a host with no saved account works without inventing an unused logon.
    if user.is_none() && native == mdrdp::favourites::NativeMode::Never {
        return Err(no_account_error(&host));
    }

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
        .or_else(|| user.clone())
        .unwrap_or_default();

    // SSH identity: flag beats favourite; absent means `~/.ssh/config` decides.
    let ssh_user = ssh_user.or_else(|| chosen.as_ref().and_then(|f| f.ssh_user.clone()));

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
        native,
        ssh_user,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The pre-native reconcile shape: transport Auto, no ssh identity. Keeps the
    /// account/port/size precedence tests focused on what they test.
    #[allow(clippy::too_many_arguments)]
    fn reconcile_basic(
        positional: Option<String>,
        chosen: Option<Favourite>,
        user: Option<String>,
        default_user: Option<String>,
        port: Option<u16>,
        domain: Option<String>,
        size: Option<(u16, u16)>,
    ) -> Result<Target, String> {
        reconcile(
            positional,
            chosen,
            user,
            default_user,
            port,
            domain,
            size,
            mdrdp::favourites::NativeMode::Auto,
            None,
        )
    }

    #[test]
    fn ssh_user_flag_beats_favourite_and_absence_defers_to_ssh_config() {
        let mut f = temper();
        f.ssh_user = Some("saved-ssh".into());
        let with_flag = reconcile(
            Some("Temper".into()),
            Some(f.clone()),
            None,
            None,
            None,
            None,
            None,
            mdrdp::favourites::NativeMode::Auto,
            Some("flag-ssh".into()),
        )
        .unwrap();
        assert_eq!(with_flag.ssh_user.as_deref(), Some("flag-ssh"));

        let with_favourite = reconcile(
            Some("Temper".into()),
            Some(f),
            None,
            None,
            None,
            None,
            None,
            mdrdp::favourites::NativeMode::Auto,
            None,
        )
        .unwrap();
        assert_eq!(with_favourite.ssh_user.as_deref(), Some("saved-ssh"));

        let bare = reconcile_basic(
            Some("Temper".into()),
            Some(temper()),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(bare.ssh_user, None, "absent means ~/.ssh/config decides");
    }

    #[test]
    fn resolved_native_mode_rides_the_target() {
        let t = reconcile(
            Some("Temper".into()),
            Some(temper()),
            None,
            None,
            None,
            None,
            None,
            mdrdp::favourites::NativeMode::Always,
            None,
        )
        .unwrap();
        assert_eq!(t.native, mdrdp::favourites::NativeMode::Always);
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
        let t = reconcile_basic(
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
        assert_eq!(t.user.as_deref(), Some("saved-user"));
        assert_eq!(t.domain.as_deref(), Some("SAVED"));
        assert_eq!(t.port, 4000);
        assert_eq!(t.size, (1280, 800));
        assert!(!t.fullscreen, "an explicit favourite size stays windowed");
    }

    #[test]
    fn every_flag_overrides_the_favourite_it_clashes_with() {
        let t = reconcile_basic(
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
        assert_eq!(t.user.as_deref(), Some("flag-user"));
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
        let t = reconcile_basic(
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
        // RDP-only (`Never`): the requirement bites here, at reconcile.
        let err = reconcile(
            Some("box".into()),
            None,
            None,
            None,
            None,
            None,
            None,
            mdrdp::favourites::NativeMode::Never,
            None,
        )
        .unwrap_err();
        assert!(err.contains("no account for box"), "got: {err}");
        // A native-capable target defers the requirement to the RDP connect arm:
        // `--native` must never demand an RDP logon it will not use (review P-M5).
        let t = reconcile_basic(Some("box".into()), None, None, None, None, None, None)
            .expect("a native-capable target defers the account requirement");
        assert_eq!(t.user, None);
    }

    #[test]
    fn the_default_username_covers_a_bare_host() {
        let t = reconcile_basic(
            Some("box".into()),
            None,
            None,
            Some("default-user".into()),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(t.user.as_deref(), Some("default-user"));
        assert_eq!(
            t.keychain_account, "default-user",
            "the default account also names the keychain entry"
        );
    }

    #[test]
    fn a_favourite_account_beats_the_default_and_a_flag_beats_both() {
        let with_favourite = reconcile_basic(
            Some("Temper".into()),
            Some(temper()),
            None,
            Some("default-user".into()),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(with_favourite.user.as_deref(), Some("saved-user"));

        let with_flag = reconcile_basic(
            Some("Temper".into()),
            Some(temper()),
            Some("flag-user".into()),
            Some("default-user".into()),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(with_flag.user.as_deref(), Some("flag-user"));
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
        let t =
            reconcile_basic(Some("Temper".into()), Some(f), None, None, None, None, None).unwrap();
        assert_eq!(
            t.user.as_deref(),
            Some("user@example.com"),
            "what we log on as"
        );
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
        let t =
            reconcile_basic(Some("Plain".into()), Some(f), None, None, None, None, None).unwrap();
        assert_eq!(
            Some(t.keychain_account.as_str()),
            t.user.as_deref(),
            "one name unless told otherwise"
        );
    }

    #[test]
    fn nothing_at_all_is_a_usage_error() {
        assert!(reconcile_basic(None, None, None, None, None, None, None).is_err());
    }

    #[test]
    fn a_fullscreen_favourite_falls_back_to_the_default_size() {
        let f = Favourite {
            username: Some("u".into()),
            window_size: WindowSize::Fullscreen,
            ..Favourite::new("FS", "fs.local")
        };
        let t = reconcile_basic(Some("FS".into()), Some(f), None, None, None, None, None).unwrap();
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
        let t = reconcile_basic(
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
