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

use super::source::{Acquired, ChangeInfo, DirtyRect, FrameSource};
use super::{convert, dxgi, encode, idd_source, input, pixel_diff, qpc, send, Result};
use crate::annexb::{self, ParameterSets};
use crate::cli::{Config, Source, DECLARED_FPS};
use crate::diff;
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

/// How long one [`FrameSource::acquire`] waits for a frame. Short enough that a
/// rebuilt source or a newly connected client is noticed promptly, long enough not
/// to spin.
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

/// How long a gap since the previous consumed frame arms the Increment 3 pixel
/// diff (HLD §6b). An isolated keystroke always clears it; video and burst typing
/// never do, so continuous content pays the diff's cost exactly zero times.
///
/// **Provisional until the §6b telemetry run**, on the same terms as
/// [`RECT_MAX_COUNT`]: a tuning knob, reported in the stats header so every
/// archived measurement names the threshold it ran under.
const DIFF_IDLE_GAP_US: i64 = 100_000;

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

    // Both sources own their own D3D11 device — the IDD consumer's is built on the
    // adapter LUID its driver publishes, not on one we choose — so everything
    // downstream is built from whichever device the frames actually live on.
    let mut source: Box<dyn FrameSource> = match cfg.source {
        Source::Dxgi => {
            let outputs = dxgi::enumerate()?;
            let info = outputs.get(cfg.output).ok_or_else(|| {
                format!(
                    "--output {} is out of range: {} output(s) present. Run --list-outputs.",
                    cfg.output,
                    outputs.len()
                )
            })?;
            Box::new(dxgi::Capture::open(info)?)
        }
        Source::Idd => Box::new(idd_source::IddSource::open()?),
    };
    eprintln!(
        "capture: source {} — {} {}x{} on {}",
        source.kind(),
        source.output_name(),
        source.width(),
        source.height(),
        source.adapter()
    );
    // The capture display's own offset into the virtual desktop, for the input
    // thread's mouse-move mapping (§5.2). Read once here rather than by the input
    // thread itself: it belongs to whichever `FrameSource` is live, and the input
    // thread never touches the source.
    let capture_origin = source.origin();

    let mut converter = convert::Nv12Converter::new(
        source.device(),
        source.context(),
        source.width(),
        source.height(),
        DECLARED_FPS,
    )?;

    // MF must be started on the thread that drives the transform, and torn down
    // after it — `_mf` outlives `encoder` because it is declared first.
    let _mf = encode::Session::start()?;
    let manager = encode::device_manager(source.device())?;
    let mut encoder = encode::create(
        source.width(),
        source.height(),
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
        cfg.rects && source.width() <= u16::MAX as u32 && source.height() <= u16::MAX as u32;
    if cfg.rects && !rects_enabled {
        eprintln!(
            "capture: {}x{} exceeds the u16 rect coordinates on the wire; \
             the raw dirty-rect fast path is disabled for this output",
            source.width(),
            source.height()
        );
    }

    // The diff rides the rect wire path and produces nothing else, so a run without
    // rects has nothing for it to emit through. Deciding it here keeps the loop from
    // re-testing a pair of flags per frame, and keeps the header honest.
    let diff_enabled = cfg.diff && rects_enabled;

    let header = build_header(
        cfg,
        clock,
        source.as_ref(),
        encoder.as_ref(),
        rects_enabled,
        diff_enabled,
    );
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
            if let Err(e) = input::serve(input_port, clock, line_tx, capture_origin) {
                eprintln!("input: listener stopped: {e}");
            }
        })?;

    let outcome = capture_loop(capture_state(
        source.as_mut(),
        &mut converter,
        encoder.as_mut(),
        clock,
        tx,
        connected,
        rects_enabled,
        diff_enabled,
    ));
    // Only reached when the loop fails; the happy path never returns. Draining the
    // MFT before `MFShutdown` runs (via `_mf`'s Drop) keeps the driver's own logs
    // readable for whoever diagnoses that failure.
    encoder.shutdown();
    outcome
}

/// Everything the capture loop needs, bundled so the signature stays readable.
struct CaptureState<'a> {
    capture: &'a mut dyn FrameSource,
    converter: &'a mut convert::Nv12Converter,
    encoder: &'a mut dyn encode::Encoder,
    clock: QpcClock,
    tx: SyncSender<send::Outbound>,
    connected: Arc<AtomicBool>,
    /// Whether the raw dirty-rect fast path may run at all: `--no-rects` and a
    /// desktop too large for the wire's u16 coordinates both switch it off.
    rects_enabled: bool,
    /// Whether the Increment 3 pixel diff may run at all. Implies `rects_enabled`:
    /// the diff emits through the rect path or not at all.
    diff_enabled: bool,
}

#[allow(clippy::too_many_arguments)]
fn capture_state<'a>(
    capture: &'a mut dyn FrameSource,
    converter: &'a mut convert::Nv12Converter,
    encoder: &'a mut dyn encode::Encoder,
    clock: QpcClock,
    tx: SyncSender<send::Outbound>,
    connected: Arc<AtomicBool>,
    rects_enabled: bool,
    diff_enabled: bool,
) -> CaptureState<'a> {
    CaptureState {
        capture,
        converter,
        encoder,
        clock,
        tx,
        connected,
        rects_enabled,
        diff_enabled,
    }
}

fn build_header(
    cfg: &Config,
    clock: QpcClock,
    capture: &dyn FrameSource,
    encoder: &dyn encode::Encoder,
    rects_enabled: bool,
    diff_enabled: bool,
) -> Header {
    let mut h = Header::new();
    h.qpc_frequency = clock.freq();
    h.video_port = cfg.video_port;
    h.input_port = cfg.input_port;
    h.bitrate_kbps = cfg.bitrate_kbps;
    h.gop = cfg.gop;
    h.fps = DECLARED_FPS;
    // The source's own answer, not the flag's: the two cannot disagree, and the
    // one that produced the frames is the one an archived run needs.
    h.source = capture.kind();
    h.output_index = cfg.output;
    h.adapter = capture.adapter().to_owned();
    h.output = capture.output_name().to_owned();
    h.width = capture.width();
    h.height = capture.height();
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
    // 0 on the same terms: a reader must be able to tell the diff's control arm
    // from a run whose idle gate simply never opened.
    h.diff_idle_gap_ms = if diff_enabled {
        (DIFF_IDLE_GAP_US / 1000) as u32
    } else {
        0
    };
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
    /// Cumulative idle-regime pixel diffs attempted.
    diff_runs: u64,
    /// Of those, how many produced a usable measured delta.
    diff_hits: u64,
    /// Cumulative microseconds spent inside those diffs — the cost side of the
    /// trade the hit count is the benefit side of.
    diff_us_total: u64,
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
    record.diff_runs = ctx.diff_runs;
    record.diff_hits = ctx.diff_hits;
    record.diff_us_total = ctx.diff_us_total;
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
/// one. Both sources already keep the two states distinct (duplication by having no
/// metadata buffer, the IDD consumer by the coverage invariant in
/// [`crate::idd_section::coverage_for`]); this function only has to not undo that.
fn takes_fast_path(change: &ChangeInfo) -> bool {
    !change.rects.is_empty()
        && change.rects.len() <= RECT_MAX_COUNT
        && change.dirty_bytes() <= RECT_MAX_BYTES
}

/// The bounding box of a change claim, clipped to the desktop — where the
/// Increment 3 diff scans when there is a claim to verify.
///
/// The claim may be wildly inflated (that is the whole reason the diff exists), but
/// it is never *short*: both sources publish coverage, so every changed pixel lies
/// inside some listed rect. Scanning the box that contains them all therefore cannot
/// miss real change, and on the isolated-keystroke frames it is far smaller than the
/// full frame the absent-metadata case has to scan.
///
/// An empty claim yields a zero-sized region; the caller never passes one (a `Some`
/// with no rects is a claim of no change, which has nothing to verify).
fn claimed_bounds(rects: &[DirtyRect], width: u32, height: u32) -> diff::Region {
    let mut left = u32::MAX;
    let mut top = u32::MAX;
    let mut right = 0u32;
    let mut bottom = 0u32;
    for r in rects {
        left = left.min(r.x);
        top = top.min(r.y);
        right = right.max(r.x.saturating_add(r.w));
        bottom = bottom.max(r.y.saturating_add(r.h));
    }
    let left = left.min(width);
    let top = top.min(height);
    let right = right.min(width);
    let bottom = bottom.min(height);
    diff::Region {
        x: left,
        y: top,
        w: right.saturating_sub(left),
        h: bottom.saturating_sub(top),
    }
}

/// Read one frame's dirty rects back raw and hand them to the sender, ahead of the
/// access unit for the same frame.
///
/// This runs inside the source's frame-validity window and *before* the converter
/// touches it: not waiting for convert+encode is the entire latency win.
fn emit_rects(
    capture: &mut dyn FrameSource,
    texture: &ID3D11Texture2D,
    change: &ChangeInfo,
    frame_seq: u64,
    ctx: &mut EmitCtx,
) -> Result<()> {
    let pack_start = qpc::now();
    let rect_pixels = capture.read_rects(texture, change)?;
    send_rects(
        rect_pixels,
        frame_seq,
        capture.width(),
        capture.height(),
        pack_start,
        false,
        ctx,
    )
}

/// Encode one already-packed rect set and hand it to the sender.
///
/// The tail both fast-path arms share: the metadata one above, which has just read
/// the claimed rects back, and the Increment 3 diff, which packed the *measured*
/// delta straight out of its own mapping. Only `from_diff` tells them apart on the
/// wire — the trust argument differs (HLD §6b) but the payload and the send policy
/// do not, and two copies of that policy would be two places for the drop
/// accounting to diverge.
///
/// `pack_start` is the stamp taken before whichever work produced `rect_pixels`, so
/// the row's `pack_start_us .. pack_end_us` span covers the diff when there was one.
fn send_rects(
    rect_pixels: Vec<rects::Rect>,
    frame_seq: u64,
    frame_width: u32,
    frame_height: u32,
    pack_start: i64,
    from_diff: bool,
    ctx: &mut EmitCtx,
) -> Result<()> {
    let rect_count = rect_pixels.len() as u32;
    let rect_bytes: u64 = rect_pixels.iter().map(|r| r.pixels.len() as u64).sum();
    let update = rects::RectUpdate {
        frame_seq,
        frame_width,
        frame_height,
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
    record.from_diff = from_diff;

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
    // (`Recreated` is a rebuilt duplication under `--source dxgi`, and a rebuilt
    // shared pool under `--source idd`; the reasoning is identical.)
    let mut suppress_rects_once = false;
    let mut last_epoch = state.encoder.config_epoch();
    // HLD §6b: the previous consumed frame, retained on the GPU, plus the stamp that
    // says how long ago it was. Both are the diff's whole state, and both reset on a
    // rebuilt source.
    let mut pixel_diff = pixel_diff::PixelDiff::new(state.capture.width(), state.capture.height());
    let mut last_acquire_qpc: Option<i64> = None;
    let frame_duration_hns = HNS_PER_SECOND / DECLARED_FPS.max(1) as i64;
    let start_qpc = qpc::now();
    let mut ctx = EmitCtx {
        clock: state.clock,
        tx: state.tx.clone(),
        dropped: 0,
        dropped_rects: 0,
        diff_runs: 0,
        diff_hits: 0,
        diff_us_total: 0,
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
            Acquired::Frame {
                texture,
                present_qpc,
                acquire_qpc,
                change,
            } => (texture, present_qpc, acquire_qpc, change),
            Acquired::Timeout => {
                // No new frame, but the async MFT may be holding a finished AU it
                // only delivers when pumped — on a static desktop that AU would
                // otherwise never leave the transform.
                state.encoder.pump(&mut |au| emit_au(au, &mut ctx))?;
                housekeep(&state, &mut ctx, &mut want_keyframe, &mut last_epoch);
                continue;
            }
            Acquired::PointerOnly => continue,
            Acquired::Recreated => {
                eprintln!("capture: frame source lost and rebuilt");
                want_keyframe = true;
                suppress_rects_once = true;
                // The retained frame predates the rebuild, so it is no longer the
                // baseline the viewer is painting on top of; a diff against it would
                // measure against something never on screen. Dropping the stamp too
                // keeps the idle gate from reading the rebuild's own outage as idle.
                pixel_diff.invalidate();
                last_acquire_qpc = None;
                continue;
            }
        };

        frame_seq += 1;

        // The fast path is an overlay, not a branch: whatever happens here, the
        // frame still goes on to convert, encode and send as H.264 below.
        let mut metadata_took_fast_path = false;
        if state.rects_enabled && !suppress_rects_once {
            if let Some(change) = change.as_ref().filter(|c| takes_fast_path(c)) {
                emit_rects(state.capture, &texture, change, frame_seq, &mut ctx)?;
                metadata_took_fast_path = true;
            }
        }

        // Increment 3 (HLD §6b): the metadata missed, so measure instead of
        // trusting. Five conditions, each earning its place:
        //
        // * `!suppress_rects_once` — a rebuilt source suppresses *both* arms for one
        //   frame, for the same decision-15 reason.
        // * `!metadata_took_fast_path` — the frame is already on the fast path;
        //   diffing it would cost 5–8 ms to re-derive an answer we have.
        // * `pixel_diff.valid()` — there is a baseline the viewer actually saw.
        // * an idle gap of at least [`DIFF_IDLE_GAP_US`] — the regime where the
        //   metadata is known to be wrong, and the one regime where the diff's cost
        //   is affordable. Continuous content never reaches it.
        // * the metadata did not honestly say "nothing changed" — a `Some` with no
        //   rects is a claim of no change, and there is nothing to verify. (Absent
        //   metadata is the opposite: it claims nothing, so the diff is the only
        //   thing that can tell us anything.)
        let idle_gap_passed = match last_acquire_qpc {
            Some(last) => ctx.clock.micros(acquire_qpc.saturating_sub(last)) >= DIFF_IDLE_GAP_US,
            None => false,
        };
        let metadata_claims_no_change = change.as_ref().is_some_and(|c| c.rects.is_empty());
        if state.diff_enabled
            && !suppress_rects_once
            && !metadata_took_fast_path
            && pixel_diff.valid()
            && idle_gap_passed
            && !metadata_claims_no_change
        {
            // Where to look. A claim, however inflated, still *contains* the true
            // delta: duplication's dirty rects are complete coverage, and the IDD
            // driver's buffer-relative damage is a superset of the desktop-relative
            // change. Absent metadata claims nothing, so the whole frame is in play.
            let region = match change.as_ref() {
                Some(c) => claimed_bounds(&c.rects, state.capture.width(), state.capture.height()),
                None => diff::Region {
                    x: 0,
                    y: 0,
                    w: state.capture.width(),
                    h: state.capture.height(),
                },
            };
            // The stamp the rect row reports as `pack_start_us`: the diff is part of
            // what this frame's fast path cost, not a prelude to it.
            let diff_start = qpc::now();
            let measured = {
                let device = state.capture.device();
                let context = state.capture.context();
                pixel_diff.diff(
                    device,
                    context,
                    &texture,
                    region,
                    RECT_MAX_COUNT,
                    RECT_MAX_BYTES,
                )?
            };
            let diff_us = ctx
                .clock
                .micros(qpc::now().saturating_sub(diff_start))
                .max(0) as u64;
            ctx.diff_runs += 1;
            ctx.diff_us_total += diff_us;
            // `None` is a miss — too many rects, or too many bytes — and needs no
            // action: the frame is on the codec path already, which is where a
            // large change belongs anyway.
            if let Some(rect_pixels) = measured {
                ctx.diff_hits += 1;
                if !rect_pixels.is_empty() {
                    send_rects(
                        rect_pixels,
                        frame_seq,
                        state.capture.width(),
                        state.capture.height(),
                        diff_start,
                        true,
                        &mut ctx,
                    )?;
                }
            }
        }
        suppress_rects_once = false;

        // Still inside the frame's validity window, and before the converter touches
        // it: this frame becomes the next one's baseline. Unconditional while the
        // diff is on — see [`pixel_diff`] for why a conditional copy would buy less
        // than it costs in ways to be wrong.
        if state.diff_enabled {
            let device = state.capture.device();
            let context = state.capture.context();
            pixel_diff.retain(device, context, &texture)?;
        }
        last_acquire_qpc = Some(acquire_qpc);

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
