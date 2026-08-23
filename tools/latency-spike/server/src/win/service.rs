//! Windows service host for the console-session Rhydra agent.
//!
//! The service keeps its LocalSystem token in session 0. It duplicates that
//! token into the active console session and starts the ordinary `run` worker
//! on `winsta0\\default`, including while Winlogon owns the input desktop.

use std::ffi::c_void;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Security::{
    DuplicateTokenEx, SecurityImpersonation, SetTokenInformation, TokenPrimary, TokenSessionId,
    TOKEN_ADJUST_SESSIONID, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_QUERY,
};
use windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SetServiceStatus, StartServiceCtrlDispatcherW,
    SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP,
    SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE,
    SERVICE_STATUS_HANDLE, SERVICE_STOPPED, SERVICE_STOP_PENDING, SERVICE_TABLE_ENTRYW,
    SERVICE_WIN32_OWN_PROCESS,
};
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess, OpenProcessToken,
    TerminateProcess, WaitForSingleObject, CREATE_NO_WINDOW, PROCESS_INFORMATION, STARTUPINFOW,
};

use crate::control::CONTROL_PORT;

pub const SERVICE_NAME: &str = "RhydraAgent";
const NO_CONSOLE_SESSION: u32 = u32::MAX;
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const GRACEFUL_STOP: Duration = Duration::from_secs(15);

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);
static CHILD_ARGUMENTS: OnceLock<Vec<String>> = OnceLock::new();
static STATUS_HANDLE: AtomicUsize = AtomicUsize::new(0);

struct Child {
    process: HANDLE,
    session_id: u32,
}

impl Drop for Child {
    fn drop(&mut self) {
        // SAFETY: the process handle is uniquely owned by this value.
        let _ = unsafe { CloseHandle(self.process) };
    }
}

fn log(message: &str) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(root) = exe.parent() else { return };
    let logs = root.join("logs");
    let _ = std::fs::create_dir_all(&logs);
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs.join("service.log"))
    else {
        return;
    };
    let unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0);
    let _ = writeln!(file, "[{unix}] {message}");
}

fn set_status(state: SERVICE_STATUS_CURRENT_STATE, accepted: u32, checkpoint: u32, wait_hint: u32) {
    let raw_handle = STATUS_HANDLE.load(Ordering::Acquire);
    if raw_handle == 0 {
        return;
    }
    let handle = SERVICE_STATUS_HANDLE(raw_handle as *mut c_void);
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: accepted,
        dwWin32ExitCode: 0,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: checkpoint,
        dwWaitHint: wait_hint,
    };
    // SAFETY: the handle was returned for this service and status is live.
    let _ = unsafe { SetServiceStatus(handle, &status) };
}

unsafe extern "system" fn control_handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    if control == SERVICE_CONTROL_STOP || control == SERVICE_CONTROL_SHUTDOWN {
        STOP_REQUESTED.store(true, Ordering::Release);
        set_status(SERVICE_STOP_PENDING, 0, 1, GRACEFUL_STOP.as_millis() as u32);
    }
    0
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    let handle = match RegisterServiceCtrlHandlerExW(w!("RhydraAgent"), Some(control_handler), None)
    {
        Ok(value) => value,
        Err(error) => {
            log(&format!("RegisterServiceCtrlHandlerExW failed: {error}"));
            return;
        }
    };
    STATUS_HANDLE.store(handle.0 as usize, Ordering::Release);
    set_status(SERVICE_START_PENDING, 0, 1, 10_000);
    let result = supervise();
    if let Err(error) = result {
        log(&format!("service failed: {error}"));
    }
    set_status(SERVICE_STOPPED, 0, 0, 0);
}

/// Enter the Windows service dispatcher. `child_arguments` are appended after
/// `run` when the console-session worker is launched.
pub fn run(child_arguments: Vec<String>) -> Result<(), String> {
    CHILD_ARGUMENTS
        .set(child_arguments)
        .map_err(|_| "service arguments were already set".to_owned())?;
    STOP_REQUESTED.store(false, Ordering::Release);
    let mut service_name: Vec<u16> = SERVICE_NAME.encode_utf16().chain(Some(0)).collect();
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(service_name.as_mut_ptr()),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    // SAFETY: the table stays alive until the dispatcher and service main exit.
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) }
        .map_err(|error| format!("StartServiceCtrlDispatcherW failed: {error}"))
}

fn supervise() -> Result<(), String> {
    set_status(
        SERVICE_RUNNING,
        SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN,
        0,
        0,
    );
    log("service running");
    let mut child: Option<Child> = None;
    let mut next_launch = std::time::Instant::now();
    while !STOP_REQUESTED.load(Ordering::Acquire) {
        // SAFETY: returns an integer session id and has no preconditions.
        let session_id = unsafe { WTSGetActiveConsoleSessionId() };
        let child_alive = child.as_ref().is_some_and(is_alive);
        let wrong_session = child
            .as_ref()
            .is_some_and(|running| running.session_id != session_id);
        if !child_alive || wrong_session || session_id == NO_CONSOLE_SESSION {
            if let Some(running) = child.take() {
                stop_child(running);
                sweep_remaining_children();
            }
            if session_id != NO_CONSOLE_SESSION && std::time::Instant::now() >= next_launch {
                match launch_worker(session_id) {
                    Ok(running) => {
                        log(&format!("started console worker in session {session_id}"));
                        child = Some(running);
                    }
                    Err(error) => log(&format!(
                        "could not start console worker in session {session_id}: {error}"
                    )),
                }
                // A broken image or missing privilege must not turn into a
                // tight LocalSystem process-launch loop.
                next_launch = std::time::Instant::now() + Duration::from_secs(1);
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    if let Some(running) = child.take() {
        stop_child(running);
        sweep_remaining_children();
    }
    log("service stopped");
    Ok(())
}

fn is_alive(child: &Child) -> bool {
    let mut exit_code = 0u32;
    // SAFETY: `child.process` remains valid while `child` is borrowed.
    unsafe { GetExitCodeProcess(child.process, &mut exit_code) }.is_ok() && exit_code == 259
}

fn launch_worker(session_id: u32) -> Result<Child, String> {
    let exe = std::env::current_exe().map_err(|error| format!("current_exe failed: {error}"))?;
    let mut service_token = HANDLE::default();
    // SAFETY: output handle points to valid storage and is closed below.
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY,
            &mut service_token,
        )
    }
    .map_err(|error| format!("OpenProcessToken failed: {error}"))?;
    let mut console_token = HANDLE::default();
    let duplicate = unsafe {
        DuplicateTokenEx(
            service_token,
            TOKEN_ASSIGN_PRIMARY | TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ADJUST_SESSIONID,
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut console_token,
        )
    };
    let _ = unsafe { CloseHandle(service_token) };
    duplicate.map_err(|error| format!("DuplicateTokenEx failed: {error}"))?;

    let result = (|| {
        let session_ptr = (&session_id as *const u32).cast::<c_void>();
        // SAFETY: the duplicated primary token is mutable and the value is a u32.
        unsafe {
            SetTokenInformation(
                console_token,
                TokenSessionId,
                session_ptr,
                std::mem::size_of::<u32>() as u32,
            )
        }
        .map_err(|error| format!("SetTokenInformation(TokenSessionId) failed: {error}"))?;

        let mut command = format!("\"{}\" run", exe.display());
        for argument in CHILD_ARGUMENTS.get().into_iter().flatten() {
            command.push(' ');
            command.push_str(argument);
        }
        let mut command_wide: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
        let application_wide: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut desktop: Vec<u16> = "winsta0\\default".encode_utf16().chain(Some(0)).collect();
        let startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let mut process = PROCESS_INFORMATION::default();
        // SAFETY: all strings are NUL-terminated and live for the call; handles
        // returned in `process` become owned by this function.
        unsafe {
            CreateProcessAsUserW(
                Some(console_token),
                PCWSTR(application_wide.as_ptr()),
                Some(PWSTR(command_wide.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_NO_WINDOW,
                None,
                None,
                &startup,
                &mut process,
            )
        }
        .map_err(|error| format!("CreateProcessAsUserW failed: {error}"))?;
        let _ = unsafe { CloseHandle(process.hThread) };
        Ok(Child {
            process: process.hProcess,
            session_id,
        })
    })();
    let _ = unsafe { CloseHandle(console_token) };
    result
}

fn stop_child(child: Child) {
    if let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", CONTROL_PORT)) {
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let _ = writeln!(stream, "{{\"cmd\":\"shutdown\"}}");
    }
    let deadline = std::time::Instant::now() + GRACEFUL_STOP;
    while std::time::Instant::now() < deadline {
        // SAFETY: process handle remains owned by `child` until this function exits.
        if unsafe { WaitForSingleObject(child.process, 250) } == WAIT_OBJECT_0 {
            return;
        }
    }
    log(&format!(
        "console worker in session {} did not stop gracefully; terminating it",
        child.session_id
    ));
    // SAFETY: termination is the bounded last resort for the child this service created.
    let _ = unsafe { TerminateProcess(child.process, 1) };
    let _ = unsafe { WaitForSingleObject(child.process, 5_000) };
}

fn sweep_remaining_children() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(root) = exe.parent() else { return };
    if let Err(error) = crate::win::agent_ops::terminate_owned_processes(
        root,
        &crate::win::agent_ops::OWNED_IMAGES,
        Some(std::process::id()),
    ) {
        log(&format!(
            "could not sweep remaining worker children: {error}"
        ));
    }
}
