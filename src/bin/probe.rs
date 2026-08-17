//! `probe` — the credential-free half of the mdrdp measurement harness.
//!
//! Everything here works against a live RDP server with no credential, because the
//! X.224 security negotiation happens before authentication and a TCP handshake needs
//! nobody's permission.
//!
//!     probe stages <host> [port]      how far a connection gets, without a credential
//!     probe negotiate <host> [port]
//!     probe rtt <host> [--samples N] [--interval-ms N] [--out FILE]
//!     probe glass --region X,Y,WxH    keypress-to-photon latency (macOS only)
//!     probe summarise <file>

use mdrdp::probe::{negotiation, rtt, stats, wire};
use mdrdp::trust;
use serde_json::json;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::ExitCode;
use std::time::{Duration, Instant};

const DEFAULT_PORT: u16 = 3389;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Wall-clock budget for reading one response, enforced across the whole message.
const READ_DEADLINE: Duration = Duration::from_secs(5);

fn usage() -> &'static str {
    "usage:\n  \
     probe stages <host> [port]\n  \
     probe negotiate <host> [port]\n  \
     probe rtt <host> [--port N] [--samples N] [--interval-ms N] [--out FILE]\n  \
     probe glass --region X,Y,WxH [--samples N] [--key CODE] [--erase-key CODE]\n              \
                 [--fps N] [--settle-ms N] [--quiet-ms N] [--gap-ms N] [--jitter-ms N]\n              \
                 [--threshold N] [--min-pixels N] [--timeout-ms N] [--target-pid N]\n              \
                 [--out FILE]\n              \
                 [--note key=value]...\n  \
     probe glass --locate\n  \
     probe summarise <file>"
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("stages") => cmd_stages(&args[1..]),
        Some("negotiate") => cmd_negotiate(&args[1..]),
        Some("rtt") => cmd_rtt(&args[1..]),
        Some("glass") => match cmd_glass(&args[1..]) {
            Ok(code) => return code,
            Err(e) => Err(e),
        },
        Some("summarise") | Some("summarize") => cmd_summarise(&args[1..]),
        _ => {
            eprintln!("{}", usage());
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Keypress-to-photon latency.
///
/// Returns its own exit code because the failure modes are not all the same: a missing
/// privacy grant exits 1 with instructions, and an unsupported platform exits 2, the
/// same as a usage error.
#[cfg(target_os = "macos")]
fn cmd_glass(args: &[String]) -> Result<ExitCode, Box<dyn std::error::Error>> {
    use mdrdp::probe::glass;

    if args.iter().any(|a| a == "--locate") {
        glass::locate()?;
        return Ok(ExitCode::SUCCESS);
    }

    // The region is required and cannot be defaulted: there is no sane guess at which
    // part of the screen the operator wants watched.
    let region_index = args
        .iter()
        .position(|a| a == "--region")
        .ok_or_else(|| format!("glass needs --region X,Y,WxH (or --locate)\n{}", usage()))?;
    let region = glass::Region::parse(
        args.get(region_index + 1)
            .ok_or("--region needs a value")?
            .as_str(),
    )?;

    let mut cfg = glass::Config::new(region);
    let mut i = 0;
    while i < args.len() {
        let value = || -> Result<&String, String> {
            args.get(i + 1)
                .ok_or_else(|| format!("{} needs a value", args[i]))
        };
        match args[i].as_str() {
            "--region" => {}
            "--samples" => cfg.samples = value()?.parse()?,
            "--key" => cfg.key = value()?.parse()?,
            "--erase-key" => cfg.erase_key = value()?.parse()?,
            "--fps" => cfg.fps = Some(value()?.parse()?),
            "--settle-ms" => cfg.settle_ms = value()?.parse()?,
            "--quiet-ms" => cfg.quiet_ms = value()?.parse()?,
            "--gap-ms" => cfg.gap_ms = value()?.parse()?,
            "--jitter-ms" => cfg.jitter_ms = value()?.parse()?,
            "--threshold" => cfg.threshold = value()?.parse()?,
            "--min-pixels" => cfg.min_pixels = value()?.parse()?,
            "--timeout-ms" => cfg.timeout_ms = value()?.parse()?,
            "--target-pid" => cfg.target_pid = Some(value()?.parse()?),
            "--out" => cfg.out = Some(value()?.clone()),
            "--note" => {
                let note = value()?;
                let (k, v) = note
                    .split_once('=')
                    .ok_or_else(|| format!("--note must be key=value (got {note:?})"))?;
                cfg.notes.push((k.to_owned(), v.to_owned()));
            }
            other => return Err(format!("unknown flag {other}\n{}", usage()).into()),
        }
        i += 2;
    }

    match glass::run(&cfg) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(e) => {
            eprintln!("{e}");
            Ok(ExitCode::FAILURE)
        }
    }
}

/// The instrument is built on ScreenCaptureKit, the CoreMedia host clock and CGEvent
/// injection. There is no Windows equivalent that would produce a comparable number, so
/// this refuses rather than pretending.
#[cfg(not(target_os = "macos"))]
fn cmd_glass(_args: &[String]) -> Result<ExitCode, Box<dyn std::error::Error>> {
    eprintln!("glass: macOS only");
    Ok(ExitCode::from(2))
}

/// Send one connection request and report what the server answers.
fn probe_offer(addr: &SocketAddr, requested: u32) -> std::io::Result<serde_json::Value> {
    let mut stream = TcpStream::connect_timeout(addr, CONNECT_TIMEOUT)?;
    stream.set_nodelay(true)?;

    stream.write_all(&negotiation::connection_request(requested))?;

    // The deadline covers the whole response. Per-syscall timeouts do not bound a
    // trickling peer; see probe::wire.
    let response = wire::read_tpkt(&mut stream, Instant::now() + READ_DEADLINE)?;

    let parsed = negotiation::parse_connection_confirm(&response);
    Ok(match parsed {
        Ok(outcome) => json!({
            "requested_protocols": format!("0x{requested:08x}"),
            "verdict": outcome,
            "requires_nla": outcome.requires_nla(),
            "raw_response_hex": hex(&response),
        }),
        Err(e) => json!({
            "requested_protocols": format!("0x{requested:08x}"),
            "verdict": { "outcome": "parse_error", "detail": e.to_string() },
            "raw_response_hex": hex(&response),
        }),
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Report how far a connection gets: TCP, X.224 negotiation, TLS — and stop.
///
/// Deliberately stops before CredSSP. It sends no credential and holds no session, so it
/// is safe to run against a host that is already refusing logons — which is exactly when
/// you need it. A host whose TCP, X.224 and TLS are all healthy while authentication
/// hangs has a wedged logon path, not a network or credential problem.
fn cmd_stages(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let host = args.first().ok_or("stages needs a host")?;
    let port: u16 = match args.get(1) {
        Some(p) => p.parse()?,
        None => DEFAULT_PORT,
    };
    let addr = rtt::resolve(host, port)?;

    let mut stages = Vec::new();
    let mut mark = |name: &str, started: Instant, detail: Option<String>| {
        stages.push(json!({
            "stage": name,
            "elapsed_ms": (started.elapsed().as_micros() as f64) / 1000.0,
            "detail": detail,
        }));
    };

    let t = Instant::now();
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
    stream.set_nodelay(true)?;
    mark("tcp_connect", t, Some(addr.to_string()));

    let t = Instant::now();
    stream.write_all(&negotiation::connection_request(
        negotiation::PROTOCOL_ALL_MODERN,
    ))?;
    let response = wire::read_tpkt(&mut stream, Instant::now() + READ_DEADLINE)?;
    let outcome = negotiation::parse_connection_confirm(&response)?;
    let requires_nla = outcome.requires_nla();
    mark("x224_negotiation", t, Some(format!("{outcome:?}")));

    // TLS only if the server actually wants it.
    if requires_nla {
        let t = Instant::now();
        let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
        let target = format!("{host}:{port}");
        let store = trust::KnownHosts::load(&trust::KnownHosts::default_path()?)?;
        let verifier = std::sync::Arc::new(trust::TofuVerifier::new(
            &target,
            trust::KnownHosts::default_path()?,
            store,
            std::sync::Arc::clone(&provider),
        ));
        let mut config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        config.resumption = rustls::client::Resumption::disabled();

        let server_name = rustls::pki_types::ServerName::try_from(host.clone())?;
        let conn = rustls::ClientConnection::new(std::sync::Arc::new(config), server_name)?;
        let mut tls = rustls::StreamOwned::new(conn, stream);
        tls.flush()?;

        let detail = format!(
            "{} {}",
            tls.conn
                .protocol_version()
                .map(|v| format!("{v:?}"))
                .unwrap_or_else(|| "?".to_owned()),
            tls.conn
                .negotiated_cipher_suite()
                .map(|s| format!("{:?}", s.suite()))
                .unwrap_or_else(|| "?".to_owned())
        );
        mark("tls_handshake", t, Some(detail));
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "target": { "host": host, "port": port, "resolved": addr.to_string() },
            "stages": stages,
            "note": "stops before CredSSP - no credential sent, no session held. \
                     All green here while authentication hangs means a wedged logon path.",
        }))?
    );
    Ok(())
}

fn cmd_negotiate(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let host = args.first().ok_or("negotiate needs a host")?;
    let port: u16 = match args.get(1) {
        Some(p) => p.parse()?,
        None => DEFAULT_PORT,
    };
    let addr = rtt::resolve(host, port)?;

    // Two offers: what a modern client sends, and legacy security. The pair reveals
    // whether the server merely prefers NLA or requires it.
    let modern = probe_offer(&addr, negotiation::PROTOCOL_ALL_MODERN)?;
    let legacy = probe_offer(&addr, negotiation::PROTOCOL_RDP)?;

    let report = json!({
        "target": { "host": host, "port": port, "resolved": addr.to_string() },
        "offers": [modern, legacy],
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn cmd_rtt(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let host = args.first().ok_or("rtt needs a host")?;
    let mut port = DEFAULT_PORT;
    let mut count = 100usize;
    let mut interval_ms = 50u64;
    let mut out: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        let value = || -> Result<&String, String> {
            args.get(i + 1)
                .ok_or_else(|| format!("{} needs a value", args[i]))
        };
        match args[i].as_str() {
            "--port" => port = value()?.parse()?,
            "--samples" => count = value()?.parse()?,
            "--interval-ms" => interval_ms = value()?.parse()?,
            "--out" => out = Some(value()?.clone()),
            other => return Err(format!("unknown flag {other}\n{}", usage()).into()),
        }
        i += 2;
    }

    let addr = rtt::resolve(host, port)?;
    let (samples, errors) = rtt::sample_batch(
        &addr,
        count,
        Duration::from_millis(interval_ms),
        CONNECT_TIMEOUT,
    );

    if let Some(path) = &out {
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        for s in &samples {
            writeln!(file, "{}", serde_json::to_string(s)?)?;
        }
    }

    let micros: Vec<u64> = samples.iter().map(|s| s.micros).collect();
    let report = json!({
        "target": { "host": host, "port": port, "resolved": addr.to_string() },
        "requested": count,
        "collected": samples.len(),
        "failed": errors.len(),
        "first_error": errors.first().map(|e| e.to_string()),
        "appended_to": out,
        "batch_summary": stats::summarise(&micros),
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn cmd_summarise(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let path = args.first().ok_or("summarise needs a file")?;

    // Which file this is, is decided by what is in it, not by its name. A glass file
    // carries `type` on every record; an rtt file carries bare `{unix_ms, micros}`.
    #[cfg(target_os = "macos")]
    if let Some(report) = mdrdp::probe::glass::resummarise(path)? {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    let reader = BufReader::new(File::open(path)?);

    let mut samples = Vec::new();
    let mut malformed = 0usize;
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<rtt::Sample>(&line) {
            Ok(s) => samples.push(s),
            Err(_) => malformed += 1,
        }
    }

    let micros: Vec<u64> = samples.iter().map(|s| s.micros).collect();
    let coverage = rtt::coverage(&samples);

    // State plainly whether the baseline requirement is met. A summary that quietly
    // reports percentiles over an inadequate sample is how a provisional number becomes
    // gospel.
    let report = json!({
        "source": path,
        "malformed_lines": malformed,
        "coverage": coverage,
        "summary_micros": stats::summarise(&micros),
        "baseline_status": if coverage.meets_requirement {
            "MEETS the baseline requirement (>=1000 samples, >=3 distinct UTC hours, \
             >=6h wall-clock span)"
        } else {
            "PROVISIONAL - does not meet the baseline requirement (>=1000 samples, \
             >=3 distinct UTC hours, >=6h wall-clock span). Distinct hour labels alone \
             are not a spread: batches run either side of an hour boundary tick new \
             labels without sampling a different time of day."
        },
        "span_hours": format!("{:.2}", coverage.span_ms as f64 / 3_600_000.0),
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
