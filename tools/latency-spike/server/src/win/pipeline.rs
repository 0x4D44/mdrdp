//! Wiring: three threads, one bounded queue, one stats writer.
//!
//! * **Main thread** — capture, convert, encode. It owns every D3D11 and Media
//!   Foundation object, which is why it is the main thread rather than a spawned
//!   one: COM apartment state and the MF event queue belong to whichever thread
//!   created them, and keeping that thread the one the process started on removes a
//!   whole class of "works until it doesn't" threading bugs.
//! * **Sender thread** — the video socket and the stats file ([`super::send`]).
//! * **Input thread** — the keystroke channel ([`super::input`]).
//!
//! The queue between capture and sender is bounded and **lossy on purpose**. When
//! the socket cannot keep up, the newest frame is dropped rather than queued: a
//! queued frame is a stale frame, and the entire point of this spike is the latency
//! number. Every drop is counted and published as `dropped_frames`, so the loss is
//! never silent.

use super::{convert, dxgi, encode, input, qpc, send, Result};
use crate::annexb::{self, ParameterSets};
use crate::cli::{Config, DECLARED_FPS};
use crate::rects;
use crate::stats::{self, FrameRecord, Header, QpcClock};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// Encoded frames to wait after a connect-edge keyframe request before asking
/// again — one second at the declared rate.
const KEYFRAME_RETRY_FRAMES: u32 = DECLARED_FPS;

/// Frames in flight between the encoder and the socket. Two is enough to overlap a
/// write with the next encode and small enough that a stall shows up as a drop
/// rather than as growing latency.
const QUEUE_DEPTH: usize = 2;

/// `AcquireNextFrame` timeout. Short enough that a `Recreated` duplication or a
/// newly connected client is noticed promptly, long enough not to spin.
const ACQUIRE_TIMEOUT_MS: u32 = 8;

/// 100-nanosecond units per second — Media Foundation's time base.
const HNS_PER_SECOND: i64 = 10_000_000;

/// Most dirty rects a frame may carry and still take the raw fast path.
///
/// **Provisional until the §5a telemetry run.** The constant is a tuning knob, not
/// a promise: it is reported in the stats header so every archived measurement
/// names the thresholds it ran under.
const RECT_MAX_COUNT: usize = 32;

/// Most raw pixel bytes a frame may carry and still take the fast path — ≈2.6 ms of
/// wire time on this LAN, against typing-class updates of 1–10 KB. Beyond it the
/// raw copy costs more than the encode it is avoiding.
///
/// **Provisional until the §5a telemetry run**, on the same terms as
/// [`RECT_MAX_COUNT`].
const RECT_MAX_BYTES: u64 = 96 * 1024;

/// `--list-outputs`.
pub fn list_outputs() -> Result<()> {
    let outputs = dxgi::enumerate()?;
    if outputs.is_empty() {
        println!("no DXGI outputs found");
        return Ok(());
    }
    println!("idx  adapter/output              resolution  attached  rotation  device");
    for o in &outputs {
        println!(
            "{:>3}  a{}/o{} {:<22} {:>4}x{:<4}  {:<8}  {:<8}  {}",
            o.index,
            o.adapter_index,
            o.output_index,
            truncate(&o.adapter, 22),
            o.width,
            o.height,
            o.attached,
            o.rotation,
            o.device_name,
        );
    }
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
}

/// Run the server. Never returns in normal operation.
pub fn run(cfg: &Config) -> Result<()> {
    // MTA: the capture thread makes blocking calls and must not be pumping a
    // message loop, which is what an STA would require.
    // SAFETY: one CoInitializeEx per thread; the process runs until killed, so the
    // matching CoUninitialize would only ever run at exit.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };

    let clock = QpcClock::new(qpc::frequency())
        .ok_or("QueryPerformanceFrequency reported a non-positive frequency")?;

    let outputs = dxgi::enumerate()?;
    let info = outputs.get(cfg.output).ok_or_else(|| {
        format!(
            "--output {} is out of range: {} output(s) present. Run --list-outputs.",
            cfg.output,
            outputs.len()
        )
    })?;
    let mut capture = dxgi::Capture::open(info)?;
    eprintln!(
        "capture: output {} {}x{} on {}",
        cfg.output, capture.width, capture.height, capture.adapter
    );

    let mut converter = convert::Nv12Converter::new(
        &capture.device,
        &capture.context,
        capture.width,
        capture.height,
        DECLARED_FPS,
    )?;

    // MF must be started on the thread that drives the transform, and torn down
    // after it — `_mf` outlives `encoder` because it is declared first.
    let _mf = encode::Session::start()?;
    let manager = encode::device_manager(&capture.device)?;
    let mut encoder = encode::create(
        capture.width,
        capture.height,
        DECLARED_FPS,
        cfg.bitrate_kbps,
        cfg.gop,
        Some(&manager),
    )?;
    eprintln!("encode: {} ({})", encoder.name(), encoder.kind());
    if !encoder.codec_api_refused().is_empty() {
        eprintln!(
            "encode: MFT refused {} — see the stats header",
            encoder.codec_api_refused().join(", ")
        );
    }

    // The wire's rect coordinates are u16, so a desktop wider or taller than that
    // cannot be addressed by the fast path at all. Decided once here rather than
    // re-tested per frame, and announced when it silently costs the operator the
    // path they asked for.
    let rects_enabled =
        cfg.rects && capture.width <= u16::MAX as u32 && capture.height <= u16::MAX as u32;
    if cfg.rects && !rects_enabled {
        eprintln!(
            "capture: {}x{} exceeds the u16 rect coordinates on the wire; \
             the raw dirty-rect fast path is disabled for this output",
            capture.width, capture.height
        );
    }

    let header = build_header(cfg, clock, &capture, encoder.as_ref(), rects_enabled);
    let header_line = stats::to_line(&header);

    let connected = Arc::new(AtomicBool::new(false));
    let (tx, rx) = sync_channel::<send::Outbound>(QUEUE_DEPTH);
    let sender = send::Sender::new(
        cfg.video_port,
        cfg.out.as_deref(),
        header_line,
        clock,
        Arc::clone(&connected),
    )?;
    std::thread::Builder::new()
        .name("spike-send".into())
        .spawn(move || sender.run(rx))?;

    let input_tx = tx.clone();
    let input_port = cfg.input_port;
    std::thread::Builder::new()
        .name("spike-input".into())
        .spawn(move || {
            let (line_tx, line_rx) = sync_channel::<String>(64);
            // A small shim thread turns the input thread's string channel into
            // Outbound values, so `input::serve` needs to know nothing about the
            // sender's message type.
            std::thread::Builder::new()
                .name("spike-input-stats".into())
                .spawn(move || {
                    while let Ok(line) = line_rx.recv() {
                        if input_tx.send(send::Outbound::Line(line)).is_err() {
                            break;
                        }
                    }
                })
                .ok();
            if let Err(e) = input::serve(input_port, clock, line_tx) {
                eprintln!("input: listener stopped: {e}");
            }
        })?;

    let outcome = capture_loop(capture_state(
        &mut capture,
        &mut converter,
        encoder.as_mut(),
        clock,
        tx,
        connected,
        rects_enabled,
    ));
    // Only reached when the loop fails; the happy path never returns. Draining the
    // MFT before `MFShutdown` runs (via `_mf`'s Drop) keeps the driver's own logs
    // readable for whoever diagnoses that failure.
    encoder.shutdown();
    outcome
}

/// Everything the capture loop needs, bundled so the signature stays readable.
struct CaptureState<'a> {
    capture: &'a mut dxgi::Capture,
    converter: &'a mut convert::Nv12Converter,
    encoder: &'a mut dyn encode::Encoder,
    clock: QpcClock,
    tx: SyncSender<send::Outbound>,
    connected: Arc<AtomicBool>,
    /// Whether the raw dirty-rect fast path may run at all: `--no-rects` and a
    /// desktop too large for the wire's u16 coordinates both switch it off.
    rects_enabled: bool,
}

fn capture_state<'a>(
    capture: &'a mut dxgi::Capture,
    converter: &'a mut convert::Nv12Converter,
    encoder: &'a mut dyn encode::Encoder,
    clock: QpcClock,
    tx: SyncSender<send::Outbound>,
    connected: Arc<AtomicBool>,
    rects_enabled: bool,
) -> CaptureState<'a> {
    CaptureState {
        capture,
        converter,
        encoder,
        clock,
        tx,
        connected,
        rects_enabled,
    }
}

fn build_header(
    cfg: &Config,
    clock: QpcClock,
    capture: &dxgi::Capture,
    encoder: &dyn encode::Encoder,
    rects_enabled: bool,
) -> Header {
    let mut h = Header::new();
    h.qpc_frequency = clock.freq();
    h.video_port = cfg.video_port;
    h.input_port = cfg.input_port;
    h.bitrate_kbps = cfg.bitrate_kbps;
    h.gop = cfg.gop;
    h.fps = DECLARED_FPS;
    h.output_index = cfg.output;
    h.adapter = capture.adapter.clone();
    h.output = capture.device_name.clone();
    h.width = capture.width;
    h.height = capture.height;
    h.encoder = encoder.name().to_owned();
    h.encoder_kind = encoder.kind();
    h.codec_api_applied = encoder.codec_api_applied().to_vec();
    h.codec_api_refused = encoder.codec_api_refused().to_vec();
    // 0/0 when the path is off, so a reader never mistakes a control-arm capture
    // for one whose predicate simply never fired.
    let (max_count, max_bytes) = if rects_enabled {
        (RECT_MAX_COUNT as u32, RECT_MAX_BYTES)
    } else {
        (0, 0)
    };
    h.rect_max_count = max_count;
    h.rect_max_bytes = max_bytes;
    h.sequence_header_available = encoder.parameter_sets().is_some();
    h
}

/// Everything [`emit_au`] mutates across access units. Split from [`CaptureState`]
/// so the emit closure can borrow it while the encoder (which lives in
/// `CaptureState`) is itself mutably borrowed by `encode`/`pump`.
struct EmitCtx {
    clock: QpcClock,
    tx: SyncSender<send::Outbound>,
    /// Cumulative frames dropped because the send queue was full.
    dropped: u64,
    /// Cumulative rect messages dropped for the same reason. Counted separately so
    /// fast-path pressure is visible on its own.
    dropped_rects: u64,
    /// SPS/PPS scanned out of the stream's own access units — the fallback when
    /// the encoder publishes no out-of-band sequence header (Quick Sync). Updated
    /// on every in-band sighting; cleared when the encoder's config epoch moves.
    stream_sets: Option<ParameterSets>,
    /// The encoder's own out-of-band sets, snapshotted before each encode call
    /// (the encoder is unborrowable from inside the sink).
    encoder_sets: Option<ParameterSets>,
    /// `Some(n)` while a keyframe request is outstanding: n non-keyframe AUs seen
    /// since. Drives the §4 "verify the connect-edge IDR" retry.
    awaiting_keyframe: Option<u32>,
    /// Set inside the sink; acted on by the loop (which owns the encoder).
    rerequest_keyframe: bool,
    /// A frame was dropped from the send queue; the decoder is desynced until the
    /// next IDR, so ask for one.
    drop_wants_keyframe: bool,
}

/// Hand one encoded access unit to the sender, with its own frame's stamps.
fn emit_au(au: encode::EncodedAu, ctx: &mut EmitCtx) -> Result<()> {
    // The in-band SPS/PPS cache: update on *every* sighting (a first-sight-only
    // cache would go stale across an encoder reconfigure), consume via
    // `ensure_parameter_sets` below.
    if ctx.encoder_sets.is_none() && annexb::has_parameter_sets(&au.data) {
        if let Some(sets) = ParameterSets::from_sequence_header(&au.data) {
            ctx.stream_sets = Some(sets);
        }
    }
    let sets = ctx.encoder_sets.as_ref().or(ctx.stream_sets.as_ref());
    let (bytes, prepended) = annexb::ensure_parameter_sets(&au.data, sets, au.keyframe);

    let mut record = FrameRecord::new();
    record.frame = au.meta.seq;
    record.present_qpc_us = ctx.clock.micros(au.meta.present_qpc);
    record.acquire_qpc_us = ctx.clock.micros(au.meta.acquire_qpc);
    record.convert_start_us = ctx.clock.micros(au.meta.convert_start_qpc);
    record.convert_end_us = ctx.clock.micros(au.meta.convert_end_qpc);
    record.encode_submit_us = ctx.clock.micros(au.submit_qpc);
    record.encode_out_us = ctx.clock.micros(au.out_qpc);
    record.au_bytes = bytes.len();
    record.keyframe = au.keyframe;
    record.param_sets_prepended = prepended;
    record.dropped_frames = ctx.dropped;
    record.dropped_rects = ctx.dropped_rects;
    record.stamp_mismatches = au.stamp_mismatches;
    if au.meta.change_valid {
        record.dirty_rect_count = Some(au.meta.dirty_rect_count);
        record.dirty_bytes = Some(au.meta.dirty_bytes);
        record.move_rect_count = Some(au.meta.move_rect_count);
    }

    match ctx.awaiting_keyframe {
        Some(n) if au.keyframe => {
            record.keyframe_wait_frames = Some(n);
            ctx.awaiting_keyframe = None;
        }
        Some(n) if n + 1 >= KEYFRAME_RETRY_FRAMES => {
            // A second's worth of frames and no IDR: the request was ignored.
            ctx.rerequest_keyframe = true;
            ctx.awaiting_keyframe = Some(0);
        }
        Some(n) => ctx.awaiting_keyframe = Some(n + 1),
        None => {}
    }

    match ctx.tx.try_send(send::Outbound::Frame(
        Box::new(record),
        au.meta.seq,
        bytes.into_owned(),
    )) {
        Ok(()) => Ok(()),
        // Full means the socket is behind. Drop the newest rather than queue it: a
        // late frame is worse than a missing one here. But a dropped AU desyncs
        // the viewer's decoder until the next IDR, so request one.
        Err(TrySendError::Full(_)) => {
            ctx.dropped += 1;
            ctx.drop_wants_keyframe = true;
            Ok(())
        }
        Err(TrySendError::Disconnected(_)) => Err("sender thread has gone away".to_owned().into()),
    }
}

/// Does this frame's change metadata put it on the raw fast path?
///
/// Called only with `Some(change)`: metadata that is **absent** must never satisfy
/// this predicate as `rect_count = 0`, because "we do not know what changed" is not
/// "nothing changed" — treating the two alike would ship a stale canvas as a fresh
/// one. `dxgi::acquire` already keeps the two states distinct; this function only
/// has to not undo that.
fn takes_fast_path(change: &dxgi::ChangeInfo) -> bool {
    !change.rects.is_empty()
        && change.rects.len() <= RECT_MAX_COUNT
        && change.dirty_bytes() <= RECT_MAX_BYTES
}

/// Read one frame's dirty rects back raw and hand them to the sender, ahead of the
/// access unit for the same frame.
///
/// This runs while the duplication frame is still held and *before* the converter
/// touches it: not waiting for convert+encode is the entire latency win.
fn emit_rects(
    capture: &mut dxgi::Capture,
    texture: &ID3D11Texture2D,
    change: &dxgi::ChangeInfo,
    frame_seq: u64,
    ctx: &mut EmitCtx,
) -> Result<()> {
    let pack_start = qpc::now();
    let rect_pixels = capture.read_rects(texture, change)?;
    let rect_count = rect_pixels.len() as u32;
    let rect_bytes: u64 = rect_pixels.iter().map(|r| r.pixels.len() as u64).sum();
    let update = rects::RectUpdate {
        frame_seq,
        frame_width: capture.width,
        frame_height: capture.height,
        rects: rect_pixels,
    };
    let mut payload = Vec::with_capacity(rects::encoded_len(&update));
    rects::encode(&update, &mut payload);
    let pack_end = qpc::now();

    let mut record = stats::RectRecord::new();
    record.frame = frame_seq;
    record.rect_count = rect_count;
    record.rect_bytes = rect_bytes;
    record.pack_start_us = ctx.clock.micros(pack_start);
    record.pack_end_us = ctx.clock.micros(pack_end);
    record.dropped_rects = ctx.dropped_rects;

    match ctx
        .tx
        .try_send(send::Outbound::Rects(Box::new(record), payload))
    {
        Ok(()) => Ok(()),
        // Full means the socket is behind. Unlike a dropped access unit this costs
        // no correctness and needs no keyframe: the same frame's AU is still on its
        // way down the ordinary path and repaints exactly this content.
        Err(TrySendError::Full(_)) => {
            ctx.dropped_rects += 1;
            Ok(())
        }
        Err(TrySendError::Disconnected(_)) => Err("sender thread has gone away".to_owned().into()),
    }
}

fn capture_loop(state: CaptureState<'_>) -> Result<()> {
    let mut frame_seq: u64 = 0;
    let mut was_connected = false;
    let mut want_keyframe = true;
    // HLD §5: the first frame after a rebuilt duplication is forced down the
    // full-frame path. Its dirty metadata describes change since the *new*
    // duplication's baseline, and whatever changed between the last delivered
    // frame and that baseline is described by nothing — a small rect here would
    // lie by omission. The AU has no such gap: the encoder references the last
    // frame it actually encoded, so the difference it ships is complete.
    let mut suppress_rects_once = false;
    let mut last_epoch = state.encoder.config_epoch();
    let frame_duration_hns = HNS_PER_SECOND / DECLARED_FPS.max(1) as i64;
    let start_qpc = qpc::now();
    let mut ctx = EmitCtx {
        clock: state.clock,
        tx: state.tx.clone(),
        dropped: 0,
        dropped_rects: 0,
        stream_sets: None,
        encoder_sets: None,
        awaiting_keyframe: None,
        rerequest_keyframe: false,
        drop_wants_keyframe: false,
    };

    loop {
        let connected = state.connected.load(Ordering::Acquire);
        if !connected {
            // Nothing is watching. Capturing anyway would hold the compositor and
            // run the GPU encoder for an audience of nobody.
            was_connected = false;
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }
        if !was_connected {
            // A new viewer starts mid-GOP and cannot decode a P-frame.
            was_connected = true;
            want_keyframe = true;
        }
        if want_keyframe {
            state.encoder.request_keyframe();
            ctx.awaiting_keyframe = Some(0);
            want_keyframe = false;
        }

        let acquired = state.capture.acquire(ACQUIRE_TIMEOUT_MS)?;
        let (texture, present_qpc, acquire_qpc, change) = match acquired {
            dxgi::Acquired::Frame {
                texture,
                present_qpc,
                acquire_qpc,
                change,
            } => (texture, present_qpc, acquire_qpc, change),
            dxgi::Acquired::Timeout => {
                // No new frame, but the async MFT may be holding a finished AU it
                // only delivers when pumped — on a static desktop that AU would
                // otherwise never leave the transform.
                state.encoder.pump(&mut |au| emit_au(au, &mut ctx))?;
                housekeep(&state, &mut ctx, &mut want_keyframe, &mut last_epoch);
                continue;
            }
            dxgi::Acquired::PointerOnly => continue,
            dxgi::Acquired::Recreated => {
                eprintln!("capture: duplication lost and rebuilt (desktop switch)");
                want_keyframe = true;
                suppress_rects_once = true;
                continue;
            }
        };

        frame_seq += 1;

        // The fast path is an overlay, not a branch: whatever happens here, the
        // frame still goes on to convert, encode and send as H.264 below.
        if state.rects_enabled && !suppress_rects_once {
            if let Some(change) = change.as_ref().filter(|c| takes_fast_path(c)) {
                emit_rects(state.capture, &texture, change, frame_seq, &mut ctx)?;
            }
        }
        suppress_rects_once = false;

        let convert_start = qpc::now();
        let nv12 = state.converter.convert(&texture)?;
        let convert_end = qpc::now();

        let mut meta = encode::FrameMeta {
            seq: frame_seq,
            present_qpc,
            acquire_qpc,
            convert_start_qpc: convert_start,
            convert_end_qpc: convert_end,
            ..Default::default()
        };
        if let Some(change) = &change {
            meta.change_valid = true;
            meta.dirty_rect_count = change.rects.len() as u32;
            meta.dirty_bytes = change.dirty_bytes();
            meta.move_rect_count = change.move_rects;
        }

        let time_hns = state
            .clock
            .micros(acquire_qpc.saturating_sub(start_qpc))
            .saturating_mul(10);
        let sample = encode::sample_from_texture(&nv12, time_hns, frame_duration_hns)?;

        ctx.encoder_sets = state.encoder.parameter_sets().cloned();
        state
            .encoder
            .encode(&sample, meta, &mut |au| emit_au(au, &mut ctx))?;
        housekeep(&state, &mut ctx, &mut want_keyframe, &mut last_epoch);
    }
}

/// Post-emission actions that need the encoder, which the sink cannot borrow.
fn housekeep(
    state: &CaptureState<'_>,
    ctx: &mut EmitCtx,
    want_keyframe: &mut bool,
    last_epoch: &mut u64,
) {
    let epoch = state.encoder.config_epoch();
    if epoch != *last_epoch {
        // The encoder renegotiated its output type; sets scanned from the old
        // stream would poison the new one.
        *last_epoch = epoch;
        ctx.stream_sets = None;
    }
    if ctx.rerequest_keyframe {
        ctx.rerequest_keyframe = false;
        eprintln!(
            "encode: keyframe request ignored for {KEYFRAME_RETRY_FRAMES} frames; asking again"
        );
        *want_keyframe = true;
    }
    if ctx.drop_wants_keyframe {
        ctx.drop_wants_keyframe = false;
        *want_keyframe = true;
    }
}
