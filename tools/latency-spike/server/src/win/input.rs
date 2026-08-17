//! The keystroke channel — stage 5, and the other half of the round trip.
//!
//! Accepts on `127.0.0.1:<input-port>`, reads fixed 8-byte records (see
//! [`crate::input_proto`]), stamps QPC on arrival, injects with `SendInput`, stamps
//! again, and emits one `input` stats line per keystroke.
//!
//! `SendInput` posts to the input queue of the **session the process runs in**. The
//! server therefore has to run in the interactive console session; from a service or
//! a session-0 context the calls succeed and nothing happens. The README says so, and
//! a zero return from `SendInput` is reported rather than ignored so the failure is
//! visible in the log instead of showing up as "the keystrokes do nothing".

use super::{qpc, Result};
use crate::input_proto::{self, KeyKind, RECORD_LEN};
use crate::stats::{self, InputEventRecord, QpcClock};
use std::io::Read;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::mpsc::SyncSender;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VIRTUAL_KEY,
};

/// Inject one key transition. Returns the QPC stamp taken immediately after the
/// call, and an error if the injection was rejected.
fn inject(vk: u16, kind: KeyKind) -> Result<i64> {
    let flags = match kind {
        KeyKind::Down => KEYBD_EVENT_FLAGS(0),
        KeyKind::Up => KEYEVENTF_KEYUP,
    };
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: one fully initialised INPUT, and the size argument is the size of the
    // very type being passed — the classic SendInput failure is a mismatched cbSize.
    let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
    let stamp = qpc::now();
    if sent != 1 {
        return Err(format!(
            "SendInput injected {sent} of 1 events for vk {vk:#04x}; \
             the process is probably not in the interactive session"
        )
        .into());
    }
    Ok(stamp)
}

fn serve_one(
    mut stream: TcpStream,
    clock: QpcClock,
    lines: &SyncSender<String>,
) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    let mut record = [0u8; RECORD_LEN];
    loop {
        // A short read is not a partial record to be retried — `read_exact` either
        // fills the buffer or the peer closed mid-record, and a stream that is out of
        // phase can never be resynchronised in a fixed-width protocol.
        match stream.read_exact(&mut record) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        }
        let recv_qpc = qpc::now();
        let parsed = match input_proto::decode(&record) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("input: closing connection: {e}");
                return Ok(());
            }
        };
        let injected_qpc = match inject(parsed.vk, parsed.kind) {
            Ok(stamp) => stamp,
            Err(e) => {
                eprintln!("input: {e}");
                qpc::now()
            }
        };
        let line = stats::to_line(&InputEventRecord::new(
            parsed.seq,
            parsed.vk,
            parsed.kind.as_str(),
            clock.micros(recv_qpc),
            clock.micros(injected_qpc),
        ));
        // A full queue drops the *stats line*, never the injection — the injection
        // already happened above, so latency is untouched either way.
        let _ = lines.try_send(line);
    }
}

/// Run the input listener until the process exits. Intended for its own thread.
///
/// Binds loopback only. That is a hard requirement, not a default: the transport to
/// the Mac is an SSH tunnel, and a keystroke injector reachable from the LAN is a
/// remote-control channel for anyone on it.
pub fn serve(port: u16, clock: QpcClock, lines: SyncSender<String>) -> Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
    eprintln!("input: listening on 127.0.0.1:{port}");
    loop {
        match listener.accept() {
            Ok((stream, peer)) => {
                eprintln!("input: connected {peer}");
                if let Err(e) = serve_one(stream, clock, &lines) {
                    eprintln!("input: connection ended: {e}");
                }
                eprintln!("input: disconnected");
            }
            Err(e) => {
                eprintln!("input: accept failed: {e}");
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
    }
}
