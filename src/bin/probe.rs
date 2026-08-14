//! `probe` — the credential-free half of the mdrdp measurement harness.
//!
//! Everything here works against a live RDP server with no credential, because the
//! X.224 security negotiation happens before authentication and a TCP handshake needs
//! nobody's permission.
//!
//!     probe negotiate <host> [port]
//!     probe rtt <host> [--samples N] [--interval-ms N] [--out FILE]
//!     probe summarise <file>

use mdrdp::probe::{negotiation, rtt, stats};
use serde_json::json;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

const DEFAULT_PORT: u16 = 3389;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(5);

fn usage() -> &'static str {
    "usage:\n  \
     probe negotiate <host> [port]\n  \
     probe rtt <host> [--port N] [--samples N] [--interval-ms N] [--out FILE]\n  \
     probe summarise <file>"
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("negotiate") => cmd_negotiate(&args[1..]),
        Some("rtt") => cmd_rtt(&args[1..]),
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

/// Read one complete TPKT-framed message.
///
/// A single `read` is not guaranteed to return the whole message, so the length is taken
/// from the header and the remainder read explicitly.
fn read_tpkt(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header)?;

    let declared = u16::from_be_bytes([header[2], header[3]]) as usize;
    if declared < header.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("TPKT declares {declared} bytes, shorter than its own header"),
        ));
    }

    let mut buf = header.to_vec();
    buf.resize(declared, 0);
    stream.read_exact(&mut buf[4..])?;
    Ok(buf)
}

/// Send one connection request and report what the server answers.
fn probe_offer(addr: &SocketAddr, requested: u32) -> io::Result<serde_json::Value> {
    let mut stream = TcpStream::connect_timeout(addr, CONNECT_TIMEOUT)?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_nodelay(true)?;

    stream.write_all(&negotiation::connection_request(requested))?;
    let response = read_tpkt(&mut stream)?;

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
            "MEETS the >=1000 sample / >=3 distinct UTC hour requirement"
        } else {
            "PROVISIONAL - does not meet the >=1000 sample / >=3 distinct UTC hour requirement"
        },
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
