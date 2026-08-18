//! Decode, and hand the newest frame to the window thread.
//!
//! The decoder is `mdrdp::h264::hardware_decoder()` (`src/h264.rs:35`) driven through
//! `H264Decoder::decode` — the exact call an mdrdp session makes for an AVC420 stream
//! (`vendor/ironrdp-egfx/src/client.rs:841`), returning RGBA8 at the coded size. Using
//! `decode` rather than `decode_yuv420` is deliberate: the spike stream is a single
//! 4:2:0 sub-stream, so there is no AVC444 luma/chroma pair to combine, and `decode`
//! is the same function plus mdrdp's own YUV→RGBA conversion — the conversion the
//! presenter downstream expects.
//!
//! A failed decode skips one frame and keeps the connection, mirroring the mdrdp patch
//! at that call site: the server's first access units can reach us before the parameter
//! sets, and the stream heals itself at the next keyframe. A run of errors that never
//! stops is the real signal, so every one is recorded.
//!
//! No window is touched here, which is what lets `tests/loopback.rs` drive this whole
//! path headless.

use std::sync::{Arc, Mutex};

use ironrdp_egfx::decode::H264Decoder;
use rhydra::{annexb, rects};

use crate::clock::Clock;
use crate::net::MessageSink;
use crate::stats::{DecodeErrorRecord, FrameRecord, FrameStamps, RectRecord, RectStamps, StatsLog};

/// Which message painted the snapshot in a [`Frame`], and its stamps.
///
/// Both paths publish through the same slot, and both owe the stats file a line that
/// only the presenter can close — but they are different records, because the rect
/// path has no decode stage and the AU path has no rect count.
#[derive(Debug, Clone, Copy)]
pub enum PaintStamps {
    Au(FrameStamps),
    Rects(RectStamps),
}

/// A rectangle of the canvas, in canvas (= frame) pixel coordinates.
///
/// Deliberately not `rects::Rect`: that one carries the wire's pixel payload and
/// `u16` fields, and the window thread wants neither. This is geometry only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DamageRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// What a published snapshot changed since the window thread last saw the canvas.
///
/// It rides the slot because only the publisher knows it, and only the presenter can
/// use it: the window thread keeps a persistent converted canvas and re-converts just
/// the damage (`crate::present::present_region_into`) instead of the whole surface.
///
/// `Full` is the honest answer for a decoded access unit — H.264 says nothing about
/// which pixels changed — and also the safe answer for anything this type cannot
/// describe cheaply, because a full convert is always correct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Damage {
    Full,
    Rects(Vec<DamageRect>),
}

/// Past this many rects the union collapses to `Full`: a bounded list keeps the
/// per-publish memory fixed, and converting that many small regions costs more than
/// the one sequential pass over the whole canvas it is trying to avoid.
const MAX_DAMAGE_RECTS: usize = 64;

impl Damage {
    /// Fold another snapshot's coverage into this one.
    ///
    /// **The displacement invariant.** A publish that displaces a snapshot the window
    /// thread never took must carry that snapshot's damage as well as its own:
    /// otherwise the displaced frame's pixels are painted into the persistent canvas
    /// by nobody, and every later partial present ships a stale region — silently,
    /// and forever, since nothing re-damages an area that stopped changing.
    ///
    /// Coverage is a set, so this is order-free: `Full` absorbs anything, and two
    /// rect lists concatenate (overlaps are converted twice, which is correct and
    /// costs only time).
    pub fn absorb(&mut self, other: &Damage) {
        match (&mut *self, other) {
            (Damage::Full, _) => {}
            (_, Damage::Full) => *self = Damage::Full,
            (Damage::Rects(mine), Damage::Rects(theirs)) => {
                if mine.len() + theirs.len() > MAX_DAMAGE_RECTS {
                    *self = Damage::Full;
                } else {
                    mine.extend_from_slice(theirs);
                }
            }
        }
    }
}

/// One canvas snapshot, ready to present.
#[derive(Debug, Clone)]
pub struct Frame {
    /// RGBA8, tightly packed, `width` pixels per row — `present_into`'s `src` contract.
    ///
    /// Shared with the canvas rather than cloned: the AU path publishes at up to the
    /// full frame rate, and an 8 MB copy per frame would be a new cost the archived
    /// pre-canvas baselines never paid. The sharing is copy-on-write — see
    /// [`Canvas::apply_rects`].
    pub rgba: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    pub stamps: PaintStamps,
    /// What changed since the window thread last took a snapshot — including the
    /// damage of any snapshot displaced before it was taken. See [`Damage::absorb`].
    pub damage: Damage,
    /// This frame still owes the stats file a line. Cleared once presented, so a
    /// repaint that shows the same frame again (a resize, an expose) does not emit a
    /// second line for it.
    pub stamps_pending: bool,
}

/// The persistent RGBA framebuffer both paint paths write into.
///
/// It exists because the two paths are not independent: a `MSG_RECTS` update paints
/// part of the picture and the decoded access unit paints all of it, so there has to
/// be one surface that remembers what the other one did. The canvas lives on the
/// network/decode thread; each mutation publishes an [`Arc`] snapshot through
/// [`FrameSlot`] (copy-on-write, so the AU path stays as move-cheap as it was before
/// the canvas existed).
///
/// **The ordering invariant.** `exact_through = Some(e)` means the canvas provably
/// holds *all* content up to and including capture frame `e`. That is what makes a
/// suppression decision safe: an AU is redundant only when everything it shows is
/// already on the canvas. The server assigns capture seqs contiguously (every
/// captured frame, rects or not, takes the next number), which is what lets rect
/// updates extend exactness one frame at a time:
///
/// * a decoded AU repaints the whole desktop, so accepting one sets
///   `exact_through` to its seq outright;
/// * a rect update for frame `e + 1` carries that frame's complete dirty set
///   (the server only sends rects when the duplication's metadata was whole), so
///   applying it advances `e` by one;
/// * a rect update for any other seq must NOT paint: ahead of `e + 1` means some
///   frame's content never reached this canvas (an AU that failed the predicate
///   and is still in flight, or was dropped), and painting newer partial content
///   while refusing the older full frame would lose that frame forever — the exact
///   defect the first review caught. Skipping is cheap: the AU path repaints
///   everything within a frame or two.
#[derive(Debug, Clone)]
pub struct Canvas {
    rgba: Arc<Vec<u8>>,
    width: u32,
    height: u32,
    /// The capture seq the canvas is exact through. `None` on a legacy v1 stream
    /// (bare `MSG_VIDEO`, no sequence domain) — rect updates never apply there,
    /// which is moot: a v1 server has no `MSG_RECTS` to send.
    exact_through: Option<u64>,
}

/// What [`Canvas::rects_skip_reason`] decided about a rect update's seq.
const SKIP_STALE: &str = "stale";
const SKIP_GAP: &str = "gap";

impl Canvas {
    /// Start a canvas from a fully decoded picture: exact through that frame.
    pub fn from_frame(rgba: Arc<Vec<u8>>, width: u32, height: u32, seq: Option<u64>) -> Self {
        Self {
            rgba,
            width,
            height,
            exact_through: seq,
        }
    }

    /// Replace the whole canvas with a newer decoded picture.
    pub fn set_frame(&mut self, rgba: Arc<Vec<u8>>, seq: Option<u64>) {
        self.rgba = rgba;
        self.exact_through = seq;
    }

    /// May a decoded access unit with this sequence number replace the canvas?
    ///
    /// `None` is a legacy v1 stream with no sequence domain, so every AU paints.
    /// Otherwise: accept exactly when the AU shows something the canvas cannot
    /// already prove it holds. An AU at or below `exact_through` is genuinely
    /// redundant — not merely "older than the last rect", which was the first cut
    /// of this rule and suppressed frames whose content had never arrived at all.
    pub fn accepts_au(&self, seq: Option<u64>) -> bool {
        match (seq, self.exact_through) {
            (None, _) => true,
            (Some(_), None) => true,
            (Some(s), Some(e)) => s > e,
        }
    }

    /// Why a rect update with this seq must be skipped, or `None` when it may
    /// paint. Only `exact_through + 1` paints — see the type-level invariant.
    pub fn rects_skip_reason(&self, seq: u64) -> Option<&'static str> {
        match self.exact_through {
            Some(e) if seq == e + 1 => None,
            // At or below `e`: an AU (or these very rects) already covered it.
            Some(e) if seq <= e => Some(SKIP_STALE),
            // Beyond `e + 1` (or no seq domain yet): some frame's content is
            // missing between the canvas and these rects.
            _ => Some(SKIP_GAP),
        }
    }

    /// Blit every rect of `update` into the canvas, swizzling BGRA to RGBA, and
    /// advance `exact_through` to the update's frame.
    ///
    /// The caller has already checked the seq ([`Canvas::rects_skip_reason`]) and
    /// that the update's declared frame size matches this canvas;
    /// [`rects::blit_bgra_to_rgba`] re-checks against the canvas itself, because a
    /// write outside it must be impossible whatever the caller did.
    ///
    /// `Arc::make_mut` is the copy-on-write: the buffer is cloned only when the
    /// last published snapshot is still alive in the slot or the window — one copy
    /// per rect update at typing cadence, instead of one per decoded frame.
    pub fn apply_rects(&mut self, update: &rects::RectUpdate) -> Result<(), rects::RectsError> {
        let rgba = Arc::make_mut(&mut self.rgba);
        for r in &update.rects {
            rects::blit_bgra_to_rgba(r, rgba, self.width, self.height)?;
        }
        self.exact_through = Some(update.frame_seq);
        Ok(())
    }
}

/// The handoff between the decode thread and the window thread: room for exactly one
/// frame, and the newest wins.
///
/// A queue would be the wrong shape. A frame waiting behind another frame is a stale
/// frame, and presenting it would make the measured latency look better than the
/// pipeline actually is. Displacing an undisplayed frame is recorded as a drop
/// (`present_done_us: null`), never silently swallowed.
#[derive(Debug, Default)]
pub struct FrameSlot {
    latest: Mutex<Option<Frame>>,
}

impl FrameSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install the newest frame, returning the one it displaced, if any.
    ///
    /// The displaced frame's damage is folded into the new one *here*, under the same
    /// lock that hands the slot to the window thread — the only place where "is there
    /// still an untaken frame?" can be answered without racing the taker. The
    /// displaced frame is returned only so its stats line can be closed; its own
    /// damage now belongs to the frame that replaced it.
    pub fn put(&self, mut frame: Frame) -> Option<Frame> {
        let mut guard = self
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(displaced) = guard.as_ref() {
            frame.damage.absorb(&displaced.damage);
        }
        guard.replace(frame)
    }

    /// Take the pending frame, if one arrived since the last call.
    pub fn take(&self) -> Option<Frame> {
        let mut guard = self
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.take()
    }
}

/// Decodes video messages and composites rect updates into [`FrameSlot`], and writes
/// the client stats lines.
pub struct DecodeSink {
    decoder: Option<Box<dyn H264Decoder>>,
    slot: Arc<FrameSlot>,
    stats: Arc<StatsLog>,
    clock: Clock,
    /// Wakes the window thread. A closure rather than an `EventLoopProxy` so this
    /// module has no winit dependency and stays testable without an event loop.
    wake: Box<dyn Fn() + Send>,
    frames: u64,
    /// `None` until the first access unit decodes: rects have nothing to composite
    /// onto before that.
    canvas: Option<Canvas>,
    /// A rect update one frame (or more) ahead of the canvas's exactness, held
    /// until the AU that closes the gap arrives. One slot, newest wins.
    ///
    /// Without this the strict adjacency gate is self-wedging (re-review's
    /// finding): the encoder runs a frame behind capture, so after one predicate
    /// miss every later rect update arrives exactly one frame ahead of exactness
    /// and would be skipped — at rect cadence, forever. Holding the update and
    /// applying it the moment adjacency is restored keeps the invariant (nothing
    /// ever paints over a hole) and restores the fast path one frame after the
    /// miss. Resolution happens in [`DecodeSink::try_apply_pending`]: applied
    /// when adjacent, expired `stale` when an AU jumps past it, dropped on a
    /// canvas rebase whose size it no longer matches.
    pending_rects: Option<PendingRects>,
    /// One warning per run for mismatched rect geometry, not one per update.
    warned_size_mismatch: bool,
}

/// A held rect update and the receive stamp its eventual record must carry.
struct PendingRects {
    update: rects::RectUpdate,
    recv_done_us: u64,
}

impl DecodeSink {
    pub fn new(
        decoder: Option<Box<dyn H264Decoder>>,
        slot: Arc<FrameSlot>,
        stats: Arc<StatsLog>,
        wake: Box<dyn Fn() + Send>,
    ) -> Self {
        Self {
            decoder,
            slot,
            stats,
            clock: Clock::new(),
            wake,
            frames: 0,
            canvas: None,
            pending_rects: None,
            warned_size_mismatch: false,
        }
    }

    /// Frames seen so far, decoded or not.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Copy the canvas into the slot and wake the window thread.
    ///
    /// A snapshot it displaced was never shown, so its own record is closed here with
    /// no present stamp — whichever message painted it. Its *damage* is not discarded
    /// with it: [`FrameSlot::put`] merges it into this snapshot.
    fn publish(&self, stamps: PaintStamps, damage: Damage) {
        let canvas = self
            .canvas
            .as_ref()
            .expect("publish is only reached once a canvas exists");
        let displaced = self.slot.put(Frame {
            // An Arc clone: the pixels are shared, not copied. The canvas's next
            // rect blit is the copy-on-write point.
            rgba: Arc::clone(&canvas.rgba),
            width: canvas.width,
            height: canvas.height,
            stamps,
            damage,
            stamps_pending: true,
        });
        if let Some(old) = displaced {
            self.record_unpresented(&old.stamps);
        }
        (self.wake)();
    }

    /// Close the record of a snapshot that never reached the window.
    fn record_unpresented(&self, stamps: &PaintStamps) {
        // `partial: false`: no convert ran for a snapshot that never reached the
        // window thread, so neither present path can claim it.
        match stamps {
            PaintStamps::Au(s) => self.stats.record(&FrameRecord::new(s, None, false)),
            PaintStamps::Rects(s) => self.stats.record(&RectRecord::painted(s, None, false)),
        }
    }

    /// Blit an update that has already passed every gate, stamp it, publish it.
    fn paint_rects(
        &mut self,
        update: &rects::RectUpdate,
        recv_done_us: u64,
    ) -> Result<(), rects::RectsError> {
        self.canvas
            .as_mut()
            .expect("painting is gated on a live canvas")
            .apply_rects(update)?;
        let paint_done_us = self.clock.now_us();
        // Exactly the rectangles just blitted, in the coordinates
        // `blit_bgra_to_rgba` used — the window thread converts these and nothing else.
        let damage = Damage::Rects(
            update
                .rects
                .iter()
                .map(|r| DamageRect {
                    x: u32::from(r.x),
                    y: u32::from(r.y),
                    w: u32::from(r.w),
                    h: u32::from(r.h),
                })
                .collect(),
        );
        self.publish(
            PaintStamps::Rects(RectStamps {
                seq: update.frame_seq,
                recv_done_us,
                paint_done_us,
                rect_count: update.rects.len() as u32,
                rect_bytes: update.rects.iter().map(|r| r.pixels.len()).sum(),
            }),
            damage,
        );
        Ok(())
    }

    /// One `MSG_RECTS`, one line: the visible counter for an update that painted
    /// nothing.
    fn record_rect_skip(
        &self,
        update: &rects::RectUpdate,
        recv_done_us: u64,
        reason: &'static str,
    ) {
        self.stats.record(&RectRecord::skipped(
            update.frame_seq,
            recv_done_us,
            update.rects.len() as u32,
            update.rects.iter().map(|r| r.pixels.len()).sum(),
            reason,
        ));
    }

    /// Resolve the held rect update, if the canvas has moved far enough.
    ///
    /// Called after every exactness advance (an accepted AU, a rebase, a painted
    /// rect update). Applied when now adjacent; expired `stale` when the canvas
    /// is already past it; dropped `size_mismatch` after a rebase it no longer
    /// fits; kept while the gap is still open. No timer: an active desktop keeps
    /// producing AUs, an idle one drains the encoder's tail through the server's
    /// pump, and a wire-dropped AU forces a keyframe whose seq expires the hold —
    /// every path resolves it.
    fn try_apply_pending(&mut self) {
        let Some((canvas_w, canvas_h)) = self.canvas.as_ref().map(|c| (c.width, c.height)) else {
            return;
        };
        let Some((p_seq, p_w, p_h)) = self.pending_rects.as_ref().map(|p| {
            (
                p.update.frame_seq,
                p.update.frame_width,
                p.update.frame_height,
            )
        }) else {
            return;
        };
        if (p_w, p_h) != (canvas_w, canvas_h) {
            let p = self.pending_rects.take().expect("checked just above");
            self.record_rect_skip(&p.update, p.recv_done_us, "size_mismatch");
            return;
        }
        match self
            .canvas
            .as_ref()
            .expect("checked just above")
            .rects_skip_reason(p_seq)
        {
            None => {
                let p = self.pending_rects.take().expect("checked just above");
                if let Err(e) = self.paint_rects(&p.update, p.recv_done_us) {
                    // Unreachable after decode's own bounds and the dims check;
                    // sizes and reasons only, never pixel contents.
                    eprintln!("rects: held update failed to blit: {e}");
                    self.record_rect_skip(&p.update, p.recv_done_us, "blit_error");
                }
            }
            Some(SKIP_STALE) => {
                let p = self.pending_rects.take().expect("checked just above");
                self.record_rect_skip(&p.update, p.recv_done_us, SKIP_STALE);
            }
            // The gap is still open; keep holding.
            Some(_) => {}
        }
    }

    fn note_decode_failure(
        &self,
        frame: u64,
        recv_done_us: u64,
        au: &[u8],
        keyframe: bool,
        detail: String,
    ) {
        // Sizes and flags only — never a byte of the payload. The stream carries the
        // remote desktop's contents.
        eprintln!(
            "decode: frame {frame} refused ({} bytes, keyframe={keyframe}): {detail}",
            au.len()
        );
        self.stats.record(&DecodeErrorRecord::new(
            frame,
            recv_done_us,
            au.len(),
            keyframe,
            detail,
        ));
    }
}

impl MessageSink for DecodeSink {
    fn on_video(&mut self, au: &[u8], seq: Option<u64>, recv_done_us: u64) {
        self.frames += 1;
        let frame = self.frames;
        // The server's own keyframe flag rides its stats line, not the video message,
        // so it is re-derived here from the access unit itself — with the server's
        // module, not a second scanner.
        let keyframe = annexb::contains_idr(au);

        let Some(decoder) = self.decoder.as_mut() else {
            self.note_decode_failure(
                frame,
                recv_done_us,
                au,
                keyframe,
                "this build has no hardware H.264 decoder".to_owned(),
            );
            return;
        };

        let decode_in_us = self.clock.now_us();
        let decoded = decoder.decode(au);
        let decode_out_us = self.clock.now_us();

        let decoded = match decoded {
            Ok(d) => d,
            Err(e) => {
                self.note_decode_failure(frame, recv_done_us, au, keyframe, e.to_string());
                return;
            }
        };

        let (width, height) = (decoded.width(), decoded.height());
        let stamps = FrameStamps {
            frame,
            seq,
            recv_done_us,
            decode_in_us,
            decode_out_us,
            au_bytes: au.len(),
            keyframe,
            width,
            height,
        };

        // A canvas of a different size is not the same desktop — a mode change makes
        // whatever it held meaningless, including any exactness recorded against it.
        let rebase = match &self.canvas {
            Some(c) => c.width != width || c.height != height,
            None => true,
        };
        if rebase {
            self.canvas = Some(Canvas::from_frame(
                Arc::new(decoded.into_data()),
                width,
                height,
                seq,
            ));
        } else {
            let canvas = self.canvas.as_mut().expect("checked just above");
            if !canvas.accepts_au(seq) {
                // Everything this AU shows is provably on the canvas already
                // (`exact_through` ≥ its seq). The decoder has run, so its state has
                // advanced; nothing visible changes. Recorded, not dropped: see
                // `FrameRecord::suppressed`.
                self.stats.record(&FrameRecord::suppressed(&stamps));
                return;
            }
            canvas.set_frame(Arc::new(decoded.into_data()), seq);
        }
        // A decoded access unit repaints the whole desktop and the bitstream says
        // nothing about which pixels moved, so the only true damage is everything.
        self.publish(PaintStamps::Au(stamps), Damage::Full);
        // The AU may have closed the gap a held rect update was waiting on.
        self.try_apply_pending();
    }

    fn on_rects(&mut self, payload: &[u8], recv_done_us: u64) -> Result<(), String> {
        // Terminal on the caller's side: a payload whose own lengths do not add up is
        // as unrecoverable as a framing error.
        let update = rects::decode(payload).map_err(|e| e.to_string())?;

        // The parser tolerates an empty update, but painting nothing must not
        // advance `exact_through` — that would claim a frame's content on the word
        // of a message that carried none. This server never emits one; skip it
        // visibly rather than trusting it.
        if update.rects.is_empty() {
            self.record_rect_skip(&update, recv_done_us, "empty");
            return Ok(());
        }

        let Some((canvas_width, canvas_height)) = self.canvas.as_ref().map(|c| (c.width, c.height))
        else {
            // Rects can legitimately beat the first decodable keyframe onto the wire.
            // There is nothing to composite onto, so drop them — and count it.
            self.record_rect_skip(&update, recv_done_us, "before_base");
            return Ok(());
        };

        if update.frame_width != canvas_width || update.frame_height != canvas_height {
            if !self.warned_size_mismatch {
                self.warned_size_mismatch = true;
                // Sizes only — never a byte of the payload. This stream carries the
                // remote desktop's contents.
                eprintln!(
                    "rects: update declares {}x{} but the canvas is {canvas_width}x{canvas_height}; skipping",
                    update.frame_width, update.frame_height
                );
            }
            self.record_rect_skip(&update, recv_done_us, "size_mismatch");
            return Ok(());
        }

        // The ordering gate: only the frame adjacent to the canvas's exactness may
        // paint — a gap means some frame's content never reached this canvas (its
        // AU is in flight or was dropped), and painting over the hole would lose it
        // (the first review's critical finding). But a gap update is *held*, not
        // discarded: the encoder runs a frame behind capture, so after one
        // predicate miss every later update would otherwise arrive exactly one
        // frame ahead of exactness and be skipped forever (the re-review's
        // finding). It applies the moment the AU closes the gap.
        match self
            .canvas
            .as_ref()
            .expect("checked just above")
            .rects_skip_reason(update.frame_seq)
        {
            None => {}
            Some(SKIP_STALE) => {
                self.record_rect_skip(&update, recv_done_us, SKIP_STALE);
                return Ok(());
            }
            Some(_) => {
                // Hold it, newest wins; the displaced older hold gets its line.
                match self.pending_rects.take() {
                    Some(old) if old.update.frame_seq >= update.frame_seq => {
                        self.record_rect_skip(&update, recv_done_us, SKIP_GAP);
                        self.pending_rects = Some(old);
                    }
                    old => {
                        if let Some(old) = old {
                            self.record_rect_skip(&old.update, old.recv_done_us, SKIP_GAP);
                        }
                        self.pending_rects = Some(PendingRects {
                            update,
                            recv_done_us,
                        });
                    }
                }
                return Ok(());
            }
        }

        // A blit failure is unreachable in practice: `decode` has already bounded
        // every rect by the update's declared frame size, and that size equals the
        // canvas's. Returned rather than ignored because the alternative to a
        // check that cannot fail is a check that silently stops holding.
        self.paint_rects(&update, recv_done_us)
            .map_err(|e| e.to_string())?;
        // Painting advanced exactness; a held update may now be adjacent.
        self.try_apply_pending();
        Ok(())
    }

    fn on_stats(&mut self, payload: &[u8]) {
        self.stats.write_line(&crate::stats::server_line(payload));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhydra::rects::{encode, Rect, RectUpdate};
    use serde_json::Value;

    fn stamps(frame: u64) -> FrameStamps {
        FrameStamps {
            frame,
            seq: Some(frame + 100),
            recv_done_us: frame * 10,
            decode_in_us: frame * 10 + 1,
            decode_out_us: frame * 10 + 2,
            au_bytes: frame as usize,
            keyframe: false,
            width: 4,
            height: 2,
        }
    }

    fn frame(n: u64) -> Frame {
        Frame {
            rgba: Arc::new(vec![n as u8; 4 * 2 * 4]),
            width: 4,
            height: 2,
            stamps: PaintStamps::Au(stamps(n)),
            damage: Damage::Full,
            stamps_pending: true,
        }
    }

    /// A frame carrying rect damage, with `n` distinguishing both the stamps and the
    /// rectangle — so a union that returned the wrong operand cannot pass.
    fn rect_damaged_frame(n: u64, damage: Vec<DamageRect>) -> Frame {
        Frame {
            damage: Damage::Rects(damage),
            ..frame(n)
        }
    }

    fn damage_rect(seed: u32) -> DamageRect {
        DamageRect {
            x: seed,
            y: seed * 2,
            w: seed + 1,
            h: seed + 3,
        }
    }

    /// The AU ordinal of a frame, for tests that only care which one came back.
    fn au_frame_number(f: &Frame) -> u64 {
        match f.stamps {
            PaintStamps::Au(s) => s.frame,
            PaintStamps::Rects(s) => {
                panic!("expected an AU-painted frame, got rects seq {}", s.seq)
            }
        }
    }

    #[test]
    fn the_slot_hands_back_the_newest_frame_and_empties() {
        let slot = FrameSlot::new();
        assert!(slot.take().is_none(), "empty to start");
        assert!(slot.put(frame(1)).is_none(), "nothing displaced");
        let got = slot.take().expect("a frame");
        assert_eq!(au_frame_number(&got), 1);
        assert!(slot.take().is_none(), "taking empties the slot");
    }

    #[test]
    fn a_second_frame_displaces_the_first_and_the_displaced_one_is_returned() {
        // Frame numbers distinct, so "returned the displaced one" is distinguishable
        // from "returned the one just put".
        let slot = FrameSlot::new();
        slot.put(frame(1));
        let displaced = slot.put(frame(2)).expect("the first frame was displaced");
        assert_eq!(au_frame_number(&displaced), 1);
        assert_eq!(au_frame_number(&slot.take().unwrap()), 2, "the newest wins");
    }

    // ---- Damage, and the union that survives displacement ----

    #[test]
    fn damage_passes_through_the_slot_unchanged_when_nothing_is_displaced() {
        let slot = FrameSlot::new();
        let mine = vec![damage_rect(1), damage_rect(2)];
        assert!(
            slot.put(rect_damaged_frame(1, mine.clone())).is_none(),
            "nothing displaced"
        );
        assert_eq!(
            slot.take().expect("a frame").damage,
            Damage::Rects(mine),
            "an untouched publish must not gain or lose damage"
        );
    }

    #[test]
    fn a_displaced_frames_damage_rides_along_with_the_frame_that_displaced_it() {
        // The invariant the persistent canvas depends on: nobody else will ever paint
        // the displaced frame's pixels, so its damage has to reach the window thread
        // on the back of the frame that replaced it — or that region stays stale on
        // screen forever.
        let slot = FrameSlot::new();
        slot.put(rect_damaged_frame(1, vec![damage_rect(1)]));
        slot.put(rect_damaged_frame(2, vec![damage_rect(2)]));

        let taken = slot.take().expect("a frame");
        assert_eq!(
            au_frame_number(&taken),
            2,
            "the newest frame is the one kept"
        );
        let Damage::Rects(rects) = taken.damage else {
            panic!("two rect publishes union to rects, not Full");
        };
        assert!(
            rects.contains(&damage_rect(1)),
            "the displaced frame's rect must survive: {rects:?}"
        );
        assert!(
            rects.contains(&damage_rect(2)),
            "and so must the surviving frame's own: {rects:?}"
        );
        assert_eq!(rects.len(), 2, "coverage is a set union, not a replacement");
    }

    #[test]
    fn full_damage_absorbs_rect_damage_in_either_order() {
        // A full repaint's coverage is everything, so a union with it is everything —
        // whichever side of the displacement it landed on.
        let displaced_full = FrameSlot::new();
        displaced_full.put(frame(1)); // Damage::Full
        displaced_full.put(rect_damaged_frame(2, vec![damage_rect(3)]));
        assert_eq!(
            displaced_full.take().expect("a frame").damage,
            Damage::Full,
            "a displaced full repaint is not narrowed to the newer frame's rects"
        );

        let displacing_full = FrameSlot::new();
        displacing_full.put(rect_damaged_frame(1, vec![damage_rect(3)]));
        displacing_full.put(frame(2)); // Damage::Full
        assert_eq!(
            displacing_full.take().expect("a frame").damage,
            Damage::Full,
            "a full repaint already covers the displaced rects"
        );
    }

    #[test]
    fn a_union_past_the_rect_cap_collapses_to_a_full_repaint() {
        // Bounded memory, and past the cap converting the rects one at a time costs
        // more than the single sequential pass a full convert makes.
        let at_cap = FrameSlot::new();
        at_cap.put(rect_damaged_frame(
            1,
            (0..40).map(damage_rect).collect::<Vec<_>>(),
        ));
        at_cap.put(rect_damaged_frame(
            2,
            (40..64).map(damage_rect).collect::<Vec<_>>(),
        ));
        match at_cap.take().expect("a frame").damage {
            Damage::Rects(rects) => assert_eq!(rects.len(), MAX_DAMAGE_RECTS, "exactly at the cap"),
            Damage::Full => panic!("{MAX_DAMAGE_RECTS} rects is not past the cap"),
        }

        let over_cap = FrameSlot::new();
        over_cap.put(rect_damaged_frame(
            1,
            (0..40).map(damage_rect).collect::<Vec<_>>(),
        ));
        over_cap.put(rect_damaged_frame(
            2,
            (40..65).map(damage_rect).collect::<Vec<_>>(),
        ));
        assert_eq!(
            over_cap.take().expect("a frame").damage,
            Damage::Full,
            "one rect past the cap collapses, and Full is always correct"
        );
    }

    // ---- Canvas: the ordering rule and the blit, with no decoder in sight ----

    /// An 8x4 RGBA canvas filled with a value the blit can never produce (alpha
    /// 0x11, which the blit always overwrites with 0xFF), so "untouched" is
    /// provable. Exact through `e`, as if frame `e`'s AU had just painted it.
    fn canvas_8x4(e: u64) -> Canvas {
        Canvas::from_frame(Arc::new(vec![0x11u8; 8 * 4 * 4]), 8, 4, Some(e))
    }

    /// A rect whose every byte differs, so a swapped channel or a wrong offset shows
    /// as a wrong value rather than an accidental match.
    fn rect(x: u16, y: u16, w: u16, h: u16, seed: u8) -> Rect {
        Rect {
            x,
            y,
            w,
            h,
            pixels: (0..w as usize * h as usize * 4)
                .map(|i| seed.wrapping_add(i as u8).wrapping_mul(7).wrapping_add(3))
                .collect(),
        }
    }

    fn update(frame_seq: u64, frame_width: u32, frame_height: u32, rects: Vec<Rect>) -> RectUpdate {
        RectUpdate {
            frame_seq,
            frame_width,
            frame_height,
            rects,
        }
    }

    fn payload(u: &RectUpdate) -> Vec<u8> {
        let mut v = Vec::new();
        encode(u, &mut v);
        v
    }

    #[test]
    fn an_au_is_accepted_only_when_its_seq_is_past_the_canvas_exactness() {
        let mut canvas = canvas_8x4(6);
        canvas
            .apply_rects(&update(7, 8, 4, vec![rect(0, 0, 2, 1, 0x20)]))
            .expect("adjacent, inside the canvas");
        assert!(
            !canvas.accepts_au(Some(7)),
            "seq 7 is the frame the rects painted, so it adds nothing"
        );
        assert!(
            !canvas.accepts_au(Some(6)),
            "the canvas is exact through 6 already"
        );
        assert!(canvas.accepts_au(Some(8)), "a newer AU carries new content");
        assert!(
            canvas.accepts_au(None),
            "a v1 stream has no ordering domain to lose to"
        );
    }

    #[test]
    fn only_the_frame_adjacent_to_the_canvas_exactness_may_paint_rects() {
        let canvas = canvas_8x4(6);
        assert_eq!(canvas.rects_skip_reason(7), None, "6 + 1 extends exactness");
        assert_eq!(
            canvas.rects_skip_reason(6),
            Some("stale"),
            "frame 6 is already on the canvas in full"
        );
        assert_eq!(canvas.rects_skip_reason(4), Some("stale"));
        assert_eq!(
            canvas.rects_skip_reason(9),
            Some("gap"),
            "frames 7 and 8 never reached this canvas; painting over the hole would lose them"
        );
        let legacy = Canvas::from_frame(Arc::new(vec![0u8; 16]), 2, 2, None);
        assert_eq!(
            legacy.rects_skip_reason(1),
            Some("gap"),
            "no sequence domain, no provable adjacency"
        );
    }

    #[test]
    fn rects_ahead_of_a_missing_frame_are_skipped_so_its_au_still_paints() {
        // The first review's critical scenario: frame 5 changed a large region (no
        // rects), frame 6 was typing-class, and the async encoder delivers AU(5)
        // *after* rects(6) arrived. The old rule painted rects(6) and then
        // suppressed AU(5) forever; the exactness rule must do the opposite.
        let mut canvas = canvas_8x4(4);
        assert_eq!(
            canvas.rects_skip_reason(6),
            Some("gap"),
            "frame 5's content has not reached the canvas"
        );
        assert!(
            canvas.accepts_au(Some(5)),
            "the only carrier of frame 5's content must still paint"
        );
        canvas.set_frame(Arc::new(vec![0u8; 8 * 4 * 4]), Some(5));
        assert_eq!(
            canvas.rects_skip_reason(6),
            None,
            "with frame 5 painted, frame 6's rects are adjacent again"
        );
    }

    #[test]
    fn applying_rects_lands_the_swizzled_pixels_where_the_rect_declared() {
        let r = rect(2, 1, 3, 2, 0x40);
        let mut canvas = canvas_8x4(0);
        canvas
            .apply_rects(&update(1, 8, 4, vec![r.clone()]))
            .expect("fits");

        let stride = 8 * 4;
        for row in 0..r.h as usize {
            for col in 0..r.w as usize {
                let s = (row * r.w as usize + col) * 4;
                let (b, g, red) = (r.pixels[s], r.pixels[s + 1], r.pixels[s + 2]);
                let d = (1 + row) * stride + (2 + col) * 4;
                assert_eq!(canvas.rgba[d], red, "R at row {row} col {col}");
                assert_eq!(canvas.rgba[d + 1], g, "G at row {row} col {col}");
                assert_eq!(canvas.rgba[d + 2], b, "B at row {row} col {col}");
                assert_eq!(canvas.rgba[d + 3], 0xFF, "A at row {row} col {col}");
            }
        }
        // A pixel outside the rect: the canvas is persistent, not wiped per update.
        assert_eq!(&canvas.rgba[0..4], &[0x11, 0x11, 0x11, 0x11]);
    }

    #[test]
    fn applying_rects_advances_exactness_to_their_frame() {
        let mut canvas = canvas_8x4(8);
        canvas
            .apply_rects(&update(9, 8, 4, vec![rect(0, 0, 1, 1, 0x33)]))
            .expect("adjacent");
        assert_eq!(canvas.exact_through, Some(9));
        assert!(
            !canvas.accepts_au(Some(9)),
            "the AU for frame 9 is now redundant"
        );
        assert!(canvas.accepts_au(Some(10)));
    }

    // ---- DecodeSink's rect path, driven with a canvas installed by hand ----

    /// A stats log backed by a temp file, so a test can read the lines it wrote.
    /// Removed by the test that made it.
    struct TempLog {
        path: std::path::PathBuf,
        log: Arc<StatsLog>,
    }

    impl TempLog {
        fn new(tag: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!("spike-sink-{tag}-{}.jsonl", std::process::id()));
            let log = Arc::new(
                StatsLog::create(Some(path.to_str().expect("a utf-8 temp path")))
                    .expect("create the stats file"),
            );
            Self { path, log }
        }

        fn lines(&self) -> Vec<Value> {
            self.log.flush();
            std::fs::read_to_string(&self.path)
                .expect("the stats file")
                .lines()
                .map(|l| serde_json::from_str(l).expect("every stats line is valid JSON"))
                .collect()
        }
    }

    impl Drop for TempLog {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// A sink with no decoder: every test below drives the rect path only, which is
    /// the point — the fast path must be testable without an H.264 stream.
    fn sink_with(log: &TempLog, slot: Arc<FrameSlot>) -> DecodeSink {
        DecodeSink::new(None, slot, log.log.clone(), Box::new(|| {}))
    }

    #[test]
    fn a_rects_message_before_any_canvas_is_skipped_and_counted() {
        let log = TempLog::new("before-base");
        let slot = Arc::new(FrameSlot::new());
        let mut sink = sink_with(&log, slot.clone());

        let u = update(3, 8, 4, vec![rect(0, 0, 2, 1, 0x20)]);
        sink.on_rects(&payload(&u), 1_234)
            .expect("a well-formed payload is not a protocol error");

        assert!(
            slot.take().is_none(),
            "nothing to composite onto, so nothing published"
        );
        let lines = log.lines();
        assert_eq!(lines.len(), 1, "the skip is visible, not silent: {lines:?}");
        assert_eq!(lines[0]["type"], "rects");
        assert_eq!(lines[0]["skipped"], "before_base");
        assert_eq!(lines[0]["seq"], 3);
        assert_eq!(lines[0]["recv_done_us"], 1_234);
        assert_eq!(lines[0]["rect_count"], 1);
        assert_eq!(lines[0]["dropped"], false);
    }

    #[test]
    fn a_rects_message_paints_the_canvas_and_publishes_a_rect_stamped_frame() {
        let log = TempLog::new("paints");
        let slot = Arc::new(FrameSlot::new());
        let mut sink = sink_with(&log, slot.clone());
        sink.canvas = Some(canvas_8x4(10));

        let r = rect(2, 1, 3, 2, 0x40);
        let u = update(11, 8, 4, vec![r.clone()]);
        sink.on_rects(&payload(&u), 5_000).expect("valid payload");

        let published = slot.take().expect("a snapshot was published");
        assert_eq!((published.width, published.height), (8, 4));
        let s = match published.stamps {
            PaintStamps::Rects(s) => s,
            PaintStamps::Au(_) => panic!("the rect path must stamp its own record type"),
        };
        assert_eq!(s.seq, 11);
        assert_eq!(s.recv_done_us, 5_000);
        assert_eq!(s.rect_count, 1);
        assert_eq!(s.rect_bytes, r.pixels.len());
        assert!(s.paint_done_us > 0, "a paint stamp was taken");
        assert!(published.stamps_pending, "the line is still owed");
        assert_eq!(
            published.damage,
            Damage::Rects(vec![DamageRect {
                x: 2,
                y: 1,
                w: 3,
                h: 2
            }]),
            "the damage is the geometry that was blitted, so the window thread can \
             convert exactly it"
        );

        // The snapshot carries the painted pixels, swizzled. Row 1, column 2.
        let d = 8 * 4 + 2 * 4;
        assert_eq!(
            &published.rgba[d..d + 4],
            &[r.pixels[2], r.pixels[1], r.pixels[0], 0xFF]
        );

        assert_eq!(
            sink.canvas.as_ref().unwrap().exact_through,
            Some(11),
            "the canvas is now exact through the painted frame"
        );
        assert!(
            !sink.canvas.as_ref().unwrap().accepts_au(Some(11)),
            "so frame 11's own AU is refused"
        );
        assert!(
            log.lines().is_empty(),
            "the record is closed at present, not here"
        );
    }

    #[test]
    fn a_rects_update_whose_frame_size_is_not_the_canvas_size_is_skipped() {
        let log = TempLog::new("size-mismatch");
        let slot = Arc::new(FrameSlot::new());
        let mut sink = sink_with(&log, slot.clone());
        sink.canvas = Some(canvas_8x4(11));

        // Legal against its own declared 16x8 frame, wrong for this 8x4 canvas.
        let u = update(12, 16, 8, vec![rect(9, 5, 2, 2, 0x60)]);
        sink.on_rects(&payload(&u), 6_000).expect("valid payload");

        assert!(slot.take().is_none(), "nothing published");
        let lines = log.lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["skipped"], "size_mismatch");
        assert_eq!(lines[0]["seq"], 12);
        assert_eq!(
            sink.canvas.as_ref().unwrap().exact_through,
            Some(11),
            "a skipped update must not advance exactness"
        );
    }

    #[test]
    fn a_malformed_rects_payload_is_reported_as_an_error() {
        let log = TempLog::new("garbage");
        let slot = Arc::new(FrameSlot::new());
        let mut sink = sink_with(&log, slot.clone());
        sink.canvas = Some(canvas_8x4(0));

        let err = sink
            .on_rects(&[0xDE, 0xAD, 0xBE, 0xEF], 7_000)
            .expect_err("a payload too short for its own header");
        assert!(err.contains("rects"), "the reason is carried up: {err}");
        assert!(slot.take().is_none(), "nothing was published");
    }

    #[test]
    fn a_gap_rects_update_is_held_not_painted() {
        let log = TempLog::new("gap-held");
        let slot = Arc::new(FrameSlot::new());
        let mut sink = sink_with(&log, slot.clone());
        sink.canvas = Some(canvas_8x4(4));

        // Frame 5's AU has not arrived; these rects are one frame ahead.
        let u = update(6, 8, 4, vec![rect(0, 0, 1, 1, 0x50)]);
        sink.on_rects(&payload(&u), 8_000).expect("valid payload");

        assert!(slot.take().is_none(), "nothing painted over the hole");
        assert!(
            log.lines().is_empty(),
            "a held update's line is written when it resolves, not now"
        );
        assert_eq!(
            sink.canvas.as_ref().unwrap().exact_through,
            Some(4),
            "holding must not advance exactness"
        );
        assert_eq!(
            sink.pending_rects.as_ref().map(|p| p.update.frame_seq),
            Some(6),
            "the update is held, not discarded"
        );
    }

    #[test]
    fn a_held_rects_update_applies_once_the_au_closes_the_gap() {
        // The re-review's collapse scenario: after one predicate miss every rect
        // update arrives one frame ahead of exactness (the encoder runs a frame
        // behind), so pure skipping kills the fast path until the desktop idles.
        // The hold must restore it one frame after the miss.
        let log = TempLog::new("gap-closed");
        let slot = Arc::new(FrameSlot::new());
        let mut sink = sink_with(&log, slot.clone());
        sink.canvas = Some(canvas_8x4(4));

        let held = update(6, 8, 4, vec![rect(1, 1, 2, 1, 0x55)]);
        sink.on_rects(&payload(&held), 8_100)
            .expect("valid payload");
        assert!(slot.take().is_none(), "held while frame 5 is missing");

        // AU(5) arrives: this is what `on_video` does after accepting it.
        sink.canvas
            .as_mut()
            .unwrap()
            .set_frame(Arc::new(vec![0x11u8; 8 * 4 * 4]), Some(5));
        sink.try_apply_pending();

        let published = slot.take().expect("the held update painted");
        match published.stamps {
            PaintStamps::Rects(s) => {
                assert_eq!(s.seq, 6);
                assert_eq!(s.recv_done_us, 8_100, "the original receive stamp");
            }
            PaintStamps::Au(_) => panic!("a rect paint must carry rect stamps"),
        }
        assert_eq!(sink.canvas.as_ref().unwrap().exact_through, Some(6));
        assert!(sink.pending_rects.is_none(), "the hold slot is free again");

        // And the next typing-class update is adjacent again — the fast path is
        // back at full rate, which is the entire point of the hold.
        let next = update(7, 8, 4, vec![rect(2, 2, 1, 1, 0x66)]);
        sink.on_rects(&payload(&next), 8_200)
            .expect("valid payload");
        let published = slot.take().expect("adjacent, painted immediately");
        match published.stamps {
            PaintStamps::Rects(s) => assert_eq!(s.seq, 7),
            PaintStamps::Au(_) => panic!("a rect paint must carry rect stamps"),
        }
    }

    #[test]
    fn a_held_rects_update_expires_stale_when_an_au_jumps_past_it() {
        let log = TempLog::new("gap-expired");
        let slot = Arc::new(FrameSlot::new());
        let mut sink = sink_with(&log, slot.clone());
        sink.canvas = Some(canvas_8x4(4));

        let held = update(6, 8, 4, vec![rect(0, 0, 1, 1, 0x50)]);
        sink.on_rects(&payload(&held), 8_300)
            .expect("valid payload");

        // A keyframe for frame 8 (say, after a wire drop) makes the hold moot.
        sink.canvas
            .as_mut()
            .unwrap()
            .set_frame(Arc::new(vec![0x11u8; 8 * 4 * 4]), Some(8));
        sink.try_apply_pending();

        assert!(slot.take().is_none(), "an expired hold paints nothing");
        let lines = log.lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["skipped"], "stale");
        assert_eq!(lines[0]["seq"], 6);
        assert!(sink.pending_rects.is_none());
    }

    #[test]
    fn a_newer_gap_update_displaces_a_held_one_which_gets_its_line() {
        let log = TempLog::new("gap-displaced");
        let slot = Arc::new(FrameSlot::new());
        let mut sink = sink_with(&log, slot.clone());
        sink.canvas = Some(canvas_8x4(4));

        let first = update(6, 8, 4, vec![rect(0, 0, 1, 1, 0x50)]);
        sink.on_rects(&payload(&first), 8_400)
            .expect("valid payload");
        let second = update(8, 8, 4, vec![rect(1, 0, 1, 1, 0x51)]);
        sink.on_rects(&payload(&second), 8_500)
            .expect("valid payload");

        let lines = log.lines();
        assert_eq!(lines.len(), 1, "exactly the displaced update got a line");
        assert_eq!(lines[0]["skipped"], "gap");
        assert_eq!(lines[0]["seq"], 6);
        assert_eq!(
            sink.pending_rects.as_ref().map(|p| p.update.frame_seq),
            Some(8),
            "newest wins the hold slot"
        );
    }

    #[test]
    fn an_empty_rects_update_is_skipped_rather_than_trusted() {
        // The parser accepts a zero-rect update; painting nothing must not claim a
        // frame's content, even when the seq is adjacent.
        let log = TempLog::new("empty");
        let slot = Arc::new(FrameSlot::new());
        let mut sink = sink_with(&log, slot.clone());
        sink.canvas = Some(canvas_8x4(4));

        let u = update(5, 8, 4, vec![]);
        sink.on_rects(&payload(&u), 9_000).expect("valid payload");

        assert!(slot.take().is_none(), "nothing published");
        let lines = log.lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["skipped"], "empty");
        assert_eq!(
            sink.canvas.as_ref().unwrap().exact_through,
            Some(4),
            "an unpainted claim must not advance exactness"
        );
    }

    #[test]
    fn publishing_shares_the_canvas_buffer_instead_of_copying_it() {
        // The Arc model exists so the AU path costs no per-frame copy; if publish
        // ever cloned the pixels, the published buffer would no longer be the same
        // allocation as the canvas's.
        let log = TempLog::new("shares");
        let slot = Arc::new(FrameSlot::new());
        let mut sink = sink_with(&log, slot.clone());
        sink.canvas = Some(canvas_8x4(10));

        let u = update(11, 8, 4, vec![rect(0, 0, 1, 1, 0x70)]);
        sink.on_rects(&payload(&u), 10_000).expect("valid payload");

        let published = slot.take().expect("published");
        assert!(
            Arc::ptr_eq(&published.rgba, &sink.canvas.as_ref().unwrap().rgba),
            "the snapshot must share the canvas allocation, not copy it"
        );
    }
}
