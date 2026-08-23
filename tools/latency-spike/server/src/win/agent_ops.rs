//! The Windows implementation of the agent's platform operations.
//!
//! Children are plain `std::process::Child`ren: `try_wait` gives the reap-once
//! contract [`ChildState`] documents, and `Drop` kills whatever is still running
//! on a clean exit. `Drop` never runs on a hard kill, which is
//! why every agent start begins with [`WinOps::sweep_orphans`] — a previous
//! agent's children would otherwise hold the device and ports 9500–9502 and the
//! fresh supervision would crash-loop on bind.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig, SetDisplayConfig,
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_MODE_INFO_TYPE_DESKTOP_IMAGE,
    DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE, DISPLAYCONFIG_MODE_INFO_TYPE_TARGET,
    DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
    QDC_VIRTUAL_MODE_AWARE, SDC_APPLY, SDC_SAVE_TO_DATABASE, SDC_USE_SUPPLIED_DISPLAY_CONFIG,
    SDC_VALIDATE, SDC_VIRTUAL_MODE_AWARE,
};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HANDLE, HWND, POINTL, WAIT_OBJECT_0,
};
use windows::Win32::Graphics::Gdi::{
    ChangeDisplaySettingsExW, EnumDisplayDevicesW, EnumDisplaySettingsW, MonitorFromPoint,
    CDS_UPDATEREGISTRY, DEVMODEW, DISPLAYCONFIG_PATH_SUPPORT_VIRTUAL_MODE, DISPLAY_DEVICEW,
    DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, DISPLAY_DEVICE_PRIMARY_DEVICE, DISP_CHANGE_SUCCESSFUL,
    DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_CURRENT_SETTINGS, MONITOR_DEFAULTTONULL,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, WS_DISABLED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

use super::wide_to_string;
use crate::agent::{
    AgentOps, ChildState, DisplayPlacement, InputDesktopObservation, Mode, PoolObservation,
};
use crate::process_ownership::same_windows_executable;

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
    let devmode = WinOps::current_devmode(&device_name)?;
    // SAFETY: `dmPosition` is the active member for a display device queried with
    // ENUM_CURRENT_SETTINGS — the same union arm the rig's prep script reads.
    let position = unsafe { devmode.Anonymous1.Anonymous2.dmPosition };
    Some((position.x, position.y))
}

/// A currently attached desktop display and the state needed to stage a
/// multi-display ChangeDisplaySettingsEx transaction.
struct AttachedDisplay {
    name: [u16; 32],
    mode: DEVMODEW,
    idd: bool,
    primary: bool,
}

struct TopologyError {
    detail: String,
    mode_restore_safe: bool,
}

impl TopologyError {
    fn unchanged(detail: String) -> Self {
        Self {
            detail,
            mode_restore_safe: true,
        }
    }

    fn uncertain(detail: String) -> Self {
        Self {
            detail,
            mode_restore_safe: false,
        }
    }
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

struct ProcessHandle(HANDLE);

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // SAFETY: this guard owns the handle returned by a Win32 open/snapshot call.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn process_image_path(process: HANDLE) -> Result<PathBuf, String> {
    let mut buffer = vec![0u16; 32_768];
    let mut len = buffer.len() as u32;
    // SAFETY: `process` remains open for the call and the buffer advertises its full size.
    unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        )
    }
    .map_err(|error| format!("QueryFullProcessImageNameW: {error}"))?;
    buffer.truncate(len as usize);
    Ok(PathBuf::from(String::from_utf16_lossy(&buffer)))
}

/// Terminate only processes whose opened executable resolves to a sibling in this
/// Rhydra installation. Image names are merely a cheap candidate filter; the full
/// path read from the opened process handle is the ownership check.
pub fn terminate_owned_processes(
    root: &Path,
    image_names: &[&str],
    excluded_pid: Option<u32>,
) -> Result<usize, String> {
    let expected: Vec<PathBuf> = image_names
        .iter()
        .map(|image| {
            let path = root.join(image);
            std::fs::canonicalize(&path).unwrap_or(path)
        })
        .collect();

    // SAFETY: the returned snapshot handle is immediately placed under an owning guard.
    let snapshot = ProcessHandle(
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
            .map_err(|error| format!("process snapshot: {error}"))?,
    );
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    unsafe { Process32FirstW(snapshot.0, &mut entry) }
        .map_err(|error| format!("read process snapshot: {error}"))?;

    let mut terminated = 0;
    let mut failures = Vec::new();
    loop {
        let pid = entry.th32ProcessID;
        let image = wide_to_string(&entry.szExeFile);
        if Some(pid) != excluded_pid
            && image_names
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(&image))
        {
            // SAFETY: PID came from the snapshot. The path and terminate operations use
            // this same opened handle, so PID reuse cannot redirect the termination.
            match unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                    false,
                    pid,
                )
            } {
                Ok(handle) => {
                    let process = ProcessHandle(handle);
                    match process_image_path(process.0) {
                        Ok(actual) => {
                            let actual = std::fs::canonicalize(&actual).unwrap_or(actual);
                            if expected
                                .iter()
                                .any(|path| same_windows_executable(path, &actual))
                            {
                                // SAFETY: the handle still identifies the path checked above.
                                match unsafe { TerminateProcess(process.0, 1) } {
                                    Ok(()) => {
                                        // Termination is asynchronous. Do not race a new server
                                        // against the old process still holding ports or files.
                                        if unsafe { WaitForSingleObject(process.0, 5_000) }
                                            == WAIT_OBJECT_0
                                        {
                                            terminated += 1;
                                        } else {
                                            failures.push(format!(
                                                "owned process {pid} did not exit within 5 s"
                                            ));
                                        }
                                    }
                                    Err(error) => failures.push(format!(
                                        "could not terminate owned process {pid}: {error}"
                                    )),
                                }
                            }
                        }
                        Err(error) => {
                            eprintln!("agent: could not identify candidate process {pid}: {error}")
                        }
                    }
                }
                Err(error) => {
                    eprintln!("agent: could not open candidate process {pid}: {error}")
                }
            }
        }

        if unsafe { Process32NextW(snapshot.0, &mut entry) }.is_err() {
            break;
        }
    }
    if failures.is_empty() {
        Ok(terminated)
    } else {
        Err(failures.join("; "))
    }
}

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
        if let Err(error) = terminate_owned_processes(&self.root, &OWNED_IMAGES, None) {
            eprintln!("agent: orphan sweep failed: {error}");
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

    /// The GDI device name (`\\.\DISPLAYn`) and primary flag of the virtual
    /// display, if attached.
    fn find_display_info() -> Option<([u16; 32], bool)> {
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
            if wide_to_string(&device.DeviceString) == DISPLAY_DEVICE_STRING
                && device
                    .StateFlags
                    .contains(DISPLAY_DEVICE_ATTACHED_TO_DESKTOP)
            {
                return Some((
                    device.DeviceName,
                    device.StateFlags.contains(DISPLAY_DEVICE_PRIMARY_DEVICE),
                ));
            }
        }
        None
    }

    /// The GDI device name of the virtual display, if attached.
    fn find_display() -> Option<[u16; 32]> {
        Self::find_display_info().map(|(name, _)| name)
    }

    fn current_devmode(device_name: &[u16; 32]) -> Option<DEVMODEW> {
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
        Some(devmode)
    }

    fn current_mode(device_name: &[u16; 32]) -> Option<Mode> {
        let devmode = Self::current_devmode(device_name)?;
        Some(Mode {
            width: devmode.dmPelsWidth,
            height: devmode.dmPelsHeight,
            hz: devmode.dmDisplayFrequency,
        })
    }

    fn attached_displays() -> Result<Vec<AttachedDisplay>, String> {
        let mut displays = Vec::new();
        for index in 0..32u32 {
            let mut device = DISPLAY_DEVICEW {
                cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };
            let found =
                unsafe { EnumDisplayDevicesW(PCWSTR::null(), index, &mut device, 0) }.as_bool();
            if !found {
                break;
            }
            if !device
                .StateFlags
                .contains(DISPLAY_DEVICE_ATTACHED_TO_DESKTOP)
            {
                continue;
            }
            let mode = Self::current_devmode(&device.DeviceName).ok_or_else(|| {
                format!(
                    "EnumDisplaySettingsW failed for attached display {}",
                    wide_to_string(&device.DeviceName)
                )
            })?;
            displays.push(AttachedDisplay {
                name: device.DeviceName,
                mode,
                idd: wide_to_string(&device.DeviceString) == DISPLAY_DEVICE_STRING,
                primary: device.StateFlags.contains(DISPLAY_DEVICE_PRIMARY_DEVICE),
            });
        }
        if displays.is_empty() {
            Err("no attached desktop displays".to_owned())
        } else {
            Ok(displays)
        }
    }

    fn mode_request(mode: Mode) -> DEVMODEW {
        DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            dmFields: DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY,
            dmPelsWidth: mode.width,
            dmPelsHeight: mode.height,
            dmDisplayFrequency: mode.hz,
            ..Default::default()
        }
    }

    fn position(devmode: &DEVMODEW) -> POINTL {
        // SAFETY: every DEVMODEW passed here came from EnumDisplaySettingsW and
        // therefore has the display position arm selected.
        unsafe { devmode.Anonymous1.Anonymous2.dmPosition }
    }

    fn change_display(
        name: &[u16; 32],
        mode: &DEVMODEW,
        flags: windows::Win32::Graphics::Gdi::CDS_TYPE,
    ) -> Result<(), String> {
        let result = unsafe {
            ChangeDisplaySettingsExW(
                PCWSTR::from_raw(name.as_ptr()),
                Some(mode),
                None,
                flags,
                None,
            )
        };
        if result == DISP_CHANGE_SUCCESSFUL {
            Ok(())
        } else {
            Err(format!(
                "ChangeDisplaySettingsExW({}) returned {}",
                wide_to_string(name),
                result.0
            ))
        }
    }

    fn active_display_config(
    ) -> Result<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>), String> {
        let flags = QDC_ONLY_ACTIVE_PATHS | QDC_VIRTUAL_MODE_AWARE;
        for _ in 0..3 {
            let mut path_count = 0;
            let mut mode_count = 0;
            let sized =
                unsafe { GetDisplayConfigBufferSizes(flags, &mut path_count, &mut mode_count) };
            if sized != ERROR_SUCCESS {
                return Err(format!("GetDisplayConfigBufferSizes returned {}", sized.0));
            }
            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
            let queried = unsafe {
                QueryDisplayConfig(
                    flags,
                    &mut path_count,
                    paths.as_mut_ptr(),
                    &mut mode_count,
                    modes.as_mut_ptr(),
                    None,
                )
            };
            if queried == ERROR_INSUFFICIENT_BUFFER {
                continue;
            }
            if queried != ERROR_SUCCESS {
                return Err(format!("QueryDisplayConfig returned {}", queried.0));
            }
            paths.truncate(path_count as usize);
            modes.truncate(mode_count as usize);
            return Ok((paths, modes));
        }
        Err("display topology changed during three consecutive queries".to_owned())
    }

    fn display_config_source_name(path: &DISPLAYCONFIG_PATH_INFO) -> Result<String, String> {
        let mut packet = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size: std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                adapterId: path.sourceInfo.adapterId,
                id: path.sourceInfo.id,
            },
            ..Default::default()
        };
        let result = unsafe {
            DisplayConfigGetDeviceInfo(
                (&mut packet as *mut DISPLAYCONFIG_SOURCE_DEVICE_NAME)
                    .cast::<DISPLAYCONFIG_DEVICE_INFO_HEADER>(),
            )
        };
        if result == 0 {
            Ok(wide_to_string(&packet.viewGdiDeviceName))
        } else {
            Err(format!("DisplayConfigGetDeviceInfo returned {result}"))
        }
    }

    fn source_mode_index(
        path: &DISPLAYCONFIG_PATH_INFO,
        modes: &[DISPLAYCONFIG_MODE_INFO],
    ) -> Result<usize, String> {
        // With virtual-mode awareness, winuser.h packs sourceModeInfoIdx into
        // the high 16 bits after cloneGroupId. Legacy paths use the full index.
        let raw = unsafe {
            if path.flags & DISPLAYCONFIG_PATH_SUPPORT_VIRTUAL_MODE != 0 {
                path.sourceInfo.Anonymous.Anonymous._bitfield >> 16
            } else {
                path.sourceInfo.Anonymous.modeInfoIdx
            }
        };
        let index = usize::try_from(raw)
            .map_err(|_| format!("display source mode index {raw} is not addressable"))?;
        let info = modes
            .get(index)
            .ok_or_else(|| format!("display source mode index {raw} is out of range"))?;
        if info.infoType != DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE
            || info.adapterId != path.sourceInfo.adapterId
            || info.id != path.sourceInfo.id
        {
            return Err(format!(
                "display source mode index {raw} does not match its active path"
            ));
        }
        Ok(index)
    }

    fn source_mode(
        modes: &[DISPLAYCONFIG_MODE_INFO],
        index: usize,
    ) -> windows::Win32::Devices::Display::DISPLAYCONFIG_SOURCE_MODE {
        // SAFETY: source_mode_index verifies infoType before an index is retained.
        unsafe { modes[index].Anonymous.sourceMode }
    }

    fn set_source_position(modes: &mut [DISPLAYCONFIG_MODE_INFO], index: usize, position: POINTL) {
        let mut source = Self::source_mode(modes, index);
        source.position = position;
        modes[index].Anonymous.sourceMode = source;
    }

    fn submit_display_config(
        paths: &[DISPLAYCONFIG_PATH_INFO],
        modes: &[DISPLAYCONFIG_MODE_INFO],
        validate: bool,
    ) -> Result<(), String> {
        let mut flags = SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_VIRTUAL_MODE_AWARE;
        flags |= if validate {
            SDC_VALIDATE
        } else {
            SDC_APPLY | SDC_SAVE_TO_DATABASE
        };
        let result = unsafe { SetDisplayConfig(Some(paths), Some(modes), flags) };
        if result == 0 {
            Ok(())
        } else {
            let action = if validate { "validation" } else { "apply" };
            Err(format!("SetDisplayConfig {action} returned {result}"))
        }
    }

    fn same_path_identities(
        expected: &[DISPLAYCONFIG_PATH_INFO],
        actual: &[DISPLAYCONFIG_PATH_INFO],
    ) -> bool {
        expected.len() == actual.len()
            && expected.iter().zip(actual).all(|(wanted, found)| {
                wanted.sourceInfo.adapterId == found.sourceInfo.adapterId
                    && wanted.sourceInfo.id == found.sourceInfo.id
                    && wanted.targetInfo.adapterId == found.targetInfo.adapterId
                    && wanted.targetInfo.id == found.targetInfo.id
            })
    }

    fn same_mode(expected: &DISPLAYCONFIG_MODE_INFO, actual: &DISPLAYCONFIG_MODE_INFO) -> bool {
        if expected.infoType != actual.infoType
            || expected.adapterId != actual.adapterId
            || expected.id != actual.id
        {
            return false;
        }
        unsafe {
            if expected.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE {
                expected.Anonymous.sourceMode == actual.Anonymous.sourceMode
            } else if expected.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_TARGET {
                let wanted = expected.Anonymous.targetMode.targetVideoSignalInfo;
                let found = actual.Anonymous.targetMode.targetVideoSignalInfo;
                wanted.pixelRate == found.pixelRate
                    && wanted.hSyncFreq == found.hSyncFreq
                    && wanted.vSyncFreq == found.vSyncFreq
                    && wanted.activeSize == found.activeSize
                    && wanted.totalSize == found.totalSize
                    && wanted.Anonymous.videoStandard == found.Anonymous.videoStandard
                    && wanted.scanLineOrdering == found.scanLineOrdering
            } else if expected.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_DESKTOP_IMAGE {
                expected.Anonymous.desktopImageInfo == actual.Anonymous.desktopImageInfo
            } else {
                false
            }
        }
    }

    fn same_mode_set(
        expected: &[DISPLAYCONFIG_MODE_INFO],
        actual: &[DISPLAYCONFIG_MODE_INFO],
    ) -> bool {
        expected.len() == actual.len()
            && expected.iter().all(|wanted| {
                let mut matching = actual.iter().filter(|found| {
                    wanted.infoType == found.infoType
                        && wanted.adapterId == found.adapterId
                        && wanted.id == found.id
                });
                let found = matching.next();
                found.is_some_and(|found| Self::same_mode(wanted, found))
                    && matching.next().is_none()
            })
    }

    fn restore_idd_mode(name: &[u16; 32], original: Mode) -> Result<(), String> {
        let current_name = Self::find_display()
            .ok_or_else(|| "IDD disappeared before its mode could be restored".to_owned())?;
        if current_name != *name {
            return Err(format!(
                "IDD identity changed from {} to {}; stale mode restore refused",
                wide_to_string(name),
                wide_to_string(&current_name)
            ));
        }
        if Self::current_mode(name) == Some(original) {
            return Ok(());
        }
        Self::change_display(name, &Self::mode_request(original), CDS_UPDATEREGISTRY)?;
        let restored = Self::current_mode(name);
        if restored == Some(original) {
            Ok(())
        } else {
            Err(format!(
                "IDD mode restore returned success but verification found {restored:?}"
            ))
        }
    }

    fn mode_change_failure(name: &[u16; 32], original: Mode, error: String) -> String {
        match Self::restore_idd_mode(name, original) {
            Ok(()) => format!("{error}; original IDD mode restored"),
            Err(restore) => format!("{error}; restoring original IDD mode failed: {restore}"),
        }
    }

    fn same_display_config(
        expected_paths: &[DISPLAYCONFIG_PATH_INFO],
        expected_modes: &[DISPLAYCONFIG_MODE_INFO],
        actual_paths: &[DISPLAYCONFIG_PATH_INFO],
        actual_modes: &[DISPLAYCONFIG_MODE_INFO],
    ) -> Result<bool, String> {
        if !Self::same_path_identities(expected_paths, actual_paths)
            || !Self::same_mode_set(expected_modes, actual_modes)
        {
            return Ok(false);
        }
        for (wanted, found) in expected_paths.iter().zip(actual_paths) {
            if wanted.sourceInfo.statusFlags != found.sourceInfo.statusFlags
                || wanted.targetInfo.outputTechnology != found.targetInfo.outputTechnology
                || wanted.targetInfo.rotation != found.targetInfo.rotation
                || wanted.targetInfo.scaling != found.targetInfo.scaling
                || wanted.targetInfo.refreshRate != found.targetInfo.refreshRate
                || wanted.targetInfo.scanLineOrdering != found.targetInfo.scanLineOrdering
                || wanted.targetInfo.targetAvailable != found.targetInfo.targetAvailable
                || wanted.targetInfo.statusFlags != found.targetInfo.statusFlags
                || wanted.flags != found.flags
            {
                return Ok(false);
            }
            let wanted_index = Self::source_mode_index(wanted, expected_modes)?;
            let found_index = Self::source_mode_index(found, actual_modes)?;
            if !Self::same_mode(&expected_modes[wanted_index], &actual_modes[found_index]) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn restore_display_config(
        original_paths: &[DISPLAYCONFIG_PATH_INFO],
        original_modes: &[DISPLAYCONFIG_MODE_INFO],
    ) -> Result<(), String> {
        // Re-query immediately before rollback. Applying a stale CCD snapshot can
        // reconfigure a newly attached monitor, so identity drift is a hard stop.
        let (current_paths, _) = Self::active_display_config()?;
        if !Self::same_path_identities(original_paths, &current_paths) {
            return Err("active display paths changed; stale rollback refused".to_owned());
        }
        Self::submit_display_config(original_paths, original_modes, false)?;
        let (restored_paths, restored_modes) = Self::active_display_config()?;
        match Self::same_display_config(
            original_paths,
            original_modes,
            &restored_paths,
            &restored_modes,
        ) {
            Ok(true) => Ok(()),
            Ok(false) => Err(
                "rollback returned success but did not restore the original display configuration"
                    .to_owned(),
            ),
            Err(error) => Err(format!("rollback verification failed: {error}")),
        }
    }

    fn make_idd_primary(idd_name: &[u16; 32], requested: Mode) -> Result<(), TopologyError> {
        let (paths, mut modes) = Self::active_display_config().map_err(TopologyError::unchanged)?;
        let originals = modes.clone();
        let wanted_name = wide_to_string(idd_name);
        let mut idd_source = None;
        let mut other_sources = Vec::new();
        for path in &paths {
            let index = Self::source_mode_index(path, &modes).map_err(TopologyError::unchanged)?;
            let name = Self::display_config_source_name(path).map_err(TopologyError::unchanged)?;
            if name.eq_ignore_ascii_case(&wanted_name) {
                idd_source = Some(index);
            } else if !other_sources.contains(&index) {
                other_sources.push(index);
            }
        }
        let idd_source = idd_source.ok_or_else(|| {
            TopologyError::unchanged(format!(
                "active DisplayConfig paths do not contain {wanted_name}"
            ))
        })?;
        let idd_mode = Self::source_mode(&modes, idd_source);
        if idd_mode.width != requested.width || idd_mode.height != requested.height {
            return Err(TopologyError::unchanged(format!(
                "IDD DisplayConfig source is {}x{} after mode set; wanted {}x{}",
                idd_mode.width, idd_mode.height, requested.width, requested.height
            )));
        }

        Self::set_source_position(&mut modes, idd_source, POINTL { x: 0, y: 0 });
        let mut next_x = i32::try_from(requested.width).map_err(|_| {
            TopologyError::unchanged(
                "requested display width exceeds the Windows coordinate range".to_owned(),
            )
        })?;
        for index in other_sources {
            let source = Self::source_mode(&modes, index);
            let width = i32::try_from(source.width).map_err(|_| {
                TopologyError::unchanged(
                    "an active display width exceeds the coordinate range".to_owned(),
                )
            })?;
            Self::set_source_position(&mut modes, index, POINTL { x: next_x, y: 0 });
            next_x = next_x.checked_add(width).ok_or_else(|| {
                TopologyError::unchanged(
                    "the requested display layout exceeds the coordinate range".to_owned(),
                )
            })?;
        }

        Self::submit_display_config(&paths, &modes, true).map_err(TopologyError::unchanged)?;
        Self::submit_display_config(&paths, &modes, false).map_err(TopologyError::unchanged)?;

        let verified = Self::active_display_config().and_then(|(current_paths, current_modes)| {
            match Self::same_display_config(&paths, &modes, &current_paths, &current_modes)? {
                true => Self::attached_displays()
                    .and_then(|after| Self::verify_layout(&after, requested)),
                false => Err(
                    "SetDisplayConfig returned success but changed unsupplied display state"
                        .to_owned(),
                ),
            }
        });
        if let Err(error) = verified {
            return match Self::restore_display_config(&paths, &originals) {
                Ok(()) => Err(TopologyError::unchanged(format!(
                    "display topology applied but {error}; original layout restored"
                ))),
                Err(rollback) => Err(TopologyError::uncertain(format!(
                    "display topology applied but {error}; restoring original layout failed: {rollback}"
                ))),
            };
        }
        Ok(())
    }

    fn verify_layout(displays: &[AttachedDisplay], requested: Mode) -> Result<(), String> {
        let idd = displays
            .iter()
            .find(|display| display.idd)
            .ok_or_else(|| "IDD is no longer attached after display commit".to_owned())?;
        let idd_position = Self::position(&idd.mode);
        if idd.mode.dmPelsWidth != requested.width
            || idd.mode.dmPelsHeight != requested.height
            || idd.mode.dmDisplayFrequency != requested.hz
            || idd_position.x != 0
            || idd_position.y != 0
            || !idd.primary
        {
            return Err(format!(
                "IDD verification failed: {}x{} @ {} Hz at ({}, {}), primary={}",
                idd.mode.dmPelsWidth,
                idd.mode.dmPelsHeight,
                idd.mode.dmDisplayFrequency,
                idd_position.x,
                idd_position.y,
                idd.primary
            ));
        }

        let mut expected_x = i32::try_from(requested.width).map_err(|_| {
            "requested display width exceeds the Windows coordinate range".to_owned()
        })?;
        let mut others: Vec<_> = displays.iter().filter(|display| !display.idd).collect();
        others.sort_by_key(|display| Self::position(&display.mode).x);
        for display in others {
            let position = Self::position(&display.mode);
            if position.x != expected_x || position.y != 0 || display.primary {
                return Err(format!(
                    "display {} verification failed: at ({}, {}), primary={}; wanted ({}, 0)",
                    wide_to_string(&display.name),
                    position.x,
                    position.y,
                    display.primary,
                    expected_x
                ));
            }
            let width = i32::try_from(display.mode.dmPelsWidth).map_err(|_| {
                format!(
                    "display {} width exceeds the Windows coordinate range",
                    wide_to_string(&display.name)
                )
            })?;
            expected_x = expected_x.checked_add(width).ok_or_else(|| {
                "the staged display layout exceeds the Windows coordinate range".to_owned()
            })?;
        }
        Ok(())
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

    fn display_placement(&mut self) -> Option<DisplayPlacement> {
        let (name, primary) = Self::find_display_info()?;
        let mode = Self::current_devmode(&name)?;
        let position = Self::position(&mode);
        Some(DisplayPlacement {
            primary,
            origin: (position.x, position.y),
        })
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
        let name = Self::find_display().ok_or_else(|| "IDD display not attached".to_owned())?;
        let original = Self::current_mode(&name)
            .ok_or_else(|| "could not read the IDD's current display mode".to_owned())?;
        let mode_changed = original != mode;
        if mode_changed {
            let request = Self::mode_request(mode);
            if let Err(error) = Self::change_display(&name, &request, CDS_UPDATEREGISTRY) {
                return Err(Self::mode_change_failure(&name, original, error));
            }
            let applied = Self::current_mode(&name);
            if applied != Some(mode) {
                return Err(Self::mode_change_failure(
                    &name,
                    original,
                    format!(
                        "IDD mode verification failed after ChangeDisplaySettingsExW: {applied:?}"
                    ),
                ));
            }
        }
        match Self::make_idd_primary(&name, mode) {
            Ok(()) => Ok(()),
            Err(error) if mode_changed && error.mode_restore_safe => Err(
                Self::mode_change_failure(&name, original, error.detail),
            ),
            Err(error) if mode_changed => Err(format!(
                "{}; original IDD mode was not restored because display identity or rollback state is uncertain",
                error.detail
            )),
            Err(error) => Err(error.detail),
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
