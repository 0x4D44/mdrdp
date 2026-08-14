//! Regression tests for the TPKT read deadline.
//!
//! A Gauntlet critic demonstrated that `TcpStream::set_read_timeout` bounds each
//! individual syscall, not a whole message: a peer declaring a large length and then
//! trickling bytes just inside the per-syscall timeout kept the probe blocked
//! indefinitely, well past its stated 5-second bound. These tests drive real sockets
//! against real adversarial peers, because the defect lived in the socket path that the
//! in-memory parser tests could never reach.

use mdrdp::probe::wire::{self, MAX_TPKT_LEN};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

/// Start a server on localhost that runs `handler` for one connection.
/// Returns the port to connect to.
fn serve_once<F>(handler: F) -> u16
where
    F: FnOnce(TcpStream) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            handler(stream);
        }
    });
    port
}

fn connect(port: u16) -> TcpStream {
    // The listener thread may not have reached accept() yet; retry briefly.
    for _ in 0..50 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
            return s;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("could not connect to test server on port {port}");
}

fn tpkt_header(declared: u16) -> [u8; 4] {
    let len = declared.to_be_bytes();
    [0x03, 0x00, len[0], len[1]]
}

#[test]
fn trickling_peer_cannot_outlast_the_deadline() {
    // Declares a length within the size cap, then dribbles bytes forever. Only a
    // total-elapsed deadline stops this; a per-syscall timeout never fires.
    let port = serve_once(|mut stream| {
        let _ = stream.write_all(&tpkt_header(512));
        loop {
            if stream.write_all(&[0x00]).is_err() || stream.flush().is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(200));
        }
    });

    let mut client = connect(port);
    let started = Instant::now();
    let result = wire::read_tpkt(&mut client, started + Duration::from_millis(800));
    let elapsed = started.elapsed();

    assert!(
        result.is_err(),
        "a trickling peer must not be read to completion"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "read must abandon at the deadline, took {elapsed:?}"
    );
}

#[test]
fn silent_peer_cannot_outlast_the_deadline() {
    // Sends a header promising more, then says nothing at all.
    let port = serve_once(|mut stream| {
        let _ = stream.write_all(&tpkt_header(64));
        thread::sleep(Duration::from_secs(30));
    });

    let mut client = connect(port);
    let started = Instant::now();
    let result = wire::read_tpkt(&mut client, started + Duration::from_millis(500));

    assert!(result.is_err(), "a silent peer must time out");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "read must abandon at the deadline"
    );
}

#[test]
fn oversized_declared_length_is_rejected_without_waiting() {
    // The cap must reject before any waiting happens — otherwise a peer can make us
    // sit for the full deadline just by naming a big number.
    let port = serve_once(|mut stream| {
        let _ = stream.write_all(&tpkt_header(u16::MAX));
        thread::sleep(Duration::from_secs(30));
    });

    let mut client = connect(port);
    let started = Instant::now();
    let result = wire::read_tpkt(&mut client, started + Duration::from_secs(10));
    let elapsed = started.elapsed();

    assert!(result.is_err(), "{MAX_TPKT_LEN}-byte cap must reject 65535");
    assert!(
        elapsed < Duration::from_secs(1),
        "oversized length must be rejected immediately, took {elapsed:?}"
    );
}

#[test]
fn peer_closing_mid_message_is_an_error_not_a_hang() {
    let port = serve_once(|mut stream| {
        let _ = stream.write_all(&tpkt_header(32));
        let _ = stream.write_all(&[0xAA; 4]);
        // drop: connection closes with 24 bytes still promised
    });

    let mut client = connect(port);
    let started = Instant::now();
    let result = wire::read_tpkt(&mut client, started + Duration::from_secs(5));

    assert!(result.is_err(), "truncated message must be an error");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "EOF should be detected promptly, not waited out"
    );
}

#[test]
fn a_well_formed_message_still_reads_completely() {
    // The deadline must not break the normal path: this is the real 19-byte
    // connection confirm captured from `temper`.
    let confirm: [u8; 19] = [
        0x03, 0x00, 0x00, 0x13, 0x0e, 0xd0, 0x00, 0x00, 0x12, 0x34, 0x00, 0x02, 0x2f, 0x08, 0x00,
        0x08, 0x00, 0x00, 0x00,
    ];
    let port = serve_once(move |mut stream| {
        // Deliberately split across two writes: one read() will not return it all.
        let _ = stream.write_all(&confirm[..7]);
        thread::sleep(Duration::from_millis(50));
        let _ = stream.write_all(&confirm[7..]);
    });

    let mut client = connect(port);
    let got = wire::read_tpkt(&mut client, Instant::now() + Duration::from_secs(5))
        .expect("a well-formed message must read");
    assert_eq!(got, confirm);
}
