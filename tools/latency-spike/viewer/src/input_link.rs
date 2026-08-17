//! The keystroke channel, viewer → server.
//!
//! Records are [`spike_server::input_proto`] — the server's own encoder, so a field
//! order or endianness change cannot desynchronise the two halves.
//!
//! Writes happen **on the window thread, inside the key event**. That is the point of
//! the channel: a queue and a worker thread would add a scheduling hop to the very
//! interval being measured. Eight bytes to a loopback socket with `TCP_NODELAY` never
//! blocks in practice, and a write error ends the link rather than stalling the UI.

use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};

use spike_server::input_proto::{self, InputRecord, KeyKind};

pub struct InputLink {
    stream: TcpStream,
    /// Starts at 1, so a `seq` of 0 in a stats file can only mean "never sent".
    next_seq: AtomicU32,
}

impl InputLink {
    pub fn connect(addr: SocketAddr) -> std::io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        Ok(Self {
            stream,
            next_seq: AtomicU32::new(1),
        })
    }

    /// Send one key transition. Returns the sequence number it went out under.
    pub fn send(&self, kind: KeyKind, vk: u16) -> std::io::Result<u32> {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let bytes = input_proto::encode(InputRecord { kind, vk, seq });
        // `&TcpStream` implements `Write`, so no `&mut self` and no lock: the socket
        // itself serialises, and one 8-byte record is a single `write` syscall.
        (&self.stream).write_all(&bytes)?;
        Ok(seq)
    }

    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}
