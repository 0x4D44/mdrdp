//! The video socket reader: bytes in, framed messages out.
//!
//! Framing is [`spike_server::framing`] — the server's own module, reassembling what
//! the server's own `encode` produced. A second implementation of a length-prefixed
//! format is a second place for it to be wrong.
//!
//! Nothing here knows about decoding or windows. That is what makes
//! `tests/loopback.rs` able to drive the whole receive path against a local listener
//! with no display attached.

use std::io::Read;

use spike_server::framing::{self, Reassembler};

use crate::clock::Clock;

/// Read size. A 1080p keyframe from the spike server is a few hundred KB, so this is
/// sized to swallow a whole small frame per syscall without making the common
/// few-KB delta frame pay for a large zeroed buffer.
const READ_CHUNK: usize = 64 * 1024;

/// What [`pump`] hands each complete message to.
pub trait MessageSink {
    /// One H.264 access unit, Annex B. `seq` is the server's capture sequence
    /// number when the stream is wire v2 (`MSG_VIDEO_SEQ`), `None` on a legacy
    /// `MSG_VIDEO` stream. `recv_done_us` is stamped immediately after the `read`
    /// that completed the message returned — the client's stage 1.
    fn on_video(&mut self, au: &[u8], seq: Option<u64>, recv_done_us: u64);
    /// One server stats line (JSON, no trailing newline).
    fn on_stats(&mut self, payload: &[u8]);
    /// One raw dirty-rect update (`MSG_RECTS` payload, undecoded). Default: skip —
    /// a sink that does not composite simply never sees painted rects.
    ///
    /// `Err` means the payload violated its own format. The pump turns that into
    /// [`PumpEnd::Protocol`]: a rect payload is walked by length fields that choose
    /// offsets into a framebuffer, so a malformed one is as terminal as a framing
    /// error and for the same reason — there is nothing to resynchronise to.
    fn on_rects(&mut self, payload: &[u8], recv_done_us: u64) -> Result<(), String> {
        let _ = (payload, recv_done_us);
        Ok(())
    }
    /// A message type this build does not know. The length prefix means an unknown
    /// type costs nothing to skip, which is the whole reason it is a length prefix.
    fn on_unknown(&mut self, msg_type: u8, payload_len: usize) {
        eprintln!("net: skipping unknown message type {msg_type} ({payload_len} bytes)");
    }
}

/// Why the pump stopped.
#[derive(Debug)]
pub enum PumpEnd {
    /// The server closed the connection cleanly.
    Eof,
    /// The stream lost sync. Terminal: a bare length-prefixed format has no
    /// resynchronisation point, so the only honest move is to stop.
    Framing(framing::FramingError),
    /// A known message type carried a payload that violates its own contract
    /// (e.g. `MSG_VIDEO_SEQ` too short for its sequence prefix). As terminal as a
    /// framing error: the peer is not speaking the protocol it declared.
    Protocol(String),
    Io(std::io::Error),
}

impl std::fmt::Display for PumpEnd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PumpEnd::Eof => write!(f, "server closed the connection"),
            PumpEnd::Framing(e) => write!(f, "{e}"),
            PumpEnd::Protocol(e) => write!(f, "protocol: {e}"),
            PumpEnd::Io(e) => write!(f, "{e}"),
        }
    }
}

/// Read framed messages until the stream ends, dispatching each to `sink`.
///
/// Every message completed by one `read` shares that read's stamp. They arrived in the
/// same packet, so distinguishing them would be inventing precision the transport does
/// not have.
///
/// Within one read's batch, `MSG_RECTS` is dispatched before the video messages. A
/// rect blit costs ~0.1 ms and an AU decode ~6 ms, so wire order made a rect that
/// shared a read with an AU wait a decode's length to paint (measured: recv→paint
/// p50 9.5 ms on the Increment 1 typing runs). Correctness never depended on wire
/// order — the sink's exactness gate skips, holds, or paints an update on its `seq`
/// alone, whatever order it arrives in — so delivery order is purely latency policy.
/// Relative order *within* each class is preserved.
pub fn pump<R: Read>(reader: &mut R, clock: &Clock, sink: &mut dyn MessageSink) -> PumpEnd {
    let mut re = Reassembler::default();
    let mut buf = vec![0u8; READ_CHUNK];
    let mut batch: Vec<framing::Message> = Vec::new();
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => return PumpEnd::Eof,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return PumpEnd::Io(e),
        };
        let recv_done_us = clock.now_us();
        re.push(&buf[..n]);
        batch.clear();
        loop {
            match re.next_message() {
                Ok(Some(msg)) => batch.push(msg),
                Ok(None) => break,
                Err(e) => return PumpEnd::Framing(e),
            }
        }
        let is_video = |t: u8| t == framing::MSG_VIDEO || t == framing::MSG_VIDEO_SEQ;
        for pass in 0..2 {
            let video_pass = pass == 1;
            for msg in &batch {
                if is_video(msg.msg_type) != video_pass {
                    continue;
                }
                match msg.msg_type {
                    framing::MSG_VIDEO => sink.on_video(&msg.payload, None, recv_done_us),
                    framing::MSG_VIDEO_SEQ => {
                        let Some(seq_bytes) = msg.payload.get(..8) else {
                            return PumpEnd::Protocol(format!(
                                "MSG_VIDEO_SEQ of {} bytes cannot hold its sequence prefix",
                                msg.payload.len()
                            ));
                        };
                        let seq = u64::from_le_bytes(seq_bytes.try_into().expect("8-byte slice"));
                        sink.on_video(&msg.payload[8..], Some(seq), recv_done_us);
                    }
                    framing::MSG_RECTS => {
                        if let Err(e) = sink.on_rects(&msg.payload, recv_done_us) {
                            return PumpEnd::Protocol(e);
                        }
                    }
                    framing::MSG_STATS => sink.on_stats(&msg.payload),
                    other => sink.on_unknown(other, msg.payload.len()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder {
        video: Vec<(Vec<u8>, Option<u64>, u64)>,
        rects: Vec<Vec<u8>>,
        stats: Vec<Vec<u8>>,
        unknown: Vec<(u8, usize)>,
        /// Dispatch order across the callback kinds, which the per-kind vecs above
        /// cannot show — the batch reordering contract is asserted against this.
        order: Vec<&'static str>,
        /// Makes `on_rects` refuse every payload, so the pump's handling of a sink
        /// error is testable without a real compositor.
        refuse_rects: bool,
    }

    impl MessageSink for Recorder {
        fn on_video(&mut self, au: &[u8], seq: Option<u64>, recv_done_us: u64) {
            self.order.push("video");
            self.video.push((au.to_vec(), seq, recv_done_us));
        }
        fn on_stats(&mut self, payload: &[u8]) {
            self.order.push("stats");
            self.stats.push(payload.to_vec());
        }
        fn on_rects(&mut self, payload: &[u8], _recv_done_us: u64) -> Result<(), String> {
            self.order.push("rects");
            self.rects.push(payload.to_vec());
            if self.refuse_rects {
                return Err("rects: refused by the test sink".to_owned());
            }
            Ok(())
        }
        fn on_unknown(&mut self, msg_type: u8, payload_len: usize) {
            self.unknown.push((msg_type, payload_len));
        }
    }

    fn wire(parts: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (t, p) in parts {
            framing::encode(*t, p, &mut out);
        }
        out
    }

    #[test]
    fn each_message_reaches_the_matching_callback() {
        // Distinct payloads: a fixture reusing one could not catch video and stats
        // being dispatched to the same handler.
        let bytes = wire(&[
            (framing::MSG_STATS, br#"{"record":"header"}"#.to_vec()),
            (framing::MSG_VIDEO, vec![0, 0, 0, 1, 0x65, 0xAA]),
            (framing::MSG_VIDEO, vec![0, 0, 0, 1, 0x41, 0xBB, 0xCC]),
        ]);
        let mut sink = Recorder::default();
        let end = pump(&mut bytes.as_slice(), &Clock::new(), &mut sink);
        assert!(matches!(end, PumpEnd::Eof), "{end}");
        assert_eq!(sink.stats.len(), 1);
        assert_eq!(sink.stats[0], br#"{"record":"header"}"#);
        assert_eq!(sink.video.len(), 2);
        assert_eq!(sink.video[0].0, vec![0, 0, 0, 1, 0x65, 0xAA]);
        assert_eq!(sink.video[0].1, None, "bare MSG_VIDEO carries no seq");
        assert_eq!(sink.video[1].0, vec![0, 0, 0, 1, 0x41, 0xBB, 0xCC]);
        assert!(sink.unknown.is_empty());
    }

    #[test]
    fn a_seq_prefixed_video_message_yields_the_seq_and_the_bare_au() {
        // Distinct seq and payload bytes so a transposed slice boundary shows.
        let mut payload = 0x0102_0304_0506_0708u64.to_le_bytes().to_vec();
        payload.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x99]);
        let bytes = wire(&[(framing::MSG_VIDEO_SEQ, payload)]);
        let mut sink = Recorder::default();
        let end = pump(&mut bytes.as_slice(), &Clock::new(), &mut sink);
        assert!(matches!(end, PumpEnd::Eof), "{end}");
        assert_eq!(sink.video.len(), 1);
        assert_eq!(sink.video[0].1, Some(0x0102_0304_0506_0708));
        assert_eq!(sink.video[0].0, vec![0, 0, 0, 1, 0x65, 0x99]);
    }

    #[test]
    fn a_video_seq_message_too_short_for_its_prefix_is_terminal() {
        let bytes = wire(&[(framing::MSG_VIDEO_SEQ, vec![1, 2, 3])]);
        let mut sink = Recorder::default();
        let end = pump(&mut bytes.as_slice(), &Clock::new(), &mut sink);
        assert!(matches!(end, PumpEnd::Protocol(_)), "{end}");
        assert!(sink.video.is_empty(), "nothing decodable was delivered");
    }

    #[test]
    fn a_rects_message_reaches_the_rects_callback_undecoded() {
        let bytes = wire(&[(framing::MSG_RECTS, vec![9, 8, 7])]);
        let mut sink = Recorder::default();
        pump(&mut bytes.as_slice(), &Clock::new(), &mut sink);
        assert_eq!(sink.rects, vec![vec![9, 8, 7]]);
        assert!(sink.video.is_empty());
    }

    #[test]
    fn a_sink_that_refuses_a_rects_payload_ends_the_pump_as_protocol() {
        // The video message after it is the point: a malformed rect update is
        // terminal, so nothing behind it may be delivered.
        let bytes = wire(&[
            (framing::MSG_RECTS, vec![9, 8, 7]),
            (framing::MSG_VIDEO, vec![0x11]),
        ]);
        let mut sink = Recorder {
            refuse_rects: true,
            ..Default::default()
        };
        let end = pump(&mut bytes.as_slice(), &Clock::new(), &mut sink);
        assert!(matches!(end, PumpEnd::Protocol(_)), "{end}");
        assert_eq!(sink.rects.len(), 1, "the refused payload was offered once");
        assert!(
            sink.video.is_empty(),
            "nothing after the refusal is delivered"
        );
    }

    #[test]
    fn rects_sharing_a_read_with_earlier_video_are_dispatched_first() {
        // Wire order is AU(1), rects(2), AU(2) in one read. A rect blit is ~60x
        // cheaper than a decode, so the pump front-runs the rects; the sink's
        // exactness gate makes any order correct, which is what licenses this.
        let mut au1 = 1u64.to_le_bytes().to_vec();
        au1.push(0xA1);
        let mut au2 = 2u64.to_le_bytes().to_vec();
        au2.push(0xA2);
        let bytes = wire(&[
            (framing::MSG_VIDEO_SEQ, au1),
            (framing::MSG_RECTS, vec![9, 8, 7]),
            (framing::MSG_VIDEO_SEQ, au2),
        ]);
        let mut sink = Recorder::default();
        let end = pump(&mut bytes.as_slice(), &Clock::new(), &mut sink);
        assert!(matches!(end, PumpEnd::Eof), "{end}");
        assert_eq!(
            sink.order,
            vec!["rects", "video", "video"],
            "the rect update paints before the batch's decodes"
        );
        assert_eq!(
            (sink.video[0].1, sink.video[1].1),
            (Some(1), Some(2)),
            "video order within the batch is preserved"
        );
    }

    #[test]
    fn a_message_split_across_reads_is_reassembled() {
        struct Dribble {
            bytes: Vec<u8>,
            pos: usize,
        }
        impl Read for Dribble {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                if self.pos >= self.bytes.len() {
                    return Ok(0);
                }
                out[0] = self.bytes[self.pos];
                self.pos += 1;
                Ok(1)
            }
        }
        let payload: Vec<u8> = (0u8..=200).collect();
        let mut reader = Dribble {
            bytes: wire(&[(framing::MSG_VIDEO, payload.clone())]),
            pos: 0,
        };
        let mut sink = Recorder::default();
        pump(&mut reader, &Clock::new(), &mut sink);
        assert_eq!(sink.video.len(), 1, "one message, not 205");
        assert_eq!(sink.video[0].0, payload);
    }

    #[test]
    fn an_unknown_type_is_skipped_and_the_stream_continues() {
        let bytes = wire(&[
            (99, vec![0xDE, 0xAD, 0xBE, 0xEF]),
            (framing::MSG_VIDEO, vec![0x11]),
        ]);
        let mut sink = Recorder::default();
        pump(&mut bytes.as_slice(), &Clock::new(), &mut sink);
        assert_eq!(sink.unknown, vec![(99, 4)]);
        assert_eq!(
            sink.video.len(),
            1,
            "the message after the unknown survives"
        );
    }

    #[test]
    fn a_corrupt_length_stops_the_pump_rather_than_guessing() {
        let mut bytes = 0u32.to_le_bytes().to_vec(); // zero-length: no type byte
        bytes.extend_from_slice(&wire(&[(framing::MSG_VIDEO, vec![1, 2, 3])]));
        let mut sink = Recorder::default();
        let end = pump(&mut bytes.as_slice(), &Clock::new(), &mut sink);
        assert!(matches!(end, PumpEnd::Framing(_)), "{end}");
        assert!(sink.video.is_empty());
    }

    #[test]
    fn the_receive_stamp_is_taken_and_shared_by_one_read() {
        let bytes = wire(&[(framing::MSG_VIDEO, vec![1]), (framing::MSG_VIDEO, vec![2])]);
        let mut sink = Recorder::default();
        pump(&mut bytes.as_slice(), &Clock::new(), &mut sink);
        assert_eq!(sink.video.len(), 2);
        assert!(sink.video[0].2 > 0, "a stamp was taken");
        assert_eq!(
            sink.video[0].2, sink.video[1].2,
            "both arrived in the same read, so both carry that read's stamp"
        );
    }
}
