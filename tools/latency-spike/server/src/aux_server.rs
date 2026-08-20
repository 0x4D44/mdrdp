//! The host's end of the auxiliary channel: bind, accept, serve one client.
//!
//! Portable on purpose. The Windows-only part of the host's clipboard is the
//! [`crate::clipboard::TextClipboard`] implementation, and everything above it
//! — the accept loop, the three per-connection threads, the teardown — is
//! written against traits and `Read`/`Write`, so it is tested on macOS against
//! a real loopback socket and a fake pasteboard.
//!
//! # One client at a time
//!
//! Mirrors the video socket's single viewer slot: one session owns the host's
//! desktop, so one session owns its clipboard. A second connection waits in the
//! accept backlog rather than being served concurrently, which would give two
//! clients racing writes to one clipboard and no way to say which won.
//!
//! # Why this is not [`crate::auxchan`]'s job
//!
//! `auxchan` owns the loops; this owns the *lifecycle*, and the two ends want
//! genuinely different ones. The client starts its threads under a session that
//! outlives them and never waits for the reader. The host waits for exactly
//! that — the reader returning is how it learns the client went away and the
//! next `accept` may proceed. Sharing the ~40 lines would mean a flag deciding
//! which shape to take, which is worse than writing both.

use std::io::Result;
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::audio_source::{self, AudioSource, Captured};

/// How the channel obtains an audio source.
///
/// Shared and called on the audio thread rather than invoked once by the caller,
/// so a COM-backed source is constructed on the thread that uses it.
pub type AudioFactory = Arc<dyn Fn() -> Box<dyn AudioSource> + Send + Sync>;
use crate::auxchan::{self, Outbox};
use crate::clipboard::{self, Bridge, Policy, TextClipboard};

/// What one connection did, as counts. No content, by construction.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ApplyCounts {
    /// Payloads the clipboard actually took.
    pub written: u64,
    /// Payloads recognised as the host's own content coming back.
    pub suppressed: u64,
    /// Payloads a direction gate or the size ceiling refused.
    ///
    /// **Always zero on the host today**, and the reason is worth knowing
    /// rather than reading a 0 as evidence of anything: the reader's policy
    /// gate refuses an inbound payload *before* it is decoded, and
    /// `decode_clipboard` refuses an oversize one, so neither ever reaches
    /// `apply_remote`. Kept because the variants exist and a future path could
    /// reach them — but it is not a measurement.
    pub refused: u64,
    /// Payloads the clipboard refused to accept — the OS-contention case.
    pub write_failed: u64,
}

impl ApplyCounts {
    fn note(&mut self, applied: clipboard::Applied) {
        match applied {
            clipboard::Applied::Written => self.written += 1,
            clipboard::Applied::Suppressed => self.suppressed += 1,
            clipboard::Applied::Disabled | clipboard::Applied::TooLarge => self.refused += 1,
            clipboard::Applied::WriteFailed => self.write_failed += 1,
        }
    }
}

/// What [`serve_one`] returns: what the reader saw, and what came of it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionReport {
    pub reader: auxchan::ReaderStats,
    pub applied: ApplyCounts,
    /// Non-silent audio frames returned by the source.
    pub audio_produced: u64,
    /// Produced audio frames accepted by the bounded outbox.
    pub audio_queued: u64,
    /// Audio frames fully written and flushed to the socket.
    pub audio_written: u64,
    /// Blocks the source produced that were silence, and so were never sent.
    ///
    /// **This is the fixture-proof, and it is why the field exists.** "Silence
    /// costs nothing on the wire" is satisfied by a working silent source AND by
    /// a source that never started — and every fleet host is currently in the
    /// second state, so on this hardware the criterion would pass for entirely
    /// the wrong reason. A non-zero count here is the evidence that something
    /// actually ran and chose not to send.
    pub audio_silent: u64,
    /// Audio frames the outbox discarded because the link was behind.
    ///
    /// Reported separately from produced/queued/written because they mean different
    /// things to whoever is diagnosing: frames sent is "the source is working",
    /// frames dropped is "the link cannot keep up". A single number would
    /// conflate a healthy quiet session with a congested one.
    pub audio_dropped: u64,
}

/// How often the host's clipboard is read.
///
/// The same quarter-second the client uses. A Windows implementation is
/// expected to make this nearly free by checking `GetClipboardSequenceNumber`
/// before opening the clipboard at all — see [`TextClipboard`].
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Serve the auxiliary channel forever.
///
/// Binds loopback-only: ssh is the security boundary, and nothing on this
/// channel may be reachable off the host. Returns only if the bind fails —
/// a failed *accept* is logged and retried, because one refused connection is
/// not a reason to lose the clipboard for the rest of the host's uptime.
pub fn serve(
    port: u16,
    mut make_clipboard: impl FnMut() -> Box<dyn TextClipboard>,
    make_audio: AudioFactory,
    policy: Policy,
) -> Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
    eprintln!("aux: listening on 127.0.0.1:{port}");
    loop {
        match listener.accept() {
            Ok((stream, peer)) => {
                eprintln!("aux: connected {peer}");
                match serve_one(stream, &mut make_clipboard, &make_audio, policy) {
                    // ASCII only: this goes to server.log, which is read
                    // through the Windows console codepage, where an em-dash
                    // comes out as mojibake.
                    Ok(report) => eprintln!(
                        "aux: disconnected - written {}, echo {}, \
                         write-failed {}, refused-by-policy {}, malformed {}, unknown-type {}, \
                         audio-produced {}, audio-queued {}, audio-written {}, \
                         audio-silent {}, audio-dropped {}",
                        report.applied.written,
                        report.applied.suppressed,
                        report.applied.write_failed,
                        report.reader.refused_by_policy,
                        report.reader.malformed,
                        report.reader.unknown_type,
                        report.audio_produced,
                        report.audio_queued,
                        report.audio_written,
                        report.audio_silent,
                        report.audio_dropped
                    ),
                    Err(e) => eprintln!("aux: connection ended: {e}"),
                }
            }
            Err(e) => {
                eprintln!("aux: accept failed: {e}");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// Serve one connection until the client goes away.
///
/// The reader runs **inline** and its return is the signal that the client has
/// gone; the writer and the clipboard poll run on their own threads and are
/// torn down after it. That ordering is what makes the next `accept` safe: by
/// the time this returns, nothing is still holding the socket or the clipboard.
///
/// Returns what the reader counted. `decoded` is the load-bearing one: it says
/// how many payloads reached the decoder, which is how "refused **before** the
/// content was decoded" can be asserted at all. Refusing after decoding would
/// leave the clipboard equally untouched and be a different, weaker property.
pub fn serve_one(
    socket: TcpStream,
    make_clipboard: &mut dyn FnMut() -> Box<dyn TextClipboard>,
    make_audio: &AudioFactory,
    policy: Policy,
) -> Result<ConnectionReport> {
    // Clipboard messages are small and bursty; Nagle would add up to 40 ms.
    socket.set_nodelay(true)?;

    // **One clipboard handle per thread, not one shared behind a mutex.**
    //
    // Sharing one is the obvious design and it is wrong: a write that blocks —
    // `EmptyClipboard` sending `WM_DESTROYCLIPBOARD` to a hung previous owner
    // is the case this tranche exists to survive — would hold that mutex, and
    // the poll thread would be stuck behind it for as long as the block lasts.
    // The wedge would spread from one direction to both.
    //
    // With separate handles the OS does the serialising it was always going to
    // do, and it does it the right way: the poll thread's `OpenClipboard` fails
    // fast under contention, bounded retry gives up for that lap, and a read
    // failure deliberately leaves the suppression slot alone so the copy is
    // picked up next time. Found by AC4's unit half, which failed against the
    // shared-handle version.
    let apply_os = Arc::new(Mutex::new(make_clipboard()));
    let poll_os = Arc::new(Mutex::new(make_clipboard()));
    let bridge = Arc::new(Mutex::new(Bridge::new(policy)));

    // Seed from what the host's clipboard already holds, or the first poll
    // reads as a change and the host clobbers the client's clipboard the
    // instant it connects — with no user action, and no way to get it back.
    {
        let seed = lock(&poll_os).read_text().ok().flatten();
        bridge
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .seed(seed.as_deref());
    }

    let slot = Outbox::new();
    let stop = Arc::new(AtomicBool::new(false));
    let mut joins = Vec::new();

    let tx_socket = socket.try_clone()?;
    let tx_slot = Arc::clone(&slot);
    let audio_written = Arc::new(AtomicU64::new(0));
    let written_counter = Arc::clone(&audio_written);
    joins.push(
        std::thread::Builder::new()
            .name("aux-tx".to_owned())
            .spawn(move || {
                let writer = auxchan::pump_writer(tx_socket, &tx_slot, &mut report);
                written_counter.store(writer.audio_written, Ordering::Relaxed);
                if let auxchan::WriterEnd::Io(reason) = writer.end {
                    report(&format!("channel write failed: {reason}"));
                }
            })?,
    );

    let poll_slot = Arc::clone(&slot);
    let poll_bridge = Arc::clone(&bridge);
    let poll_os = Arc::clone(&poll_os);
    let poll_stop = Arc::clone(&stop);
    joins.push(
        std::thread::Builder::new()
            .name("aux-poll".to_owned())
            .spawn(move || {
                while !poll_stop.load(Ordering::Relaxed) {
                    {
                        // Only the clipboard handle is locked here; the bridge
                        // locks itself, briefly, inside.
                        let mut os = lock(&poll_os);
                        if let Some(text) =
                            clipboard::poll_local(&mut **os, &poll_bridge, &mut report)
                        {
                            poll_slot.put_clipboard(text);
                        }
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
            })?,
    );

    // **Audio runs only when the client asks.** Nothing is captured or sent
    // until a MSG_AUDIO_CONTROL enable arrives, so a client that does not want
    // audio pays no bandwidth on a channel the parent HLD says must never back
    // up video — and a host with no render endpoint never spins.
    let audio_on = Arc::new(AtomicBool::new(false));
    let audio_slot = Arc::clone(&slot);
    let audio_stop = Arc::clone(&stop);
    let audio_enabled = Arc::clone(&audio_on);
    let build_source = Arc::clone(make_audio);
    let audio_produced = Arc::new(AtomicU64::new(0));
    let produced_counter = Arc::clone(&audio_produced);
    let audio_queued = Arc::new(AtomicU64::new(0));
    let queued_counter = Arc::clone(&audio_queued);
    let audio_silent = Arc::new(AtomicU64::new(0));
    let silent_counter = Arc::clone(&audio_silent);
    joins.push(
        std::thread::Builder::new()
            .name("aux-audio".to_owned())
            .spawn(move || {
                // Built lazily HERE, on the thread that will use it: a WASAPI
                // client is COM and must not be shuffled between threads. The
                // drop on disable is intentional; the next enable edge builds a
                // fresh source so a lost endpoint is re-enumerated.
                let mut source: Option<Box<dyn AudioSource>> = None;
                let mut was_enabled = false;
                let block = Duration::from_millis(audio_source::FRAME_MS as u64);
                let mut said_unavailable = false;
                // **Paced against a deadline, not by sleeping a fixed amount.**
                //
                // `generate; sleep(block)` makes each period `block` PLUS however
                // long generation took, so the source runs permanently slower
                // than real time and the client's ring can never fill. Measured
                // live before this fix: 1882 frames delivered and 1588 underrun
                // episodes across 19 s — a continuous stream of gaps on a link
                // that was dropping nothing. The audio was correct and the
                // timing was not, which is a failure a frame counter cannot see.
                let mut due = std::time::Instant::now();
                while !audio_stop.load(Ordering::Relaxed) {
                    let enabled = audio_enabled.load(Ordering::Relaxed);
                    if enabled && !was_enabled {
                        source = Some(build_source());
                        was_enabled = true;
                        said_unavailable = false;
                        due = std::time::Instant::now();
                    } else if !enabled && was_enabled {
                        source = None;
                        was_enabled = false;
                        due = std::time::Instant::now();
                    }
                    if !enabled {
                        std::thread::sleep(block);
                        // Nothing is owed for time spent disabled.
                        due = std::time::Instant::now();
                        continue;
                    }
                    let Some(source) = source.as_mut() else {
                        std::thread::sleep(block);
                        continue;
                    };
                    match source.next_block() {
                        Captured::Frame(frame) => {
                            if frame.pcm.is_empty() {
                                // One existing-shape boundary frame tells the
                                // client that a quiet window began. It is queued
                                // on the wire, but is neither produced nor
                                // playable audio.
                                silent_counter.fetch_add(1, Ordering::Relaxed);
                            } else {
                                produced_counter.fetch_add(1, Ordering::Relaxed);
                            }
                            if audio_slot.put_audio(frame) {
                                queued_counter.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        // Nothing on the wire, and the capture position has
                        // already advanced inside the source — which is what
                        // keeps elided silence distinguishable from loss.
                        Captured::Silence { .. } => {
                            silent_counter.fetch_add(1, Ordering::Relaxed);
                        }
                        Captured::Empty => {}
                        Captured::Unavailable => {
                            if !said_unavailable {
                                // **Once, not every lap.** A host with no render
                                // endpoint is a supported configuration, and a
                                // log line per 10 ms would turn a supported
                                // state into a fault report.
                                eprintln!(
                                    "aux: no audio render endpoint on this host; \
                                     audio is unavailable (see audio-probe)"
                                );
                                said_unavailable = true;
                            }
                            // Back off hard: there is nothing to poll for.
                            std::thread::sleep(Duration::from_secs(1));
                            due = std::time::Instant::now();
                        }
                    }
                    due += block;
                    let now = std::time::Instant::now();
                    if due > now {
                        std::thread::sleep(due - now);
                    } else {
                        // Behind. Do not try to catch up by generating faster:
                        // the samples are synthesised from a sample counter, so
                        // a burst would be correct audio delivered too quickly
                        // and would simply overrun the ring at the far end.
                        // Give up the debt and carry on from now.
                        due = now;
                    }
                }
            })?,
    );

    // Counted from what `apply_remote` actually did, not from what reached the
    // decoder. Those differ exactly when it matters: a payload refused by the
    // OS clipboard is decoded and NOT applied, and a summary that conflated
    // them would report a refused write as a success. Found while running AC4,
    // where the log said "2 applied" during a clipboard hold and could not tell
    // me whether either write had landed.
    let outcome = Arc::new(Mutex::new(ApplyCounts::default()));
    let reader_outcome = Arc::clone(&outcome);
    let mut stats = auxchan::ReaderStats::default();
    let control_flag = Arc::clone(&audio_on);
    let end = auxchan::pump_reader(
        &socket,
        &mut auxchan::ReaderSinks {
            accepts_clipboard: &mut || {
                bridge
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .accepts_incoming()
            },
            on_text: &mut |text| {
                let mut os = lock(&apply_os);
                let applied = clipboard::apply_remote(&mut **os, &bridge, text, &mut report);
                lock(&reader_outcome).note(applied);
            },
            // The host never receives audio; this direction is host -> client.
            on_audio: &mut |_frame| {},
            on_audio_control: &mut |enable| {
                control_flag.store(enable, Ordering::Relaxed);
            },
        },
        &mut stats,
    );

    // Same order as the client's teardown, for the same reason: closing the
    // slot is what lets the writer return from its park, and dropping the
    // socket is what unblocks anything still in `read`. Joining first would
    // hang here on a thread that has not been told to stop.
    stop.store(true, Ordering::Relaxed);
    slot.close();
    let _ = socket.shutdown(Shutdown::Both);
    for join in joins {
        let _ = join.join();
    }

    match end {
        auxchan::ReaderEnd::Eof => Ok(ConnectionReport {
            reader: stats,
            applied: *lock(&outcome),
            audio_produced: audio_produced.load(Ordering::Relaxed),
            audio_queued: audio_queued.load(Ordering::Relaxed),
            audio_written: audio_written.load(Ordering::Relaxed),
            audio_silent: audio_silent.load(Ordering::Relaxed),
            audio_dropped: slot.audio_dropped(),
        }),
        auxchan::ReaderEnd::Io(reason) => Err(std::io::Error::other(reason)),
    }
}

/// A poisoned clipboard mutex is not worth ending a connection over: the state
/// it guards is one string and a handle, and the next lap rebuilds both.
fn lock<T: ?Sized>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Where this module says things.
///
/// **The host writes an unrotated `server.log` that is read and quoted verbatim
/// during diagnosis**, so the no-content rule binds harder here than on the
/// client. Every caller names sizes and reasons only.
fn report(message: &str) {
    eprintln!("aux: {message}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aux_proto;
    use std::io::{Read, Write};
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    /// A fake host pasteboard the test can change underneath the poll thread
    /// and read back to see what arrived.
    ///
    /// The read counter is not decoration: several tests must change the
    /// content **after** the server has seeded from it, and there is no other
    /// way to know the seed has happened. Changing it first would be captured
    /// by the seed and never sent — a test that fails for the wrong reason, or
    /// worse, passes for one.
    #[derive(Clone, Default)]
    struct Pasteboard {
        text: Arc<Mutex<Option<String>>>,
        reads: Arc<std::sync::atomic::AtomicUsize>,
        /// While true, every write blocks — the OS-refusal case AC4 is about.
        writes_block: Arc<AtomicBool>,
        /// While true, every write is refused outright, as a busy clipboard is.
        writes_refuse: Arc<AtomicBool>,
    }

    impl Pasteboard {
        fn holding(text: &str) -> Self {
            Self {
                text: Arc::new(Mutex::new(Some(text.to_owned()))),
                reads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                writes_block: Arc::new(AtomicBool::new(false)),
                writes_refuse: Arc::new(AtomicBool::new(false)),
            }
        }
        fn empty() -> Self {
            Self::default()
        }
        fn get(&self) -> Option<String> {
            lock(&self.text).clone()
        }
        fn set(&self, text: &str) {
            *lock(&self.text) = Some(text.to_owned());
        }
        /// Block until the server has read at least `n` times, so a change made
        /// afterwards is genuinely a change rather than part of the seed.
        fn await_reads(&self, n: usize) {
            wait_until(
                || self.reads.load(Ordering::SeqCst) >= n,
                "the host to have read its clipboard",
            );
        }
    }

    struct FakeClipboard(Pasteboard);

    impl TextClipboard for FakeClipboard {
        fn read_text(&mut self) -> Result2<Option<String>> {
            self.0.reads.fetch_add(1, Ordering::SeqCst);
            Ok(lock(&self.0.text).clone())
        }
        fn write_text(&mut self, text: &str) -> Result2<()> {
            // Stands in for a write sequence stuck in EmptyClipboard's
            // synchronous WM_DESTROYCLIPBOARD to a hung previous owner: the
            // call simply does not return.
            while self.0.writes_block.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            // And this stands in for the ordinary case: another process holds
            // the clipboard, so the open is refused after bounded retry.
            if self.0.writes_refuse.load(Ordering::SeqCst) {
                return Err("the clipboard was held by another process".to_owned());
            }
            *lock(&self.0.text) = Some(text.to_owned());
            Ok(())
        }
    }

    struct LifecycleSource {
        dropped: Arc<AtomicUsize>,
    }

    impl AudioSource for LifecycleSource {
        fn next_block(&mut self) -> Captured {
            Captured::Empty
        }

        fn describe(&self) -> &'static str {
            "lifecycle-test"
        }
    }

    impl Drop for LifecycleSource {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }

    type Result2<T> = std::result::Result<T, String>;

    /// Join the server thread, but **never** wait forever for it.
    ///
    /// A `serve_one` that cannot tear down is a real defect — it holds the
    /// accept loop shut, so the host has no clipboard until it restarts — and
    /// every test here would otherwise hang the whole suite on it instead of
    /// failing. A hang is not a failure: nothing reports it, nothing names the
    /// test, and CI just stops.
    fn join_server(server: std::thread::JoinHandle<Result<ConnectionReport>>) -> ConnectionReport {
        let done: Arc<Mutex<Option<Result<ConnectionReport>>>> = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&done);
        std::thread::spawn(move || {
            let outcome = server.join();
            *lock(&slot) = Some(outcome.unwrap_or_else(|_| Err(std::io::Error::other("panicked"))));
        });
        wait_until(
            || lock(&done).is_some(),
            "serve_one to return — a clipboard thread is still parked",
        );
        let outcome = lock(&done).take().unwrap();
        outcome.expect("clean end")
    }

    fn wait_until(mut cond: impl FnMut() -> bool, what: &str) {
        let end = Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Connect a client to a `serve_one` running on its own thread.
    /// Like [`connected`], but the host has a working audio source.
    fn connected_with_tone(
        held: Pasteboard,
        policy: Policy,
    ) -> (TcpStream, std::thread::JoinHandle<Result<ConnectionReport>>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut make = || Box::new(FakeClipboard(held.clone())) as Box<dyn TextClipboard>;
            serve_one(
                stream,
                &mut make,
                &(Arc::new(|| {
                    Box::new(crate::audio_source::ToneSource::new(48_000, 2))
                        as Box<dyn crate::audio_source::AudioSource>
                }) as AudioFactory),
                policy,
            )
        });
        let client = TcpStream::connect(addr).unwrap();
        client.set_nodelay(true).unwrap();
        (client, server)
    }

    /// Read framed messages for up to `budget`, returning how many audio frames
    /// arrived.
    fn count_audio_for(client: &mut TcpStream, budget: Duration) -> u64 {
        use std::io::Read;
        client
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut reassembler = crate::framing::Reassembler::new(crate::framing::DEFAULT_MAX_PAYLOAD);
        let mut buf = [0u8; 8192];
        let mut frames = 0u64;
        let deadline = std::time::Instant::now() + budget;
        while std::time::Instant::now() < deadline {
            match client.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => reassembler.push(&buf[..n]),
                Err(_) => {}
            }
            while let Ok(Some(message)) = reassembler.next_message() {
                if message.msg_type == aux_proto::MSG_AUDIO {
                    // Decoded, not merely counted: a byte with the right type
                    // and a malformed body would otherwise pass for audio.
                    aux_proto::decode_audio(&message.payload).expect("a well-formed frame");
                    frames += 1;
                }
            }
        }
        frames
    }

    #[test]
    fn a_silent_source_costs_nothing_on_the_wire_and_is_proved_to_have_run() {
        // **AC5, with the fixture-proof that makes it mean anything.**
        //
        // "Silence produces zero audio bytes" is satisfied just as well by a
        // source that never started, and on every fleet host that is exactly the
        // state things are in — no render endpoint, nothing to capture. So the
        // zero is only evidence when something is known to have run and chosen
        // not to send. `audio_silent` is that something.
        //
        // This repo has paid for the general lesson twice: a clipboard holder
        // that reported success while holding nothing, and a soak generator
        // whose launch silently failed. Same shape, third time, caught in
        // advance rather than after a wasted run.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let held = Pasteboard::holding("irrelevant");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut make = || Box::new(FakeClipboard(held.clone())) as Box<dyn TextClipboard>;
            serve_one(
                stream,
                &mut make,
                &(Arc::new(|| {
                    Box::new(crate::audio_source::SilentSource::new(48_000))
                        as Box<dyn crate::audio_source::AudioSource>
                }) as AudioFactory),
                Policy::default(),
            )
        });
        let mut client = TcpStream::connect(addr).unwrap();
        client.set_nodelay(true).unwrap();

        // Ask for audio, so the source is definitely running.
        let mut request = Vec::new();
        aux_proto::encode_audio_control(true, &mut request);
        client.write_all(&request).unwrap();

        let frames = count_audio_for(&mut client, Duration::from_millis(600));
        assert_eq!(frames, 0, "silence must not be framed onto the wire");

        drop(client);
        let report = join_server(server);
        assert_eq!(
            report.audio_produced, 0,
            "nothing should have been produced"
        );
        assert_eq!(report.audio_queued, 0, "nothing should have been queued");
        assert_eq!(
            report.audio_written, 0,
            "nothing should have reached the socket"
        );
        assert!(
            report.audio_silent > 0,
            "the silent source must be PROVED to have run, or the zero above \
             means nothing: {report:?}"
        );
    }

    #[test]
    fn audio_source_is_created_on_enable_and_dropped_on_disable() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let constructed = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let server_constructed = Arc::clone(&constructed);
        let server_dropped = Arc::clone(&dropped);
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let held = Pasteboard::holding("irrelevant");
            let mut make = || Box::new(FakeClipboard(held.clone())) as Box<dyn TextClipboard>;
            let factory = Arc::new(move || {
                server_constructed.fetch_add(1, Ordering::SeqCst);
                Box::new(LifecycleSource {
                    dropped: Arc::clone(&server_dropped),
                }) as Box<dyn AudioSource>
            }) as AudioFactory;
            serve_one(stream, &mut make, &factory, Policy::default())
        });
        let mut client = TcpStream::connect(addr).unwrap();
        client.set_nodelay(true).unwrap();

        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(constructed.load(Ordering::SeqCst), 0);

        let mut request = Vec::new();
        aux_proto::encode_audio_control(true, &mut request);
        client.write_all(&request).unwrap();
        wait_until(
            || constructed.load(Ordering::SeqCst) == 1,
            "audio source construction after enable",
        );

        request.clear();
        aux_proto::encode_audio_control(false, &mut request);
        client.write_all(&request).unwrap();
        wait_until(
            || dropped.load(Ordering::SeqCst) == 1,
            "audio source drop after disable",
        );

        drop(client);
        let _ = join_server(server);
    }

    #[test]
    fn the_host_sends_no_audio_until_the_client_asks_for_it() {
        // Audio is opt-in on the wire. A host that streamed the moment a client
        // connected would spend bandwidth on a channel the parent HLD says must
        // never back up video, for a client that may have no output device.
        let held = Pasteboard::holding("irrelevant");
        let (mut client, server) = connected_with_tone(held, Policy::default());

        let unasked = count_audio_for(&mut client, Duration::from_millis(400));
        assert_eq!(unasked, 0, "audio arrived without being requested");

        // Now ask, and it should start.
        let mut request = Vec::new();
        aux_proto::encode_audio_control(true, &mut request);
        client.write_all(&request).unwrap();

        let asked = count_audio_for(&mut client, Duration::from_millis(600));
        assert!(
            asked > 0,
            "no audio arrived after asking for it (got {asked} frames)"
        );

        drop(client);
        let report = join_server(server);
        assert!(
            report.audio_produced > 0 && report.audio_queued > 0 && report.audio_written > 0,
            "the host should distinguish production, queueing, and socket writes: {report:?}"
        );
    }

    fn connected(
        held: Pasteboard,
        policy: Policy,
    ) -> (TcpStream, std::thread::JoinHandle<Result<ConnectionReport>>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            // A factory, so each thread gets its own handle exactly as the real
            // host does — sharing one here would test a design we do not ship.
            let mut make = || Box::new(FakeClipboard(held.clone())) as Box<dyn TextClipboard>;
            // The existing tests are about the clipboard; an unavailable
            // source is the right stand-in because it is also the state every
            // fleet host is actually in.
            serve_one(
                stream,
                &mut make,
                &(Arc::new(|| {
                    Box::new(crate::audio_source::UnavailableSource)
                        as Box<dyn crate::audio_source::AudioSource>
                }) as AudioFactory),
                policy,
            )
        });
        let client = TcpStream::connect(addr).unwrap();
        client.set_nodelay(true).unwrap();
        (client, server)
    }

    fn read_one_clipboard_message(client: &mut TcpStream) -> String {
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reassembler = crate::framing::Reassembler::new(crate::framing::DEFAULT_MAX_PAYLOAD);
        let mut buf = [0u8; 4096];
        loop {
            if let Ok(Some(message)) = reassembler.next_message() {
                assert_eq!(message.msg_type, aux_proto::MSG_CLIPBOARD);
                let aux_proto::AuxMessage::ClipboardText(text) =
                    aux_proto::decode_clipboard(&message.payload).expect("well formed")
                else {
                    panic!("the clipboard decoder yielded a non-clipboard message");
                };
                return text;
            }
            let n = client.read(&mut buf).expect("the host should send");
            assert_ne!(n, 0, "the host closed the channel");
            reassembler.push(&buf[..n]);
        }
    }

    #[test]
    fn a_refused_write_is_reported_as_refused_and_never_as_applied() {
        // The summary counted `decoded` and called it "applied". Those differ
        // exactly when it matters — a payload the OS clipboard refuses IS
        // decoded and is NOT applied — so a clipboard held by another process
        // would have been logged as a success.
        //
        // Found running AC4: the host log said "2 applied" during a 10 s
        // clipboard hold, and could not tell me whether either write landed.
        let held = Pasteboard::holding("untouched");
        held.writes_refuse.store(true, Ordering::SeqCst);
        let (mut client, server) = connected(held.clone(), Policy::default());

        let mut wire = Vec::new();
        aux_proto::encode_clipboard_text("the clipboard will refuse this", &mut wire).unwrap();
        client.write_all(&wire).unwrap();
        std::thread::sleep(POLL_INTERVAL * 3);
        assert_eq!(held.get().as_deref(), Some("untouched"));

        drop(client);
        let report = join_server(server);
        assert_eq!(
            report.applied.write_failed, 1,
            "a refused write must be counted as one"
        );
        assert_eq!(report.applied.written, 0, "and never as written");
        // The payload really did reach the decoder — which is why counting
        // decodes as applications was wrong rather than merely imprecise.
        assert_eq!(report.reader.decoded, 1);
    }

    #[test]
    fn a_stuck_clipboard_write_does_not_stop_the_poll_loop() {
        // AC4's unit half. The wedge this tranche exists to beat is
        // EmptyClipboard blocking on a hung previous owner while the clipboard
        // is held open for the whole desktop. It cannot be prevented — the
        // question is whether it takes the rest of the session with it.
        //
        // Here a write blocks for ever. The poll thread is separate, so it must
        // keep reading and keep sending: the session stays alive and the
        // clipboard keeps working in the direction that is not stuck.
        let held = Pasteboard::holding("start");
        held.writes_block.store(true, Ordering::SeqCst);
        let (mut client, server) = connected(held.clone(), Policy::default());

        // Send a payload the host will block trying to apply.
        let mut wire = Vec::new();
        aux_proto::encode_clipboard_text("this write will hang", &mut wire).unwrap();
        client.write_all(&wire).unwrap();

        // The reads counter is the oracle: it can only advance if the poll
        // thread is still running its loop while the writer is stuck.
        let before = held.reads.load(Ordering::SeqCst);
        wait_until(
            || held.reads.load(Ordering::SeqCst) > before + 2,
            "the poll thread to keep reading while a write is stuck",
        );

        // And the direction that is not stuck still works.
        held.set("copied while the write is stuck");
        assert_eq!(
            read_one_clipboard_message(&mut client),
            "copied while the write is stuck"
        );

        // Release the write so teardown can finish, then confirm it does.
        held.writes_block.store(false, Ordering::SeqCst);
        drop(client);
        let _ = join_server(server);
    }

    #[test]
    fn a_change_on_the_host_clipboard_reaches_the_client() {
        let held = Pasteboard::holding("before the session");
        let (mut client, server) = connected(held.clone(), Policy::default());

        // Changed only once the seed has happened, so this cannot pass on a
        // seed wrongly sent as a change — and cannot fail because the change
        // was swallowed by the seed.
        held.await_reads(1);
        held.set("copied on the host");

        assert_eq!(
            read_one_clipboard_message(&mut client),
            "copied on the host"
        );
        drop(client);
        let _ = join_server(server);
    }

    #[test]
    fn what_the_host_already_held_at_connect_is_never_pushed() {
        // The seed. Without it the host's existing clipboard lands on the
        // client's the instant it connects, with no user action at all.
        let held = Pasteboard::holding("was already here");
        let (mut client, server) = connected(held, Policy::default());

        client
            .set_read_timeout(Some(Duration::from_millis(800)))
            .unwrap();
        let mut buf = [0u8; 64];
        let quiet = match client.read(&mut buf) {
            Ok(0) => true,
            Ok(_) => false,
            Err(_) => true, // the timeout: nothing arrived, which is the point
        };
        assert!(quiet, "the host pushed its pre-existing clipboard unasked");
        drop(client);
        let _ = join_server(server);
    }

    #[test]
    fn text_from_the_client_lands_on_the_host_clipboard() {
        let held = Pasteboard::empty();
        let (mut client, server) = connected(held.clone(), Policy::default());

        let mut wire = Vec::new();
        aux_proto::encode_clipboard_text("from the client", &mut wire).unwrap();
        client.write_all(&wire).unwrap();

        wait_until(
            || held.get().as_deref() == Some("from the client"),
            "the host clipboard to be written",
        );
        drop(client);
        let _ = join_server(server);
    }

    #[test]
    fn applied_text_is_not_echoed_straight_back() {
        // The host's own poll will read what the client just sent. If it were
        // sent back, the client would apply it, its poll would see a change,
        // and round it would go at poll cadence forever.
        let held = Pasteboard::empty();
        let (mut client, server) = connected(held.clone(), Policy::default());

        let mut wire = Vec::new();
        aux_proto::encode_clipboard_text("no echo please", &mut wire).unwrap();
        client.write_all(&wire).unwrap();
        wait_until(
            || held.get().as_deref() == Some("no echo please"),
            "the host clipboard to be written",
        );

        // Several poll intervals of silence.
        client.set_read_timeout(Some(POLL_INTERVAL * 4)).unwrap();
        let mut buf = [0u8; 64];
        assert!(
            matches!(client.read(&mut buf), Err(_) | Ok(0)),
            "the host echoed back what the client sent it"
        );
        drop(client);
        let _ = join_server(server);
    }

    #[test]
    fn the_client_going_away_ends_the_connection_and_frees_the_host() {
        // The accept loop must be able to take the next client. If a thread
        // were still parked on the socket or the clipboard, this would hang —
        // which is why the join is bounded rather than trusted.
        let (client, server) = connected(Pasteboard::holding("anything"), Policy::default());
        drop(client);

        let _ = join_server(server);
    }

    #[test]
    fn to_remote_off_means_the_host_never_pushes_a_change() {
        let held = Pasteboard::holding("start");
        let (mut client, server) = connected(
            held.clone(),
            Policy {
                to_remote: false,
                ..Policy::default()
            },
        );
        held.await_reads(1);
        held.set("copied on the host");

        client.set_read_timeout(Some(POLL_INTERVAL * 4)).unwrap();
        let mut buf = [0u8; 64];
        assert!(
            matches!(client.read(&mut buf), Err(_) | Ok(0)),
            "the host pushed a change with to_remote off"
        );
        drop(client);
        let _ = join_server(server);
    }

    #[test]
    fn from_remote_off_leaves_the_host_clipboard_untouched() {
        let held = Pasteboard::holding("host content");
        let (mut client, server) = connected(
            held.clone(),
            Policy {
                from_remote: false,
                ..Policy::default()
            },
        );

        let mut wire = Vec::new();
        aux_proto::encode_clipboard_text("should be refused", &mut wire).unwrap();
        client.write_all(&wire).unwrap();
        std::thread::sleep(POLL_INTERVAL * 3);
        assert_eq!(held.get().as_deref(), Some("host content"));
        drop(client);

        // The stronger claim, and the reason the reader takes a gate at all:
        // the payload must be refused BEFORE it is decoded. An untouched
        // clipboard alone would also pass on an implementation that decoded the
        // content and then threw it away.
        let stats = join_server(server);
        assert_eq!(stats.reader.refused_by_policy, 1);
        assert_eq!(stats.reader.decoded, 0, "the payload reached the decoder");
    }
}
