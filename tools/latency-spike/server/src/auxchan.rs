//! The auxiliary channel's I/O: one reader loop, one writer loop, and the
//! outbound slot between whatever produces payloads and the writer.
//!
//! **Both ends use this.** It lives beside `framing` and `aux_proto` rather
//! than in the client because it is wire plumbing, not client policy — the
//! host serves the same channel with the same loops, and a second copy of
//! this would be two chances to get the teardown or the skip-unknown rule
//! subtly different.
//!
//! Both loops are written against `Read`/`Write` rather than `TcpStream`, so
//! every property below is tested against in-memory pipes — no socket, no ssh,
//! no host.
//!
//! # The outbound slot is depth-1 and keep-latest, on purpose
//!
//! The server→client video path shares a 2-deep, deliberately lossy channel
//! behind a single writer; neither of its send modes is acceptable here.
//! `try_send` drops silently under video load, and a blocking `send` would park
//! the clipboard thread on a full window — through an ssh tunnel that adds its
//! own buffer. A depth-1 keep-latest slot has neither failure: **clipboard is
//! last-writer-wins, so superseding a queued payload is not a loss**, and
//! unbounded growth is impossible by construction. Supersessions are counted
//! rather than silent, because "the clipboard is slow" and "the clipboard is
//! dropping things" need different answers.
//!
//! Tranche 6 adds audio *above* this tier with a strict-priority sender — any
//! pending audio before any pending clipboard. That tier is deliberately not
//! built here: there is nothing to rank against yet.

use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::aux_proto::{self, AuxMessage};
use crate::framing::{self, Reassembler};

/// How long a taker parks before looping, so a closed slot is noticed promptly
/// without a busy wait.
pub const SLOT_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// What a taker got.
#[derive(Debug, PartialEq, Eq)]
pub enum Take {
    /// A payload to send.
    Item(String),
    /// Nothing pending; the wait elapsed. Keep going.
    Idle,
    /// The session is ending. Stop.
    Closed,
}

/// A depth-1, keep-latest outbound slot.
pub struct Slot {
    state: Mutex<SlotState>,
    ready: Condvar,
    /// How long a taker parks before looping. Configurable **so the condvar
    /// wake-up is testable**: with the production quarter-second, a test cannot
    /// tell a taker that was woken from one that merely timed out, so removing
    /// the notify would pass every assertion.
    interval: Duration,
}

#[derive(Default)]
struct SlotState {
    pending: Option<String>,
    closed: bool,
    superseded: u64,
}

impl Slot {
    pub fn new() -> Arc<Self> {
        Self::with_interval(SLOT_POLL_INTERVAL)
    }

    pub fn with_interval(interval: Duration) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(SlotState::default()),
            ready: Condvar::new(),
            interval,
        })
    }

    /// Queue `text`, replacing anything already pending.
    ///
    /// Never blocks and never grows. A payload replaced here was never the
    /// clipboard's current content by the time it would have been sent, so
    /// dropping it is correct rather than merely tolerable.
    pub fn put(&self, text: String) {
        let mut state = self.lock();
        if state.closed {
            return;
        }
        if state.pending.is_some() {
            state.superseded += 1;
        }
        state.pending = Some(text);
        drop(state);
        self.ready.notify_one();
    }

    /// Wait up to [`SLOT_POLL_INTERVAL`] for a payload.
    pub fn take(&self) -> Take {
        let mut state = self.lock();
        if let Some(text) = state.pending.take() {
            return Take::Item(text);
        }
        if state.closed {
            return Take::Closed;
        }
        let (mut state, _) = self
            .ready
            .wait_timeout(state, self.interval)
            .unwrap_or_else(|e| {
                let (guard, timeout) = e.into_inner();
                (guard, timeout)
            });
        if let Some(text) = state.pending.take() {
            Take::Item(text)
        } else if state.closed {
            Take::Closed
        } else {
            Take::Idle
        }
    }

    /// End the session. Wakes any parked taker.
    pub fn close(&self) {
        self.lock().closed = true;
        self.ready.notify_all();
    }

    /// How many queued payloads were replaced before they could be sent.
    pub fn superseded(&self) -> u64 {
        self.lock().superseded
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SlotState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Why the writer loop ended.
#[derive(Debug, PartialEq, Eq)]
pub enum WriterEnd {
    /// The slot closed: an orderly session end.
    Closed,
    /// The socket failed. The session's own teardown owns what happens next.
    Io(String),
}

/// Drain the slot onto the wire until it closes or the socket fails.
///
/// An over-ceiling payload is **reported and dropped, never written**. That is
/// the session-safety property: above the framing layer's own limit the peer
/// would see a terminal `FramingError` and drop the session, so a large copy
/// would kill the remote desktop — worse than having no clipboard.
pub fn pump_writer(mut out: impl Write, slot: &Slot, report: &mut impl FnMut(&str)) -> WriterEnd {
    let mut buf = Vec::new();
    loop {
        match slot.take() {
            Take::Closed => return WriterEnd::Closed,
            Take::Idle => continue,
            Take::Item(text) => {
                buf.clear();
                if let Err(e) = aux_proto::encode_clipboard_text(&text, &mut buf) {
                    // Names sizes only; `AuxProtoError` carries no content.
                    report(&format!("clipboard not sent: {e}"));
                    continue;
                }
                if let Err(e) = out.write_all(&buf) {
                    return WriterEnd::Io(e.to_string());
                }
                if let Err(e) = out.flush() {
                    return WriterEnd::Io(e.to_string());
                }
            }
        }
    }
}

/// Why the reader loop ended.
#[derive(Debug, PartialEq, Eq)]
pub enum ReaderEnd {
    /// The peer closed the channel. Not a session failure on its own.
    Eof,
    /// The socket failed, or the framing did.
    Io(String),
}

/// Counters the reader keeps, so a test can assert what did *not* happen.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReaderStats {
    /// Payloads handed to the decoder. The policy gate is asserted against this
    /// rather than against delivery: "not delivered" and "not decoded" are
    /// different claims, and AC7 requires the stronger one.
    pub decoded: u64,
    /// Framed messages whose type this build does not speak.
    pub unknown_type: u64,
    /// Payloads that failed to decode.
    pub malformed: u64,
    /// Payloads refused before decoding, because the policy forbids inbound.
    pub refused_by_policy: u64,
}

/// Read framed messages until EOF, handing clipboard text to `on_text`.
///
/// `accepts` is consulted **before** the payload is decoded. Handing content to
/// the decoder and discarding the result afterwards is not the same as refusing
/// it, and the policy gate must be the stronger of the two.
///
/// An unknown message type is skipped, never fatal — the property the framing
/// buys, and one that was previously true server→client only. That is what lets
/// a later tranche add audio to this channel without a flag day.
pub fn pump_reader(
    mut input: impl Read,
    accepts: &mut impl FnMut() -> bool,
    on_text: &mut impl FnMut(&str),
    stats: &mut ReaderStats,
) -> ReaderEnd {
    let mut reassembler = Reassembler::new(framing::DEFAULT_MAX_PAYLOAD);
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = match input.read(&mut buf) {
            Ok(0) => return ReaderEnd::Eof,
            Ok(n) => n,
            Err(e) => return ReaderEnd::Io(e.to_string()),
        };
        reassembler.push(&buf[..n]);
        loop {
            match reassembler.next_message() {
                Ok(None) => break,
                Ok(Some(message)) => {
                    if message.msg_type != aux_proto::MSG_CLIPBOARD {
                        stats.unknown_type += 1;
                        continue;
                    }
                    if !accepts() {
                        stats.refused_by_policy += 1;
                        continue;
                    }
                    stats.decoded += 1;
                    match aux_proto::decode_clipboard(&message.payload) {
                        Ok(AuxMessage::ClipboardText(text)) => on_text(&text),
                        Err(_) => stats.malformed += 1,
                    }
                }
                // Framing is structural: a bad length means the stream is no
                // longer parseable, so there is nothing to resynchronise to.
                Err(e) => return ReaderEnd::Io(format!("{e:?}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Poll a condition rather than sleeping a guessed interval.
    fn wait_until(mut cond: impl FnMut() -> bool) {
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(std::time::Instant::now() < end, "condition never held");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn framed_clipboard(text: &str) -> Vec<u8> {
        let mut out = Vec::new();
        aux_proto::encode_clipboard_text(text, &mut out).expect("under the ceiling");
        out
    }

    #[test]
    fn the_slot_keeps_the_latest_payload_and_counts_what_it_replaced() {
        let slot = Slot::new();
        slot.put("first".to_owned());
        slot.put("second".to_owned());
        assert_eq!(slot.take(), Take::Item("second".to_owned()));
        assert_eq!(
            slot.superseded(),
            1,
            "a replaced payload must be counted, not silently vanish"
        );
        // Depth 1: the first payload is gone, not queued behind the second.
        assert_eq!(slot.take(), Take::Idle);
    }

    #[test]
    fn an_empty_slot_reports_idle_rather_than_closed() {
        // The distinction is what keeps the writer looping instead of ending
        // the channel the first quiet quarter-second. Short interval: this test
        // is about which answer comes back, not how long it waits.
        let slot = Slot::with_interval(Duration::from_millis(10));
        assert_eq!(slot.take(), Take::Idle);
    }

    /// A park interval far longer than [`PROMPT`], so "was it woken?" and "did
    /// it merely time out?" give different answers. Long enough to be
    /// unambiguous, short enough that a regression fails in half a minute
    /// rather than parking a suite.
    const LONG_PARK: Duration = Duration::from_secs(30);

    /// What counts as prompt. Absurdly generous against a 30 s park — a woken
    /// taker returns in a millisecond or two — so this cannot flake on a loaded
    /// machine while still being nowhere near the timeout.
    const PROMPT: Duration = Duration::from_secs(5);

    #[test]
    fn closing_wakes_a_parked_taker() {
        // The elapsed-time assertion is the whole test. Asserting only that the
        // taker eventually answers `Closed` passes with the notify deleted: it
        // would simply time out and re-check. That is a correct answer arrived
        // at a park interval late, which for a teardown path means threads that
        // outlive the session.
        let slot = Slot::with_interval(LONG_PARK);
        let waker = Arc::clone(&slot);
        let t = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            (waker.take(), started.elapsed())
        });
        std::thread::sleep(Duration::from_millis(50));
        slot.close();
        let (outcome, elapsed) = t.join().unwrap();
        assert_eq!(outcome, Take::Closed);
        assert!(
            elapsed < PROMPT,
            "the taker was not woken — it waited {elapsed:?} for the park to elapse"
        );
    }

    #[test]
    fn a_queued_payload_wakes_the_writer_rather_than_waiting_for_the_next_lap() {
        // Same shape, and it guards a user-visible property: without the notify
        // on `put`, every clipboard send would sit for a full park interval
        // before reaching the wire. Correct, and a quarter-second slower than
        // it needs to be, on a product whose premise is latency.
        let slot = Slot::with_interval(LONG_PARK);
        let taker = Arc::clone(&slot);
        let t = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            (taker.take(), started.elapsed())
        });
        // Let the taker park first, or `put` beats it to the lock and the test
        // proves nothing about waking.
        std::thread::sleep(Duration::from_millis(50));
        slot.put("now".to_owned());
        let (outcome, elapsed) = t.join().unwrap();
        assert_eq!(outcome, Take::Item("now".to_owned()));
        assert!(
            elapsed < PROMPT,
            "the payload waited {elapsed:?} for the park to elapse instead of waking the writer"
        );
    }

    #[test]
    fn a_payload_already_pending_when_the_slot_closes_is_still_delivered() {
        // Closing is an orderly end, not a discard: a copy made a moment before
        // teardown should still reach the peer if the socket is still there.
        let slot = Slot::new();
        slot.put("last words".to_owned());
        slot.close();
        assert_eq!(slot.take(), Take::Item("last words".to_owned()));
        assert_eq!(slot.take(), Take::Closed);
    }

    #[test]
    fn putting_after_close_is_dropped_rather_than_queued_forever() {
        let slot = Slot::new();
        slot.close();
        slot.put("too late".to_owned());
        assert_eq!(slot.take(), Take::Closed);
    }

    #[test]
    fn the_writer_frames_a_queued_payload_and_stops_when_the_slot_closes() {
        let slot = Slot::new();
        slot.put("hello\nworld".to_owned());
        slot.close();
        let mut wire = Vec::new();
        let mut reports = Vec::new();
        let end = pump_writer(&mut wire, &slot, &mut |m| reports.push(m.to_owned()));
        assert_eq!(end, WriterEnd::Closed);
        assert_eq!(wire, framed_clipboard("hello\nworld"));
        assert!(reports.is_empty());
    }

    #[test]
    fn the_writer_drops_an_oversize_payload_and_keeps_the_channel_alive() {
        // The session-safety property: an oversize message must never reach the
        // peer's reassembler, where it is a terminal framing error.
        //
        // The two payloads are handed over one at a time, because the slot is
        // keep-latest: queueing both at once would replace the oversize one
        // before the writer ever saw it, and the test would pass without
        // exercising the refusal at all.
        let slot = Slot::new();
        let reports: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let wire: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

        let writer_slot = Arc::clone(&slot);
        let writer_reports = Arc::clone(&reports);
        let writer_wire = Arc::clone(&wire);
        let writer = std::thread::spawn(move || {
            struct Shared(Arc<Mutex<Vec<u8>>>);
            impl Write for Shared {
                fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                    self.0.lock().unwrap().extend_from_slice(buf);
                    Ok(buf.len())
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            pump_writer(Shared(writer_wire), &writer_slot, &mut |m| {
                writer_reports.lock().unwrap().push(m.to_owned())
            })
        });

        slot.put("x".repeat(aux_proto::MAX_CLIPBOARD_BYTES + 1));
        wait_until(|| reports.lock().unwrap().len() == 1);
        slot.put("small".to_owned());
        wait_until(|| !wire.lock().unwrap().is_empty());
        slot.close();
        assert_eq!(writer.join().unwrap(), WriterEnd::Closed);

        // Only the payload under the ceiling reached the wire, and the loop
        // carried on to send it rather than ending on the refusal.
        assert_eq!(*wire.lock().unwrap(), framed_clipboard("small"));
        let reports = reports.lock().unwrap();
        assert_eq!(reports.len(), 1);
        assert!(reports[0].contains("not sent"));
        assert!(
            !reports[0].contains("xxx"),
            "a report may name sizes, never content: {}",
            reports[0]
        );
    }

    #[test]
    fn a_write_failure_ends_the_writer_with_the_reason() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "gone"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let slot = Slot::new();
        slot.put("anything".to_owned());
        slot.close();
        let end = pump_writer(Broken, &slot, &mut |_| {});
        assert!(matches!(end, WriterEnd::Io(m) if m.contains("gone")));
    }

    #[test]
    fn the_reader_delivers_clipboard_text_and_ends_cleanly_at_eof() {
        let wire = framed_clipboard("from the host");
        let mut got = Vec::new();
        let mut stats = ReaderStats::default();
        let end = pump_reader(
            &wire[..],
            &mut || true,
            &mut |t| got.push(t.to_owned()),
            &mut stats,
        );
        assert_eq!(end, ReaderEnd::Eof);
        assert_eq!(got, vec!["from the host".to_owned()]);
        assert_eq!(stats.decoded, 1);
    }

    #[test]
    fn an_unknown_message_type_is_skipped_and_the_next_message_still_arrives() {
        // The both-directions property this channel exists to provide. Tranche 6
        // adds audio here; an old client must skip it, not die on it — and the
        // proof is that a *later* message is still delivered.
        let mut wire = Vec::new();
        framing::encode(0x21, b"audio, one day", &mut wire);
        wire.extend_from_slice(&framed_clipboard("still here"));
        let mut got = Vec::new();
        let mut stats = ReaderStats::default();
        let end = pump_reader(
            &wire[..],
            &mut || true,
            &mut |t| got.push(t.to_owned()),
            &mut stats,
        );
        assert_eq!(end, ReaderEnd::Eof);
        assert_eq!(got, vec!["still here".to_owned()]);
        assert_eq!(stats.unknown_type, 1);
    }

    #[test]
    fn a_forbidden_payload_is_refused_before_it_is_decoded() {
        // AC7's stronger claim. `decoded` is the oracle: asserting only that the
        // callback never fired would also pass on an implementation that
        // decoded the payload and threw the result away.
        let wire = framed_clipboard("secret from the host");
        let mut got = Vec::new();
        let mut stats = ReaderStats::default();
        let end = pump_reader(
            &wire[..],
            &mut || false,
            &mut |t| got.push(t.to_owned()),
            &mut stats,
        );
        assert_eq!(end, ReaderEnd::Eof);
        assert!(got.is_empty());
        assert_eq!(stats.decoded, 0, "the payload must never reach the decoder");
        assert_eq!(stats.refused_by_policy, 1);
    }

    #[test]
    fn a_malformed_payload_is_counted_and_the_channel_survives() {
        let mut wire = Vec::new();
        framing::encode(aux_proto::MSG_CLIPBOARD, &[99, b'h', b'i'], &mut wire);
        wire.extend_from_slice(&framed_clipboard("fine"));
        let mut got = Vec::new();
        let mut stats = ReaderStats::default();
        let end = pump_reader(
            &wire[..],
            &mut || true,
            &mut |t| got.push(t.to_owned()),
            &mut stats,
        );
        assert_eq!(end, ReaderEnd::Eof);
        assert_eq!(stats.malformed, 1);
        assert_eq!(got, vec!["fine".to_owned()], "one bad payload is not fatal");
    }

    #[test]
    fn a_message_split_across_reads_is_reassembled() {
        // The socket decides where the boundaries fall, not the sender.
        struct Dribble(Vec<u8>, usize);
        impl Read for Dribble {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.1 >= self.0.len() {
                    return Ok(0);
                }
                buf[0] = self.0[self.1];
                self.1 += 1;
                Ok(1)
            }
        }
        let mut got = Vec::new();
        let mut stats = ReaderStats::default();
        let end = pump_reader(
            Dribble(framed_clipboard("a byte at a time"), 0),
            &mut || true,
            &mut |t| got.push(t.to_owned()),
            &mut stats,
        );
        assert_eq!(end, ReaderEnd::Eof);
        assert_eq!(got, vec!["a byte at a time".to_owned()]);
    }

    #[test]
    fn two_messages_in_one_read_both_arrive() {
        let mut wire = framed_clipboard("first");
        wire.extend_from_slice(&framed_clipboard("second"));
        let mut got = Vec::new();
        let mut stats = ReaderStats::default();
        pump_reader(
            &wire[..],
            &mut || true,
            &mut |t| got.push(t.to_owned()),
            &mut stats,
        );
        assert_eq!(got, vec!["first".to_owned(), "second".to_owned()]);
    }
}
