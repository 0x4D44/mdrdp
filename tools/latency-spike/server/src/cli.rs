//! Hand-rolled argument parsing, in the style of `src/bin/probe.rs`.
//!
//! No clap: this crate is meant to cross-compile with nothing in the tree that
//! cannot be read in an afternoon, and the flag surface is nine options wide.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Index into the flat list printed by `--list-outputs`.
    pub output: usize,
    pub video_port: u16,
    pub input_port: u16,
    pub bitrate_kbps: u32,
    pub gop: u32,
    pub out: Option<String>,
    pub list_outputs: bool,
    /// Send small dirty regions as raw BGRA rects beside the H.264 stream. On by
    /// default; `--no-rects` turns it off to give a measurement its control arm.
    pub rects: bool,
}

pub const DEFAULT_VIDEO_PORT: u16 = 9500;
pub const DEFAULT_INPUT_PORT: u16 = 9501;
pub const DEFAULT_BITRATE_KBPS: u32 = 20_000;
pub const DEFAULT_GOP: u32 = 120;

/// Frame rate declared to Media Foundation and to the video processor.
///
/// Desktop Duplication has no frame rate of its own — it hands over whatever the
/// compositor presented — so this is a rate *hint* for the encoder's rate control,
/// not a cadence the server enforces.
pub const DECLARED_FPS: u32 = 60;

impl Default for Config {
    fn default() -> Self {
        Self {
            output: 0,
            video_port: DEFAULT_VIDEO_PORT,
            input_port: DEFAULT_INPUT_PORT,
            bitrate_kbps: DEFAULT_BITRATE_KBPS,
            gop: DEFAULT_GOP,
            out: None,
            list_outputs: false,
            rects: true,
        }
    }
}

pub fn usage() -> &'static str {
    "usage:\n  \
     spike-server --output N [--video-port 9500] [--input-port 9501]\n               \
                  [--bitrate-kbps 20000] [--gop 120] [--out FILE.jsonl]\n               \
                  [--no-rects]\n  \
     spike-server --list-outputs\n\n\
     --no-rects withholds the raw dirty-rect fast path, forcing every update down\n  \
     the H.264-only path. That is the control arm for a measurement, not a tuning\n  \
     knob: quote it whenever a figure is compared against the hybrid wire.\n\n\
     Both listeners bind 127.0.0.1 only. Reach them over an SSH tunnel:\n  \
     ssh -L 9500:127.0.0.1:9500 -L 9501:127.0.0.1:9501 user@host"
}

pub fn parse(args: &[String]) -> Result<Config, String> {
    let mut cfg = Config::default();
    let mut saw_output = false;
    let mut i = 0usize;

    while i < args.len() {
        let flag = args[i].as_str();
        // Bare flags first: they consume no value, so the cursor advances by one.
        if flag == "--list-outputs" {
            cfg.list_outputs = true;
            i += 1;
            continue;
        }
        if flag == "--no-rects" {
            cfg.rects = false;
            i += 1;
            continue;
        }
        if flag == "--help" || flag == "-h" {
            return Err(usage().to_owned());
        }
        let value = args
            .get(i + 1)
            .ok_or_else(|| format!("{flag} needs a value\n{}", usage()))?;
        match flag {
            "--output" => {
                cfg.output = value
                    .parse()
                    .map_err(|e| format!("--output {value:?}: {e}"))?;
                saw_output = true;
            }
            "--video-port" => {
                cfg.video_port = value
                    .parse()
                    .map_err(|e| format!("--video-port {value:?}: {e}"))?
            }
            "--input-port" => {
                cfg.input_port = value
                    .parse()
                    .map_err(|e| format!("--input-port {value:?}: {e}"))?
            }
            "--bitrate-kbps" => {
                cfg.bitrate_kbps = value
                    .parse()
                    .map_err(|e| format!("--bitrate-kbps {value:?}: {e}"))?
            }
            "--gop" => cfg.gop = value.parse().map_err(|e| format!("--gop {value:?}: {e}"))?,
            "--out" => cfg.out = Some(value.clone()),
            other => return Err(format!("unknown flag {other}\n{}", usage())),
        }
        i += 2;
    }

    if cfg.list_outputs {
        return Ok(cfg);
    }
    if !saw_output {
        return Err(format!(
            "--output is required (run --list-outputs to find the index)\n{}",
            usage()
        ));
    }
    if cfg.video_port == 0 || cfg.input_port == 0 {
        return Err("ports must be non-zero: an ephemeral port cannot be tunnelled".to_owned());
    }
    if cfg.video_port == cfg.input_port {
        return Err(format!(
            "--video-port and --input-port are both {}; they are separate connections",
            cfg.video_port
        ));
    }
    if cfg.bitrate_kbps == 0 {
        return Err("--bitrate-kbps must be non-zero".to_owned());
    }
    if cfg.gop == 0 {
        return Err(
            "--gop must be non-zero (use a large value for 'IDR only at start')".to_owned(),
        );
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn the_defaults_are_the_documented_ones() {
        let cfg = parse(&args(&["--output", "0"])).unwrap();
        assert_eq!(cfg.video_port, 9500);
        assert_eq!(cfg.input_port, 9501);
        assert_eq!(cfg.bitrate_kbps, 20_000);
        assert_eq!(cfg.gop, 120);
        assert_eq!(cfg.out, None);
        assert!(!cfg.list_outputs);
        // The fast path is the default: a run has to opt *out* of it, so a forgotten
        // flag never silently produces the control arm's numbers.
        assert!(cfg.rects);
    }

    #[test]
    fn every_flag_lands_in_its_own_field() {
        // All five numeric values distinct: a fixture reusing one number could not
        // catch two flags writing the same field. `--no-rects` sits in the middle of
        // the list, where a bare flag that wrongly consumed a value would derail
        // every flag after it.
        let cfg = parse(&args(&[
            "--output",
            "2",
            "--video-port",
            "19500",
            "--input-port",
            "19501",
            "--no-rects",
            "--bitrate-kbps",
            "8000",
            "--gop",
            "30",
            "--out",
            "/tmp/x.jsonl",
        ]))
        .unwrap();
        assert_eq!(cfg.output, 2);
        assert_eq!(cfg.video_port, 19500);
        assert_eq!(cfg.input_port, 19501);
        assert_eq!(cfg.bitrate_kbps, 8000);
        assert_eq!(cfg.gop, 30);
        assert_eq!(cfg.out.as_deref(), Some("/tmp/x.jsonl"));
        assert!(!cfg.rects);
    }

    #[test]
    fn no_rects_turns_the_fast_path_off_and_consumes_no_value() {
        // The trailing `--out` proves the cursor advanced by one: had `--no-rects`
        // eaten a value it would have swallowed `--out` and left `out` unset.
        let cfg = parse(&args(&[
            "--output",
            "0",
            "--no-rects",
            "--out",
            "/tmp/y.jsonl",
        ]))
        .unwrap();
        assert!(!cfg.rects);
        assert_eq!(cfg.out.as_deref(), Some("/tmp/y.jsonl"));
    }

    #[test]
    fn list_outputs_needs_no_output_index() {
        let cfg = parse(&args(&["--list-outputs"])).unwrap();
        assert!(cfg.list_outputs);
    }

    #[test]
    fn a_missing_output_index_is_refused() {
        let err = parse(&args(&[])).unwrap_err();
        assert!(err.contains("--output is required"), "{err}");
    }

    #[test]
    fn a_flag_with_no_value_is_refused_rather_than_defaulted() {
        let err = parse(&args(&["--output"])).unwrap_err();
        assert!(err.contains("needs a value"), "{err}");
    }

    #[test]
    fn an_unknown_flag_is_refused() {
        let err = parse(&args(&["--output", "0", "--bind", "0.0.0.0"])).unwrap_err();
        assert!(err.contains("unknown flag --bind"), "{err}");
    }

    #[test]
    fn identical_ports_are_refused() {
        let err = parse(&args(&[
            "--output",
            "0",
            "--video-port",
            "9500",
            "--input-port",
            "9500",
        ]))
        .unwrap_err();
        assert!(err.contains("separate connections"), "{err}");
    }

    #[test]
    fn zero_valued_numbers_are_refused() {
        for flag in ["--video-port", "--input-port", "--bitrate-kbps", "--gop"] {
            let err = parse(&args(&["--output", "0", flag, "0"])).unwrap_err();
            assert!(!err.is_empty(), "{flag} accepted 0");
        }
    }

    #[test]
    fn a_non_numeric_value_is_refused() {
        let err = parse(&args(&["--output", "left"])).unwrap_err();
        assert!(err.contains("--output"), "{err}");
    }
}
