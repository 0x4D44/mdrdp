//! The input channel — stage 5, and the other half of the round trip.
//!
//! Accepts on `127.0.0.1:<input-port>`, reads one v2 record at a time (see
//! [`crate::input_proto`]: a `kind` byte, then `kind_len(kind) - 1` more bytes),
//! stamps QPC on arrival, injects with `SendInput`, stamps again, and emits stats —
//! one `input`/`mouse` JSONL line per record for keys, buttons and wheel notches, a
//! periodic aggregate for mouse motion (§5.2, review S-m5: per-record JSON on the
//! injection thread at motion rate is a latency bug in waiting).
//!
//! `SendInput` posts to the input queue of the **session and desktop the calling
//! thread runs in**. The server therefore runs in the console session and this thread
//! follows Windows' current input desktop. A bounded 250 ms resynchronisation covers
//! lock/unlock transitions where injection into the stale desktop can report success;
//! a rejected call also synchronises and retries once immediately.

use super::{qpc, Result};
use crate::input_proto::{self, KeyKind, MouseButton, Record, WheelAxis};
use crate::input_state::{
    deliver_with_desktop_sync, trusted_ssh_peer_image, DesktopSyncCadence, HeldInputs,
    InputTransition,
};
use crate::input_stream::{self, ReadRecord};
use crate::stats::{self, InputEventRecord, MouseEventRecord, MouseMoveSummaryRecord, QpcClock};
use std::cell::RefCell;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};
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

/// Once a record's kind byte has arrived, its remaining nine bytes at most must
/// follow promptly. Idle time between complete records remains unlimited.
const RECORD_BODY_TIMEOUT: Duration = Duration::from_secs(1);

thread_local! {
    /// `SetThreadDesktop` makes the selected handle the thread's current
    /// desktop, so Windows will not let us close it. Retain exactly one handle
    /// and close the previous one only after a successful switch replaces it.
    static INPUT_DESKTOP_HANDLE: RefCell<Option<windows::Win32::System::StationsAndDesktops::HDESK>> =
        const { RefCell::new(None) };
}

/// Attach the calling thread to the console session's current input desktop.
///
/// The input listener calls this around `SendInput`. The console-session agent also
/// calls it before reconciliation so children created while Winlogon is active inherit
/// Winlogon rather than cold-starting the virtual display on a hidden Default desktop.
pub fn sync_thread_to_input_desktop() -> Result<String> {
    use windows::Win32::System::StationsAndDesktops::{
        CloseDesktop, GetThreadDesktop, GetUserObjectInformationW, OpenInputDesktop,
        SetThreadDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, UOI_NAME,
    };
    use windows::Win32::System::Threading::GetCurrentThreadId;

    let desktop_name = |desktop: windows::Win32::System::StationsAndDesktops::HDESK| {
        let mut buffer = [0u16; 256];
        let mut needed = 0u32;
        unsafe {
            GetUserObjectInformationW(
                windows::Win32::Foundation::HANDLE(desktop.0),
                UOI_NAME,
                Some(buffer.as_mut_ptr().cast()),
                (buffer.len() * 2) as u32,
                Some(&mut needed),
            )
        }?;
        let len = buffer
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(buffer.len());
        Ok::<_, windows::core::Error>(String::from_utf16_lossy(&buffer[..len]))
    };

    // GENERIC_ALL is required on the secure Winlogon desktop. The service launcher
    // gives this process a SYSTEM token in the console session; a per-user task is
    // deliberately unable to open it.
    let desktop = unsafe {
        OpenInputDesktop(
            DESKTOP_CONTROL_FLAGS(0),
            false,
            DESKTOP_ACCESS_FLAGS(0x1000_0000),
        )
    }?;
    let input_name = match desktop_name(desktop) {
        Ok(name) => name,
        Err(error) => {
            let _ = unsafe { CloseDesktop(desktop) };
            return Err(error.into());
        }
    };
    let current = unsafe { GetThreadDesktop(GetCurrentThreadId()) }?;
    if desktop_name(current).is_ok_and(|name| name == input_name) {
        let _ = unsafe { CloseDesktop(desktop) };
        return Ok(input_name);
    }
    if let Err(error) = unsafe { SetThreadDesktop(desktop) } {
        let _ = unsafe { CloseDesktop(desktop) };
        return Err(error.into());
    }
    INPUT_DESKTOP_HANDLE.with(|current| {
        if let Some(previous) = current.borrow_mut().replace(desktop) {
            let _ = unsafe { CloseDesktop(previous) };
        }
    });
    Ok(input_name)
}

fn send_one(input: INPUT, sync_due: bool, failure: impl FnOnce() -> String) -> Result<i64> {
    let delivered = deliver_with_desktop_sync(
        sync_due,
        || sync_thread_to_input_desktop().map(|_| ()),
        || unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) == 1 },
    )?;
    let stamp = qpc::now();
    if !delivered {
        return Err(failure().into());
    }
    Ok(stamp)
}

/// Inject one key transition. Returns the QPC stamp taken immediately after the
/// call, and an error if the injection was rejected.
///
/// Unchanged from the latency rig's original shape — the measurement rig depends on
/// this exact behaviour (§5.1).
fn inject(vk: u16, kind: KeyKind, sync_due: bool) -> Result<i64> {
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
    send_one(input, sync_due, || {
        format!("SendInput rejected an event for vk {vk:#04x} after desktop resynchronisation")
    })
}

/// Inject one scancode transition, layout-independent by construction
/// (`KEYEVENTF_SCANCODE`). `scancode` is the wire convention (§5.1): low byte the
/// set-1 code, high byte `0xE0` when extended, 0x00 otherwise.
fn inject_scan(scancode: u16, down: bool, sync_due: bool) -> Result<i64> {
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
    send_one(input, sync_due, || {
        format!("SendInput rejected scancode {scancode:#06x} after desktop resynchronisation")
    })
}

/// Inject one absolute mouse move. `local` is the wire's capture-display-local
/// pixel (`x, y`); `origin` is that display's own offset into the virtual desktop
/// ([`super::source::FrameSource::origin`]). Maps through
/// [`input_proto::map_to_virtual_desk`] into `SendInput`'s
/// `ABSOLUTE|VIRTUALDESK` space (§5.2).
fn inject_mouse_move(
    local_x: u16,
    local_y: u16,
    origin: (i32, i32),
    sync_due: bool,
) -> Result<i64> {
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
    send_one(input, sync_due, || {
        "SendInput rejected a mouse move after desktop resynchronisation".to_owned()
    })
}

/// Inject one mouse-button transition. X1/X2 carry their button ordinal in
/// `mouseData` (`XBUTTON1`/`XBUTTON2`); the other three encode the transition in
/// `dwFlags` alone.
fn inject_mouse_button(button: MouseButton, down: bool, sync_due: bool) -> Result<i64> {
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
    send_one(input, sync_due, || {
        "SendInput rejected a mouse button after desktop resynchronisation".to_owned()
    })
}

/// Inject one wheel notch. `delta120` is already validated a nonzero multiple of
/// 120 by [`input_proto::decode_record`].
fn inject_wheel(axis: WheelAxis, delta120: i16, sync_due: bool) -> Result<i64> {
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
    send_one(input, sync_due, || {
        "SendInput rejected a wheel event after desktop resynchronisation".to_owned()
    })
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
    received: (Record, i64),
    sync_due: bool,
    origin: (i32, i32),
    clock: QpcClock,
    lines: &SyncSender<String>,
    moves: &mut MoveAggregate,
    held: &mut HeldInputs,
) {
    let (record, recv_qpc) = received;
    let injection = |result: Result<i64>| match result {
        Ok(stamp) => (stamp, true),
        Err(e) => {
            eprintln!("input: {e}");
            (qpc::now(), false)
        }
    };
    let mut tracked_injection = |result: Result<i64>, transition: InputTransition| {
        let (stamp, injected) = injection(result);
        if injected {
            held.apply(transition);
        }
        (stamp, injected)
    };
    match record {
        Record::VkDown { vk, seq } => {
            let (injected_qpc, injected) = tracked_injection(
                inject(vk, KeyKind::Down, sync_due),
                InputTransition::VirtualKey { vk, down: true },
            );
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
            let (injected_qpc, injected) = tracked_injection(
                inject(vk, KeyKind::Up, sync_due),
                InputTransition::VirtualKey { vk, down: false },
            );
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
            let (injected_qpc, injected) = tracked_injection(
                inject_scan(scancode, true, sync_due),
                InputTransition::Scancode {
                    scancode,
                    down: true,
                },
            );
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
            let (injected_qpc, injected) = tracked_injection(
                inject_scan(scancode, false, sync_due),
                InputTransition::Scancode {
                    scancode,
                    down: false,
                },
            );
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
            let (_, injected) = injection(inject_mouse_move(x, y, origin, sync_due));
            // No per-record stats line — see `MoveAggregate` and the module docs.
            moves.record(recv_qpc, injected, clock, lines);
        }
        Record::MouseButton { button, down, seq } => {
            let kind = mouse_button_kind(button, down);
            let (injected_qpc, injected) = tracked_injection(
                inject_mouse_button(button, down, sync_due),
                InputTransition::MouseButton { button, down },
            );
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
            let (injected_qpc, injected) = injection(inject_wheel(axis, delta120, sync_due));
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

fn release_held(held: &mut HeldInputs) {
    let mut sync_due = true;
    for transition in held.release_plan() {
        let result = match transition {
            InputTransition::VirtualKey { vk, .. } => inject(vk, KeyKind::Up, sync_due),
            InputTransition::Scancode { scancode, .. } => inject_scan(scancode, false, sync_due),
            InputTransition::MouseButton { button, .. } => {
                inject_mouse_button(button, false, sync_due)
            }
        };
        sync_due = false;
        if let Err(e) = result {
            eprintln!("input: disconnect release failed: {e}");
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
    let mut held = HeldInputs::default();
    let desktop_started = Instant::now();
    let mut desktop_sync = DesktopSyncCadence::default();
    let result: std::io::Result<()> = loop {
        let len = match input_stream::read_record(&mut stream, &mut buf, RECORD_BODY_TIMEOUT) {
            Ok(ReadRecord::Complete(len)) => len,
            Ok(ReadRecord::Eof) => break Ok(()),
            Ok(ReadRecord::UnknownKind(kind)) => {
                eprintln!("input: closing connection: unknown record kind {kind}");
                break Ok(());
            }
            Err(e) => break Err(e),
        };
        let recv_qpc = qpc::now();
        let record = match input_proto::decode_record(&buf[..len]) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("input: closing connection: {e}");
                break Ok(());
            }
        };
        let sync_due = desktop_sync.due(desktop_started.elapsed());
        handle_record(
            (record, recv_qpc),
            sync_due,
            origin,
            clock,
            lines,
            &mut moves,
            &mut held,
        );
    };
    moves.flush(clock, lines);
    release_held(&mut held);
    result
}

/// Log this thread's window station and desktop names after synchronisation.
///
/// The diagnostic for "SendInput succeeds and nothing happens": the input desktop
/// can change between Winlogon and Default while this thread remains on the stale one.
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

fn trusted_input_peer(stream: &TcpStream) -> Result<String> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_ALL,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let local_port = stream.local_addr()?.port();
    let peer_port = stream.peer_addr()?.port();
    let mut size = 0u32;
    let _ = unsafe { GetExtendedTcpTable(None, &mut size, false, 2, TCP_TABLE_OWNER_PID_ALL, 0) };
    if size == 0 {
        return Err("Windows returned no TCP owner table size".into());
    }
    let mut buffer = vec![0u8; size as usize];
    let result = unsafe {
        GetExtendedTcpTable(
            Some(buffer.as_mut_ptr().cast()),
            &mut size,
            false,
            2,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        )
    };
    if result != 0 {
        return Err(format!("GetExtendedTcpTable failed with {result}").into());
    }
    let table = unsafe { &*buffer.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>() };
    let rows =
        unsafe { std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize) };
    const ESTABLISHED: u32 = 5;
    let peer_pid = rows
        .iter()
        .find(|row| {
            row.dwState == ESTABLISHED
                && row.dwLocalPort == u32::from(peer_port.to_be())
                && row.dwRemotePort == u32::from(local_port.to_be())
        })
        .map(|row| row.dwOwningPid)
        .ok_or_else(|| "could not identify the reverse loopback connection owner".to_owned())?;

    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, peer_pid) }
        .map_err(|error| format!("OpenProcess({peer_pid}) failed: {error}"))?;
    struct ProcessHandle(HANDLE);
    impl Drop for ProcessHandle {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
    let process = ProcessHandle(process);
    let mut image = vec![0u16; 32_768];
    let mut len = image.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(image.as_mut_ptr()),
            &mut len,
        )
    }
    .map_err(|error| format!("QueryFullProcessImageNameW({peer_pid}) failed: {error}"))?;
    image.truncate(len as usize);
    let image = String::from_utf16_lossy(&image);
    let windows_root = std::env::var("SystemRoot")
        .map_err(|_| "SystemRoot is missing from the service environment".to_owned())?;
    if !trusted_ssh_peer_image(&image, &windows_root) {
        return Err(format!("input peer process is not Windows OpenSSH: {image:?}").into());
    }
    Ok(image)
}

/// Run the input listener until the process exits. Intended for its own thread.
///
/// Binds loopback only. That is a hard requirement, not a default: the transport to
/// the Mac is an SSH tunnel, and a keystroke injector reachable from the LAN is a
/// remote-control channel for anyone on it. Because this process can follow secure
/// desktops as SYSTEM, accepted peers must also resolve to Windows' protected
/// `sshd.exe`; loopback alone is not an authorisation boundary.
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
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
    serve_listener(listener, clock, lines, origin)
}

/// Serve on a listener synchronously pre-bound by the session startup gate.
pub(crate) fn serve_listener(
    listener: TcpListener,
    clock: QpcClock,
    lines: SyncSender<String>,
    origin: (i32, i32),
) -> Result<()> {
    // Start on the live input desktop so the first PIN event is never sacrificed
    // to discovering a desktop transition. A failed sync remains visible and each
    // rejected SendInput will retry it.
    if let Err(error) = sync_thread_to_input_desktop() {
        eprintln!("input: initial desktop synchronisation failed: {error}");
    }
    log_input_desktop();
    let port = listener.local_addr()?.port();
    eprintln!("input: listening on 127.0.0.1:{port}");
    loop {
        match listener.accept() {
            Ok((stream, peer)) => {
                match trusted_input_peer(&stream) {
                    Ok(image) => eprintln!("input: authorised tunnel peer {image:?}"),
                    Err(error) => {
                        eprintln!("input: rejected unauthorised peer {peer}: {error}");
                        continue;
                    }
                }
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
