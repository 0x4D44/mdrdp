//! Wiring: capture plus video, sparse, and input workers; each pixel lane has a
//! shallow bounded queue and the video worker owns the stats file.
//!
//! * **Main thread** — capture, convert, encode. It owns every D3D11 and Media
//!   Foundation object, which is why it is the main thread rather than a spawned
//!   one: COM apartment state and the MF event queue belong to whichever thread
//!   created them, and keeping that thread the one the process started on removes a
//!   whole class of "works until it doesn't" threading bugs.
//! * **Sender thread** — the video socket and the stats file ([`super::send`]).
//! * **Sparse sender thread** — raw final pixels on their own socket.
//! * **Input thread** — the keystroke channel ([`super::input`]).
//!
//! The queue between capture and sender is bounded and **lossy on purpose**. When
//! the socket cannot keep up, the newest frame is dropped rather than queued: a
//! queued frame is a stale frame, and the entire point of this spike is the latency
//! number. Every drop is counted and published as `dropped_frames`, so the loss is
//! never silent.

use super::source::{Acquired, ChangeInfo, DirtyRect, FrameSource};
use super::{convert, dxgi, encode, idd_source, input, pixel_diff, qpc, send, Result};
use crate::adaptive::{self, Region, UpdatePlan};
use crate::annexb::{self, AvcParameterSets, ParameterSets};
use crate::bootstrap::ViewerBootstrap;
use crate::channel_listeners::BoundChannels;
use crate::cli::{Config, Source, DECLARED_FPS};
use crate::diff;
use crate::logical_frame;
use crate::rects;
use crate::send_schedule::AdmissionGate;
use crate::stats::{self, FrameRecord, Header, QpcClock};
use crate::video_update::{self, VideoKind, VideoTile, VideoUpdate};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// Encoded frames to wait after a connect-edge keyframe request before asking
/// again — one second at the declared rate.
const KEYFRAME_RETRY_FRAMES: u32 = DECLARED_FPS;

/// Frames in flight between the encoder and the socket. Two is enough to overlap a
/// write with the next encode and small enough that a stall shows up as a drop
/// rather than as growing latency.
const QUEUE_DEPTH: usize = 2;
const SPARSE_QUEUE_DEPTH: usize = 2;

/// Async tile encoders can finish one entire converter budget apart. Matching that
/// fixed budget lets one tile drain before its peer without evicting a valid set;
/// no larger skew can be submitted because whole-frame admission stops as soon as
/// any tile has no free converter surface.
const MAX_PENDING_LOGICAL_FRAMES: usize = convert::POOL_SIZE;

/// How long one [`FrameSource::acquire`] waits for a frame. Short enough that a
/// rebuilt source or a newly connected client is noticed promptly, long enough not
/// to spin.
const ACQUIRE_TIMEOUT_MS: u32 = 8;

/// A low-latency encoder that retires no submission for this long is unhealthy.
/// Exit so the session agent rebuilds it instead of leaving the viewer frozen.
const SURFACE_STALL_TIMEOUT: Duration = Duration::from_millis(500);

/// 100-nanosecond units per second — Media Foundation's time base.
const HNS_PER_SECOND: i64 = 10_000_000;
const CODEC_BLOCK_SIZE: u32 = 16;

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
const MAX_INFERRED_MOVE_SHIFT: u32 = 512;

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

    // MF must be started on the thread that drives the transform, and torn down
    // after it — `_mf` outlives every tile encoder because it is declared first.
    let _mf = encode::Session::start()?;
    let manager = encode::device_manager(source.device())?;
    let (codec, tiles) = match build_tiles(encode::Codec::H264, cfg, source.as_ref(), &manager) {
        Ok(tiles) => (encode::Codec::H264, tiles),
        Err(h264_error) => {
            eprintln!(
                "encode: tiled H.264 unavailable; falling back explicitly to HEVC: {h264_error}"
            );
            let tiles = build_tiles(encode::Codec::Hevc, cfg, source.as_ref(), &manager)
                .map_err(|hevc_error| {
                    format!(
                        "no native encoder path: H.264 failed ({h264_error}); HEVC fallback failed ({hevc_error})"
                    )
                })?;
            (encode::Codec::Hevc, tiles)
        }
    };

    // The wire's rect coordinates are u16, so a desktop wider or taller than that
    // cannot be addressed by the fast path at all. Decided once here rather than
    // re-tested per frame, and announced when it silently costs the operator the
    // path they asked for.
    let rects_enabled = cfg.rects
        && codec == encode::Codec::H264
        && source.width() <= u16::MAX as u32
        && source.height() <= u16::MAX as u32;
    if cfg.rects && codec == encode::Codec::Hevc {
        eprintln!("capture: raw sparse updates require the H.264 precedence grid; HEVC fallback remains full-frame");
    }
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

    // Every capability in the video header must already exist. Binding both side
    // channels here makes startup fail before a viewer can receive a dead contract.
    let BoundChannels {
        input: input_listener,
        sparse: sparse_listener,
        aux: aux_listener,
    } = BoundChannels::bind(cfg.input_port, cfg.sparse_port, cfg.aux_port)?;
    let aux_enabled = aux_listener.is_some();

    let header = build_header(
        cfg,
        clock,
        source.as_ref(),
        codec,
        &tiles,
        rects_enabled,
        diff_enabled,
        aux_enabled,
    );
    let header_line = stats::to_line(&header);

    let connected = Arc::new(AtomicBool::new(false));
    let sparse_connected = Arc::new(AtomicBool::new(false));
    let session_epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let cursor_hidden = Arc::new(AtomicBool::new(source.hide_local_cursor()));
    let (tx, rx) = sync_channel::<send::Outbound>(QUEUE_DEPTH);
    let sender = send::Sender::new(
        cfg.video_port,
        cfg.out.as_deref(),
        header_line,
        clock,
        Arc::clone(&connected),
        Arc::clone(&sparse_connected),
        Arc::clone(&session_epoch),
        Arc::clone(&cursor_hidden),
    )?;
    std::thread::Builder::new()
        .name("spike-send".into())
        .spawn(move || sender.run(rx))?;

    let (sparse_tx, sparse_rx) = sync_channel::<send::SparseOutbound>(SPARSE_QUEUE_DEPTH);
    let sparse_sender = send::SparseSender::new(
        sparse_listener,
        Arc::clone(&sparse_connected),
        Arc::clone(&session_epoch),
        tx.clone(),
        clock,
    )?;
    std::thread::Builder::new()
        .name("spike-sparse".into())
        .spawn(move || sparse_sender.run(sparse_rx))?;

    let input_tx = tx.clone();
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
            if let Err(e) = input::serve_listener(input_listener, clock, line_tx, capture_origin) {
                eprintln!("input: listener stopped: {e}");
            }
        })?;

    // The auxiliary channel: clipboard now, audio in tranche 6. Its own thread
    // and its own listener, so neither the capture loop nor the input channel
    // can be delayed by it — the priority separation Arthur set out, made
    // structural rather than promised.
    if let Some(aux_listener) = aux_listener {
        let audio_kind = cfg.audio_source;
        std::thread::Builder::new()
            .name("spike-aux".into())
            .spawn(move || {
                let policy = crate::clipboard::Policy::default();
                if let Err(e) = crate::aux_server::serve_listener(
                    aux_listener,
                    || Box::new(super::clipboard::ClipboardOwner::new()),
                    std::sync::Arc::new(move || -> Box<dyn crate::audio_source::AudioSource> {
                        match audio_kind {
                            crate::cli::AudioKind::Off => {
                                Box::new(crate::audio_source::UnavailableSource)
                            }
                            crate::cli::AudioKind::Tone => {
                                Box::new(crate::audio_source::ToneSource::new(48_000, 2))
                            }
                            // Constructed on the audio thread, not here: the
                            // WASAPI client is COM-backed and must be built and
                            // used on one thread.
                            crate::cli::AudioKind::Loopback => {
                                Box::new(super::audio::LoopbackCapture::new())
                            }
                        }
                    }),
                    policy,
                ) {
                    // Never fatal: a session without a clipboard is a working
                    // session, and the capture loop must not care.
                    eprintln!("aux: listener stopped: {e}");
                }
            })?;
    }

    let outcome = capture_loop(CaptureState {
        capture: source.as_mut(),
        codec,
        tiles,
        clock,
        tx,
        connected,
        cursor_hidden,
        rects_enabled,
        diff_enabled,
        sparse_tx,
        sparse_connected,
    });
    // Only reached when the loop fails; the happy path never returns. Draining the
    // MFT before `MFShutdown` runs (via `_mf`'s Drop) keeps the driver's own logs
    // readable for whoever diagnoses that failure.
    outcome
}

struct TilePipeline {
    header: stats::TileHeader,
    codec_canvas: Option<convert::CodecCanvas>,
    converter: convert::Nv12Converter,
    encoder: Box<dyn encode::Encoder>,
}

fn build_tiles(
    codec: encode::Codec,
    cfg: &Config,
    source: &dyn FrameSource,
    manager: &windows::Win32::Media::MediaFoundation::IMFDXGIDeviceManager,
) -> Result<Vec<TilePipeline>> {
    let layout = match codec {
        encode::Codec::H264 => stats::tile_layout(source.width(), source.height()),
        encode::Codec::Hevc => stats::hevc_fallback_layout(source.width(), source.height()),
    };
    let tile_count = u32::try_from(layout.len()).unwrap_or(1);
    let per_tile_bitrate = cfg.bitrate_kbps.div_ceil(tile_count).max(1);
    let mut tiles = Vec::with_capacity(layout.len());
    for header in layout {
        let bounds = Region {
            x: header.x,
            y: header.y,
            width: header.width,
            height: header.height,
        };
        let (codec_canvas, converter) = match codec {
            encode::Codec::H264 => (
                Some(convert::CodecCanvas::new(
                    source.device(),
                    source.context(),
                    bounds,
                )?),
                convert::Nv12Converter::new(
                    source.device(),
                    source.context(),
                    header.width,
                    header.height,
                    DECLARED_FPS,
                )?,
            ),
            encode::Codec::Hevc => (
                None,
                convert::Nv12Converter::new_region(
                    source.device(),
                    source.context(),
                    source.width(),
                    source.height(),
                    header.x,
                    header.y,
                    header.width,
                    header.height,
                    DECLARED_FPS,
                )?,
            ),
        };
        let encoder = encode::create(
            codec,
            header.width,
            header.height,
            DECLARED_FPS,
            per_tile_bitrate,
            cfg.gop,
            Some(manager),
        )?;
        eprintln!(
            "encode: {} tile {} {}x{} at {} kbit/s — {} ({})",
            codec.wire_name(),
            header.id,
            header.width,
            header.height,
            per_tile_bitrate,
            encoder.name(),
            encoder.kind()
        );
        tiles.push(TilePipeline {
            header,
            codec_canvas,
            converter,
            encoder,
        });
    }
    Ok(tiles)
}

impl Drop for TilePipeline {
    fn drop(&mut self) {
        self.encoder.shutdown();
    }
}

/// Everything the capture loop needs, bundled so the signature stays readable.
struct CaptureState<'a> {
    capture: &'a mut dyn FrameSource,
    codec: encode::Codec,
    tiles: Vec<TilePipeline>,
    clock: QpcClock,
    tx: SyncSender<send::Outbound>,
    connected: Arc<AtomicBool>,
    cursor_hidden: Arc<AtomicBool>,
    /// Whether the raw dirty-rect fast path may run at all: `--no-rects` and a
    /// desktop too large for the wire's u16 coordinates both switch it off.
    rects_enabled: bool,
    /// Whether the Increment 3 pixel diff may run at all. Implies `rects_enabled`:
    /// the diff emits through the rect path or not at all.
    diff_enabled: bool,
    sparse_tx: SyncSender<send::SparseOutbound>,
    sparse_connected: Arc<AtomicBool>,
}

fn build_header(
    cfg: &Config,
    clock: QpcClock,
    capture: &dyn FrameSource,
    codec: encode::Codec,
    tiles: &[TilePipeline],
    rects_enabled: bool,
    diff_enabled: bool,
    aux_enabled: bool,
) -> Header {
    let mut h = Header::new();
    h.qpc_frequency = clock.freq();
    h.video_port = cfg.video_port;
    h.input_port = cfg.input_port;
    h.sparse_port = cfg.sparse_port;
    // Advertised only when the channel is actually being served. The client's
    // whole safety gate is this flag: it opens 9503 if and only if this says
    // true, and a host that advertises a port nothing is listening on would
    // make every session pay a failed connect for no reason.
    h.clipboard = aux_enabled;
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
    h.codec = codec.wire_name();
    h.tiles = tiles.iter().map(|tile| tile.header).collect();
    let encoder = tiles[0].encoder.as_ref();
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
    sparse_tx: SyncSender<send::SparseOutbound>,
    sparse_connected: Arc<AtomicBool>,
    tile_id: u8,
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
    native_moves: u64,
    inferred_moves: u64,
    move_inference_runs: u64,
    move_inference_hits: u64,
    move_inference_us_total: u64,
    codec: CodecEmitState,
    config_epoch: u64,
    param_set_failures: u64,
    clean_point_mismatches: u64,
    /// `Some(n)` while a keyframe request is outstanding: n non-keyframe AUs seen
    /// since. Drives the §4 "verify the connect-edge IRAP" retry.
    awaiting_keyframe: Option<u32>,
    /// Set inside the sink; acted on by the loop (which owns the encoder).
    rerequest_keyframe: bool,
    /// AUs prepared by this encoder callback. The capture loop drains them into the
    /// shared logical-frame assembler after the encoder borrow ends.
    emitted: Vec<send::FrameTile>,
}

struct VideoPlan {
    atomic_avc: bool,
    kind: VideoKind,
    frame_width: u32,
    frame_height: u32,
    block_size: u16,
    coverage: Vec<(u8, Vec<Region>)>,
    raw: Option<send::SparseOutbound>,
    move_remainder: bool,
}

struct PreparedMove {
    prelude: send::SparseOutbound,
    fence: Vec<u8>,
}

struct RoutingPlan {
    raw: Option<ChangeInfo>,
    video: Option<VideoPlan>,
}

fn full_video_plan(
    codec: encode::Codec,
    kind: VideoKind,
    frame_width: u32,
    frame_height: u32,
    tiles: &[TilePipeline],
) -> VideoPlan {
    VideoPlan {
        atomic_avc: codec == encode::Codec::H264,
        kind,
        frame_width,
        frame_height,
        block_size: CODEC_BLOCK_SIZE as u16,
        raw: None,
        move_remainder: false,
        coverage: tiles
            .iter()
            .map(|tile| {
                (
                    tile.header.id,
                    vec![Region {
                        x: tile.header.x,
                        y: tile.header.y,
                        width: tile.header.width,
                        height: tile.header.height,
                    }],
                )
            })
            .collect(),
    }
}

fn select_routing_plan(
    codec: encode::Codec,
    recovery: bool,
    force_full: bool,
    sparse_allowed: bool,
    frame_width: u32,
    frame_height: u32,
    changed: Option<&ChangeInfo>,
    tiles: &[TilePipeline],
) -> Result<RoutingPlan> {
    if recovery || force_full || codec == encode::Codec::Hevc || changed.is_none() {
        return Ok(RoutingPlan {
            raw: None,
            video: Some(full_video_plan(
                codec,
                if recovery {
                    VideoKind::Recovery
                } else {
                    VideoKind::Full
                },
                frame_width,
                frame_height,
                tiles,
            )),
        });
    }
    let changed = changed.expect("absence returned full above");
    if changed.rects.is_empty() {
        return Ok(RoutingPlan {
            raw: None,
            video: None,
        });
    }
    let changed_regions: Vec<_> = changed
        .rects
        .iter()
        .map(|rect| Region {
            x: rect.x,
            y: rect.y,
            width: rect.w,
            height: rect.h,
        })
        .collect();
    let raw_candidates = if sparse_allowed {
        changed_regions
            .iter()
            .copied()
            .filter(|region| {
                let left = region.x / CODEC_BLOCK_SIZE * CODEC_BLOCK_SIZE;
                let top = region.y / CODEC_BLOCK_SIZE * CODEC_BLOCK_SIZE;
                let right = region
                    .x
                    .saturating_add(region.width)
                    .div_ceil(CODEC_BLOCK_SIZE)
                    .saturating_mul(CODEC_BLOCK_SIZE)
                    .min(frame_width);
                let bottom = region
                    .y
                    .saturating_add(region.height)
                    .div_ceil(CODEC_BLOCK_SIZE)
                    .saturating_mul(CODEC_BLOCK_SIZE)
                    .min(frame_height);
                u64::from(right.saturating_sub(left))
                    .saturating_mul(u64::from(bottom.saturating_sub(top)))
                    .saturating_mul(4)
                    <= RECT_MAX_BYTES
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let plan = adaptive::partition_budgeted_raw(
        frame_width,
        frame_height,
        CODEC_BLOCK_SIZE,
        &changed_regions,
        &raw_candidates,
        RECT_MAX_COUNT,
        RECT_MAX_BYTES,
        4,
    )?;
    let raw_regions =
        adaptive::blocks_to_regions(&plan.raw, frame_width, frame_height, CODEC_BLOCK_SIZE)?;
    let total_blocks = u64::from(frame_width.div_ceil(CODEC_BLOCK_SIZE))
        * u64::from(frame_height.div_ceil(CODEC_BLOCK_SIZE));
    if (plan.video.len() as u64).saturating_mul(4) >= total_blocks.saturating_mul(3) {
        return Ok(RoutingPlan {
            raw: None,
            video: Some(full_video_plan(
                codec,
                VideoKind::Full,
                frame_width,
                frame_height,
                tiles,
            )),
        });
    }
    let regions =
        adaptive::blocks_to_regions(&plan.video, frame_width, frame_height, CODEC_BLOCK_SIZE)?;
    let coverage: Vec<_> = tiles
        .iter()
        .filter_map(|tile| {
            let bounds = Region {
                x: tile.header.x,
                y: tile.header.y,
                width: tile.header.width,
                height: tile.header.height,
            };
            let tile_regions: Vec<_> = regions
                .iter()
                .filter_map(|region| adaptive::intersect(*region, bounds))
                .collect();
            (!tile_regions.is_empty()).then_some((tile.header.id, tile_regions))
        })
        .collect();
    let raw = (!raw_regions.is_empty()).then(|| ChangeInfo {
        rects: raw_regions
            .into_iter()
            .map(|region| DirtyRect {
                x: region.x,
                y: region.y,
                w: region.width,
                h: region.height,
            })
            .collect(),
        moves: Vec::new(),
    });
    let video = (!plan.video.is_empty()).then_some(VideoPlan {
        atomic_avc: true,
        kind: VideoKind::Regional,
        frame_width,
        frame_height,
        block_size: CODEC_BLOCK_SIZE as u16,
        coverage,
        raw: None,
        move_remainder: false,
    });
    Ok(RoutingPlan { raw, video })
}

enum CodecEmitState {
    H264 {
        stream_sets: Option<AvcParameterSets>,
        encoder_sets: Option<AvcParameterSets>,
    },
    Hevc {
        stream_sets: Option<ParameterSets>,
        encoder_sets: Option<ParameterSets>,
        stream_config: Option<annexb::StreamConfig>,
        expected_width: u32,
        expected_height: u32,
        expected_level_idc: u8,
        awaiting_epoch_irap: bool,
        config_wait_drops: u64,
    },
}

/// Hand one encoded access unit to the sender, with its own frame's stamps.
fn emit_au(au: encode::EncodedAu, ctx: &mut EmitCtx) -> Result<()> {
    let (keyframe, bytes, prepended) = match &mut ctx.codec {
        CodecEmitState::H264 {
            stream_sets,
            encoder_sets,
        } => {
            let idr = annexb::avc_contains_idr(&au.data);
            if idr != au.keyframe {
                ctx.clean_point_mismatches += 1;
            }
            if annexb::avc_has_parameter_sets(&au.data) {
                if let Some(sets) = AvcParameterSets::from_sequence_header(&au.data) {
                    *stream_sets = Some(sets);
                }
            }
            let sets = stream_sets.as_ref().or(encoder_sets.as_ref());
            if idr && sets.is_none() {
                ctx.param_set_failures += 1;
                ctx.rerequest_keyframe = true;
                return Ok(());
            }
            let (bytes, prepended) = annexb::ensure_avc_parameter_sets(&au.data, sets, idr);
            (idr, bytes, prepended)
        }
        CodecEmitState::Hevc {
            stream_sets,
            encoder_sets,
            stream_config,
            expected_width,
            expected_height,
            expected_level_idc,
            awaiting_epoch_irap,
            config_wait_drops,
        } => {
            let irap = annexb::contains_irap(&au.data);
            if irap != au.keyframe {
                ctx.clean_point_mismatches += 1;
            }
            let candidate =
                ParameterSets::from_sequence_header(&au.data).or_else(|| encoder_sets.clone());
            if let Some(sets) = candidate {
                if stream_sets.as_ref() != Some(&sets) {
                    if stream_sets.is_some() {
                        ctx.config_epoch += 1;
                    }
                    let config = annexb::validate_stream(
                        &sets.to_annex_b(),
                        *expected_width,
                        *expected_height,
                        *expected_level_idc,
                    )
                    .map_err(|error| format!("encode: refusing HEVC configuration: {error}"))?;
                    *stream_sets = Some(sets);
                    *stream_config = Some(config);
                    *awaiting_epoch_irap = true;
                    eprintln!(
                        "encode: accepted HEVC fallback epoch {}: profile={} tier={} level={} chroma={} depth={}/{} size={}x{}",
                        ctx.config_epoch,
                        config.profile_idc,
                        if config.high_tier { "high" } else { "main" },
                        config.level_idc,
                        config.chroma_format_idc,
                        config.bit_depth_luma,
                        config.bit_depth_chroma,
                        config.width,
                        config.height
                    );
                }
            }
            if *awaiting_epoch_irap && !irap {
                *config_wait_drops += 1;
                return Ok(());
            }
            let (bytes, prepended) =
                match annexb::ensure_parameter_sets(&au.data, stream_sets.as_ref(), irap) {
                    Ok(ready) => ready,
                    Err(_) => {
                        ctx.param_set_failures += 1;
                        ctx.rerequest_keyframe = true;
                        return Ok(());
                    }
                };
            if stream_config.is_none() {
                return Err("encode: refusing HEVC AU before a validated configuration".into());
            }
            if irap {
                *awaiting_epoch_irap = false;
            }
            (irap, bytes, prepended)
        }
    };

    let mut record = FrameRecord::new();
    record.frame = au.meta.seq;
    record.tile_id = ctx.tile_id;
    record.present_qpc_us = ctx.clock.micros(au.meta.present_qpc);
    record.acquire_qpc_us = ctx.clock.micros(au.meta.acquire_qpc);
    record.convert_start_us = ctx.clock.micros(au.meta.convert_start_qpc);
    record.convert_end_us = ctx.clock.micros(au.meta.convert_end_qpc);
    record.encode_submit_us = ctx.clock.micros(au.submit_qpc);
    record.encode_out_us = ctx.clock.micros(au.out_qpc);
    record.au_bytes = bytes.len();
    record.keyframe = keyframe;
    record.param_sets_prepended = prepended;
    record.config_epoch = ctx.config_epoch;
    record.param_set_failures = ctx.param_set_failures;
    record.clean_point_mismatches = ctx.clean_point_mismatches;
    record.dropped_rects = ctx.dropped_rects;
    record.diff_runs = ctx.diff_runs;
    record.diff_hits = ctx.diff_hits;
    record.diff_us_total = ctx.diff_us_total;
    record.native_moves = ctx.native_moves;
    record.inferred_moves = ctx.inferred_moves;
    record.move_inference_runs = ctx.move_inference_runs;
    record.move_inference_hits = ctx.move_inference_hits;
    record.move_inference_us_total = ctx.move_inference_us_total;
    record.stamp_mismatches = au.stamp_mismatches;
    record.claimed_changed_pixels = au.meta.claimed_changed_pixels;
    record.measured_changed_pixels = au.meta.measured_changed_pixels;
    record.frame_pixels = au.meta.frame_pixels;
    record.pro_rata_unchanged_au_bytes = stats::pro_rata_unchanged_bytes(
        bytes.len(),
        au.meta.frame_pixels,
        au.meta
            .measured_changed_pixels
            .or(au.meta.claimed_changed_pixels),
    );
    record.raw_rect_attempted = au.meta.raw_rect_attempted;
    record.raw_rect_sent = au.meta.raw_rect_sent;
    if au.meta.change_valid {
        record.dirty_rect_count = Some(au.meta.dirty_rect_count);
        record.dirty_bytes = Some(au.meta.dirty_bytes);
        record.move_rect_count = Some(au.meta.move_rect_count);
    }

    match ctx.awaiting_keyframe {
        Some(n) if keyframe => {
            record.keyframe_wait_frames = Some(n);
            ctx.awaiting_keyframe = None;
        }
        Some(n) if n + 1 >= KEYFRAME_RETRY_FRAMES => {
            // A second's worth of frames and no IRAP: the request was ignored.
            ctx.rerequest_keyframe = true;
            ctx.awaiting_keyframe = Some(0);
        }
        Some(n) => ctx.awaiting_keyframe = Some(n + 1),
        None => {}
    }

    ctx.emitted.push(send::FrameTile {
        record: Box::new(record),
        tile_id: ctx.tile_id,
        seq: au.meta.seq,
        au: bytes.into_owned(),
    });
    Ok(())
}

fn admit_emitted(
    ctx: &mut EmitCtx,
    assembler: &mut logical_frame::PlannedAssembler<VideoPlan, send::FrameTile>,
    tx: &SyncSender<send::Outbound>,
    recovery: &mut logical_frame::Recovery,
    want_keyframe: &mut bool,
) -> Result<()> {
    let emitted: Vec<_> = ctx.emitted.drain(..).collect();
    for tile in emitted {
        let assembled = assembler.push(tile.seq, tile.tile_id, tile);
        admit_ready_frames(assembled.ready, ctx, tx, recovery, want_keyframe)?;
    }
    Ok(())
}

fn admit_ready_frames(
    ready: Vec<logical_frame::PlannedComplete<VideoPlan, send::FrameTile>>,
    ctx: &mut EmitCtx,
    tx: &SyncSender<send::Outbound>,
    recovery: &mut logical_frame::Recovery,
    want_keyframe: &mut bool,
) -> Result<()> {
    for mut frame in ready {
        debug_assert!(frame.tiles.iter().all(|(_, tile)| tile.seq == frame.seq));
        let has_keyframe = frame.tiles.iter().any(|(_, tile)| tile.record.keyframe);
        let all_keyframes = frame.tiles.iter().all(|(_, tile)| tile.record.keyframe);
        let is_recovery = frame.plan.kind == VideoKind::Recovery && all_keyframes;
        match recovery.prepare(has_keyframe, is_recovery) {
            logical_frame::RecoveryDecision::Admit => {}
            logical_frame::RecoveryDecision::Suppress if frame.plan.move_remainder => {
                return Err("move video remainder was suppressed after its prelude".into());
            }
            logical_frame::RecoveryDecision::Suppress => continue,
            logical_frame::RecoveryDecision::SuppressAndRequest => {
                if frame.plan.move_remainder {
                    return Err("move video remainder requested recovery after its prelude".into());
                }
                *want_keyframe = true;
                continue;
            }
        }
        let mut records = Vec::with_capacity(frame.tiles.len());
        let mut wire_tiles = Vec::with_capacity(frame.tiles.len());
        for (tile_id, mut tile) in frame.tiles {
            tile.record.dropped_frames = recovery.dropped();
            let coverage = frame
                .plan
                .coverage
                .iter()
                .find(|(planned_id, _)| *planned_id == tile_id)
                .map(|(_, coverage)| coverage.clone())
                .expect("assembler returned only planned tile ids");
            wire_tiles.push(VideoTile {
                tile_id,
                coverage,
                au: std::mem::take(&mut tile.au),
            });
            records.push(tile);
        }
        let outbound = if frame.plan.atomic_avc {
            let update = VideoUpdate {
                frame_seq: frame.seq,
                frame_width: frame.plan.frame_width,
                frame_height: frame.plan.frame_height,
                block_size: frame.plan.block_size,
                kind: frame.plan.kind,
                tiles: wire_tiles,
            };
            let mut payload = Vec::with_capacity(video_update::encoded_len(&update));
            video_update::encode(&update, &mut payload);
            send::Outbound::Video(records, payload, None)
        } else {
            debug_assert_eq!(records.len(), 1, "HEVC fallback is one full-frame tile");
            send::Outbound::FrameSet(records)
        };
        let had_raw = frame.plan.raw.is_some();
        match admit_video_and_raw(tx, &ctx.sparse_tx, outbound, frame.plan.raw.take())? {
            true => recovery.admitted(is_recovery),
            false => {
                if frame.plan.move_remainder {
                    return Err("move video remainder was rejected after its prelude".into());
                }
                if had_raw {
                    ctx.dropped_rects += 1;
                }
                // Even a complete recovery frame may be the item rejected by
                // the queue, so every full admission needs a fresh all-tile IRAP.
                recovery.queue_full();
                *want_keyframe = true;
            }
        }
    }
    Ok(())
}

fn admit_move(
    video_tx: &SyncSender<send::Outbound>,
    sparse_tx: &SyncSender<send::SparseOutbound>,
    movement: PreparedMove,
) -> Result<bool> {
    let gate = AdmissionGate::pending();
    let outbound = send::Outbound::MoveFence(movement.fence, Some(gate.clone()));
    match video_tx.try_send(outbound) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            gate.decide(false);
            return Ok(false);
        }
        Err(TrySendError::Disconnected(_)) => {
            gate.decide(false);
            return Err("sender thread has gone away".to_owned().into());
        }
    }
    let mut prelude = movement.prelude;
    prelude.gate = Some(gate.clone());
    match sparse_tx.try_send(prelude) {
        Ok(()) => {
            gate.decide(true);
            Ok(true)
        }
        Err(TrySendError::Full(_)) => {
            gate.decide(false);
            Ok(false)
        }
        Err(TrySendError::Disconnected(_)) => {
            gate.decide(false);
            Err("sparse sender thread has gone away".to_owned().into())
        }
    }
}

fn admit_video_and_raw(
    video_tx: &SyncSender<send::Outbound>,
    sparse_tx: &SyncSender<send::SparseOutbound>,
    mut video: send::Outbound,
    raw: Option<send::SparseOutbound>,
) -> Result<bool> {
    let Some(mut raw) = raw else {
        return match video_tx.try_send(video) {
            Ok(()) => Ok(true),
            Err(TrySendError::Full(_)) => Ok(false),
            Err(TrySendError::Disconnected(_)) => {
                Err("sender thread has gone away".to_owned().into())
            }
        };
    };
    let gate = AdmissionGate::pending();
    match &mut video {
        send::Outbound::Video(_, _, video_gate) => *video_gate = Some(gate.clone()),
        _ => {
            return Err("raw pixels cannot accompany the HEVC fallback"
                .to_owned()
                .into())
        }
    }
    raw.gate = Some(gate.clone());
    match video_tx.try_send(video) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            gate.decide(false);
            return Ok(false);
        }
        Err(TrySendError::Disconnected(_)) => {
            gate.decide(false);
            return Err("sender thread has gone away".to_owned().into());
        }
    }
    match sparse_tx.try_send(raw) {
        Ok(()) => {
            gate.decide(true);
            Ok(true)
        }
        Err(TrySendError::Full(_)) => {
            gate.decide(false);
            Ok(false)
        }
        Err(TrySendError::Disconnected(_)) => {
            gate.decide(false);
            Err("sparse sender thread has gone away".to_owned().into())
        }
    }
}

fn block_aligned_raw_change(
    change: &ChangeInfo,
    frame_width: u32,
    frame_height: u32,
) -> Result<Option<ChangeInfo>> {
    let changed: Vec<_> = change
        .rects
        .iter()
        .map(|rect| Region {
            x: rect.x,
            y: rect.y,
            width: rect.w,
            height: rect.h,
        })
        .collect();
    let UpdatePlan::Incremental(plan) = adaptive::partition_blocks(
        frame_width,
        frame_height,
        CODEC_BLOCK_SIZE,
        &changed,
        &[],
        &changed,
        false,
    )?
    else {
        unreachable!("full fallback was not requested")
    };
    let regions =
        adaptive::blocks_to_regions(&plan.raw, frame_width, frame_height, CODEC_BLOCK_SIZE)?;
    let aligned = ChangeInfo {
        rects: regions
            .into_iter()
            .map(|region| DirtyRect {
                x: region.x,
                y: region.y,
                w: region.width,
                h: region.height,
            })
            .collect(),
        moves: Vec::new(),
    };
    Ok(
        (aligned.rects.len() <= RECT_MAX_COUNT && aligned.dirty_bytes() <= RECT_MAX_BYTES)
            .then_some(aligned),
    )
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
) -> Result<bool> {
    let outbound = prepare_rects(
        rect_pixels,
        frame_seq,
        frame_width,
        frame_height,
        pack_start,
        from_diff,
        ctx,
    );
    admit_rects(outbound, ctx)
}

fn prepare_rects(
    rect_pixels: Vec<rects::Rect>,
    frame_seq: u64,
    frame_width: u32,
    frame_height: u32,
    pack_start: i64,
    from_diff: bool,
    ctx: &EmitCtx,
) -> send::SparseOutbound {
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

    send::SparseOutbound {
        msg_type: crate::framing::MSG_RECTS,
        record: Some(Box::new(record)),
        payload,
        gate: None,
    }
}

fn prepare_move(
    movements: &[super::source::MoveRect],
    baseline_seq: u64,
    frame_seq: u64,
    frame_width: u32,
    frame_height: u32,
    remainder: rects::MoveRemainder,
    inferred: bool,
    pack_start: i64,
    ctx: &EmitCtx,
) -> Result<PreparedMove> {
    let moves = movements
        .iter()
        .map(|movement| {
            Ok(rects::MoveRect {
                src_x: u16::try_from(movement.src_x)?,
                src_y: u16::try_from(movement.src_y)?,
                dst_x: u16::try_from(movement.dst_x)?,
                dst_y: u16::try_from(movement.dst_y)?,
                w: u16::try_from(movement.w)?,
                h: u16::try_from(movement.h)?,
            })
        })
        .collect::<std::result::Result<Vec<_>, std::num::TryFromIntError>>()?;
    let update = rects::MovePrelude {
        baseline_seq,
        frame_seq,
        frame_width,
        frame_height,
        remainder,
        moves,
    };
    let mut payload = Vec::with_capacity(rects::move_encoded_len(&update));
    rects::encode_move_prelude(&update, &mut payload);
    let mut record = stats::RectRecord::new();
    record.record = "move";
    record.frame = frame_seq;
    record.move_count = movements.len() as u32;
    record.pack_start_us = ctx.clock.micros(pack_start);
    record.pack_end_us = ctx.clock.micros(qpc::now());
    record.dropped_rects = ctx.dropped_rects;
    record.move_inferred = inferred;
    Ok(PreparedMove {
        prelude: send::SparseOutbound {
            msg_type: crate::framing::MSG_MOVE_UPDATE,
            record: Some(Box::new(record)),
            payload,
            gate: None,
        },
        fence: crate::framing::encode_move_fence(baseline_seq, frame_seq).to_vec(),
    })
}

/// Video fallback for a move frame owns the move destinations as final pixels.
/// Without this expansion, disabling the move path would silently omit them.
fn final_pixel_coverage(change: &ChangeInfo) -> ChangeInfo {
    let mut rects = change.rects.clone();
    rects.extend(change.moves.iter().map(|movement| DirtyRect {
        x: movement.dst_x,
        y: movement.dst_y,
        w: movement.w,
        h: movement.h,
    }));
    ChangeInfo {
        rects,
        moves: Vec::new(),
    }
}

fn admit_rects(outbound: send::SparseOutbound, ctx: &mut EmitCtx) -> Result<bool> {
    if !ctx.sparse_connected.load(Ordering::Acquire) {
        ctx.dropped_rects += 1;
        return Ok(false);
    }
    match ctx.sparse_tx.try_send(outbound) {
        Ok(()) => Ok(true),
        // Full means the complete logical update was lost. The caller enters
        // recovery; no dependent sparse/regional traffic may follow it.
        Err(TrySendError::Full(_)) => {
            ctx.dropped_rects += 1;
            Ok(false)
        }
        Err(TrySendError::Disconnected(_)) => Err("sender thread has gone away".to_owned().into()),
    }
}

fn capture_loop(mut state: CaptureState<'_>) -> Result<()> {
    let mut frame_seq: u64 = 0;
    let mut bootstrap = ViewerBootstrap::new();
    let mut want_keyframe = true;
    let mut recovery = logical_frame::Recovery::waiting();
    let mut assembler =
        logical_frame::PlannedAssembler::new(state.tiles.len(), MAX_PENDING_LOGICAL_FRAMES);
    // HLD §5: the first frame after a rebuilt duplication is forced down the
    // full-frame path. Its dirty metadata describes change since the *new*
    // duplication's baseline, and whatever changed between the last delivered
    // frame and that baseline is described by nothing — a small rect here would
    // lie by omission. The AU has no such gap: the encoder references the last
    // frame it actually encoded, so the difference it ships is complete.
    // (`Recreated` is a rebuilt duplication under `--source dxgi`, and a rebuilt
    // shared pool under `--source idd`; the reasoning is identical.)
    let mut suppress_rects_once = false;
    // Sequence of the newest logical update scheduled for this viewer. Capture
    // sequence gaps with no payload are deliberately absent from this baseline.
    let mut last_scheduled_seq: Option<u64> = None;
    let mut last_epochs: Vec<u64> = state
        .tiles
        .iter()
        .map(|tile| tile.encoder.config_epoch())
        .collect();
    // HLD §6b: the previous consumed frame, retained on the GPU, plus the stamp that
    // says how long ago it was. Both are the diff's whole state, and both reset on a
    // rebuilt source.
    let mut pixel_diff = pixel_diff::PixelDiff::new(state.capture.width(), state.capture.height());
    let mut last_acquire_qpc: Option<i64> = None;
    let frame_duration_hns = HNS_PER_SECOND / DECLARED_FPS.max(1) as i64;
    let start_qpc = qpc::now();
    let mut surface_stall = crate::surface_pool::SaturationWatchdog::new(SURFACE_STALL_TIMEOUT);
    let mut contexts: Vec<EmitCtx> = state
        .tiles
        .iter()
        .zip(last_epochs.iter().copied())
        .map(|(tile, epoch)| EmitCtx {
            clock: state.clock,
            sparse_tx: state.sparse_tx.clone(),
            sparse_connected: Arc::clone(&state.sparse_connected),
            tile_id: tile.header.id,
            dropped_rects: 0,
            diff_runs: 0,
            diff_hits: 0,
            diff_us_total: 0,
            native_moves: 0,
            inferred_moves: 0,
            move_inference_runs: 0,
            move_inference_hits: 0,
            move_inference_us_total: 0,
            codec: match state.codec {
                encode::Codec::H264 => CodecEmitState::H264 {
                    stream_sets: None,
                    encoder_sets: None,
                },
                encode::Codec::Hevc => CodecEmitState::Hevc {
                    stream_sets: None,
                    encoder_sets: None,
                    stream_config: None,
                    expected_width: tile.header.width,
                    expected_height: tile.header.height,
                    expected_level_idc: tile
                        .encoder
                        .hevc_level_idc()
                        .expect("HEVC encoder must publish its selected level"),
                    awaiting_epoch_irap: true,
                    config_wait_drops: 0,
                },
            },
            config_epoch: epoch,
            param_set_failures: 0,
            clean_point_mismatches: 0,
            awaiting_keyframe: None,
            rerequest_keyframe: false,
            emitted: Vec::new(),
        })
        .collect();

    loop {
        state
            .cursor_hidden
            .store(state.capture.hide_local_cursor(), Ordering::Release);
        let connected = state.connected.load(Ordering::Acquire);
        if !connected {
            // Nothing is watching. Capturing anyway would hold the compositor and
            // run the GPU encoder for an audience of nobody.
            bootstrap.observe(false);
            recovery.reset();
            assembler.discard_through(frame_seq);
            last_scheduled_seq = None;
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }
        if bootstrap.observe(true) {
            // A new viewer starts mid-GOP and cannot decode a P-frame.
            want_keyframe = true;
            last_scheduled_seq = None;
        }
        // Drain completed output before waiting for another capture. This keeps a
        // final AU moving on a pointer-only desktop and avoids putting old encode
        // work in front of a newly captured interaction frame.
        pump_tiles(
            &mut state.tiles,
            &mut contexts,
            &mut last_epochs,
            &mut assembler,
            &state.tx,
            &mut recovery,
            &mut want_keyframe,
        )?;
        let pool_saturated = state
            .tiles
            .iter()
            .any(|tile| !tile.converter.has_capacity());
        if surface_stall.expired(pool_saturated, Instant::now()) {
            return Err(format!(
                "whole-frame NV12 capacity did not recover for {} ms; restarting instead of freezing",
                SURFACE_STALL_TIMEOUT.as_millis()
            )
            .into());
        }
        let keyframe_requested =
            request_keyframes(&mut state.tiles, &mut contexts, &mut want_keyframe);

        let acquired = state.capture.acquire(ACQUIRE_TIMEOUT_MS)?;
        let (texture, present_qpc, acquire_qpc, change) = match acquired {
            Acquired::Frame {
                texture,
                present_qpc,
                acquire_qpc,
                change,
            } => (texture, present_qpc, acquire_qpc, change),
            Acquired::Timeout | Acquired::PointerOnly
                if bootstrap.use_retained_on_idle(pixel_diff.valid(), keyframe_requested) =>
            {
                // Give a newly attached viewer the current desktop even when DWM is
                // idle. Polling the source first lets a rebuild invalidate this copy
                // and lets genuinely fresh pixels win. Both timestamps describe the
                // synthetic acquisition now; the retained presentation time would
                // otherwise report the idle interval as capture latency.
                let now = qpc::now();
                (
                    pixel_diff
                        .retained()
                        .expect("valid retained frame must have a texture"),
                    now,
                    now,
                    None,
                )
            }
            Acquired::Timeout | Acquired::PointerOnly => continue,
            Acquired::Recreated => {
                eprintln!("capture: frame source lost and rebuilt");
                bootstrap.source_recreated();
                want_keyframe = true;
                recovery.reset();
                assembler.discard_through(frame_seq);
                last_scheduled_seq = None;
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
        if pool_saturated {
            // No tile has been submitted yet, so shedding here preserves both tile
            // coherence and every decoder reference. The next captured frame's
            // damage metadata no longer spans the viewer's baseline, so suppress
            // sparse use and let the full-frame codec catch up atomically.
            recovery.drop_before_encode();
            suppress_rects_once = true;
            continue;
        }
        bootstrap.frame_admitted();

        let claimed_changed_pixels = change.as_ref().map(|change| {
            let mut bounds: Vec<_> = change
                .rects
                .iter()
                .map(|rect| (rect.x, rect.y, rect.w, rect.h))
                .collect();
            bounds.extend(
                change
                    .moves
                    .iter()
                    .map(|movement| (movement.dst_x, movement.dst_y, movement.w, movement.h)),
            );
            rects::union_area(&bounds, state.capture.width(), state.capture.height())
        });
        let mut measured_changed_pixels = None;
        let mut raw_rect_attempted = false;
        let mut raw_rect_sent = false;
        let sparse_allowed =
            recovery.allows_overlays() && state.sparse_connected.load(Ordering::Acquire);
        let force_full_once = suppress_rects_once;
        suppress_rects_once = false;
        let mut effective_change = change.clone();
        let mut inferred_move = false;
        if state.codec == encode::Codec::H264
            && state.rects_enabled
            && sparse_allowed
            && assembler.is_empty()
            && !force_full_once
            && last_scheduled_seq == frame_seq.checked_sub(1)
            && pixel_diff.valid()
            && effective_change.as_ref().is_some_and(|change| {
                change.moves.is_empty()
                    && !change.rects.is_empty()
                    && change.dirty_bytes() > RECT_MAX_BYTES
            })
        {
            let damage = effective_change
                .as_ref()
                .expect("inference predicate established damage")
                .rects
                .iter()
                .map(|rect| diff::Region {
                    x: rect.x,
                    y: rect.y,
                    w: rect.w,
                    h: rect.h,
                })
                .collect::<Vec<_>>();
            let bounds = claimed_bounds(
                &effective_change
                    .as_ref()
                    .expect("inference predicate established damage")
                    .rects,
                state.capture.width(),
                state.capture.height(),
            );
            let inference_start = qpc::now();
            let inferred = {
                let device = state.capture.device();
                let context = state.capture.context();
                pixel_diff.infer_vertical_move(
                    device,
                    context,
                    &texture,
                    bounds,
                    &damage,
                    MAX_INFERRED_MOVE_SHIFT,
                )?
            };
            contexts[0].move_inference_runs += 1;
            contexts[0].move_inference_us_total += state
                .clock
                .micros(qpc::now().saturating_sub(inference_start))
                .max(0) as u64;
            if let Some(inferred) = inferred {
                contexts[0].move_inference_hits += 1;
                inferred_move = true;
                effective_change = Some(ChangeInfo {
                    rects: inferred
                        .remainder
                        .into_iter()
                        .map(|region| DirtyRect {
                            x: region.x,
                            y: region.y,
                            w: region.w,
                            h: region.h,
                        })
                        .collect(),
                    moves: vec![super::source::MoveRect {
                        src_x: inferred.movement.source.x,
                        src_y: inferred.movement.source.y,
                        dst_x: inferred.movement.destination.x,
                        dst_y: inferred.movement.destination.y,
                        w: inferred.movement.destination.w,
                        h: inferred.movement.destination.h,
                    }],
                });
            }
        }
        let move_change = effective_change.as_ref().filter(|change| {
            !change.moves.is_empty()
                && change.moves.len() <= RECT_MAX_COUNT
                && change.moves.iter().fold(0u64, |pixels, movement| {
                    pixels.saturating_add(u64::from(movement.w) * u64::from(movement.h))
                }) <= u64::from(state.capture.width()) * u64::from(state.capture.height())
                && state.codec == encode::Codec::H264
                && state.rects_enabled
                && sparse_allowed
                && assembler.is_empty()
                && !force_full_once
                && last_scheduled_seq == frame_seq.checked_sub(1)
        });
        let fallback_change = effective_change
            .as_ref()
            .filter(|change| move_change.is_none() && !change.moves.is_empty())
            .map(final_pixel_coverage);
        let mut move_remainder_active = false;
        let mut routing =
            if let (Some(move_change), Some(baseline_seq)) = (move_change, last_scheduled_seq) {
                let dirty_only = ChangeInfo {
                    rects: move_change.rects.clone(),
                    moves: Vec::new(),
                };
                let raw_remainder = if dirty_only.rects.is_empty() {
                    None
                } else {
                    block_aligned_raw_change(
                        &dirty_only,
                        state.capture.width(),
                        state.capture.height(),
                    )?
                };
                let remainder = if dirty_only.rects.is_empty() {
                    rects::MoveRemainder::None
                } else if raw_remainder.is_some() {
                    rects::MoveRemainder::Raw
                } else {
                    rects::MoveRemainder::Video
                };
                let prepared = prepare_move(
                    &move_change.moves,
                    baseline_seq,
                    frame_seq,
                    state.capture.width(),
                    state.capture.height(),
                    remainder,
                    inferred_move,
                    qpc::now(),
                    &contexts[0],
                )?;
                if admit_move(&state.tx, &contexts[0].sparse_tx, prepared)? {
                    if inferred_move {
                        contexts[0].inferred_moves += move_change.moves.len() as u64;
                    } else {
                        contexts[0].native_moves += move_change.moves.len() as u64;
                    }
                    move_remainder_active = true;
                    last_scheduled_seq = Some(frame_seq);
                    match remainder {
                        rects::MoveRemainder::None => RoutingPlan {
                            raw: None,
                            video: None,
                        },
                        rects::MoveRemainder::Raw => RoutingPlan {
                            raw: raw_remainder,
                            video: None,
                        },
                        rects::MoveRemainder::Video => {
                            let mut route = select_routing_plan(
                                state.codec,
                                false,
                                false,
                                false,
                                state.capture.width(),
                                state.capture.height(),
                                Some(&dirty_only),
                                &state.tiles,
                            )?;
                            route
                                .video
                                .as_mut()
                                .expect("non-empty video-only remainder must make a video plan")
                                .move_remainder = true;
                            route
                        }
                    }
                } else {
                    let final_pixels = final_pixel_coverage(move_change);
                    select_routing_plan(
                        state.codec,
                        !recovery.allows_overlays(),
                        true,
                        false,
                        state.capture.width(),
                        state.capture.height(),
                        Some(&final_pixels),
                        &state.tiles,
                    )?
                }
            } else {
                select_routing_plan(
                    state.codec,
                    !recovery.allows_overlays(),
                    force_full_once,
                    state.rects_enabled && sparse_allowed,
                    state.capture.width(),
                    state.capture.height(),
                    fallback_change.as_ref().or(effective_change.as_ref()),
                    &state.tiles,
                )?
            };
        let mut raw_only = None;

        // Read raw-owned blocks immediately while the capture texture is valid.
        // All-raw plans enqueue now; mixed plans carry the prepared payload until
        // their regional AUs are complete, then both bounded queues commit together.
        if let Some(raw_change) = routing.raw.take() {
            raw_rect_attempted = true;
            let pack_start = qpc::now();
            let rect_pixels = state.capture.read_rects(&texture, &raw_change)?;
            let outbound = prepare_rects(
                rect_pixels,
                frame_seq,
                state.capture.width(),
                state.capture.height(),
                pack_start,
                false,
                &contexts[0],
            );
            if let Some(video) = routing.video.as_mut() {
                video.raw = Some(outbound);
                raw_rect_sent = true;
            } else {
                raw_only = Some(outbound);
            }
        }
        let metadata_took_fast_path = raw_rect_attempted || move_remainder_active;

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
            Some(last) => state.clock.micros(acquire_qpc.saturating_sub(last)) >= DIFF_IDLE_GAP_US,
            None => false,
        };
        let metadata_claims_no_change = change
            .as_ref()
            .is_some_and(|change| change.rects.is_empty() && change.moves.is_empty());
        if state.diff_enabled
            && sparse_allowed
            && !force_full_once
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
            let diff_us = state
                .clock
                .micros(qpc::now().saturating_sub(diff_start))
                .max(0) as u64;
            contexts[0].diff_runs += 1;
            contexts[0].diff_us_total += diff_us;
            // `None` is a miss — too many rects, or too many bytes — and needs no
            // action: the frame is on the codec path already, which is where a
            // large change belongs anyway.
            if let Some(rect_pixels) = measured {
                contexts[0].diff_hits += 1;
                if !rect_pixels.is_empty() {
                    measured_changed_pixels = Some(rects::union_area(
                        &rect_pixels
                            .iter()
                            .map(|rect| {
                                (rect.x as u32, rect.y as u32, rect.w as u32, rect.h as u32)
                            })
                            .collect::<Vec<_>>(),
                        state.capture.width(),
                        state.capture.height(),
                    ));
                    let measured_change = ChangeInfo {
                        rects: rect_pixels
                            .iter()
                            .map(|rect| DirtyRect {
                                x: u32::from(rect.x),
                                y: u32::from(rect.y),
                                w: u32::from(rect.w),
                                h: u32::from(rect.h),
                            })
                            .collect(),
                        moves: Vec::new(),
                    };
                    if let Some(aligned) = block_aligned_raw_change(
                        &measured_change,
                        state.capture.width(),
                        state.capture.height(),
                    )? {
                        raw_rect_attempted = true;
                        let rect_pixels = state.capture.read_rects(&texture, &aligned)?;
                        raw_rect_sent = send_rects(
                            rect_pixels,
                            frame_seq,
                            state.capture.width(),
                            state.capture.height(),
                            diff_start,
                            true,
                            &mut contexts[0],
                        )?;
                        routing.video = None;
                    }
                }
            }
        }
        // Still inside the frame's validity window, and before the converter touches
        // it: this frame becomes the next one's baseline and the current desktop a
        // later viewer can bootstrap from. The staging surfaces still stay lazy when
        // diff is off — see [`pixel_diff`].
        let device = state.capture.device();
        let context = state.capture.context();
        pixel_diff.retain(device, context, &texture)?;
        last_acquire_qpc = Some(acquire_qpc);

        if let Some(outbound) = raw_only {
            raw_rect_sent = admit_rects(outbound, &mut contexts[0])?;
            if move_remainder_active && !raw_rect_sent {
                return Err("move raw remainder was rejected after its prelude".into());
            }
            if raw_rect_sent {
                last_scheduled_seq = Some(frame_seq);
            }
        }
        // A diff-selected or all-raw update is complete without H.264. A mixed
        // metadata plan continues below with its raw payload attached to `video`.
        if raw_rect_attempted && routing.video.is_none() {
            if !raw_rect_sent {
                recovery.queue_full();
                want_keyframe = true;
                suppress_rects_once = true;
            }
            continue;
        }

        let time_hns = state
            .clock
            .micros(acquire_qpc.saturating_sub(start_qpc))
            .saturating_mul(10);

        // The rect/diff counters describe the captured desktop frame and therefore
        // belong on both tile rows, not only tile zero which owns the raw fast path.
        let shared = (
            contexts[0].dropped_rects,
            contexts[0].diff_runs,
            contexts[0].diff_hits,
            contexts[0].diff_us_total,
            contexts[0].native_moves,
            contexts[0].inferred_moves,
            contexts[0].move_inference_runs,
            contexts[0].move_inference_hits,
            contexts[0].move_inference_us_total,
        );
        for ctx in contexts.iter_mut().skip(1) {
            (
                ctx.dropped_rects,
                ctx.diff_runs,
                ctx.diff_hits,
                ctx.diff_us_total,
                ctx.native_moves,
                ctx.inferred_moves,
                ctx.move_inference_runs,
                ctx.move_inference_hits,
                ctx.move_inference_us_total,
            ) = shared;
        }

        let Some(plan) = routing.video else {
            continue;
        };
        let planned_coverage = plan.coverage.clone();
        let expected_tiles: Vec<_> = plan.coverage.iter().map(|(tile_id, _)| *tile_id).collect();
        let dropped = assembler.begin(frame_seq, plan, &expected_tiles);
        if dropped != 0 && move_remainder_active {
            return Err("move video remainder assembler dropped a frame after its prelude".into());
        }
        if dropped != 0 {
            recovery.drop_incomplete(dropped);
            want_keyframe = true;
        }
        last_scheduled_seq = Some(frame_seq);

        for (((tile, ctx), last_epoch), tile_index) in state
            .tiles
            .iter_mut()
            .zip(&mut contexts)
            .zip(&mut last_epochs)
            .zip(0usize..)
        {
            let Some((_, tile_coverage)) = planned_coverage
                .iter()
                .find(|(tile_id, _)| *tile_id == tile.header.id)
            else {
                continue;
            };
            let convert_source = match tile.codec_canvas.as_ref() {
                Some(canvas) => {
                    let copied = canvas.update(&texture, tile_coverage);
                    debug_assert!(copied, "selected tile must own codec coverage");
                    canvas.texture()
                }
                None => &texture,
            };
            let convert_start = qpc::now();
            let nv12 = tile.converter.convert(convert_source)?;
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
                meta.move_rect_count = change.moves.len() as u32;
            }
            meta.claimed_changed_pixels = claimed_changed_pixels;
            meta.measured_changed_pixels = measured_changed_pixels;
            // The changed-pixel counts describe the whole captured desktop. Keep the
            // denominator in the same coordinate space; using tile pixels here would
            // exaggerate the changed share on a two-tile 5K frame.
            meta.frame_pixels =
                u64::from(state.capture.width()) * u64::from(state.capture.height());
            meta.raw_rect_attempted = raw_rect_attempted;
            meta.raw_rect_sent = raw_rect_sent;

            let sample = encode::sample_from_texture(&nv12.texture, time_hns, frame_duration_hns)?;
            match (&mut ctx.codec, tile.encoder.parameter_sets()) {
                (
                    CodecEmitState::H264 { encoder_sets, .. },
                    Some(encode::CodecParameterSets::H264(sets)),
                ) => *encoder_sets = Some(sets.clone()),
                (
                    CodecEmitState::Hevc { encoder_sets, .. },
                    Some(encode::CodecParameterSets::Hevc(sets)),
                ) => *encoder_sets = Some(sets.clone()),
                (_, None) => {}
                _ => return Err("encoder parameter-set codec changed mid-stream".into()),
            }
            tile.encoder
                .encode(&sample, meta, nv12.slot, &mut |au| emit_au(au, ctx))?;
            tile.encoder
                .release_retired_surfaces(&mut |slot| tile.converter.release(slot))?;
            admit_emitted(
                ctx,
                &mut assembler,
                &state.tx,
                &mut recovery,
                &mut want_keyframe,
            )?;
            housekeep(tile.encoder.as_ref(), ctx, &mut want_keyframe, last_epoch);
            debug_assert_eq!(usize::from(tile.header.id), tile_index);
        }
    }
}

fn request_keyframes(
    tiles: &mut [TilePipeline],
    contexts: &mut [EmitCtx],
    want_keyframe: &mut bool,
) -> bool {
    if !*want_keyframe {
        return false;
    }
    for (tile, ctx) in tiles.iter_mut().zip(contexts) {
        tile.encoder.request_keyframe();
        ctx.awaiting_keyframe = Some(0);
    }
    *want_keyframe = false;
    true
}

fn pump_tiles(
    tiles: &mut [TilePipeline],
    contexts: &mut [EmitCtx],
    last_epochs: &mut [u64],
    assembler: &mut logical_frame::PlannedAssembler<VideoPlan, send::FrameTile>,
    tx: &SyncSender<send::Outbound>,
    recovery: &mut logical_frame::Recovery,
    want_keyframe: &mut bool,
) -> Result<()> {
    for ((tile, ctx), last_epoch) in tiles.iter_mut().zip(contexts).zip(last_epochs) {
        tile.encoder.pump(&mut |au| emit_au(au, ctx))?;
        tile.encoder
            .release_retired_surfaces(&mut |slot| tile.converter.release(slot))?;
        admit_emitted(ctx, assembler, tx, recovery, want_keyframe)?;
        housekeep(tile.encoder.as_ref(), ctx, want_keyframe, last_epoch);
    }
    Ok(())
}

/// Post-emission actions that need the encoder, which the sink cannot borrow.
fn housekeep(
    encoder: &dyn encode::Encoder,
    ctx: &mut EmitCtx,
    want_keyframe: &mut bool,
    last_epoch: &mut u64,
) {
    let epoch = encoder.config_epoch();
    if epoch != *last_epoch {
        // The encoder renegotiated its output type; sets scanned from the old
        // stream would poison the new one.
        *last_epoch = epoch;
        ctx.config_epoch += 1;
        match &mut ctx.codec {
            CodecEmitState::H264 { stream_sets, .. } => *stream_sets = None,
            CodecEmitState::Hevc {
                stream_sets,
                stream_config,
                awaiting_epoch_irap,
                ..
            } => {
                *stream_sets = None;
                *stream_config = None;
                *awaiting_epoch_irap = true;
            }
        }
    }
    if ctx.rerequest_keyframe {
        ctx.rerequest_keyframe = false;
        eprintln!(
            "encode: keyframe request ignored for {KEYFRAME_RETRY_FRAMES} frames; asking again"
        );
        *want_keyframe = true;
    }
}
