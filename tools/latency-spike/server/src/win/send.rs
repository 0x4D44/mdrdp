//! Stage 4 — the video socket and the stats file, both owned by one thread.
//!
//! One writer, on purpose. The capture thread and the input thread both produce
//! stats lines, and the video socket has to interleave them with the access units;
//! funnelling everything through a single [`Outbound`] channel means neither a mutex
//! nor an interleaving question exists.
//!
//! Loopback only. The transport to the Mac is an SSH tunnel — binding anything wider
//! would put an unauthenticated screen feed on the LAN.

use super::{qpc, Result};
use crate::framing;
use crate::stats::{FrameRecord, QpcClock};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

/// How long a blocked socket write is allowed to stall the sender before the client
/// is treated as gone. Generous next to a frame interval, short next to a human.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// What the sender thread consumes.
pub enum Outbound {
    /// An encoded access unit plus the stats row it belongs to. `send_done_us` is
    /// filled in here, because only this thread knows when the write returned.
    Frame(Box<FrameRecord>, Vec<u8>),
    /// A pre-serialised JSONL line (the header, or an input event).
    Line(String),
}

pub struct Sender {
    listener: TcpListener,
    client: Option<TcpStream>,
    connected: Arc<AtomicBool>,
    stats: Option<BufWriter<File>>,
    /// Replayed to every client that connects, so an archived capture is readable
    /// without the operator having to fetch the server's own file.
    header_line: String,
    clock: QpcClock,
    scratch: Vec<u8>,
}

impl Sender {
    pub fn new(
        port: u16,
        out_path: Option<&str>,
        header_line: String,
        clock: QpcClock,
        connected: Arc<AtomicBool>,
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
        Ok(Self {
            listener,
            client: None,
            connected,
            stats,
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
                self.client = Some(stream);
                self.connected.store(true, Ordering::Release);
                // The header goes first so a viewer knows the QPC frequency before
                // it sees a single stamp.
                let header = self.header_line.clone();
                self.write_message(framing::MSG_STATS, header.as_bytes());
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => eprintln!("video: accept failed: {e}"),
        }
    }

    fn drop_client(&mut self, why: &str) {
        if self.client.take().is_some() {
            eprintln!("video: disconnected ({why})");
        }
        self.connected.store(false, Ordering::Release);
    }

    /// Frame one message and push it. A write failure ends the connection; the
    /// listener goes straight back to accepting.
    fn write_message(&mut self, msg_type: u8, payload: &[u8]) {
        let Some(client) = self.client.as_mut() else {
            return;
        };
        self.scratch.clear();
        framing::encode(msg_type, payload, &mut self.scratch);
        let outcome = client
            .write_all(&self.scratch)
            .and_then(|()| client.flush());
        if let Err(e) = outcome {
            self.drop_client(&e.to_string());
        }
    }

    fn write_stats(&mut self, line: &str) {
        if let Some(file) = self.stats.as_mut() {
            // Flushed per line: the operator kills this process with Ctrl-C, and a
            // buffered tail lost at that moment is a measurement lost.
            if let Err(e) = writeln!(file, "{line}").and_then(|()| file.flush()) {
                eprintln!("stats: write failed, dropping the file: {e}");
                self.stats = None;
            }
        }
        self.write_message(framing::MSG_STATS, line.as_bytes());
    }

    /// Consume the channel until the producers are gone.
    pub fn run(mut self, rx: Receiver<Outbound>) {
        loop {
            self.poll_accept();
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Outbound::Frame(mut record, au)) => {
                    self.write_message(framing::MSG_VIDEO, &au);
                    record.send_done_us = self.clock.micros(qpc::now());
                    let line = crate::stats::to_line(&*record);
                    self.write_stats(&line);
                }
                Ok(Outbound::Line(line)) => self.write_stats(&line),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        if let Some(file) = self.stats.as_mut() {
            let _ = file.flush();
        }
    }
}
