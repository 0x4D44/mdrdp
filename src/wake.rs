//! Wake-able readiness wait for the session pump.
//!
//! The pump owns one blocking TLS stream, so its reads and writes cannot be split
//! across threads: rustls needs `&mut` to the same connection state for both
//! directions. The loop therefore used to sleep inside `recv` in 5 ms slices and
//! discover queued keyboard input only when a slice expired — a 0–5 ms tax on every
//! keystroke, and ~200 wakeups a second holding an idle link (measured: 405 ms of
//! CPU in `recvfrom` over a 75 s session). Instead, the pump sleeps in
//! `poll(2)`/`WSAPoll` on two sockets at once: the RDP TCP socket and a loopback
//! UDP "doorbell" every input sender rings. Either becoming readable wakes the pump
//! immediately, so input reaches the wire in microseconds and an idle pump wakes a
//! handful of times a second instead of two hundred.
//!
//! A connected UDP pair is the wake primitive because it is the one self-wake
//! channel std can build portably that both poll implementations accept — a pipe is
//! not `WSAPoll`-able on Windows. A connected UDP socket discards datagrams from
//! any other source address, so a stray local sender cannot ring the doorbell; and
//! a spurious ring costs one empty drain, never correctness.

use std::io;
use std::net::{Ipv4Addr, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::mpsc::{SendError, Sender};
use std::time::Duration;

/// The ringing end. Cheap to clone; every clone rings the same receiver.
#[derive(Clone)]
pub struct Doorbell(Arc<UdpSocket>);

impl Doorbell {
    /// Wake the pump. Never blocks: the socket is non-blocking, and a full socket
    /// buffer means wakes are already pending, so dropping this one loses nothing.
    pub fn ring(&self) {
        let _ = self.0.send(&[1]);
    }
}

/// The pump's end: drained once per loop pass, waited on in [`wait_readable`].
pub struct DoorbellReceiver(UdpSocket);

impl DoorbellReceiver {
    /// Swallow every pending ring. Non-blocking.
    pub fn drain(&self) {
        let mut byte = [0u8; 8];
        while self.0.recv(&mut byte).is_ok() {}
    }
}

/// A connected loopback pair: ring the first, wake whoever polls the second.
pub fn doorbell() -> io::Result<(Doorbell, DoorbellReceiver)> {
    let ringer = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    let listener = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    ringer.connect(listener.local_addr()?)?;
    listener.connect(ringer.local_addr()?)?;
    ringer.set_nonblocking(true)?;
    listener.set_nonblocking(true)?;
    Ok((Doorbell(Arc::new(ringer)), DoorbellReceiver(listener)))
}

/// An mpsc sender that rings the pump's doorbell after every send, so the event is
/// acted on immediately instead of on the next read timeout.
pub struct WakingSender<T> {
    tx: Sender<T>,
    bell: Option<Doorbell>,
}

impl<T> Clone for WakingSender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            bell: self.bell.clone(),
        }
    }
}

impl<T> WakingSender<T> {
    pub fn new(tx: Sender<T>, bell: Doorbell) -> Self {
        Self {
            tx,
            bell: Some(bell),
        }
    }

    /// A sender with no doorbell, for tests that talk straight to a receiver with
    /// no pump asleep behind it.
    pub fn silent(tx: Sender<T>) -> Self {
        Self { tx, bell: None }
    }

    /// Send, then ring. The mpsc send completes before the ring goes out, so a pump
    /// woken by the ring always finds the event already in the channel.
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        self.tx.send(value)?;
        self.wake();
        Ok(())
    }

    /// Ring without putting a reliable event in this sender's FIFO. Native
    /// window motion uses this after replacing its separate latest-value slot.
    pub fn wake(&self) {
        if let Some(bell) = &self.bell {
            bell.ring();
        }
    }
}

/// What [`wait_readable`] found. Both `false` means the timeout elapsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ready {
    /// The RDP socket has bytes (or an error/hangup a read will surface).
    pub socket: bool,
    /// The doorbell was rung: queued input or a stop request wants the loop to run.
    pub bell: bool,
}

/// Sleep until the RDP socket or the doorbell is readable, or `timeout` elapses.
///
/// The socket may be (and is) a blocking socket: readiness polling does not require
/// non-blocking mode, it only reports that a read would succeed. Error and hangup
/// conditions on the RDP socket report as `socket: true` so the pump's own read
/// path surfaces them; the doorbell's errors are ignored because its only failure
/// mode is a missed wake, which the timeout bounds.
pub fn wait_readable(
    socket: &TcpStream,
    bell: &DoorbellReceiver,
    timeout: Duration,
) -> io::Result<Ready> {
    imp::wait_readable(socket, &bell.0, timeout)
}

/// Sleep until the TCP socket can accept another write, or `timeout` elapses.
///
/// Error and hangup events count as ready so the following write surfaces the
/// transport failure. `false` means only that the timeout elapsed.
pub fn wait_writable(socket: &TcpStream, timeout: Duration) -> io::Result<bool> {
    imp::wait_writable(socket, timeout)
}

/// Read and write readiness observed while an outbound TLS record is blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WriteReadiness {
    pub readable: bool,
    pub writable: bool,
}

/// Sleep until the TCP socket can make progress in either direction.
///
/// rustls drains pending TLS output before it reads. Waiting only for writability can
/// therefore deadlock against a peer whose own send path must drain before it reads our
/// output. Reporting inbound readiness lets the caller buffer authenticated TLS plaintext
/// without dispatching or reordering its RDP payload.
pub fn wait_write_progress(socket: &TcpStream, timeout: Duration) -> io::Result<WriteReadiness> {
    imp::wait_write_progress(socket, timeout)
}

#[cfg(unix)]
mod imp {
    use super::Ready;
    use std::io;
    use std::net::{TcpStream, UdpSocket};
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    fn poll_timeout(remaining: Duration) -> i32 {
        i32::try_from(remaining.as_millis().max(1)).unwrap_or(i32::MAX)
    }

    pub(super) fn wait_readable(
        socket: &TcpStream,
        bell: &UdpSocket,
        timeout: Duration,
    ) -> io::Result<Ready> {
        // POLLIN is the only requested event; POLLERR/POLLHUP/POLLNVAL arrive
        // unrequested and count as readable so the reader sees the failure.
        let mut fds = [
            libc::pollfd {
                fd: socket.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: bell.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        loop {
            let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    // Restarting the full timeout after EINTR is fine: signals are
                    // rare and the timeout is a cadence, not a deadline.
                    continue;
                }
                return Err(e);
            }
            return Ok(Ready {
                socket: fds[0].revents != 0,
                bell: fds[1].revents != 0,
            });
        }
    }

    pub(super) fn wait_writable(socket: &TcpStream, timeout: Duration) -> io::Result<bool> {
        let deadline = Instant::now() + timeout;
        let mut fd = libc::pollfd {
            fd: socket.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            let n = unsafe { libc::poll(&mut fd, 1, poll_timeout(remaining)) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            return Ok(n > 0);
        }
    }

    pub(super) fn wait_write_progress(
        socket: &TcpStream,
        timeout: Duration,
    ) -> io::Result<super::WriteReadiness> {
        let deadline = Instant::now() + timeout;
        let mut fd = libc::pollfd {
            fd: socket.as_raw_fd(),
            events: libc::POLLIN | libc::POLLOUT,
            revents: 0,
        };
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(super::WriteReadiness::default());
            }
            let n = unsafe { libc::poll(&mut fd, 1, poll_timeout(remaining)) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if n == 0 {
                return Ok(super::WriteReadiness::default());
            }
            let readable = fd.revents & libc::POLLIN != 0;
            let writable = fd.revents & libc::POLLOUT != 0;
            let terminal = fd.revents != 0 && !readable && !writable;
            return Ok(super::WriteReadiness {
                readable: readable || terminal,
                writable: writable || terminal,
            });
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::Ready;
    use std::io;
    use std::net::{TcpStream, UdpSocket};
    use std::os::windows::io::AsRawSocket;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Networking::WinSock::{
        POLLRDNORM, POLLWRNORM, SOCKET_ERROR, WSAPOLLFD, WSAPoll,
    };

    fn poll_timeout(remaining: Duration) -> i32 {
        i32::try_from(remaining.as_millis().max(1)).unwrap_or(i32::MAX)
    }

    pub(super) fn wait_readable(
        socket: &TcpStream,
        bell: &UdpSocket,
        timeout: Duration,
    ) -> io::Result<Ready> {
        let mut fds = [
            WSAPOLLFD {
                fd: socket.as_raw_socket() as usize,
                events: POLLRDNORM,
                revents: 0,
            },
            WSAPOLLFD {
                fd: bell.as_raw_socket() as usize,
                events: POLLRDNORM,
                revents: 0,
            },
        ];
        let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        let n = unsafe { WSAPoll(fds.as_mut_ptr(), fds.len() as u32, millis) };
        if n == SOCKET_ERROR {
            return Err(io::Error::last_os_error());
        }
        Ok(Ready {
            socket: fds[0].revents != 0,
            bell: fds[1].revents != 0,
        })
    }

    pub(super) fn wait_writable(socket: &TcpStream, timeout: Duration) -> io::Result<bool> {
        let deadline = Instant::now() + timeout;
        let mut fd = WSAPOLLFD {
            fd: socket.as_raw_socket() as usize,
            events: POLLWRNORM,
            revents: 0,
        };
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            let n = unsafe { WSAPoll(&mut fd, 1, poll_timeout(remaining)) };
            if n == SOCKET_ERROR {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            return Ok(n > 0);
        }
    }

    pub(super) fn wait_write_progress(
        socket: &TcpStream,
        timeout: Duration,
    ) -> io::Result<super::WriteReadiness> {
        let deadline = Instant::now() + timeout;
        let mut fd = WSAPOLLFD {
            fd: socket.as_raw_socket() as usize,
            events: POLLRDNORM | POLLWRNORM,
            revents: 0,
        };
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(super::WriteReadiness::default());
            }
            let n = unsafe { WSAPoll(&mut fd, 1, poll_timeout(remaining)) };
            if n == SOCKET_ERROR {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if n == 0 {
                return Ok(super::WriteReadiness::default());
            }
            let readable = fd.revents & POLLRDNORM != 0;
            let writable = fd.revents & POLLWRNORM != 0;
            let terminal = fd.revents != 0 && !readable && !writable;
            return Ok(super::WriteReadiness {
                readable: readable || terminal,
                writable: writable || terminal,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Instant;

    /// A connected TCP pair on loopback, for readiness tests.
    fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    #[test]
    fn a_ring_wakes_the_wait_without_touching_the_socket() {
        let (bell, rx) = doorbell().unwrap();
        let (socket, _peer) = tcp_pair();
        bell.ring();
        let ready = wait_readable(&socket, &rx, Duration::from_secs(5)).unwrap();
        assert!(ready.bell);
        assert!(!ready.socket);
    }

    #[test]
    fn socket_data_wakes_the_wait() {
        let (_bell, rx) = doorbell().unwrap();
        let (socket, peer) = tcp_pair();
        use std::io::Write as _;
        (&peer).write_all(&[42]).unwrap();
        let ready = wait_readable(&socket, &rx, Duration::from_secs(5)).unwrap();
        assert!(ready.socket);
        assert!(!ready.bell);
    }

    #[test]
    fn connected_socket_reports_writable_without_polling_on_a_timer() {
        let (socket, _peer) = tcp_pair();
        assert!(wait_writable(&socket, Duration::from_secs(5)).unwrap());
    }

    #[test]
    fn a_quiet_wait_times_out_rather_than_hanging() {
        let (_bell, rx) = doorbell().unwrap();
        let (socket, _peer) = tcp_pair();
        let start = Instant::now();
        let ready = wait_readable(&socket, &rx, Duration::from_millis(50)).unwrap();
        assert_eq!(ready, Ready::default());
        // Generous upper bound: the point is it returned, not that it was precise.
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn drain_swallows_every_pending_ring() {
        let (bell, rx) = doorbell().unwrap();
        let (socket, _peer) = tcp_pair();
        for _ in 0..10 {
            bell.ring();
        }
        // Loopback UDP delivery can lag the send (observed on macOS), so a single
        // drain may miss late datagrams — drain until the poll reports quiet. If
        // drain made no progress this never converges and the test fails.
        for _ in 0..50 {
            rx.drain();
            let ready = wait_readable(&socket, &rx, Duration::from_millis(10)).unwrap();
            if !ready.bell {
                return;
            }
        }
        panic!("doorbell still readable after repeated drains");
    }

    #[test]
    fn a_waking_sender_delivers_and_rings() {
        let (bell, rx) = doorbell().unwrap();
        let (socket, _peer) = tcp_pair();
        let (tx, events) = mpsc::channel();
        let sender = WakingSender::new(tx, bell);
        sender.send(7u32).unwrap();
        assert_eq!(events.try_recv().unwrap(), 7);
        let ready = wait_readable(&socket, &rx, Duration::from_secs(5)).unwrap();
        assert!(ready.bell);
    }

    #[test]
    fn a_silent_sender_still_delivers() {
        let (tx, events) = mpsc::channel();
        let sender = WakingSender::silent(tx);
        sender.send(7u32).unwrap();
        assert_eq!(events.try_recv().unwrap(), 7);
    }
}
