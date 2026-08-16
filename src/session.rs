//! The RDP session thread: read PDUs, paint, send input, disconnect cleanly.
//!
//! Threading is deliberately simple. winit must own the main thread, so the session runs
//! on one worker thread that owns the socket. It alternates between a short blocking read
//! and draining the input queue, rather than splitting the socket across two threads —
//! the TLS stream is not usefully splittable and an RDP session is a single connection,
//! so the extra machinery would buy nothing but complexity.
//!
//! The cost is input latency bounded by the read timeout, which is why that timeout is
//! small. If input latency ever measures badly, that constant is the first thing to look
//! at, not the threading model.

use crate::clipboard::ClipboardBridge;
use crate::connect::{ConnectError, Established, describe, send_shutdown};
use crate::input::{InputEvent, encode_fastpath_input, to_fastpath};
use crate::stats::StatsHandle;
use crate::surface::SurfaceStore;
use crate::window::Waker;
use ironrdp::session::{ActiveStageOutput, image::DecodedImage};
use ironrdp_blocking::Framed;
use ironrdp_cliprdr::CliprdrClient;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long a read blocks before the loop checks the input queue.
///
/// This is the worst-case added latency between a keystroke and it reaching the wire, so
/// it is small. Too small and the loop spins; 5 ms is well under the ~3.3 ms network
/// floor plus server processing, so it is not the bottleneck.
const READ_SLICE: Duration = Duration::from_millis(5);

/// How often the local clipboard is checked for a change the user made.
///
/// No cross-platform OS notification exists for "the pasteboard changed", so it is
/// polled. 250 ms is below the threshold at which a copy-then-paste feels broken, and
/// the check is a cheap string read, not a channel round trip.
const CLIPBOARD_POLL: Duration = Duration::from_millis(250);

/// Services the session drives alongside the pixel stream.
///
/// Bundled rather than passed as four more arguments, and defaulted so a probe or a test
/// can spawn a session without caring about any of them.
#[derive(Default)]
pub struct SessionServices {
    /// `None` when the clipboard channel was not negotiated.
    pub clipboard: Option<ClipboardBridge>,
    pub stats: StatsHandle,
    /// The graphics counters, so the overlay can show decode failures and stale regions.
    ///
    /// Without this the overlay's STALE line is unreachable: those counters live in the
    /// EGFX handler, which is moved into the connector and unreachable afterwards, so
    /// nothing was ever copying them across and the overlay silently reported zero.
    pub gfx: Option<crate::gfx::GfxStatsHandle>,
}

/// A request the window can make of the running session.
///
/// Distinct from [`InputEvent`]: input is what the user types into the remote desktop,
/// while a command is addressed to the session itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionCommand {
    /// Ask the server for a new session resolution over the Display Control channel.
    ///
    /// Best-effort by design: a server that never opened the channel simply keeps the
    /// old resolution, and the window keeps letterboxing — that is the graceful
    /// degradation, not an error.
    Resize {
        width: u32,
        height: u32,
        /// Desktop scale factor percent (100–500) to advertise alongside, so a HiDPI
        /// native resolution can come with "please render the UI at 200%". `None`
        /// advertises nothing.
        scale_percent: Option<u32>,
    },
}

/// How long a requested resize waits for the Display Control channel to open before the
/// request is dropped.
///
/// The channel opens within the first second of a session on a server that supports it;
/// a server that does not support it will never open it, and retrying forever would spin
/// a lookup every pump slice for the life of the session.
const RESIZE_PATIENCE: Duration = Duration::from_secs(10);

/// Wall-clock bound on the whole Deactivation-Reactivation sequence.
///
/// Reactivation is a handful of small PDUs on a link measured in milliseconds; if it has
/// not completed in this long the session is wedged and failing beats hanging.
const REACTIVATION_DEADLINE: Duration = Duration::from_secs(15);

/// Why the session ended.
#[derive(Debug)]
pub enum SessionEnd {
    /// The server or the user ended it — the normal path.
    Graceful,
    /// The window closed, so we disconnected.
    WindowClosed,
    Failed(ConnectError),
}

pub struct SessionHandle {
    join: JoinHandle<SessionEnd>,
    stop: Arc<AtomicBool>,
}

impl SessionHandle {
    /// Ask the session to disconnect and wait for it.
    pub fn shutdown(self) -> SessionEnd {
        self.stop.store(true, Ordering::Relaxed);
        self.join.join().unwrap_or(SessionEnd::Graceful)
    }
}

/// Run the session on its own thread.
///
/// Takes ownership of the established connection. The caller keeps the store (to paint
/// from) and the input sender (to feed it).
pub fn spawn(
    established: Established,
    store: Arc<Mutex<SurfaceStore>>,
    input: Receiver<InputEvent>,
    commands: Receiver<SessionCommand>,
    waker: Waker,
    services: SessionServices,
) -> SessionHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);

    let join = std::thread::Builder::new()
        .name("mdrdp-session".to_owned())
        .spawn(move || {
            run(
                established,
                store,
                input,
                commands,
                waker,
                thread_stop,
                services,
            )
        })
        .expect("spawn session thread");

    SessionHandle { join, stop }
}

#[allow(clippy::too_many_arguments)]
fn run(
    mut established: Established,
    store: Arc<Mutex<SurfaceStore>>,
    input: Receiver<InputEvent>,
    commands: Receiver<SessionCommand>,
    waker: Waker,
    stop: Arc<AtomicBool>,
    mut services: SessionServices,
) -> SessionEnd {
    // A short read timeout is what lets one thread serve both directions.
    if let Err(e) = established
        .socket
        .set_read_timeout(Some(READ_SLICE))
        .map_err(ConnectError::Io)
    {
        return SessionEnd::Failed(e);
    }

    let desktop = established.desktop_size;
    // EGFX paints into our SurfaceStore, so this image is only touched by the legacy
    // fast-path surface bits. Kept because ActiveStage::process requires it.
    let mut image = DecodedImage::new(
        ironrdp_graphics::image_processing::PixelFormat::RgbA32,
        desktop.width,
        desktop.height,
    );

    let mut last_generation = store.lock().map(|s| s.generation()).unwrap_or(0);
    let outcome = pump(
        &mut established,
        &store,
        &input,
        &commands,
        &waker,
        &stop,
        &mut image,
        &mut last_generation,
        &mut services,
    );

    // Always disconnect properly. Abandoning the socket leaves a session alive on the
    // Windows host, and they accumulate until it stops accepting logons.
    let _ = send_shutdown(&established.stage, &mut established.framed);

    // Tell the window the session is over — but ONLY when the session ended on its own.
    //
    // If `stop` is set, the window is already tearing down and is blocked in `join()`
    // waiting for this very thread (see `SessionWindow::on_exit`). Posting to the event
    // loop proxy from here would then be a thread waking a loop that is waiting on it:
    // a deadlock that hangs the process on every exit, which is worse than the frozen
    // window this call exists to prevent, because a hung process gets killed and a
    // killed process never sends the Shutdown Request at all.
    if !stop.load(Ordering::Relaxed) {
        waker.close();
    }

    outcome
}

#[allow(clippy::too_many_arguments)]
fn pump(
    established: &mut Established,
    store: &Arc<Mutex<SurfaceStore>>,
    input: &Receiver<InputEvent>,
    commands: &Receiver<SessionCommand>,
    waker: &Waker,
    stop: &Arc<AtomicBool>,
    image: &mut DecodedImage,
    last_generation: &mut u64,
    services: &mut SessionServices,
) -> SessionEnd {
    let mut last_clipboard_poll = Instant::now();
    // A resize waiting for the Display Control channel to open, with when it was asked.
    let mut pending_resize: Option<(SessionCommand, Instant)> = None;
    // When input went out with no resulting paint seen yet. The gap between the two is
    // the round trip the latency requirement is about.
    let mut input_sent_at: Option<Instant> = None;
    // Accumulated locally and flushed when the picture changes: taking the stats lock on
    // every PDU would put a mutex in the hottest path in the client for a counter nobody
    // reads more than a few times a second.
    let mut bytes_since_flush: u64 = 0;

    loop {
        if stop.load(Ordering::Relaxed) {
            return SessionEnd::Graceful;
        }

        // --- outbound: input --------------------------------------------------
        match drain_input(input, &mut established.framed) {
            Ok(Drained::Closed) => return SessionEnd::WindowClosed,
            Ok(Drained::Sent) => {
                // Only the first unanswered input starts the clock: overwriting it with
                // each later keystroke would measure the gap to the *last* one and make a
                // slow link look fast.
                input_sent_at.get_or_insert_with(Instant::now);
            }
            Ok(Drained::Idle) => {}
            Err(e) => return SessionEnd::Failed(e),
        }

        // --- outbound: session commands ---------------------------------------
        // Only the newest resize matters: a user who toggled fullscreen twice while the
        // channel was still opening wants where they ended up, not the journey.
        while let Ok(command) = commands.try_recv() {
            pending_resize = Some((command, Instant::now()));
        }
        if let Err(e) = service_resize(established, &mut pending_resize) {
            return SessionEnd::Failed(e);
        }

        // --- clipboard --------------------------------------------------------
        if let Err(e) = service_clipboard(established, services, &mut last_clipboard_poll) {
            // A clipboard failure is never worth dropping the desktop for.
            tracing::warn!(error = %e, "clipboard exchange failed; session continues");
        }

        // --- inbound: server PDUs ---------------------------------------------
        let (action, payload) = match established.framed.read_pdu() {
            Ok(pdu) => pdu,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                // Quiet link. Normal — this is how the loop yields to the input queue.
                notify_if_painted(
                    store,
                    waker,
                    last_generation,
                    services,
                    &mut input_sent_at,
                    &mut bytes_since_flush,
                );
                continue;
            }
            Err(e) => return SessionEnd::Failed(ConnectError::Io(e)),
        };

        bytes_since_flush = bytes_since_flush.saturating_add(payload.len() as u64);

        let outputs = match established.stage.process(image, action, &payload) {
            Ok(outputs) => outputs,
            Err(e) => return SessionEnd::Failed(ConnectError::Protocol(describe(&e))),
        };

        for output in outputs {
            match output {
                ActiveStageOutput::ResponseFrame(frame) => {
                    if let Err(e) = established.framed.write_all(&frame) {
                        return SessionEnd::Failed(ConnectError::Io(e));
                    }
                }
                ActiveStageOutput::Terminate(_) => return SessionEnd::Graceful,
                ActiveStageOutput::DeactivateAll => {
                    // The server tore the session layer down — this is how a Display
                    // Control resolution change completes. Run the
                    // Deactivation-Reactivation sequence and carry on at the new size.
                    if let Err(e) = reactivate(established, image) {
                        return SessionEnd::Failed(e);
                    }
                }
                _ => {}
            }
        }

        notify_if_painted(
            store,
            waker,
            last_generation,
            services,
            &mut input_sent_at,
            &mut bytes_since_flush,
        );
    }
}

/// Move clipboard traffic in both directions.
///
/// Split out of the pump because it is the one part of the loop with a timer of its own,
/// and because a clipboard fault must be reported and swallowed rather than ending the
/// session — the desktop keeps working when the clipboard does not.
fn service_clipboard(
    established: &mut Established,
    services: &mut SessionServices,
    last_poll: &mut Instant,
) -> Result<(), ConnectError> {
    let Some(bridge) = services.clipboard.as_mut() else {
        return Ok(());
    };

    // Timers first: a paste that timed out must return to Idle before the next pump, or
    // the request it is blocking never gets made.
    bridge.check_timeouts();
    if last_poll.elapsed() >= CLIPBOARD_POLL {
        bridge.poll_local_change();
        *last_poll = Instant::now();
    }

    let Some(cliprdr) = established.stage.get_svc_processor_mut::<CliprdrClient>() else {
        // The server never joined CLIPRDR. Queued actions have nowhere to go; draining
        // them keeps the channel from growing without bound for the life of the session.
        bridge.discard_pending();
        return Ok(());
    };

    let batches = bridge.pump(cliprdr);
    for batch in batches {
        let encoded = established
            .stage
            .process_svc_processor_messages(batch)
            .map_err(|e| ConnectError::Protocol(describe(&e)))?;
        established
            .framed
            .write_all(&encoded)
            .map_err(ConnectError::Io)?;
    }
    Ok(())
}

/// Nudge the window only when something actually changed.
///
/// The generation counter exists so the presenter never redraws an unchanged frame —
/// redrawing on a timer would burn battery and add latency for nothing.
fn notify_if_painted(
    store: &Arc<Mutex<SurfaceStore>>,
    waker: &Waker,
    last: &mut u64,
    services: &SessionServices,
    input_sent_at: &mut Option<Instant>,
    bytes_since_flush: &mut u64,
) {
    let Ok(guard) = store.lock() else {
        return;
    };
    let now = guard.generation();
    let cache = guard.cache_stats();
    drop(guard);
    if now == *last {
        return;
    }
    *last = now;

    // A paint following input is the closest thing to a round trip we can observe from
    // the client alone. It is a proxy, not a measurement of the server: a frame the
    // remote painted for its own reasons (a clock, a blinking cursor) that happens to
    // land after a keystroke reports a shorter time than the input really took. It is
    // still the right number to watch, because *drift* in it is what the "latency must
    // not degrade" requirement is about, and drift survives the noise.
    if let Some(sent) = input_sent_at.take() {
        let micros = u32::try_from(sent.elapsed().as_micros()).unwrap_or(u32::MAX);
        services.stats.update(|s| s.latency.record(micros));
    }
    // The graphics counters live in the EGFX handler, which the connector owns; copy the
    // ones the overlay reports so its STALE line reflects reality rather than a constant
    // zero. A snapshot is cheap and this runs only when the picture actually changed.
    let gfx = services.gfx.as_ref().map(|g| g.snapshot());
    services.stats.update(|s| {
        s.frames = s.frames.saturating_add(1);
        s.cache = cache;
        s.bytes_in = s.bytes_in.saturating_add(*bytes_since_flush);
        if let Some(gfx) = gfx {
            s.decode_errors = gfx.decode_errors;
            s.undecoded_regions = gfx.undecoded_regions;
            s.codecs = gfx.codec_ids_seen;
        }
    });

    *bytes_since_flush = 0;
    waker.damaged();
}

/// Try to send the pending resolution request, if the channel is ready for it.
///
/// Split from the pump for the same reason as the clipboard: it has its own retry state,
/// and a failure here must be swallowed rather than ending the session — the desktop at
/// the old resolution is strictly better than no desktop.
fn service_resize(
    established: &mut Established,
    pending: &mut Option<(SessionCommand, Instant)>,
) -> Result<(), ConnectError> {
    let Some((
        SessionCommand::Resize {
            width,
            height,
            scale_percent,
        },
        asked,
    )) = *pending
    else {
        return Ok(());
    };

    // MS-RDPEDISP bounds: each axis within 200..=8192 and the width even. A monitor
    // reports whatever it likes; the wire has rules.
    let (width, height) =
        ironrdp::displaycontrol::pdu::MonitorLayoutEntry::adjust_display_size(width, height);

    match established
        .stage
        .encode_resize(width, height, scale_percent, None)
    {
        Some(Ok(frame)) => {
            *pending = None;
            tracing::info!(
                width,
                height,
                scale_percent,
                "requested a session resolution change"
            );
            established
                .framed
                .write_all(&frame)
                .map_err(ConnectError::Io)
        }
        Some(Err(e)) => {
            // Losing one resize is not worth losing the desktop.
            *pending = None;
            tracing::warn!(error = %describe(&e), "could not encode the resolution change; keeping the current resolution");
            Ok(())
        }
        // The Display Control channel is not open (yet). Keep the request pending and
        // retry each pump slice: on a supporting server it opens within the first
        // second; on any other, patience runs out and the resolution stays fixed.
        None => {
            if asked.elapsed() > RESIZE_PATIENCE {
                *pending = None;
                tracing::info!(
                    "the server never opened the Display Control channel; the resolution stays fixed"
                );
            }
            Ok(())
        }
    }
}

/// Run the [MS-RDPBCGR] Deactivation-Reactivation sequence after a Server Deactivate All.
///
/// This is how a Display Control resolution change completes: the server deactivates,
/// capabilities are re-exchanged (carrying the new desktop size), and finalization runs
/// again. The MCS channel IDs are invariant across it, so every joined channel — EGFX,
/// clipboard, audio — survives; only the fast-path processor is rebuilt, because the
/// share ID can change.
fn reactivate(established: &mut Established, image: &mut DecodedImage) -> Result<(), ConnectError> {
    // The pump's 5 ms read slice would make every quiet moment here look like a stall.
    // Reactivation is a short sequential exchange: give reads a longer slice and bound
    // the whole sequence with a deadline instead.
    established
        .socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .map_err(ConnectError::Io)?;

    let outcome = drive_reactivation(established, image);

    // Whatever happened, the pump depends on its short slice being back.
    established
        .socket
        .set_read_timeout(Some(READ_SLICE))
        .map_err(ConnectError::Io)?;

    outcome
}

fn drive_reactivation(
    established: &mut Established,
    image: &mut DecodedImage,
) -> Result<(), ConnectError> {
    use ironrdp::connector::Sequence as _;
    use ironrdp::connector::connection_activation::ConnectionActivationState;

    let mut sequence = established.activation_factory.create();
    let mut buf = ironrdp::core::WriteBuf::new();
    let deadline = Instant::now() + REACTIVATION_DEADLINE;

    loop {
        if let ConnectionActivationState::Finalized {
            desktop_size,
            share_id,
            enable_server_pointer,
            pointer_software_rendering,
        } = sequence.connection_activation_state()
        {
            // The share ID can change across reactivation, so the fast-path processor is
            // rebuilt around it. The channel IDs are invariant for the connection.
            established.stage.set_fastpath_processor(
                ironrdp::session::fast_path::ProcessorBuilder {
                    io_channel_id: established.activation_factory.io_channel_id(),
                    user_channel_id: established.activation_factory.user_channel_id(),
                    share_id,
                    enable_server_pointer,
                    pointer_software_rendering,
                    // mdrdp never negotiates bulk compression (connect.rs sets
                    // `compression_type: None`), so there is nothing to rebuild here.
                    bulk_decompressor: None,
                }
                .build(),
            );
            *image = DecodedImage::new(
                ironrdp_graphics::image_processing::PixelFormat::RgbA32,
                desktop_size.width,
                desktop_size.height,
            );
            established.desktop_size = desktop_size;
            tracing::info!(
                width = desktop_size.width,
                height = desktop_size.height,
                "session reactivated"
            );
            return Ok(());
        }

        let Some(hint) = sequence.next_pdu_hint() else {
            return Err(ConnectError::Protocol(
                "reactivation stalled: the sequence wants no PDU but is not finalized".to_owned(),
            ));
        };
        let pdu = loop {
            match established.framed.read_by_hint(hint) {
                Ok(pdu) => break pdu,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    if Instant::now() > deadline {
                        return Err(ConnectError::Protocol(
                            "reactivation timed out waiting for the server".to_owned(),
                        ));
                    }
                }
                Err(e) => return Err(ConnectError::Io(e)),
            }
        };
        buf.clear();
        let written = sequence
            .step(&pdu, &mut buf)
            .map_err(|e| ConnectError::Protocol(describe(&e)))?;
        if let Some(len) = written.size() {
            established
                .framed
                .write_all(&buf[..len])
                .map_err(ConnectError::Io)?;
        }
    }
}

/// What a drain of the input queue did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drained {
    /// Nothing was waiting.
    Idle,
    /// Input went out on the wire.
    Sent,
    /// The window dropped its sender.
    Closed,
}

/// Send every queued input event.
fn drain_input<S: std::io::Read + std::io::Write>(
    input: &Receiver<InputEvent>,
    framed: &mut Framed<S>,
) -> Result<Drained, ConnectError> {
    let mut batch = Vec::new();
    loop {
        match input.try_recv() {
            Ok(event) => batch.push(to_fastpath(event)),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => return Ok(Drained::Closed),
        }
    }
    if batch.is_empty() {
        return Ok(Drained::Idle);
    }

    // Batched into one PDU: a burst of mouse moves should not become a burst of writes.
    let encoded = encode_fastpath_input(batch)
        .map_err(|e| ConnectError::Protocol(format!("encode input: {e}")))?;
    framed.write_all(&encoded).map_err(ConnectError::Io)?;
    Ok(Drained::Sent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::SurfaceStore;
    use std::sync::mpsc;

    /// A framed sink that records what was written, so the input path can be tested
    /// without a server.
    struct Sink(Vec<u8>);

    impl std::io::Read for Sink {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "quiet"))
        }
    }

    impl std::io::Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn an_empty_queue_writes_nothing() {
        // A quiet loop iteration must not emit an empty PDU every 5 ms.
        let (_tx, rx) = mpsc::channel::<InputEvent>();
        let mut framed = Framed::new(Sink(Vec::new()));
        assert_eq!(drain_input(&rx, &mut framed).unwrap(), Drained::Idle);
        assert!(framed.into_inner_no_leftover().0.is_empty());
    }

    #[test]
    fn queued_events_are_batched_into_one_write() {
        // Three events, one write — a burst of mouse moves must not become a burst of
        // syscalls, which is what makes input feel bad on a slow link.
        let (tx, rx) = mpsc::channel();
        tx.send(InputEvent::MouseMove { x: 1, y: 2 }).unwrap();
        tx.send(InputEvent::MouseMove { x: 3, y: 4 }).unwrap();
        tx.send(InputEvent::MouseMove { x: 5, y: 6 }).unwrap();

        let mut framed = Framed::new(Sink(Vec::new()));
        assert_eq!(
            drain_input(&rx, &mut framed).unwrap(),
            Drained::Sent,
            "sending is what starts the latency clock"
        );
        let written = framed.into_inner_no_leftover().0;
        assert!(!written.is_empty(), "three events should produce a PDU");
    }

    #[test]
    fn a_dropped_sender_reports_the_window_is_gone() {
        // The window closing drops its Sender; that is the session's cue to disconnect.
        let (tx, rx) = mpsc::channel::<InputEvent>();
        drop(tx);
        let mut framed = Framed::new(Sink(Vec::new()));
        assert_eq!(drain_input(&rx, &mut framed).unwrap(), Drained::Closed);
    }

    #[test]
    fn the_window_is_only_nudged_when_the_store_changed() {
        // Redrawing an unchanged frame wastes power and adds latency for nothing.
        let store = Arc::new(Mutex::new(SurfaceStore::new()));
        let mut last = store.lock().unwrap().generation();

        // No waker available in a unit test, so assert on the generation bookkeeping,
        // which is the part that decides whether a nudge happens at all.
        let before = last;
        store.lock().unwrap().create(1, 4, 4);
        let after = store.lock().unwrap().generation();
        assert_ne!(after, before, "a mutation must bump the generation");

        last = after;
        let unchanged = store.lock().unwrap().generation();
        assert_eq!(unchanged, last, "no mutation, no change");
    }
}
