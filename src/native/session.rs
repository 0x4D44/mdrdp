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

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ironrdp_egfx::decode::H264Decoder;
use rhydra::framing::{self, Reassembler};
use rhydra::input_proto::{MouseButton as WireButton, Record, WheelAxis, encode_record};
use rhydra::rects::{self, RectUpdate};

use crate::input::{InputEvent, MouseButton, ScrollAxis};
use crate::session::{SessionCommand, SessionEnd};
use crate::stats::StatsHandle;
use crate::surface::{Rect, SurfaceStore};
use crate::wake::{self, DoorbellReceiver};
use crate::window::Waker;

use super::probe::ProbedTransport;
use super::ssh::Tunnel;

/// The surface id the native session paints. There is only ever one.
pub const OUTPUT_SURFACE: u16 = 0;

/// The codec label the title bar and HUD show for native frames.
const CODEC_LABEL: &str = "AVC (rhydra)";

/// A running native session. `shutdown` is the only way out, mirroring
/// `session::SessionHandle`; dropping the handle without it leaks nothing —
/// the tunnel is kill-on-drop and the threads exit on the closed sockets.
pub struct NativeHandle {
    stop: Arc<AtomicBool>,
    video: TcpStream,
    input: TcpStream,
    video_join: JoinHandle<SessionEnd>,
    input_join: JoinHandle<Option<String>>,
    tunnel: Tunnel,
}

impl NativeHandle {
    /// Stop both threads, kill the tunnel, and report how the session ended.
    pub fn shutdown(mut self) -> SessionEnd {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.video.shutdown(Shutdown::Both);
        let _ = self.input.shutdown(Shutdown::Both);
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
    decoder: Option<Box<dyn H264Decoder>>,
    store: Arc<Mutex<SurfaceStore>>,
    input_rx: Receiver<InputEvent>,
    commands: Receiver<SessionCommand>,
    waker: Waker,
    stats: StatsHandle,
    wake_rx: DoorbellReceiver,
) -> std::io::Result<NativeHandle> {
    let ProbedTransport { conn, tunnel, .. } = transport;
    let stop = Arc::new(AtomicBool::new(false));

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

    let net_stop = Arc::clone(&stop);
    let net_input_sock = conn.input.try_clone()?;
    let mut sink = NativeSink::new(
        decoder,
        store,
        (conn.header.width, conn.header.height),
        Box::new(move || {
            let _ = waker.damaged();
        }),
        stats,
    );
    let mut video_sock = conn.video;
    let mut reassembler = conn.reassembler;
    let video_join = std::thread::Builder::new()
        .name("native-net".to_owned())
        .spawn(move || {
            let end = pump_video(&mut video_sock, &mut reassembler, &mut sink, &net_stop);
            // Either side's death tears the whole session down (HLD §6).
            net_stop.store(true, Ordering::Relaxed);
            let _ = net_input_sock.shutdown(Shutdown::Both);
            end
        })?;

    let input_stop = Arc::clone(&stop);
    let input_video_sock = video.try_clone()?;
    let input_sock = conn.input;
    let input_join = std::thread::Builder::new()
        .name("native-input".to_owned())
        .spawn(move || {
            let failure = pump_input(input_sock, input_rx, commands, wake_rx, &input_stop);
            input_stop.store(true, Ordering::Relaxed);
            let _ = input_video_sock.shutdown(Shutdown::Both);
            failure
        })?;

    Ok(NativeHandle {
        stop,
        video,
        input,
        video_join,
        input_join,
        tunnel,
    })
}

/// Read and dispatch framed messages until the socket closes, the wire is
/// violated, or the stop flag is raised.
fn pump_video(
    video: &mut TcpStream,
    reassembler: &mut Reassembler,
    sink: &mut NativeSink,
    stop: &AtomicBool,
) -> SessionEnd {
    let mut buf = vec![0u8; 64 * 1024];
    // Messages completed by one read are dispatched rects-first: a rect update
    // is the low-latency path and never depends on an AU in the same batch
    // (the exactness gate makes ordering safe either way).
    let mut batch: Vec<framing::Message> = Vec::new();
    loop {
        if stop.load(Ordering::Relaxed) {
            return SessionEnd::WindowClosed;
        }
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
        batch.clear();
        loop {
            match reassembler.next_message() {
                Ok(Some(m)) => batch.push(m),
                Ok(None) => break,
                Err(e) => return SessionEnd::TransportFailed(format!("framing: {e:?}")),
            }
        }
        for m in batch.iter().filter(|m| m.msg_type == framing::MSG_RECTS) {
            if let Err(reason) = sink.on_rects(&m.payload) {
                return SessionEnd::TransportFailed(reason);
            }
        }
        for m in batch.iter().filter(|m| m.msg_type != framing::MSG_RECTS) {
            let outcome = match m.msg_type {
                framing::MSG_VIDEO_SEQ => {
                    if m.payload.len() < 8 {
                        Err("MSG_VIDEO_SEQ shorter than its sequence prefix".to_owned())
                    } else {
                        let seq = u64::from_le_bytes(m.payload[..8].try_into().expect("8 bytes"));
                        sink.on_au(&m.payload[8..], Some(seq))
                    }
                }
                framing::MSG_VIDEO => sink.on_au(&m.payload, None),
                framing::MSG_STATS => Ok(()), // per-frame server stats: not consumed yet
                _ => Ok(()),                  // unknown types skip by design
            };
            if let Err(reason) = outcome {
                return SessionEnd::TransportFailed(reason);
            }
        }
    }
}

/// Drain the window's input events onto the wire; watch the socket for EOF.
/// Returns `Some(reason)` on a failure worth reporting, `None` on a clean stop.
fn pump_input(
    sock: TcpStream,
    input_rx: Receiver<InputEvent>,
    commands: Receiver<SessionCommand>,
    wake_rx: DoorbellReceiver,
    stop: &AtomicBool,
) -> Option<String> {
    let mut writer = match sock.try_clone() {
        Ok(w) => w,
        Err(e) => return Some(format!("input socket: {e}")),
    };
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
    loop {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        let ready = match wake::wait_readable(&sock, &wake_rx, Duration::from_millis(250)) {
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
        loop {
            match input_rx.try_recv() {
                Ok(event) => {
                    for record in wire_records(&event, &mut seq) {
                        let (bytes, len) = encode_record(record);
                        if let Err(e) = writer.write_all(&bytes[..len]) {
                            return if stop.load(Ordering::Relaxed) {
                                None
                            } else {
                                Some(format!("input write: {e}"))
                            };
                        }
                        written.0 += 1;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return None,
            }
        }
        loop {
            match commands.try_recv() {
                Ok(SessionCommand::Resize { .. }) => {
                    // MVP: the host owns its resolution; the window letterboxes.
                    if !resize_noted {
                        resize_noted = true;
                        eprintln!("native: resize requests are letterbox-only in this build");
                    }
                }
                Ok(SessionCommand::SetVisibility { .. }) => {} // no suppress notion yet
                Err(_) => break,
            }
        }
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
        InputEvent::MouseButton { button, down, .. } => vec![Record::MouseButton {
            button: match button {
                MouseButton::Left => WireButton::Left,
                MouseButton::Right => WireButton::Right,
                MouseButton::Middle => WireButton::Middle,
                MouseButton::X1 => WireButton::X1,
                MouseButton::X2 => WireButton::X2,
            },
            down,
            seq: next(),
        }],
        InputEvent::Scroll { axis, units, .. } => vec![Record::Wheel {
            axis: match axis {
                ScrollAxis::Vertical => WheelAxis::Vertical,
                ScrollAxis::Horizontal => WheelAxis::Horizontal,
            },
            delta120: units,
            seq: next(),
        }],
    }
}

/// A rect update held because its seq is ahead of the canvas's exactness.
struct PendingRects {
    update: RectUpdate,
}

/// The decode-and-composite state: the viewer's exactness machinery, writing
/// into the shared store instead of a private canvas.
pub(crate) struct NativeSink {
    decoder: Option<Box<dyn H264Decoder>>,
    store: Arc<Mutex<SurfaceStore>>,
    wire_size: (u32, u32),
    /// The capture seq surface 0 is exact through. `None` until the first AU
    /// lands (rects before a base frame have nothing to composite onto).
    exact_through: Option<u64>,
    has_base: bool,
    pending: Option<PendingRects>,
    wake: Box<dyn Fn() + Send>,
    stats: StatsHandle,
    // Visible-behaviour counters (tests and debugging; not user-facing yet).
    pub(crate) suppressed: u64,
    pub(crate) skipped_stale: u64,
    pub(crate) skipped_empty: u64,
    pub(crate) skipped_before_base: u64,
    pub(crate) held: u64,
}

impl NativeSink {
    pub(crate) fn new(
        decoder: Option<Box<dyn H264Decoder>>,
        store: Arc<Mutex<SurfaceStore>>,
        wire_size: (u32, u32),
        wake: Box<dyn Fn() + Send>,
        stats: StatsHandle,
    ) -> Self {
        Self {
            decoder,
            store,
            wire_size,
            exact_through: None,
            has_base: false,
            pending: None,
            wake,
            stats,
            suppressed: 0,
            skipped_stale: 0,
            skipped_empty: 0,
            skipped_before_base: 0,
            held: 0,
        }
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
            self.stats.update(|s| {
                s.frames += 1;
                s.bytes_in += bytes;
                s.decode.record(decode_us);
                *s.codec_painted.entry(CODEC_LABEL.to_owned()).or_insert(0) += bytes;
                s.mark_painted(generation);
            });
        }
        self.exact_through = seq;
        self.has_base = true;
        (self.wake)();
        self.try_apply_pending();
        Ok(())
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
        let mut painted_bytes: u64 = 0;
        {
            let mut store = self
                .store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for r in &update.rects {
                let dest = Rect::new(r.x, r.y, r.x.saturating_add(r.w), r.y.saturating_add(r.h));
                store
                    .blit_bgra_strict(OUTPUT_SURFACE, dest, &r.pixels)
                    .map_err(|e| format!("rect blit: {e}"))?;
                painted_bytes += r.pixels.len() as u64;
            }
            let generation = store.generation();
            self.stats.update(|s| {
                s.bytes_in += painted_bytes;
                *s.codec_painted.entry(CODEC_LABEL.to_owned()).or_insert(0) += painted_bytes;
                s.mark_painted(generation);
            });
        }
        self.exact_through = Some(update.frame_seq);
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
    use super::*;
    use ironrdp_egfx::decode::{DecodedFrame, DecoderResult, H264Decoder};
    use rhydra::input_proto::decode_record;
    use rhydra::rects::Rect as WireRect;

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

    fn sink_with_store(size: (u32, u32)) -> (NativeSink, Arc<Mutex<SurfaceStore>>) {
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
        );
        (sink, store)
    }

    fn surface_fill(store: &Arc<Mutex<SurfaceStore>>) -> u8 {
        let guard = store.lock().unwrap();
        guard.get(OUTPUT_SURFACE).unwrap().pixels()[0]
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
        let (bytes, len) = encode_record(recs[0]);
        match decode_record(&bytes[..len]).unwrap() {
            Record::Wheel {
                axis,
                delta120,
                seq: s,
            } => {
                assert_eq!(axis, WheelAxis::Horizontal);
                assert_eq!(delta120, -120);
                assert_eq!(s, 2);
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
        let (bytes, len) = encode_record(recs[0]);
        match decode_record(&bytes[..len]).unwrap() {
            Record::MouseButton {
                button,
                down,
                seq: s,
            } => {
                assert_eq!(button, WireButton::Right);
                assert!(!down);
                assert_eq!(s, 3);
            }
            other => panic!("expected MouseButton, got {other:?}"),
        }
    }
}
