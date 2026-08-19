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
///
/// **Staggered against the host generator's period on purpose.** Simultaneous
/// copies at both ends inside one poll interval leave the two clipboards
/// disagreeing — HLD §5 states that case and does not solve it — so a soak that
/// collided them would report known, accepted behaviour as a wedge, and someone
/// would spend a morning on it.
const CYCLE: Duration = Duration::from_secs(60);

/// What `clipboard-cycle` must be launched with on the host.
///
/// The client copies at the top of its cycle and waits for the host's nonce
/// afterwards, so the two never land together — see `CYCLE`.
/// Longer than [`CYCLE`] on purpose: at most one host write per client cycle,
/// so the mac->host check always has clear air in front of it.
const DEFAULT_HOST_PERIOD: Duration = Duration::from_secs(90);

/// How often the hourly stressors run.
const STRESS_EVERY: Duration = Duration::from_secs(60 * 60);

/// Transfers in an hourly burst, at 1 Hz.
///
/// Bursts matter because the wedge this product exists to beat shows up under
/// load, not at one transfer per thirty seconds.
const BURST: u32 = 60;

/// How long to wait for a Mac→host payload before calling it a miss.
///
/// Generous against a 250 ms poll on each side plus a network hop: a miss
/// should mean *gone*, not *slow*, or the run will chase its own timeouts.
const ARRIVAL_DEADLINE: Duration = Duration::from_secs(20);

/// Slack added to the host generator's period to get the host→Mac deadline.
///
/// **The deadline must exceed the generator's period, and by a clear margin.**
/// The client cannot make the host copy anything; it waits for the generator's
/// *next* write, so a wait that begins just after one has to sit through a full
/// period before it can possibly succeed. A smoke run with a 20 s deadline
/// against a 20 s period duly reported `WEDGED` — a **false wedge**, which is
/// the one thing a soak harness must never produce, because the next person
/// spends a morning looking for a bug that was a timeout.
const HOST_ARRIVAL_SLACK: Duration = Duration::from_secs(15);

/// How often resident memory is sampled.
const RSS_EVERY: Duration = Duration::from_secs(60);

/// Growth beyond this over a run is worth reporting on its own: the product's
/// third named fault is "latency degrades the longer a session runs", and an
/// leaking client is one way that happens.
const RSS_GROWTH_THRESHOLD_MB: f64 = 100.0;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(host) = args.first().filter(|a| !a.starts_with("--")).cloned() else {
        eprintln!(
            "usage: clipboard-soak <host> --hours 8 [--ssh-user ano] [--host-period 30] \
             [--out report.json]"
        );
        return std::process::ExitCode::from(2);
    };
    let hours = flag(&args, "--hours")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(8.0);
    let ssh_user = flag(&args, "--ssh-user");
    let out = flag(&args, "--out");
    let host_period = flag(&args, "--host-period")
        .and_then(|v| v.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_HOST_PERIOD);

    match run(
        &host,
        hours,
        ssh_user.as_deref(),
        out.as_deref(),
        host_period,
    ) {
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
    host_period: Duration,
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
    // Highest host-generated sequence the Mac has seen. Monotonic, so a stale
    // value cannot be counted as a fresh arrival.
    // Set by the startup probe below, which will not let the run begin until it
    // has seen the host generator produce one.
    let mut host_seen: u64;
    // Must exceed the generator's period: the client waits for its NEXT write.
    let host_deadline = host_period + HOST_ARRIVAL_SLACK;
    println!(
        "soak: launch `clipboard-cycle.exe {} <minutes>` on the host's console session; \
         host->mac deadline is {}s",
        host_period.as_secs(),
        host_deadline.as_secs()
    );

    // Let the session connect and seed before the first transfer: content
    // already on the clipboard at connect is deliberately never sent, so a
    // transfer attempted too early would be suppressed and read as a miss.
    std::thread::sleep(Duration::from_secs(15));

    // **Refuse to start if the host generator is not visibly running.**
    //
    // Without this the run reports host->mac misses that are nothing to do with
    // the clipboard, and two in a row is a wedge — so a soak with no generator
    // declares the exact failure it exists to detect. That happened three times
    // in shakedown: twice from timing, once because the generator's launch had
    // silently failed (its log file was still held by the previous instance,
    // so the shell redirect could not open and the process never started).
    //
    // The same lesson as the clipboard holder that reported success while
    // holding nothing: **a fixture that is not doing its job must say so, or
    // every conclusion drawn downstream is void.**
    let probe_deadline = host_period + HOST_ARRIVAL_SLACK;
    println!(
        "soak: checking the host generator is running (up to {}s)…",
        probe_deadline.as_secs()
    );
    let mut probe_seen = pbpaste_soak_seq().unwrap_or(0);
    if await_mac(&mut probe_seen, Instant::now(), probe_deadline) == Outcome::NeverArrived {
        let _ = session.kill();
        let _ = session.wait();
        return Err(format!(
            "no host clipboard nonce arrived within {}s, so `clipboard-cycle` is not running on \
             {host}'s console session. Refusing to start: the run would report host->mac misses \
             that mean nothing, and two in a row would be reported as a wedge.",
            probe_deadline.as_secs()
        ));
    }
    println!("soak: host generator confirmed (seq {probe_seen})");
    host_seen = probe_seen;

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

        // **host->mac FIRST, then mac->host.** The order is the staggering.
        //
        // The host->mac wait returns the moment the generator writes, so doing
        // the mac->host check immediately afterwards puts it as far from the
        // generator's *next* write as the period allows. The other order
        // measured one false miss in an eight-cycle smoke run: the generator
        // overwrote the host clipboard between the copy and the check, which is
        // exactly the value the check reads. Over eight hours that lands two in
        // a row eventually, and two in a row is a wedge.
        //
        // The generator period must also exceed `CYCLE` — see the README — so
        // there is at most one host write per cycle to be far away from.
        for direction in [Direction::HostToMac, Direction::MacToHost] {
            seq += 1;
            let attempt = transfer(
                &mdrdp,
                host,
                ssh_user,
                direction,
                seq,
                began,
                &mut host_seen,
                host_deadline,
            );
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
                let attempt = transfer(
                    &mdrdp,
                    host,
                    ssh_user,
                    Direction::MacToHost,
                    seq,
                    began,
                    &mut host_seen,
                    host_deadline,
                );
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
#[allow(clippy::too_many_arguments)]
fn transfer(
    mdrdp: &str,
    host: &str,
    ssh_user: Option<&str>,
    direction: Direction,
    seq: u64,
    began: Instant,
    host_seen: &mut u64,
    host_deadline: Duration,
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
        // Driven by `clipboard-cycle` on the host, launched once at session
        // start — setting the host clipboard needs something in its console
        // session, and a soak must not type on the desktop for hours.
        //
        // **Arrival only, no latency.** The generator deliberately stamps no
        // wall-clock time, because the two machines' clocks are only as aligned
        // as NTP has left them and a few hundred milliseconds of skew would sit
        // inside the range being measured. A number that is wrong is worse than
        // one that is absent.
        Direction::HostToMac => await_mac(host_seen, sent, host_deadline),
    };

    Attempt {
        direction,
        at_secs,
        outcome,
        nonce,
    }
}

/// Wait for the host's generator to land a NEW nonce on the Mac's pasteboard.
///
/// `host_seen` is the highest sequence number already observed; a payload must
/// be strictly newer to count, so a value still sitting there from last cycle
/// cannot be mistaken for a fresh arrival. That mistake would turn a wedged
/// host->Mac direction into a run of apparent successes.
fn await_mac(host_seen: &mut u64, sent: Instant, deadline: Duration) -> Outcome {
    while sent.elapsed() < deadline {
        if let Some(seq) = pbpaste_soak_seq() {
            if seq > *host_seen {
                *host_seen = seq;
                return Outcome::Arrived(sent.elapsed().as_secs_f64() * 1000.0);
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Outcome::NeverArrived
}

/// The sequence number on the Mac's pasteboard, if it holds one of ours.
fn pbpaste_soak_seq() -> Option<u64> {
    let out = Command::new("pbpaste").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.trim().strip_prefix("SOAKHOST-")?.parse().ok()
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

/// What each direction's figures actually measure. Printed every run, because
/// a number without its caveat is the one that gets quoted.
fn latency_caveat(direction: Direction) -> &'static str {
    match direction {
        Direction::MacToHost => {
            "(UPPER BOUND — each check spawns ssh, worth ~180 ms before it asks anything)"
        }
        Direction::HostToMac => {
            "(NOT clipboard latency — dominated by the wait for the host generator's next write)"
        }
    }
}

fn print_summary(attempts: &[Attempt], latencies: &[f64], rss: &[f64]) {
    println!("\n--- soak summary ---");
    let exercised = attempts
        .iter()
        .filter(|a| a.outcome != Outcome::NotExercised)
        .count();
    let _ = latencies;
    println!("attempted {exercised} (of {} cycle slots)", attempts.len());

    // **Per direction, never pooled.** The two measure different things, and a
    // single distribution over both describes neither: a smoke run produced a
    // combined p95 of 19.7 s, which was the host generator's period showing up
    // as though it were clipboard latency.
    for direction in [Direction::MacToHost, Direction::HostToMac] {
        let samples: Vec<f64> = attempts
            .iter()
            .filter(|a| a.direction == direction)
            .filter_map(|a| a.outcome.latency_ms())
            .collect();
        match report::latencies(&samples) {
            Some(l) => {
                println!(
                    "{}: n={} min={:.0}ms median={:.0}ms p95={:.0}ms max={:.0}ms",
                    direction.name(),
                    l.n,
                    l.min_ms,
                    l.median_ms,
                    l.p95_ms,
                    l.max_ms
                );
                println!("  {}", latency_caveat(direction));
            }
            // Never zeroes: an absent distribution must not look like a fast one.
            None => println!("{}: no samples", direction.name()),
        }
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
        "latency_mac_to_host": report::latencies(
            &attempts
                .iter()
                .filter(|a| a.direction == Direction::MacToHost)
                .filter_map(|a| a.outcome.latency_ms())
                .collect::<Vec<_>>(),
        ),
        "latency_host_to_mac": report::latencies(
            &attempts
                .iter()
                .filter(|a| a.direction == Direction::HostToMac)
                .filter_map(|a| a.outcome.latency_ms())
                .collect::<Vec<_>>(),
        ),
        "latency_host_to_mac_note":
            "NOT clipboard latency — dominated by the wait for the host generator's next write",
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
