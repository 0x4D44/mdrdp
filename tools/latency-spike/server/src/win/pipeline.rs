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
use crate::annexb;
use crate::cli::{Config, DECLARED_FPS};
use crate::stats::{self, FrameRecord, Header, QpcClock};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// Frames in flight between the encoder and the socket. Two is enough to overlap a
/// write with the next encode and small enough that a stall shows up as a drop
/// rather than as growing latency.
const QUEUE_DEPTH: usize = 2;

/// `AcquireNextFrame` timeout. Short enough that a `Recreated` duplication or a
/// newly connected client is noticed promptly, long enough not to spin.
const ACQUIRE_TIMEOUT_MS: u32 = 8;

/// 100-nanosecond units per second — Media Foundation's time base.
const HNS_PER_SECOND: i64 = 10_000_000;

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

    let header = build_header(cfg, clock, &capture, encoder.as_ref());
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
}

fn capture_state<'a>(
    capture: &'a mut dxgi::Capture,
    converter: &'a mut convert::Nv12Converter,
    encoder: &'a mut dyn encode::Encoder,
    clock: QpcClock,
    tx: SyncSender<send::Outbound>,
    connected: Arc<AtomicBool>,
) -> CaptureState<'a> {
    CaptureState {
        capture,
        converter,
        encoder,
        clock,
        tx,
        connected,
    }
}

fn build_header(
    cfg: &Config,
    clock: QpcClock,
    capture: &dxgi::Capture,
    encoder: &dyn encode::Encoder,
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
    h.sequence_header_available = encoder.parameter_sets().is_some();
    h
}

fn capture_loop(state: CaptureState<'_>) -> Result<()> {
    let mut frame_index: u64 = 0;
    let mut dropped: u64 = 0;
    let mut was_connected = false;
    let mut want_keyframe = true;
    let frame_duration_hns = HNS_PER_SECOND / DECLARED_FPS.max(1) as i64;
    let start_qpc = qpc::now();

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
            want_keyframe = false;
        }

        let acquired = state.capture.acquire(ACQUIRE_TIMEOUT_MS)?;
        let (texture, present_qpc, acquire_qpc) = match acquired {
            dxgi::Acquired::Frame {
                texture,
                present_qpc,
                acquire_qpc,
            } => (texture, present_qpc, acquire_qpc),
            dxgi::Acquired::Timeout | dxgi::Acquired::PointerOnly => continue,
            dxgi::Acquired::Recreated => {
                eprintln!("capture: duplication lost and rebuilt (desktop switch)");
                want_keyframe = true;
                continue;
            }
        };

        let convert_start = qpc::now();
        let nv12 = state.converter.convert(&texture)?;
        let convert_end = qpc::now();

        let time_hns = state
            .clock
            .micros(acquire_qpc.saturating_sub(start_qpc))
            .saturating_mul(10);
        let sample = encode::sample_from_texture(&nv12, time_hns, frame_duration_hns)?;

        let clock = state.clock;
        let tx = state.tx.clone();
        let sets = state.encoder.parameter_sets().cloned();
        let mut sink = |au: encode::EncodedAu| -> Result<()> {
            let (bytes, prepended) =
                annexb::ensure_parameter_sets(&au.data, sets.as_ref(), au.keyframe);
            let mut record = FrameRecord::new();
            record.frame = frame_index;
            record.present_qpc_us = clock.micros(present_qpc);
            record.acquire_qpc_us = clock.micros(acquire_qpc);
            record.convert_start_us = clock.micros(convert_start);
            record.convert_end_us = clock.micros(convert_end);
            record.encode_submit_us = clock.micros(au.submit_qpc);
            record.encode_out_us = clock.micros(au.out_qpc);
            record.au_bytes = bytes.len();
            record.keyframe = au.keyframe;
            record.param_sets_prepended = prepended;
            record.dropped_frames = dropped;
            frame_index += 1;

            match tx.try_send(send::Outbound::Frame(Box::new(record), bytes.into_owned())) {
                Ok(()) => Ok(()),
                // Full means the socket is behind. Drop the newest rather than
                // queue it: a late frame is worse than a missing one here.
                Err(TrySendError::Full(_)) => {
                    dropped += 1;
                    Ok(())
                }
                Err(TrySendError::Disconnected(_)) => {
                    Err("sender thread has gone away".to_owned().into())
                }
            }
        };

        state.encoder.encode(&sample, &mut sink)?;
    }
}
