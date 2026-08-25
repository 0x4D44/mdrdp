//! Stage 4 — independent video and sparse-pixel writers; the video writer also
//! owns the stats file.
//!
//! One bulk writer, on purpose. The capture thread and the input thread both produce
//! stats lines, and the video socket has to interleave them with the access units;
//! funnelling everything through a single [`Outbound`] channel means neither a mutex
//! nor an interleaving question exists.
//!
//! Loopback only. The transport to the Mac is an SSH tunnel — binding anything wider
//! would put an unauthenticated screen feed on the LAN.

use super::{qpc, Result};
use crate::framing;
use crate::send_schedule::{self, AdmissionGate, BatchKind};
use crate::stats::{FrameRecord, QpcClock, RectRecord};
use crate::visual_flow::{Epoch, VisualFlow};
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long a blocked socket write is allowed to stall the sender before the client
/// is treated as gone. Generous next to a frame interval, short next to a human.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Further messages one wakeup pulls off the channel before writing the batch out.
/// The queue between capture and sender is two frames deep, so this is generous
/// next to what can actually be waiting; it bounds the reordering pass rather than
/// tuning throughput.
const DRAIN_BATCH: usize = 8;

/// What the sender thread consumes.
pub enum Outbound {
    /// One complete logical video update. Coverage and all selected tile AUs share
    /// one framed payload, so the socket cannot expose a partial 5K update.
    Video(Vec<FrameTile>, Vec<u8>, Option<AdmissionGate>),
    /// Bulk-lane baseline barrier paired atomically with a sparse move prelude.
    MoveFence(Vec<u8>, Option<AdmissionGate>),
    /// HEVC remains the explicit full-frame fallback and retains its existing
    /// per-tile envelope; it never claims the AVC regional-update contract.
    FrameSet(Vec<FrameTile>),
    /// A pre-serialised JSONL line (the header, or an input event).
    Line(String),
}

pub struct FrameTile {
    pub record: Box<FrameRecord>,
    pub tile_id: u8,
    pub seq: u64,
    pub au: Vec<u8>,
}

pub struct SparseOutbound {
    pub msg_type: u8,
    pub record: Option<Box<RectRecord>>,
    pub payload: Vec<u8>,
    pub gate: Option<AdmissionGate>,
}

/// Dedicated raw-final-pixel writer. It has its own socket, queue and thread, so
/// no video write can hold its bytes behind an access unit.
pub struct SparseSender {
    listener: TcpListener,
    client: Option<TcpStream>,
    connected: Arc<AtomicBool>,
    session_epoch: Arc<AtomicU64>,
    accepted_epoch: u64,
    stats_tx: SyncSender<Outbound>,
    clock: QpcClock,
    scratch: Vec<u8>,
}

impl SparseSender {
    pub fn new(
        listener: TcpListener,
        connected: Arc<AtomicBool>,
        session_epoch: Arc<AtomicU64>,
        stats_tx: SyncSender<Outbound>,
        clock: QpcClock,
    ) -> Result<Self> {
        listener.set_nonblocking(true)?;
        eprintln!("sparse: listening on {}", listener.local_addr()?);
        Ok(Self {
            listener,
            client: None,
            connected,
            session_epoch,
            accepted_epoch: 0,
            stats_tx,
            clock,
            scratch: Vec::new(),
        })
    }

    fn poll_accept(&mut self) {
        let current_epoch = self.session_epoch.load(Ordering::Acquire);
        if self.client.is_some() && self.accepted_epoch != current_epoch {
            self.client = None;
            self.connected.store(false, Ordering::Release);
        }
        if self.client.is_some() {
            return;
        }
        if current_epoch == 0 {
            return;
        }
        match self.listener.accept() {
            Ok((stream, peer)) => {
                if stream
                    .set_nodelay(true)
                    .and_then(|()| stream.set_write_timeout(Some(WRITE_TIMEOUT)))
                    .is_ok()
                {
                    eprintln!("sparse: connected {peer}");
                    self.client = Some(stream);
                    self.accepted_epoch = current_epoch;
                    self.connected.store(true, Ordering::Release);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => eprintln!("sparse: accept failed: {error}"),
        }
    }

    fn write(&mut self, update: SparseOutbound) {
        if update.gate.as_ref().is_some_and(|gate| !gate.admitted()) {
            return;
        }
        let Some(client) = self.client.as_mut() else {
            self.connected.store(false, Ordering::Release);
            return;
        };
        self.scratch.clear();
        framing::encode(update.msg_type, &update.payload, &mut self.scratch);
        if let Err(error) = client
            .write_all(&self.scratch)
            .and_then(|()| client.flush())
        {
            eprintln!("sparse: disconnected ({error})");
            self.client = None;
            self.connected.store(false, Ordering::Release);
            return;
        }
        if let Some(mut record) = update.record {
            record.send_done_us = self.clock.micros(qpc::now());
            let _ = self
                .stats_tx
                .try_send(Outbound::Line(crate::stats::to_line(&*record)));
        }
    }

    pub fn run(mut self, rx: Receiver<SparseOutbound>) {
        loop {
            self.poll_accept();
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(update) => self.write(update),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        self.connected.store(false, Ordering::Release);
    }
}

pub struct Sender {
    listener: TcpListener,
    client: Option<TcpStream>,
    connected: Arc<AtomicBool>,
    sparse_connected: Arc<AtomicBool>,
    session_epoch: Arc<AtomicU64>,
    accepted_epoch: Epoch,
    visual_flow: Arc<VisualFlow>,
    feedback_tx: mpsc::Sender<FeedbackEnd>,
    feedback_rx: mpsc::Receiver<FeedbackEnd>,
    feedback_join: Option<JoinHandle<()>>,
    viewer_socket: Arc<ViewerSocket>,
    cursor_hidden: Arc<AtomicBool>,
    sent_cursor_hidden: Option<bool>,
    stats: Option<BufWriter<File>>,
    stats_dirty: bool,
    last_stats_flush: Instant,
    /// Replayed to every client that connects, so an archived capture is readable
    /// without the operator having to fetch the server's own file.
    header_line: String,
    clock: QpcClock,
    scratch: Vec<u8>,
}

struct FeedbackEnd {
    epoch: Epoch,
    why: String,
}

#[derive(Default)]
pub struct ViewerSocket {
    current: Mutex<Option<(Epoch, TcpStream)>>,
}

impl ViewerSocket {
    fn install(&self, epoch: Epoch, stream: TcpStream) {
        *self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((epoch, stream));
    }

    fn clear(&self, epoch: Epoch) {
        let mut current = self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.as_ref().is_some_and(|(owned, _)| *owned == epoch) {
            *current = None;
        }
    }

    pub fn shutdown(&self, epoch: Epoch) {
        let current = self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((owned, stream)) = current.as_ref() {
            if *owned == epoch {
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
    }
}

impl Sender {
    pub fn new(
        port: u16,
        out_path: Option<&str>,
        header_line: String,
        clock: QpcClock,
        connected: Arc<AtomicBool>,
        sparse_connected: Arc<AtomicBool>,
        session_epoch: Arc<AtomicU64>,
        cursor_hidden: Arc<AtomicBool>,
        visual_flow: Arc<VisualFlow>,
        viewer_socket: Arc<ViewerSocket>,
    ) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
        listener.set_nonblocking(true)?;
        let mut stats = match out_path {
            Some(path) => Some(BufWriter::new(File::create(path)?)),
            None => None,
        };
        if let Some(file) = stats.as_mut() {
            writeln!(file, "{header_line}")?;
            file.flush()?;
        }
        eprintln!("video: listening on 127.0.0.1:{port}");
        let (feedback_tx, feedback_rx) = mpsc::channel();
        Ok(Self {
            listener,
            client: None,
            connected,
            sparse_connected,
            session_epoch,
            accepted_epoch: 0,
            visual_flow,
            feedback_tx,
            feedback_rx,
            feedback_join: None,
            viewer_socket,
            cursor_hidden,
            sent_cursor_hidden: None,
            stats,
            stats_dirty: false,
            last_stats_flush: Instant::now(),
            header_line,
            clock,
            scratch: Vec::new(),
        })
    }

    /// Take a waiting connection, if there is one and we are free.
    fn poll_accept(&mut self) {
        if self.client.is_some() {
            return;
        }
        match self.listener.accept() {
            Ok((stream, peer)) => {
                if let Err(e) = stream
                    .set_nodelay(true)
                    .and_then(|()| stream.set_write_timeout(Some(WRITE_TIMEOUT)))
                {
                    eprintln!("video: rejecting {peer}: {e}");
                    return;
                }
                eprintln!("video: connected {peer}");
                let epoch = self.visual_flow.begin_epoch();
                let feedback_stream = match stream.try_clone() {
                    Ok(stream) => stream,
                    Err(error) => {
                        let _ = stream.shutdown(Shutdown::Both);
                        let _ = self.visual_flow.disconnect(epoch);
                        eprintln!("video: rejecting {peer}: feedback clone failed: {error}");
                        return;
                    }
                };
                let feedback_flow = Arc::clone(&self.visual_flow);
                let feedback_tx = self.feedback_tx.clone();
                let feedback_join = match std::thread::Builder::new()
                    .name("spike-feedback".into())
                    .spawn(move || {
                        feedback_loop(feedback_stream, epoch, feedback_flow, feedback_tx)
                    }) {
                    Ok(join) => join,
                    Err(error) => {
                        let _ = stream.shutdown(Shutdown::Both);
                        let _ = self.visual_flow.disconnect(epoch);
                        eprintln!("video: rejecting {peer}: feedback thread failed: {error}");
                        return;
                    }
                };
                self.client = Some(stream);
                self.accepted_epoch = epoch;
                self.feedback_join = Some(feedback_join);
                let cancel_stream = match self
                    .client
                    .as_ref()
                    .expect("accepted viewer socket")
                    .try_clone()
                {
                    Ok(stream) => stream,
                    Err(error) => {
                        self.drop_client(&format!("cancellation clone failed: {error}"));
                        return;
                    }
                };
                self.viewer_socket.install(epoch, cancel_stream);
                self.session_epoch.store(epoch, Ordering::Release);
                self.sparse_connected.store(false, Ordering::Release);
                self.connected.store(true, Ordering::Release);
                // The header goes first so a viewer knows the QPC frequency before
                // it sees a single stamp.
                let header = self.header_line.clone();
                self.write_message(framing::MSG_STATS, header.as_bytes());
                self.write_cursor_if_changed();
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => eprintln!("video: accept failed: {e}"),
        }
    }

    fn drop_client(&mut self, why: &str) {
        if let Some(client) = self.client.take() {
            let _ = client.shutdown(Shutdown::Both);
            eprintln!("video: disconnected ({why})");
        }
        let _ = self.visual_flow.disconnect(self.accepted_epoch);
        self.viewer_socket.clear(self.accepted_epoch);
        if let Some(join) = self.feedback_join.take() {
            let _ = join.join();
        }
        self.connected.store(false, Ordering::Release);
        self.sparse_connected.store(false, Ordering::Release);
        self.accepted_epoch = 0;
        self.sent_cursor_hidden = None;
    }

    fn poll_feedback(&mut self) {
        while let Ok(end) = self.feedback_rx.try_recv() {
            if self.client.is_some() && end.epoch == self.accepted_epoch {
                self.drop_client(&end.why);
                return;
            }
        }
        if self.client.is_some() && self.visual_flow.current_epoch() != self.accepted_epoch {
            self.drop_client("viewer flow cancelled");
        }
    }

    /// Frame one message and push it. A write failure ends the connection; the
    /// listener goes straight back to accepting.
    fn write_message(&mut self, msg_type: u8, payload: &[u8]) -> bool {
        let Some(client) = self.client.as_mut() else {
            return false;
        };
        self.scratch.clear();
        framing::encode(msg_type, payload, &mut self.scratch);
        let outcome = client
            .write_all(&self.scratch)
            .and_then(|()| client.flush());
        match outcome {
            Ok(()) => true,
            Err(error) => {
                self.drop_client(&error.to_string());
                false
            }
        }
    }

    fn write_cursor_if_changed(&mut self) {
        if self.client.is_none() {
            return;
        }
        let hidden = self.cursor_hidden.load(Ordering::Acquire);
        if self.sent_cursor_hidden == Some(hidden) {
            return;
        }
        self.write_message(framing::MSG_CURSOR, &framing::encode_cursor(hidden));
        if self.client.is_some() {
            self.sent_cursor_hidden = Some(hidden);
        }
    }

    fn write_video(&mut self, tile_id: u8, seq: u64, au: &[u8]) -> bool {
        let payload = framing::encode_tile_au(tile_id, seq, au);
        self.write_message(framing::MSG_VIDEO_TILE, &payload)
    }

    fn write_stats(&mut self, line: &str) {
        if let Some(file) = self.stats.as_mut() {
            if let Err(e) = writeln!(file, "{line}") {
                eprintln!("stats: write failed, dropping the file: {e}");
                self.stats = None;
            } else {
                self.stats_dirty = true;
            }
        }
        self.write_message(framing::MSG_STATS, line.as_bytes());
    }

    fn flush_stats(&mut self) {
        let outcome = self.stats.as_mut().map(|file| file.flush());
        if let Some(Err(e)) = outcome {
            eprintln!("stats: flush failed, dropping the file: {e}");
            self.stats = None;
        }
        self.stats_dirty = false;
        self.last_stats_flush = Instant::now();
    }

    fn flush_stats_if_due(&mut self, now: Instant) {
        if send_schedule::flush_due(
            self.stats_dirty,
            now.saturating_duration_since(self.last_stats_flush),
        ) {
            self.flush_stats();
        }
    }

    /// Write one payload and append its rows for the batch's telemetry pass.
    fn handle(&mut self, msg: Outbound, stats_lines: &mut Vec<String>) {
        match msg {
            Outbound::Video(tiles, payload, gate) => {
                if gate.as_ref().is_some_and(|gate| !gate.admitted()) {
                    return;
                }
                let delivered = self.write_message(framing::MSG_VIDEO_UPDATE, &payload);
                for mut tile in tiles {
                    send_schedule::emit_if_delivered(
                        delivered,
                        &mut tile.record,
                        |record| record.send_done_us = self.clock.micros(qpc::now()),
                        |record| stats_lines.push(crate::stats::to_line(&*record)),
                    );
                }
            }
            Outbound::MoveFence(payload, gate) => {
                if gate.as_ref().is_some_and(|gate| !gate.admitted()) {
                    return;
                }
                self.write_message(framing::MSG_MOVE_FENCE, &payload);
            }
            Outbound::FrameSet(tiles) => {
                for mut tile in tiles {
                    let delivered = self.write_video(tile.tile_id, tile.seq, &tile.au);
                    send_schedule::emit_if_delivered(
                        delivered,
                        &mut tile.record,
                        |record| record.send_done_us = self.clock.micros(qpc::now()),
                        |record| stats_lines.push(crate::stats::to_line(&*record)),
                    );
                }
            }
            Outbound::Line(line) => stats_lines.push(line),
        }
    }

    /// Consume the channel until the producers are gone.
    ///
    /// Each wakeup drains a small bounded batch. Pixel-bearing messages retain
    /// arrival order; only stats lines move behind them. Cross-update reordering is
    /// unsafe until the dedicated sparse channel carries per-block precedence.
    pub fn run(mut self, rx: Receiver<Outbound>) {
        let mut batch: Vec<Outbound> = Vec::with_capacity(DRAIN_BATCH + 1);
        let mut stats_lines: Vec<String> = Vec::with_capacity(DRAIN_BATCH + 1);
        loop {
            self.poll_feedback();
            self.poll_accept();
            self.write_cursor_if_changed();
            self.flush_stats_if_due(Instant::now());
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(msg) => batch.push(msg),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            }
            for _ in 0..DRAIN_BATCH {
                match rx.try_recv() {
                    Ok(msg) => batch.push(msg),
                    // Empty or disconnected: either way there is nothing more to
                    // add now. A disconnect is noticed by the blocking recv above
                    // on the next lap, after this batch has been written out.
                    Err(_) => break,
                }
            }
            // Timestamp and frame_seq carry causality. JSONL/socket position does not,
            // so stats may follow payloads that arrived later in this bounded batch
            // rather than delaying those payloads.
            send_schedule::payload_first(&mut batch, |msg| match msg {
                Outbound::Video(..) | Outbound::MoveFence(..) | Outbound::FrameSet(..) => {
                    BatchKind::Payload
                }
                Outbound::Line(..) => BatchKind::Line,
            });
            for msg in batch.drain(..) {
                self.handle(msg, &mut stats_lines);
            }
            for line in stats_lines.drain(..) {
                self.write_stats(&line);
            }
            self.flush_stats_if_due(Instant::now());
        }
        self.flush_stats();
        self.drop_client("sender stopped");
    }
}

fn feedback_loop(
    mut stream: TcpStream,
    epoch: Epoch,
    visual_flow: Arc<VisualFlow>,
    end_tx: mpsc::Sender<FeedbackEnd>,
) {
    let mut reassembler = framing::Reassembler::new(framing::FRAME_ACK_BYTES);
    let mut bytes = [0u8; 64];
    let why = 'reading: loop {
        match stream.read(&mut bytes) {
            Ok(0) => break 'reading "viewer closed".to_owned(),
            Ok(count) => {
                reassembler.push(&bytes[..count]);
                loop {
                    match reassembler.next_message() {
                        Ok(Some(message)) if message.msg_type == framing::MSG_FRAME_ACK => {
                            let frame_seq = match framing::decode_frame_ack(&message.payload) {
                                Ok(frame_seq) => frame_seq,
                                Err(error) => break 'reading format!("invalid frame ACK: {error}"),
                            };
                            if let Err(error) = visual_flow.ack(epoch, frame_seq) {
                                break 'reading error.to_string();
                            }
                        }
                        Ok(Some(message)) => {
                            break 'reading format!(
                                "unexpected viewer message type {} on video feedback",
                                message.msg_type
                            );
                        }
                        Ok(None) => break,
                        Err(error) => break 'reading error.to_string(),
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => break 'reading error.to_string(),
        }
    };
    let _ = visual_flow.disconnect(epoch);
    let _ = end_tx.send(FeedbackEnd { epoch, why });
}
