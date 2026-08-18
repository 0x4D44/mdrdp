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
    match std::env::args().nth(1).as_deref() {
        Some("run") => win::run(),
        Some("install") => win::install(),
        Some("uninstall") => win::uninstall(),
        _ => {
            eprintln!("usage: rhydra-agent run|install|uninstall");
            ExitCode::from(2)
        }
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
        let shared = Arc::new(Mutex::new(Shared {
            status_line: control::status_line(&rec.status(0)),
            restart_requested: false,
            shutdown_requested: false,
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
            shared.lock().expect("not poisoned").status_line = control::status_line(&report);

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
                std::thread::sleep(Duration::from_secs(2));
            }
            Err(_) => {
                // Not running. Its children cannot be running supervised either,
                // but a hard-killed agent may have left orphans: sweep the owned
                // images (never our own image — that would kill this process).
                for image in OWNED_IMAGES {
                    let _ = Command::new("taskkill").args(["/f", "/im", image]).status();
                }
            }
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
}
