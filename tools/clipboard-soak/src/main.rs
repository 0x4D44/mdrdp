//! The clipboard soak: rhydra tranche 5's AC8.
//!
//! ```text
//! clipboard-soak <host> --hours 8 [--ssh-user ano] [--out report.json]
//! ```
//!
//! Runs one long `mdrdp --native` session and exercises the clipboard across
//! it, then writes a report that says what it proved and — more importantly —
//! what it did not.
//!
//! # Why a soak at all
//!
//! The requirement is *"survives an all-day soak with zero wedges"*, which is
//! the first of the five faults this product exists to fix. **A green unit test
//! cannot discharge it**: every wedge worth preventing is a thing that happens
//! after hours, under contention, at the tail of a distribution.
//!
//! # Rules this harness follows, and why each one is load-bearing
//!
//! - **One session for the whole run.** It never reconnects and never rebuilds
//!   the clipboard handle. Doing either would silently repair the state under
//!   test — a harness that reconnects on trouble reports a healthy clipboard
//!   for a broken one.
//! - **Every payload carries a fresh nonce and is synthetic by construction**,
//!   so a mismatch can be recorded in full without ever writing someone's real
//!   clipboard to a log.
//! - **Two consecutive misses in one direction stops the run.** Averaging a
//!   wedge over the remaining hours is how a soak reports 99.8% success on a
//!   clipboard that died at 02:00.
//! - **A competing session voids the run rather than failing it.** Another
//!   agent connecting means the thing measured was not ours.
//! - **A short run is labelled, not rounded up.** Four clean hours is genuinely
//!   useful and genuinely not eight.

mod report;

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use report::{Attempt, Direction, Ending, MissRun, Outcome};

/// How often a routine there-and-back cycle runs.
const CYCLE: Duration = Duration::from_secs(30);

/// How often the hourly stressors run.
const STRESS_EVERY: Duration = Duration::from_secs(60 * 60);

/// Transfers in an hourly burst, at 1 Hz.
///
/// Bursts matter because the wedge this product exists to beat shows up under
/// load, not at one transfer per thirty seconds.
const BURST: u32 = 60;

/// How long to wait for a payload before calling it a miss.
///
/// Generous against a 250 ms poll on each side plus a network hop: a miss
/// should mean *gone*, not *slow*, or the run will chase its own timeouts.
const ARRIVAL_DEADLINE: Duration = Duration::from_secs(20);

/// How often resident memory is sampled.
const RSS_EVERY: Duration = Duration::from_secs(60);

/// Growth beyond this over a run is worth reporting on its own: the product's
/// third named fault is "latency degrades the longer a session runs", and an
/// leaking client is one way that happens.
const RSS_GROWTH_THRESHOLD_MB: f64 = 100.0;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(host) = args.first().filter(|a| !a.starts_with("--")).cloned() else {
        eprintln!("usage: clipboard-soak <host> --hours 8 [--ssh-user ano] [--out report.json]");
        return std::process::ExitCode::from(2);
    };
    let hours = flag(&args, "--hours")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(8.0);
    let ssh_user = flag(&args, "--ssh-user");
    let out = flag(&args, "--out");

    match run(&host, hours, ssh_user.as_deref(), out.as_deref()) {
        Ok(verdict) => {
            println!("\n{}", verdict.summary);
            if verdict.discharges_ac8 || verdict.provisional {
                std::process::ExitCode::SUCCESS
            } else {
                std::process::ExitCode::from(1)
            }
        }
        Err(e) => {
            eprintln!("soak: {e}");
            std::process::ExitCode::from(1)
        }
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn run(
    host: &str,
    hours: f64,
    ssh_user: Option<&str>,
    out: Option<&str>,
) -> Result<report::Verdict, String> {
    let mdrdp = std::env::var("MDRDP").unwrap_or_else(|_| "./target/release/mdrdp".to_owned());
    let planned = Duration::from_secs_f64(hours * 3600.0);
    let started_utc = SystemTime::now();

    println!("soak: {host}, {hours} h planned, one session throughout");
    println!("soak: client {}", version(&mdrdp));

    // ONE session, for the whole run. `--duration` ends it cleanly; the harness
    // never restarts it, because restarting is what would hide the failure.
    let mut session = spawn_session(&mdrdp, host, ssh_user, planned + Duration::from_secs(60))?;

    let began = Instant::now();
    let mut attempts: Vec<Attempt> = Vec::new();
    let mut misses = MissRun::default();
    let mut rss_samples: Vec<f64> = Vec::new();
    let mut last_stress = Instant::now();
    let mut last_rss = Instant::now() - RSS_EVERY;
    let mut ending = Ending::Completed;
    let mut seq: u64 = 0;

    // Let the session connect and seed before the first transfer: content
    // already on the clipboard at connect is deliberately never sent, so a
    // transfer attempted too early would be suppressed and read as a miss.
    std::thread::sleep(Duration::from_secs(15));

    'outer: while began.elapsed() < planned {
        if let Some(why) = session_died(&mut session) {
            // **Which kind of death matters.** quench serves one viewer at a
            // time, so another agent connecting kicks us — and a run that ended
            // that way measured a session that was not ours for part of its
            // life. Reporting it as a failure would send someone hunting a bug
            // that is not there; reporting it as a pass would be worse.
            ending = match another_viewer_holds_the_host(&mdrdp, host, ssh_user) {
                Some(true) => Ending::Void {
                    why: format!("another viewer holds the host ({why})"),
                },
                _ => Ending::SessionEnded { why },
            };
            break;
        }

        if last_rss.elapsed() >= RSS_EVERY {
            if let Some(mb) = client_rss_mb(&session) {
                rss_samples.push(mb);
            }
            last_rss = Instant::now();
        }

        // The routine cycle. Only Mac->host is driven; see `transfer`.
        for direction in [Direction::MacToHost, Direction::HostToMac] {
            seq += 1;
            let attempt = transfer(&mdrdp, host, ssh_user, direction, seq, began);
            if attempt.outcome.is_miss() {
                println!(
                    "soak: MISS {} at {:.0}s (nonce {})",
                    direction.name(),
                    attempt.at_secs,
                    attempt.nonce
                );
            }
            let outcome = attempt.outcome;
            attempts.push(attempt);
            if let Some(wedge) = misses.note(direction, outcome) {
                ending = wedge;
                break 'outer;
            }
        }

        if last_stress.elapsed() >= STRESS_EVERY {
            println!("soak: hourly burst of {BURST} at 1 Hz");
            for _ in 0..BURST {
                seq += 1;
                let attempt = transfer(&mdrdp, host, ssh_user, Direction::MacToHost, seq, began);
                let outcome = attempt.outcome;
                attempts.push(attempt);
                if let Some(wedge) = misses.note(Direction::MacToHost, outcome) {
                    ending = wedge;
                    break 'outer;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            last_stress = Instant::now();
        }

        std::thread::sleep(CYCLE);
    }

    // Whatever happened, end the session the same way — abandoning it leaves
    // the host holding a viewer slot it does not reclaim promptly.
    let _ = session.kill();
    let _ = session.wait();

    let ran_for = began.elapsed();
    let latencies: Vec<f64> = attempts
        .iter()
        .filter_map(|a| a.outcome.latency_ms())
        .collect();
    let miss_count = attempts.iter().filter(|a| a.outcome.is_miss()).count() as u32;
    let verdict = report::judge(&ending, ran_for, miss_count);

    if let Some(path) = out {
        write_report(
            path,
            host,
            &mdrdp,
            started_utc,
            ran_for,
            &attempts,
            &rss_samples,
            &verdict,
        )?;
        println!("soak: report written to {path}");
    }
    print_summary(&attempts, &latencies, &rss_samples);
    Ok(verdict)
}

/// One transfer, timed from the copy to the confirmed arrival.
fn transfer(
    mdrdp: &str,
    host: &str,
    ssh_user: Option<&str>,
    direction: Direction,
    seq: u64,
    began: Instant,
) -> Attempt {
    // Synthetic by construction: a counter and a run-unique prefix, never
    // anything read from a real clipboard. That is what makes it safe to write
    // a mismatching payload into the report.
    let nonce = format!("SOAK-{}-{seq:06}", direction.name().replace("->", "2"));
    let at_secs = began.elapsed().as_secs_f64();
    let sent = Instant::now();

    let outcome = match direction {
        Direction::MacToHost => {
            if pbcopy(&nonce).is_err() {
                Outcome::NeverArrived
            } else {
                match await_host(mdrdp, host, ssh_user, &nonce, sent) {
                    Some(ms) => Outcome::Arrived(ms),
                    None => Outcome::NeverArrived,
                }
            }
        }
        // **Not driven, and recorded as such rather than as a miss.**
        //
        // Setting the host's clipboard needs something running in its console
        // session — an ssh session is a different window station — and a soak
        // must not type on the desktop for eight hours. The missing piece is a
        // small host-side generator, launched once at session start, that
        // writes a predictable nonce sequence the Mac can watch for.
        //
        // Until that exists this soak covers one direction, and AC8's
        // "a transfer each way" is only half met. Saying so is the whole reason
        // `NotExercised` is a state: the first version returned "no latency"
        // here, which counts as a non-arrival, and the run would have declared
        // a wedge on its second cycle.
        Direction::HostToMac => Outcome::NotExercised,
    };

    Attempt {
        direction,
        at_secs,
        outcome,
        nonce,
    }
}

/// Poll the host until it holds `nonce`, or the deadline passes.
///
/// # What the resulting latency is, and is not
///
/// **It is an upper bound on transfer latency, not a measurement of it.** Each
/// poll spawns `mdrdp --clipboard-check`, which spawns ssh, which costs on the
/// order of 180 ms before it has asked anything. A smoke run measured
/// `min=186ms median=188ms` — those figures are the *instrument*, not the
/// clipboard.
///
/// That is fine for what AC8 asks (did it arrive, and did arrival degrade over
/// hours) and it must not be quoted as clipboard latency. This repo has twice
/// published a figure that turned out to be measuring something else, and the
/// caveat is written here so the third time does not start in this function.
fn await_host(
    mdrdp: &str,
    host: &str,
    ssh_user: Option<&str>,
    nonce: &str,
    sent: Instant,
) -> Option<f64> {
    while sent.elapsed() < ARRIVAL_DEADLINE {
        let mut cmd = Command::new(mdrdp);
        cmd.arg(host).arg("--clipboard-check").arg(nonce);
        if let Some(user) = ssh_user {
            cmd.arg("--ssh-user").arg(user);
        }
        let ok = cmd
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Some(sent.elapsed().as_secs_f64() * 1000.0);
        }
    }
    None
}

fn pbcopy(text: &str) -> Result<(), String> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .as_mut()
        .ok_or("no stdin")?
        .write_all(text.as_bytes())
        .map_err(|e| e.to_string())?;
    child.wait().map_err(|e| e.to_string())?;
    Ok(())
}

fn spawn_session(
    mdrdp: &str,
    host: &str,
    ssh_user: Option<&str>,
    duration: Duration,
) -> Result<Child, String> {
    let mut cmd = Command::new(mdrdp);
    cmd.arg(host)
        .arg("--native")
        .arg("--foreground")
        .arg("--duration")
        .arg(duration.as_secs().to_string());
    if let Some(user) = ssh_user {
        cmd.arg("--ssh-user").arg(user);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start the session: {e}"))
}

/// Is someone else holding the host's single viewer slot?
///
/// Asked only once our own session has died, so a `true` here cannot be us.
/// `None` means the question could not be answered, and the caller treats that
/// as "not void" — claiming a run void on a failed query would let any network
/// hiccup erase a real failure.
///
/// A heuristic, and deliberately a cheap one: the board claim is the actual
/// protection against a competing session, and this is the backstop for when
/// somebody did not read it.
fn another_viewer_holds_the_host(mdrdp: &str, host: &str, ssh_user: Option<&str>) -> Option<bool> {
    let mut cmd = Command::new(mdrdp);
    cmd.arg(host).arg("--doctor");
    if let Some(user) = ssh_user {
        cmd.arg("--ssh-user").arg(user);
    }
    let out = cmd.output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let viewer = text
        .lines()
        .find(|l| l.trim_start().starts_with("viewer"))?;
    // The doctor prints "viewer ok none" when the slot is free.
    Some(!viewer.contains("none"))
}

fn session_died(session: &mut Child) -> Option<String> {
    match session.try_wait() {
        Ok(Some(status)) => Some(format!("the client exited with {status}")),
        Ok(None) => None,
        Err(e) => Some(format!("could not poll the client: {e}")),
    }
}

/// Resident memory of the session process, in MB.
fn client_rss_mb(session: &Child) -> Option<f64> {
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &session.id().to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<f64>()
        .ok()
        .map(|kb| kb / 1024.0)
}

fn version(mdrdp: &str) -> String {
    Command::new(mdrdp)
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn print_summary(attempts: &[Attempt], latencies: &[f64], rss: &[f64]) {
    println!("\n--- soak summary ---");
    let exercised = attempts
        .iter()
        .filter(|a| a.outcome != Outcome::NotExercised)
        .count();
    println!(
        "attempted {exercised} (of {} cycle slots; host->mac is not driven — see `transfer`)",
        attempts.len()
    );
    match report::latencies(latencies) {
        Some(l) => {
            println!(
                "arrival latency: n={} min={:.0}ms median={:.0}ms p95={:.0}ms max={:.0}ms",
                l.n, l.min_ms, l.median_ms, l.p95_ms, l.max_ms
            );
            // Said every run, not buried in a doc comment: the floor here is
            // the polling instrument, and someone will otherwise quote it.
            println!(
                "  (an UPPER BOUND — each check spawns ssh, worth ~180 ms before it asks anything)"
            );
        }
        // Never zeroes: an absent distribution must not look like a fast one.
        None => println!("arrival latency: no samples"),
    }
    for a in attempts.iter().filter(|a| a.outcome.is_miss()) {
        println!(
            "NON-ARRIVAL {} at {:.0}s ({})",
            a.direction.name(),
            a.at_secs,
            a.nonce
        );
    }
    if let (Some(first), Some(last)) = (rss.first(), rss.last()) {
        let growth = last - first;
        println!(
            "client RSS: {first:.0} MB -> {last:.0} MB (growth {growth:.0} MB, threshold {RSS_GROWTH_THRESHOLD_MB:.0} MB){}",
            if growth > RSS_GROWTH_THRESHOLD_MB { " -- OVER" } else { "" }
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn write_report(
    path: &str,
    host: &str,
    mdrdp: &str,
    started: SystemTime,
    ran_for: Duration,
    attempts: &[Attempt],
    rss: &[f64],
    verdict: &report::Verdict,
) -> Result<(), String> {
    let latencies: Vec<f64> = attempts
        .iter()
        .filter_map(|a| a.outcome.latency_ms())
        .collect();
    let exercised = attempts
        .iter()
        .filter(|a| a.outcome != Outcome::NotExercised)
        .count();
    let doc = serde_json::json!({
        "host": host,
        "client_version": version(mdrdp),
        "started_unix": started.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        "ran_for_secs": ran_for.as_secs(),
        "attempted": exercised,
        "recorded_but_not_driven": attempts.len() - exercised,
        "arrived": latencies.len(),
        "latency": report::latencies(&latencies),
        "non_arrivals": attempts.iter().filter(|a| a.outcome.is_miss()).collect::<Vec<_>>(),
        "rss_mb": rss,
        "verdict": verdict,
    });
    std::fs::write(
        path,
        serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
