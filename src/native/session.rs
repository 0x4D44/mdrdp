//! The native session: two threads feeding the existing window contract.
//!
//! `native-net` owns the video socket (handed over by the probe with the
//! header already consumed): read framed messages, decode, write pixels into
//! the shared [`SurfaceStore`], wake the window. `native-input` drains the
//! window's input channel, encodes wire records, and writes the input socket —
//! sleeping in [`wake::wait_readable`] on the socket *and* the doorbell, which
//! solves two review findings at once: a bare `recv()` can never be joined
//! (the window holds the sender for its own lifetime — S-M1), and a thread
//! that only ever writes discovers a dead channel two keystrokes late unless
//! it watches for readability, which for this socket means EOF (S-M8).
//!
//! Ordering: the viewer's `exact_through` invariant, ported onto the store.
//! The store's surface 0 IS the canvas — an AU swaps the decoder's buffer in
//! (`adopt_pixels`, no 8 MB copy), a rect update swizzles in place under the
//! lock, and **exactness advances only after every blit of an update has
//! succeeded** (review S-m9). A gap update is held (one slot, newest wins)
//! until the AU that closes its gap arrives; painting over a hole would lose
//! the missing frame's content forever (transport HLD decisions 13/16).

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ironrdp_egfx::decode::{DecodedFrame, DecoderResult, H264Decoder};
use rhydra::aux_proto::AudioFrame;
use rhydra::auxchan::{self, Outbox};
use rhydra::framing::{self, Reassembler};
use rhydra::input_proto::{MouseButton as WireButton, Record, WheelAxis, encode_record};
use rhydra::rects::{self, RectUpdate};
use rhydra::video_update::{self, VideoKind, VideoUpdate};

use crate::audio::{AudioFormatSummary, AudioRing};
use crate::clipboard::ArboardClipboard;
use crate::hevc::VideoDecoder;
use crate::input::{InputEvent, LatestMouseMove, MouseButton, ScrollAxis};
use crate::session::{SessionCommand, SessionEnd};
use crate::stats::StatsHandle;
use crate::surface::{Rect, SurfaceStore};
use crate::wake::{self, DoorbellReceiver};
use crate::window::{CursorUpdate, Waker};

use super::clipboard::{COUNTERS, TextOnly};
use super::probe::{ProbedTransport, TileHeader};
use super::ssh::Tunnel;
use rhydra::clipboard::{self as clip, Bridge, Policy, TextClipboard};

/// The surface id the native session paints. There is only ever one.
pub const OUTPUT_SURFACE: u16 = 0;

/// Decoder selected by the server header. H.264 remains primary; HEVC is kept
/// as a full-frame fallback when the host cannot construct the tiled AVC path.
pub enum NativeDecoder {
    H264(Box<dyn H264Decoder>),
    Hevc(Box<dyn VideoDecoder>),
}

impl NativeDecoder {
    fn decode(&mut self, data: &[u8]) -> DecoderResult<DecodedFrame> {
        match self {
            Self::H264(decoder) => decoder.decode(data),
            Self::Hevc(decoder) => decoder.decode(data),
        }
    }

    const fn label(&self) -> &'static str {
        match self {
            Self::H264(_) => "AVC (rhydra)",
            Self::Hevc(_) => "HEVC (rhydra fallback)",
        }
    }
}

/// The codec label the title bar and HUD show for native frames.
const CODEC_LABEL: &str = "AVC (rhydra)";

/// How often the local clipboard is read. Matches the RDP bridge's cadence:
/// macOS has no change notification worth using, so this is a poll.
const CLIPBOARD_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Bound a host that accepts the input channel and then stops reading it. Five seconds is
/// the existing native auxiliary/video write bound: generous beside an input burst, finite
/// beside a dead peer, and long enough not to turn a transient congestion window into a
/// disconnect.
const NATIVE_INPUT_WRITE_TIMEOUT: Duration = auxchan::WRITE_TIMEOUT;

/// Give the command channel a turn after a small reliable-input batch. The RDP fast-path
/// permits 255 events in one PDU, but native input writes each rhydra record separately (and
/// a button or wheel can emit two records), so 32 logical events bounds command latency
/// without making a keyboard burst pay a sleep between batches.
const NATIVE_INPUT_BATCH_MAX: usize = 32;

trait NativeTimedWrite: Write {
    fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()>;
}

impl NativeTimedWrite for TcpStream {
    fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        TcpStream::set_write_timeout(self, timeout)
    }
}

/// Everything one session's auxiliary channel owns, so [`NativeHandle`] carries
/// one optional field rather than five.
///
/// Absent whenever the host does not advertise the channel or its connect
/// failed — a session without a clipboard is a working session, and every path
/// here is written so that losing the clipboard cannot end one.
/// What the native transport observed about audio this session.
///
/// A process global for the same reason the clipboard counters are: the audio
/// thread is spawned deep inside `spawn_aux` and the epilogue that reports it
/// lives in `main`, with no value plumbed between them. **Session-scoped by
/// convention, not by construction** — one session per process is the product's
/// architecture, and this is only true for as long as that holds.
pub static AUDIO: AudioCounters = AudioCounters::new();

/// Counters and a one-shot signal check.
pub struct AudioCounters {
    frames: AtomicU64,
    /// The measured left/right peak frequencies, in Hz, or zero if not measured.
    ///
    /// Stored as integers because there is no atomic float and this is a
    /// diagnostic, not a measurement anyone will do arithmetic on.
    left_hz: AtomicU64,
    right_hz: AtomicU64,
    /// One-shot per-channel RMS, scaled by one million to keep the report atomic.
    left_rms_micros: AtomicU64,
    right_rms_micros: AtomicU64,
    queue_drops: AtomicU64,
    capture_gaps: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NativeAudioMetrics {
    pub queue_drops: u64,
    pub capture_gaps: u64,
}

impl AudioCounters {
    const fn new() -> Self {
        Self {
            frames: AtomicU64::new(0),
            left_hz: AtomicU64::new(0),
            right_hz: AtomicU64::new(0),
            left_rms_micros: AtomicU64::new(0),
            right_rms_micros: AtomicU64::new(0),
            queue_drops: AtomicU64::new(0),
            capture_gaps: AtomicU64::new(0),
        }
    }

    fn note_frame(&self) {
        self.frames.fetch_add(1, Ordering::Relaxed);
    }

    fn note_queue_drop(&self) {
        self.queue_drops.fetch_add(1, Ordering::Relaxed);
    }

    fn note_capture_gap(&self) {
        self.capture_gaps.fetch_add(1, Ordering::Relaxed);
    }

    /// Measure the dominant frequency in each channel, once.
    ///
    /// This is the oracle the acceptance criteria rest on. A frame counter says
    /// bytes moved; this says the *right* bytes moved, in the right ears, at the
    /// right rate — which is what a channel swap, a resample error and a
    /// byte-order mistake all fail.
    fn note_probe(&self, interleaved: &[f32], rate: u32, channels: u16) {
        use rhydra::audio_source::{deinterleave, peak_frequency};
        let left = deinterleave(interleaved, channels as u8, 0);
        let right = if channels >= 2 {
            deinterleave(interleaved, channels as u8, 1)
        } else {
            left.clone()
        };
        // A generous search window: wide enough that a badly wrong rate still
        // lands inside it and is reported as wrong, rather than falling outside
        // and being reported as absent.
        let range = 100..=2_000;
        if let Some(hz) = peak_frequency(&left, rate, range.clone()) {
            self.left_hz.store(hz as u64, Ordering::Relaxed);
        }
        if let Some(hz) = peak_frequency(&right, rate, range) {
            self.right_hz.store(hz as u64, Ordering::Relaxed);
        }
        let rms_micros = |samples: &[f32]| -> u64 {
            if samples.is_empty() {
                return 0;
            }
            let mean_square = samples
                .iter()
                .map(|sample| f64::from(*sample).powi(2))
                .sum::<f64>()
                / samples.len() as f64;
            (mean_square.sqrt() * 1_000_000.0).round() as u64
        };
        self.left_rms_micros
            .store(rms_micros(&left), Ordering::Relaxed);
        self.right_rms_micros
            .store(rms_micros(&right), Ordering::Relaxed);
    }

    /// Frames received, and the measured tone and RMS in each channel if any.
    pub fn report(&self) -> (u64, u64, u64, u64, u64) {
        (
            self.frames.load(Ordering::Relaxed),
            self.left_hz.load(Ordering::Relaxed),
            self.right_hz.load(Ordering::Relaxed),
            self.left_rms_micros.load(Ordering::Relaxed),
            self.right_rms_micros.load(Ordering::Relaxed),
        )
    }

    pub fn metrics(&self) -> NativeAudioMetrics {
        NativeAudioMetrics {
            queue_drops: self.queue_drops.load(Ordering::Relaxed),
            capture_gaps: self.capture_gaps.load(Ordering::Relaxed),
        }
    }
}

fn enqueue_audio(tx: &SyncSender<AudioFrame>, frame: AudioFrame, counters: &AudioCounters) {
    match tx.try_send(frame) {
        Ok(()) | Err(TrySendError::Disconnected(_)) => {}
        Err(TrySendError::Full(_)) => counters.note_queue_drop(),
    }
}

#[derive(Debug, Default)]
struct CaptureContinuity {
    next_capture_pos: Option<u64>,
    sample_rate: Option<u32>,
}

impl CaptureContinuity {
    fn observe(&mut self, capture_pos: u64, frame_count: usize, sample_rate: u32) -> bool {
        let gap = self
            .next_capture_pos
            .zip(self.sample_rate)
            .is_some_and(|(expected, rate)| rate == sample_rate && capture_pos != expected);
        self.next_capture_pos = Some(capture_pos.saturating_add(frame_count as u64));
        self.sample_rate = Some(sample_rate);
        gap
    }

    fn observe_frame(&mut self, frame: &AudioFrame) -> bool {
        // The host emits exactly one zero-PCM frame on entry to a quiet window.
        // It is a boundary in the existing wire shape, never playable audio.
        if frame.pcm.is_empty() {
            self.next_capture_pos = None;
            self.sample_rate = None;
            return false;
        }
        let stride = usize::from(frame.channels).saturating_mul(2);
        if stride == 0 || !frame.pcm.len().is_multiple_of(stride) {
            self.next_capture_pos = None;
            self.sample_rate = None;
            return false;
        }
        self.observe(
            frame.capture_pos,
            frame.pcm.len() / stride,
            frame.sample_rate,
        )
    }
}

/// What the client needs to turn wire audio into sound.
///
/// The device format travels with the ring because every frame is converted to
/// it: the wire is self-describing, so the host's rate and the device's rate are
/// independent and either can change without the other knowing.
pub struct AudioPlayout {
    pub ring: AudioRing,
    pub device: AudioFormatSummary,
}

struct AuxChannel {
    socket: TcpStream,
    slot: Arc<Outbox>,
    joins: Vec<JoinHandle<()>>,
}

impl AuxChannel {
    /// Close the slot, drop the socket, and join the threads.
    ///
    /// Order matters: closing the slot is what lets the writer return from its
    /// park, and shutting the socket is what unblocks the reader out of
    /// `read`. Joining before either would hang the session's teardown on a
    /// thread that is still waiting to be told to stop.
    fn shutdown(self) {
        self.slot.close();
        let _ = self.socket.shutdown(Shutdown::Both);
        for join in self.joins {
            let _ = join.join();
        }
    }
}

/// A running native session. `shutdown` is the only way out, mirroring
/// `session::SessionHandle`; dropping the handle without it leaks nothing —
/// the tunnel is kill-on-drop and the threads exit on the closed sockets.
pub struct NativeHandle {
    stop: Arc<AtomicBool>,
    video: TcpStream,
    input: TcpStream,
    video_join: JoinHandle<SessionEnd>,
    input_join: JoinHandle<Option<String>>,
    aux: Option<AuxChannel>,
    tunnel: Tunnel,
}

impl NativeHandle {
    /// Stop both threads, kill the tunnel, and report how the session ended.
    pub fn shutdown(mut self) -> SessionEnd {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.video.shutdown(Shutdown::Both);
        let _ = self.input.shutdown(Shutdown::Both);
        // The clipboard goes first and its result is discarded: it is the one
        // part of a session whose failure must never change how the session is
        // reported to have ended.
        if let Some(aux) = self.aux.take() {
            aux.shutdown();
        }
        let input_end = self.input_join.join().unwrap_or(None);
        let end = self.video_join.join().unwrap_or(SessionEnd::WindowClosed);
        self.tunnel.kill();
        match end {
            // The window closing is the normal path; an input-side failure only
            // matters when the video side did not already explain the end.
            SessionEnd::WindowClosed => match input_end {
                Some(reason) => SessionEnd::TransportFailed(reason),
                None => SessionEnd::WindowClosed,
            },
            other => other,
        }
    }
}

/// Wire the probe's sockets into the window contract and start both threads.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    transport: ProbedTransport,
    decoders: Vec<NativeDecoder>,
    store: Arc<Mutex<SurfaceStore>>,
    latest_mouse_move: Arc<LatestMouseMove>,
    input_rx: Receiver<InputEvent>,
    commands: Receiver<SessionCommand>,
    waker: Waker,
    stats: StatsHandle,
    wake_rx: DoorbellReceiver,
    clipboard_policy: Policy,
    audio: Option<AudioPlayout>,
) -> std::io::Result<NativeHandle> {
    let ProbedTransport { conn, tunnel, .. } = transport;
    let stop = Arc::new(AtomicBool::new(false));

    // Name the auxiliary channel's state once, out loud. A clipboard that
    // silently does nothing is indistinguishable from one that is broken, and
    // the three states have three different remedies: redeploy the host, look
    // at the host's aux listener, or nothing at all.
    eprintln!(
        "native: clipboard {}",
        match (conn.header.clipboard, conn.aux.is_some()) {
            (false, _) => "unsupported by this host",
            (true, true) => "ready",
            (true, false) => "advertised, but its channel did not connect",
        }
    );

    // Only when the host advertised it AND the socket connected. Failure here is
    // reported and dropped: it may not stop a session starting.
    let aux = match conn.aux {
        Some(socket) => match spawn_aux(
            socket,
            // The shared bridge speaks text only; `TextOnly` is the adapter.
            &mut || Box::new(TextOnly(ArboardClipboard::new())),
            clipboard_policy,
            Arc::clone(&stop),
            audio,
        ) {
            Ok(channel) => Some(channel),
            Err(e) => {
                eprintln!("native: clipboard threads did not start ({e}); continuing without");
                None
            }
        },
        None => None,
    };

    // The store's surface 0 is created at the wire size before any thread runs,
    // so the window can size itself and the first AU adopts cleanly.
    {
        let mut guard = store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.create(
            OUTPUT_SURFACE,
            conn.header.width as u16,
            conn.header.height as u16,
        );
        guard.map_to_output(OUTPUT_SURFACE);
    }

    let video = conn.video.try_clone()?;
    let input = conn.input.try_clone()?;

    // A previous native session may have ended while the host cursor was hidden.
    // Reset immediately; the host's initial cursor message then states the truth.
    let _ = waker.cursor(CursorUpdate::Default);

    // One clock, both threads: `native-input` stamps it, `native-net` closes it
    // on the next paint. That is the session's input round trip.
    let input_clock = InputClock::default();

    let net_stop = Arc::clone(&stop);
    let net_input_sock = conn.input.try_clone()?;
    let damage_waker = waker.clone();
    let mut sink = NativeSink::new_tiled(
        decoders,
        conn.header.tiles.clone(),
        store,
        (conn.header.width, conn.header.height),
        Box::new(move || {
            let _ = damage_waker.damaged();
        }),
        stats,
        input_clock.clone(),
    );
    let mut video_sock = conn.video;
    let mut reassembler = conn.reassembler;
    let video_waker = waker.clone();
    let video_join = std::thread::Builder::new()
        .name("native-net".to_owned())
        .spawn(move || {
            let mut on_cursor = |update| {
                let _ = video_waker.cursor(update);
            };
            let end = pump_video(
                &mut video_sock,
                &mut reassembler,
                &mut sink,
                &mut on_cursor,
                &net_stop,
            );
            let end = finish_video_worker(end, &net_stop, || {
                let _ = video_waker.close();
            });
            let _ = video_waker.cursor(CursorUpdate::Default);
            // Either side's death tears the whole session down (HLD §6).
            let _ = net_input_sock.shutdown(Shutdown::Both);
            end
        })?;

    let input_stop = Arc::clone(&stop);
    let input_video_sock = video.try_clone()?;
    let input_sock = conn.input;
    let input_waker = waker.clone();
    let input_join = std::thread::Builder::new()
        .name("native-input".to_owned())
        .spawn(move || {
            let moves = Arc::clone(&latest_mouse_move);
            let failure = pump_input(
                input_sock,
                latest_mouse_move,
                input_rx,
                commands,
                wake_rx,
                &input_stop,
                input_clock,
            );
            let failure = finish_input_worker(failure, &input_stop, || {
                let _ = input_waker.close();
            });
            moves.close();
            let _ = input_video_sock.shutdown(Shutdown::Both);
            failure
        })?;

    Ok(NativeHandle {
        stop,
        video,
        input,
        video_join,
        input_join,
        aux,
        tunnel,
    })
}

/// Start the auxiliary channel's three threads: read, write, and poll.
///
/// Three rather than two because the poll must keep running while the writer is
/// blocked on a slow socket. Folding the poll into the writer's idle lap would
/// stall it behind a write, and the suppression slot would then be stale by the
/// time the next arrival is judged against it.
/// The clipboard is injected rather than constructed here, so the teardown test
/// can run without touching the developer's own pasteboard.
fn spawn_aux(
    socket: TcpStream,
    make_clipboard: &mut dyn FnMut() -> Box<dyn TextClipboard>,
    policy: Policy,
    stop: Arc<AtomicBool>,
    audio: Option<AudioPlayout>,
) -> std::io::Result<AuxChannel> {
    // Nagle would add up to 40 ms to a small, bursty clipboard message.
    socket.set_nodelay(true)?;
    socket.set_write_timeout(Some(auxchan::WRITE_TIMEOUT))?;

    // **Ask for audio, or none arrives.** The host captures nothing until a
    // client requests it, so a client with a working device has to say so. Sent
    // here, before either pump thread exists, so it cannot race the writer for
    // the socket — and a failure is reported and dropped, because a session
    // without sound is still a working session.
    if audio.is_some() {
        let mut request = Vec::new();
        rhydra::aux_proto::encode_audio_control(true, &mut request);
        if let Err(e) = (&socket).write_all(&request) {
            report(&format!("audio not requested: {e}"));
        }
    }

    // **One clipboard handle per thread, not one shared behind a mutex**, and
    // the same reasoning as the host: a write that blocks would otherwise hold
    // that mutex and stall the poll thread for as long as the block lasted,
    // spreading a one-direction wedge to both.
    let apply_os = Arc::new(Mutex::new(make_clipboard()));
    let poll_os = Arc::new(Mutex::new(make_clipboard()));
    let bridge = Arc::new(Mutex::new(Bridge::new(policy)));

    // Seed from whatever the pasteboard already holds. Without this the first
    // poll reads as a change and one end clobbers the other's clipboard with no
    // user action — and which end wins is a race.
    {
        let mut guard = lock(&bridge);
        // An image or an unreadable pasteboard both mean "no text we could
        // have sent", which is exactly what an empty seed says.
        let seed = lock(&poll_os).read_text().ok().flatten();
        guard.seed(seed.as_deref());
    }

    let slot = Outbox::new();
    let mut joins = Vec::new();

    // Audio gets its own thread and a channel to reach it.
    //
    // The reader must not do the decode/remap/resample work itself: that cost
    // would land on the latency of every clipboard message sharing the thread,
    // and the whole point of this channel is that lower-priority traffic never
    // delays higher-priority traffic.
    //
    // **Bounded.** An unbounded queue here would sit in front of a bounded ring
    // and quietly defeat it: if decode or playback fell behind, stale audio would
    // accumulate without limit, adding exactly the latency the host's drop-oldest
    // FIFO exists to prevent, and teardown would then have to chew through the
    // whole backlog before this thread could exit. Depth matches the host's
    // outbox so the two bounds agree.
    let (audio_tx, audio_rx) = match audio {
        Some(_) => {
            let (tx, rx) =
                std::sync::mpsc::sync_channel::<AudioFrame>(rhydra::auxchan::AUDIO_FIFO_FRAMES);
            (Some(tx), Some(rx))
        }
        None => (None, None),
    };
    if let (Some(rx), Some(playout)) = (audio_rx, audio) {
        joins.push(
            std::thread::Builder::new()
                .name("native-audio".to_owned())
                .spawn(move || {
                    // A short window of what actually arrived, kept once and
                    // then never again. It is what turns "frames arrived" into
                    // "the right signal arrived": a frame counter cannot see a
                    // channel swap, a resampling error or wrong endianness, and
                    // those are the faults most likely to occur.
                    let mut probe: Vec<f32> = Vec::new();
                    let probe_target =
                        playout.device.sample_rate as usize * playout.device.channels as usize / 4; // a quarter-second
                    let mut probed = false;

                    // Ends when the reader thread drops its sender, which is
                    // exactly when there is no more audio coming.
                    while let Ok(frame) = rx.recv() {
                        AUDIO.note_frame();
                        let samples = crate::audio::pcm16_le_to_f32(&frame.pcm);
                        let matched = crate::audio::remap_channels(
                            &samples,
                            u16::from(frame.channels),
                            playout.device.channels,
                        );
                        let ready = crate::audio::linear_resample(
                            &matched,
                            playout.device.channels as usize,
                            frame.sample_rate,
                            playout.device.sample_rate,
                        );
                        if !probed {
                            probe.extend_from_slice(&ready);
                            if probe.len() >= probe_target {
                                AUDIO.note_probe(
                                    &probe,
                                    playout.device.sample_rate,
                                    playout.device.channels,
                                );
                                probed = true;
                                probe = Vec::new();
                            }
                        }
                        playout.ring.push(&ready);
                    }
                })?,
        );
    }

    let rx_socket = socket.try_clone()?;
    let rx_bridge = Arc::clone(&bridge);
    let rx_os = Arc::clone(&apply_os);
    joins.push(
        std::thread::Builder::new()
            .name("native-aux-rx".to_owned())
            .spawn(move || {
                let mut stats = auxchan::ReaderStats::default();
                let mut audio_continuity = CaptureContinuity::default();
                let end = auxchan::pump_reader(
                    rx_socket,
                    &mut auxchan::ReaderSinks {
                        accepts_clipboard: &mut || lock(&rx_bridge).accepts_incoming(),
                        on_text: &mut |text| {
                            // Both locks are taken here and nowhere else together,
                            // and never while the reader holds either — so the
                            // ordering cannot deadlock against the poll thread.
                            let mut os = lock(&rx_os);
                            // Counted from what `apply_remote` reports rather than
                            // decided again here: two places deciding the same
                            // policy eventually disagree.
                            match clip::apply_remote(&mut **os, &rx_bridge, text, &mut report) {
                                clip::Applied::Written => COUNTERS.note_applied(),
                                clip::Applied::Suppressed => COUNTERS.note_echo_suppressed(),
                                clip::Applied::Disabled
                                | clip::Applied::TooLarge
                                | clip::Applied::WriteFailed => COUNTERS.note_refused(),
                            }
                        },
                        // **Handed off, never processed here.** Decode, channel
                        // remap and resample are the expensive part, and doing them
                        // on the reader would add their cost to the latency of every
                        // clipboard message sharing this thread.
                        //
                        // `try_send`, never `send`. The queue is bounded, and
                        // `SyncSender::send` BLOCKS when a bounded queue is full —
                        // on this thread that would stall the reader, which is what
                        // keeps the clipboard moving, so a stalled audio device
                        // would wedge the clipboard. Dropping a frame is the right
                        // trade: the alternative is wedging both directions to
                        // preserve audio nobody can hear.
                        on_audio: &mut |frame| {
                            if audio_continuity.observe_frame(&frame) {
                                AUDIO.note_capture_gap();
                            }
                            if !frame.pcm.is_empty()
                                && let Some(tx) = audio_tx.as_ref()
                            {
                                enqueue_audio(tx, frame, &AUDIO);
                            }
                        },
                        // Client -> host only; the client never receives it.
                        on_audio_control: &mut |_| {},
                    },
                    &mut stats,
                );
                if let auxchan::ReaderEnd::Io(reason) = end {
                    report(&format!("clipboard channel closed: {reason}"));
                }
            })?,
    );

    let tx_socket = socket.try_clone()?;
    let tx_slot = Arc::clone(&slot);
    joins.push(
        std::thread::Builder::new()
            .name("native-aux-tx".to_owned())
            .spawn(move || {
                let writer = auxchan::pump_writer(&tx_socket, &tx_slot, &mut report);
                if let auxchan::WriterEnd::Io(reason) = writer.end {
                    report(&format!("clipboard channel write failed: {reason}"));
                    let _ = tx_socket.shutdown(Shutdown::Both);
                }
            })?,
    );

    let poll_slot = Arc::clone(&slot);
    let poll_bridge = Arc::clone(&bridge);
    let poll_os = Arc::clone(&poll_os);
    joins.push(
        std::thread::Builder::new()
            .name("native-clipboard-poll".to_owned())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    {
                        let mut os = lock(&poll_os);
                        if let Some(text) = clip::poll_local(&mut **os, &poll_bridge, &mut report) {
                            COUNTERS.note_sent();
                            poll_slot.put_clipboard(text);
                        }
                    }
                    std::thread::sleep(CLIPBOARD_POLL_INTERVAL);
                }
            })?,
    );

    Ok(AuxChannel {
        socket,
        slot,
        joins,
    })
}

/// A poisoned clipboard mutex is not worth ending a session over: the state it
/// guards is a fingerprint and a handle, and the next lap rebuilds both.
fn lock<T: ?Sized>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Where the clipboard threads say things. Never carries content — every caller
/// is a message that names sizes and reasons only.
fn report(message: &str) {
    eprintln!("native: {message}");
}

/// Rects may jump ahead of video only within this small window. This preserves
/// the measured fast-path win without allowing one socket read to invert an
/// arbitrary number of messages ahead of an earlier frame.
const VIDEO_DISPATCH_BATCH_MESSAGES: usize = 16;

/// Turn a failed video worker into a window close, but leave an intentional shutdown alone.
///
/// The worker owns the transport and can be the first party to discover that the session has
/// died. Setting `stop` only tells its sibling to stop; without the close event the window
/// keeps presenting its last frame forever and nobody calls [`NativeHandle::shutdown`].
fn finish_video_worker(
    end: SessionEnd,
    stop: &AtomicBool,
    close_window: impl FnOnce(),
) -> SessionEnd {
    if matches!(&end, SessionEnd::TransportFailed(_)) && !stop.load(Ordering::Relaxed) {
        close_window();
    }
    stop.store(true, Ordering::Relaxed);
    end
}

/// The input worker reports transport failures as `Some(reason)` rather than a `SessionEnd`.
/// Keep the same terminal-window contract as the video worker.
fn finish_input_worker(
    failure: Option<String>,
    stop: &AtomicBool,
    close_window: impl FnOnce(),
) -> Option<String> {
    if failure.is_some() && !stop.load(Ordering::Relaxed) {
        close_window();
    }
    stop.store(true, Ordering::Relaxed);
    failure
}

/// Read and dispatch framed messages until the socket closes, the wire is
/// violated, or the stop flag is raised.
fn pump_video(
    video: &mut TcpStream,
    reassembler: &mut Reassembler,
    sink: &mut NativeSink,
    on_cursor: &mut dyn FnMut(CursorUpdate),
    stop: &AtomicBool,
) -> SessionEnd {
    let mut buf = vec![0u8; 64 * 1024];
    // A bounded group of completed messages is dispatched rects-first: a rect update
    // is the low-latency path and never depends on an AU in the same batch
    // (the exactness gate makes ordering safe either way).
    let mut batch: Vec<framing::Message> = Vec::new();
    loop {
        if stop.load(Ordering::Relaxed) {
            return SessionEnd::WindowClosed;
        }
        batch.clear();
        while batch.len() < VIDEO_DISPATCH_BATCH_MESSAGES {
            match reassembler.next_message() {
                Ok(Some(m)) => batch.push(m),
                Ok(None) => break,
                Err(e) => return SessionEnd::TransportFailed(format!("framing: {e:?}")),
            }
        }
        if batch.is_empty() {
            let n = match video.read(&mut buf) {
                Ok(0) => {
                    return if stop.load(Ordering::Relaxed) {
                        SessionEnd::WindowClosed
                    } else {
                        SessionEnd::TransportFailed(
                            "the host closed the video channel (tunnel or server died)".to_owned(),
                        )
                    };
                }
                Ok(n) => n,
                Err(_) if stop.load(Ordering::Relaxed) => return SessionEnd::WindowClosed,
                Err(e) => return SessionEnd::TransportFailed(format!("video read: {e}")),
            };
            reassembler.push(&buf[..n]);
            continue;
        }
        for m in batch.iter().filter(|m| m.msg_type == framing::MSG_RECTS) {
            if let Err(reason) = sink.on_rects(&m.payload) {
                return SessionEnd::TransportFailed(reason);
            }
        }
        for m in batch.iter().filter(|m| m.msg_type != framing::MSG_RECTS) {
            let outcome = dispatch_video_message(m, sink, on_cursor);
            if let Err(reason) = outcome {
                return SessionEnd::TransportFailed(reason);
            }
        }
    }
}

fn dispatch_video_message(
    message: &framing::Message,
    sink: &mut NativeSink,
    on_cursor: &mut dyn FnMut(CursorUpdate),
) -> Result<(), String> {
    match message.msg_type {
        framing::MSG_VIDEO_SEQ => {
            if message.payload.len() < 8 {
                Err("MSG_VIDEO_SEQ shorter than its sequence prefix".to_owned())
            } else {
                let seq = u64::from_le_bytes(message.payload[..8].try_into().expect("8 bytes"));
                sink.on_au(&message.payload[8..], Some(seq))
            }
        }
        framing::MSG_VIDEO_TILE => match framing::decode_tile_au(&message.payload) {
            Ok(tile) => sink.on_tile_au(tile.tile_id, tile.au, tile.capture_seq),
            Err(e) => Err(format!("MSG_VIDEO_TILE: {e}")),
        },
        framing::MSG_VIDEO_UPDATE => match video_update::decode(&message.payload) {
            Ok(update) => sink.on_video_update(update),
            Err(error) => Err(format!("MSG_VIDEO_UPDATE: {error}")),
        },
        framing::MSG_VIDEO => sink.on_au(&message.payload, None),
        framing::MSG_CURSOR => match framing::decode_cursor(&message.payload) {
            Ok(true) => {
                on_cursor(CursorUpdate::Hidden);
                Ok(())
            }
            Ok(false) => {
                on_cursor(CursorUpdate::Default);
                Ok(())
            }
            Err(e) => Err(format!("MSG_CURSOR: {e}")),
        },
        framing::MSG_STATS => Ok(()), // per-frame server stats: not consumed yet
        _ => Ok(()),                  // unknown types skip by design
    }
}

/// Drain the window's input events onto the wire; watch the socket for EOF.
/// Returns `Some(reason)` on a failure worth reporting, `None` on a clean stop.
fn pump_input(
    sock: TcpStream,
    latest_mouse_move: Arc<LatestMouseMove>,
    input_rx: Receiver<InputEvent>,
    commands: Receiver<SessionCommand>,
    wake_rx: DoorbellReceiver,
    stop: &AtomicBool,
    input_clock: InputClock,
) -> Option<String> {
    pump_input_with_timeout(
        sock,
        latest_mouse_move,
        input_rx,
        commands,
        wake_rx,
        stop,
        input_clock,
        NATIVE_INPUT_WRITE_TIMEOUT,
    )
}

#[allow(clippy::too_many_arguments)]
fn pump_input_with_timeout(
    sock: TcpStream,
    latest_mouse_move: Arc<LatestMouseMove>,
    input_rx: Receiver<InputEvent>,
    commands: Receiver<SessionCommand>,
    wake_rx: DoorbellReceiver,
    stop: &AtomicBool,
    input_clock: InputClock,
    write_timeout: Duration,
) -> Option<String> {
    let mut writer = match sock.try_clone() {
        Ok(w) => w,
        Err(e) => return Some(format!("input socket: {e}")),
    };
    if let Err(e) = writer.set_write_timeout(Some(write_timeout)) {
        return Some(format!("input write setup: {e}"));
    }
    // How many records this session put on the wire — printed however the thread
    // exits; the one number that splits "the client never sent it" from "the host
    // never injected it".
    struct WrittenReport(u64);
    impl Drop for WrittenReport {
        fn drop(&mut self) {
            eprintln!("native: input records written: {}", self.0);
        }
    }
    let mut written = WrittenReport(0);
    let mut seq: u32 = 1;
    let mut resize_noted = false;
    let mut wait_timeout = Duration::from_millis(250);
    loop {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        let ready = match wake::wait_readable(&sock, &wake_rx, wait_timeout) {
            Ok(r) => r,
            Err(e) => return Some(format!("input wait: {e}")),
        };
        if ready.socket {
            // This thread never reads; the server never writes. Readable means
            // EOF or error — the channel is dead, and silently eating keystrokes
            // is the worst failure this product can have (review S-M8).
            return if stop.load(Ordering::Relaxed) {
                None
            } else {
                Some("the host closed the input channel".to_owned())
            };
        }
        if ready.bell {
            wake_rx.drain();
        }
        let mut clock_stamped = false;
        let batch = match drain_native_input_batch(
            NativeWriteBudget {
                writer: &mut writer,
                timeout: write_timeout,
            },
            &input_rx,
            &latest_mouse_move,
            &input_clock,
            &mut clock_stamped,
            &mut seq,
            &mut written.0,
        ) {
            Ok(batch) => batch,
            Err(e) => {
                return if stop.load(Ordering::Relaxed) {
                    None
                } else {
                    Some(format!("input write: {e}"))
                };
            }
        };
        if matches!(batch, NativeInputBatch::Closed) {
            return None;
        }
        service_native_commands(&commands, &mut resize_noted);
        wait_timeout = native_input_wait_after_batch(batch);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeInputBatch {
    Idle,
    Sent { full: bool },
    Closed,
}

struct NativeWriteBudget<'a, W> {
    writer: &'a mut W,
    timeout: Duration,
}

/// Drain at most one reliable-input batch, leaving the rest for the next pump turn.
fn drain_native_input_batch<W: NativeTimedWrite>(
    write_budget: NativeWriteBudget<'_, W>,
    input_rx: &Receiver<InputEvent>,
    latest_mouse_move: &LatestMouseMove,
    input_clock: &InputClock,
    clock_stamped: &mut bool,
    seq: &mut u32,
    written: &mut u64,
) -> std::io::Result<NativeInputBatch> {
    let NativeWriteBudget { writer, timeout } = write_budget;
    let mut drained = 0;
    while drained < NATIVE_INPUT_BATCH_MAX {
        match next_native_input(input_rx, latest_mouse_move) {
            Ok(Some(event)) => {
                write_native_records(
                    writer,
                    wire_records(&event, seq),
                    input_clock,
                    clock_stamped,
                    written,
                    timeout,
                )?;
                drained += 1;
            }
            Ok(None) | Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => return Ok(NativeInputBatch::Closed),
        }
    }
    if drained == 0 {
        Ok(NativeInputBatch::Idle)
    } else {
        Ok(NativeInputBatch::Sent {
            full: drained == NATIVE_INPUT_BATCH_MAX,
        })
    }
}

/// Keep resize/visibility service between reliable-input batches.
fn service_native_commands(commands: &Receiver<SessionCommand>, resize_noted: &mut bool) -> usize {
    let mut serviced = 0;
    while let Ok(command) = commands.try_recv() {
        serviced += 1;
        match command {
            SessionCommand::Resize { .. } => {
                // MVP: the host owns its resolution; the window letterboxes.
                if !*resize_noted {
                    *resize_noted = true;
                    eprintln!("native: resize requests are letterbox-only in this build");
                }
            }
            SessionCommand::SetVisibility { .. } => {} // no suppress notion yet
        }
    }
    serviced
}

/// A full batch means reliable input is still likely queued. Poll without sleeping so the
/// next batch follows immediately; an idle or partial batch keeps the existing 250 ms cadence.
fn native_input_wait_after_batch(batch: NativeInputBatch) -> Duration {
    if matches!(batch, NativeInputBatch::Sent { full: true }) {
        Duration::ZERO
    } else {
        Duration::from_millis(250)
    }
}

/// Write one input event's wire records, stamping after the first successful record.
///
/// `clock_stamped` lasts for the current drain burst. `InputClock::stamp` itself preserves
/// an earlier unanswered sample when a later burst arrives before a paint.
fn write_native_records<W, I>(
    writer: &mut W,
    records: I,
    input_clock: &InputClock,
    clock_stamped: &mut bool,
    written: &mut u64,
    write_timeout: Duration,
) -> io::Result<()>
where
    W: NativeTimedWrite,
    I: IntoIterator<Item = Record>,
{
    let mut now = Instant::now;
    write_native_records_with_clock(
        writer,
        records,
        input_clock,
        clock_stamped,
        written,
        write_timeout,
        &mut now,
    )
}

fn write_native_records_with_clock<W, I, N>(
    writer: &mut W,
    records: I,
    input_clock: &InputClock,
    clock_stamped: &mut bool,
    written: &mut u64,
    write_timeout: Duration,
    now: &mut N,
) -> io::Result<()>
where
    W: NativeTimedWrite,
    I: IntoIterator<Item = Record>,
    N: FnMut() -> Instant,
{
    for record in records {
        let (bytes, len) = encode_record(record);
        let started = now();
        let deadline = started.checked_add(write_timeout).unwrap_or(started);
        write_record_until(writer, &bytes[..len], deadline, now)?;
        if !*clock_stamped {
            // Something reached the host: start the round-trip clock the next paint on
            // `native-net` will close.
            input_clock.stamp();
            *clock_stamped = true;
        }
        *written = (*written).saturating_add(1);
    }
    Ok(())
}

fn write_record_until<W, N>(
    writer: &mut W,
    bytes: &[u8],
    deadline: Instant,
    now: &mut N,
) -> io::Result<()>
where
    W: NativeTimedWrite,
    N: FnMut() -> Instant,
{
    let mut offset = 0;
    while offset < bytes.len() {
        let current = now();
        let remaining = deadline.saturating_duration_since(current);
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "input record write deadline exceeded after {offset}/{} bytes",
                    bytes.len()
                ),
            ));
        }
        writer.set_write_timeout(Some(remaining))?;
        match writer.write(&bytes[offset..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "input record writer returned zero bytes",
                ));
            }
            Ok(written) => offset += written,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn next_native_input(
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

/// Translate one window input event into wire records.
///
/// Key events use the scancode kinds (layout-independent injection); the wire
/// convention is low byte = set-1 code, high byte 0xE0 for extended — mapped
/// from mdrdp's `Scancode { code, extended }`.
fn wire_records(event: &InputEvent, seq: &mut u32) -> Vec<Record> {
    let mut next = || {
        let s = *seq;
        *seq = seq.wrapping_add(1).max(1);
        s
    };
    match *event {
        InputEvent::Key { scancode, down } => {
            let wire = u16::from(scancode.code) | if scancode.extended { 0xE000 } else { 0 };
            vec![if down {
                Record::ScanDown {
                    scancode: wire,
                    seq: next(),
                }
            } else {
                Record::ScanUp {
                    scancode: wire,
                    seq: next(),
                }
            }]
        }
        InputEvent::MouseMove { x, y } => vec![Record::MouseMove { x, y, seq: next() }],
        InputEvent::MouseButton { button, down, x, y } => vec![
            Record::MouseMove { x, y, seq: next() },
            Record::MouseButton {
                button: match button {
                    MouseButton::Left => WireButton::Left,
                    MouseButton::Right => WireButton::Right,
                    MouseButton::Middle => WireButton::Middle,
                    MouseButton::X1 => WireButton::X1,
                    MouseButton::X2 => WireButton::X2,
                },
                down,
                seq: next(),
            },
        ],
        InputEvent::Scroll { axis, units, x, y } => vec![
            Record::MouseMove { x, y, seq: next() },
            Record::Wheel {
                axis: match axis {
                    ScrollAxis::Vertical => WheelAxis::Vertical,
                    ScrollAxis::Horizontal => WheelAxis::Horizontal,
                },
                delta120: units,
                seq: next(),
            },
        ],
    }
}

/// A rect update held because its seq is ahead of the canvas's exactness.
struct PendingRects {
    update: RectUpdate,
}

/// The first input still waiting for a paint, shared across the session's two
/// threads.
///
/// The RDP loop measures the same input→paint proxy inside a single thread
/// ([`crate::session::notify_if_painted`]); a native session sends input on
/// `native-input` and paints on `native-net`, so the stamp has to be shared
/// state rather than a local. Only the FIRST unanswered input starts the clock:
/// overwriting it with each later keystroke would measure the gap to the *last*
/// one and make a slow link look fast.
#[derive(Clone, Default)]
pub(crate) struct InputClock(Arc<Mutex<Option<Instant>>>);

impl InputClock {
    /// Input just went on the wire. A no-op while an earlier one is still
    /// unanswered.
    fn stamp(&self) {
        let mut held = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.get_or_insert_with(Instant::now);
    }

    /// A paint just landed: close the outstanding round trip, in microseconds.
    fn take_us(&self) -> Option<u32> {
        let sent = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()?;
        Some(u32::try_from(sent.elapsed().as_micros()).unwrap_or(u32::MAX))
    }
}

/// The decode-and-composite state: the viewer's exactness machinery, writing
/// into the shared store instead of a private canvas.
pub(crate) struct NativeSink {
    decoder: Option<Box<dyn H264Decoder>>,
    tile_decoders: Vec<TileDecodeState>,
    tile_frame: Option<TileFrameProgress>,
    codec_label: String,
    store: Arc<Mutex<SurfaceStore>>,
    wire_size: (u32, u32),
    /// The capture seq surface 0 is exact through. `None` until the first AU
    /// lands (rects before a base frame have nothing to composite onto).
    exact_through: Option<u64>,
    has_base: bool,
    pending: Option<PendingRects>,
    wake: Box<dyn Fn() + Send>,
    stats: StatsHandle,
    /// Shared with `native-input`; a paint closes whatever it holds.
    input_clock: InputClock,
    // Visible-behaviour counters (tests and debugging; not user-facing yet).
    pub(crate) suppressed: u64,
    pub(crate) skipped_stale: u64,
    pub(crate) skipped_empty: u64,
    pub(crate) skipped_before_base: u64,
    pub(crate) held: u64,
}

struct TileDecodeState {
    header: TileHeader,
    decoder: NativeDecoder,
    exact_through: Option<u64>,
    has_base: bool,
}

struct StagedTile {
    dest: Rect,
    pixels: Vec<u8>,
}

struct StagedVideoRegion {
    dest: Rect,
    pixels: Vec<u8>,
}

struct TileFrameProgress {
    seen: Vec<bool>,
    changed: bool,
    tiles: Vec<Option<StagedTile>>,
    seq: u64,
    bytes: u64,
    decode_us: u32,
}

impl NativeSink {
    #[cfg(test)]
    pub(crate) fn new(
        decoder: Option<Box<dyn H264Decoder>>,
        store: Arc<Mutex<SurfaceStore>>,
        wire_size: (u32, u32),
        wake: Box<dyn Fn() + Send>,
        stats: StatsHandle,
        input_clock: InputClock,
    ) -> Self {
        Self {
            decoder,
            tile_decoders: Vec::new(),
            tile_frame: None,
            codec_label: CODEC_LABEL.to_owned(),
            store,
            wire_size,
            exact_through: None,
            has_base: false,
            pending: None,
            wake,
            stats,
            input_clock,
            suppressed: 0,
            skipped_stale: 0,
            skipped_empty: 0,
            skipped_before_base: 0,
            held: 0,
        }
    }

    pub(crate) fn new_tiled(
        decoders: Vec<NativeDecoder>,
        headers: Vec<TileHeader>,
        store: Arc<Mutex<SurfaceStore>>,
        wire_size: (u32, u32),
        wake: Box<dyn Fn() + Send>,
        stats: StatsHandle,
        input_clock: InputClock,
    ) -> Self {
        assert_eq!(
            decoders.len(),
            headers.len(),
            "one decoder is required per advertised tile"
        );
        let codec_label = decoders
            .first()
            .map_or(CODEC_LABEL, NativeDecoder::label)
            .to_owned();
        let tile_decoders = headers
            .into_iter()
            .zip(decoders)
            .map(|(header, decoder)| TileDecodeState {
                header,
                decoder,
                exact_through: None,
                has_base: false,
            })
            .collect();
        Self {
            decoder: None,
            tile_decoders,
            tile_frame: None,
            codec_label,
            store,
            wire_size,
            exact_through: None,
            has_base: false,
            pending: None,
            wake,
            stats,
            input_clock,
            suppressed: 0,
            skipped_stale: 0,
            skipped_empty: 0,
            skipped_before_base: 0,
            held: 0,
        }
    }

    /// Fold one painted frame into the session stats.
    ///
    /// Both paint paths come through here on purpose. They used to keep their
    /// own stats blocks, and the rects path — which carries almost every frame
    /// of a native session — silently omitted the frame count, so `--sessions`
    /// reported FRAMES 1 beside megabytes of RX (MDR-BUG-FLUX-00012). One
    /// place now decides what a painted frame costs the collectors.
    ///
    /// `decode_us` is `Some` only for the AU path; a rect blit decodes nothing.
    fn record_paint(&self, bytes: u64, generation: u64, decode_us: Option<u32>) {
        let input_us = self.input_clock.take_us();
        self.stats.update(|s| {
            s.frames += 1;
            s.bytes_in += bytes;
            *s.codecs.entry(self.codec_label.clone()).or_insert(0) += 1;
            *s.codec_painted.entry(self.codec_label.clone()).or_insert(0) += bytes;
            if let Some(us) = decode_us {
                s.decode.record(us);
            }
            // A paint following input is the closest thing to a round trip the
            // client can observe alone — the same proxy, and the same caveats,
            // as the RDP path's.
            if let Some(us) = input_us {
                s.latency.record(us);
            }
            s.mark_painted(generation);
        });
    }

    /// May a decoded AU with this seq replace the surface? (The viewer's rule:
    /// at or below `exact_through` is provably redundant; a seqless v1 AU
    /// always paints.)
    fn accepts_au(&self, seq: Option<u64>) -> bool {
        match (seq, self.exact_through) {
            (None, _) | (Some(_), None) => true,
            (Some(s), Some(e)) => s > e,
        }
    }

    pub(crate) fn on_au(&mut self, au: &[u8], seq: Option<u64>) -> Result<(), String> {
        let started = Instant::now();
        let Some(decoder) = self.decoder.as_mut() else {
            return Err("this build has no hardware H.264 decoder".to_owned());
        };
        let decoded = match decoder.decode(au) {
            Ok(d) => d,
            Err(_) => {
                // One bad AU is not terminal — the next keyframe recovers the
                // stream. Counted so the HUD's decode_errors line shows it.
                self.stats.update(|s| s.decode_errors += 1);
                return Ok(());
            }
        };
        let (w, h) = (decoded.width(), decoded.height());
        if (w, h) != self.wire_size {
            // The host's display mode changed under the session. Never a silent
            // crop (review S-m8).
            return Err(format!(
                "host display mode changed ({w}x{h} vs session {}x{}); reconnect",
                self.wire_size.0, self.wire_size.1
            ));
        }
        if self.has_base && !self.accepts_au(seq) {
            self.suppressed += 1;
            return Ok(());
        }
        let bytes = au.len() as u64;
        let decode_us = started.elapsed().as_micros().min(u128::from(u32::MAX)) as u32;
        {
            let mut store = self
                .store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            store
                .adopt_pixels(OUTPUT_SURFACE, decoded.into_data())
                .map_err(|e| format!("surface adopt: {e}"))?;
            let generation = store.generation();
            self.record_paint(bytes, generation, Some(decode_us));
        }
        self.exact_through = seq;
        self.has_base = true;
        (self.wake)();
        self.try_apply_pending();
        Ok(())
    }

    pub(crate) fn on_tile_au(&mut self, tile_id: u8, au: &[u8], seq: u64) -> Result<(), String> {
        let started = Instant::now();
        let Some(tile_index) = self
            .tile_decoders
            .iter()
            .position(|tile| tile.header.id == tile_id)
        else {
            return Err(format!("host sent unadvertised tile {tile_id}"));
        };
        let (header, decoded, suppress_paint) = {
            let tile = &mut self.tile_decoders[tile_index];
            let suppress_paint =
                tile.has_base && tile.exact_through.is_some_and(|exact| seq <= exact);
            let decoded = match tile.decoder.decode(au) {
                Ok(decoded) => decoded,
                Err(_) => {
                    self.stats.update(|stats| stats.decode_errors += 1);
                    return Ok(());
                }
            };
            (tile.header, decoded, suppress_paint)
        };
        if (decoded.width(), decoded.height()) != (header.width, header.height) {
            return Err(format!(
                "tile {tile_id} decoded at {}x{}, expected {}x{}",
                decoded.width(),
                decoded.height(),
                header.width,
                header.height
            ));
        }
        let dest = Self::tile_dest(tile_id, header)?;
        if suppress_paint {
            self.suppressed += 1;
        }
        let bytes = au.len() as u64;
        let decode_us = started.elapsed().as_micros().min(u128::from(u32::MAX)) as u32;

        // There is only one pixel-bearing in-flight sequence. TCP preserves the
        // server's tile order, so a newer sequence supersedes an incomplete one;
        // an older late tile is decoded for reference state and then discarded.
        let mut progress = match self.tile_frame.take() {
            Some(progress) if progress.seq == seq => progress,
            Some(progress) if progress.seq > seq => {
                self.tile_frame = Some(progress);
                return Ok(());
            }
            Some(_) | None => TileFrameProgress {
                seen: vec![false; self.tile_decoders.len()],
                changed: false,
                tiles: (0..self.tile_decoders.len()).map(|_| None).collect(),
                seq,
                bytes: 0,
                decode_us: 0,
            },
        };

        // A repeated tile in one sequence still has to be decoded, but it must
        // not replace the first staged pixels or inflate one logical frame.
        if !progress.seen[tile_index] {
            progress.seen[tile_index] = true;
            progress.bytes = progress.bytes.saturating_add(bytes);
            progress.decode_us = progress.decode_us.saturating_add(decode_us);
            if !suppress_paint {
                progress.changed = true;
                progress.tiles[tile_index] = Some(StagedTile {
                    dest,
                    pixels: decoded.into_data(),
                });
            }
        }

        if !progress.seen.iter().all(|seen| *seen) {
            self.tile_frame = Some(progress);
            return Ok(());
        }

        let bytes = progress.bytes;
        let decode_us = progress.decode_us;
        let changed = progress.changed;
        let generation = if changed {
            let updates: Vec<_> = progress
                .tiles
                .iter()
                .filter_map(|tile| {
                    tile.as_ref()
                        .map(|tile| (tile.dest, tile.pixels.as_slice()))
                })
                .collect();
            let mut store = self
                .store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let generation = store
                .blit_rgba_strict_batch(OUTPUT_SURFACE, &updates)
                .map_err(|e| format!("tile batch: {e}"))?;

            // The decoder already consumed every AU above. Visible exactness
            // advances only after the complete batch passed validation and was
            // copied into the output surface.
            for (tile_index, staged) in progress.tiles.iter().enumerate() {
                if staged.is_some() {
                    self.tile_decoders[tile_index].exact_through = Some(seq);
                }
                self.tile_decoders[tile_index].has_base = true;
            }
            self.has_base = true;
            self.exact_through = self
                .tile_decoders
                .iter()
                .map(|tile| tile.exact_through)
                .collect::<Option<Vec<_>>>()
                .and_then(|seqs| seqs.into_iter().min());
            self.record_paint(bytes, generation, Some(decode_us));
            Some(generation)
        } else {
            // A wholly suppressed sequence still counts as an arrival, but it
            // has no visible mutation or wake. Decoder references progressed
            // through every AU before this branch.
            self.has_base = self.tile_decoders.iter().all(|tile| tile.has_base);
            self.exact_through = self
                .tile_decoders
                .iter()
                .map(|tile| tile.exact_through)
                .collect::<Option<Vec<_>>>()
                .and_then(|seqs| seqs.into_iter().min());
            None
        };
        if generation.is_some() {
            (self.wake)();
            self.try_apply_pending();
        }
        Ok(())
    }

    /// Decode one complete regional/full video update, then publish only its
    /// declared coverage as one visible mutation.
    pub(crate) fn on_video_update(&mut self, update: VideoUpdate) -> Result<(), String> {
        if (update.frame_width, update.frame_height) != self.wire_size {
            return Err(format!(
                "host display mode changed ({}x{} vs session {}x{}); reconnect",
                update.frame_width, update.frame_height, self.wire_size.0, self.wire_size.1
            ));
        }
        let stale = self
            .exact_through
            .is_some_and(|exact| update.frame_seq <= exact);
        let adjacent =
            self.exact_through.and_then(|exact| exact.checked_add(1)) == Some(update.frame_seq);
        if !stale && update.kind != VideoKind::Recovery && (!self.has_base || !adjacent) {
            return Err(format!(
                "video update {} cannot follow {:?} without recovery",
                update.frame_seq, self.exact_through
            ));
        }

        // Validate the advertised layout before advancing any decoder reference.
        let mut selected = Vec::with_capacity(update.tiles.len());
        let mut seen = vec![false; self.tile_decoders.len()];
        for wire_tile in &update.tiles {
            let Some(tile_index) = self
                .tile_decoders
                .iter()
                .position(|tile| tile.header.id == wire_tile.tile_id)
            else {
                return Err(format!("host sent unadvertised tile {}", wire_tile.tile_id));
            };
            if std::mem::replace(&mut seen[tile_index], true) {
                return Err(format!("host repeated tile {}", wire_tile.tile_id));
            }
            let header = self.tile_decoders[tile_index].header;
            let tile_right = header.x.saturating_add(header.width);
            let tile_bottom = header.y.saturating_add(header.height);
            for region in &wire_tile.coverage {
                let right = region.x.saturating_add(region.width);
                let bottom = region.y.saturating_add(region.height);
                if region.x < header.x
                    || region.y < header.y
                    || right > tile_right
                    || bottom > tile_bottom
                {
                    return Err(format!(
                        "tile {} coverage {},{} {}x{} is outside its advertised bounds",
                        wire_tile.tile_id, region.x, region.y, region.width, region.height
                    ));
                }
            }
            selected.push(tile_index);
        }

        let started = Instant::now();
        let mut bytes = 0u64;
        let mut staged = Vec::new();
        for (wire_tile, tile_index) in update.tiles.iter().zip(selected.iter().copied()) {
            let tile = &mut self.tile_decoders[tile_index];
            let decoded = match tile.decoder.decode(&wire_tile.au) {
                Ok(decoded) => decoded,
                Err(_) => {
                    self.stats.update(|stats| stats.decode_errors += 1);
                    return Ok(());
                }
            };
            if (decoded.width(), decoded.height()) != (tile.header.width, tile.header.height) {
                return Err(format!(
                    "tile {} decoded at {}x{}, expected {}x{}",
                    tile.header.id,
                    decoded.width(),
                    decoded.height(),
                    tile.header.width,
                    tile.header.height
                ));
            }
            bytes = bytes.saturating_add(wire_tile.au.len() as u64);
            let pixels = decoded.into_data();
            if wire_tile.coverage.as_slice()
                == [rhydra::adaptive::Region {
                    x: tile.header.x,
                    y: tile.header.y,
                    width: tile.header.width,
                    height: tile.header.height,
                }]
            {
                staged.push(StagedVideoRegion {
                    dest: Self::tile_dest(tile.header.id, tile.header)?,
                    pixels,
                });
                tile.has_base = true;
                continue;
            }
            for region in &wire_tile.coverage {
                let width = region.width as usize;
                let height = region.height as usize;
                let local_x = (region.x - tile.header.x) as usize;
                let local_y = (region.y - tile.header.y) as usize;
                let source_width = tile.header.width as usize;
                let mut packed = Vec::with_capacity(width * height * 4);
                for row in local_y..local_y + height {
                    let start = (row * source_width + local_x) * 4;
                    packed.extend_from_slice(&pixels[start..start + width * 4]);
                }
                staged.push(StagedVideoRegion {
                    dest: Rect::new(
                        region.x as u16,
                        region.y as u16,
                        (region.x + region.width) as u16,
                        (region.y + region.height) as u16,
                    ),
                    pixels: packed,
                });
            }
            tile.has_base = true;
        }

        if stale {
            self.suppressed += 1;
            return Ok(());
        }
        let updates: Vec<_> = staged
            .iter()
            .map(|region| (region.dest, region.pixels.as_slice()))
            .collect();
        let decode_us = started.elapsed().as_micros().min(u128::from(u32::MAX)) as u32;
        {
            let mut store = self
                .store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let generation = store
                .blit_rgba_strict_batch(OUTPUT_SURFACE, &updates)
                .map_err(|error| format!("video update batch: {error}"))?;
            self.record_paint(bytes, generation, Some(decode_us));
        }
        self.has_base = self.tile_decoders.iter().all(|tile| tile.has_base);
        self.exact_through = Some(update.frame_seq);
        for tile_index in selected {
            let tile = &mut self.tile_decoders[tile_index];
            tile.exact_through = Some(
                tile.exact_through
                    .map_or(update.frame_seq, |seq| seq.max(update.frame_seq)),
            );
        }
        (self.wake)();
        self.try_apply_pending();
        Ok(())
    }

    fn tile_dest(tile_id: u8, header: TileHeader) -> Result<Rect, String> {
        let right = header
            .x
            .checked_add(header.width)
            .ok_or_else(|| format!("tile {tile_id} right edge overflows"))?;
        let bottom = header
            .y
            .checked_add(header.height)
            .ok_or_else(|| format!("tile {tile_id} bottom edge overflows"))?;
        let edge = |value: u32, name: &str| {
            u16::try_from(value)
                .map_err(|_| format!("tile {tile_id} {name} edge {value} exceeds the canvas"))
        };
        Ok(Rect::new(
            edge(header.x, "left")?,
            edge(header.y, "top")?,
            edge(right, "right")?,
            edge(bottom, "bottom")?,
        ))
    }

    pub(crate) fn on_rects(&mut self, payload: &[u8]) -> Result<(), String> {
        let update = rects::decode(payload).map_err(|e| format!("rects payload: {e}"))?;
        self.apply_update(update)
    }

    fn apply_update(&mut self, update: RectUpdate) -> Result<(), String> {
        // An empty update must not advance exactness: it would claim a frame's
        // content on the word of a message that carried none.
        if update.rects.is_empty() {
            self.skipped_empty += 1;
            return Ok(());
        }
        if !self.has_base {
            // Rects can legitimately beat the first decodable keyframe.
            self.skipped_before_base += 1;
            return Ok(());
        }
        if (update.frame_width, update.frame_height) != self.wire_size {
            // Mode change mid-stream: terminal, never a silent crop (HLD §6).
            return Err(format!(
                "host display mode changed ({}x{} vs session {}x{}); reconnect",
                update.frame_width, update.frame_height, self.wire_size.0, self.wire_size.1
            ));
        }
        match self.exact_through {
            Some(e) if update.frame_seq == e + 1 => {}
            Some(e) if update.frame_seq <= e => {
                self.skipped_stale += 1;
                return Ok(());
            }
            _ => {
                // A gap: hold it (one slot, newest wins). Painting over the hole
                // would lose the missing frame's content forever; skipping alone
                // starves the fast path at rect cadence (decisions 13/16).
                self.held += 1;
                match &self.pending {
                    Some(old) if old.update.frame_seq >= update.frame_seq => {}
                    _ => self.pending = Some(PendingRects { update }),
                }
                return Ok(());
            }
        }
        self.paint(&update)?;
        self.try_apply_pending();
        Ok(())
    }

    /// Blit every rect of an already-gated update; exactness advances only when
    /// every blit succeeded (review S-m9).
    fn paint(&mut self, update: &RectUpdate) -> Result<(), String> {
        let painted_bytes = update.rects.iter().fold(0u64, |total, rect| {
            total.saturating_add(rect.pixels.len() as u64)
        });
        let updates = update.rects.iter().map(|rect| {
            (
                Rect::new(
                    rect.x,
                    rect.y,
                    rect.x.saturating_add(rect.w),
                    rect.y.saturating_add(rect.h),
                ),
                rect.pixels.as_slice(),
            )
        });
        {
            let mut store = self
                .store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let generation = store
                .blit_bgra_strict_batch(OUTPUT_SURFACE, updates)
                .map_err(|e| format!("rect batch: {e}"))?;
            self.record_paint(painted_bytes, generation, None);
        }
        self.exact_through = Some(update.frame_seq);
        for tile in &mut self.tile_decoders {
            tile.exact_through = Some(
                tile.exact_through
                    .map_or(update.frame_seq, |seq| seq.max(update.frame_seq)),
            );
        }
        if self
            .tile_frame
            .as_ref()
            .is_some_and(|frame| frame.seq <= update.frame_seq)
        {
            // The rect update now carries this sequence's visible content; a
            // staged tile set at or below it must not later overwrite that
            // exact canvas. Newer staged sequences remain eligible.
            self.tile_frame = None;
        }
        (self.wake)();
        Ok(())
    }

    fn try_apply_pending(&mut self) {
        if let Some(held) = self.pending.take() {
            // Re-enters the gate: applies if now adjacent, drops as stale if an
            // AU jumped past it, or goes back on hold.
            let _ = self.apply_update(held.update);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, TcpListener};

    #[test]
    fn capture_gaps_reset_only_on_an_explicit_quiet_boundary() {
        let mut continuity = CaptureContinuity::default();
        let frame = |capture_pos, pcm| AudioFrame {
            sample_rate: 48_000,
            channels: 2,
            capture_pos,
            pcm,
        };
        assert!(!continuity.observe_frame(&frame(100, vec![0; 40])));
        assert!(!continuity.observe_frame(&frame(110, vec![0; 40])));
        assert!(continuity.observe_frame(&frame(130, vec![0; 40])));
        assert!(!continuity.observe_frame(&frame(0, Vec::new())));
        assert!(!continuity.observe_frame(&frame(1_000, vec![0; 40])));
        assert!(!continuity.observe_frame(&frame(1_010, vec![0; 40])));
    }

    #[test]
    fn native_audio_counters_report_queue_drops_and_capture_gaps() {
        let counters = AudioCounters::new();
        counters.note_capture_gap();
        let (tx, _rx) = std::sync::mpsc::sync_channel(0);
        enqueue_audio(
            &tx,
            AudioFrame {
                sample_rate: 48_000,
                channels: 1,
                capture_pos: 0,
                pcm: vec![0; 20],
            },
            &counters,
        );
        let report = counters.metrics();
        assert_eq!(report.queue_drops, 1);
        assert_eq!(report.capture_gaps, 1);
    }

    #[test]
    fn native_audio_probe_reports_nonzero_rms_for_each_channel() {
        let counters = AudioCounters::new();
        let frames = 4_800;
        let mut stereo = Vec::with_capacity(frames * 2);
        for sample in 0..frames {
            let time = sample as f32 / 48_000.0;
            stereo.push((std::f32::consts::TAU * 440.0 * time).sin() * 0.5);
            stereo.push((std::f32::consts::TAU * 660.0 * time).sin() * 0.25);
        }

        counters.note_probe(&stereo, 48_000, 2);
        let (_, _, _, left_rms_micros, right_rms_micros) = counters.report();
        assert!(left_rms_micros > 300_000, "left RMS must prove signal");
        assert!(right_rms_micros > 150_000, "right RMS must prove signal");
    }

    /// A pasteboard that holds nothing and never changes, so the poll thread
    /// runs its real loop without touching the developer's own clipboard.
    struct InertClipboard;

    impl TextClipboard for InertClipboard {
        fn read_text(&mut self) -> Result<Option<String>, String> {
            Ok(Some(String::new()))
        }
        fn write_text(&mut self, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn tearing_down_the_auxiliary_channel_stops_all_three_threads() {
        // The one way this unit can hurt a user: a clipboard thread that will
        // not stop holds the whole session's teardown open. All three park
        // somewhere different — the reader in `read`, the writer on a condvar,
        // the poller in `sleep` — so each needs its own thing to happen, in
        // order: close the slot, drop the socket, then join.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let peer = std::thread::spawn(move || listener.accept().map(|(s, _)| s));
        let socket = TcpStream::connect(addr).unwrap();
        let held_open = peer.join().unwrap().unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let aux = spawn_aux(
            socket,
            &mut || Box::new(InertClipboard) as Box<dyn TextClipboard>,
            Policy::default(),
            Arc::clone(&stop),
            // The teardown test is about threads, not sound.
            None,
        )
        .expect("threads should start");

        // Run the teardown on its own thread so a hang FAILS this test rather
        // than stalling it forever. A bare `aux.shutdown()` here would give a
        // wrong implementation nothing worse than an infinite wait, which no
        // assertion can catch.
        let finished = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&finished);
        stop.store(true, Ordering::Relaxed);
        std::thread::spawn(move || {
            aux.shutdown();
            flag.store(true, Ordering::SeqCst);
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        while !finished.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "the auxiliary channel did not tear down: a clipboard thread is still parked"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(held_open);
    }

    use super::*;
    use ironrdp_egfx::decode::{DecodedFrame, DecoderResult};
    use rhydra::input_proto::decode_record;
    use rhydra::rects::Rect as WireRect;
    use std::io::{self, Write};
    use std::sync::atomic::AtomicUsize;

    /// A decoder whose "AU" is one byte: the fill value of the produced frame.
    /// Distinct fills make "which frame's pixels are on the surface" checkable.
    struct FakeDecoder {
        size: (u32, u32),
    }

    impl H264Decoder for FakeDecoder {
        fn decode(&mut self, data: &[u8]) -> DecoderResult<DecodedFrame> {
            let (w, h) = self.size;
            let fill = data.first().copied().unwrap_or(0);
            Ok(DecodedFrame::new(vec![fill; (w * h * 4) as usize], w, h))
        }
    }

    /// A 1x1 decoder whose AU is the exact RGBA pixel it produces.
    struct PixelDecoder;

    impl H264Decoder for PixelDecoder {
        fn decode(&mut self, data: &[u8]) -> DecoderResult<DecodedFrame> {
            Ok(DecodedFrame::new(data.to_vec(), 1, 1))
        }
    }

    struct CountingFakeDecoder {
        size: (u32, u32),
        calls: Arc<AtomicUsize>,
    }

    impl H264Decoder for CountingFakeDecoder {
        fn decode(&mut self, data: &[u8]) -> DecoderResult<DecodedFrame> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let (w, h) = self.size;
            let fill = data.first().copied().unwrap_or(0);
            Ok(DecodedFrame::new(vec![fill; (w * h * 4) as usize], w, h))
        }
    }

    fn sink_with_store(size: (u32, u32)) -> (NativeSink, Arc<Mutex<SurfaceStore>>) {
        sink_with_clock(size, InputClock::default())
    }

    fn sink_with_clock(
        size: (u32, u32),
        clock: InputClock,
    ) -> (NativeSink, Arc<Mutex<SurfaceStore>>) {
        let store = Arc::new(Mutex::new(SurfaceStore::new()));
        {
            let mut s = store.lock().unwrap();
            s.create(OUTPUT_SURFACE, size.0 as u16, size.1 as u16);
            s.map_to_output(OUTPUT_SURFACE);
        }
        let sink = NativeSink::new(
            Some(Box::new(FakeDecoder { size })),
            Arc::clone(&store),
            size,
            Box::new(|| {}),
            StatsHandle::new(),
            clock,
        );
        (sink, store)
    }

    fn surface_fill(store: &Arc<Mutex<SurfaceStore>>) -> u8 {
        let guard = store.lock().unwrap();
        guard.get(OUTPUT_SURFACE).unwrap().pixels()[0]
    }

    #[test]
    fn cursor_state_messages_reach_the_platform_cursor_without_painting() {
        let (mut sink, _) = sink_with_store((1, 1));
        let mut updates = Vec::new();
        let mut collect = |update| updates.push(update);

        dispatch_video_message(
            &framing::Message {
                msg_type: framing::MSG_CURSOR,
                payload: framing::encode_cursor(true).to_vec(),
            },
            &mut sink,
            &mut collect,
        )
        .unwrap();
        dispatch_video_message(
            &framing::Message {
                msg_type: framing::MSG_CURSOR,
                payload: framing::encode_cursor(false).to_vec(),
            },
            &mut sink,
            &mut collect,
        )
        .unwrap();

        assert_eq!(updates, [CursorUpdate::Hidden, CursorUpdate::Default]);
    }

    #[test]
    fn remote_video_eof_closes_the_window_signal() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let mut video = TcpStream::connect(addr).unwrap();
        let (peer, _) = listener.accept().unwrap();
        drop(peer);

        let (mut sink, _) = sink_with_store((1, 1));
        let mut reassembler = Reassembler::default();
        let mut on_cursor = |_| {};
        let stop = AtomicBool::new(false);
        let end = pump_video(
            &mut video,
            &mut reassembler,
            &mut sink,
            &mut on_cursor,
            &stop,
        );
        let (closed_tx, closed_rx) = std::sync::mpsc::sync_channel(1);
        let end = finish_video_worker(end, &stop, || closed_tx.send(()).unwrap());

        assert!(
            matches!(end, SessionEnd::TransportFailed(reason) if reason.contains("closed the video channel"))
        );
        assert_eq!(closed_rx.try_recv(), Ok(()));
        assert!(stop.load(Ordering::Relaxed));
    }

    #[test]
    fn malformed_video_framing_closes_the_window_signal() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let mut video = TcpStream::connect(addr).unwrap();
        let (_peer, _) = listener.accept().unwrap();

        let (mut sink, _) = sink_with_store((1, 1));
        let mut reassembler = Reassembler::default();
        reassembler.push(&0u32.to_le_bytes());
        let mut on_cursor = |_| {};
        let stop = AtomicBool::new(false);
        let end = pump_video(
            &mut video,
            &mut reassembler,
            &mut sink,
            &mut on_cursor,
            &stop,
        );
        let (closed_tx, closed_rx) = std::sync::mpsc::sync_channel(1);
        let end = finish_video_worker(end, &stop, || closed_tx.send(()).unwrap());

        assert!(
            matches!(end, SessionEnd::TransportFailed(reason) if reason.starts_with("framing:"))
        );
        assert_eq!(closed_rx.try_recv(), Ok(()));
        assert!(stop.load(Ordering::Relaxed));
    }

    #[test]
    fn remote_input_eof_closes_the_window_signal() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let sock = TcpStream::connect(addr).unwrap();
        let (peer, _) = listener.accept().unwrap();
        drop(peer);

        let (_bell, wake_rx) = crate::wake::doorbell().unwrap();
        let (_input_tx, input_rx) = std::sync::mpsc::channel();
        let (_commands_tx, commands) = std::sync::mpsc::channel();
        let stop = AtomicBool::new(false);
        let failure = pump_input(
            sock,
            Arc::new(LatestMouseMove::default()),
            input_rx,
            commands,
            wake_rx,
            &stop,
            InputClock::default(),
        );
        let (closed_tx, closed_rx) = std::sync::mpsc::sync_channel(1);
        let failure = finish_input_worker(failure, &stop, || closed_tx.send(()).unwrap());

        assert!(failure.is_some_and(|reason| reason.contains("closed the input channel")));
        assert_eq!(closed_rx.try_recv(), Ok(()));
        assert!(stop.load(Ordering::Relaxed));
    }

    #[test]
    fn intentional_native_shutdown_does_not_send_a_second_close() {
        let stop = AtomicBool::new(true);
        let _ = finish_video_worker(
            SessionEnd::TransportFailed("late video error".to_owned()),
            &stop,
            || panic!("intentional shutdown must not close the window again"),
        );
        let _ = finish_input_worker(Some("late input error".to_owned()), &stop, || {
            panic!("intentional shutdown must not close the window again")
        });
    }

    #[test]
    fn two_tiles_with_one_capture_sequence_both_paint() {
        let size = (4, 2);
        let store = Arc::new(Mutex::new(SurfaceStore::new()));
        {
            let mut surface = store.lock().unwrap();
            surface.create(OUTPUT_SURFACE, size.0 as u16, size.1 as u16);
            surface.map_to_output(OUTPUT_SURFACE);
        }
        let tiles = vec![
            super::super::probe::TileHeader {
                id: 0,
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
            super::super::probe::TileHeader {
                id: 1,
                x: 2,
                y: 0,
                width: 2,
                height: 2,
            },
        ];
        let decoders = vec![
            NativeDecoder::H264(Box::new(FakeDecoder { size: (2, 2) })),
            NativeDecoder::H264(Box::new(FakeDecoder { size: (2, 2) })),
        ];
        let stats = StatsHandle::new();
        let mut sink = NativeSink::new_tiled(
            decoders,
            tiles,
            Arc::clone(&store),
            size,
            Box::new(|| {}),
            stats.clone(),
            InputClock::default(),
        );

        let before = store.lock().unwrap().generation();
        sink.on_tile_au(0, &[0x11], 7).unwrap();
        assert_eq!(stats.snapshot().frames, 0, "half a 5K frame is not a frame");
        assert_eq!(
            store.lock().unwrap().generation(),
            before,
            "staging the first tile must not publish a generation"
        );
        sink.on_tile_au(1, &[0x22], 7).unwrap();
        assert_eq!(stats.snapshot().frames, 1, "both tiles are one frame");

        let guard = store.lock().unwrap();
        assert_eq!(
            guard.generation(),
            before + 1,
            "one logical frame is one commit"
        );
        let pixels = guard.get(OUTPUT_SURFACE).unwrap().pixels();
        assert_eq!(pixels[0], 0x11);
        assert_eq!(pixels[(2 * 4) as usize], 0x22);
    }

    #[test]
    fn the_first_tile_keeps_the_last_presentation_until_commit() {
        let size = (4, 2);
        let store = Arc::new(Mutex::new(SurfaceStore::new()));
        {
            let mut surface = store.lock().unwrap();
            surface.create(OUTPUT_SURFACE, size.0 as u16, size.1 as u16);
            surface.map_to_output(OUTPUT_SURFACE);
            surface
                .blit_rgba_strict(OUTPUT_SURFACE, Rect::new(0, 0, 4, 2), &[7; 32])
                .unwrap();
        }
        let mut sink = NativeSink::new_tiled(
            vec![
                NativeDecoder::H264(Box::new(FakeDecoder { size: (2, 2) })),
                NativeDecoder::H264(Box::new(FakeDecoder { size: (2, 2) })),
            ],
            vec![
                super::super::probe::TileHeader {
                    id: 0,
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 2,
                },
                super::super::probe::TileHeader {
                    id: 1,
                    x: 2,
                    y: 0,
                    width: 2,
                    height: 2,
                },
            ],
            Arc::clone(&store),
            size,
            Box::new(|| {}),
            StatsHandle::new(),
            InputClock::default(),
        );

        let before = store.lock().unwrap().generation();
        sink.on_tile_au(0, &[0x11], 7).unwrap();

        let mut snapshot = crate::surface::PresentationSnapshot::default();
        let guard = store.lock().unwrap();
        assert_eq!(guard.generation(), before);
        assert!(guard.copy_presentation(&mut snapshot));
        assert_eq!(snapshot.generation, before);
        assert_eq!(snapshot.pixels, vec![7; 32]);
    }

    #[test]
    fn a_malformed_later_tile_cannot_partially_paint_a_staged_batch() {
        let size = (4, 2);
        let store = Arc::new(Mutex::new(SurfaceStore::new()));
        {
            let mut surface = store.lock().unwrap();
            surface.create(OUTPUT_SURFACE, size.0 as u16, size.1 as u16);
            surface.map_to_output(OUTPUT_SURFACE);
            surface
                .blit_rgba_strict(OUTPUT_SURFACE, Rect::new(0, 0, 4, 2), &[7; 32])
                .unwrap();
        }
        let mut sink = NativeSink::new_tiled(
            vec![
                NativeDecoder::H264(Box::new(FakeDecoder { size: (2, 2) })),
                NativeDecoder::H264(Box::new(FakeDecoder { size: (2, 2) })),
            ],
            vec![
                super::super::probe::TileHeader {
                    id: 0,
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 2,
                },
                // The decoded extent is valid, but this advertised destination
                // overhangs the output and must fail the batch preflight.
                super::super::probe::TileHeader {
                    id: 1,
                    x: 3,
                    y: 0,
                    width: 2,
                    height: 2,
                },
            ],
            Arc::clone(&store),
            size,
            Box::new(|| {}),
            StatsHandle::new(),
            InputClock::default(),
        );

        let before = store.lock().unwrap().generation();
        sink.on_tile_au(0, &[0x11], 7).unwrap();
        let error = sink.on_tile_au(1, &[0x22], 7).unwrap_err();
        assert!(error.contains("tile batch"));

        let mut snapshot = crate::surface::PresentationSnapshot::default();
        let guard = store.lock().unwrap();
        assert_eq!(guard.generation(), before);
        assert!(guard.copy_presentation(&mut snapshot));
        assert_eq!(snapshot.pixels, vec![7; 32]);
    }

    #[test]
    fn a_tiled_sequence_wakes_once_after_all_tiles_arrive() {
        let size = (4, 2);
        let store = Arc::new(Mutex::new(SurfaceStore::new()));
        {
            let mut surface = store.lock().unwrap();
            surface.create(OUTPUT_SURFACE, size.0 as u16, size.1 as u16);
            surface.map_to_output(OUTPUT_SURFACE);
        }
        let wake_count = Arc::new(AtomicUsize::new(0));
        let wake = Arc::clone(&wake_count);
        let mut sink = NativeSink::new_tiled(
            vec![
                NativeDecoder::H264(Box::new(FakeDecoder { size: (2, 2) })),
                NativeDecoder::H264(Box::new(FakeDecoder { size: (2, 2) })),
            ],
            vec![
                super::super::probe::TileHeader {
                    id: 0,
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 2,
                },
                super::super::probe::TileHeader {
                    id: 1,
                    x: 2,
                    y: 0,
                    width: 2,
                    height: 2,
                },
            ],
            Arc::clone(&store),
            size,
            Box::new(move || {
                wake.fetch_add(1, Ordering::Relaxed);
            }),
            StatsHandle::new(),
            InputClock::default(),
        );

        sink.on_tile_au(0, &[0x11], 7).unwrap();
        assert_eq!(
            wake_count.load(Ordering::Relaxed),
            0,
            "an incomplete logical frame must not wake"
        );
        sink.on_tile_au(1, &[0x22], 7).unwrap();
        assert_eq!(
            wake_count.load(Ordering::Relaxed),
            1,
            "two tiles are one logical frame"
        );

        // Both AUs are fully redundant, but still need decoding to keep the H.264
        // reference chains correct. They must not schedule another redraw.
        sink.on_tile_au(0, &[0x33], 7).unwrap();
        sink.on_tile_au(1, &[0x44], 7).unwrap();
        assert_eq!(
            wake_count.load(Ordering::Relaxed),
            1,
            "a redundant sequence must not wake"
        );
    }

    #[test]
    fn a_suppressed_tile_still_completes_a_changed_logical_frame() {
        let size = (4, 2);
        let store = Arc::new(Mutex::new(SurfaceStore::new()));
        {
            let mut surface = store.lock().unwrap();
            surface.create(OUTPUT_SURFACE, size.0 as u16, size.1 as u16);
            surface.map_to_output(OUTPUT_SURFACE);
        }
        let wake_count = Arc::new(AtomicUsize::new(0));
        let wake = Arc::clone(&wake_count);
        let mut sink = NativeSink::new_tiled(
            vec![
                NativeDecoder::H264(Box::new(FakeDecoder { size: (2, 2) })),
                NativeDecoder::H264(Box::new(FakeDecoder { size: (2, 2) })),
            ],
            vec![
                super::super::probe::TileHeader {
                    id: 0,
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 2,
                },
                super::super::probe::TileHeader {
                    id: 1,
                    x: 2,
                    y: 0,
                    width: 2,
                    height: 2,
                },
            ],
            Arc::clone(&store),
            size,
            Box::new(move || {
                wake.fetch_add(1, Ordering::Relaxed);
            }),
            StatsHandle::new(),
            InputClock::default(),
        );
        sink.on_tile_au(0, &[0x11], 7).unwrap();
        sink.on_tile_au(1, &[0x22], 7).unwrap();
        assert_eq!(wake_count.load(Ordering::Relaxed), 1);

        // Model a rect update that made tile 0 exact through seq 8 while tile 1
        // still needs its encoded AU. Tile 0's decoded AU is an arrival, not a
        // reason to paint or wake; tile 1 completes the logical frame.
        sink.tile_decoders[0].exact_through = Some(8);
        sink.on_tile_au(0, &[0x33], 8).unwrap();
        assert_eq!(
            wake_count.load(Ordering::Relaxed),
            1,
            "the suppressed half is not complete yet"
        );
        sink.on_tile_au(1, &[0x44], 8).unwrap();
        assert_eq!(
            wake_count.load(Ordering::Relaxed),
            2,
            "a changed tile completes the logical frame"
        );
    }

    #[test]
    fn a_single_tile_stream_wakes_once_per_changed_sequence() {
        let store = Arc::new(Mutex::new(SurfaceStore::new()));
        {
            let mut surface = store.lock().unwrap();
            surface.create(OUTPUT_SURFACE, 1, 1);
            surface.map_to_output(OUTPUT_SURFACE);
        }
        let wake_count = Arc::new(AtomicUsize::new(0));
        let wake = Arc::clone(&wake_count);
        let mut sink = NativeSink::new_tiled(
            vec![NativeDecoder::H264(Box::new(PixelDecoder))],
            vec![super::super::probe::TileHeader {
                id: 4,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            }],
            Arc::clone(&store),
            (1, 1),
            Box::new(move || {
                wake.fetch_add(1, Ordering::Relaxed);
            }),
            StatsHandle::new(),
            InputClock::default(),
        );

        sink.on_tile_au(4, &[1, 2, 3, 4], 1).unwrap();
        assert_eq!(wake_count.load(Ordering::Relaxed), 1);
        sink.on_tile_au(4, &[9, 8, 7, 6], 1).unwrap();
        assert_eq!(wake_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn tiled_h264_preserves_distinct_rgba_channels() {
        let store = Arc::new(Mutex::new(SurfaceStore::new()));
        {
            let mut surface = store.lock().unwrap();
            surface.create(OUTPUT_SURFACE, 1, 1);
            surface.map_to_output(OUTPUT_SURFACE);
        }
        let mut sink = NativeSink::new_tiled(
            vec![NativeDecoder::H264(Box::new(PixelDecoder))],
            vec![super::super::probe::TileHeader {
                id: 7,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            }],
            Arc::clone(&store),
            (1, 1),
            Box::new(|| {}),
            StatsHandle::new(),
            InputClock::default(),
        );

        let rgba = [0x11, 0x22, 0x33, 0x44];
        sink.on_tile_au(7, &rgba, 1).unwrap();

        let guard = store.lock().unwrap();
        assert_eq!(guard.get(OUTPUT_SURFACE).unwrap().pixels(), rgba);
    }

    #[test]
    fn regional_video_paints_only_declared_coverage() {
        use rhydra::adaptive::Region;
        use rhydra::video_update::{VideoKind, VideoTile, VideoUpdate};

        let size = (4, 2);
        let store = Arc::new(Mutex::new(SurfaceStore::new()));
        {
            let mut surface = store.lock().unwrap();
            surface.create(OUTPUT_SURFACE, size.0 as u16, size.1 as u16);
            surface.map_to_output(OUTPUT_SURFACE);
        }
        let mut sink = NativeSink::new_tiled(
            vec![
                NativeDecoder::H264(Box::new(FakeDecoder { size: (2, 2) })),
                NativeDecoder::H264(Box::new(FakeDecoder { size: (2, 2) })),
            ],
            vec![
                super::super::probe::TileHeader {
                    id: 0,
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 2,
                },
                super::super::probe::TileHeader {
                    id: 1,
                    x: 2,
                    y: 0,
                    width: 2,
                    height: 2,
                },
            ],
            Arc::clone(&store),
            size,
            Box::new(|| {}),
            StatsHandle::new(),
            InputClock::default(),
        );

        sink.on_video_update(VideoUpdate {
            frame_seq: 1,
            frame_width: 4,
            frame_height: 2,
            block_size: 1,
            kind: VideoKind::Recovery,
            tiles: vec![
                VideoTile {
                    tile_id: 0,
                    coverage: vec![Region {
                        x: 0,
                        y: 0,
                        width: 2,
                        height: 2,
                    }],
                    au: vec![0x10],
                },
                VideoTile {
                    tile_id: 1,
                    coverage: vec![Region {
                        x: 2,
                        y: 0,
                        width: 2,
                        height: 2,
                    }],
                    au: vec![0x20],
                },
            ],
        })
        .unwrap();
        sink.on_video_update(VideoUpdate {
            frame_seq: 2,
            frame_width: 4,
            frame_height: 2,
            block_size: 1,
            kind: VideoKind::Regional,
            tiles: vec![VideoTile {
                tile_id: 0,
                coverage: vec![Region {
                    x: 1,
                    y: 0,
                    width: 1,
                    height: 1,
                }],
                au: vec![0x77],
            }],
        })
        .unwrap();

        let guard = store.lock().unwrap();
        let pixels = guard.get(OUTPUT_SURFACE).unwrap().pixels();
        let first = |x: usize, y: usize| pixels[(y * 4 + x) * 4];
        assert_eq!(first(0, 0), 0x10);
        assert_eq!(first(1, 0), 0x77);
        assert_eq!(first(0, 1), 0x10);
        assert_eq!(first(2, 0), 0x20);
        assert_eq!(first(3, 1), 0x20);
    }

    #[test]
    fn rect_completed_tile_aus_still_advance_both_decoder_reference_chains() {
        let size = (4, 2);
        let store = Arc::new(Mutex::new(SurfaceStore::new()));
        {
            let mut surface = store.lock().unwrap();
            surface.create(OUTPUT_SURFACE, size.0 as u16, size.1 as u16);
            surface.map_to_output(OUTPUT_SURFACE);
        }
        let tiles = vec![
            super::super::probe::TileHeader {
                id: 0,
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
            super::super::probe::TileHeader {
                id: 1,
                x: 2,
                y: 0,
                width: 2,
                height: 2,
            },
        ];
        let calls = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
        let decoders = calls
            .iter()
            .map(|calls| {
                NativeDecoder::H264(Box::new(CountingFakeDecoder {
                    size: (2, 2),
                    calls: Arc::clone(calls),
                }) as Box<dyn H264Decoder>)
            })
            .collect();
        let stats = StatsHandle::new();
        let mut sink = NativeSink::new_tiled(
            decoders,
            tiles,
            Arc::clone(&store),
            size,
            Box::new(|| {}),
            stats.clone(),
            InputClock::default(),
        );

        sink.on_tile_au(0, &[0x11], 7).unwrap();
        sink.on_tile_au(1, &[0x22], 7).unwrap();
        sink.apply_update(one_rect_update(size, 8, 200)).unwrap();
        assert_eq!(stats.snapshot().frames, 2, "tile base plus rect update");

        sink.on_tile_au(0, &[0x33], 8).unwrap();
        sink.on_tile_au(1, &[0x44], 8).unwrap();

        assert_eq!(calls[0].load(Ordering::Relaxed), 2);
        assert_eq!(calls[1].load(Ordering::Relaxed), 2);
        assert_eq!(sink.suppressed, 2);
        assert_eq!(stats.snapshot().frames, 2, "redundant AUs must not repaint");
        let guard = store.lock().unwrap();
        let pixels = guard.get(OUTPUT_SURFACE).unwrap().pixels();
        assert_eq!(&pixels[0..4], &[0, 0, 200, 255], "rect result survives");
        assert_eq!(
            pixels[(2 * 4) as usize],
            0x22,
            "right tile was not repainted"
        );
    }

    /// One 1x1 BGRA rect update at (0,0) with the given seq and blue value.
    fn one_rect_update(size: (u32, u32), seq: u64, blue: u8) -> RectUpdate {
        RectUpdate {
            frame_seq: seq,
            frame_width: size.0,
            frame_height: size.1,
            rects: vec![WireRect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
                pixels: vec![blue, 0, 0, 255],
            }],
        }
    }

    #[test]
    fn an_au_adopts_the_decoded_frame_and_sets_exactness() {
        let (mut sink, store) = sink_with_store((4, 2));
        sink.on_au(&[7], Some(10)).unwrap();
        assert_eq!(surface_fill(&store), 7);
        assert_eq!(sink.exact_through, Some(10));
    }

    #[test]
    fn a_redundant_au_is_suppressed_not_painted() {
        let (mut sink, store) = sink_with_store((4, 2));
        sink.on_au(&[7], Some(10)).unwrap();
        sink.on_au(&[9], Some(10)).unwrap(); // same seq: provably redundant
        assert_eq!(surface_fill(&store), 7, "older content must survive");
        assert_eq!(sink.suppressed, 1);
    }

    #[test]
    fn an_adjacent_rect_update_paints_and_advances_exactness() {
        let (mut sink, store) = sink_with_store((4, 2));
        sink.on_au(&[7], Some(10)).unwrap();
        sink.apply_update(one_rect_update((4, 2), 11, 200)).unwrap();
        // BGRA [200,0,0,255] swizzles to RGBA [0,0,200,255].
        let guard = store.lock().unwrap();
        let px = &guard.get(OUTPUT_SURFACE).unwrap().pixels()[0..4];
        assert_eq!(px, [0, 0, 200, 255]);
        drop(guard);
        assert_eq!(sink.exact_through, Some(11));
    }

    #[test]
    fn a_malformed_later_rect_cannot_partially_paint_the_batch() {
        let size = (4, 2);
        let (mut sink, store) = sink_with_store(size);
        sink.on_au(&[7], Some(10)).unwrap();
        let before_generation = store.lock().unwrap().generation();
        let before_stats = sink.stats.snapshot();
        let update = RectUpdate {
            frame_seq: 11,
            frame_width: size.0,
            frame_height: size.1,
            rects: vec![
                WireRect {
                    x: 0,
                    y: 0,
                    w: 1,
                    h: 1,
                    pixels: vec![200, 0, 0, 255],
                },
                WireRect {
                    x: 4,
                    y: 0,
                    w: 1,
                    h: 1,
                    pixels: vec![201, 0, 0, 255],
                },
            ],
        };

        let error = sink.apply_update(update).unwrap_err();
        assert!(error.contains("rect batch"), "unexpected failure: {error}");
        let guard = store.lock().unwrap();
        assert_eq!(guard.generation(), before_generation);
        assert_eq!(
            guard.get(OUTPUT_SURFACE).unwrap().pixels(),
            vec![7; 4 * 2 * 4],
            "the valid first rectangle must remain hidden when a later rectangle fails"
        );
        drop(guard);
        assert_eq!(sink.exact_through, Some(10));
        assert_eq!(sink.stats.snapshot().frames, before_stats.frames);
        assert_eq!(sink.stats.snapshot().bytes_in, before_stats.bytes_in);
    }

    #[test]
    fn a_gap_rect_update_is_held_and_applies_when_the_au_closes_the_gap() {
        let (mut sink, store) = sink_with_store((4, 2));
        sink.on_au(&[7], Some(10)).unwrap();
        // Seq 12 is ahead of adjacency (11 is missing): held, nothing painted.
        sink.apply_update(one_rect_update((4, 2), 12, 200)).unwrap();
        assert_eq!(surface_fill(&store), 7, "a gap update must not paint");
        assert_eq!(sink.held, 1);
        // The AU for seq 11 closes the gap; the held update applies on its heels.
        sink.on_au(&[8], Some(11)).unwrap();
        let guard = store.lock().unwrap();
        let px = &guard.get(OUTPUT_SURFACE).unwrap().pixels()[0..4];
        assert_eq!(
            px,
            [0, 0, 200, 255],
            "held rects must apply after the gap closes"
        );
        drop(guard);
        assert_eq!(sink.exact_through, Some(12));
    }

    #[test]
    fn a_stale_rect_update_is_skipped_and_an_empty_one_never_advances_exactness() {
        let (mut sink, _store) = sink_with_store((4, 2));
        sink.on_au(&[7], Some(10)).unwrap();
        sink.apply_update(one_rect_update((4, 2), 9, 200)).unwrap();
        assert_eq!(sink.skipped_stale, 1);
        let empty = RectUpdate {
            frame_seq: 11,
            frame_width: 4,
            frame_height: 2,
            rects: vec![],
        };
        sink.apply_update(empty).unwrap();
        assert_eq!(sink.skipped_empty, 1);
        assert_eq!(
            sink.exact_through,
            Some(10),
            "neither may advance exactness"
        );
    }

    #[test]
    fn rects_before_the_first_au_are_dropped_and_counted() {
        let (mut sink, store) = sink_with_store((4, 2));
        sink.apply_update(one_rect_update((4, 2), 1, 200)).unwrap();
        assert_eq!(sink.skipped_before_base, 1);
        assert_eq!(surface_fill(&store), 0, "nothing to composite onto yet");
    }

    /// MDR-BUG-FLUX-00012: the rects path carries almost every frame of a live
    /// native session, and it used to paint without counting. `--sessions` then
    /// showed FRAMES 1 next to megabytes of RX, and FPS graded from a stat no
    /// rect update ever fed.
    #[test]
    fn every_painted_frame_is_counted_not_just_the_keyframe() {
        let (mut sink, _store) = sink_with_store((4, 2));
        sink.on_au(&[7], Some(10)).unwrap();
        sink.apply_update(one_rect_update((4, 2), 11, 200)).unwrap();
        sink.apply_update(one_rect_update((4, 2), 12, 201)).unwrap();
        let s = sink.stats.snapshot();
        assert_eq!(s.frames, 3, "the AU and both rect updates each painted");
        assert_eq!(
            s.codecs.get(CODEC_LABEL),
            Some(&3),
            "the codec tally must count every painted frame, not just the AU"
        );
        // 4 bytes of BGRA per rect update, plus the one-byte fake AU.
        assert_eq!(s.bytes_in, 9);

        // An update that paints nothing must not inflate the count.
        sink.apply_update(one_rect_update((4, 2), 9, 202)).unwrap(); // stale
        sink.apply_update(one_rect_update((4, 2), 99, 203)).unwrap(); // held: a gap
        let s = sink.stats.snapshot();
        assert_eq!(s.frames, 3, "a skipped or held update painted nothing");
        assert_eq!(s.codecs.get(CODEC_LABEL), Some(&3));
    }

    /// The blank P50 column: nothing on the native path ever recorded the
    /// input round trip, so `latency` stayed empty for the life of a session.
    #[test]
    fn a_paint_closes_the_outstanding_input_round_trip_on_both_paths() {
        let clock = InputClock::default();
        let (mut sink, _store) = sink_with_clock((4, 2), clock.clone());

        clock.stamp();
        sink.on_au(&[7], Some(10)).unwrap();
        assert_eq!(
            sink.stats.snapshot().latency.count(),
            1,
            "the AU path must close an outstanding round trip"
        );

        // Nothing outstanding: a paint the user did not ask for records nothing.
        sink.apply_update(one_rect_update((4, 2), 11, 200)).unwrap();
        assert_eq!(sink.stats.snapshot().latency.count(), 1);

        clock.stamp();
        sink.apply_update(one_rect_update((4, 2), 12, 201)).unwrap();
        assert_eq!(
            sink.stats.snapshot().latency.count(),
            2,
            "the rects path must close one too"
        );

        // An update that paints nothing must leave the clock running, or a
        // stale frame would answer the keystroke instead of the real one.
        clock.stamp();
        sink.apply_update(one_rect_update((4, 2), 9, 202)).unwrap();
        assert_eq!(sink.stats.snapshot().latency.count(), 2);
        assert!(clock.take_us().is_some(), "the stamp must still be pending");
    }

    #[test]
    fn only_the_first_unanswered_input_starts_the_round_trip_clock() {
        let clock = InputClock::default();
        clock.stamp();
        std::thread::sleep(Duration::from_millis(25));
        clock.stamp(); // a later keystroke must not restart the clock
        let us = clock.take_us().expect("a stamp is outstanding");
        assert!(
            us >= 25_000,
            "the clock must run from the FIRST input, got {us}us"
        );
        assert!(
            clock.take_us().is_none(),
            "taking the stamp must clear it: one paint answers one round trip"
        );
    }

    /// The wiring the two tests above cannot see: `native-input` is what stamps
    /// the clock, and a clock nobody stamps measures nothing.
    #[test]
    fn the_input_thread_stamps_the_clock_when_a_record_reaches_the_wire() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let sock = TcpStream::connect(addr).unwrap();
        // Hold the peer open: a readable input socket means EOF, which ends the pump.
        let _peer = listener.accept().unwrap().0;

        let (bell, wake_rx) = crate::wake::doorbell().unwrap();
        let (input_tx, input_rx) = std::sync::mpsc::channel();
        let (_commands_tx, commands) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let clock = InputClock::default();

        let thread_clock = clock.clone();
        let thread_stop = Arc::clone(&stop);
        let join = std::thread::spawn(move || {
            pump_input(
                sock,
                Arc::new(LatestMouseMove::default()),
                input_rx,
                commands,
                wake_rx,
                &thread_stop,
                thread_clock,
            )
        });

        input_tx.send(InputEvent::MouseMove { x: 1, y: 2 }).unwrap();
        bell.ring();

        let deadline = Instant::now() + Duration::from_secs(5);
        let stamped = loop {
            if let Some(us) = clock.take_us() {
                break Some(us);
            }
            assert!(
                Instant::now() < deadline,
                "the input thread never stamped the round-trip clock"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(stamped.is_some());

        stop.store(true, Ordering::Relaxed);
        bell.ring();
        let _ = join.join();
    }

    struct StampProbe {
        clock: InputClock,
        writes: usize,
        stamp_seen_on_first_write: bool,
        stamp_seen_on_second_write: bool,
    }

    impl Write for StampProbe {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            let stamped = self.clock.take_us().is_some();
            match self.writes {
                1 => self.stamp_seen_on_first_write = stamped,
                2 => self.stamp_seen_on_second_write = stamped,
                _ => {}
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl NativeTimedWrite for StampProbe {
        fn set_write_timeout(&mut self, _timeout: Option<Duration>) -> io::Result<()> {
            Ok(())
        }
    }

    impl NativeTimedWrite for Vec<u8> {
        fn set_write_timeout(&mut self, _timeout: Option<Duration>) -> io::Result<()> {
            Ok(())
        }
    }

    struct DribblingTimedWriter {
        bytes: Vec<u8>,
        timeouts: Vec<Duration>,
    }

    impl Write for DribblingTimedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let Some(&byte) = bytes.first() else {
                return Ok(0);
            };
            self.bytes.push(byte);
            Ok(1)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl NativeTimedWrite for DribblingTimedWriter {
        fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
            self.timeouts
                .push(timeout.expect("record writes are always bounded"));
            Ok(())
        }
    }

    #[test]
    fn dribbling_native_record_honours_one_absolute_deadline() {
        let base = Instant::now();
        let deadline = base + Duration::from_millis(5);
        let mut clock = base;
        let mut writer = DribblingTimedWriter {
            bytes: Vec::new(),
            timeouts: Vec::new(),
        };

        let error = write_record_until(&mut writer, b"0123456789", deadline, &mut || {
            clock += Duration::from_millis(1);
            clock
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(writer.bytes, b"0123");
        assert_eq!(
            writer.timeouts,
            [
                Duration::from_millis(4),
                Duration::from_millis(3),
                Duration::from_millis(2),
                Duration::from_millis(1),
            ]
        );
    }

    #[test]
    fn native_input_stamps_before_the_second_record_of_a_burst() {
        let clock = InputClock::default();
        let mut probe = StampProbe {
            clock: clock.clone(),
            writes: 0,
            stamp_seen_on_first_write: false,
            stamp_seen_on_second_write: false,
        };
        let event = InputEvent::MouseButton {
            button: MouseButton::Left,
            down: true,
            x: 7,
            y: 9,
        };
        let mut seq = 1;
        let mut clock_stamped = false;
        let mut written = 0;

        write_native_records(
            &mut probe,
            wire_records(&event, &mut seq),
            &clock,
            &mut clock_stamped,
            &mut written,
            NATIVE_INPUT_WRITE_TIMEOUT,
        )
        .unwrap();

        assert_eq!(
            probe.writes, 2,
            "a button event has move and button records"
        );
        assert!(
            !probe.stamp_seen_on_first_write,
            "the stamp must follow the first successful write"
        );
        assert!(
            probe.stamp_seen_on_second_write,
            "the first record must start the clock before the next write"
        );
        assert_eq!(written, 2);
        assert!(
            clock.take_us().is_none(),
            "the controlled writer consumed the single outstanding sample"
        );
    }

    #[test]
    fn native_reliable_input_batch_yields_to_commands() {
        let (input_tx, input_rx) = std::sync::mpsc::channel();
        for code in 0..=NATIVE_INPUT_BATCH_MAX {
            input_tx
                .send(InputEvent::Key {
                    scancode: crate::input::Scancode::plain(u8::try_from(code).unwrap()),
                    down: true,
                })
                .unwrap();
        }
        let (command_tx, commands) = std::sync::mpsc::channel();
        command_tx
            .send(SessionCommand::SetVisibility { visible: false })
            .unwrap();

        let mut writer = Vec::new();
        let mut clock_stamped = false;
        let mut seq = 1;
        let mut written = 0;
        let batch = drain_native_input_batch(
            NativeWriteBudget {
                writer: &mut writer,
                timeout: NATIVE_INPUT_WRITE_TIMEOUT,
            },
            &input_rx,
            &LatestMouseMove::default(),
            &InputClock::default(),
            &mut clock_stamped,
            &mut seq,
            &mut written,
        )
        .unwrap();

        assert_eq!(batch, NativeInputBatch::Sent { full: true });
        assert_eq!(written, NATIVE_INPUT_BATCH_MAX as u64);
        assert!(
            input_rx.try_recv().is_ok(),
            "reliable input beyond one batch must remain for the next turn"
        );
        let mut resize_noted = false;
        assert_eq!(
            service_native_commands(&commands, &mut resize_noted),
            1,
            "a queued command must be serviced after the bounded input turn"
        );
        assert_eq!(native_input_wait_after_batch(batch), Duration::ZERO);
    }

    #[test]
    fn a_nonreading_input_peer_times_out_instead_of_wedging_native_input() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let sock = TcpStream::connect(addr).unwrap();
        // Keep the receive side open but never read it: the sender must eventually
        // exhaust the kernel's window and exercise the native writer timeout.
        let peer = listener.accept().unwrap().0;

        let (bell, wake_rx) = crate::wake::doorbell().unwrap();
        let (input_tx, input_rx) = std::sync::mpsc::channel();
        let (_commands_tx, commands) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let clock = InputClock::default();
        let thread_stop = Arc::clone(&stop);
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        let join = std::thread::spawn(move || {
            let result = pump_input_with_timeout(
                sock,
                Arc::new(LatestMouseMove::default()),
                input_rx,
                commands,
                wake_rx,
                &thread_stop,
                clock,
                Duration::from_millis(25),
            );
            let _ = done_tx.send(result);
        });

        // A button carries two reliable records, so this bounded test burst exceeds a
        // loopback send window without depending on platform-specific socket options.
        let event = InputEvent::MouseButton {
            button: MouseButton::Left,
            down: true,
            x: 1,
            y: 2,
        };
        for _ in 0..1_000_000 {
            input_tx.send(event).unwrap();
        }
        bell.ring();

        let outcome = done_rx.recv_timeout(Duration::from_secs(3));
        if outcome.is_err() {
            // Let the no-timeout mutation below unwind cleanly instead of leaving a
            // writer thread parked on the deliberately stalled peer.
            stop.store(true, Ordering::Relaxed);
            bell.ring();
            drop(peer);
            let _ = join.join();
            panic!("native input writer did not reach its timeout");
        }
        let failure = outcome
            .unwrap()
            .expect("a timed-out write must fail the native input pump");
        assert!(
            failure.starts_with("input write:"),
            "unexpected native input failure: {failure}"
        );

        // The receiver is gone with the failed pump, so a later reliable event cannot
        // remain queued behind the stalled write or be accepted out of order.
        assert!(
            input_tx
                .send(InputEvent::Key {
                    scancode: crate::input::Scancode::plain(0x1e),
                    down: true,
                })
                .is_err(),
            "later input must not survive a failed native input channel"
        );
        stop.store(true, Ordering::Relaxed);
        bell.ring();
        drop(peer);
        join.join().unwrap();
    }

    #[test]
    fn a_key_bypasses_ten_thousand_window_moves_and_the_final_position_survives() {
        let latest = LatestMouseMove::default();
        for x in 0..10_000u16 {
            assert!(latest.replace(x, x + 1));
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let key = InputEvent::Key {
            scancode: crate::input::Scancode::plain(0x1e),
            down: true,
        };
        tx.send(key).unwrap();
        drop(tx);

        let mut emitted = Vec::new();
        while let Ok(Some(event)) = next_native_input(&rx, &latest) {
            emitted.push(event);
        }
        assert_eq!(
            emitted,
            [
                key,
                InputEvent::MouseMove {
                    x: 9_999,
                    y: 10_000
                }
            ]
        );
    }

    #[test]
    fn a_mode_change_is_terminal_for_both_paths() {
        let (mut sink, _store) = sink_with_store((4, 2));
        sink.on_au(&[7], Some(10)).unwrap();
        let err = sink
            .apply_update(one_rect_update((8, 4), 11, 200))
            .unwrap_err();
        assert!(err.contains("display mode changed"), "got: {err}");
        // AU path: a decoder suddenly producing a different size.
        sink.decoder = Some(Box::new(FakeDecoder { size: (8, 4) }));
        let err = sink.on_au(&[9], Some(11)).unwrap_err();
        assert!(err.contains("display mode changed"), "got: {err}");
    }

    #[test]
    fn input_events_translate_to_the_wire_records_the_server_decodes() {
        let mut seq = 1u32;
        // An extended scancode (right arrow, 0xE0 0x4D).
        let recs = wire_records(
            &InputEvent::Key {
                scancode: crate::input::Scancode {
                    code: 0x4D,
                    extended: true,
                },
                down: true,
            },
            &mut seq,
        );
        let (bytes, len) = encode_record(recs[0]);
        match decode_record(&bytes[..len]).unwrap() {
            Record::ScanDown { scancode, seq: s } => {
                assert_eq!(scancode, 0xE04D);
                assert_eq!(s, 1);
            }
            other => panic!("expected ScanDown, got {other:?}"),
        }
        // A wheel notch and a right-click, with distinct fields.
        let recs = wire_records(
            &InputEvent::Scroll {
                axis: ScrollAxis::Horizontal,
                units: -120,
                x: 5,
                y: 6,
            },
            &mut seq,
        );
        assert_eq!(recs[0], Record::MouseMove { x: 5, y: 6, seq: 2 });
        let (bytes, len) = encode_record(recs[1]);
        match decode_record(&bytes[..len]).unwrap() {
            Record::Wheel {
                axis,
                delta120,
                seq: s,
            } => {
                assert_eq!(axis, WheelAxis::Horizontal);
                assert_eq!(delta120, -120);
                assert_eq!(s, 3);
            }
            other => panic!("expected Wheel, got {other:?}"),
        }
        let recs = wire_records(
            &InputEvent::MouseButton {
                button: MouseButton::Right,
                down: false,
                x: 9,
                y: 9,
            },
            &mut seq,
        );
        assert_eq!(recs[0], Record::MouseMove { x: 9, y: 9, seq: 4 });
        let (bytes, len) = encode_record(recs[1]);
        match decode_record(&bytes[..len]).unwrap() {
            Record::MouseButton {
                button,
                down,
                seq: s,
            } => {
                assert_eq!(button, WireButton::Right);
                assert!(!down);
                assert_eq!(s, 5);
            }
            other => panic!("expected MouseButton, got {other:?}"),
        }
    }
}
