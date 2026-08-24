//! The RDP session thread: read PDUs, paint, send input, disconnect cleanly.
//!
//! Threading is deliberately simple. winit must own the main thread, so the session runs
//! on one worker thread that owns the socket. It alternates between reading PDUs and
//! draining the input queue, rather than splitting the socket across two threads —
//! the TLS stream is not usefully splittable and an RDP session is a single connection,
//! so the extra machinery would buy nothing but complexity.
//!
//! Input latency does NOT pay for that simplicity: when nothing is decodable the loop
//! sleeps in [`wake::wait_readable`] on the socket *and* a doorbell every input sender
//! rings, so a keystroke wakes it immediately instead of waiting out a read timeout.
//! [`READ_SLICE`] only bounds the rare wait for the rest of an already-started PDU.

use crate::clipboard::ClipboardBridge;
use crate::connect::{ConnectError, Established, describe, send_shutdown, write_framed};
use crate::disconnect::{self, ServerFarewell};
use crate::input::{InputEvent, LatestMouseMove, encode_fastpath_input, to_fastpath};
use crate::stats::{CacheStats, StatsHandle};
use crate::surface::SurfaceStore;
use crate::wake::{self, Doorbell, DoorbellReceiver};
use crate::window::{CursorUpdate, Waker};
use ironrdp::connector::DesktopSize;
use ironrdp::session::{ActiveStageOutput, image::DecodedImage};
use ironrdp_blocking::Framed;
use ironrdp_cliprdr::CliprdrClient;
use rustls::{ClientConnection, StreamOwned};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long a read blocks when a PDU has started arriving but is not complete yet.
///
/// This is NOT the input-latency bound — input wakes the loop through the doorbell
/// while the socket is quiet. It only caps the wait for the tail of a PDU whose head
/// is already buffered, where more socket data is the only thing that can help.
const READ_SLICE: Duration = Duration::from_millis(5);
/// A peer that stops reading must not hold the sole session thread forever.
///
/// Five seconds matches the native side-channel policy: long enough for transient
/// backpressure, but finite. Any write error is terminal because part of an RDP frame
/// may already have reached the wire.
const RDP_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Fast-path's event count is one byte. Limiting one pump turn to one valid PDU also
/// prevents a producer that stays ahead of the wire from starving inbound processing.
const FASTPATH_INPUT_BATCH_MAX: usize = 255;

/// How long the idle sleep lasts when neither the socket nor the doorbell fires.
///
/// This is a cadence, not a latency bound: it is what keeps the clipboard poll and
/// paste timers serviced on a completely quiet link. Matched to [`CLIPBOARD_POLL`]
/// so the poll runs at most one period late.
const IDLE_WAIT: Duration = Duration::from_millis(250);

/// How often the local clipboard is checked for a change the user made.
///
/// No cross-platform OS notification exists for "the pasteboard changed", so it is
/// polled. 250 ms is below the threshold at which a copy-then-paste feels broken, and
/// the check is submitted to the clipboard worker and never blocks this session thread.
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
    /// Tell the server whether anyone can currently see the session window.
    ///
    /// Maps to the Suppress Output PDU (MS-RDPBCGR 2.2.11.3): a fully occluded window
    /// asks the server to stop sending graphics; becoming visible again asks for them
    /// back, followed by a Refresh Rect PDU for the full desktop — allowing updates
    /// alone resumes only *future* changes, so the explicit repaint request is what
    /// recovers everything missed while suppressed. Sent best-effort — a server that
    /// ignores it just keeps streaming, and the window keeps discarding.
    SetVisibility { visible: bool },
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
    /// The server ended it and said why — a reboot, a shutdown, another logon taking
    /// the session. Still an orderly end, but one the user is owed an explanation for.
    ServerEnded(ServerFarewell),
    Failed(ConnectError),
    /// A native (rhydra) transport failure — the tunnel died, a channel closed, or
    /// the wire was violated. Its own variant so the end dialog names the tunnel
    /// rather than dressing it as an RDP error (HLD tranche 3 §6, review S-m6).
    TransportFailed(String),
}

pub struct SessionHandle {
    join: JoinHandle<SessionEnd>,
    stop: Arc<AtomicBool>,
    /// Rung after `stop` is set so a pump asleep in `wait_readable` exits now,
    /// not at the end of its idle sleep.
    bell: Doorbell,
}

impl SessionHandle {
    /// Ask the session to disconnect and wait for it.
    pub fn shutdown(self) -> SessionEnd {
        self.stop.store(true, Ordering::Relaxed);
        self.bell.ring();
        self.join.join().unwrap_or(SessionEnd::Graceful)
    }
}

/// Run the session on its own thread.
///
/// Takes ownership of the established connection. The caller keeps the store (to paint
/// from) and the input sender (to feed it).
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    established: Established,
    store: Arc<Mutex<SurfaceStore>>,
    latest_mouse_move: Arc<LatestMouseMove>,
    input: Receiver<InputEvent>,
    commands: Receiver<SessionCommand>,
    waker: Waker,
    services: SessionServices,
    bell: Doorbell,
    wake_rx: DoorbellReceiver,
) -> SessionHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);

    let join = std::thread::Builder::new()
        .name("mdrdp-session".to_owned())
        .spawn(move || {
            run(
                established,
                store,
                latest_mouse_move,
                input,
                commands,
                waker,
                thread_stop,
                services,
                wake_rx,
            )
        })
        .expect("spawn session thread");

    SessionHandle { join, stop, bell }
}

#[allow(clippy::too_many_arguments)]
fn run(
    mut established: Established,
    store: Arc<Mutex<SurfaceStore>>,
    latest_mouse_move: Arc<LatestMouseMove>,
    input: Receiver<InputEvent>,
    commands: Receiver<SessionCommand>,
    waker: Waker,
    stop: Arc<AtomicBool>,
    mut services: SessionServices,
    wake_rx: DoorbellReceiver,
) -> SessionEnd {
    // A short read timeout lets one thread serve both directions; a finite write
    // timeout keeps peer backpressure from wedging that same thread forever.
    if let Err(e) = configure_session_socket(&established.socket) {
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

    let (mut last_generation, mut last_cache) = store
        .lock()
        .map(|s| (s.generation(), s.cache_stats()))
        .unwrap_or((0, CacheStats::default()));
    let outcome = pump(
        &mut established,
        &store,
        &latest_mouse_move,
        &input,
        &commands,
        &waker,
        &stop,
        &mut image,
        &mut last_generation,
        &mut last_cache,
        &mut services,
        &wake_rx,
    );

    // No later window event may keep replacing the physical position after the session
    // has stopped draining it. This mirrors the native input thread's close-before-exit.
    latest_mouse_move.close();

    // Disconnect properly while the transport is still usable. After an I/O failure a
    // frame may be partial and another write can only spend the timeout or further corrupt
    // framing; the peer will reap the broken TCP connection instead.
    if !matches!(&outcome, SessionEnd::Failed(ConnectError::Io(_))) {
        let _ = send_shutdown(&established.stage, &mut established.framed);
    }

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

fn configure_session_socket(socket: &TcpStream) -> Result<(), ConnectError> {
    socket
        .set_read_timeout(Some(READ_SLICE))
        .map_err(ConnectError::Io)?;
    socket
        .set_write_timeout(Some(RDP_WRITE_TIMEOUT))
        .map_err(ConnectError::Io)
}

#[allow(clippy::too_many_arguments)]
fn pump(
    established: &mut Established,
    store: &Arc<Mutex<SurfaceStore>>,
    latest_mouse_move: &LatestMouseMove,
    input: &Receiver<InputEvent>,
    commands: &Receiver<SessionCommand>,
    waker: &Waker,
    stop: &Arc<AtomicBool>,
    image: &mut DecodedImage,
    last_generation: &mut u64,
    last_cache: &mut CacheStats,
    services: &mut SessionServices,
    wake_rx: &DoorbellReceiver,
) -> SessionEnd {
    let mut last_clipboard_poll = Instant::now();
    // A resize waiting for the Display Control channel to open, with when it was asked.
    let mut pending_resize: Option<(SessionCommand, Instant)> = None;
    // The latest visibility change not yet told to the server.
    let mut pending_visibility: Option<bool> = None;
    // When input went out with no resulting paint seen yet. The gap between the two is
    // the round trip the latency requirement is about.
    let mut input_sent_at: Option<Instant> = None;
    // Accumulated locally and flushed when the picture changes: taking the stats lock on
    // every PDU would put a mutex in the hottest path in the client for a counter nobody
    // reads more than a few times a second.
    let mut bytes_since_flush: u64 = 0;
    // Summed `process` time since the last paint — the client's decode cost for the
    // frame it is building. Flushed alongside the paint notification.
    let mut decode_spent = Duration::ZERO;

    loop {
        if stop.load(Ordering::Relaxed) {
            return SessionEnd::Graceful;
        }

        // Swallow pending doorbell rings first: anything rung after this point
        // stays queued and cuts the coming `wait_readable` short, so a send can
        // never slip between the channel drains below and the sleep.
        wake_rx.drain();

        // --- outbound: input --------------------------------------------------
        let input_batch_full = match drain_input(
            input,
            latest_mouse_move,
            &mut established.framed,
            &mut input_sent_at,
            Instant::now,
        ) {
            Ok(Drained::Closed) => return SessionEnd::WindowClosed,
            Ok(Drained::Sent { batch_full }) => batch_full,
            Ok(Drained::Idle) => false,
            Err(e) => return SessionEnd::Failed(e),
        };

        // --- outbound: session commands ---------------------------------------
        // Only the newest of each kind matters: a user who toggled fullscreen twice
        // while the channel was still opening wants where they ended up, not the
        // journey, and likewise a window hidden and revealed in one slice is visible.
        while let Ok(command) = commands.try_recv() {
            match command {
                SessionCommand::Resize { .. } => {
                    pending_resize = Some((command, Instant::now()));
                }
                SessionCommand::SetVisibility { visible } => {
                    pending_visibility = Some(visible);
                }
            }
        }
        if let Err(e) = service_resize(established, &mut pending_resize) {
            return SessionEnd::Failed(e);
        }
        if let Err(e) = service_visibility(established, &mut pending_visibility) {
            return SessionEnd::Failed(e);
        }

        // --- clipboard --------------------------------------------------------
        let clipboard_batch_full =
            match service_clipboard(established, services, &mut last_clipboard_poll) {
                Ok(batch_full) => batch_full,
                Err(e) => {
                    if matches!(&e, ConnectError::Io(_)) {
                        // A failed write may have emitted only part of one static-channel frame.
                        // Continuing would corrupt ordering, so the bounded transport failure is
                        // terminal even though OS clipboard and encoding failures remain recoverable.
                        return SessionEnd::Failed(e);
                    }
                    tracing::warn!(error = %e, "clipboard exchange failed; session continues");
                    false
                }
            };

        // --- inbound: server PDUs ---------------------------------------------
        // Only read when something is already decodable client-side or the socket
        // has bytes. Otherwise flush any pending paint and sleep until the socket
        // or the doorbell wakes the loop — this is what keeps a keystroke's path
        // to the wire free of read-timeout waits.
        if !decodable_waiting(&mut established.framed) {
            notify_if_painted(
                store,
                waker,
                last_generation,
                last_cache,
                services,
                &mut input_sent_at,
                &mut bytes_since_flush,
                &mut decode_spent,
            );
            let ready = match wake::wait_readable(
                &established.socket,
                wake_rx,
                readiness_wait_after_work(input_batch_full || clipboard_batch_full),
            ) {
                Ok(ready) => ready,
                Err(e) => return SessionEnd::Failed(ConnectError::Io(e)),
            };
            if !ready.socket {
                // Doorbell ring or cadence tick: service the queues at the loop top.
                continue;
            }
        }

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
                    last_cache,
                    services,
                    &mut input_sent_at,
                    &mut bytes_since_flush,
                    &mut decode_spent,
                );
                continue;
            }
            Err(e) => return SessionEnd::Failed(ConnectError::Io(e)),
        };

        bytes_since_flush = bytes_since_flush.saturating_add(payload.len() as u64);

        let process_started = Instant::now();
        let outputs = match established.stage.process(image, action, &payload) {
            Ok(outputs) => outputs,
            Err(e) => return SessionEnd::Failed(ConnectError::Protocol(describe(&e))),
        };
        decode_spent += process_started.elapsed();

        for output in outputs {
            match output {
                ActiveStageOutput::ResponseFrame(frame) => {
                    if let Err(e) = write_framed(&mut established.framed, &frame) {
                        return SessionEnd::Failed(ConnectError::Io(e));
                    }
                }
                ActiveStageOutput::Terminate(reason) => {
                    // The server chose to end the session; the reason is the only
                    // clue to why (idle policy, another logon, server-side error).
                    eprintln!("server ended the session: {reason}");
                    return match disconnect::classify(&reason) {
                        Some(farewell) => SessionEnd::ServerEnded(farewell),
                        None => SessionEnd::Graceful,
                    };
                }
                // The remote's pointer shape, mirrored onto the local window. A dead
                // window is discovered by the input drain, not here.
                ActiveStageOutput::PointerDefault => {
                    waker.cursor(CursorUpdate::Default);
                }
                ActiveStageOutput::PointerHidden => {
                    waker.cursor(CursorUpdate::Hidden);
                }
                ActiveStageOutput::PointerBitmap(pointer) => {
                    // Dimensions only — a pointer bitmap is session content.
                    tracing::debug!(
                        width = pointer.width,
                        height = pointer.height,
                        "remote cursor shape"
                    );
                    waker.cursor(CursorUpdate::Bitmap {
                        width: pointer.width,
                        height: pointer.height,
                        hotspot_x: pointer.hotspot_x,
                        hotspot_y: pointer.hotspot_y,
                        rgba: pointer.bitmap_data.clone(),
                    });
                }
                // A server-side pointer warp. Moving the user's physical mouse for the
                // server would be a fight over the one cursor the user owns — skip it,
                // like every mainstream client does by default.
                ActiveStageOutput::PointerPosition { .. } => {}
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
            last_cache,
            services,
            &mut input_sent_at,
            &mut bytes_since_flush,
            &mut decode_spent,
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
) -> Result<bool, ConnectError> {
    let Some(bridge) = services.clipboard.as_mut() else {
        return Ok(false);
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
        return Ok(false);
    };

    let (batches, batch_full) = bridge.pump_bounded(cliprdr);
    for batch in batches {
        let encoded = established
            .stage
            .process_svc_processor_messages(batch)
            .map_err(|e| ConnectError::Protocol(describe(&e)))?;
        write_framed(&mut established.framed, &encoded).map_err(ConnectError::Io)?;
    }
    Ok(batch_full)
}

/// Nudge the window only when something actually changed.
///
/// The generation counter exists so the presenter never redraws an unchanged frame —
/// redrawing on a timer would burn battery and add latency for nothing.
#[allow(clippy::too_many_arguments)]
fn notify_if_painted(
    store: &Arc<Mutex<SurfaceStore>>,
    waker: &Waker,
    last: &mut u64,
    last_cache: &mut CacheStats,
    services: &SessionServices,
    input_sent_at: &mut Option<Instant>,
    bytes_since_flush: &mut u64,
    decode_spent: &mut Duration,
) {
    notify_if_painted_inner(
        store,
        last,
        last_cache,
        services,
        input_sent_at,
        bytes_since_flush,
        decode_spent,
        || {
            waker.damaged();
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn notify_if_painted_inner<F: FnOnce()>(
    store: &Arc<Mutex<SurfaceStore>>,
    last: &mut u64,
    last_cache: &mut CacheStats,
    services: &SessionServices,
    input_sent_at: &mut Option<Instant>,
    bytes_since_flush: &mut u64,
    decode_spent: &mut Duration,
    damage: F,
) {
    let Ok(guard) = store.lock() else {
        return;
    };
    let now = guard.generation();
    let cache = guard.cache_stats();
    drop(guard);
    refresh_cache_stats(cache, last_cache, services);
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
    let decode_micros = u32::try_from(decode_spent.as_micros()).unwrap_or(u32::MAX);
    services.stats.update(|s| {
        s.frames = s.frames.saturating_add(1);
        s.cache = cache;
        s.bytes_in = s.bytes_in.saturating_add(*bytes_since_flush);
        // This frame's client-side split: the decode work it took to build, and
        // the paint→present handoff the window will close when it puts
        // `generation` (or newer) on screen.
        s.decode.record(decode_micros);
        s.mark_painted(now);
        if let Some(gfx) = gfx {
            s.decode_errors = gfx.decode_errors;
            s.undecoded_regions = gfx.undecoded_regions;
            s.codecs = gfx.codec_ids_seen;
            s.codec_painted = gfx.codec_bytes_painted;
        }
    });

    *bytes_since_flush = 0;
    *decode_spent = Duration::ZERO;
    damage();
}

/// Refresh cache diagnostics without treating cache bookkeeping as a painted frame.
///
/// Cache PDUs may be the only traffic in a pump turn. They must update the overlay's
/// counters, but they must not wake the window, mark a paint, or answer input latency.
fn refresh_cache_stats(cache: CacheStats, last_cache: &mut CacheStats, services: &SessionServices) {
    if cache == *last_cache {
        return;
    }
    *last_cache = cache;
    services.stats.update(|s| s.cache = cache);
}

/// The largest frame a single H.264 stream can carry: level 5.2's 36 864-macroblock
/// ceiling, which 4096x2304 hits exactly at 16:9.
const MAX_ENCODABLE_WIDTH: u32 = 4096;
const MAX_ENCODABLE_HEIGHT: u32 = 2304;

/// What a fullscreen window should ask of the session, for a monitor of
/// `width`x`height` physical pixels at `scale_percent` UI scale.
///
/// - A monitor that fits under the H.264 encoder ceiling gets its native resolution
///   and scale: every remote pixel maps 1:1 onto a screen pixel — true Retina.
/// - Past the ceiling with `integer_fit` (the default), the request drops to the
///   smallest integer division that fits: a 5K panel becomes 2560x1440 at 100%,
///   which the presenter blows up 2x — uniform pixel-doubling, no fractional
///   raggedness. The scale divides with the resolution so the remote UI keeps its
///   apparent size (MS-RDPEDISP ignores a scale below 100, so it pins there).
/// - Past the ceiling without `integer_fit`, the request is the fractional best
///   fit ([`clamp_to_encodable`]): more pixels than the integer fit, but presented
///   through a non-integer stretch. Kept reachable (Settings ▸ Graphics) so the two
///   can be compared on a live session.
///
/// Returned values are always encodable, so a caller can also *connect* at them.
/// A scale outside MS-RDPEDISP's 100–500 becomes `None` (advertise nothing).
pub fn fullscreen_request(
    width: u32,
    height: u32,
    scale_percent: u32,
    integer_fit: bool,
) -> (u32, u32, Option<u32>) {
    let scale = (100..=500)
        .contains(&scale_percent)
        .then_some(scale_percent);
    if width <= MAX_ENCODABLE_WIDTH && height <= MAX_ENCODABLE_HEIGHT {
        return (width, height, scale);
    }
    if !integer_fit {
        return clamp_to_encodable(width, height, scale);
    }
    let Some(divisor) =
        (2u32..=8).find(|d| width / d <= MAX_ENCODABLE_WIDTH && height / d <= MAX_ENCODABLE_HEIGHT)
    else {
        // No plausible monitor needs more than /8; fractional-fit rather than divide
        // a pathological size down to a postage stamp.
        return clamp_to_encodable(width, height, scale);
    };
    // Floor to even: H.264 4:2:0 subsampling needs even dimensions on both axes.
    let (width, height) = ((width / divisor) & !1, (height / divisor) & !1);
    let scale = scale.map(|s| (s / divisor).max(100));
    (width, height, scale)
}

/// Shrink a resolution request past the encoder ceiling, preserving aspect ratio and
/// apparent UI size (the scale shrinks by the same ratio).
///
/// A full-screen request on a Retina display sends the panel's physical size, and
/// 5120x2880 is beyond any single H.264 stream. A server running AVC does not fall
/// back: quench accepted the 5K layout, failed to reinitialise its encoder, and ended
/// the session with "the server-side graphics subsystem is in an error state"
/// (2026-08-16). The clamp applies under every codec, not just AVC — a resolution no
/// encoder refuses beats coupling the resize path to codec negotiation.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn clamp_to_encodable(
    width: u32,
    height: u32,
    scale_percent: Option<u32>,
) -> (u32, u32, Option<u32>) {
    if width <= MAX_ENCODABLE_WIDTH && height <= MAX_ENCODABLE_HEIGHT {
        return (width, height, scale_percent);
    }
    let ratio = f64::min(
        f64::from(MAX_ENCODABLE_WIDTH) / f64::from(width),
        f64::from(MAX_ENCODABLE_HEIGHT) / f64::from(height),
    );
    // Floor to even: H.264 4:2:0 subsampling needs even dimensions on both axes.
    let width = ((f64::from(width) * ratio) as u32) & !1;
    let height = ((f64::from(height) * ratio) as u32) & !1;
    // MS-RDPEDISP ignores a scale below 100, so a sub-100 result pins there: the remote
    // UI renders a little larger than local, which beats the scale being dropped.
    let scale_percent = scale_percent.map(|s| ((f64::from(s) * ratio).round() as u32).max(100));
    (width, height, scale_percent)
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

    let (requested_width, requested_height) = (width, height);
    let (width, height, scale_percent) = clamp_to_encodable(width, height, scale_percent);

    // MS-RDPEDISP bounds: each axis within 200..=8192 and the width even. A monitor
    // reports whatever it likes; the wire has rules.
    let (width, height) =
        ironrdp::displaycontrol::pdu::MonitorLayoutEntry::adjust_display_size(width, height);

    // A session that connected at this exact resolution and scale (the fullscreen-at-
    // start path) has nothing to renegotiate; asking anyway costs a server round of
    // Deactivate All / reactivation for zero change.
    if resize_is_redundant(
        width,
        height,
        scale_percent,
        established.desktop_size,
        established.desktop_scale_percent,
    ) {
        *pending = None;
        return Ok(());
    }

    // A monitor layout may only be sent after the server's capabilities PDU has arrived
    // (MS-RDPEDISP 3.3.5.2) — the channel being open is NOT enough, and a layout sent
    // early is silently ignored by Windows. `encode_resize` checks only that the channel
    // exists, so readiness is gated here; an unready channel keeps the request pending
    // under the same patience budget as an unopened one.
    let ready = established
        .stage
        .get_dvc::<ironrdp::displaycontrol::client::DisplayControlClient>()
        .and_then(|dvc| {
            dvc.channel_processor_downcast_ref::<ironrdp::displaycontrol::client::DisplayControlClient>()
        })
        .is_some_and(|client| client.ready());
    if !ready {
        if asked.elapsed() > RESIZE_PATIENCE {
            *pending = None;
            eprintln!(
                "resolution: the server never announced Display Control capabilities; \
                 the resolution stays fixed"
            );
        }
        return Ok(());
    }

    match established
        .stage
        .encode_resize(width, height, scale_percent, None)
    {
        Some(Ok(frame)) => {
            *pending = None;
            // Optimistic: the size lands at reactivation, but the scale has no
            // confirmation PDU, so the request is the best record of it there is.
            established.desktop_scale_percent = scale_percent;
            // eprintln, not tracing: the client installs no global tracing subscriber,
            // so tracing here is invisible. These are user-facing outcome lines, like
            // the channel report at connect.
            if requested_width > MAX_ENCODABLE_WIDTH || requested_height > MAX_ENCODABLE_HEIGHT {
                eprintln!(
                    "resolution: {requested_width}x{requested_height} exceeds the H.264 \
                     encoder ceiling; asking for {width}x{height} instead"
                );
            }
            eprintln!("resolution: requested {width}x{height} (scale {scale_percent:?})");
            write_framed(&mut established.framed, &frame).map_err(ConnectError::Io)
        }
        Some(Err(e)) => {
            // Losing one resize is not worth losing the desktop.
            *pending = None;
            eprintln!(
                "resolution: could not encode the change ({}); keeping the current resolution",
                describe(&e)
            );
            Ok(())
        }
        // The Display Control channel is not open (yet). Keep the request pending and
        // retry each pump slice: on a supporting server it opens within the first
        // second; on any other, patience runs out and the resolution stays fixed.
        None => {
            if asked.elapsed() > RESIZE_PATIENCE {
                *pending = None;
                eprintln!(
                    "resolution: the server never opened the Display Control channel; \
                     the resolution stays fixed"
                );
            }
            Ok(())
        }
    }
}

/// Tell the server whether to keep sending graphics, per the latest visibility change.
///
/// Unlike a resize this needs no channel to open — the Suppress Output PDU rides the
/// static global channel, which exists from the moment the session is established — so
/// there is no pending/retry state: it either goes out now or the send error ends the
/// session (a failed `write_all` means the socket is gone, not that suppression failed).
fn service_visibility(
    established: &mut Established,
    pending: &mut Option<bool>,
) -> Result<(), ConnectError> {
    let Some(visible) = *pending else {
        return Ok(());
    };

    let mut buf = ironrdp::core::WriteBuf::new();
    for pdu in visibility_pdus(visible, established.desktop_size) {
        if let Err(e) = established.stage.encode_static(&mut buf, pdu) {
            // Losing one suppression is not worth losing the desktop: the only cost of
            // a server that keeps streaming is the idle CPU this was meant to save.
            *pending = None;
            eprintln!(
                "display: could not encode the visibility change ({}); updates keep flowing",
                describe(&e)
            );
            return Ok(());
        }
    }
    *pending = None;
    if visible {
        eprintln!("display: window visible again; asked the server to resume and repaint");
    } else {
        eprintln!("display: window hidden; asked the server to suppress updates");
    }
    write_framed(&mut established.framed, buf.filled()).map_err(ConnectError::Io)
}

/// Build the PDUs a visibility change owes the server.
///
/// Pure so it is testable without an [`Established`]. Hidden sends Suppress Output with
/// no rectangle (that IS the suppression, per MS-RDPBCGR 2.2.11.3). Visible sends the
/// allow form with the full desktop as an *inclusive* rectangle — and then a Refresh
/// Rect PDU (2.2.11.2) for the same rectangle, because allowing updates alone does NOT
/// make the server repaint: Windows under the graphics pipeline resumes forwarding only
/// *future* changes, so a desktop that went on changing while suppressed and then sat
/// still would never be sent at all. A session that connected occluded showed exactly
/// that as a permanent black window (kiln, 2026-08-18).
fn visibility_pdus(
    visible: bool,
    desktop: DesktopSize,
) -> Vec<ironrdp::pdu::rdp::headers::ShareDataPdu> {
    use ironrdp::pdu::geometry::InclusiveRectangle;
    use ironrdp::pdu::rdp::headers::ShareDataPdu;
    use ironrdp::pdu::rdp::refresh_rectangle::RefreshRectanglePdu;
    use ironrdp::pdu::rdp::suppress_output::SuppressOutputPdu;

    let full_desktop = || InclusiveRectangle {
        left: 0,
        top: 0,
        right: desktop.width.saturating_sub(1),
        bottom: desktop.height.saturating_sub(1),
    };
    if visible {
        vec![
            ShareDataPdu::SuppressOutput(SuppressOutputPdu {
                desktop_rect: Some(full_desktop()),
            }),
            ShareDataPdu::RefreshRectangle(RefreshRectanglePdu {
                areas_to_refresh: vec![full_desktop()],
            }),
        ]
    } else {
        vec![ShareDataPdu::SuppressOutput(SuppressOutputPdu {
            desktop_rect: None,
        })]
    }
}

/// Whether a resize request names the state the session is already in.
///
/// Pure so it is testable without an [`Established`]. `scale` compares exactly:
/// `None` (nothing advertised) is not the same state as `Some(100)`, because the
/// server treats an absent scale as "keep whatever you had".
fn resize_is_redundant(
    width: u32,
    height: u32,
    scale: Option<u32>,
    current: DesktopSize,
    current_scale: Option<u32>,
) -> bool {
    width == u32::from(current.width)
        && height == u32::from(current.height)
        && scale == current_scale
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
            eprintln!(
                "resolution: session reactivated at {}x{}",
                desktop_size.width, desktop_size.height
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
            write_framed(&mut established.framed, &buf[..len]).map_err(ConnectError::Io)?;
        }
    }
}

/// True when `read_pdu` can make progress without new socket bytes.
///
/// Two stashes can hold a frame the socket will never signal for: the framer's own
/// buffer (a second PDU read alongside the first) and rustls's plaintext buffer (a
/// decrypted record the framer has not pulled yet). Sleeping in `poll` while either
/// holds data would stall a frame for the whole idle wait, so the pump asks first.
///
/// A partial PDU — head buffered, tail still in flight — reports `false`: only more
/// socket data can finish it, so the socket poll is exactly the right wait.
fn decodable_waiting(framed: &mut Framed<StreamOwned<ClientConnection, TcpStream>>) -> bool {
    match ironrdp::pdu::find_size(framed.peek()) {
        Ok(Some(info)) if framed.peek().len() >= info.length => return true,
        // Malformed framing: let read_pdu hit it and report the error properly.
        Err(_) => return true,
        _ => {}
    }
    let (stream, _) = framed.get_inner_mut();
    match stream.conn.process_new_packets() {
        Ok(state) => state.plaintext_bytes_to_read() > 0,
        // A TLS-level fault: let read_pdu surface it rather than swallowing it here.
        Err(_) => true,
    }
}

/// What a drain of the input queue did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drained {
    /// Nothing was waiting.
    Idle,
    /// Input went out on the wire.
    Sent { batch_full: bool },
    /// The window dropped its sender.
    Closed,
}

/// Send one bounded batch of queued input events.
fn drain_input<S: std::io::Read + std::io::Write>(
    input: &Receiver<InputEvent>,
    latest_mouse_move: &LatestMouseMove,
    framed: &mut Framed<S>,
    input_sent_at: &mut Option<Instant>,
    now: impl FnOnce() -> Instant,
) -> Result<Drained, ConnectError> {
    let mut batch = Vec::new();
    let mut receiver_closed = false;
    while batch.len() < FASTPATH_INPUT_BATCH_MAX {
        match next_session_input(input, latest_mouse_move) {
            Ok(Some(event)) => batch.push(to_fastpath(event)),
            Ok(None) => break,
            Err(TryRecvError::Disconnected) => {
                receiver_closed = true;
                break;
            }
            // `next_session_input` currently turns an empty reliable queue into
            // `Ok(None)`, but keep the receiver's other non-blocking outcome explicit.
            Err(TryRecvError::Empty) => break,
        }
    }
    if batch.is_empty() {
        return Ok(if receiver_closed {
            Drained::Closed
        } else {
            Drained::Idle
        });
    }

    let batch_full = batch.len() == FASTPATH_INPUT_BATCH_MAX;
    // Start before encoding and transport delivery so socket backpressure is part of the
    // user-visible input-to-paint measurement. Keep the first unanswered input: replacing
    // it with every later key would make a slow link look fast.
    let previous_input_sent_at = *input_sent_at;
    input_sent_at.get_or_insert_with(now);
    // Batched into one PDU: a burst of mouse moves should not become a burst of writes.
    let encoded = match encode_fastpath_input(batch) {
        Ok(encoded) => encoded,
        Err(e) => {
            *input_sent_at = previous_input_sent_at;
            return Err(ConnectError::Protocol(format!("encode input: {e}")));
        }
    };
    if let Err(e) = write_framed(framed, &encoded) {
        *input_sent_at = previous_input_sent_at;
        return Err(ConnectError::Io(e));
    }
    Ok(Drained::Sent { batch_full })
}

/// Take reliable input first, then the one pending physical position.
///
/// A disconnected reliable sender still gets one last chance to flush the physical slot;
/// the following call reports `Disconnected` once that slot is empty.
fn next_session_input(
    reliable: &Receiver<InputEvent>,
    latest_mouse_move: &LatestMouseMove,
) -> Result<Option<InputEvent>, TryRecvError> {
    match reliable.try_recv() {
        Ok(event) => Ok(Some(event)),
        Err(TryRecvError::Empty) => Ok(latest_mouse_move.take()),
        Err(TryRecvError::Disconnected) => match latest_mouse_move.take() {
            Some(event) => Ok(Some(event)),
            None => Err(TryRecvError::Disconnected),
        },
    }
}

fn readiness_wait_after_work(batch_full: bool) -> Duration {
    if batch_full {
        Duration::ZERO
    } else {
        IDLE_WAIT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::LatestMouseMove;
    use crate::surface::SurfaceStore;
    use ironrdp::core::decode;
    use ironrdp::pdu::input::fast_path::{FastPathInput, FastPathInputEvent};
    use std::sync::mpsc;

    #[test]
    fn the_session_socket_bounds_both_read_and_write_stalls() {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let peer = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_server, _) = listener.accept().unwrap();

        configure_session_socket(&peer).unwrap();

        assert_eq!(peer.read_timeout().unwrap(), Some(READ_SLICE));
        assert_eq!(peer.write_timeout().unwrap(), Some(RDP_WRITE_TIMEOUT));
    }

    // --- visibility / suppress output --------------------------------------------

    #[test]
    fn a_hidden_window_suppresses_updates_with_no_rectangle() {
        // The polarity is the whole PDU: an absent rectangle means SUPPRESS, a present
        // one means allow (MS-RDPBCGR 2.2.11.3). Swapping it would make hiding the
        // window ask for MORE traffic — a bug invisible to every local test but this.
        use ironrdp::pdu::rdp::headers::ShareDataPdu;
        let desktop = DesktopSize {
            width: 2560,
            height: 1440,
        };
        let pdus = visibility_pdus(false, desktop);
        assert_eq!(pdus.len(), 1, "hiding owes the server exactly one PDU");
        match &pdus[0] {
            ShareDataPdu::SuppressOutput(pdu) => assert!(pdu.desktop_rect.is_none()),
            other => panic!("wrong PDU kind: {other:?}"),
        }
    }

    #[test]
    fn a_visible_window_resumes_updates_with_the_full_inclusive_desktop() {
        // The rectangle is INCLUSIVE: right/bottom are width-1/height-1. Sending the
        // exclusive form asks for a column and row that do not exist, which Windows
        // answers by ignoring the PDU — updates would never resume.
        use ironrdp::pdu::rdp::headers::ShareDataPdu;
        let desktop = DesktopSize {
            width: 2560,
            height: 1440,
        };
        match &visibility_pdus(true, desktop)[0] {
            ShareDataPdu::SuppressOutput(pdu) => {
                let rect = pdu
                    .desktop_rect
                    .as_ref()
                    .expect("visible must carry the rect");
                assert_eq!((rect.left, rect.top), (0, 0));
                assert_eq!((rect.right, rect.bottom), (2559, 1439));
            }
            other => panic!("wrong PDU kind: {other:?}"),
        }
    }

    #[test]
    fn revealing_also_requests_a_repaint_of_the_full_desktop() {
        // Allowing updates alone does not repaint: the server resumes forwarding only
        // FUTURE changes, so everything that changed while suppressed — including the
        // whole first paint of a session that connected occluded — stays unsent and the
        // window stays black (kiln, 2026-08-18). The reveal must carry an explicit
        // Refresh Rect PDU for the full desktop, after the allow (a refresh sent while
        // output is still suppressed would itself be suppressed).
        use ironrdp::pdu::rdp::headers::ShareDataPdu;
        let desktop = DesktopSize {
            width: 2560,
            height: 1440,
        };
        let pdus = visibility_pdus(true, desktop);
        assert_eq!(pdus.len(), 2, "reveal owes allow + refresh");
        match &pdus[1] {
            ShareDataPdu::RefreshRectangle(pdu) => {
                assert_eq!(pdus.len(), 2);
                let [rect] = pdu.areas_to_refresh.as_slice() else {
                    panic!("one rectangle covering the desktop, got {pdu:?}");
                };
                assert_eq!((rect.left, rect.top), (0, 0));
                assert_eq!((rect.right, rect.bottom), (2559, 1439));
            }
            other => panic!("the second PDU must be the refresh, got: {other:?}"),
        }
        // Hiding must NOT request a repaint: the refresh would fight the suppression
        // it rides along with.
        assert_eq!(visibility_pdus(false, desktop).len(), 1);
    }

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

    struct FlushFailure {
        wrote: bool,
    }

    struct WriteObserver {
        clock_armed: Arc<AtomicBool>,
        armed_at_first_write: Option<bool>,
    }

    impl std::io::Read for WriteObserver {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "quiet"))
        }
    }

    impl std::io::Write for WriteObserver {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.armed_at_first_write.is_none() {
                self.armed_at_first_write = Some(self.clock_armed.load(Ordering::SeqCst));
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct WriteFailure;

    impl std::io::Read for WriteFailure {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "quiet"))
        }
    }

    impl std::io::Write for WriteFailure {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "write failed",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            panic!("flush must not follow a failed write")
        }
    }

    impl std::io::Read for FlushFailure {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "quiet"))
        }
    }

    impl std::io::Write for FlushFailure {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.wrote = true;
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "socket write timed out",
            ))
        }
    }

    #[test]
    fn a_deferred_transport_error_is_observed_by_the_frame_flush() {
        let mut framed = Framed::new(FlushFailure { wrote: false });
        let error = write_framed(&mut framed, b"frame").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(framed.get_inner().0.wrote, "the write preceded the flush");
    }

    #[test]
    fn input_clock_covers_delivery_and_rolls_back_on_failure() {
        let send_one = || {
            let (tx, rx) = mpsc::channel();
            tx.send(InputEvent::MouseMove { x: 1, y: 2 }).unwrap();
            (tx, rx)
        };
        let latest = LatestMouseMove::default();

        let (_tx, rx) = send_one();
        let clock_armed = Arc::new(AtomicBool::new(false));
        let mut framed = Framed::new(WriteObserver {
            clock_armed: Arc::clone(&clock_armed),
            armed_at_first_write: None,
        });
        let mut input_sent_at = None;
        assert_eq!(
            drain_input(&rx, &latest, &mut framed, &mut input_sent_at, || {
                clock_armed.store(true, Ordering::SeqCst);
                Instant::now()
            })
            .unwrap(),
            Drained::Sent { batch_full: false }
        );
        assert!(
            framed.get_inner().0.armed_at_first_write.unwrap(),
            "the latency clock must include transport delivery"
        );

        let prior = Instant::now();
        let (_tx, rx) = send_one();
        let mut framed = Framed::new(Sink(Vec::new()));
        let mut input_sent_at = Some(prior);
        assert!(drain_input(&rx, &latest, &mut framed, &mut input_sent_at, Instant::now,).is_ok());
        assert_eq!(
            input_sent_at,
            Some(prior),
            "keep the earliest pending input"
        );

        let (_tx, rx) = send_one();
        let mut framed = Framed::new(WriteFailure);
        let mut input_sent_at = None;
        assert!(drain_input(&rx, &latest, &mut framed, &mut input_sent_at, Instant::now,).is_err());
        assert!(
            input_sent_at.is_none(),
            "a failed write must not leave an unsent latency sample"
        );

        let (_tx, rx) = send_one();
        let mut framed = Framed::new(FlushFailure { wrote: false });
        let mut input_sent_at = None;
        assert!(drain_input(&rx, &latest, &mut framed, &mut input_sent_at, Instant::now,).is_err());
        assert!(
            input_sent_at.is_none(),
            "failed delivery must not leave an unsent latency sample"
        );
    }

    fn decode_input(wire: &[u8]) -> Vec<FastPathInputEvent> {
        decode::<FastPathInput>(wire)
            .expect("one encoded fast-path input PDU")
            .input_events()
            .to_vec()
    }

    #[test]
    fn a_resolution_within_the_ceiling_passes_through_untouched() {
        assert_eq!(
            clamp_to_encodable(1920, 1080, Some(100)),
            (1920, 1080, Some(100))
        );
        // The ceiling itself is encodable — exactly level 5.2's macroblock budget.
        assert_eq!(clamp_to_encodable(4096, 2304, None), (4096, 2304, None));
    }

    #[test]
    fn retina_fullscreen_clamps_to_the_ceiling_and_shrinks_the_scale() {
        // The case that killed the quench session: a 5K Retina panel in full screen.
        // 0.8x on both axes lands exactly on the ceiling; the scale follows, so the
        // remote UI keeps the same apparent size on the glass.
        assert_eq!(
            clamp_to_encodable(5120, 2880, Some(200)),
            (4096, 2304, Some(160))
        );
    }

    #[test]
    fn a_scale_the_wire_would_ignore_pins_at_100() {
        // A 5120x1440 super-ultrawide at native scale: the width forces 0.8x, but
        // MS-RDPEDISP ignores a scale of 80, so it pins at 100.
        assert_eq!(
            clamp_to_encodable(5120, 1440, Some(100)),
            (4096, 1152, Some(100))
        );
    }

    #[test]
    fn clamped_dimensions_are_floored_to_even() {
        // 1000 * (4096/5121) = 799.8…, which must floor to 798, not round to 800 or
        // stay odd at 799 — H.264 4:2:0 needs even axes.
        assert_eq!(clamp_to_encodable(5121, 1000, None), (4096, 798, None));
    }

    #[test]
    fn a_monitor_under_the_ceiling_gets_native_resolution_and_scale() {
        // A MacBook panel: true Retina, 1:1, whatever the fit mode.
        assert_eq!(
            fullscreen_request(3456, 2234, 200, true),
            (3456, 2234, Some(200))
        );
        assert_eq!(
            fullscreen_request(3456, 2234, 200, false),
            (3456, 2234, Some(200))
        );
    }

    #[test]
    fn a_5k_monitor_integer_fits_to_half_resolution_at_100() {
        // The whole point of the feature: 5120x2880 cannot ride one H.264 stream, so
        // the request halves to 2560x1440 and the scale halves with it — the
        // presenter's 2x stretch is then a uniform pixel-doubling, and 100% is an
        // integer DPI Windows renders crisply.
        assert_eq!(
            fullscreen_request(5120, 2880, 200, true),
            (2560, 1440, Some(100))
        );
    }

    #[test]
    fn a_5k_monitor_without_integer_fit_keeps_the_fractional_clamp() {
        // The A/B alternative (Settings ▸ Graphics): most pixels a stream can carry,
        // at the cost of a 1.25x fractional stretch on the glass.
        assert_eq!(
            fullscreen_request(5120, 2880, 200, false),
            (4096, 2304, Some(160))
        );
    }

    #[test]
    fn an_integer_fit_scale_never_drops_below_the_wire_minimum() {
        // A 5K panel run at 1x: halving 100% would ask for 50%, which MS-RDPEDISP
        // ignores; it pins at 100 like the fractional path does.
        assert_eq!(
            fullscreen_request(5120, 2880, 100, true),
            (2560, 1440, Some(100))
        );
    }

    #[test]
    fn a_scale_outside_the_wire_range_is_not_advertised() {
        // MS-RDPEDISP allows 100–500; anything else advertises nothing.
        assert_eq!(fullscreen_request(1920, 1080, 0, true), (1920, 1080, None));
        assert_eq!(
            fullscreen_request(1920, 1080, 600, true),
            (1920, 1080, None)
        );
    }

    #[test]
    fn a_redundant_resize_is_recognised_and_a_scale_change_is_not() {
        let current = DesktopSize {
            width: 2560,
            height: 1440,
        };
        // The fullscreen-at-start path: connected at the plan, the window's start-up
        // request matches, nothing to renegotiate.
        assert!(resize_is_redundant(
            2560,
            1440,
            Some(100),
            current,
            Some(100)
        ));
        // Same size but a scale the server has not been told about must still go out.
        assert!(!resize_is_redundant(2560, 1440, Some(100), current, None));
        assert!(!resize_is_redundant(
            2560,
            1440,
            Some(200),
            current,
            Some(100)
        ));
        assert!(!resize_is_redundant(
            1920,
            1080,
            Some(100),
            current,
            Some(100)
        ));
    }

    #[test]
    fn an_empty_queue_writes_nothing() {
        // A quiet loop iteration must not emit an empty PDU every 5 ms.
        let (_tx, rx) = mpsc::channel::<InputEvent>();
        let latest = LatestMouseMove::default();
        let mut framed = Framed::new(Sink(Vec::new()));
        assert_eq!(
            drain_input(&rx, &latest, &mut framed, &mut None, Instant::now).unwrap(),
            Drained::Idle
        );
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

        let latest = LatestMouseMove::default();
        let mut framed = Framed::new(Sink(Vec::new()));
        assert_eq!(
            drain_input(&rx, &latest, &mut framed, &mut None, Instant::now).unwrap(),
            Drained::Sent { batch_full: false },
            "sending is what starts the latency clock"
        );
        let written = framed.into_inner_no_leftover().0;
        assert!(!written.is_empty(), "three events should produce a PDU");
    }

    #[test]
    fn an_oversized_input_burst_is_split_across_pump_turns() {
        // Fast-path carries an 8-bit event count. A busy window can queue more than
        // 255 events while decode owns the session thread; one pump turn must send a
        // valid bounded batch and leave the remainder for the next turn, not end the
        // session with BadBatchSize or monopolise the pump until the queue is empty.
        let (tx, rx) = mpsc::channel();
        for x in 0..=u8::MAX {
            tx.send(InputEvent::MouseMove {
                x: u16::from(x),
                y: 0,
            })
            .unwrap();
        }

        let latest = LatestMouseMove::default();
        let mut framed = Framed::new(Sink(Vec::new()));
        let drained = drain_input(&rx, &latest, &mut framed, &mut None, Instant::now).unwrap();
        assert_eq!(drained, Drained::Sent { batch_full: true });
        let Drained::Sent { batch_full } = drained else {
            unreachable!("asserted sent above")
        };
        assert_eq!(
            readiness_wait_after_work(batch_full),
            Duration::ZERO,
            "a full batch may have consumed the only wake for its queued tail"
        );
        assert!(
            rx.try_recv().is_ok(),
            "one event must remain queued so inbound processing gets a turn"
        );
    }

    #[test]
    fn a_dropped_sender_reports_the_window_is_gone() {
        // The window closing drops its Sender; that is the session's cue to disconnect.
        let (tx, rx) = mpsc::channel::<InputEvent>();
        drop(tx);
        let latest = LatestMouseMove::default();
        let mut framed = Framed::new(Sink(Vec::new()));
        assert_eq!(
            drain_input(&rx, &latest, &mut framed, &mut None, Instant::now).unwrap(),
            Drained::Closed
        );
    }

    #[test]
    fn ten_thousand_physical_moves_leave_only_the_final_position_after_a_reliable_key() {
        let latest = LatestMouseMove::default();
        for x in 0..10_000u16 {
            assert!(latest.replace(x, x + 1));
        }
        let key = InputEvent::Key {
            scancode: crate::input::Scancode::plain(0x1e),
            down: true,
        };
        let (tx, rx) = mpsc::channel();
        tx.send(key).unwrap();

        let mut framed = Framed::new(Sink(Vec::new()));
        assert_eq!(
            drain_input(&rx, &latest, &mut framed, &mut None, Instant::now).unwrap(),
            Drained::Sent { batch_full: false }
        );
        let wire = framed.into_inner_no_leftover().0;
        assert_eq!(
            decode_input(&wire),
            vec![
                to_fastpath(key),
                to_fastpath(InputEvent::MouseMove {
                    x: 9_999,
                    y: 10_000,
                }),
            ],
            "the reliable key must not wait behind stale physical coordinates"
        );
    }

    #[test]
    fn scripted_reliable_motion_stays_fifo_before_the_physical_latest_value() {
        let scripted_first = InputEvent::MouseMove { x: 10, y: 20 };
        let scripted_second = InputEvent::MouseMove { x: 30, y: 40 };
        let physical_final = InputEvent::MouseMove { x: 90, y: 100 };
        let latest = LatestMouseMove::default();
        assert!(latest.replace(90, 100));
        let (tx, rx) = mpsc::channel();
        tx.send(scripted_first).unwrap();
        tx.send(scripted_second).unwrap();

        let mut framed = Framed::new(Sink(Vec::new()));
        assert_eq!(
            drain_input(&rx, &latest, &mut framed, &mut None, Instant::now).unwrap(),
            Drained::Sent { batch_full: false }
        );
        let wire = framed.into_inner_no_leftover().0;
        assert_eq!(
            decode_input(&wire),
            vec![
                to_fastpath(scripted_first),
                to_fastpath(scripted_second),
                to_fastpath(physical_final),
            ]
        );
    }

    #[test]
    fn a_later_drain_does_not_replay_the_physical_latest_value() {
        let latest = LatestMouseMove::default();
        assert!(latest.replace(321, 654));
        let (_tx, rx) = mpsc::channel::<InputEvent>();
        let mut framed = Framed::new(Sink(Vec::new()));

        assert_eq!(
            drain_input(&rx, &latest, &mut framed, &mut None, Instant::now).unwrap(),
            Drained::Sent { batch_full: false }
        );
        let first_write = framed.get_inner().0.0.clone();
        assert_eq!(decode_input(&first_write).len(), 1);

        assert_eq!(
            drain_input(&rx, &latest, &mut framed, &mut None, Instant::now).unwrap(),
            Drained::Idle
        );
        assert_eq!(
            framed.get_inner().0.0.as_slice(),
            first_write.as_slice(),
            "taking the latest value must remove it from future drains"
        );
    }

    #[test]
    fn offscreen_and_cache_work_refresh_stats_without_painting_or_waking() {
        let store = Arc::new(Mutex::new(SurfaceStore::new()));
        {
            let mut guard = store.lock().unwrap();
            guard.create(1, 2, 2);
            guard
                .solid_fill(1, &[crate::surface::Rect::new(0, 0, 2, 2)], [1, 2, 3, 255])
                .unwrap();
            guard.map_to_output(1);
        }

        let mut last_generation = store.lock().unwrap().generation();
        let mut last_cache = store.lock().unwrap().cache_stats();
        let stats = StatsHandle::new();
        let services = SessionServices {
            stats: stats.clone(),
            ..SessionServices::default()
        };
        let mut input_sent_at = Some(Instant::now());
        let mut bytes_since_flush = 0;
        let mut decode_spent = Duration::ZERO;
        {
            let mut guard = store.lock().unwrap();
            guard.create(2, 2, 2);
            guard
                .solid_fill(2, &[crate::surface::Rect::new(0, 0, 2, 2)], [4, 5, 6, 255])
                .unwrap();
            assert_eq!(
                guard.surface_to_cache(2, crate::surface::Rect::new(0, 0, 2, 2), 7),
                Ok(Some((2, 2)))
            );
        }

        let mut damaged = false;
        notify_if_painted_inner(
            &store,
            &mut last_generation,
            &mut last_cache,
            &services,
            &mut input_sent_at,
            &mut bytes_since_flush,
            &mut decode_spent,
            || damaged = true,
        );

        assert!(!damaged, "non-presentation work must not wake the window");
        assert_eq!(stats.snapshot().frames, 0);
        assert_eq!(stats.snapshot().latency.count(), 0);
        assert!(
            input_sent_at.is_some(),
            "unrelated work must not answer input"
        );
        assert_eq!(
            stats.snapshot().cache,
            store.lock().unwrap().cache_stats(),
            "cache diagnostics must still refresh"
        );

        store
            .lock()
            .unwrap()
            .solid_fill(1, &[crate::surface::Rect::new(0, 0, 1, 1)], [7, 8, 9, 255])
            .unwrap();
        notify_if_painted_inner(
            &store,
            &mut last_generation,
            &mut last_cache,
            &services,
            &mut input_sent_at,
            &mut bytes_since_flush,
            &mut decode_spent,
            || damaged = true,
        );
        assert!(damaged, "visible output work must wake the window");
        assert_eq!(stats.snapshot().frames, 1);
        assert_eq!(stats.snapshot().latency.count(), 1);
        assert!(input_sent_at.is_none());
    }

    #[test]
    fn the_window_is_only_nudged_when_the_store_changed() {
        // Redrawing an unchanged frame wastes power and adds latency for nothing.
        let store = Arc::new(Mutex::new(SurfaceStore::new()));
        {
            let mut guard = store.lock().unwrap();
            guard.create(1, 4, 4);
            guard.map_to_output(1);
        }
        let mut last = store.lock().unwrap().generation();

        // No waker available in a unit test, so assert on the generation bookkeeping,
        // which is the part that decides whether a nudge happens at all.
        let before = last;
        store
            .lock()
            .unwrap()
            .solid_fill(1, &[crate::surface::Rect::new(0, 0, 1, 1)], [1, 2, 3, 255])
            .unwrap();
        let after = store.lock().unwrap().generation();
        assert_ne!(after, before, "a mutation must bump the generation");

        last = after;
        let unchanged = store.lock().unwrap().generation();
        assert_eq!(unchanged, last, "no mutation, no change");
    }
}
