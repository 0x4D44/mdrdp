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
use spike_server::annexb;

use crate::clock::Clock;
use crate::net::MessageSink;
use crate::stats::{DecodeErrorRecord, FrameRecord, FrameStamps, StatsLog};

/// One decoded frame, ready to present.
#[derive(Debug, Clone)]
pub struct Frame {
    /// RGBA8, tightly packed, `width` pixels per row — `present_into`'s `src` contract.
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub stamps: FrameStamps,
    /// This frame still owes the stats file a line. Cleared once presented, so a
    /// repaint that shows the same frame again (a resize, an expose) does not emit a
    /// second line for it.
    pub stamps_pending: bool,
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
    pub fn put(&self, frame: Frame) -> Option<Frame> {
        let mut guard = self
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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

/// Decodes video messages into [`FrameSlot`] and writes the client stats lines.
pub struct DecodeSink {
    decoder: Option<Box<dyn H264Decoder>>,
    slot: Arc<FrameSlot>,
    stats: Arc<StatsLog>,
    clock: Clock,
    /// Wakes the window thread. A closure rather than an `EventLoopProxy` so this
    /// module has no winit dependency and stays testable without an event loop.
    wake: Box<dyn Fn() + Send>,
    frames: u64,
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
        }
    }

    /// Frames seen so far, decoded or not.
    pub fn frames(&self) -> u64 {
        self.frames
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
    fn on_video(&mut self, au: &[u8], recv_done_us: u64) {
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

        let stamps = FrameStamps {
            frame,
            recv_done_us,
            decode_in_us,
            decode_out_us,
            au_bytes: au.len(),
            keyframe,
            width: decoded.width(),
            height: decoded.height(),
        };
        let displaced = self.slot.put(Frame {
            width: decoded.width(),
            height: decoded.height(),
            rgba: decoded.into_data(),
            stamps,
            stamps_pending: true,
        });
        if let Some(old) = displaced {
            self.stats.record(&FrameRecord::new(&old.stamps, None));
        }
        (self.wake)();
    }

    fn on_stats(&mut self, payload: &[u8]) {
        self.stats.write_line(&crate::stats::server_line(payload));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamps(frame: u64) -> FrameStamps {
        FrameStamps {
            frame,
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
            rgba: vec![n as u8; 4 * 2 * 4],
            width: 4,
            height: 2,
            stamps: stamps(n),
            stamps_pending: true,
        }
    }

    #[test]
    fn the_slot_hands_back_the_newest_frame_and_empties() {
        let slot = FrameSlot::new();
        assert!(slot.take().is_none(), "empty to start");
        assert!(slot.put(frame(1)).is_none(), "nothing displaced");
        let got = slot.take().expect("a frame");
        assert_eq!(got.stamps.frame, 1);
        assert!(slot.take().is_none(), "taking empties the slot");
    }

    #[test]
    fn a_second_frame_displaces_the_first_and_the_displaced_one_is_returned() {
        // Frame numbers distinct, so "returned the displaced one" is distinguishable
        // from "returned the one just put".
        let slot = FrameSlot::new();
        slot.put(frame(1));
        let displaced = slot.put(frame(2)).expect("the first frame was displaced");
        assert_eq!(displaced.stamps.frame, 1);
        assert_eq!(slot.take().unwrap().stamps.frame, 2, "the newest wins");
    }
}
