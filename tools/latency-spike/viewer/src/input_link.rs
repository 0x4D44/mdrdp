//! The keystroke channel, viewer → server.
//!
//! Records are [`rhydra::input_proto`] — the server's own encoder, so a field
//! order or endianness change cannot desynchronise the two halves.
//!
//! Writes happen **on the window thread, inside the key event**. That is the point of
//! the channel: a queue and a worker thread would add a scheduling hop to the very
//! interval being measured. Healthy writes remain on the direct path; a short deadline
//! bounds backpressure, and its first error closes the link so a partial frameless record
//! can never be followed by another record.

use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use rhydra::input_proto::{self, InputRecord, KeyKind};

/// Fault containment for the sole window thread. A healthy loopback write returns at
/// once; ten milliseconds is the maximum one broken input link may steal from painting.
const WRITE_TIMEOUT: Duration = Duration::from_millis(10);

pub struct InputLink {
    stream: TcpStream,
    healthy: AtomicBool,
    /// Starts at 1, so a `seq` of 0 in a stats file can only mean "never sent".
    next_seq: AtomicU32,
}

impl InputLink {
    pub fn connect(addr: SocketAddr) -> std::io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        Ok(Self {
            stream,
            healthy: AtomicBool::new(true),
            next_seq: AtomicU32::new(1),
        })
    }

    /// Send one key transition. Returns the sequence number it went out under.
    pub fn send(&self, kind: KeyKind, vk: u16) -> std::io::Result<u32> {
        if !self.healthy.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "input link already failed",
            ));
        }
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let bytes = input_proto::encode(InputRecord { kind, vk, seq });
        // `&TcpStream` implements `Write`, so no `&mut self`, application lock, or queue:
        // the complete eight-byte record goes straight to the socket.
        let result = match (&self.stream).write(&bytes) {
            Ok(written) if written == bytes.len() => return Ok(seq),
            Ok(written) => Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                format!("input record was only {written}/{} bytes", bytes.len()),
            )),
            Err(error) => Err(error),
        };
        // A timeout or short write may have put a partial record on the wire. Closing is
        // the only safe recovery for a frameless stream: disconnect releases held input.
        self.fail();
        result
    }

    pub fn shutdown(&self) {
        self.fail();
    }

    fn fail(&self) {
        self.healthy.store(false, Ordering::Release);
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Shutdown, TcpListener};
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, Instant};

    #[cfg(unix)]
    #[test]
    fn a_nonreading_peer_cannot_wedge_the_input_link() {
        use std::os::fd::AsRawFd as _;

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let link = Arc::new(InputLink::connect(listener.local_addr().unwrap()).unwrap());
        let (peer, _) = listener.accept().unwrap();

        // Reach backpressure after a few records rather than millions. The OS may clamp
        // this upward, but it remains small enough for the bounded test.
        let bytes: libc::c_int = 1024;
        let result = unsafe {
            libc::setsockopt(
                link.stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&bytes as *const libc::c_int).cast(),
                std::mem::size_of_val(&bytes) as libc::socklen_t,
            )
        };
        assert_eq!(result, 0, "shrink the probe socket's send buffer");

        let sender_link = Arc::clone(&link);
        let (done_tx, done_rx) = mpsc::channel();
        let sender = std::thread::spawn(move || {
            let first_started = Instant::now();
            let first_error =
                (0..1_000_000).find_map(|_| sender_link.send(KeyKind::Down, 0x58).err());
            let first_elapsed = first_started.elapsed();
            let later_started = Instant::now();
            let later_error = sender_link.send(KeyKind::Up, 0x58).err();
            done_tx
                .send((
                    first_error,
                    later_error,
                    later_started.elapsed(),
                    first_elapsed,
                ))
                .unwrap();
        });

        let timely = done_rx.recv_timeout(Duration::from_secs(1));
        if timely.is_err() {
            // Always release the deliberately wedged writer before asserting, so a red
            // test leaves no thread or socket behind.
            let _ = peer.shutdown(Shutdown::Both);
        }
        let completed_in_time = timely.is_ok();
        let report = timely
            .ok()
            .or_else(|| done_rx.recv_timeout(Duration::from_secs(1)).ok())
            .expect("sender must stop after peer shutdown");
        sender.join().unwrap();

        assert!(
            report.0.is_some(),
            "backpressure must become a terminal send error"
        );
        assert!(
            report.1.is_some(),
            "a failed link must reject later records instead of extending a partial record"
        );
        assert!(
            report.2 < Duration::from_millis(100),
            "later sends on a failed link must return immediately"
        );
        assert!(
            report.3 < Duration::from_millis(250),
            "socket backpressure exceeded the viewer's bounded failure budget"
        );
        assert!(
            completed_in_time,
            "the first backpressured send blocked the caller for over one second"
        );
    }
}
