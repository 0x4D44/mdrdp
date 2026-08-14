//! `mdrdp` — connect to an RDP host and show its desktop.
//!
//!     mdrdp <host> --user <account> [--port N] [--size WxH] [--domain D]
//!
//! The password comes from the OS keychain (service `mdrdp`, account `<user>`); it is
//! never an argument, an environment variable, or a log line.
//!
//! Ordering here is not arbitrary:
//!
//! 1. Connect first — the server reports the desktop size and the window is built to it.
//! 2. Build the window on the **main thread**; winit requires that.
//! 3. Only then spawn the session thread, which needs the window's waker.

use ironrdp::connector::DesktopSize;
use mdrdp::connect::{ConnectOptions, establish};
use mdrdp::gfx::GfxHandler;
use mdrdp::input::InputEvent;
use mdrdp::session;
use mdrdp::surface::SurfaceStore;
use mdrdp::trust::KnownHosts;
use mdrdp::window::{SessionWindow, WindowConfig};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

fn usage() -> &'static str {
    "usage: mdrdp <host> --user <account> [--port N] [--size WxH] [--domain D]"
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let host = args
        .first()
        .filter(|h| !h.starts_with("--"))
        .ok_or(usage())?
        .clone();

    let mut user: Option<String> = None;
    let mut port: u16 = 3389;
    let mut domain: Option<String> = None;
    let mut size = (1024u16, 768u16);

    let mut i = 1;
    while i < args.len() {
        let value = || -> Result<&String, String> {
            args.get(i + 1)
                .ok_or_else(|| format!("{} needs a value", args[i]))
        };
        match args[i].as_str() {
            "--user" => user = Some(value()?.clone()),
            "--port" => port = value()?.parse()?,
            "--domain" => domain = Some(value()?.clone()),
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').ok_or("--size wants WxH, e.g. 1280x800")?;
                size = (w.parse()?, h.parse()?);
            }
            other => return Err(format!("unknown flag {other}\n{}", usage()).into()),
        }
        i += 2;
    }
    let user = user.ok_or(usage())?;

    let secret = mdrdp::creds::lookup(&user)?;

    let store = Arc::new(Mutex::new(SurfaceStore::new()));
    let (input_tx, input_rx) = mpsc::channel::<InputEvent>();

    // The stats handle must be taken before the handler is boxed away.
    let handler = GfxHandler::new(Arc::clone(&store));
    let stats = handler.stats();

    let opts = ConnectOptions {
        host: host.clone(),
        port,
        username: user,
        domain,
        desktop_size: DesktopSize {
            width: size.0,
            height: size.1,
        },
        known_hosts: KnownHosts::default_path()?,
        observe_egfx: None,
    };

    eprintln!("connecting to {host}:{port} …");
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
        WindowConfig::new(format!("mdrdp — {host}"), desktop.width, desktop.height),
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
    eprintln!(
        "session ended: {end:?} — frames {}, decode errors {}, codecs {:?}",
        s.frames_completed, s.decode_errors, s.codec_ids_seen
    );

    window_result?;
    Ok(())
}
