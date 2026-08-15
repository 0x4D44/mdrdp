//! `mdrdp` — connect to an RDP host and show its desktop.
//!
//!     mdrdp                          pick from the favourites launcher
//!     mdrdp <favourite>              connect to a saved favourite by name
//!     mdrdp <host> --user <account>  connect to a host directly
//!
//! The password comes from the OS keychain (service `mdrdp`, account `<user>`); it is
//! never an argument, an environment variable, or a log line.
//!
//! Ordering here is not arbitrary:
//!
//! 1. Build the process's one event loop first — winit allows exactly one, and both the
//!    launcher and the session window need it.
//! 2. Run the launcher on it, if there is a choice to make.
//! 3. Connect — the server reports the desktop size and the window is built to it.
//! 4. Build the session window on the **main thread**; winit requires that.
//! 5. Only then spawn the session thread, which needs the window's waker.

use ironrdp::connector::DesktopSize;
use mdrdp::connect::{ConnectOptions, establish};
use mdrdp::favourites::{Favourite, Favourites, WindowSize};
use mdrdp::gfx::GfxHandler;
use mdrdp::input::InputEvent;
use mdrdp::launcher;
use mdrdp::session;
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
     --list                 print saved favourites and exit\n  \
     --capture-failures DIR dump undecodable tiles for offline debugging\n\n\
     Flags override whatever the chosen favourite specifies."
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
    domain: Option<String>,
    size: (u16, u16),
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", usage());
        return Ok(());
    }

    let positional = args.first().filter(|h| !h.starts_with("--")).cloned();

    let mut user: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut domain: Option<String> = None;
    let mut size: Option<(u16, u16)> = None;
    let mut capture: Option<String> = None;
    let mut list_only = false;

    let mut i = usize::from(positional.is_some());
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
            "--user" => user = Some(value()?.clone()),
            "--port" => port = Some(value()?.parse()?),
            "--domain" => domain = Some(value()?.clone()),
            "--capture-failures" => capture = Some(value()?.clone()),
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').ok_or("--size wants WxH, e.g. 1280x800")?;
                size = Some((w.parse()?, h.parse()?));
            }
            other => return Err(format!("unknown flag {other}\n{}", usage()).into()),
        }
        i += 2;
    }

    let config_path = Favourites::default_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(no config directory)".to_owned());

    // A malformed favourites file is reported, never silently treated as empty — the
    // list is the user's data and quietly losing it is worse than refusing to start.
    // With an explicit target on the command line we can still proceed without it.
    let favourites = match Favourites::load() {
        Ok(f) => f,
        Err(e) if positional.is_some() => {
            eprintln!("warning: could not read favourites ({e}); treating the argument as a host");
            Favourites::default()
        }
        Err(e) => return Err(e.into()),
    };

    if list_only {
        if favourites.is_empty() {
            println!("no favourites yet — add them in {config_path}");
        } else {
            for f in favourites.iter() {
                let account = f.username.as_deref().unwrap_or("(no account)");
                println!("{:<24} {}@{}:{}", f.name, account, f.host, f.port);
            }
        }
        return Ok(());
    }

    // One event loop for the whole process: the launcher runs on it, then the session
    // window does. Building it up front also means a headless machine fails here, with a
    // clear message, rather than after a connection has been established.
    let mut event_loop = SessionWindow::event_loop()?;

    let chosen: Option<Favourite> = match &positional {
        Some(name) => favourites.resolve(name).cloned(),
        None => match launcher::pick(&mut event_loop, &favourites, &config_path)? {
            Some(f) => Some(f),
            // Closing the launcher without choosing is a normal way to quit.
            None => return Ok(()),
        },
    };

    let target = reconcile(positional, chosen, user, port, domain, size)?;

    let secret = mdrdp::creds::lookup(&target.user)?;

    let store = Arc::new(Mutex::new(SurfaceStore::new()));
    let (input_tx, input_rx) = mpsc::channel::<InputEvent>();

    // The stats handle must be taken before the handler is boxed away.
    let handler = GfxHandler::new(Arc::clone(&store));
    let handler = match &capture {
        Some(dir) => {
            eprintln!("capturing undecodable tiles to {dir} (session content — your call)");
            handler.capturing_failures_to(dir)
        }
        None => handler,
    };
    let stats = handler.stats();

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
    };

    eprintln!("connecting to {}:{} …", target.host, target.port);
    let established = establish(&opts, &secret, Some(Box::new(handler)))?;
    let desktop = established.desktop_size;
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
    let window = SessionWindow::new(
        event_loop,
        WindowConfig::new(
            format!("mdrdp — {}", target.host),
            desktop.width,
            desktop.height,
        ),
        Arc::clone(&store),
        input_tx,
    )?;
    let waker = window.waker();

    let session = session::spawn(established, Arc::clone(&store), input_rx, waker);

    let window_result = window.run();

    // Window gone: disconnect properly rather than dropping the socket, which would
    // leave a session alive on the host.
    let end = session.shutdown();
    let s = stats.snapshot();
    let cache = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .cache_stats();
    eprintln!(
        "session ended: {end:?}\n  frames {}  decode errors {}  undecoded regions {}\n  codecs {:?}",
        s.frames_completed, s.decode_errors, s.undecoded_regions, s.codec_ids_seen
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
    if s.decode_errors > 0 && capture.is_none() {
        eprintln!(
            "  {} tiles failed to decode. Re-run with --capture-failures <dir> to keep \
             the bytes for offline debugging.",
            s.decode_errors
        );
    }
    if s.undecoded_regions > 0 {
        eprintln!(
            "  {} regions arrived in a codec we cannot yet decode (RFX Progressive); \
             those parts of the desktop will be stale.",
            s.undecoded_regions
        );
    }

    window_result?;
    Ok(())
}

/// Reconcile a favourite with command-line flags. Flags always win.
///
/// Pulled out of `run` because this precedence is the one piece of the argument handling
/// that can be silently wrong — connecting as the wrong account, or to the favourite's
/// host when one was typed explicitly — and it is worth a test.
fn reconcile(
    positional: Option<String>,
    chosen: Option<Favourite>,
    user: Option<String>,
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
        .ok_or_else(|| {
            format!(
                "no account for {host}: pass --user <account>, or set one on the favourite\n{}",
                usage()
            )
        })?;

    let size = size.unwrap_or_else(|| match chosen.as_ref().map(|f| f.window_size) {
        Some(WindowSize::Explicit { width, height }) => (width, height),
        _ => DEFAULT_SIZE,
    });

    Ok(Target {
        host,
        port: port
            .or_else(|| chosen.as_ref().map(|f| f.port))
            .unwrap_or(mdrdp::favourites::DEFAULT_PORT),
        user,
        domain: domain.or_else(|| chosen.as_ref().and_then(|f| f.domain.clone())),
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        )
        .unwrap();
        assert_eq!(t.host, "temper.local", "the argument was a name, not a host");
        assert_eq!(t.user, "saved-user");
        assert_eq!(t.domain.as_deref(), Some("SAVED"));
        assert_eq!(t.port, 4000);
        assert_eq!(t.size, (1280, 800));
    }

    #[test]
    fn every_flag_overrides_the_favourite_it_clashes_with() {
        let t = reconcile(
            Some("Temper".into()),
            Some(temper()),
            Some("flag-user".into()),
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
        assert_eq!(t.host, "temper.local", "but the host still comes from the favourite");
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
        )
        .unwrap();
        assert_eq!(t.host, "192.0.2.50");
        assert_eq!(t.port, mdrdp::favourites::DEFAULT_PORT);
        assert_eq!(t.size, DEFAULT_SIZE);
        assert_eq!(t.domain, None);
    }

    #[test]
    fn a_host_with_no_account_anywhere_is_an_error_not_a_guess() {
        let err = reconcile(Some("box".into()), None, None, None, None, None).unwrap_err();
        assert!(err.contains("no account for box"), "got: {err}");
    }

    #[test]
    fn nothing_at_all_is_a_usage_error() {
        assert!(reconcile(None, None, None, None, None, None).is_err());
    }

    #[test]
    fn a_fullscreen_favourite_falls_back_to_the_default_size() {
        let f = Favourite {
            username: Some("u".into()),
            window_size: WindowSize::Fullscreen,
            ..Favourite::new("FS", "fs.local")
        };
        let t = reconcile(Some("FS".into()), Some(f), None, None, None, None).unwrap();
        assert_eq!(
            t.size, DEFAULT_SIZE,
            "fullscreen cannot be resolved to pixels before a window exists"
        );
    }
}
