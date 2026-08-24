//! The client-side stats file: one JSON object per line.
//!
//! Every `*_us` field is an absolute stamp on **this machine's** clock (see
//! [`crate::clock`]). Differences between fields on one line are the stage costs.
//! Server stamps are on the server's QPC and are never comparable to these — the two
//! clocks are not synchronised, by design — so server lines are passed through in a
//! wrapper that keeps them visibly separate rather than merged into the same shape.
//!
//! A dedicated writer thread flushes lines individually. The bounded handoff never
//! makes decode or present wait for disk; overload is recorded explicitly. The
//! operator ends a run with Ctrl-C or by closing the window, and a buffered tail lost
//! at that moment is a measurement lost.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;

/// Bumped whenever a field changes meaning, so an archived file stays readable.
/// Schema 2: frames gained `seq` — the server's capture sequence number from
/// `MSG_VIDEO_SEQ`, the exact cross-file join key (`null` on a v1 stream).
/// Schema 3: adds `rects` records (the raw dirty-rect fast path) and `suppressed`
/// on frame records — so `dropped` no longer means "not presented" on its own.
/// Schema 4: adds `partial` on both painted shapes — which convert path put this
/// snapshot on screen, so an A/B can tell whether the damage-only path carried a run
/// rather than assuming the build implies it.
pub const SCHEMA: u32 = 4;

/// First line of the file: what this run was, and on which clock.
#[derive(Debug, Clone, Serialize)]
pub struct Header {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub schema: u32,
    /// Which clock every `*_us` field below is on.
    pub clock: &'static str,
    pub connect: String,
    pub input: String,
    /// Whether `mdrdp::hevc::hardware_decoder()` returned a decoder on this build.
    /// Without one, every access unit is a `decode_error` and the window stays black.
    pub decoder: bool,
}

impl Header {
    pub fn new(clock: &'static str, connect: String, input: String, decoder: bool) -> Self {
        Self {
            kind: "client-header",
            schema: SCHEMA,
            clock,
            connect,
            input,
            decoder,
        }
    }
}

/// The client-side stage stamps of one frame, carried from the decode thread to the
/// window thread so `present_done_us` can close the line where it is actually taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameStamps {
    /// Arrival ordinal on this client (counts every video message seen).
    pub frame: u64,
    /// The server's capture sequence number, when the stream carried one.
    pub seq: Option<u64>,
    /// When the `read` that completed this message returned.
    pub recv_done_us: u64,
    /// Immediately before `VideoDecoder::decode`.
    pub decode_in_us: u64,
    /// Immediately after it returned.
    pub decode_out_us: u64,
    pub au_bytes: usize,
    pub keyframe: bool,
    pub width: u32,
    pub height: u32,
}

/// The client-side stamps of one `MSG_RECTS` update, carried from the network thread
/// to the window thread exactly as [`FrameStamps`] is — a canvas snapshot published by
/// the rect path still owes the file a line, and only the presenter knows when it lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RectStamps {
    /// The server's capture sequence number for the frame these rects belong to.
    /// Always present: `MSG_RECTS` is a wire-v2 message, so there is no legacy case.
    pub seq: u64,
    /// When the `read` that completed this message returned.
    pub recv_done_us: u64,
    /// Immediately after the last rect was blitted into the canvas.
    pub paint_done_us: u64,
    pub rect_count: u32,
    /// Pixel bytes across all the rects, excluding the per-rect headers.
    pub rect_bytes: usize,
}

/// One frame, end to end on the client.
///
/// `dropped` is `present_done_us.is_none() && !suppressed`. A suppressed frame was
/// never a candidate to present — the rect path had already painted its content, so
/// the ordering rule withheld the decoded picture on purpose — and counting it as
/// dropped would overstate how much the pipeline actually lost. A genuine drop is
/// still recorded rather than discarded, for the same reason.
#[derive(Debug, Clone, Serialize)]
pub struct FrameRecord {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub frame: u64,
    /// `null` only on a v1 stream that sent bare `MSG_VIDEO`.
    pub seq: Option<u64>,
    pub recv_done_us: u64,
    pub decode_in_us: u64,
    pub decode_out_us: u64,
    pub present_done_us: Option<u64>,
    pub au_bytes: usize,
    pub keyframe: bool,
    pub width: u32,
    pub height: u32,
    pub dropped: bool,
    /// The decoded picture was withheld from the canvas by the ordering rule: rects
    /// for this same frame had already painted it. The decoder still ran, so the
    /// decode stamps on this line are real.
    pub suppressed: bool,
    /// This snapshot reached the screen through the damage-only convert (the window
    /// thread's persistent canvas took just the damaged rects) rather than a
    /// full-surface `present_into`. Always `false` on a record with no present stamp:
    /// nothing converted anything for a snapshot that was never shown.
    pub partial: bool,
}

impl FrameRecord {
    pub fn new(stamps: &FrameStamps, present_done_us: Option<u64>, partial: bool) -> Self {
        Self {
            kind: "frame",
            frame: stamps.frame,
            seq: stamps.seq,
            recv_done_us: stamps.recv_done_us,
            decode_in_us: stamps.decode_in_us,
            decode_out_us: stamps.decode_out_us,
            present_done_us,
            au_bytes: stamps.au_bytes,
            keyframe: stamps.keyframe,
            width: stamps.width,
            height: stamps.height,
            dropped: present_done_us.is_none(),
            suppressed: false,
            partial,
        }
    }

    /// A decoded access unit the ordering rule kept off the canvas. It has no present
    /// stamp and never will, and that is not a loss.
    pub fn suppressed(stamps: &FrameStamps) -> Self {
        Self {
            kind: "frame",
            frame: stamps.frame,
            seq: stamps.seq,
            recv_done_us: stamps.recv_done_us,
            decode_in_us: stamps.decode_in_us,
            decode_out_us: stamps.decode_out_us,
            present_done_us: None,
            au_bytes: stamps.au_bytes,
            keyframe: stamps.keyframe,
            width: stamps.width,
            height: stamps.height,
            dropped: false,
            suppressed: true,
            // It never reached a canvas at all, so no convert path claims it.
            partial: false,
        }
    }
}

/// One `MSG_RECTS` update, end to end on the client — the fast path's own line.
///
/// `skipped` is the visible counter the HLD demands: a rect update the viewer could
/// not composite is never silently discarded, because "the fast path was not taken"
/// is exactly the thing a run has to be able to measure.
#[derive(Debug, Clone, Serialize)]
pub struct RectRecord {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub seq: u64,
    pub recv_done_us: u64,
    /// `null` when the update was skipped: nothing was painted.
    pub paint_done_us: Option<u64>,
    pub present_done_us: Option<u64>,
    pub rect_count: u32,
    pub rect_bytes: usize,
    /// Why this update painted nothing, or `null` when it painted.
    pub skipped: Option<&'static str>,
    pub dropped: bool,
    /// This snapshot reached the screen through the damage-only convert rather than a
    /// full-surface `present_into` — the fast path the rect path exists to feed.
    /// Always `false` when nothing was presented.
    pub partial: bool,
}

impl RectRecord {
    /// An update that was blitted into the canvas. `present_done_us` is `None` when a
    /// newer snapshot displaced this one before the window thread ever showed it.
    pub fn painted(stamps: &RectStamps, present_done_us: Option<u64>, partial: bool) -> Self {
        Self {
            kind: "rects",
            seq: stamps.seq,
            recv_done_us: stamps.recv_done_us,
            paint_done_us: Some(stamps.paint_done_us),
            present_done_us,
            rect_count: stamps.rect_count,
            rect_bytes: stamps.rect_bytes,
            skipped: None,
            dropped: present_done_us.is_none(),
            partial,
        }
    }

    /// An update the viewer refused to composite. `reason` is `"before_base"` (no
    /// canvas yet — rects can beat the first decodable keyframe), `"size_mismatch"`
    /// (the update's declared frame size is not the canvas's), `"gap"` / `"stale"`
    /// (the seq is not adjacent to the canvas's exactness — see
    /// `sink::Canvas::rects_skip_reason`), or `"empty"` (zero rects: painting
    /// nothing must not claim a frame's content). Not a drop: nothing was painted,
    /// so nothing was lost between paint and present.
    pub fn skipped(
        seq: u64,
        recv_done_us: u64,
        rect_count: u32,
        rect_bytes: usize,
        reason: &'static str,
    ) -> Self {
        Self {
            kind: "rects",
            seq,
            recv_done_us,
            paint_done_us: None,
            present_done_us: None,
            rect_count,
            rect_bytes,
            skipped: Some(reason),
            dropped: false,
            // Nothing was painted, so no convert path ran for it.
            partial: false,
        }
    }
}

/// An access unit the decoder refused.
///
/// Expected at the head of a stream — the server's first frames can reach us before
/// the parameter sets — and self-healing at the next keyframe. A run of these that
/// never stops is the real signal, which is why every one is recorded.
#[derive(Debug, Clone, Serialize)]
pub struct DecodeErrorRecord {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub frame: u64,
    pub recv_done_us: u64,
    pub au_bytes: usize,
    pub keyframe: bool,
    pub detail: String,
}

impl DecodeErrorRecord {
    pub fn new(
        frame: u64,
        recv_done_us: u64,
        au_bytes: usize,
        keyframe: bool,
        detail: String,
    ) -> Self {
        Self {
            kind: "decode_error",
            frame,
            recv_done_us,
            au_bytes,
            keyframe,
            detail,
        }
    }
}

/// One keystroke this viewer put on the input channel.
///
/// `seq` is the record's own sequence number, echoed by the server's `input` stats
/// line, which is what lets a keystroke be followed across the two files despite the
/// two clocks never being comparable.
#[derive(Debug, Clone, Serialize)]
pub struct InputRecord {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub seq: u32,
    pub vk: u16,
    pub transition: &'static str,
    pub sent_us: u64,
}

impl InputRecord {
    pub fn new(seq: u32, vk: u16, transition: &'static str, sent_us: u64) -> Self {
        Self {
            kind: "input",
            seq,
            vk,
            transition,
            sent_us,
        }
    }
}

/// Serialise one record as a JSONL line (no trailing newline).
///
/// Falls back to a minimal error object rather than panicking: losing one stats line
/// must never take the viewer down.
pub fn to_line<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|e| format!(r#"{{"type":"error","detail":"serialize: {e}"}}"#))
}

/// Wrap a server stats line for the client's file, keeping its bytes intact.
///
/// The server's own lines are already JSON and use `record` as their discriminator,
/// where ours use `type`; merging them into one namespace would make a reader guess.
/// So a well-formed line is nested verbatim under `line` — the original bytes are
/// concatenated, not re-serialised, so nothing is reordered or reformatted — and
/// anything that is not valid JSON is escaped into `text` instead of being dropped.
pub fn server_line(payload: &[u8]) -> String {
    match std::str::from_utf8(payload) {
        Ok(text) if serde_json::from_str::<serde_json::Value>(text).is_ok() => {
            format!(r#"{{"type":"server","line":{text}}}"#)
        }
        Ok(text) => to_line(&serde_json::json!({"type": "server_raw", "text": text})),
        Err(_) => to_line(&serde_json::json!({
            "type": "server_raw",
            "text": null,
            "bytes": payload.len(),
        })),
    }
}

/// Enough room for ordinary writer jitter without turning a sustained slow disk into
/// unbounded memory growth. Producers never wait for this queue: overflow is counted
/// and written as an explicit `stats_drop` record when the writer catches up.
const WRITE_QUEUE_CAPACITY: usize = 1_024;
/// Cap the queue by bytes as well as records. Legitimate stats JSON is a few KiB;
/// video access units use another message type and never enter this queue.
const MAX_QUEUED_LINE_BYTES: usize = 16 * 1024;
/// Bound work on untrusted server stats before UTF-8 validation or JSON wrapping.
/// JSON escaping can expand one input byte to six output bytes.
const MAX_SERVER_LINE_BYTES: usize = 2 * 1024;
const FLUSH_TIMEOUT: Duration = Duration::from_secs(1);

enum WriterCommand {
    Line(String),
    Flush(mpsc::SyncSender<()>),
}

/// The stats file, or a sink that discards everything when `--out` was not given.
pub struct StatsLog {
    sender: Option<mpsc::SyncSender<WriterCommand>>,
    enqueue_gate: RwLock<()>,
    closed: AtomicBool,
    dropped: Arc<AtomicU64>,
    write_failed: Arc<AtomicBool>,
    /// A write failure is reported once. A per-frame warning would bury the run's own
    /// output under thousands of identical lines.
    writer_warned: AtomicBool,
}

impl StatsLog {
    pub fn create(path: Option<&str>) -> std::io::Result<Self> {
        match path {
            Some(p) => Self::with_writer(Box::new(BufWriter::new(File::create(p)?))),
            None => Ok(Self::discarding()),
        }
    }

    /// A log that writes nowhere — for tests and for a run without `--out`.
    pub fn discarding() -> Self {
        Self {
            sender: None,
            enqueue_gate: RwLock::new(()),
            closed: AtomicBool::new(false),
            dropped: Arc::new(AtomicU64::new(0)),
            write_failed: Arc::new(AtomicBool::new(false)),
            writer_warned: AtomicBool::new(false),
        }
    }

    fn with_writer(mut out: Box<dyn Write + Send>) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(WRITE_QUEUE_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        let worker_dropped = dropped.clone();
        let write_failed = Arc::new(AtomicBool::new(false));
        let worker_write_failed = write_failed.clone();
        drop(
            std::thread::Builder::new()
                .name("spike-stats".to_owned())
                .spawn(move || {
                    let mut write_warned = false;
                    while let Ok(command) = receiver.recv() {
                        if !write_dropped_count(&mut *out, &worker_dropped, &mut write_warned) {
                            worker_write_failed.store(true, Ordering::Relaxed);
                        }
                        match command {
                            WriterCommand::Line(line) => {
                                if !write_one_line(&mut *out, &line, &mut write_warned) {
                                    worker_dropped.fetch_add(1, Ordering::Relaxed);
                                    worker_write_failed.store(true, Ordering::Relaxed);
                                }
                            }
                            WriterCommand::Flush(done) => {
                                if !flush_writer(&mut *out, &mut write_warned) {
                                    worker_write_failed.store(true, Ordering::Relaxed);
                                }
                                let _ = done.send(());
                            }
                        }
                    }
                    // The final sender disappeared. Drain any loss count that raced the
                    // last queued command, then make the best bounded-process-lifetime
                    // effort to persist the tail.
                    if !write_dropped_count(&mut *out, &worker_dropped, &mut write_warned) {
                        worker_write_failed.store(true, Ordering::Relaxed);
                    }
                    if !flush_writer(&mut *out, &mut write_warned) {
                        worker_write_failed.store(true, Ordering::Relaxed);
                    }
                })?,
        );
        Ok(Self {
            sender: Some(sender),
            enqueue_gate: RwLock::new(()),
            closed: AtomicBool::new(false),
            dropped,
            write_failed,
            writer_warned: AtomicBool::new(false),
        })
    }

    pub fn record<T: Serialize>(&self, value: &T) {
        self.enqueue(to_line(value));
    }

    pub fn write_line(&self, line: &str) {
        if line.len() > MAX_QUEUED_LINE_BYTES {
            self.count_drop();
            return;
        }
        self.enqueue(line.to_owned());
    }

    pub fn record_server_line(&self, payload: &[u8]) {
        if payload.len() > MAX_SERVER_LINE_BYTES {
            self.count_drop();
            return;
        }
        self.enqueue(server_line(payload));
    }

    fn enqueue(&self, line: String) {
        let Some(sender) = &self.sender else {
            return;
        };
        if line.len() > MAX_QUEUED_LINE_BYTES {
            self.count_drop();
            return;
        }
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        let _gate = match self.enqueue_gate.try_read() {
            Ok(gate) => gate,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        match sender.try_send(WriterCommand::Line(line)) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                self.warn_writer_once("stats: writer stopped; further records are discarded");
            }
        }
    }

    fn count_drop(&self) {
        if self.sender.is_none() || self.closed.load(Ordering::SeqCst) {
            return;
        }
        let _gate = match self.enqueue_gate.try_read() {
            Ok(gate) => gate,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        if !self.closed.load(Ordering::SeqCst) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn flush(&self) -> bool {
        // The gate establishes that no producer can pass the final flush marker.
        // A producer racing shutdown either finishes first or observes `closed`.
        self.closed.store(true, Ordering::SeqCst);
        let _gate = self
            .enqueue_gate
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ok = self.flush_for(FLUSH_TIMEOUT);
        if !ok {
            self.warn_writer_once(
                "stats: final flush failed or timed out; the unwritten tail may be lost",
            );
        }
        ok
    }

    fn flush_for(&self, timeout: Duration) -> bool {
        let Some(sender) = &self.sender else {
            return true;
        };
        let (done_tx, done_rx) = mpsc::sync_channel(0);
        let deadline = Instant::now() + timeout;
        let mut command = WriterCommand::Flush(done_tx);
        loop {
            match sender.try_send(command) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(returned)) => {
                    command = returned;
                    let now = Instant::now();
                    if now >= deadline {
                        return false;
                    }
                    std::thread::sleep((deadline - now).min(Duration::from_millis(1)));
                }
                Err(mpsc::TrySendError::Disconnected(_)) => return false,
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        done_rx.recv_timeout(deadline - now).is_ok() && !self.write_failed.load(Ordering::Relaxed)
    }

    fn warn_writer_once(&self, message: &str) {
        if !self.writer_warned.swap(true, Ordering::Relaxed) {
            eprintln!("{message}");
        }
    }
}

fn write_dropped_count(out: &mut dyn Write, dropped: &AtomicU64, warned: &mut bool) -> bool {
    let count = dropped.swap(0, Ordering::Relaxed);
    if count != 0
        && !write_one_line(
            out,
            &format!(r#"{{"type":"stats_drop","lines":{count}}}"#),
            warned,
        )
    {
        dropped.fetch_add(count, Ordering::Relaxed);
        false
    } else {
        true
    }
}

fn write_one_line(out: &mut dyn Write, line: &str, warned: &mut bool) -> bool {
    // Flushed per line: see the module docs. Only the dedicated writer thread waits.
    if let Err(e) = writeln!(out, "{line}").and_then(|()| out.flush()) {
        if !*warned {
            *warned = true;
            eprintln!("stats: write failed, further failures are silent: {e}");
        }
        false
    } else {
        true
    }
}

fn flush_writer(out: &mut dyn Write, warned: &mut bool) -> bool {
    if let Err(e) = out.flush() {
        if !*warned {
            *warned = true;
            eprintln!("stats: flush failed, further failures are silent: {e}");
        }
        false
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    struct BlockingWriter {
        started: Option<std::sync::mpsc::Sender<()>>,
        release: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
        written: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }

    impl Write for BlockingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if let Some(started) = self.started.take() {
                let _ = started.send(());
            }
            let (lock, ready) = &*self.release;
            let mut released = lock.lock().expect("release mutex");
            while !*released {
                released = ready.wait(released).expect("release wait");
            }
            self.written
                .lock()
                .expect("written bytes mutex")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct FailFirstWriter {
        fail: bool,
        written: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }

    impl Write for FailFirstWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.fail {
                self.fail = false;
                return Err(std::io::Error::other("injected stats write failure"));
            }
            self.written
                .lock()
                .expect("written bytes mutex")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    type WriterGate = std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>;

    fn blocked_log() -> (
        StatsLog,
        std::sync::mpsc::Receiver<()>,
        WriterGate,
        std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    ) {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let release =
            std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = StatsLog::with_writer(Box::new(BlockingWriter {
            started: Some(started_tx),
            release: release.clone(),
            written: written.clone(),
        }))
        .expect("start stats writer");
        (log, started_rx, release, written)
    }

    fn release_writer(release: &WriterGate) {
        let (lock, ready) = &**release;
        *lock.lock().expect("release mutex") = true;
        ready.notify_all();
    }

    fn parse(line: &str) -> Value {
        serde_json::from_str(line).expect("a stats line must be valid JSON")
    }

    #[test]
    fn a_presented_frame_carries_every_stage_and_is_not_dropped() {
        // Every stamp a different value: a fixture where they agreed could not catch
        // two fields being written from the same variable.
        let stamps = FrameStamps {
            frame: 7,
            seq: Some(42),
            recv_done_us: 1_000,
            decode_in_us: 1_100,
            decode_out_us: 3_400,
            au_bytes: 4_321,
            keyframe: true,
            width: 1920,
            height: 1080,
        };
        // `keyframe: true` against `partial: false` is deliberate: with every bool the
        // same value, a line that serialised one under another's name would pass.
        let v = parse(&to_line(&FrameRecord::new(&stamps, Some(5_000), false)));
        assert_eq!(v["type"], "frame");
        assert_eq!(v["frame"], 7);
        assert_eq!(
            v["seq"], 42,
            "seq must be the server's number, not the ordinal"
        );
        assert_eq!(v["recv_done_us"], 1_000);
        assert_eq!(v["decode_in_us"], 1_100);
        assert_eq!(v["decode_out_us"], 3_400);
        assert_eq!(v["present_done_us"], 5_000);
        assert_eq!(v["au_bytes"], 4_321);
        assert_eq!(v["keyframe"], true);
        assert_eq!(v["width"], 1920);
        assert_eq!(v["height"], 1080);
        assert_eq!(v["dropped"], false);
        assert_eq!(v["suppressed"], false);
        assert_eq!(v["partial"], false, "a full convert put this one on screen");
    }

    #[test]
    fn a_frame_presented_by_the_damage_only_path_says_which_path_it_was() {
        // The A/B's whole question: did the partial path actually carry the run? Here
        // `partial` is the only true bool on the line, so a field written under the
        // wrong name shows up as one of the other three flipping.
        let stamps = FrameStamps {
            frame: 11,
            seq: Some(52),
            recv_done_us: 1_000,
            decode_in_us: 1_100,
            decode_out_us: 1_200,
            au_bytes: 900,
            keyframe: false,
            width: 1920,
            height: 1080,
        };
        let v = parse(&to_line(&FrameRecord::new(&stamps, Some(9_000), true)));
        assert_eq!(v["partial"], true);
        assert_eq!(v["keyframe"], false);
        assert_eq!(v["dropped"], false);
        assert_eq!(v["suppressed"], false);
    }

    #[test]
    fn a_frame_with_no_present_stamp_is_marked_dropped() {
        let stamps = FrameStamps {
            frame: 8,
            seq: None,
            recv_done_us: 1,
            decode_in_us: 2,
            decode_out_us: 3,
            au_bytes: 4,
            keyframe: false,
            width: 640,
            height: 360,
        };
        let v = parse(&to_line(&FrameRecord::new(&stamps, None, false)));
        assert_eq!(v["present_done_us"], Value::Null);
        assert_eq!(v["dropped"], true);
        assert_eq!(v["suppressed"], false);
        assert_eq!(
            v["partial"], false,
            "nothing converted a snapshot that was never shown"
        );
        assert_eq!(v["seq"], Value::Null, "a v1 stream has no seq to invent");
    }

    #[test]
    fn a_suppressed_frame_is_not_counted_as_dropped() {
        // The distinction the schema exists for: this access unit was decoded and then
        // deliberately withheld, so calling it dropped would inflate pipeline loss.
        let stamps = FrameStamps {
            frame: 9,
            seq: Some(31),
            recv_done_us: 11,
            decode_in_us: 22,
            decode_out_us: 33,
            au_bytes: 44,
            keyframe: false,
            width: 1280,
            height: 720,
        };
        let v = parse(&to_line(&FrameRecord::suppressed(&stamps)));
        assert_eq!(v["type"], "frame");
        assert_eq!(v["frame"], 9);
        assert_eq!(v["seq"], 31);
        assert_eq!(v["decode_in_us"], 22, "the decoder really did run");
        assert_eq!(v["decode_out_us"], 33);
        assert_eq!(v["present_done_us"], Value::Null);
        assert_eq!(v["suppressed"], true);
        assert_eq!(v["dropped"], false, "suppressed is not dropped");
        assert_eq!(v["partial"], false, "and it reached no canvas at all");
    }

    #[test]
    fn a_painted_and_presented_rect_update_carries_both_stages() {
        // Every field a different value, so a line built from the wrong stamp shows.
        let stamps = RectStamps {
            seq: 77,
            recv_done_us: 2_000,
            paint_done_us: 2_150,
            rect_count: 3,
            rect_bytes: 9_600,
        };
        let v = parse(&to_line(&RectRecord::painted(&stamps, Some(2_900), false)));
        assert_eq!(v["type"], "rects");
        assert_eq!(v["seq"], 77);
        assert_eq!(v["recv_done_us"], 2_000);
        assert_eq!(v["paint_done_us"], 2_150);
        assert_eq!(v["present_done_us"], 2_900);
        assert_eq!(v["rect_count"], 3);
        assert_eq!(v["rect_bytes"], 9_600);
        assert_eq!(v["skipped"], Value::Null);
        assert_eq!(v["dropped"], false);
        assert_eq!(
            v["partial"], false,
            "this one was carried by a full convert"
        );
    }

    #[test]
    fn a_rect_update_presented_by_the_damage_only_path_says_which_path_it_was() {
        // `partial` true against `dropped` false, and the reverse in the test below:
        // the pair pins the two flags to their own names.
        let stamps = RectStamps {
            seq: 79,
            recv_done_us: 4_000,
            paint_done_us: 4_100,
            rect_count: 2,
            rect_bytes: 1_280,
        };
        let v = parse(&to_line(&RectRecord::painted(&stamps, Some(4_400), true)));
        assert_eq!(v["partial"], true);
        assert_eq!(v["dropped"], false);
        assert_eq!(v["skipped"], Value::Null);
    }

    #[test]
    fn a_painted_rect_update_that_was_never_presented_is_dropped() {
        let stamps = RectStamps {
            seq: 78,
            recv_done_us: 3_000,
            paint_done_us: 3_050,
            rect_count: 1,
            rect_bytes: 64,
        };
        let v = parse(&to_line(&RectRecord::painted(&stamps, None, false)));
        assert_eq!(v["paint_done_us"], 3_050, "it was painted, just not shown");
        assert_eq!(v["present_done_us"], Value::Null);
        assert_eq!(v["dropped"], true);
        assert_eq!(v["partial"], false, "no convert ran for it");
    }

    #[test]
    fn a_skipped_rect_update_names_its_reason_and_is_not_a_drop() {
        let v = parse(&to_line(&RectRecord::skipped(
            5,
            700,
            2,
            512,
            "before_base",
        )));
        assert_eq!(v["type"], "rects");
        assert_eq!(v["seq"], 5);
        assert_eq!(v["recv_done_us"], 700);
        assert_eq!(v["rect_count"], 2);
        assert_eq!(v["rect_bytes"], 512, "the size is counted even unpainted");
        assert_eq!(v["skipped"], "before_base");
        assert_eq!(v["paint_done_us"], Value::Null);
        assert_eq!(v["present_done_us"], Value::Null);
        assert_eq!(
            v["dropped"], false,
            "nothing was painted, so nothing was lost after painting"
        );
        assert_eq!(v["partial"], false, "and no convert path claims it");
    }

    #[test]
    fn a_server_line_is_nested_byte_for_byte() {
        // Field order and spacing preserved: the wrapper concatenates, it does not
        // re-serialise. A round trip through serde_json would reorder nothing here but
        // would drop the spacing, which is how this test tells the two apart.
        let raw = br#"{"record":"frame", "frame":1,"au_bytes":900}"#;
        let wrapped = server_line(raw);
        assert_eq!(
            wrapped,
            r#"{"type":"server","line":{"record":"frame", "frame":1,"au_bytes":900}}"#
        );
        let v = parse(&wrapped);
        assert_eq!(v["type"], "server");
        assert_eq!(v["line"]["record"], "frame");
        assert_eq!(v["line"]["au_bytes"], 900);
    }

    #[test]
    fn a_server_line_that_is_not_json_is_escaped_rather_than_dropped() {
        let wrapped = server_line(b"input: SendInput injected 0 of 1 events");
        let v = parse(&wrapped);
        assert_eq!(v["type"], "server_raw");
        assert_eq!(v["text"], "input: SendInput injected 0 of 1 events");
    }

    #[test]
    fn a_server_line_that_is_not_utf8_still_produces_a_valid_line() {
        let v = parse(&server_line(&[0xFF, 0xFE, 0x00]));
        assert_eq!(v["type"], "server_raw");
        assert_eq!(v["bytes"], 3);
    }

    #[test]
    fn the_discarding_log_accepts_writes_without_a_file() {
        let log = StatsLog::discarding();
        log.write_line("anything");
        log.flush();
    }

    #[test]
    fn a_blocked_stats_writer_does_not_block_the_recording_thread() {
        let (log, started_rx, release, _) = blocked_log();
        let log = std::sync::Arc::new(log);
        let caller_log = log.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let caller = std::thread::spawn(move || {
            caller_log.write_line("one record");
            let _ = done_tx.send(());
        });

        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("the writer started");
        let timely = done_rx.recv_timeout(std::time::Duration::from_secs(1));
        release_writer(&release);
        caller.join().expect("recording caller");

        assert!(timely.is_ok(), "recording waited for the blocked writer");
    }

    #[test]
    fn queue_overload_is_bounded_and_recorded_after_the_writer_recovers() {
        let (log, started_rx, release, written) = blocked_log();

        log.write_line("lead");
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("the writer started");
        for _ in 0..WRITE_QUEUE_CAPACITY + 10 {
            log.write_line("queued");
        }
        assert_eq!(log.dropped.load(Ordering::Relaxed), 10);

        release_writer(&release);
        log.flush();

        let bytes = written.lock().expect("written bytes mutex");
        let text = std::str::from_utf8(&bytes).expect("writer received utf-8");
        assert!(
            text.contains(r#"{"type":"stats_drop","lines":10}"#),
            "overload must be visible in the stats stream"
        );
    }

    #[test]
    fn flush_is_bounded_when_the_writer_never_returns() {
        let (log, started_rx, release, _) = blocked_log();
        let log = std::sync::Arc::new(log);
        log.write_line("lead");
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("the writer started");
        let flush_log = log.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let caller = std::thread::spawn(move || {
            assert!(!flush_log.flush_for(std::time::Duration::from_millis(10)));
            let _ = done_tx.send(());
        });

        let timely = done_rx.recv_timeout(std::time::Duration::from_millis(50));
        release_writer(&release);
        caller.join().expect("flush caller");
        assert!(timely.is_ok(), "flush waited without a deadline");
    }

    #[test]
    fn drop_is_bounded_when_the_writer_never_returns() {
        let (log, started_rx, release, _) = blocked_log();
        log.write_line("lead");
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("the writer started");
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let caller = std::thread::spawn(move || {
            drop(log);
            let _ = done_tx.send(());
        });

        let timely = done_rx.recv_timeout(std::time::Duration::from_millis(50));
        release_writer(&release);
        caller.join().expect("drop caller");
        assert!(timely.is_ok(), "drop joined a blocked writer");
    }

    #[test]
    fn oversized_records_are_dropped_before_the_bounded_queue() {
        let (log, started_rx, release, written) = blocked_log();
        log.write_line(&"x".repeat(MAX_QUEUED_LINE_BYTES + 1));
        assert_eq!(log.dropped.load(Ordering::Relaxed), 1);

        log.write_line("lead");
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("the writer started");
        release_writer(&release);
        log.flush();

        let bytes = written.lock().expect("written bytes mutex");
        let output = String::from_utf8_lossy(&bytes);
        assert!(output.contains(r#"{"type":"stats_drop","lines":1}"#));
        assert!(!output.contains(&"x".repeat(MAX_QUEUED_LINE_BYTES + 1)));
    }

    #[test]
    fn oversized_server_stats_are_dropped_before_wrapping() {
        let (log, started_rx, release, written) = blocked_log();
        log.record_server_line(&vec![b'x'; MAX_SERVER_LINE_BYTES + 1]);
        assert_eq!(log.dropped.load(Ordering::Relaxed), 1);

        log.write_line("lead");
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("the writer started");
        release_writer(&release);
        assert!(log.flush());

        let bytes = written.lock().expect("written bytes mutex");
        let output = String::from_utf8_lossy(&bytes);
        assert!(output.contains(r#"{"type":"stats_drop","lines":1}"#));
    }

    #[test]
    fn a_line_write_failure_is_reported_by_final_flush() {
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = StatsLog::with_writer(Box::new(FailFirstWriter {
            fail: true,
            written: written.clone(),
        }))
        .expect("start stats writer");
        log.write_line("lost");
        log.write_line("kept");

        assert!(!log.flush(), "a failed artifact must invalidate the run");
        let bytes = written.lock().expect("written bytes mutex");
        let output = String::from_utf8_lossy(&bytes);
        assert!(output.contains(r#"{"type":"stats_drop","lines":1}"#));
        assert!(output.contains("kept"));
        assert!(!output.contains("lost"));
    }

    #[test]
    fn a_late_producer_cannot_cross_the_final_flush_marker() {
        let (log, started_rx, release, _) = blocked_log();
        let log = std::sync::Arc::new(log);
        log.write_line("lead");
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("the writer started");

        let flush_log = log.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let caller = std::thread::spawn(move || {
            let _ = done_tx.send(flush_log.flush());
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !log.closed.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(
            log.closed.load(Ordering::SeqCst),
            "flush did not close producers"
        );

        log.record_server_line(&vec![b'x'; MAX_SERVER_LINE_BYTES + 1]);
        assert_eq!(
            log.dropped.load(Ordering::Relaxed),
            0,
            "a producer after closure crossed the final marker"
        );
        release_writer(&release);
        assert!(done_rx.recv().expect("flush result"));
        caller.join().expect("flush caller");
    }
}
