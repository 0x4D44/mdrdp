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

use windows::core::PCWSTR;
use windows::Win32::Graphics::Gdi::{
    ChangeDisplaySettingsExW, EnumDisplayDevicesW, EnumDisplaySettingsW, CDS_UPDATEREGISTRY,
    DEVMODEW, DISPLAY_DEVICEW, DISP_CHANGE_SUCCESSFUL, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT,
    DM_PELSWIDTH, ENUM_CURRENT_SETTINGS,
};

use super::wide_to_string;
use crate::agent::{AgentOps, ChildState, Mode};

/// The device string the IDD driver's INF declares — the same one the rig's
/// scripts key on.
pub const DISPLAY_DEVICE_STRING: &str = "mdrdp latency-spike display";

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

    fn device_present(&mut self) -> bool {
        Self::find_display().is_some()
    }

    fn display_mode(&mut self) -> Option<Mode> {
        Self::find_display().and_then(|name| Self::current_mode(&name))
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

    fn poll_server(&mut self) -> ChildState {
        Self::poll(&mut self.server)
    }

    fn spawn_server(&mut self) -> Result<(), String> {
        // IDD source: bound to our display by the shared section's well-known
        // name, so no output-index guessing. Stats land beside the logs.
        let stats = self.root.join("logs").join("server-stats.jsonl");
        let stats = stats.to_string_lossy().into_owned();
        self.server = Some(self.spawn(
            "rhydra-server.exe",
            &["--source", "idd", "--out", &stats],
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

/// Where the agent's exes and logs live: the directory of the running exe.
/// No environment variable, no config file — deploy puts everything in one place.
pub fn exe_root() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "agent exe has no parent directory".to_owned())
}
