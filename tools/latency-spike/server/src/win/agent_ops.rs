//! The Windows implementation of the agent's platform operations.
//!
//! Children are plain `std::process::Child`ren: `try_wait` gives the reap-once
//! contract [`ChildState`] documents, and `Drop` kills whatever is still running
//! on a clean exit. `Drop` never runs on a hard kill (`taskkill /f`), which is
//! why every agent start begins with [`WinOps::sweep_orphans`] — a previous
//! agent's children would otherwise hold the device and ports 9500–9502 and the
//! fresh supervision would crash-loop on bind.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    ChangeDisplaySettingsExW, EnumDisplayDevicesW, EnumDisplaySettingsW, MonitorFromPoint,
    CDS_UPDATEREGISTRY, DEVMODEW, DISPLAY_DEVICEW, DISP_CHANGE_SUCCESSFUL, DM_DISPLAYFREQUENCY,
    DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_CURRENT_SETTINGS, MONITOR_DEFAULTTONULL,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, WS_DISABLED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

use super::wide_to_string;
use crate::agent::{AgentOps, ChildState, InputDesktopObservation, Mode, PoolObservation};

fn scale_percent_from_dpi(dpi: u32) -> Option<u32> {
    if dpi == 0 {
        return None;
    }
    u32::try_from((u64::from(dpi) * 100 + 48) / 96).ok()
}

/// A short-lived, non-activating window used only to ask Windows which effective DPI it
/// assigns content on a monitor. Destruction must happen on the creating thread, which is
/// why the guard never leaves `display_scale_percent`.
struct DpiProbeWindow(HWND);

impl DpiProbeWindow {
    fn at(point: windows::Win32::Foundation::POINT) -> Option<Self> {
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                w!("STATIC"),
                PCWSTR::null(),
                WS_POPUP | WS_DISABLED,
                point.x,
                point.y,
                1,
                1,
                None,
                None,
                None,
                None,
            )
        }
        .ok()?;
        Some(Self(hwnd))
    }

    fn dpi(&self) -> u32 {
        unsafe { GetDpiForWindow(self.0) }
    }
}

impl Drop for DpiProbeWindow {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

/// The device string the IDD driver's INF declares — the same one the rig's
/// scripts key on.
pub const DISPLAY_DEVICE_STRING: &str = "mdrdp latency-spike display";

/// Where the IDD virtual display's top-left corner sits in the virtual desktop,
/// in physical pixels — `DEVMODEW::dmPosition` for the display whose device
/// string is [`DISPLAY_DEVICE_STRING`]. `None` when the display is not attached.
///
/// The IDD shared-pool header carries no desktop coordinates, so the source
/// cannot learn its own placement from the pool the way Desktop Duplication
/// learns it from `DXGI_OUTPUT_DESC::DesktopCoordinates`; GDI is the only path
/// to it. Without this the mouse injector mapped every click as though the
/// display sat at the desktop origin, which is correct only while it happens to
/// be the sole display: attaching a console pushed it to (1920, 0) and every
/// click landed 1920 px to its left, on the other display (HLD tranche 3 §5.2,
/// AC2's corner clicks).
pub fn idd_display_origin() -> Option<(i32, i32)> {
    let device_name = WinOps::find_display()?;
    let mut devmode = DEVMODEW {
        dmSize: std::mem::size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    // SAFETY: `device_name` is a NUL-terminated wide buffer we own, and the
    // out-parameter is a correctly-sized `DEVMODEW` whose `dmSize` we set.
    let ok = unsafe {
        EnumDisplaySettingsW(
            PCWSTR::from_raw(device_name.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            &mut devmode,
        )
    }
    .as_bool();
    if !ok {
        return None;
    }
    // SAFETY: `dmPosition` is the active member for a display device queried with
    // ENUM_CURRENT_SETTINGS — the same union arm the rig's prep script reads.
    let position = unsafe { devmode.Anonymous1.Anonymous2.dmPosition };
    Some((position.x, position.y))
}

/// Whether anything is in LISTEN on the capture server's video port.
///
/// Asked of the OS's TCP table, never by connecting. A probe connect would be
/// accepted as the server's one and only viewer (`win::send`'s `poll_accept`
/// takes a connection only while it is free), unparking the capture loop and
/// occupying the slot a real session needs — a health check that breaks the
/// thing it measures.
///
/// `None` means the table could not be read: that is ignorance, and the rung
/// reports it as `Unknown` rather than inventing a fault.
fn video_port_listening() -> Option<bool> {
    tcp_video_port_states().map(|(listening, _)| listening)
}

/// Whether a viewer holds the capture server's single slot.
///
/// The same table read as the LISTEN check, so it costs nothing extra and — like
/// that check — never touches the server. An ESTABLISHED connection to the video
/// port is a viewer by construction: the port carries nothing else.
fn video_port_has_viewer() -> Option<bool> {
    tcp_video_port_states().map(|(_, established)| established)
}

/// One read of the OS's TCP table: is anything LISTENING on the video port, and
/// is anything CONNECTED to it?
fn tcp_video_port_states() -> Option<(bool, bool)> {
    use windows::Win32::NetworkManagement::IpHelper::{GetTcpTable2, MIB_TCPTABLE2};

    // Ask for the size first; the table is a variable-length trailing array, so
    // there is no single correct fixed buffer.
    let mut size: u32 = 0;
    // SAFETY: a null table pointer with a zero size is the documented
    // "tell me how big" call; it writes only through `size`.
    let _ = unsafe { GetTcpTable2(None, &mut size, false) };
    if size == 0 {
        return None;
    }
    let mut buffer = vec![0u8; size as usize];
    // SAFETY: `buffer` is `size` bytes and stays alive for the call; the API
    // writes at most `size` bytes and updates `size` with what it used.
    let rc = unsafe {
        GetTcpTable2(
            Some(buffer.as_mut_ptr().cast::<MIB_TCPTABLE2>()),
            &mut size,
            false,
        )
    };
    if rc != 0 {
        return None;
    }
    // SAFETY: on success the buffer holds a `MIB_TCPTABLE2`: a u32 count
    // followed by that many rows.
    let table = unsafe { &*buffer.as_ptr().cast::<MIB_TCPTABLE2>() };
    let count = table.dwNumEntries as usize;
    // SAFETY: `table.table` is the first element of the trailing array the
    // header's count describes, and the buffer was sized by the API for exactly
    // that many rows.
    let rows = unsafe { std::slice::from_raw_parts(table.table.as_ptr(), count) };

    // The port is big-endian in the table; LISTEN is 2 and ESTABLISHED is 5.
    const MIB_TCP_STATE_LISTEN: u32 = 2;
    const MIB_TCP_STATE_ESTAB: u32 = 5;
    let wanted = u32::from(crate::cli::DEFAULT_VIDEO_PORT.to_be());
    let mine = rows.iter().filter(|row| row.dwLocalPort == wanted);
    let mut listening = false;
    let mut established = false;
    for row in mine {
        listening |= row.dwState == MIB_TCP_STATE_LISTEN;
        established |= row.dwState == MIB_TCP_STATE_ESTAB;
    }
    Some((listening, established))
}

/// Whether the desktop that would receive injected input is the one this stack
/// is on (HLD tranche 4 §6 rung 5).
///
/// Compares the **agent's own** thread desktop against `OpenInputDesktop`. It
/// deliberately does not claim to inspect the injector: that thread lives in the
/// *server* process and Windows offers no way to read another process's thread
/// desktop. The agent spawns the server with an inherited station and desktop, so
/// its own is a sound proxy — stated here rather than implied.
fn observe_input_desktop() -> InputDesktopObservation {
    use windows::Win32::System::StationsAndDesktops::{
        CloseDesktop, GetThreadDesktop, GetUserObjectInformationW, OpenInputDesktop,
        DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, UOI_NAME,
    };
    use windows::Win32::System::Threading::GetCurrentThreadId;

    // SAFETY: `handle` is a live desktop handle; the buffer is caller-sized and
    // `GetUserObjectInformationW` writes at most what it is told.
    let name_of = |handle: windows::Win32::Foundation::HANDLE| -> Option<String> {
        let mut buf = [0u16; 256];
        let mut needed = 0u32;
        let ok = unsafe {
            GetUserObjectInformationW(
                handle,
                UOI_NAME,
                Some(buf.as_mut_ptr().cast()),
                (buf.len() * 2) as u32,
                Some(&mut needed),
            )
        };
        if ok.is_err() {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    };

    // SAFETY: a plain query for this thread's desktop; the handle is owned by
    // the thread and must not be closed.
    let ours = match unsafe { GetThreadDesktop(GetCurrentThreadId()) } {
        Ok(d) => match name_of(windows::Win32::Foundation::HANDLE(d.0)) {
            Some(n) => n,
            None => {
                return InputDesktopObservation::Unknown(
                    "could not read this thread's desktop name".to_owned(),
                )
            }
        },
        Err(e) => return InputDesktopObservation::Unknown(format!("GetThreadDesktop failed: {e}")),
    };

    // SAFETY: opens a handle we close below. Access-denied is expected and
    // handled rather than treated as evidence of anything.
    let input = match unsafe {
        OpenInputDesktop(
            DESKTOP_CONTROL_FLAGS(0),
            false,
            DESKTOP_ACCESS_FLAGS(0x0001),
        )
    } {
        Ok(d) => d,
        Err(e) => {
            // Access-denied is ALSO what a caller in a different session or
            // window station gets, so this cannot distinguish "locked" from
            // "looking from the wrong place". Report ignorance.
            return InputDesktopObservation::Unknown(format!(
                "OpenInputDesktop failed ({e}) — this may mean the secure desktop is up, or \
                 simply that the agent is not in the console session"
            ));
        }
    };
    let input_name = name_of(windows::Win32::Foundation::HANDLE(input.0));
    // SAFETY: `input` came from `OpenInputDesktop` and is not used again.
    let _ = unsafe { CloseDesktop(input) };

    match input_name {
        None => {
            InputDesktopObservation::Unknown("could not read the input desktop's name".to_owned())
        }
        Some(name) if name == ours => InputDesktopObservation::Matches { desktop: ours },
        Some(name) => InputDesktopObservation::Differs { ours, input: name },
    }
}

/// The images the agent owns on the box. Swept at start, killed at shutdown.
/// Deliberately does NOT include the agent's own image (an uninstall would kill
/// itself) or the rig's `spike-server-inc3.exe`.
pub const OWNED_IMAGES: [&str; 2] = ["rhydra-server.exe", "mdrdp-idd-create.exe"];

pub struct WinOps {
    /// Directory holding the sibling exes; logs go to `<root>\logs\`.
    root: PathBuf,
    creator: Option<Child>,
    server: Option<Child>,
}

impl WinOps {
    pub fn new(root: PathBuf) -> Self {
        if let Err(error) = super::audio_policy::recover_stale_lease() {
            eprintln!("audio: stale default lease recovery deferred: {error}");
        }
        Self {
            root,
            creator: None,
            server: None,
        }
    }

    /// Kill stray owned images from a previous agent whose `Drop` never ran.
    pub fn sweep_orphans(&mut self) {
        for image in OWNED_IMAGES {
            // Exit status deliberately ignored: 128 just means "no such process".
            let _ = Command::new("taskkill")
                .args(["/f", "/im", image])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

    fn poll(child: &mut Option<Child>) -> ChildState {
        match child.as_mut() {
            None => ChildState::NotStarted,
            Some(c) => match c.try_wait() {
                Ok(None) => ChildState::Running,
                Ok(Some(status)) => {
                    let code = status.code().unwrap_or(-1);
                    *child = None;
                    ChildState::Exited(code)
                }
                Err(_) => {
                    *child = None;
                    ChildState::Exited(-1)
                }
            },
        }
    }

    /// One append-mode log per child, under `<root>\logs\`.
    fn child_log(&self, name: &str) -> Result<File, String> {
        let dir = self.root.join("logs");
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let path = dir.join(name);
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("open {}: {e}", path.display()))
    }

    fn spawn(&self, exe: &str, args: &[&str], log: &str) -> Result<Child, String> {
        let path = self.root.join(exe);
        let out = self.child_log(log)?;
        let err = out
            .try_clone()
            .map_err(|e| format!("clone log handle: {e}"))?;
        Command::new(&path)
            .args(args)
            .current_dir(&self.root)
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .spawn()
            .map_err(|e| format!("spawn {}: {e}", path.display()))
    }

    /// The GDI device name (`\\.\DISPLAYn`) of the virtual display, if attached.
    fn find_display() -> Option<[u16; 32]> {
        for index in 0..32u32 {
            let mut device = DISPLAY_DEVICEW {
                cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };
            let found =
                unsafe { EnumDisplayDevicesW(PCWSTR::null(), index, &mut device, 0) }.as_bool();
            if !found {
                return None;
            }
            if wide_to_string(&device.DeviceString) == DISPLAY_DEVICE_STRING {
                return Some(device.DeviceName);
            }
        }
        None
    }

    fn current_mode(device_name: &[u16; 32]) -> Option<Mode> {
        let mut devmode = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        let ok = unsafe {
            EnumDisplaySettingsW(
                PCWSTR::from_raw(device_name.as_ptr()),
                ENUM_CURRENT_SETTINGS,
                &mut devmode,
            )
        }
        .as_bool();
        if !ok {
            return None;
        }
        Some(Mode {
            width: devmode.dmPelsWidth,
            height: devmode.dmPelsHeight,
            hz: devmode.dmDisplayFrequency,
        })
    }
}

impl AgentOps for WinOps {
    fn poll_creator(&mut self) -> ChildState {
        Self::poll(&mut self.creator)
    }

    fn spawn_creator(&mut self) -> Result<(), String> {
        self.creator = Some(self.spawn("mdrdp-idd-create.exe", &["--wait"], "creator.log")?);
        Ok(())
    }

    fn device_id(&mut self) -> Option<String> {
        // The GDI name (`\\.\DISPLAYn`) changes when the device is re-created,
        // which is exactly the identity signal the reconciler keys on.
        Self::find_display().map(|name| wide_to_string(&name))
    }

    fn display_mode(&mut self) -> Option<Mode> {
        Self::find_display().and_then(|name| Self::current_mode(&name))
    }

    fn display_scale_percent(&mut self) -> Option<u32> {
        let name = Self::find_display()?;
        let mode = Self::current_mode(&name)?;
        let mut devmode = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        let ok = unsafe {
            EnumDisplaySettingsW(
                PCWSTR::from_raw(name.as_ptr()),
                ENUM_CURRENT_SETTINGS,
                &mut devmode,
            )
        }
        .as_bool();
        if !ok {
            return None;
        }
        // A point strictly inside this display identifies its HMONITOR without
        // relying on the volatile \\.\DISPLAYn name outside this query.
        let position = unsafe { devmode.Anonymous1.Anonymous2.dmPosition };
        let point = windows::Win32::Foundation::POINT {
            x: position
                .x
                .saturating_add(i32::try_from(mode.width / 2).ok()?),
            y: position
                .y
                .saturating_add(i32::try_from(mode.height / 2).ok()?),
        };
        let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONULL) };
        if monitor.0.is_null() {
            return None;
        }
        let window = DpiProbeWindow::at(point)?;
        scale_percent_from_dpi(window.dpi())
    }

    fn set_display_mode(&mut self, mode: Mode) -> Result<(), String> {
        let name = Self::find_display().ok_or("display not attached")?;
        let devmode = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            dmFields: DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY,
            dmPelsWidth: mode.width,
            dmPelsHeight: mode.height,
            dmDisplayFrequency: mode.hz,
            ..Default::default()
        };
        let result = unsafe {
            ChangeDisplaySettingsExW(
                PCWSTR::from_raw(name.as_ptr()),
                Some(&devmode),
                None,
                CDS_UPDATEREGISTRY,
                None,
            )
        };
        if result == DISP_CHANGE_SUCCESSFUL {
            Ok(())
        } else {
            Err(format!("ChangeDisplaySettingsExW returned {}", result.0))
        }
    }

    fn audio_endpoint(&mut self) -> Option<bool> {
        super::audio::endpoint_available()
    }

    fn poll_server(&mut self) -> ChildState {
        Self::poll(&mut self.server)
    }

    fn spawn_server(&mut self) -> Result<(), String> {
        // IDD source: bound to our display by the shared section's well-known
        // name, so no output-index guessing. Stats land beside the logs.
        let stats = self.root.join("logs").join("server-stats.jsonl");
        let stats = stats.to_string_lossy().into_owned();
        let audio = audio_source_arg(&self.root);
        self.server = Some(self.spawn(
            "rhydra-server.exe",
            &["--source", "idd", "--out", &stats, "--audio-source", &audio],
            "server.log",
        )?);
        Ok(())
    }

    fn kill_server(&mut self) {
        if let Some(mut c) = self.server.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    fn kill_creator(&mut self) {
        // Taking the creator down takes the virtual device with it, which is what
        // makes the driver rebuild and republish the section. Supervision
        // respawns the creator on the next tick.
        if let Some(mut c) = self.creator.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    fn pool(&mut self) -> PoolObservation {
        super::idd_source::observe_pool()
    }

    fn server_listening(&mut self) -> Option<bool> {
        video_port_listening()
    }

    fn input_desktop(&mut self) -> InputDesktopObservation {
        observe_input_desktop()
    }

    fn viewer_connected(&mut self) -> Option<bool> {
        video_port_has_viewer()
    }

    fn kill_all(&mut self) {
        self.kill_server();
        if let Some(mut c) = self.creator.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for WinOps {
    fn drop(&mut self) {
        self.kill_all();
    }
}

#[cfg(test)]
mod tests {
    use super::scale_percent_from_dpi;

    #[test]
    fn effective_dpi_converts_to_windows_scale_percent() {
        assert_eq!(scale_percent_from_dpi(0), None);
        assert_eq!(scale_percent_from_dpi(96), Some(100));
        assert_eq!(scale_percent_from_dpi(192), Some(200));
        assert_eq!(scale_percent_from_dpi(173), Some(180));
    }
}

/// Where the agent's exes and logs live: the directory of the running exe.
/// No environment variable, no config file — deploy puts everything in one place.
pub fn exe_root() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "agent exe has no parent directory".to_owned())
}

/// Which audio source the supervised server should use.
///
/// Read from a one-word file, `audio-source`, beside the binaries; absent means
/// `off`, which is the only safe default.
///
/// **This knob is temporary and exists for one reason.** WASAPI loopback capture
/// does not exist yet, so the only working host source is the synthetic tone —
/// and baking that into the agent would ship a test signal as the product's
/// audio. When loopback lands, the agent should pass it unconditionally and this
/// function should go.
///
/// Anything other than the exact word `tone` reads as `off`, including a typo. A
/// value that silently fell back to the tone would put a 440 Hz sine into a real
/// session; a value that falls back to silence is merely unhelpful.
fn audio_source_arg(root: &std::path::Path) -> String {
    match std::fs::read_to_string(root.join("audio-source")) {
        // **`loopback` was missing here, and its absence made the design's
        // central promise false.** The HLD says capture starts working the
        // moment a host grows an endpoint, "with no other change" — but the
        // supervised agent mapped every value except `tone` to `off`, so the
        // deployed path could never select real capture at all. Only a manual
        // server invocation could, which is not how any fleet host runs.
        Ok(s) if s.trim() == "tone" => "tone".to_owned(),
        Ok(s) if s.trim() == "loopback" => "loopback".to_owned(),
        // Anything unrecognised, including a typo, reads as off. A value that
        // silently fell back to a *working* source would be worse: a typo would
        // start streaming audio nobody asked for.
        _ => "off".to_owned(),
    }
}
