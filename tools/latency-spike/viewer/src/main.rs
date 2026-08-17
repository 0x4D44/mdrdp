//! `spike-viewer` — the macOS half of the mdrdp latency spike.
//!
//! See `README.md` beside this file for how to run it against the server, what the
//! stats mean, and why the presenter is mdrdp's own.

use std::process::ExitCode;

#[cfg(target_os = "macos")]
fn main() -> ExitCode {
    use std::net::TcpStream;
    use std::sync::Arc;

    use spike_viewer::app::{UserEvent, ViewerApp};
    use spike_viewer::clock::Clock;
    use spike_viewer::input_link::InputLink;
    use spike_viewer::sink::{DecodeSink, FrameSlot};
    use spike_viewer::stats::{Header, StatsLog};
    use spike_viewer::{cli, interrupt, net};
    use winit::event_loop::EventLoop;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = match cli::parse(&args) {
        Ok(cfg) => cfg,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    let stats = match StatsLog::create(cfg.out.as_deref()) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("error: cannot write {:?}: {e}", cfg.out.unwrap_or_default());
            return ExitCode::FAILURE;
        }
    };

    // The decoder is resolved before anything connects: without one every access unit
    // becomes a decode error and the run is worthless, so say so up front rather than
    // after ten thousand refused frames.
    let decoder = mdrdp::h264::hardware_decoder();
    if decoder.is_none() {
        eprintln!("warning: this build has no hardware H.264 decoder; nothing will be shown");
    }

    let clock = Clock::new();
    stats.record(&Header::new(
        clock.name(),
        cfg.connect.to_string(),
        cfg.input.to_string(),
        decoder.is_some(),
    ));

    let video = match TcpStream::connect(cfg.connect) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: video connect to {} failed: {e}", cfg.connect);
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = video.set_nodelay(true) {
        eprintln!("error: TCP_NODELAY on the video socket failed: {e}");
        return ExitCode::FAILURE;
    }
    eprintln!("video: connected {}", cfg.connect);

    // The input channel is optional at runtime: a picture with no keystrokes is still
    // a usable decode/present measurement, and refusing to start would waste a
    // configured run over a missing tunnel.
    let input = match InputLink::connect(cfg.input) {
        Ok(link) => {
            eprintln!("input: connected {}", cfg.input);
            Some(Arc::new(link))
        }
        Err(e) => {
            eprintln!(
                "warning: input connect to {} failed: {e}; keystrokes are disabled",
                cfg.input
            );
            None
        }
    };

    interrupt::arm();

    let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: event loop: {e}");
            return ExitCode::FAILURE;
        }
    };
    let proxy = event_loop.create_proxy();

    let slot = Arc::new(FrameSlot::new());
    let reader_slot = slot.clone();
    let reader_stats = stats.clone();
    let reader = std::thread::Builder::new()
        .name("spike-video".to_owned())
        .spawn(move || {
            let mut sink = DecodeSink::new(
                decoder,
                reader_slot,
                reader_stats,
                // A failed send means the loop has already exited; the next read
                // error or EOF ends this thread.
                Box::new(move || {
                    let _ = proxy.send_event(UserEvent::Frame);
                }),
            );
            let mut video = video;
            let end = net::pump(&mut video, &Clock::new(), &mut sink);
            eprintln!("video: {end} after {} access units", sink.frames());
        });
    if let Err(e) = reader {
        eprintln!("error: cannot start the video thread: {e}");
        return ExitCode::FAILURE;
    }

    let mut app = ViewerApp::new(cfg.title, slot, stats.clone(), input);
    let outcome = event_loop.run_app(&mut app);
    stats.flush();
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The viewer decodes with VideoToolbox and stamps the CoreMedia host clock, and it
/// exists to be measured by `probe glass`, which is macOS-only for the same reasons.
/// Exiting 2 rather than panicking keeps `cargo check`/`cargo test` useful elsewhere.
#[cfg(not(target_os = "macos"))]
fn main() -> ExitCode {
    eprintln!("spike-viewer: macos only");
    ExitCode::from(2)
}
