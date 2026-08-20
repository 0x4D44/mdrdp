//! `rhydra-agent` — the logon-task session agent.
//!
//! `run` is the scheduled task's target: it sweeps orphans, then reconciles the
//! stack (IDD creator → device → 240 Hz mode → capture server) every tick,
//! serving status and restart/shutdown commands on the loopback control port.
//! `install` registers the onlogon task and starts it now; `uninstall` shuts a
//! running agent down over the control port and deletes the task.
//!
//! Stopping the agent any other way than `shutdown` (or `uninstall`) orphans the
//! children — `schtasks /end` in particular kills only the task process. The
//! orphan sweep at the next start repairs it, but the sanctioned stop is
//! `{"cmd":"shutdown"}`.

use std::process::ExitCode;

#[cfg(windows)]
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => win::run(),
        Some("install") => win::install(),
        Some("uninstall") => win::uninstall(),
        Some("status") => match parse_wait(&args[1..]) {
            Ok(wait) => win::status(wait),
            Err(e) => {
                eprintln!("agent: {e}");
                ExitCode::from(2)
            }
        },
        _ => {
            eprintln!("usage: rhydra-agent run|install|uninstall|status [--wait <secs>]");
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

#[cfg(windows)]
mod win {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::process::{Command, ExitCode};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use rhydra::agent::{Reconciler, TICK_SECS};
    use rhydra::control::{self, Request, CONTROL_PORT};
    use rhydra::win::agent_ops::{exe_root, WinOps, OWNED_IMAGES};

    const TASK_NAME: &str = "rhydra-agent";

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

    pub fn run() -> ExitCode {
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

        let mut ops = WinOps::new(root);
        ops.sweep_orphans();

        let mut rec = Reconciler::new();
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
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let line = line?;
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
            };
            writeln!(writer, "{reply}")?;
        }
        Ok(())
    }

    pub fn install() -> ExitCode {
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("agent: current_exe: {e}");
                return ExitCode::FAILURE;
            }
        };
        // onlogon alone would not fire until the NEXT logon, so start it now too.
        let create = Command::new("schtasks")
            .args([
                "/create",
                "/tn",
                TASK_NAME,
                "/sc",
                "onlogon",
                "/rl",
                "highest",
                "/f",
                "/tr",
                &format!("\"{}\" run", exe.display()),
            ])
            .status();
        match create {
            Ok(s) if s.success() => {}
            other => {
                eprintln!("agent: schtasks /create failed: {other:?}");
                return ExitCode::FAILURE;
            }
        }
        match Command::new("schtasks")
            .args(["/run", "/tn", TASK_NAME])
            .status()
        {
            Ok(s) if s.success() => {
                println!("installed and started: task {TASK_NAME}");
                ExitCode::SUCCESS
            }
            other => {
                eprintln!("agent: task registered but /run failed: {other:?}");
                ExitCode::FAILURE
            }
        }
    }

    pub fn uninstall() -> ExitCode {
        // The sanctioned stop: ask the running agent to kill its children and exit.
        match TcpStream::connect(("127.0.0.1", CONTROL_PORT)) {
            Ok(mut stream) => {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let _ = writeln!(stream, "{}", r#"{"cmd":"shutdown"}"#);
                let mut reply = String::new();
                let _ = BufReader::new(&stream).read_line(&mut reply);
                println!("agent shutdown acknowledged: {}", reply.trim());
                // The agent notices shutdown at the top of its loop (up to a full
                // tick plus a reconcile away), then still has to reap children.
                // Poll until the port actually refuses — a fixed sleep raced the
                // exit and handed deploy a still-locked exe (review finding).
                let deadline = Instant::now() + Duration::from_secs(15);
                loop {
                    std::thread::sleep(Duration::from_millis(500));
                    if TcpStream::connect(("127.0.0.1", CONTROL_PORT)).is_err() {
                        break;
                    }
                    if Instant::now() >= deadline {
                        eprintln!("agent: still answering {CONTROL_PORT} 15 s after shutdown ack");
                        return ExitCode::FAILURE;
                    }
                }
                // Port closed; give image unmap a beat.
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(_) => {
                // No listener. Either no agent, or one that is wedged/starting with
                // its port down — kill other instances of our own image by PID
                // filter (plain /im would kill this process too), then sweep the
                // orphans a hard-killed agent leaves behind.
                let self_pid = std::process::id().to_string();
                let _ = Command::new("taskkill")
                    .args([
                        "/f",
                        "/fi",
                        "IMAGENAME eq rhydra-agent.exe",
                        "/fi",
                        &format!("PID ne {self_pid}"),
                    ])
                    .status();
                for image in OWNED_IMAGES {
                    let _ = Command::new("taskkill").args(["/f", "/im", image]).status();
                }
            }
        }
        // A clean host has no task registered; "not found" is success, not failure
        // (review finding: uninstall used to fail every fresh deploy's quiesce).
        let registered = Command::new("schtasks")
            .args(["/query", "/tn", TASK_NAME])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !registered {
            println!("task {TASK_NAME} not registered; nothing to delete");
            return ExitCode::SUCCESS;
        }
        match Command::new("schtasks")
            .args(["/delete", "/tn", TASK_NAME, "/f"])
            .status()
        {
            Ok(s) if s.success() => {
                println!("task {TASK_NAME} deleted");
                ExitCode::SUCCESS
            }
            other => {
                eprintln!("agent: schtasks /delete failed: {other:?}");
                ExitCode::FAILURE
            }
        }
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
}
