//! Detaching a GUI run from the terminal that launched it.
//!
//! `mdrdp` and `mdrdp <host>` open windows; holding the shell prompt hostage for the
//! life of the window helps nobody. When a GUI run starts from a terminal, the process
//! re-spawns itself detached — new process group, stdin closed, stdout/stderr to a log
//! file under the config directory — and the parent exits, giving the prompt back.
//!
//! The parent does not vanish instantly: it lingers just long enough to catch the
//! failures that happen at startup (a bad favourite name, a missing keychain entry, a
//! refused connect) and replays the child's log to the terminal, so a fast failure
//! still reads exactly like a foreground one. For a session run it watches the log for
//! the [`CONNECTED_MARKER`] line and releases the terminal the moment the session is
//! actually up.
//!
//! What never detaches: anything that talks to the invoker over stdio
//! (`--stage-json`, `--password-stdin`), scripted runs whose caller waits for the exit
//! status (`--duration`, `--screenshot`, `--metrics-json`, `--input-script`,
//! `--capture-failures`), `--list`, an explicit `--foreground`, a run whose stderr is
//! not a terminal (there is no prompt to free), and the detached child itself.
//!
//! The log carries only what stderr already carried — status lines and stats, never
//! credentials or session contents.

use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Set in the detached child's environment so it neither detaches again nor falls back
/// to a terminal password prompt — reading the tty from a background process group
/// stops the process with SIGTTIN, invisibly, which is worse than failing with a
/// message in the log.
pub const DETACHED_ENV: &str = "MDRDP_DETACHED";

/// The stable prefix of the line a session prints once the connection is established.
/// The detaching parent scans the child's log for it; `main` prints it. One constant so
/// the two cannot drift apart.
pub const CONNECTED_MARKER: &str = "connected:";

/// Logs older than this are removed when a new one is created, so detached runs never
/// accumulate files forever.
const LOG_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How long the parent will wait for a session child to report a connection before
/// declaring it "still starting" and releasing the terminal anyway. Generous on
/// purpose: waiting costs nothing once the marker check exists, and a WAN connect can
/// legitimately take seconds. Sized above a worst-case native probe (8 s deadline)
/// plus the RDP fallback connect that can follow it under `native = auto`.
const CONNECT_GRACE: Duration = Duration::from_secs(15);

/// How long the parent watches a launcher child (which has no "connected" moment) for
/// an early exit before releasing the terminal.
const LAUNCHER_GRACE: Duration = Duration::from_millis(800);

const POLL: Duration = Duration::from_millis(40);

/// The flags that keep a run attached to its terminal.
#[derive(Debug, Default)]
pub struct LaunchFlags {
    pub foreground: bool,
    /// Stdio is a protocol pipe to the launcher.
    pub stage_json: bool,
    /// The password arrives on stdin.
    pub password_stdin: bool,
    pub list_only: bool,
    /// `--duration`, `--screenshot`, `--metrics-json`, `--input-script` or
    /// `--capture-failures`: a harness is driving and waits for the exit status.
    pub scripted: bool,
}

/// Whether this process is the detached child of an earlier `mdrdp` invocation.
pub fn already_detached() -> bool {
    std::env::var_os(DETACHED_ENV).is_some()
}

/// The one policy decision: should this run give the terminal back?
pub fn should_detach(
    flags: &LaunchFlags,
    stderr_is_terminal: bool,
    already_detached: bool,
) -> bool {
    stderr_is_terminal
        && !already_detached
        && !flags.foreground
        && !flags.stage_json
        && !flags.password_stdin
        && !flags.list_only
        && !flags.scripted
}

/// Whether a child's log shows the session came up.
///
/// Matches only a line *starting* with the marker: "connecting to host …" must not
/// count, and neither may the marker appearing mid-line in some future message.
fn log_reports_connected(log: &str) -> bool {
    log.lines().any(|l| l.starts_with(CONNECTED_MARKER))
}

/// How the detached startup went, as far as the lingering parent could see.
pub enum StartupOutcome {
    /// The child connected, or was still healthy when the grace ran out.
    Running,
    /// The child exited during startup. Its log has already been replayed to stderr.
    Failed { code: Option<i32> },
}

/// Re-spawn this invocation detached and watch its startup.
///
/// `waits_for_connect` is true for a session run (there is a "connected" moment to wait
/// for) and false for the launcher. An `Err` means the detach could not even be set up
/// — the caller should warn and simply continue in the foreground.
pub fn respawn(waits_for_connect: bool) -> Result<StartupOutcome, String> {
    let log_path = new_log_path()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("could not open {}: {e}", log_path.display()))?;
    let log_for_stdout = log
        .try_clone()
        .map_err(|e| format!("could not clone the log handle: {e}"))?;

    let exe = std::env::current_exe().map_err(|e| format!("cannot find own path: {e}"))?;
    let mut command = Command::new(exe);
    command
        .args(std::env::args().skip(1))
        .env(DETACHED_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_for_stdout))
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        // A new process group: terminal signals (Ctrl-C, hangup on close) stay with
        // the shell's job and never reach the session.
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
    }
    #[cfg(windows)]
    {
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP: no console, no Ctrl-C sharing.
        std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0000_0208);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("could not re-launch detached: {e}"))?;

    let grace = if waits_for_connect {
        CONNECT_GRACE
    } else {
        LAUNCHER_GRACE
    };
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Died during startup: put its log on the terminal, where a foreground
                // failure would have landed.
                replay_log(&log_path);
                return Ok(StartupOutcome::Failed {
                    code: status.code(),
                });
            }
            Ok(None) => {}
            Err(e) => return Err(format!("could not watch the detached process: {e}")),
        }
        if waits_for_connect
            && std::fs::read_to_string(&log_path).is_ok_and(|log| log_reports_connected(&log))
        {
            eprintln!(
                "connected — running in the background (log: {})",
                log_path.display()
            );
            return Ok(StartupOutcome::Running);
        }
        if Instant::now() >= deadline {
            if waits_for_connect {
                eprintln!(
                    "still starting — running in the background (log: {})",
                    log_path.display()
                );
            } else {
                eprintln!("running in the background (log: {})", log_path.display());
            }
            return Ok(StartupOutcome::Running);
        }
        std::thread::sleep(POLL);
    }
}

/// A fresh log path under `<config dir>/logs`, pruning stale siblings while there.
fn new_log_path() -> Result<PathBuf, String> {
    let dir = crate::favourites::Favourites::default_path()
        .map_err(|e| e.to_string())?
        .with_file_name("logs");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    prune_old_logs(&dir);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Ok(dir.join(format!("mdrdp-{stamp}-{}.log", std::process::id())))
}

/// Best-effort removal of this module's own old logs; any error just leaves the file.
fn prune_old_logs(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !(name.starts_with("mdrdp-") && name.ends_with(".log")) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > LOG_RETENTION);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Print the child's short startup log to stderr, trimmed of the trailing newline.
fn replay_log(path: &std::path::Path) {
    let mut text = String::new();
    if std::fs::File::open(path)
        .and_then(|mut f| f.read_to_string(&mut text))
        .is_ok()
    {
        eprintln!("{}", text.trim_end());
    } else {
        eprintln!("(its log at {} could not be read back)", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gui_run() -> LaunchFlags {
        LaunchFlags::default()
    }

    #[test]
    fn a_plain_gui_run_from_a_terminal_detaches() {
        assert!(should_detach(&gui_run(), true, false));
    }

    #[test]
    fn no_terminal_means_nothing_to_free() {
        assert!(!should_detach(&gui_run(), false, false));
    }

    #[test]
    fn the_detached_child_never_detaches_again() {
        assert!(!should_detach(&gui_run(), true, true));
    }

    #[test]
    fn every_stdio_and_scripted_flag_keeps_the_run_attached() {
        for flags in [
            LaunchFlags {
                foreground: true,
                ..gui_run()
            },
            LaunchFlags {
                stage_json: true,
                ..gui_run()
            },
            LaunchFlags {
                password_stdin: true,
                ..gui_run()
            },
            LaunchFlags {
                list_only: true,
                ..gui_run()
            },
            LaunchFlags {
                scripted: true,
                ..gui_run()
            },
        ] {
            assert!(
                !should_detach(&flags, true, false),
                "{flags:?} must stay attached"
            );
        }
    }

    #[test]
    fn the_connected_marker_is_matched_only_at_line_start() {
        assert!(log_reports_connected(
            "mdrdp v0.1.37\nconnecting to temper:3389 …\nconnected: 1920x1080, pinned, TLSv1_2\n"
        ));
        assert!(
            !log_reports_connected("mdrdp v0.1.37\nconnecting to temper:3389 …\n"),
            "still connecting is not connected"
        );
        assert!(
            !log_reports_connected("note: not connected: retrying\n"),
            "mid-line mentions must not count"
        );
    }
}
