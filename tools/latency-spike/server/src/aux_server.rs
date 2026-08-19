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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::auxchan::{self, Slot};
use crate::clipboard::{self, Bridge, Policy, TextClipboard};

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
    policy: Policy,
) -> Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
    eprintln!("aux: listening on 127.0.0.1:{port}");
    loop {
        match listener.accept() {
            Ok((stream, peer)) => {
                eprintln!("aux: connected {peer}");
                match serve_one(stream, make_clipboard(), policy) {
                    Ok(stats) => eprintln!(
                        "aux: disconnected — {} applied, {} refused by policy, \
                         {} malformed, {} of an unknown type",
                        stats.decoded - stats.malformed,
                        stats.refused_by_policy,
                        stats.malformed,
                        stats.unknown_type
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
    clipboard: Box<dyn TextClipboard>,
    policy: Policy,
) -> Result<auxchan::ReaderStats> {
    // Clipboard messages are small and bursty; Nagle would add up to 40 ms.
    socket.set_nodelay(true)?;

    let os = Arc::new(Mutex::new(clipboard));
    let bridge = Arc::new(Mutex::new(Bridge::new(policy)));

    // Seed from what the host's clipboard already holds, or the first poll
    // reads as a change and the host clobbers the client's clipboard the
    // instant it connects — with no user action, and no way to get it back.
    {
        let seed = lock(&os).read_text().ok().flatten();
        lock(&bridge).seed(seed.as_deref());
    }

    let slot = Slot::new();
    let stop = Arc::new(AtomicBool::new(false));
    let mut joins = Vec::new();

    let tx_socket = socket.try_clone()?;
    let tx_slot = Arc::clone(&slot);
    joins.push(
        std::thread::Builder::new()
            .name("aux-tx".to_owned())
            .spawn(move || {
                if let auxchan::WriterEnd::Io(reason) =
                    auxchan::pump_writer(tx_socket, &tx_slot, &mut report)
                {
                    report(&format!("channel write failed: {reason}"));
                }
            })?,
    );

    let poll_slot = Arc::clone(&slot);
    let poll_bridge = Arc::clone(&bridge);
    let poll_os = Arc::clone(&os);
    let poll_stop = Arc::clone(&stop);
    joins.push(
        std::thread::Builder::new()
            .name("aux-poll".to_owned())
            .spawn(move || {
                while !poll_stop.load(Ordering::Relaxed) {
                    {
                        let mut bridge = lock(&poll_bridge);
                        let mut os = lock(&poll_os);
                        if let Some(text) =
                            clipboard::poll_local(&mut **os, &mut bridge, &mut report)
                        {
                            poll_slot.put(text);
                        }
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
            })?,
    );

    let mut stats = auxchan::ReaderStats::default();
    let end = auxchan::pump_reader(
        &socket,
        &mut || lock(&bridge).accepts_incoming(),
        &mut |text| {
            let mut bridge = lock(&bridge);
            let mut os = lock(&os);
            clipboard::apply_remote(&mut **os, &mut bridge, text, &mut report);
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
        auxchan::ReaderEnd::Eof => Ok(stats),
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
    }

    impl Pasteboard {
        fn holding(text: &str) -> Self {
            Self {
                text: Arc::new(Mutex::new(Some(text.to_owned()))),
                reads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
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
            *lock(&self.0.text) = Some(text.to_owned());
            Ok(())
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
    fn join_server(
        server: std::thread::JoinHandle<Result<auxchan::ReaderStats>>,
    ) -> auxchan::ReaderStats {
        let done: Arc<Mutex<Option<Result<auxchan::ReaderStats>>>> = Arc::new(Mutex::new(None));
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
    fn connected(
        held: Pasteboard,
        policy: Policy,
    ) -> (
        TcpStream,
        std::thread::JoinHandle<Result<auxchan::ReaderStats>>,
    ) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            serve_one(stream, Box::new(FakeClipboard(held)), policy)
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
                    aux_proto::decode_clipboard(&message.payload).expect("well formed");
                return text;
            }
            let n = client.read(&mut buf).expect("the host should send");
            assert_ne!(n, 0, "the host closed the channel");
            reassembler.push(&buf[..n]);
        }
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
        assert_eq!(stats.refused_by_policy, 1);
        assert_eq!(stats.decoded, 0, "the payload reached the decoder");
    }
}
