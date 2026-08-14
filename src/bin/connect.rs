//! `connect` — authenticate to an RDP host and reach capability exchange.
//!
//! Emits a payload-free stage trace as JSON. The password is read from the OS keychain
//! at connect time; it is never an argument, an environment variable, or logged.
//!
//!     connect <host> --user <account> [--port N] [--domain D] [--size WxH]

use ironrdp::connector::DesktopSize;
use mdrdp::connect::{ConnectOptions, connect};
use mdrdp::creds;
use mdrdp::trust::KnownHosts;
use std::process::ExitCode;

fn usage() -> &'static str {
    "usage: connect <host> --user <account> [--port N] [--domain D] [--size WxH]"
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let host = args
        .first()
        .filter(|h| !h.starts_with("--"))
        .ok_or(usage())?
        .clone();

    let mut user: Option<String> = None;
    let mut port: u16 = 3389;
    let mut domain: Option<String> = None;
    let mut size = (1024u16, 768u16);

    let mut i = 1;
    while i < args.len() {
        let value = || -> Result<&String, String> {
            args.get(i + 1)
                .ok_or_else(|| format!("{} needs a value", args[i]))
        };
        match args[i].as_str() {
            "--user" => user = Some(value()?.clone()),
            "--port" => port = value()?.parse()?,
            "--domain" => domain = Some(value()?.clone()),
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').ok_or("--size wants WxH, e.g. 1024x768")?;
                size = (w.parse()?, h.parse()?);
            }
            other => return Err(format!("unknown flag {other}\n{}", usage()).into()),
        }
        i += 2;
    }

    let user = user.ok_or(usage())?;
    let known_hosts = KnownHosts::default_path()?;

    // Reading the keychain may prompt for permission the first time.
    let secret = creds::lookup(&user)?;

    let opts = ConnectOptions {
        host,
        port,
        username: user,
        domain,
        desktop_size: DesktopSize {
            width: size.0,
            height: size.1,
        },
        known_hosts,
    };

    let report = connect(&opts, &secret)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
