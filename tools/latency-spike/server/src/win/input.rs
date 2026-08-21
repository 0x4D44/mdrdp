//! The input channel — stage 5, and the other half of the round trip.
//!
//! Accepts on `127.0.0.1:<input-port>`, reads one v2 record at a time (see
//! [`crate::input_proto`]: a `kind` byte, then `kind_len(kind) - 1` more bytes),
//! stamps QPC on arrival, injects with `SendInput`, stamps again, and emits stats —
//! one `input`/`mouse` JSONL line per record for keys, buttons and wheel notches, a
//! periodic aggregate for mouse motion (§5.2, review S-m5: per-record JSON on the
//! injection thread at motion rate is a latency bug in waiting).
//!
//! `SendInput` posts to the input queue of the **session the process runs in**. The
//! server therefore has to run in the interactive console session; from a service or
//! a session-0 context the calls succeed and nothing happens. The README says so, and
//! a zero return from `SendInput` is reported rather than ignored so the failure is
//! visible in the log instead of showing up as "the keystrokes do nothing".

use super::{qpc, Result};
use crate::input_proto::{self, KeyKind, MouseButton, Record, WheelAxis};
use crate::stats::{self, InputEventRecord, MouseEventRecord, MouseMoveSummaryRecord, QpcClock};
use std::io::Read;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::mpsc::SyncSender;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
    VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    XBUTTON1, XBUTTON2,
};

/// Inject one key transition. Returns the QPC stamp taken immediately after the
/// call, and an error if the injection was rejected.
///
/// Unchanged from the latency rig's original shape — the measurement rig depends on
/// this exact behaviour (§5.1).
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

/// Inject one scancode transition, layout-independent by construction
/// (`KEYEVENTF_SCANCODE`). `scancode` is the wire convention (§5.1): low byte the
/// set-1 code, high byte `0xE0` when extended, 0x00 otherwise.
fn inject_scan(scancode: u16, down: bool) -> Result<i64> {
    let extended = (scancode >> 8) as u8 == 0xE0;
    let mut flags = KEYEVENTF_SCANCODE;
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: scancode & 0x00FF,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: as `inject` above.
    let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
    let stamp = qpc::now();
    if sent != 1 {
        return Err(format!(
            "SendInput injected {sent} of 1 events for scancode {scancode:#06x}; \
             the process is probably not in the interactive session"
        )
        .into());
    }
    Ok(stamp)
}

/// Inject one absolute mouse move. `local` is the wire's capture-display-local
/// pixel (`x, y`); `origin` is that display's own offset into the virtual desktop
/// ([`super::source::FrameSource::origin`]). Maps through
/// [`input_proto::map_to_virtual_desk`] into `SendInput`'s
/// `ABSOLUTE|VIRTUALDESK` space (§5.2).
fn inject_mouse_move(local_x: u16, local_y: u16, origin: (i32, i32)) -> Result<i64> {
    // SAFETY: plain syscalls, no pointers cross the FFI boundary.
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let dx = input_proto::map_to_virtual_desk(local_x, origin.0, vx, vw);
    let dy = input_proto::map_to_virtual_desk(local_y, origin.1, vy, vh);
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: as `inject` above.
    let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
    let stamp = qpc::now();
    if sent != 1 {
        return Err(format!(
            "SendInput injected {sent} of 1 mouse-move events; \
             the process is probably not in the interactive session"
        )
        .into());
    }
    Ok(stamp)
}

/// Inject one mouse-button transition. X1/X2 carry their button ordinal in
/// `mouseData` (`XBUTTON1`/`XBUTTON2`); the other three encode the transition in
/// `dwFlags` alone.
fn inject_mouse_button(button: MouseButton, down: bool) -> Result<i64> {
    let (flags, mouse_data) = match (button, down) {
        (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0u32),
        (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
        (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (MouseButton::X1, true) => (MOUSEEVENTF_XDOWN, XBUTTON1 as u32),
        (MouseButton::X1, false) => (MOUSEEVENTF_XUP, XBUTTON1 as u32),
        (MouseButton::X2, true) => (MOUSEEVENTF_XDOWN, XBUTTON2 as u32),
        (MouseButton::X2, false) => (MOUSEEVENTF_XUP, XBUTTON2 as u32),
    };
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: mouse_data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: as `inject` above.
    let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
    let stamp = qpc::now();
    if sent != 1 {
        return Err(format!(
            "SendInput injected {sent} of 1 mouse-button events; \
             the process is probably not in the interactive session"
        )
        .into());
    }
    Ok(stamp)
}

/// Inject one wheel notch. `delta120` is already validated a nonzero multiple of
/// 120 by [`input_proto::decode_record`].
fn inject_wheel(axis: WheelAxis, delta120: i16) -> Result<i64> {
    let flags = match axis {
        WheelAxis::Vertical => MOUSEEVENTF_WHEEL,
        WheelAxis::Horizontal => MOUSEEVENTF_HWHEEL,
    };
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: delta120 as i32 as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: as `inject` above.
    let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
    let stamp = qpc::now();
    if sent != 1 {
        return Err(format!(
            "SendInput injected {sent} of 1 wheel events; \
             the process is probably not in the interactive session"
        )
        .into());
    }
    Ok(stamp)
}

fn mouse_button_kind(button: MouseButton, down: bool) -> &'static str {
    match (button, down) {
        (MouseButton::Left, true) => "btn_left_down",
        (MouseButton::Left, false) => "btn_left_up",
        (MouseButton::Right, true) => "btn_right_down",
        (MouseButton::Right, false) => "btn_right_up",
        (MouseButton::Middle, true) => "btn_middle_down",
        (MouseButton::Middle, false) => "btn_middle_up",
        (MouseButton::X1, true) => "btn_x1_down",
        (MouseButton::X1, false) => "btn_x1_up",
        (MouseButton::X2, true) => "btn_x2_down",
        (MouseButton::X2, false) => "btn_x2_up",
    }
}

/// Emit a summary line at most this often, in injected moves. Motion runs at up to
/// hundreds of records a second; a line every 200 keeps the stats file live without
/// putting per-record JSON serialisation on the injection thread's critical path
/// (§5.2, review S-m5).
const MOVE_SUMMARY_INTERVAL: u64 = 200;

/// Counts injected `MouseMove` records between summary lines. `record` counts one
/// move and flushes automatically at [`MOVE_SUMMARY_INTERVAL`]; `flush` sends
/// whatever is pending (a partial window at connection end, or nothing).
#[derive(Default)]
struct MoveAggregate {
    count: u64,
    failed: u64,
    window_start_qpc: Option<i64>,
    window_end_qpc: i64,
}

impl MoveAggregate {
    fn record(
        &mut self,
        recv_qpc: i64,
        injected: bool,
        clock: QpcClock,
        lines: &SyncSender<String>,
    ) {
        if self.window_start_qpc.is_none() {
            self.window_start_qpc = Some(recv_qpc);
        }
        self.window_end_qpc = recv_qpc;
        self.count += 1;
        self.failed += u64::from(!injected);
        if self.count >= MOVE_SUMMARY_INTERVAL {
            self.flush(clock, lines);
        }
    }

    fn flush(&mut self, clock: QpcClock, lines: &SyncSender<String>) {
        if self.count == 0 {
            return;
        }
        let start = self.window_start_qpc.unwrap_or(self.window_end_qpc);
        let line = stats::to_line(&MouseMoveSummaryRecord::new(
            self.count,
            self.failed,
            clock.micros(start),
            clock.micros(self.window_end_qpc),
        ));
        // A full queue drops the *summary line*, never any injection.
        let _ = lines.try_send(line);
        self.count = 0;
        self.failed = 0;
        self.window_start_qpc = None;
    }
}

fn emit_key_line(
    lines: &SyncSender<String>,
    clock: QpcClock,
    seq: u32,
    code: u16,
    kind: &'static str,
    recv_qpc: i64,
    injected_qpc: i64,
    injected: bool,
) {
    let line = stats::to_line(&InputEventRecord::new(
        seq,
        code,
        kind,
        clock.micros(recv_qpc),
        clock.micros(injected_qpc),
        injected,
    ));
    // A full queue drops the *stats line*, never the injection — the injection
    // already happened before this is called, so latency is untouched either way.
    let _ = lines.try_send(line);
}

fn emit_mouse_line(
    lines: &SyncSender<String>,
    clock: QpcClock,
    seq: u32,
    kind: &'static str,
    value: i32,
    recv_qpc: i64,
    injected_qpc: i64,
    injected: bool,
) {
    let line = stats::to_line(&MouseEventRecord::new(
        seq,
        kind,
        value,
        clock.micros(recv_qpc),
        clock.micros(injected_qpc),
        injected,
    ));
    let _ = lines.try_send(line);
}

/// Handle one decoded record: inject it, then account for it in stats (per-record
/// for everything except `MouseMove`, which only counts — see [`MoveAggregate`]).
fn handle_record(
    record: Record,
    recv_qpc: i64,
    origin: (i32, i32),
    clock: QpcClock,
    lines: &SyncSender<String>,
    moves: &mut MoveAggregate,
) {
    let injection = |result: Result<i64>| match result {
        Ok(stamp) => (stamp, true),
        Err(e) => {
            eprintln!("input: {e}");
            (qpc::now(), false)
        }
    };
    match record {
        Record::VkDown { vk, seq } => {
            let (injected_qpc, injected) = injection(inject(vk, KeyKind::Down));
            emit_key_line(
                lines,
                clock,
                seq,
                vk,
                "down",
                recv_qpc,
                injected_qpc,
                injected,
            );
        }
        Record::VkUp { vk, seq } => {
            let (injected_qpc, injected) = injection(inject(vk, KeyKind::Up));
            emit_key_line(
                lines,
                clock,
                seq,
                vk,
                "up",
                recv_qpc,
                injected_qpc,
                injected,
            );
        }
        Record::ScanDown { scancode, seq } => {
            let (injected_qpc, injected) = injection(inject_scan(scancode, true));
            emit_key_line(
                lines,
                clock,
                seq,
                scancode,
                "scan_down",
                recv_qpc,
                injected_qpc,
                injected,
            );
        }
        Record::ScanUp { scancode, seq } => {
            let (injected_qpc, injected) = injection(inject_scan(scancode, false));
            emit_key_line(
                lines,
                clock,
                seq,
                scancode,
                "scan_up",
                recv_qpc,
                injected_qpc,
                injected,
            );
        }
        Record::MouseMove { x, y, .. } => {
            let (_, injected) = injection(inject_mouse_move(x, y, origin));
            // No per-record stats line — see `MoveAggregate` and the module docs.
            moves.record(recv_qpc, injected, clock, lines);
        }
        Record::MouseButton { button, down, seq } => {
            let kind = mouse_button_kind(button, down);
            let (injected_qpc, injected) = injection(inject_mouse_button(button, down));
            emit_mouse_line(
                lines,
                clock,
                seq,
                kind,
                button as i32,
                recv_qpc,
                injected_qpc,
                injected,
            );
        }
        Record::Wheel {
            axis,
            delta120,
            seq,
        } => {
            let kind = match axis {
                WheelAxis::Vertical => "wheel_v",
                WheelAxis::Horizontal => "wheel_h",
            };
            let (injected_qpc, injected) = injection(inject_wheel(axis, delta120));
            emit_mouse_line(
                lines,
                clock,
                seq,
                kind,
                delta120 as i32,
                recv_qpc,
                injected_qpc,
                injected,
            );
        }
    }
}

fn serve_one(
    mut stream: TcpStream,
    clock: QpcClock,
    lines: &SyncSender<String>,
    origin: (i32, i32),
) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    let mut buf = [0u8; input_proto::MAX_RECORD_LEN];
    let mut moves = MoveAggregate::default();
    loop {
        // Read the kind byte first — a frameless stream cannot know how many more
        // bytes belong to this record until it knows the kind (`kind_len`, §5.1).
        //
        // A short read here is not a partial record to be retried: `read_exact`
        // either fills the buffer or the peer closed mid-record, and a stream that
        // is out of phase can never be resynchronised in a frameless protocol —
        // exactly the behaviour the original fixed-8-byte reader had, preserved.
        match stream.read_exact(&mut buf[..1]) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                moves.flush(clock, lines);
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        let kind = buf[0];
        let Some(len) = input_proto::kind_len(kind) else {
            eprintln!("input: closing connection: unknown record kind {kind}");
            moves.flush(clock, lines);
            return Ok(());
        };
        if len > 1 {
            match stream.read_exact(&mut buf[1..len]) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    moves.flush(clock, lines);
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
        }
        let recv_qpc = qpc::now();
        let record = match input_proto::decode_record(&buf[..len]) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("input: closing connection: {e}");
                moves.flush(clock, lines);
                return Ok(());
            }
        };
        handle_record(record, recv_qpc, origin, clock, lines, &mut moves);
    }
}

/// Log this thread's window station and desktop names, and whether that desktop is
/// the one currently receiving input. Read-only: it queries, it never switches.
///
/// The diagnostic for "SendInput succeeds and nothing happens": a process launched
/// by a scheduled task can land on a non-interactive station (`Service-0x…-…$`) or a
/// desktop other than the console's input desktop, where injection goes nowhere.
fn log_input_desktop() {
    use windows::Win32::System::StationsAndDesktops::{
        GetProcessWindowStation, GetThreadDesktop, GetUserObjectInformationW, UOI_NAME,
    };
    use windows::Win32::System::Threading::GetCurrentThreadId;

    // SAFETY: each call takes a handle we own for the process lifetime (the station
    // and desktop are not closed here) and a caller-sized buffer; `GetUserObjectInformationW`
    // reports the needed size on failure, which we do not need to grow for names.
    unsafe {
        let name = |get: &dyn Fn() -> Option<windows::Win32::Foundation::HANDLE>| -> String {
            let Some(handle) = get() else {
                return "<none>".to_owned();
            };
            let hobj = windows::Win32::System::StationsAndDesktops::HDESK(handle.0);
            let mut buf = [0u16; 256];
            let mut needed = 0u32;
            let ok = GetUserObjectInformationW(
                windows::Win32::Foundation::HANDLE(hobj.0),
                UOI_NAME,
                Some(buf.as_mut_ptr().cast()),
                (buf.len() * 2) as u32,
                Some(&mut needed),
            );
            if ok.is_err() {
                return "<unreadable>".to_owned();
            }
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            String::from_utf16_lossy(&buf[..len])
        };

        let station = name(&|| {
            GetProcessWindowStation()
                .ok()
                .map(|s| windows::Win32::Foundation::HANDLE(s.0))
        });
        let desktop = name(&|| {
            GetThreadDesktop(GetCurrentThreadId())
                .ok()
                .map(|d| windows::Win32::Foundation::HANDLE(d.0))
        });
        eprintln!("input: window station {station:?}, thread desktop {desktop:?}");
    }
}

/// Run the input listener until the process exits. Intended for its own thread.
///
/// Binds loopback only. That is a hard requirement, not a default: the transport to
/// the Mac is an SSH tunnel, and a keystroke injector reachable from the LAN is a
/// remote-control channel for anyone on it.
///
/// `origin` is the capture display's own offset into the virtual desktop
/// ([`super::source::FrameSource::origin`]) — fixed for the process's lifetime,
/// same as `port` and `clock`.
pub fn serve(
    port: u16,
    clock: QpcClock,
    lines: SyncSender<String>,
    origin: (i32, i32),
) -> Result<()> {
    // `SendInput` reaches a desktop only if this thread is on the input desktop of
    // the console window station. On a headless IddCx host the injection can succeed
    // (returns 1) yet reach nothing — this line names the station+desktop we are
    // actually on, so "keys do nothing" stops being a mystery (see module docs).
    log_input_desktop();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
    eprintln!("input: listening on 127.0.0.1:{port}");
    loop {
        match listener.accept() {
            Ok((stream, peer)) => {
                eprintln!("input: connected {peer}");
                if let Err(e) = serve_one(stream, clock, &lines, origin) {
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
