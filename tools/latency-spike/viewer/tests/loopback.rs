//! The receive path against a real socket, with no server and no window.
//!
//! What this proves: a TCP stream carrying the server's own framing reaches the real
//! [`DecodeSink`] — the same decoder wiring the binary runs — and that a malformed
//! access unit is recorded and *survived* rather than ending the connection. The
//! server's first frames can legitimately race its parameter sets, so "the stream
//! self-heals" is a behaviour the viewer has to have, not an optimism.
//!
//! What it deliberately does not prove: that a real H.264 stream decodes. That needs
//! a real encoder, and it is the live run's job.

use std::io::Write;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::Value;
use spike_server::framing;
use spike_viewer::clock::Clock;
use spike_viewer::net::{self, PumpEnd};
use spike_viewer::sink::{DecodeSink, FrameSlot};
use spike_viewer::stats::StatsLog;

/// A stats path under the OS temp dir, removed by the test that made it.
struct TempStats(std::path::PathBuf);

impl TempStats {
    fn new(tag: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!("spike-viewer-{tag}-{}.jsonl", std::process::id()));
        Self(path)
    }

    fn path(&self) -> &str {
        self.0.to_str().expect("a utf-8 temp path")
    }

    fn lines(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.0)
            .expect("the stats file")
            .lines()
            .map(|l| serde_json::from_str(l).expect("every stats line is valid JSON"))
            .collect()
    }
}

impl Drop for TempStats {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Serve `wire` once on an ephemeral loopback port, then close.
fn serve_once(wire: Vec<u8>) -> std::net::SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        stream.write_all(&wire).expect("write");
        // Closing is the signal the pump stops on; without it the reader blocks.
        drop(stream);
    });
    addr
}

fn encoded(parts: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (t, p) in parts {
        framing::encode(*t, p, &mut out);
    }
    out
}

#[test]
fn the_receive_path_handles_a_stats_line_and_a_garbage_access_unit() {
    let stats_file = TempStats::new("loopback");
    let stats = Arc::new(StatsLog::create(Some(stats_file.path())).expect("create stats"));

    // A header-shaped server line, then two access units that are not H.264 at all,
    // then another server line. If a refused access unit killed the connection, the
    // trailing line would never arrive — which is what makes it the interesting one.
    let wire = encoded(&[
        (
            framing::MSG_STATS,
            br#"{"record":"header","schema":1,"width":1920}"#.to_vec(),
        ),
        (framing::MSG_VIDEO, vec![0xDE, 0xAD, 0xBE, 0xEF]),
        (
            framing::MSG_VIDEO,
            // Annex B shaped, IDR NAL type, still meaningless to a decoder. The
            // keyframe flag in the record must come out true, which distinguishes
            // "the AU was inspected" from "the field was defaulted".
            vec![0, 0, 0, 1, 0x65, 0x11, 0x22, 0x33],
        ),
        (
            framing::MSG_STATS,
            br#"{"record":"frame","frame":1}"#.to_vec(),
        ),
    ]);
    let addr = serve_once(wire);

    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = wakes.clone();
    let mut sink = DecodeSink::new(
        mdrdp::h264::hardware_decoder(),
        Arc::new(FrameSlot::new()),
        stats.clone(),
        Box::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }),
    );

    let mut stream = TcpStream::connect(addr).expect("connect");
    stream.set_nodelay(true).expect("nodelay");
    let end = net::pump(&mut stream, &Clock::new(), &mut sink);

    assert!(
        matches!(end, PumpEnd::Eof),
        "the pump must reach a clean EOF, not stop early: {end}"
    );
    assert_eq!(sink.frames(), 2, "both access units were dispatched");
    assert_eq!(
        wakes.load(Ordering::Relaxed),
        0,
        "no frame decoded, so the window is never woken"
    );

    stats.flush();
    let lines = stats_file.lines();
    assert_eq!(
        lines.len(),
        4,
        "two server lines and two decode errors: {lines:?}"
    );

    assert_eq!(lines[0]["type"], "server");
    assert_eq!(lines[0]["line"]["record"], "header");
    assert_eq!(lines[0]["line"]["width"], 1920);

    assert_eq!(lines[1]["type"], "decode_error");
    assert_eq!(lines[1]["au_bytes"], 4);
    assert_eq!(lines[1]["keyframe"], false);
    assert!(
        lines[1]["detail"].as_str().is_some_and(|d| !d.is_empty()),
        "the reason is recorded, not just the fact"
    );

    assert_eq!(lines[2]["type"], "decode_error");
    assert_eq!(lines[2]["au_bytes"], 8);
    assert_eq!(lines[2]["keyframe"], true, "an IDR NAL is a keyframe");

    assert_eq!(
        lines[3]["type"], "server",
        "the connection survived both refusals"
    );
    assert_eq!(lines[3]["line"]["record"], "frame");
}

#[test]
fn a_server_that_hangs_up_mid_message_ends_the_pump_without_a_panic() {
    // Half a message: the reassembler must wait for bytes that never come and then
    // report EOF. A viewer that panicked here would take a whole run with it.
    let mut wire = encoded(&[(framing::MSG_VIDEO, vec![1, 2, 3, 4, 5, 6, 7, 8])]);
    wire.truncate(7);
    let addr = serve_once(wire);

    let mut sink = DecodeSink::new(
        None,
        Arc::new(FrameSlot::new()),
        Arc::new(StatsLog::discarding()),
        Box::new(|| {}),
    );
    let mut stream = TcpStream::connect(addr).expect("connect");
    let end = net::pump(&mut stream, &Clock::new(), &mut sink);
    assert!(matches!(end, PumpEnd::Eof), "{end}");
    assert_eq!(
        sink.frames(),
        0,
        "an incomplete message is never dispatched"
    );
}
