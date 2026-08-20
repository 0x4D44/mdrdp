//! Hand-rolled argument parsing, in the style of `../server/src/cli.rs` and
//! `src/bin/probe.rs`.
//!
//! Both endpoints are parsed as literal [`SocketAddr`]s rather than host strings.
//! That is not laziness: the server binds loopback only and is reached through an SSH
//! tunnel, so the address is always `127.0.0.1:<port>`. Anything else is a mistake
//! worth refusing at startup instead of discovering as a connection timeout.

use std::net::SocketAddr;

pub const DEFAULT_VIDEO_ADDR: &str = "127.0.0.1:9500";
pub const DEFAULT_INPUT_ADDR: &str = "127.0.0.1:9501";
pub const DEFAULT_TITLE: &str = "spike-viewer";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Video channel — framed HEVC access units and server stats lines.
    pub connect: SocketAddr,
    /// Keystroke channel — 8-byte records, viewer to server.
    pub input: SocketAddr,
    /// Client-side stats JSONL. Without it nothing is recorded and the viewer is
    /// just a picture.
    pub out: Option<String>,
    pub title: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            connect: DEFAULT_VIDEO_ADDR.parse().expect("literal address"),
            input: DEFAULT_INPUT_ADDR.parse().expect("literal address"),
            out: None,
            title: DEFAULT_TITLE.to_owned(),
        }
    }
}

pub fn usage() -> &'static str {
    "usage:\n  \
     spike-viewer [--connect 127.0.0.1:9500] [--input 127.0.0.1:9501]\n               \
                  [--out FILE.jsonl] [--title STR]\n\n\
     Both ports are the local ends of an SSH tunnel to the spike server:\n  \
     ssh -L 9500:127.0.0.1:9500 -L 9501:127.0.0.1:9501 user@host"
}

pub fn parse(args: &[String]) -> Result<Config, String> {
    let mut cfg = Config::default();
    let mut i = 0usize;

    while i < args.len() {
        let flag = args[i].as_str();
        if flag == "--help" || flag == "-h" {
            return Err(usage().to_owned());
        }
        // Recognised before the value is demanded: a typo'd flag at the end of the
        // command line otherwise reports "needs a value", which sends the reader
        // hunting for a missing argument instead of at the typo.
        if !matches!(flag, "--connect" | "--input" | "--out" | "--title") {
            return Err(format!("unknown flag {flag}\n{}", usage()));
        }
        let value = args
            .get(i + 1)
            .ok_or_else(|| format!("{flag} needs a value\n{}", usage()))?;
        match flag {
            "--connect" => cfg.connect = parse_addr("--connect", value)?,
            "--input" => cfg.input = parse_addr("--input", value)?,
            "--out" => cfg.out = Some(value.clone()),
            "--title" => cfg.title = value.clone(),
            other => unreachable!("{other} passed the recognised-flag check above"),
        }
        i += 2;
    }

    if cfg.connect == cfg.input {
        return Err(format!(
            "--connect and --input are both {}; they are separate connections",
            cfg.connect
        ));
    }
    Ok(cfg)
}

fn parse_addr(flag: &str, value: &str) -> Result<SocketAddr, String> {
    value.parse().map_err(|e| {
        format!("{flag} {value:?}: {e} (an ADDRESS:PORT literal is required, not a host name)")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn the_defaults_are_the_documented_ones() {
        let cfg = parse(&args(&[])).unwrap();
        assert_eq!(cfg.connect.port(), 9500);
        assert_eq!(cfg.input.port(), 9501);
        assert!(cfg.connect.ip().is_loopback());
        assert_eq!(cfg.out, None);
        assert_eq!(cfg.title, "spike-viewer");
    }

    #[test]
    fn every_flag_lands_in_its_own_field() {
        // Every value distinct: a fixture that reused one could not catch two flags
        // writing the same field.
        let cfg = parse(&args(&[
            "--connect",
            "127.0.0.1:19500",
            "--input",
            "127.0.0.1:19501",
            "--out",
            "/tmp/client.jsonl",
            "--title",
            "idd run 3",
        ]))
        .unwrap();
        assert_eq!(cfg.connect.port(), 19500);
        assert_eq!(cfg.input.port(), 19501);
        assert_eq!(cfg.out.as_deref(), Some("/tmp/client.jsonl"));
        assert_eq!(cfg.title, "idd run 3");
    }

    #[test]
    fn a_host_name_is_refused_with_the_reason() {
        let err = parse(&args(&["--connect", "quench:9500"])).unwrap_err();
        assert!(err.contains("not a host name"), "{err}");
    }

    #[test]
    fn a_flag_with_no_value_is_refused_rather_than_defaulted() {
        let err = parse(&args(&["--out"])).unwrap_err();
        assert!(err.contains("needs a value"), "{err}");
    }

    #[test]
    fn an_unknown_flag_is_refused() {
        let err = parse(&args(&["--fullscreen"])).unwrap_err();
        assert!(err.contains("unknown flag --fullscreen"), "{err}");
    }

    #[test]
    fn identical_endpoints_are_refused() {
        let err = parse(&args(&[
            "--connect",
            "127.0.0.1:9500",
            "--input",
            "127.0.0.1:9500",
        ]))
        .unwrap_err();
        assert!(err.contains("separate connections"), "{err}");
    }
}
