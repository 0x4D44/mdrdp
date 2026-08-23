//! `rhydra-agent` — the Windows service and console-session agent.
//!
//! `service` runs as LocalSystem and launches `run` in the active console
//! session. `run` sweeps orphans, then reconciles the
//! stack (IDD creator → device → 240 Hz mode → capture server) every tick,
//! serving status and restart/shutdown commands on the loopback control port.
//! `install` hardens the deployment ACL, registers the service, removes the old
//! on-logon task, and starts it. `uninstall` removes both launch paths.

use std::process::ExitCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DisplayArgs {
    width: u32,
    height: u32,
    hz: u32,
    scale: u32,
}

/// Parse the optional display tuple accepted by `run` and `install`.
///
/// The finite mode and scale validation belongs to `Reconciler`; this parser
/// only turns the portable command-line representation into numbers so the
/// Windows and non-Windows test builds share the same argument contract.
fn parse_display(args: &[String]) -> Result<Option<DisplayArgs>, String> {
    match args {
        [] => Ok(None),
        [flag, width, height, hz, scale] if flag == "--display" => {
            let parse = |name: &str, value: &str| {
                value
                    .parse::<u32>()
                    .map_err(|_| format!("--display {name} needs a whole number, got {value:?}"))
            };
            Ok(Some(DisplayArgs {
                width: parse("<width>", width)?,
                height: parse("<height>", height)?,
                hz: parse("<hz>", hz)?,
                scale: parse("<scale>", scale)?,
            }))
        }
        _ => Err(format!(
            "expected --display <width> <height> <hz> <scale>, got {args:?}"
        )),
    }
}

fn format_display_args(display: Option<DisplayArgs>) -> String {
    display.map_or_else(String::new, |display| {
        format!(
            " --display {} {} {} {}",
            display.width, display.height, display.hz, display.scale
        )
    })
}

fn windows_service_command(exe: &std::path::Path, display: Option<DisplayArgs>) -> String {
    format!(
        "\"{}\" service{}",
        exe.display(),
        format_display_args(display)
    )
}

fn windows_acl_commands(root: &str) -> Vec<Vec<String>> {
    let descendants = format!("{}\\*", root.trim_end_matches(['\\', '/']));
    vec![
        vec![
            root.to_owned(),
            "/setowner".to_owned(),
            "*S-1-5-32-544".to_owned(),
            "/T".to_owned(),
            "/Q".to_owned(),
        ],
        vec![root.to_owned(), "/reset".to_owned(), "/Q".to_owned()],
        vec![
            root.to_owned(),
            "/inheritance:r".to_owned(),
            "/grant:r".to_owned(),
            "*S-1-5-18:(OI)(CI)F".to_owned(),
            "*S-1-5-32-544:(OI)(CI)F".to_owned(),
            "/Q".to_owned(),
        ],
        vec![
            descendants,
            "/reset".to_owned(),
            "/T".to_owned(),
            "/Q".to_owned(),
        ],
    ]
}

#[cfg(windows)]
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => match parse_display(&args[1..]) {
            Ok(display) => win::run(display),
            Err(e) => {
                eprintln!("agent: {e}");
                ExitCode::from(2)
            }
        },
        Some("service") => match parse_display(&args[1..]) {
            Ok(display) => win::service(display),
            Err(e) => {
                eprintln!("agent: {e}");
                ExitCode::from(2)
            }
        },
        Some("install") => match parse_display(&args[1..]) {
            Ok(display) => win::install(display),
            Err(e) => {
                eprintln!("agent: {e}");
                ExitCode::from(2)
            }
        },
        Some("uninstall") => win::uninstall(),
        Some("configure-audio") => win::configure_audio(),
        Some("check-audio") => win::check_audio(),
        Some("status") => match parse_wait(&args[1..]) {
            Ok(wait) => win::status(wait),
            Err(e) => {
                eprintln!("agent: {e}");
                ExitCode::from(2)
            }
        },
        _ => {
            eprintln!(
                "usage: rhydra-agent run|service|install [--display <width> <height> <hz> <scale>]\n       rhydra-agent uninstall|configure-audio|check-audio|status [--wait <secs>]"
            );
            ExitCode::from(2)
        }
    }
}

/// Parse `status`'s optional `--wait <secs>`.
#[cfg(windows)]
fn parse_wait(args: &[String]) -> Result<Option<u64>, String> {
    match args {
        [] => Ok(None),
        [flag, secs] if flag == "--wait" => secs
            .parse::<u64>()
            .map(Some)
            .map_err(|_| format!("--wait needs whole seconds, got {secs:?}")),
        _ => Err(format!("unrecognised status arguments: {args:?}")),
    }
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("rhydra-agent: windows only");
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::{
        format_display_args, parse_display, windows_acl_commands, windows_service_command,
        DisplayArgs,
    };

    #[test]
    fn parse_display_preserves_the_native_mode_tuple() {
        let args = [
            "--display".to_owned(),
            "5120".to_owned(),
            "2880".to_owned(),
            "240".to_owned(),
            "200".to_owned(),
        ];

        assert_eq!(
            parse_display(&args),
            Ok(Some(DisplayArgs {
                width: 5120,
                height: 2880,
                hz: 240,
                scale: 200,
            }))
        );
    }

    #[test]
    fn format_display_args_is_suitable_for_the_worker_command() {
        assert_eq!(
            format_display_args(Some(DisplayArgs {
                width: 5120,
                height: 2880,
                hz: 240,
                scale: 200,
            })),
            " --display 5120 2880 240 200"
        );
        assert_eq!(format_display_args(None), "");
    }

    #[test]
    fn service_command_quotes_the_exact_versioned_executable() {
        assert_eq!(
            windows_service_command(
                std::path::Path::new(r"C:\Program Files\mdrdp\v0.5.0\rhydra-agent.exe"),
                Some(DisplayArgs {
                    width: 5120,
                    height: 2880,
                    hz: 240,
                    scale: 200,
                }),
            ),
            r#""C:\Program Files\mdrdp\v0.5.0\rhydra-agent.exe" service --display 5120 2880 240 200"#
        );
    }

    #[test]
    fn acl_plan_protects_the_root_then_reenables_descendant_inheritance() {
        let commands = windows_acl_commands(r"C:\mdrdp");
        assert_eq!(commands.len(), 4);
        assert_eq!(commands[1], vec![r"C:\mdrdp", "/reset", "/Q"]);
        assert_eq!(
            commands[2],
            vec![
                r"C:\mdrdp",
                "/inheritance:r",
                "/grant:r",
                "*S-1-5-18:(OI)(CI)F",
                "*S-1-5-32-544:(OI)(CI)F",
                "/Q",
            ]
        );
        assert_eq!(commands[3], vec![r"C:\mdrdp\*", "/reset", "/T", "/Q"]);
    }
}

#[cfg(windows)]
mod win {
    use super::{windows_acl_commands, windows_service_command, DisplayArgs};
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::process::{Command, ExitCode};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use rhydra::agent::{Reconciler, TICK_SECS};
    use rhydra::control::{self, Request, CONTROL_PORT};
    use rhydra::win::agent_ops::{exe_root, terminate_owned_processes, WinOps, OWNED_IMAGES};

    const LEGACY_TASK_NAME: &str = "rhydra-agent";
    const SERVICE_NAME: &str = rhydra::win::service::SERVICE_NAME;

    /// What the control thread hands the reconcile loop, and vice versa.
    struct Shared {
        status_line: String,
        restart_requested: bool,
        shutdown_requested: bool,
        /// The token `cycle-device` must echo, refreshed with the status each
        /// tick. Kept beside the serialised line rather than re-derived, so the
        /// guard and the status a caller read can never disagree.
        cycle_challenge: String,
        cycle_requested: bool,
        display_request: Option<(rhydra::agent::Mode, u32)>,
    }

    /// Read the console session's clipboard.
    ///
    /// Windows-only by nature: the whole point is the window station this
    /// process sits in, and there is no such thing anywhere else. The
    /// non-Windows arm exists so the binary still type-checks on the machine it
    /// is cross-built from, and says so rather than pretending to succeed.
    #[cfg(windows)]
    fn read_console_clipboard() -> Result<Option<String>, String> {
        use rhydra::clipboard::TextClipboard;
        // A fresh owner per call: this is asked rarely and on demand, so there
        // is no sequence-number cache worth keeping, and a fresh one cannot
        // serve a stale answer to an assertion.
        rhydra::win::clipboard::ClipboardOwner::new().read_text()
    }

    #[cfg(not(windows))]
    fn read_console_clipboard() -> Result<Option<String>, String> {
        Err("clipboard reads need Windows".to_owned())
    }

    /// One timestamped line to the agent log and stderr. The log is the record;
    /// stderr shows up when run by hand.
    fn log(file: &mut std::fs::File, message: &str) {
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = format!("[{unix}] {message}");
        eprintln!("{line}");
        let _ = writeln!(file, "{line}");
    }

    /// Put the reconcile thread on the live console desktop and in one physical
    /// coordinate space before it creates either child process.
    ///
    /// A worker launched while Windows is locked starts on `winsta0\\default` by
    /// service convention. If its reconcile thread stays there, the children inherit
    /// that hidden desktop and the new IDD publishes black frames. Joining Winlogon
    /// first makes a cold service start behave like the already-working transition
    /// from an unlocked desktop into Winlogon.
    ///
    /// `EnumDisplaySettingsW` reports physical geometry, while `MonitorFromPoint`
    /// and `GetDpiForWindow` otherwise see a DPI-virtualised desktop and
    /// can miss a 200%-scaled secondary display.
    fn initialize_reconcile_thread() -> rhydra::win::Result<String> {
        let desktop = rhydra::win::input::sync_thread_to_input_desktop()?;
        rhydra::win::init_thread_dpi_awareness()?;
        Ok(desktop)
    }

    fn apply_display_request(
        rec: &mut Reconciler,
        display: Option<DisplayArgs>,
    ) -> Result<(), String> {
        if let Some(display) = display {
            let mode = rhydra::agent::Mode {
                width: display.width,
                height: display.height,
                hz: display.hz,
            };
            rec.request_display_mode(mode, display.scale)?;
        }
        Ok(())
    }

    pub fn run(display: Option<DisplayArgs>) -> ExitCode {
        let mut reconcile_desktop = match initialize_reconcile_thread() {
            Ok(desktop) => desktop,
            Err(error) => {
                eprintln!("agent: interactive thread initialisation failed: {error}");
                return ExitCode::FAILURE;
            }
        };
        let root = match exe_root() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("agent: {e}");
                return ExitCode::FAILURE;
            }
        };
        let logs = root.join("logs");
        if let Err(e) = std::fs::create_dir_all(&logs) {
            eprintln!("agent: create {}: {e}", logs.display());
            return ExitCode::FAILURE;
        }
        let mut log_file = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(logs.join("agent.log"))
        {
            Ok(f) => f,
            Err(e) => {
                eprintln!("agent: open agent.log: {e}");
                return ExitCode::FAILURE;
            }
        };
        log(
            &mut log_file,
            &format!("agent starting in {}", root.display()),
        );
        log(
            &mut log_file,
            &format!("reconcile thread desktop {reconcile_desktop:?}"),
        );

        let mut ops = WinOps::new(root);
        ops.sweep_orphans();

        let mut rec = Reconciler::new();
        if let Err(error) = apply_display_request(&mut rec, display) {
            eprintln!("agent: invalid display request: {error}");
            return ExitCode::from(2);
        }
        // Seed before the first tick so a replacement agent never spends a
        // reconcile interval advertising the safe 1440p/100% defaults.
        let first = rec.status(0);
        let shared = Arc::new(Mutex::new(Shared {
            status_line: control::status_line(&first),
            restart_requested: false,
            shutdown_requested: false,
            cycle_challenge: first.cycle_challenge(),
            cycle_requested: false,
            display_request: None,
        }));

        let listener = match TcpListener::bind(("127.0.0.1", CONTROL_PORT)) {
            Ok(l) => l,
            Err(e) => {
                // A second agent is the usual cause; single-owner semantics say die.
                log(&mut log_file, &format!("control bind failed: {e}"));
                return ExitCode::FAILURE;
            }
        };
        log(
            &mut log_file,
            &format!("control listening on 127.0.0.1:{CONTROL_PORT}"),
        );
        {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || serve_control(listener, &shared));
        }

        let started = Instant::now();
        let mut last_stuck: Option<String> = None;
        loop {
            {
                let mut s = shared
                    .lock()
                    .expect("control thread cannot panic holding this");
                if s.shutdown_requested {
                    drop(s);
                    log(&mut log_file, "shutdown requested; killing children");
                    // WinOps::drop kills the children; explicit for the log's sake.
                    break;
                }
                if s.restart_requested {
                    s.restart_requested = false;
                    rec.request_server_restart();
                }
                if s.cycle_requested {
                    s.cycle_requested = false;
                    rec.request_device_cycle();
                }
                if let Some((mode, scale_percent)) = s.display_request.take() {
                    if let Err(e) = rec.request_display_mode(mode, scale_percent) {
                        log(&mut log_file, &format!("display request rejected: {e}"));
                    }
                }
            }

            // Follow lock, unlock and UAC desktop transitions before a reconcile
            // pass can replace either child. Existing children keep running while
            // a transient desktop query fails; starting a replacement on a stale
            // desktop would be worse than delaying the pass by one tick.
            match rhydra::win::input::sync_thread_to_input_desktop() {
                Ok(desktop) => {
                    if desktop != reconcile_desktop {
                        log(
                            &mut log_file,
                            &format!("reconcile thread desktop {desktop:?}"),
                        );
                        reconcile_desktop = desktop;
                    }
                }
                Err(error) => {
                    log(
                        &mut log_file,
                        &format!("input desktop unavailable; delaying reconcile: {error}"),
                    );
                    std::thread::sleep(Duration::from_secs(u64::from(TICK_SECS)));
                    continue;
                }
            }

            rec.tick(&mut ops);
            let report = rec.status(started.elapsed().as_secs());
            if report.stuck != last_stuck {
                match &report.stuck {
                    Some(step) => log(&mut log_file, &format!("waiting on: {step}")),
                    None => log(&mut log_file, "stack green"),
                }
                last_stuck = report.stuck.clone();
            }
            {
                let mut shared = shared.lock().expect("not poisoned");
                shared.status_line = control::status_line(&report);
                shared.cycle_challenge = report.cycle_challenge();
            }

            std::thread::sleep(Duration::from_secs(u64::from(TICK_SECS)));
        }
        drop(ops);
        log(&mut log_file, "agent exiting");
        ExitCode::SUCCESS
    }

    pub fn service(display: Option<DisplayArgs>) -> ExitCode {
        let mut arguments = Vec::new();
        if let Some(display) = display {
            arguments.extend([
                "--display".to_owned(),
                display.width.to_string(),
                display.height.to_string(),
                display.hz.to_string(),
                display.scale.to_string(),
            ]);
        }
        match rhydra::win::service::run(arguments) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("agent: service failed: {error}");
                ExitCode::FAILURE
            }
        }
    }

    /// One client at a time, short read timeout: a silent client cannot wedge us.
    fn serve_control(listener: TcpListener, shared: &Mutex<Shared>) {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let _ = serve_one(stream, shared);
            if shared.lock().expect("not poisoned").shutdown_requested {
                return; // Stop accepting; the main loop is on its way out.
            }
        }
    }

    fn serve_one(stream: TcpStream, shared: &Mutex<Shared>) -> std::io::Result<()> {
        let mut writer = stream.try_clone()?;
        let mut reader = BufReader::new(stream);
        while let Some(line) = control::read_request_line(&mut reader)? {
            if line.trim().is_empty() {
                continue;
            }
            let reply = match control::parse_request(&line) {
                Err(e) => control::error_line(&e),
                Ok(Request::Status) => shared.lock().expect("not poisoned").status_line.clone(),
                Ok(Request::RestartServer) => {
                    shared.lock().expect("not poisoned").restart_requested = true;
                    control::ok_line()
                }
                Ok(Request::Shutdown) => {
                    shared.lock().expect("not poisoned").shutdown_requested = true;
                    let _ = writeln!(writer, "{}", control::ok_line());
                    return Ok(());
                }
                // The guard runs here, in the agent, because loopback is not an
                // authorisation boundary: the probe forwards this port on every
                // Auto connect, and anything on the box can reach it. A caller
                // must prove it read current status, which also makes a blind
                // retry after a read timeout safe (the token a completed cycle
                // would have invalidated no longer matches).
                Ok(Request::CycleDevice { confirm }) => {
                    let expected = {
                        let shared = shared.lock().expect("not poisoned");
                        shared.cycle_challenge.clone()
                    };
                    if confirm != expected {
                        eprintln!(
                            "control: refused cycle-device (confirmation {:?} does not match \
                             current state)",
                            confirm
                        );
                        control::error_line(
                            "cycle-device needs the confirmation token from the current status: \
                             it recreates the display and drops every session on this host",
                        )
                    } else {
                        eprintln!(
                            "control: cycle-device confirmed — tearing the stack down and \
                             rebuilding the device"
                        );
                        shared.lock().expect("not poisoned").cycle_requested = true;
                        control::ok_line()
                    }
                }
                Ok(Request::ClipboardMatches { expected }) => {
                    // Answered on this thread rather than routed through the
                    // reconcile loop: the clipboard belongs to the *process's*
                    // window station, and this process is the one in the
                    // console session, so any of its threads can read it.
                    match read_console_clipboard() {
                        Ok(actual) => {
                            let (matches, actual_bytes, expected_bytes) =
                                control::clipboard_verdict(actual.as_deref(), &expected);
                            control::clipboard_match_line(matches, actual_bytes, expected_bytes)
                        }
                        // Never the clipboard's content, and never the caller's
                        // expectation either — this line goes to a log file.
                        Err(e) => {
                            control::error_line(&format!("could not read the clipboard: {e}"))
                        }
                    }
                }
                Ok(Request::PrepareDisplay {
                    width,
                    height,
                    hz,
                    scale_percent,
                }) => {
                    let mode = rhydra::agent::Mode { width, height, hz };
                    // Validate before acknowledging; the real reconciler receives
                    // the same already-checked value on its next tick.
                    let mut validator = Reconciler::new();
                    match validator.request_display_mode(mode, scale_percent) {
                        Ok(()) => {
                            shared.lock().expect("not poisoned").display_request =
                                Some((mode, scale_percent));
                            control::ok_line()
                        }
                        Err(e) => control::error_line(&e),
                    }
                }
                Ok(Request::ConfigureAudio) => match rhydra::win::audio_policy::configure_audio() {
                    Ok(detail) => {
                        eprintln!("control: {detail}");
                        control::ok_line()
                    }
                    Err(error) => control::error_line(&error),
                },
                Ok(Request::CheckAudio) => match rhydra::win::audio_policy::check_audio() {
                    Ok(detail) => {
                        eprintln!("control: {detail}");
                        control::ok_line()
                    }
                    Err(error) => control::error_line(&error),
                },
            };
            writeln!(writer, "{reply}")?;
        }
        Ok(())
    }

    pub fn install(display: Option<DisplayArgs>) -> ExitCode {
        let mut validator = Reconciler::new();
        if let Err(error) = apply_display_request(&mut validator, display) {
            eprintln!("agent: invalid display request: {error}");
            return ExitCode::from(2);
        }
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("agent: current_exe: {e}");
                return ExitCode::FAILURE;
            }
        };
        let root = match installation_root(&exe) {
            Ok(root) => root,
            Err(error) => {
                eprintln!("agent: cannot identify installation root: {error}");
                return ExitCode::FAILURE;
            }
        };
        if let Err(error) = harden_installation_acl(&root) {
            eprintln!("agent: refusing to register a SYSTEM service: {error}");
            return ExitCode::FAILURE;
        }
        let command = windows_service_command(&exe, display);
        let existing_service = service_registered();
        if existing_service {
            let _ = Command::new("sc.exe").args(["stop", SERVICE_NAME]).status();
            if !wait_for_service_stopped(Duration::from_secs(20)) {
                eprintln!("agent: service {SERVICE_NAME} did not stop within 20 s");
                return ExitCode::FAILURE;
            }
        } else if let Err(error) = request_worker_shutdown() {
            eprintln!("agent: legacy worker shutdown failed: {error}");
            return ExitCode::FAILURE;
        }
        if let Err(error) = delete_legacy_task() {
            eprintln!("agent: {error}");
            return ExitCode::FAILURE;
        }

        if existing_service {
            let configured = Command::new("sc.exe")
                .args([
                    "config",
                    SERVICE_NAME,
                    "binPath=",
                    &command,
                    "start=",
                    "auto",
                    "obj=",
                    "LocalSystem",
                ])
                .status();
            if !matches!(configured, Ok(status) if status.success()) {
                eprintln!("agent: sc.exe config failed: {configured:?}");
                return ExitCode::FAILURE;
            }
        } else {
            let created = Command::new("sc.exe")
                .args([
                    "create",
                    SERVICE_NAME,
                    "binPath=",
                    &command,
                    "start=",
                    "auto",
                    "obj=",
                    "LocalSystem",
                    "DisplayName=",
                    "Rhydra Agent",
                ])
                .status();
            if !matches!(created, Ok(status) if status.success()) {
                eprintln!("agent: sc.exe create failed: {created:?}");
                return ExitCode::FAILURE;
            }
        }
        let _ = Command::new("sc.exe")
            .args([
                "description",
                SERVICE_NAME,
                "Runs Rhydra capture and input in the active console session",
            ])
            .status();
        match Command::new("sc.exe")
            .args(["start", SERVICE_NAME])
            .status()
        {
            Ok(status) if status.success() => {
                println!("installed and started: service {SERVICE_NAME}");
                ExitCode::SUCCESS
            }
            other => {
                eprintln!("agent: service registered but start failed: {other:?}");
                ExitCode::FAILURE
            }
        }
    }

    fn installation_root(exe: &std::path::Path) -> Result<std::path::PathBuf, String> {
        let version_dir = exe
            .parent()
            .ok_or_else(|| "agent executable has no parent".to_owned())?;
        match version_dir.file_name().and_then(|name| name.to_str()) {
            Some(name) if name.starts_with('v') => version_dir
                .parent()
                .map(std::path::Path::to_path_buf)
                .ok_or_else(|| "version directory has no parent".to_owned()),
            _ => Ok(version_dir.to_path_buf()),
        }
    }

    fn harden_installation_acl(root: &std::path::Path) -> Result<(), String> {
        let root = root
            .to_str()
            .ok_or_else(|| "installation root is not valid Unicode".to_owned())?;
        for (step, arguments) in windows_acl_commands(root).into_iter().enumerate() {
            let status = Command::new("icacls.exe")
                .args(&arguments)
                .status()
                .map_err(|error| format!("icacls step {} failed to start: {error}", step + 1))?;
            if !status.success() {
                return Err(format!("icacls step {} exited with {status}", step + 1));
            }
        }
        Ok(())
    }

    fn service_registered() -> bool {
        Command::new("sc.exe")
            .args(["query", SERVICE_NAME])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn wait_for_service_stopped(timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let output = Command::new("sc.exe")
                .args(["query", SERVICE_NAME])
                .output();
            match output {
                Ok(output) if !output.status.success() => return true,
                Ok(output) if String::from_utf8_lossy(&output.stdout).contains("STOPPED") => {
                    return true
                }
                _ if Instant::now() >= deadline => return false,
                _ => std::thread::sleep(Duration::from_millis(250)),
            }
        }
    }

    fn delete_legacy_task() -> Result<(), String> {
        let registered = Command::new("schtasks")
            .args(["/query", "/tn", LEGACY_TASK_NAME])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if registered {
            let _ = Command::new("schtasks")
                .args(["/end", "/tn", LEGACY_TASK_NAME])
                .status();
            let deleted = Command::new("schtasks")
                .args(["/delete", "/tn", LEGACY_TASK_NAME, "/f"])
                .status();
            if !matches!(deleted, Ok(status) if status.success()) {
                return Err(format!("schtasks /delete failed: {deleted:?}"));
            }
        }
        Ok(())
    }

    fn request_worker_shutdown() -> Result<(), String> {
        let Ok(mut stream) = TcpStream::connect(("127.0.0.1", CONTROL_PORT)) else {
            return Ok(());
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|error| format!("set timeout failed: {error}"))?;
        writeln!(stream, "{{\"cmd\":\"shutdown\"}}")
            .map_err(|error| format!("shutdown request failed: {error}"))?;
        let mut reply = String::new();
        BufReader::new(&stream)
            .read_line(&mut reply)
            .map_err(|error| format!("shutdown response failed: {error}"))?;
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if TcpStream::connect(("127.0.0.1", CONTROL_PORT)).is_err() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        Err(format!(
            "worker still answered on {CONTROL_PORT} 15 s after shutdown"
        ))
    }

    /// Forward the request to the already-running interactive agent.  This
    /// executable may be invoked from an SSH service session, but it never
    /// performs per-user Core Audio mutation in that session.
    pub fn configure_audio() -> ExitCode {
        match audio_control_request("configure-audio", true) {
            Ok(reply) => {
                let source_path = match std::env::current_exe()
                    .ok()
                    .and_then(|path| path.parent().map(|parent| parent.join("audio-source")))
                {
                    Some(path) => path,
                    None => {
                        eprintln!("agent: cannot resolve the deployed audio-source path");
                        return ExitCode::FAILURE;
                    }
                };
                if let Err(error) = std::fs::write(&source_path, "loopback\n") {
                    eprintln!(
                        "agent: enable loopback source {} failed: {error}",
                        source_path.display()
                    );
                    return ExitCode::FAILURE;
                }
                if let Err(error) = rhydra::win::audio_policy::write_format_marker(reply.trim()) {
                    eprintln!("agent: configure-audio marker failed: {error}");
                    return ExitCode::FAILURE;
                }
                println!("{}", reply.trim());
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("agent: configure-audio failed: {error}");
                ExitCode::FAILURE
            }
        }
    }

    pub fn check_audio() -> ExitCode {
        match audio_control_request("check-audio", false) {
            Ok(reply) => {
                println!("{}", reply.trim());
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("agent: check-audio failed: {error}");
                ExitCode::FAILURE
            }
        }
    }

    fn audio_control_request(command: &str, wait_for_agent: bool) -> Result<String, String> {
        let deadline = Instant::now()
            + if wait_for_agent {
                Duration::from_secs(15)
            } else {
                Duration::ZERO
            };
        let mut stream = loop {
            match TcpStream::connect(("127.0.0.1", CONTROL_PORT)) {
                Ok(stream) => break stream,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(250));
                }
                Err(error) => return Err(format!("needs the interactive agent: {error}")),
            }
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        writeln!(stream, "{{\"cmd\":\"{command}\"}}")
            .map_err(|error| format!("request failed: {error}"))?;
        let mut reply = String::new();
        BufReader::new(&stream)
            .read_line(&mut reply)
            .map_err(|error| format!("response failed: {error}"))?;
        let parsed: serde_json::Value =
            serde_json::from_str(&reply).map_err(|error| format!("invalid response: {error}"))?;
        if parsed.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
            Ok(reply)
        } else {
            Err(format!("refused: {}", reply.trim()))
        }
    }

    pub fn uninstall() -> ExitCode {
        if service_registered() {
            let stop = Command::new("sc.exe").args(["stop", SERVICE_NAME]).status();
            if !matches!(stop, Ok(status) if status.success()) {
                eprintln!("agent: sc.exe stop failed: {stop:?}");
            }
            if !wait_for_service_stopped(Duration::from_secs(20)) {
                eprintln!("agent: service {SERVICE_NAME} did not stop within 20 s");
                return ExitCode::FAILURE;
            }
        }
        let root = match exe_root() {
            Ok(root) => root,
            Err(error) => {
                eprintln!("agent: cannot identify installation: {error}");
                return ExitCode::FAILURE;
            }
        };
        let images = ["rhydra-agent.exe", OWNED_IMAGES[0], OWNED_IMAGES[1]];
        if let Err(error) = terminate_owned_processes(&root, &images, Some(std::process::id())) {
            eprintln!("agent: fallback shutdown failed: {error}");
            return ExitCode::FAILURE;
        }
        if service_registered() {
            let deleted = Command::new("sc.exe")
                .args(["delete", SERVICE_NAME])
                .status();
            if !matches!(deleted, Ok(status) if status.success()) {
                eprintln!("agent: sc.exe delete failed: {deleted:?}");
                return ExitCode::FAILURE;
            }
        }
        if let Err(error) = delete_legacy_task() {
            eprintln!("agent: {error}");
            return ExitCode::FAILURE;
        }
        println!("uninstalled service {SERVICE_NAME} and legacy task {LEGACY_TASK_NAME}");
        ExitCode::SUCCESS
    }

    /// One status sample (or a stable-green wait) against the local control port.
    /// Exit 0: green (stable-green under `--wait`). Exit 1: agent answered but is
    /// not green yet — bring-up walking `stuck` steps lands here by design.
    /// Exit 2: nothing answered.
    pub fn status(wait: Option<u64>) -> ExitCode {
        use rhydra::control::{green, query_status, wait_stable_green, QueryError, WaitOutcome};
        const ADDR: (&str, u16) = ("127.0.0.1", CONTROL_PORT);
        // This CLI path (`rhydra-agent status`) is a local diagnostic, not the
        // §4.3 native-connect probe — it keeps the previous 5 s budget.
        const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
        match wait {
            None => match query_status(ADDR, QUERY_TIMEOUT) {
                Ok(report) => {
                    println!("{}", control::status_line(&report));
                    if green(&report) {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::from(1)
                    }
                }
                Err(QueryError::NoAnswer(e)) | Err(QueryError::Bad(e)) => {
                    eprintln!("agent: {e}");
                    ExitCode::from(2)
                }
            },
            Some(secs) => {
                match wait_stable_green(
                    ADDR,
                    Duration::from_secs(secs),
                    Duration::from_secs(3),
                    QUERY_TIMEOUT,
                ) {
                    WaitOutcome::StableGreen(report) => {
                        println!("{}", control::status_line(&report));
                        ExitCode::SUCCESS
                    }
                    WaitOutcome::NotGreen(report) => {
                        println!("{}", control::status_line(&report));
                        eprintln!(
                            "agent: not stably green after {secs} s (stuck: {})",
                            report.stuck.as_deref().unwrap_or("server counters moving")
                        );
                        ExitCode::from(1)
                    }
                    WaitOutcome::NoAnswer(e) => {
                        eprintln!("agent: {e}");
                        ExitCode::from(2)
                    }
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn reconcile_thread_selects_per_monitor_v2_dpi_awareness() {
            super::initialize_reconcile_thread().expect("initialize reconcile-thread DPI context");
            let context = unsafe { windows::Win32::UI::HiDpi::GetThreadDpiAwarenessContext() };
            assert!(unsafe {
                windows::Win32::UI::HiDpi::AreDpiAwarenessContextsEqual(
                    context,
                    windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                )
            }
            .as_bool());
        }
    }
}
