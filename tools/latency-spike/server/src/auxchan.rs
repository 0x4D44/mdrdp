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
//! Tranche 6 adds audio *above* this tier, under **bounded** priority rather
//! than strict priority: audio goes first, but at most
//! [`AUDIO_BURST_BEFORE_CLIPBOARD`] frames before a waiting clipboard payload is
//! let through. Strict priority was the first design and is a livelock — audio
//! is continuous, so under sustained back-pressure it is always pending and the
//! clipboard is never selected. See that constant for why the distinction
//! matters more than it looks.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Bound a peer that stops reading without penalising legitimately idle channels.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

use crate::aux_proto::{self, AudioFrame, AuxMessage};
use crate::framing::{self, Reassembler};

/// How long a taker parks before looping, so a closed slot is noticed promptly
/// without a busy wait.
pub const SLOT_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// What a taker got.
///
/// `Debug` is **hand-written**, like every other type on this path that can hold
/// a payload. It derived it first, and a `debug!(?take)` in the writer loop
/// printed the whole clipboard — a password, a one-time code — which is the
/// tranche-3 pattern exactly: a type whose derived `Debug` looks innocuous and
/// carries content. AC9's leak check found it. Audio is the same class of
/// content: a voice call is as sensitive as a clipboard.
#[derive(PartialEq, Eq)]
pub enum Take {
    /// A clipboard payload to send.
    Clipboard(String),
    /// One block of PCM to send.
    Audio(AudioFrame),
    /// Nothing pending; the wait elapsed. Keep going.
    Idle,
    /// The session is ending. Stop.
    Closed,
}

impl std::fmt::Debug for Take {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // The variant and a length. Never the bytes.
            Take::Clipboard(text) => f
                .debug_struct("Clipboard")
                .field("bytes", &text.len())
                .finish(),
            // Delegates to AudioFrame's own hand-written Debug, which prints
            // shape and timing and never samples.
            Take::Audio(frame) => f.debug_tuple("Audio").field(frame).finish(),
            Take::Idle => f.write_str("Idle"),
            Take::Closed => f.write_str("Closed"),
        }
    }
}

/// How many audio frames the outbound queue holds before dropping the oldest.
///
/// 20 frames is 200 ms at the 10 ms frame this tranche sends. Bounded so a
/// stalled link cannot grow memory without limit, and drop-**oldest** because
/// when we are behind, stale audio is worthless and the listener wants to catch
/// up rather than hear the past.
pub const AUDIO_FIFO_FRAMES: usize = 20;

/// How many audio frames may be sent before a waiting clipboard payload is let
/// through.
///
/// **This constant is the whole difference between bounded priority and
/// starvation.** The first design drained *all* pending audio before any
/// clipboard, which reads like a clean statement of the priority order and is a
/// livelock: audio is continuous, so under the sustained back-pressure the FIFO
/// exists to survive, audio is *always* pending and the clipboard is never
/// selected — for as long as the congestion lasts, in exactly the window where
/// the user is working. Product requirement 1 is that the clipboard never
/// wedges, so a policy that makes a wedge reachable by construction is wrong
/// however well it expresses the priority.
///
/// Eight frames is 80 ms — below the threshold where a clipboard paste feels
/// delayed, and far above the point where audio would notice.
pub const AUDIO_BURST_BEFORE_CLIPBOARD: usize = 8;

/// The outbound side of the auxiliary channel: an audio FIFO and a keep-latest
/// clipboard slot, drained under bounded priority.
///
/// **One mutex and one condvar, deliberately.** Composing two independently
/// synchronised queues would give two condition variables, and `std` cannot wait
/// on both: a taker parked on one would miss a `put` or a `close` on the other
/// and wake only when the poll interval elapsed. That is not a deadlock, which
/// is what makes it dangerous — it is a silent quarter-second added to every
/// clipboard send and to teardown, on a product whose premise is latency, and it
/// would regress two properties this module already makes failable.
pub struct Outbox {
    state: Mutex<OutboxState>,
    ready: Condvar,
    /// How long a taker parks before looping. Configurable **so the condvar
    /// wake-up is testable**: with the production quarter-second, a test cannot
    /// tell a taker that was woken from one that merely timed out, so removing
    /// the notify would pass every assertion.
    interval: Duration,
}

#[derive(Default)]
struct OutboxState {
    clipboard: Option<String>,
    audio: VecDeque<AudioFrame>,
    closed: bool,
    superseded: u64,
    audio_dropped: u64,
    /// Audio frames taken since the last clipboard payload went out.
    audio_run: usize,
}

impl Outbox {
    pub fn new() -> Arc<Self> {
        Self::with_interval(SLOT_POLL_INTERVAL)
    }

    pub fn with_interval(interval: Duration) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(OutboxState::default()),
            ready: Condvar::new(),
            interval,
        })
    }

    /// Queue clipboard `text`, replacing anything already pending.
    ///
    /// Never blocks and never grows. A payload replaced here was never the
    /// clipboard's current content by the time it would have been sent, so
    /// dropping it is correct rather than merely tolerable.
    pub fn put_clipboard(&self, text: String) {
        let mut state = self.lock();
        if state.closed {
            return;
        }
        if state.clipboard.is_some() {
            state.superseded += 1;
        }
        state.clipboard = Some(text);
        drop(state);
        self.ready.notify_one();
    }

    /// Queue one audio frame, dropping the oldest if the queue is full.
    pub fn put_audio(&self, frame: AudioFrame) -> bool {
        let mut state = self.lock();
        if state.closed {
            return false;
        }
        while state.audio.len() >= AUDIO_FIFO_FRAMES {
            state.audio.pop_front();
            state.audio_dropped += 1;
        }
        state.audio.push_back(frame);
        drop(state);
        self.ready.notify_one();
        true
    }

    /// Wait up to the poll interval for something to send.
    pub fn take(&self) -> Take {
        let mut state = self.lock();
        if let Some(take) = Self::next(&mut state) {
            return take;
        }
        let (mut state, _) = self
            .ready
            .wait_timeout(state, self.interval)
            .unwrap_or_else(|e| {
                let (guard, timeout) = e.into_inner();
                (guard, timeout)
            });
        Self::next(&mut state).unwrap_or(Take::Idle)
    }

    /// The scheduling decision, in one place so it can be reasoned about.
    fn next(state: &mut OutboxState) -> Option<Take> {
        if state.closed {
            // **On close, queued audio is discarded and a pending clipboard
            // payload is still delivered.** `close()` is followed immediately by
            // a socket shutdown, so a writer that drained 200 ms of audio first
            // would find the socket gone and lose the last copy — breaking the
            // property that a copy made a moment before teardown still reaches
            // the peer. Stale audio at teardown is worth nothing; that copy is.
            state.audio.clear();
            return Some(match state.clipboard.take() {
                Some(text) => Take::Clipboard(text),
                None => Take::Closed,
            });
        }
        let clipboard_waiting = state.clipboard.is_some();
        if clipboard_waiting && state.audio_run >= AUDIO_BURST_BEFORE_CLIPBOARD {
            state.audio_run = 0;
            return state.clipboard.take().map(Take::Clipboard);
        }
        if let Some(frame) = state.audio.pop_front() {
            state.audio_run += 1;
            return Some(Take::Audio(frame));
        }
        // Nothing to prioritise against: audio is empty, so the clipboard goes.
        if let Some(text) = state.clipboard.take() {
            state.audio_run = 0;
            return Some(Take::Clipboard(text));
        }
        None
    }

    /// End the session. Wakes any parked taker.
    pub fn close(&self) {
        self.lock().closed = true;
        self.ready.notify_all();
    }

    /// How many queued clipboard payloads were replaced before they could be sent.
    pub fn superseded(&self) -> u64 {
        self.lock().superseded
    }

    /// How many audio frames were dropped because the queue was full.
    pub fn audio_dropped(&self) -> u64 {
        self.lock().audio_dropped
    }

    /// How many audio frames are queued.
    ///
    /// Exists so the discard-on-close rule is observable. Without it that
    /// `clear()` is invisible from outside — the close path returns the
    /// clipboard first either way — and a test claiming to cover it would be
    /// asserting something it cannot see.
    pub fn audio_pending(&self) -> usize {
        self.lock().audio.len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, OutboxState> {
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

#[derive(Debug, PartialEq, Eq)]
pub struct WriterReport {
    pub end: WriterEnd,
    /// Audio frames fully written and flushed to the socket.
    pub audio_written: u64,
}

/// Drain the slot onto the wire until it closes or the socket fails.
///
/// An over-ceiling payload is **reported and dropped, never written**. That is
/// the session-safety property: above the framing layer's own limit the peer
/// would see a terminal `FramingError` and drop the session, so a large copy
/// would kill the remote desktop — worse than having no clipboard.
pub fn pump_writer(
    mut out: impl Write,
    outbox: &Outbox,
    report: &mut impl FnMut(&str),
) -> WriterReport {
    let mut buf = Vec::new();
    let mut audio_written = 0u64;
    loop {
        let (encoded, is_audio) = match outbox.take() {
            Take::Closed => {
                return WriterReport {
                    end: WriterEnd::Closed,
                    audio_written,
                };
            }
            Take::Idle => continue,
            Take::Clipboard(text) => {
                buf.clear();
                let result = match aux_proto::encode_clipboard_text(&text, &mut buf) {
                    Ok(()) => Ok(()),
                    // Names sizes only; `AuxProtoError` carries no content.
                    Err(e) => Err(format!("clipboard not sent: {e}")),
                };
                (result, false)
            }
            Take::Audio(frame) => {
                buf.clear();
                let result = match aux_proto::encode_audio(&frame, &mut buf) {
                    Ok(()) => Ok(()),
                    Err(e) => Err(format!("audio frame not sent: {e}")),
                };
                (result, true)
            }
        };
        if let Err(why) = encoded {
            // A malformed payload is dropped, never written: above the framing
            // layer's limit the peer sees a terminal error and drops the
            // session, so one bad message would kill the remote desktop.
            report(&why);
            continue;
        }
        if let Err(e) = out.write_all(&buf) {
            return WriterReport {
                end: WriterEnd::Io(e.to_string()),
                audio_written,
            };
        }
        if let Err(e) = out.flush() {
            return WriterReport {
                end: WriterEnd::Io(e.to_string()),
                audio_written,
            };
        }
        if is_audio {
            audio_written = audio_written.saturating_add(1);
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
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
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
    /// Audio frames decoded off the wire.
    pub audio_frames: u64,
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
/// Where each kind of inbound message goes.
///
/// A struct rather than four positional closures. With clipboard, audio and
/// audio-control all arriving on one socket the argument list had reached the
/// point where transposing two same-shaped closures would still compile — and
/// the resulting bug (clipboard text handed to the audio sink) is exactly the
/// kind that type-checks and then behaves strangely at runtime.
pub struct ReaderSinks<'a> {
    /// Whether inbound clipboard content is allowed by policy right now.
    pub accepts_clipboard: &'a mut dyn FnMut() -> bool,
    pub on_text: &'a mut dyn FnMut(&str),
    pub on_audio: &'a mut dyn FnMut(AudioFrame),
    /// The peer asking us to start or stop capturing.
    pub on_audio_control: &'a mut dyn FnMut(bool),
}

pub fn pump_reader(
    mut input: impl Read,
    sinks: &mut ReaderSinks<'_>,
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
                Ok(Some(message)) => match message.msg_type {
                    aux_proto::MSG_CLIPBOARD => {
                        if !(sinks.accepts_clipboard)() {
                            stats.refused_by_policy += 1;
                            continue;
                        }
                        stats.decoded += 1;
                        match aux_proto::decode_clipboard(&message.payload) {
                            Ok(AuxMessage::ClipboardText(text)) => (sinks.on_text)(&text),
                            // `decode_clipboard` yields only ClipboardText. Any
                            // other variant here would mean this dispatch was
                            // wired to the wrong decoder — a defect worth
                            // counting rather than a case worth ignoring.
                            Ok(_) | Err(_) => stats.malformed += 1,
                        }
                    }
                    aux_proto::MSG_AUDIO => {
                        // No policy gate: audio is requested by the client with
                        // MSG_AUDIO_CONTROL and stops when it says so, which is
                        // a cleaner control point than refusing frames that have
                        // already crossed the wire.
                        match aux_proto::decode_audio(&message.payload) {
                            Ok(AuxMessage::Audio(frame)) => {
                                stats.audio_frames += 1;
                                (sinks.on_audio)(frame);
                            }
                            Ok(_) | Err(_) => stats.malformed += 1,
                        }
                    }
                    aux_proto::MSG_AUDIO_CONTROL => {
                        match aux_proto::decode_audio_control(&message.payload) {
                            Ok(AuxMessage::AudioControl { enable }) => {
                                (sinks.on_audio_control)(enable)
                            }
                            Ok(_) | Err(_) => stats.malformed += 1,
                        }
                    }
                    _ => stats.unknown_type += 1,
                },
                // Framing is structural: a bad length means the stream is no
                // longer parseable, so there is nothing to resynchronise to.
                Err(e) => return ReaderEnd::Io(format!("{e:?}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    /// Reader sinks for a test that only cares about clipboard.
    ///
    /// A macro rather than a function because the no-op audio closures are
    /// temporaries: expanded at the call site they live to the end of the
    /// enclosing statement, which a function returning the struct could not
    /// arrange.
    macro_rules! clip_sinks {
        ($accepts:expr, $on_text:expr) => {
            &mut ReaderSinks {
                accepts_clipboard: &mut $accepts,
                on_text: &mut $on_text,
                on_audio: &mut |_: AudioFrame| {},
                on_audio_control: &mut |_: bool| {},
            }
        };
    }

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
    fn nothing_on_the_send_path_prints_clipboard_content() {
        // AC9's mechanical leak check, as a test rather than a one-off: every
        // type a `debug!(?x)` at the send site could reach must print no
        // payload byte.
        //
        // This started RED. `Take` derived `Debug`, so `Take::Clipboard(String)`
        // printed the clipboard in full — the exact tranche-3 pattern (a type
        // whose derived Debug looks innocuous and carries content), in code
        // written two units after that lesson was recorded.
        let secret = "hunter2-the-actual-secret";

        let slot = Outbox::with_interval(Duration::from_millis(10));
        slot.put_clipboard(secret.to_owned());
        let taken = slot.take();
        let rendered = format!("{taken:?}");
        assert!(
            !rendered.contains("hunter2"),
            "Take leaked clipboard content: {rendered}"
        );
        // …and still says which variant it is, or it is useless for debugging.
        assert!(
            rendered.contains("Clipboard"),
            "Take must still name its variant: {rendered}"
        );

        // Audio is the same class of content and must be checked here too. A
        // review of the first tranche-6 design pointed out that this test
        // enumerates variants **by hand**, so exhaustiveness does not cover it:
        // a new payload-carrying variant with a derived-looking Debug would sail
        // straight through a green suite. The match below exists to make that
        // impossible — adding a variant fails to compile until it is considered.
        let sample_bytes = vec![0xABu8, 0xCD, 0xAB, 0xCD];
        let audio = Take::Audio(crate::aux_proto::AudioFrame {
            sample_rate: 48_000,
            channels: 2,
            capture_pos: 7,
            pcm: sample_bytes,
        });
        let audio_rendered = format!("{audio:?}");
        assert!(
            !audio_rendered.contains("171") && !audio_rendered.contains("205"),
            "Take leaked audio samples: {audio_rendered}"
        );
        assert!(
            audio_rendered.contains("Audio"),
            "Take must still name its variant: {audio_rendered}"
        );

        // Exhaustive by construction: a future variant will not compile here
        // until someone decides whether it can carry content.
        for variant in [taken, audio, Take::Idle, Take::Closed] {
            match &variant {
                Take::Clipboard(_) | Take::Audio(_) => {
                    let s = format!("{variant:?}");
                    assert!(
                        !s.contains("hunter2") && !s.contains("171"),
                        "a payload-carrying variant leaked: {s}"
                    );
                }
                // The other two carry nothing, and must keep saying so plainly.
                Take::Idle => assert_eq!(format!("{variant:?}"), "Idle"),
                Take::Closed => assert_eq!(format!("{variant:?}"), "Closed"),
            }
        }

        // The reader's counters are the one thing here that is safe to print
        // whole — no content can reach them by construction.
        let stats = ReaderStats::default();
        assert!(!format!("{stats:?}").contains("hunter2"));
    }

    #[test]
    fn the_slot_keeps_the_latest_payload_and_counts_what_it_replaced() {
        let slot = Outbox::new();
        slot.put_clipboard("first".to_owned());
        slot.put_clipboard("second".to_owned());
        assert_eq!(slot.take(), Take::Clipboard("second".to_owned()));
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
        let slot = Outbox::with_interval(Duration::from_millis(10));
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
        let slot = Outbox::with_interval(LONG_PARK);
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
        let slot = Outbox::with_interval(LONG_PARK);
        let taker = Arc::clone(&slot);
        let t = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            (taker.take(), started.elapsed())
        });
        // Let the taker park first, or `put` beats it to the lock and the test
        // proves nothing about waking.
        std::thread::sleep(Duration::from_millis(50));
        slot.put_clipboard("now".to_owned());
        let (outcome, elapsed) = t.join().unwrap();
        assert_eq!(outcome, Take::Clipboard("now".to_owned()));
        assert!(
            elapsed < PROMPT,
            "the payload waited {elapsed:?} for the park to elapse instead of waking the writer"
        );
    }

    #[test]
    fn a_payload_already_pending_when_the_slot_closes_is_still_delivered() {
        // Closing is an orderly end, not a discard: a copy made a moment before
        // teardown should still reach the peer if the socket is still there.
        let slot = Outbox::new();
        slot.put_clipboard("last words".to_owned());
        slot.close();
        assert_eq!(slot.take(), Take::Clipboard("last words".to_owned()));
        assert_eq!(slot.take(), Take::Closed);
    }

    #[test]
    fn putting_after_close_is_dropped_rather_than_queued_forever() {
        let slot = Outbox::new();
        slot.close();
        slot.put_clipboard("too late".to_owned());
        assert_eq!(slot.take(), Take::Closed);
    }

    #[test]
    fn the_writer_frames_a_queued_payload_and_stops_when_the_slot_closes() {
        let slot = Outbox::new();
        slot.put_clipboard("hello\nworld".to_owned());
        slot.close();
        let mut wire = Vec::new();
        let mut reports = Vec::new();
        let report = pump_writer(&mut wire, &slot, &mut |m| reports.push(m.to_owned()));
        assert_eq!(report.end, WriterEnd::Closed);
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
        let slot = Outbox::new();
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

        slot.put_clipboard("x".repeat(aux_proto::MAX_CLIPBOARD_BYTES + 1));
        wait_until(|| reports.lock().unwrap().len() == 1);
        slot.put_clipboard("small".to_owned());
        wait_until(|| !wire.lock().unwrap().is_empty());
        slot.close();
        assert_eq!(writer.join().unwrap().end, WriterEnd::Closed);

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
        let slot = Outbox::new();
        slot.put_clipboard("anything".to_owned());
        slot.close();
        let report = pump_writer(Broken, &slot, &mut |_| {});
        assert!(matches!(report.end, WriterEnd::Io(m) if m.contains("gone")));
    }

    #[test]
    fn the_reader_delivers_clipboard_text_and_ends_cleanly_at_eof() {
        let wire = framed_clipboard("from the host");
        let mut got = Vec::new();
        let mut stats = ReaderStats::default();
        let end = pump_reader(
            &wire[..],
            clip_sinks!(|| true, |t: &str| got.push(t.to_owned())),
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
        // 0x7F, not 0x21: 0x21 became MSG_AUDIO in tranche 6, so the original
        // byte here would now be a malformed AUDIO frame rather than an unknown
        // type — the test would still pass, against a different property.
        framing::encode(0x7F, b"something from a later build", &mut wire);
        wire.extend_from_slice(&framed_clipboard("still here"));
        let mut got = Vec::new();
        let mut stats = ReaderStats::default();
        let end = pump_reader(
            &wire[..],
            clip_sinks!(|| true, |t: &str| got.push(t.to_owned())),
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
            clip_sinks!(|| false, |t: &str| got.push(t.to_owned())),
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
            clip_sinks!(|| true, |t: &str| got.push(t.to_owned())),
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
            clip_sinks!(|| true, |t: &str| got.push(t.to_owned())),
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
            clip_sinks!(|| true, |t: &str| got.push(t.to_owned())),
            &mut stats,
        );
        assert_eq!(got, vec!["first".to_owned(), "second".to_owned()]);
    }

    // -- the outbox: bounded priority (tranche 6) --------------------------------

    fn frame(pos: u64) -> AudioFrame {
        AudioFrame {
            sample_rate: 48_000,
            channels: 2,
            capture_pos: pos,
            // Four bytes = one stereo frame. Content is irrelevant here; the
            // capture position is what each assertion identifies a frame by.
            pcm: vec![0, 0, 0, 0],
        }
    }

    #[test]
    fn audio_is_taken_before_a_waiting_clipboard_payload() {
        let outbox = Outbox::with_interval(Duration::from_millis(10));
        outbox.put_clipboard("later".to_owned());
        outbox.put_audio(frame(1));
        assert_eq!(outbox.take(), Take::Audio(frame(1)));
    }

    #[test]
    fn a_clipboard_payload_cannot_be_starved_by_a_continuous_audio_stream() {
        // **The criterion the first design omitted, and its absence was the
        // tell**: that design drained ALL pending audio before any clipboard.
        // Audio is continuous, so under sustained back-pressure it is always
        // pending and the clipboard would never be selected. Requirement 1 is
        // that the clipboard never wedges.
        //
        // The producer here never stops, which is the case strict priority
        // could not survive.
        let outbox = Outbox::with_interval(Duration::from_millis(10));
        outbox.put_clipboard("must get through".to_owned());

        let mut audio_taken = 0usize;
        for i in 0..1000 {
            // Refill every lap: the queue is never allowed to run dry, so
            // "the clipboard goes when audio happens to be empty" cannot be
            // what rescues this test.
            outbox.put_audio(frame(i));
            match outbox.take() {
                Take::Audio(_) => audio_taken += 1,
                Take::Clipboard(text) => {
                    assert_eq!(text, "must get through");
                    assert!(
                        audio_taken <= AUDIO_BURST_BEFORE_CLIPBOARD,
                        "clipboard waited behind {audio_taken} audio frames, over the bound"
                    );
                    return;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        panic!("the clipboard was starved: {audio_taken} audio frames and it never went");
    }

    #[test]
    fn the_audio_queue_is_bounded_and_drops_the_oldest() {
        let outbox = Outbox::with_interval(Duration::from_millis(10));
        // Two full queues' worth, each identifiable by its capture position.
        for i in 0..(AUDIO_FIFO_FRAMES as u64 * 2) {
            outbox.put_audio(frame(i));
        }
        assert_eq!(
            outbox.audio_dropped(),
            AUDIO_FIFO_FRAMES as u64,
            "one drop for every frame past the bound"
        );
        // What survives must be the NEWEST, not the oldest: stale audio is
        // worthless when we are behind.
        let first = outbox.take();
        assert_eq!(
            first,
            Take::Audio(frame(AUDIO_FIFO_FRAMES as u64)),
            "the oldest surviving frame should be the first of the second batch"
        );
    }

    #[test]
    fn closing_discards_queued_audio_but_still_delivers_a_pending_clipboard() {
        // close() is followed immediately by a socket shutdown, so a writer that
        // drained 200 ms of audio first would find the socket gone and lose the
        // last copy. A copy made a moment before teardown must still reach the
        // peer; stale audio at teardown is worth nothing.
        let outbox = Outbox::with_interval(Duration::from_millis(10));
        for i in 0..AUDIO_FIFO_FRAMES as u64 {
            outbox.put_audio(frame(i));
        }
        outbox.put_clipboard("last words".to_owned());
        outbox.close();

        assert_eq!(outbox.take(), Take::Clipboard("last words".to_owned()));
        assert_eq!(
            outbox.audio_pending(),
            0,
            "queued audio must be discarded at close, not held"
        );
        assert_eq!(outbox.take(), Take::Closed, "and then it is done");
    }

    #[test]
    fn a_queued_audio_frame_wakes_a_parked_taker_rather_than_waiting_for_the_next_lap() {
        // The single-condvar requirement, made failable. With two condvars a
        // taker parked on the clipboard's would miss this notify entirely and
        // wake only when LONG_PARK elapsed -- not a deadlock, which is what makes
        // it dangerous, just a silent delay on a product whose premise is latency.
        let outbox = Outbox::with_interval(LONG_PARK);
        let waiter = Arc::clone(&outbox);
        let started = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            started.0.send(()).unwrap();
            waiter.take()
        });
        started.1.recv().unwrap();
        std::thread::sleep(Duration::from_millis(50));

        let began = std::time::Instant::now();
        outbox.put_audio(frame(42));
        let outcome = handle.join().unwrap();
        let elapsed = began.elapsed();

        assert_eq!(outcome, Take::Audio(frame(42)));
        assert!(
            elapsed < LONG_PARK / 2,
            "the taker waited {elapsed:?}, so it timed out rather than being woken"
        );
    }

    #[test]
    fn putting_audio_after_close_is_dropped_rather_than_queued_forever() {
        let outbox = Outbox::with_interval(Duration::from_millis(10));
        outbox.close();
        outbox.put_audio(frame(1));
        // Asserting on `take()` alone would prove nothing: the close path clears
        // the queue, so this passes whether or not `put_audio` honours `closed`.
        // A mutation pass caught exactly that. What the guard is actually for is
        // memory -- a producer that keeps pushing after teardown, with nobody
        // taking, must not accumulate -- so the queue depth is what to check.
        assert_eq!(
            outbox.audio_pending(),
            0,
            "a frame queued after close would grow without bound"
        );
        assert_eq!(outbox.take(), Take::Closed);
    }

    #[test]
    fn the_writer_frames_an_audio_frame_onto_the_wire() {
        let outbox = Outbox::with_interval(Duration::from_millis(10));
        outbox.put_audio(frame(9));
        let closer = Arc::clone(&outbox);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            closer.close();
        });

        let mut sink = Vec::new();
        let mut said = Vec::new();
        let report = pump_writer(&mut sink, &outbox, &mut |m| said.push(m.to_owned()));
        assert_eq!(report.end, WriterEnd::Closed);
        assert_eq!(report.audio_written, 1);

        let mut reassembler = Reassembler::new(framing::DEFAULT_MAX_PAYLOAD);
        reassembler.push(&sink);
        let message = reassembler
            .next_message()
            .expect("well-formed")
            .expect("one message");
        assert_eq!(message.msg_type, aux_proto::MSG_AUDIO);
        match aux_proto::decode_audio(&message.payload).expect("decodes") {
            AuxMessage::Audio(got) => assert_eq!(got.capture_pos, 9),
            other => panic!("expected audio, got {other:?}"),
        }
        assert!(
            said.is_empty(),
            "a good frame should report nothing: {said:?}"
        );
    }
}
