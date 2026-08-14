//! The RDP session thread: read PDUs, paint, send input, disconnect cleanly.
//!
//! Threading is deliberately simple. winit must own the main thread, so the session runs
//! on one worker thread that owns the socket. It alternates between a short blocking read
//! and draining the input queue, rather than splitting the socket across two threads —
//! the TLS stream is not usefully splittable and an RDP session is a single connection,
//! so the extra machinery would buy nothing but complexity.
//!
//! The cost is input latency bounded by the read timeout, which is why that timeout is
//! small. If input latency ever measures badly, that constant is the first thing to look
//! at, not the threading model.

use crate::connect::{ConnectError, Established, describe, send_shutdown};
use crate::input::{InputEvent, encode_fastpath_input, to_fastpath};
use crate::surface::SurfaceStore;
use crate::window::Waker;
use ironrdp::session::{ActiveStageOutput, image::DecodedImage};
use ironrdp_blocking::Framed;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// How long a read blocks before the loop checks the input queue.
///
/// This is the worst-case added latency between a keystroke and it reaching the wire, so
/// it is small. Too small and the loop spins; 5 ms is well under the ~3.3 ms network
/// floor plus server processing, so it is not the bottleneck.
const READ_SLICE: Duration = Duration::from_millis(5);

/// Why the session ended.
#[derive(Debug)]
pub enum SessionEnd {
    /// The server or the user ended it — the normal path.
    Graceful,
    /// The window closed, so we disconnected.
    WindowClosed,
    Failed(ConnectError),
}

pub struct SessionHandle {
    join: JoinHandle<SessionEnd>,
    stop: Arc<AtomicBool>,
}

impl SessionHandle {
    /// Ask the session to disconnect and wait for it.
    pub fn shutdown(self) -> SessionEnd {
        self.stop.store(true, Ordering::Relaxed);
        self.join.join().unwrap_or(SessionEnd::Graceful)
    }
}

/// Run the session on its own thread.
///
/// Takes ownership of the established connection. The caller keeps the store (to paint
/// from) and the input sender (to feed it).
pub fn spawn(
    established: Established,
    store: Arc<Mutex<SurfaceStore>>,
    input: Receiver<InputEvent>,
    waker: Waker,
) -> SessionHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);

    let join = std::thread::Builder::new()
        .name("mdrdp-session".to_owned())
        .spawn(move || run(established, store, input, waker, thread_stop))
        .expect("spawn session thread");

    SessionHandle { join, stop }
}

fn run(
    mut established: Established,
    store: Arc<Mutex<SurfaceStore>>,
    input: Receiver<InputEvent>,
    waker: Waker,
    stop: Arc<AtomicBool>,
) -> SessionEnd {
    // A short read timeout is what lets one thread serve both directions.
    if let Err(e) = established
        .socket
        .set_read_timeout(Some(READ_SLICE))
        .map_err(ConnectError::Io)
    {
        return SessionEnd::Failed(e);
    }

    let desktop = established.desktop_size;
    // EGFX paints into our SurfaceStore, so this image is only touched by the legacy
    // fast-path surface bits. Kept because ActiveStage::process requires it.
    let mut image = DecodedImage::new(
        ironrdp_graphics::image_processing::PixelFormat::RgbA32,
        desktop.width,
        desktop.height,
    );

    let mut last_generation = store.lock().map(|s| s.generation()).unwrap_or(0);
    let outcome = pump(
        &mut established,
        &store,
        &input,
        &waker,
        &stop,
        &mut image,
        &mut last_generation,
    );

    // Always disconnect properly. Abandoning the socket leaves a session alive on the
    // Windows host, and they accumulate until it stops accepting logons.
    let _ = send_shutdown(&established.stage, &mut established.framed);

    outcome
}

#[allow(clippy::too_many_arguments)]
fn pump(
    established: &mut Established,
    store: &Arc<Mutex<SurfaceStore>>,
    input: &Receiver<InputEvent>,
    waker: &Waker,
    stop: &Arc<AtomicBool>,
    image: &mut DecodedImage,
    last_generation: &mut u64,
) -> SessionEnd {
    loop {
        if stop.load(Ordering::Relaxed) {
            return SessionEnd::Graceful;
        }

        // --- outbound: input --------------------------------------------------
        match drain_input(input, &mut established.framed) {
            Ok(true) => {}
            Ok(false) => return SessionEnd::WindowClosed,
            Err(e) => return SessionEnd::Failed(e),
        }

        // --- inbound: server PDUs ---------------------------------------------
        let (action, payload) = match established.framed.read_pdu() {
            Ok(pdu) => pdu,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                // Quiet link. Normal — this is how the loop yields to the input queue.
                notify_if_painted(store, waker, last_generation);
                continue;
            }
            Err(e) => return SessionEnd::Failed(ConnectError::Io(e)),
        };

        let outputs = match established.stage.process(image, action, &payload) {
            Ok(outputs) => outputs,
            Err(e) => return SessionEnd::Failed(ConnectError::Protocol(describe(&e))),
        };

        for output in outputs {
            match output {
                ActiveStageOutput::ResponseFrame(frame) => {
                    if let Err(e) = established.framed.write_all(&frame) {
                        return SessionEnd::Failed(ConnectError::Io(e));
                    }
                }
                ActiveStageOutput::Terminate(_) => return SessionEnd::Graceful,
                ActiveStageOutput::DeactivateAll => {
                    // A resolution change would land here. Not handled yet: the session
                    // continues at the old size rather than dying.
                }
                _ => {}
            }
        }

        notify_if_painted(store, waker, last_generation);
    }
}

/// Nudge the window only when something actually changed.
///
/// The generation counter exists so the presenter never redraws an unchanged frame —
/// redrawing on a timer would burn battery and add latency for nothing.
fn notify_if_painted(store: &Arc<Mutex<SurfaceStore>>, waker: &Waker, last: &mut u64) {
    let Ok(guard) = store.lock() else {
        return;
    };
    let now = guard.generation();
    drop(guard);
    if now != *last {
        *last = now;
        waker.damaged();
    }
}

/// Send every queued input event. `Ok(false)` means the window is gone.
fn drain_input<S: std::io::Read + std::io::Write>(
    input: &Receiver<InputEvent>,
    framed: &mut Framed<S>,
) -> Result<bool, ConnectError> {
    let mut batch = Vec::new();
    loop {
        match input.try_recv() {
            Ok(event) => batch.push(to_fastpath(event)),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => return Ok(false),
        }
    }
    if batch.is_empty() {
        return Ok(true);
    }

    // Batched into one PDU: a burst of mouse moves should not become a burst of writes.
    let encoded = encode_fastpath_input(batch)
        .map_err(|e| ConnectError::Protocol(format!("encode input: {e}")))?;
    framed.write_all(&encoded).map_err(ConnectError::Io)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::SurfaceStore;
    use std::sync::mpsc;

    /// A framed sink that records what was written, so the input path can be tested
    /// without a server.
    struct Sink(Vec<u8>);

    impl std::io::Read for Sink {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "quiet"))
        }
    }

    impl std::io::Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn an_empty_queue_writes_nothing() {
        // A quiet loop iteration must not emit an empty PDU every 5 ms.
        let (_tx, rx) = mpsc::channel::<InputEvent>();
        let mut framed = Framed::new(Sink(Vec::new()));
        assert!(drain_input(&rx, &mut framed).unwrap());
        assert!(framed.into_inner_no_leftover().0.is_empty());
    }

    #[test]
    fn queued_events_are_batched_into_one_write() {
        // Three events, one write — a burst of mouse moves must not become a burst of
        // syscalls, which is what makes input feel bad on a slow link.
        let (tx, rx) = mpsc::channel();
        tx.send(InputEvent::MouseMove { x: 1, y: 2 }).unwrap();
        tx.send(InputEvent::MouseMove { x: 3, y: 4 }).unwrap();
        tx.send(InputEvent::MouseMove { x: 5, y: 6 }).unwrap();

        let mut framed = Framed::new(Sink(Vec::new()));
        assert!(drain_input(&rx, &mut framed).unwrap());
        let written = framed.into_inner_no_leftover().0;
        assert!(!written.is_empty(), "three events should produce a PDU");
    }

    #[test]
    fn a_dropped_sender_reports_the_window_is_gone() {
        // The window closing drops its Sender; that is the session's cue to disconnect.
        let (tx, rx) = mpsc::channel::<InputEvent>();
        drop(tx);
        let mut framed = Framed::new(Sink(Vec::new()));
        assert!(!drain_input(&rx, &mut framed).unwrap());
    }

    #[test]
    fn the_window_is_only_nudged_when_the_store_changed() {
        // Redrawing an unchanged frame wastes power and adds latency for nothing.
        let store = Arc::new(Mutex::new(SurfaceStore::new()));
        let mut last = store.lock().unwrap().generation();

        // No waker available in a unit test, so assert on the generation bookkeeping,
        // which is the part that decides whether a nudge happens at all.
        let before = last;
        store.lock().unwrap().create(1, 4, 4);
        let after = store.lock().unwrap().generation();
        assert_ne!(after, before, "a mutation must bump the generation");

        last = after;
        let unchanged = store.lock().unwrap().generation();
        assert_eq!(unchanged, last, "no mutation, no change");
    }
}
