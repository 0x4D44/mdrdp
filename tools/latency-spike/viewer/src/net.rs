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
    /// One H.264 access unit, Annex B. `recv_done_us` is stamped immediately after
    /// the `read` that completed the message returned — the client's stage 1.
    fn on_video(&mut self, au: &[u8], recv_done_us: u64);
    /// One server stats line (JSON, no trailing newline).
    fn on_stats(&mut self, payload: &[u8]);
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
    Io(std::io::Error),
}

impl std::fmt::Display for PumpEnd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PumpEnd::Eof => write!(f, "server closed the connection"),
            PumpEnd::Framing(e) => write!(f, "{e}"),
            PumpEnd::Io(e) => write!(f, "{e}"),
        }
    }
}

/// Read framed messages until the stream ends, dispatching each to `sink`.
///
/// Every message completed by one `read` shares that read's stamp. They arrived in the
/// same packet, so distinguishing them would be inventing precision the transport does
/// not have.
pub fn pump<R: Read>(reader: &mut R, clock: &Clock, sink: &mut dyn MessageSink) -> PumpEnd {
    let mut re = Reassembler::default();
    let mut buf = vec![0u8; READ_CHUNK];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => return PumpEnd::Eof,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return PumpEnd::Io(e),
        };
        let recv_done_us = clock.now_us();
        re.push(&buf[..n]);
        loop {
            match re.next_message() {
                Ok(Some(msg)) => match msg.msg_type {
                    framing::MSG_VIDEO => sink.on_video(&msg.payload, recv_done_us),
                    framing::MSG_STATS => sink.on_stats(&msg.payload),
                    other => sink.on_unknown(other, msg.payload.len()),
                },
                Ok(None) => break,
                Err(e) => return PumpEnd::Framing(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder {
        video: Vec<(Vec<u8>, u64)>,
        stats: Vec<Vec<u8>>,
        unknown: Vec<(u8, usize)>,
    }

    impl MessageSink for Recorder {
        fn on_video(&mut self, au: &[u8], recv_done_us: u64) {
            self.video.push((au.to_vec(), recv_done_us));
        }
        fn on_stats(&mut self, payload: &[u8]) {
            self.stats.push(payload.to_vec());
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
        assert_eq!(sink.video[1].0, vec![0, 0, 0, 1, 0x41, 0xBB, 0xCC]);
        assert!(sink.unknown.is_empty());
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
        assert!(sink.video[0].1 > 0, "a stamp was taken");
        assert_eq!(
            sink.video[0].1, sink.video[1].1,
            "both arrived in the same read, so both carry that read's stamp"
        );
    }
}
