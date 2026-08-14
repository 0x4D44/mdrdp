//! TPKT framing, read under a **total-elapsed deadline**.
//!
//! `TcpStream::set_read_timeout` bounds each individual syscall, not a whole message. A
//! peer that declares a large length and then trickles one byte at a time, just inside
//! that per-syscall timeout, keeps a naive `read_exact` blocked forever. This module
//! enforces a wall-clock deadline across the entire read, so a hostile or merely broken
//! peer cannot hold the probe open.

use std::io::{self, Read};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// An X.224 connection confirm carrying a negotiation response is 19 bytes. Anything far
/// larger is not this exchange — and a large declared length is precisely how a hostile
/// peer asks us to keep waiting.
pub const MAX_TPKT_LEN: usize = 512;

const TPKT_HEADER_LEN: usize = 4;

/// Longest a single blocking read may park before the deadline is re-checked. The
/// deadline governs; this only bounds the granularity of enforcement.
const MAX_CHUNK_WAIT: Duration = Duration::from_millis(250);

/// Read one complete TPKT-framed message, or fail before `deadline`.
pub fn read_tpkt(stream: &mut TcpStream, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut header = [0u8; TPKT_HEADER_LEN];
    read_exact_by(stream, &mut header, deadline)?;

    let declared = u16::from_be_bytes([header[2], header[3]]) as usize;
    if declared < TPKT_HEADER_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("TPKT declares {declared} bytes, shorter than its own header"),
        ));
    }
    if declared > MAX_TPKT_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("TPKT declares {declared} bytes, over the {MAX_TPKT_LEN}-byte limit"),
        ));
    }

    let mut buf = header.to_vec();
    buf.resize(declared, 0);
    read_exact_by(stream, &mut buf[TPKT_HEADER_LEN..], deadline)?;
    Ok(buf)
}

/// Fill `buf` completely, or return `TimedOut` once `deadline` passes.
fn read_exact_by(stream: &mut TcpStream, buf: &mut [u8], deadline: Instant) -> io::Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("read deadline exceeded with {}/{} bytes", filled, buf.len()),
            ));
        }

        // A zero timeout means "block forever" to the OS, so never let it reach zero.
        let wait = (deadline - now)
            .min(MAX_CHUNK_WAIT)
            .max(Duration::from_millis(1));
        stream.set_read_timeout(Some(wait))?;

        match stream.read(&mut buf[filled..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("peer closed after {}/{} bytes", filled, buf.len()),
                ));
            }
            Ok(n) => filled += n,
            // A per-chunk timeout is expected: loop, and let the deadline decide.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
