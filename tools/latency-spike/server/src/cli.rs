//! Hand-rolled argument parsing, in the style of `src/bin/probe.rs`.
//!
//! No clap: this crate is meant to cross-compile with nothing in the tree that
//! cannot be read in an afternoon, and the flag surface is nine options wide.

/// Where the host's audio comes from.
///
/// `loopback` is the real source. It reports "unavailable" and stays healthy on a
/// host with no render endpoint, which is every fleet host today.
/// A flag value that silently produced nothing would be worse than no flag: it
/// would look like a working configuration and sound like a broken product.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioKind {
    /// Capture nothing. The default.
    Off,
    /// A synthetic two-tone generator, for proving the path end to end.
    Tone,
    /// WASAPI loopback from the default render endpoint — the real source.
    ///
    /// **Never executed on any fleet host**: neither has a render endpoint, so
    /// selecting this today yields "unavailable" and silence. It is a real
    /// option rather than a hidden one because a host that grows an endpoint
    /// should need no new build to use it.
    Loopback,
}

impl AudioKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AudioKind::Off => "off",
            AudioKind::Tone => "tone",
            AudioKind::Loopback => "loopback",
        }
    }
}

/// Where frames come from. `dxgi` is the default until the IDD source is proven
/// against the gate (HLD decision 7) — it works against any output on any host,
/// where `idd` needs our own driver installed and running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// DXGI Desktop Duplication.
    Dxgi,
    /// The IddCx driver's shared texture pool.
    Idd,
}

impl Source {
    /// The name the flag takes and the stats header reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dxgi => "dxgi",
            Self::Idd => "idd",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Index into the flat list printed by `--list-outputs`. Meaningful only for
    /// `--source dxgi`: the IDD pool is found by name, not by output index.
    pub output: usize,
    pub video_port: u16,
    pub input_port: u16,
    /// The auxiliary channel's port: clipboard now, audio in tranche 6.
    ///
    /// `0` means "do not serve it", which is how a run can opt out without a
    /// separate flag — and how the header knows not to advertise it.
    pub aux_port: u16,
    pub bitrate_kbps: u32,
    pub gop: u32,
    pub out: Option<String>,
    pub list_outputs: bool,
    /// Send small dirty regions as raw BGRA rects beside the HEVC stream. On by
    /// default; `--no-rects` turns it off to give a measurement its control arm.
    pub rects: bool,
    /// Verify a missed fast-path predicate by diffing the frame against the
    /// previous one, when the frame follows an idle gap (HLD §6b). On by default;
    /// `--no-diff` turns it off to give that measurement its own control arm. It
    /// rides the rect wire path, so `--no-rects` disables it too.
    pub diff: bool,
    /// Which capture source to run.
    pub source: Source,
    /// Where audio comes from, if the client asks for any.
    pub audio_source: AudioKind,
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
/// The auxiliary channel's default port. `0` on the command line turns it off.
pub const DEFAULT_AUX_PORT: u16 = 9503;

pub const DECLARED_FPS: u32 = 60;

impl Default for Config {
    fn default() -> Self {
        Self {
            output: 0,
            video_port: DEFAULT_VIDEO_PORT,
            input_port: DEFAULT_INPUT_PORT,
            aux_port: DEFAULT_AUX_PORT,
            bitrate_kbps: DEFAULT_BITRATE_KBPS,
            gop: DEFAULT_GOP,
            out: None,
            list_outputs: false,
            rects: true,
            diff: true,
            source: Source::Dxgi,
            // Off by default. Audio is opt-in on the host as well as
            // client-requested: nothing should start capturing because a binary
            // was launched.
            audio_source: AudioKind::Off,
        }
    }
}

pub fn usage() -> &'static str {
    "usage:\n  \
     rhydra-server --output N [--video-port 9500] [--input-port 9501]\n               \
     [--aux-port 9503 | --aux-port 0 to disable]\n               \
                  [--bitrate-kbps 20000] [--gop 120] [--out FILE.jsonl]\n               \
                  [--no-rects] [--no-diff] [--source dxgi|idd]\n  \
     [--audio-source off|tone|loopback]\n  \
     rhydra-server --source idd [--video-port 9500] ...\n  \
     rhydra-server --list-outputs\n\n\
     --no-rects withholds the raw dirty-rect fast path, forcing every update down\n  \
     the HEVC-only path. That is the control arm for a measurement, not a tuning\n  \
     knob: quote it whenever a figure is compared against the hybrid wire.\n\n\
     --no-diff withholds the Increment 3 pixel-diff fast path; metadata-driven\n  \
     rects still run. That is the control arm for the diff's own A/B, so quote it\n  \
     whenever an idle-regime figure is compared against a diffing server.\n\n\
     --source idd reads the mdrdp-idd driver's shared texture pool instead of\n  \
     Desktop Duplication, removing duplication's present-to-acquire gap. It needs\n  \
     the driver installed and started, and it takes no --output: the pool is found\n  \
     by name. --source dxgi is the default and the fallback.\n\n\
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
        if flag == "--no-diff" {
            cfg.diff = false;
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
            "--aux-port" => {
                cfg.aux_port = value
                    .parse()
                    .map_err(|e| format!("--aux-port {value:?}: {e}"))?
            }
            "--bitrate-kbps" => {
                cfg.bitrate_kbps = value
                    .parse()
                    .map_err(|e| format!("--bitrate-kbps {value:?}: {e}"))?
            }
            "--gop" => cfg.gop = value.parse().map_err(|e| format!("--gop {value:?}: {e}"))?,
            "--out" => cfg.out = Some(value.clone()),
            "--audio-source" => {
                cfg.audio_source = match value.as_str() {
                    "off" => AudioKind::Off,
                    "tone" => AudioKind::Tone,
                    "loopback" => AudioKind::Loopback,
                    other => {
                        return Err(format!(
                            "--audio-source {other:?}: expected off, tone or loopback"
                        ))
                    }
                }
            }
            "--source" => {
                cfg.source = match value.as_str() {
                    "dxgi" => Source::Dxgi,
                    "idd" => Source::Idd,
                    other => return Err(format!("--source {other:?}: expected dxgi or idd")),
                }
            }
            other => return Err(format!("unknown flag {other}\n{}", usage())),
        }
        i += 2;
    }

    if cfg.list_outputs {
        return Ok(cfg);
    }
    // The IDD source finds its pool through a named shared section, so there is no
    // output index for it to take. Requiring one would be a flag the operator has
    // to invent a value for, and a value the header would then report as if it
    // meant something.
    if !saw_output && cfg.source == Source::Dxgi {
        return Err(format!(
            "--output is required for --source dxgi (run --list-outputs to find the index)\n{}",
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
    // No `aux_port != 0` guard: the check above has already refused a zero video
    // or input port, so the documented "off" value cannot collide with either.
    if cfg.aux_port == cfg.video_port || cfg.aux_port == cfg.input_port {
        return Err(format!(
            "--aux-port {} collides with the video or input port; they are separate connections",
            cfg.aux_port
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
        // flag never silently produces the control arm's numbers. Same for the
        // pixel diff riding on top of it.
        assert!(cfg.rects);
        assert!(cfg.diff);
        // Duplication is the default until the IDD source passes the gate, so a
        // forgotten `--source` measures the proven path, not the new one.
        assert_eq!(cfg.source, Source::Dxgi);
    }

    #[test]
    fn every_flag_lands_in_its_own_field() {
        // All five numeric values distinct: a fixture reusing one number could not
        // catch two flags writing the same field. `--no-rects` and `--no-diff` sit
        // in the middle of the list, where a bare flag that wrongly consumed a value
        // would derail every flag after it.
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
            "--no-diff",
            "--gop",
            "30",
            "--source",
            "idd",
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
        assert!(!cfg.diff);
        assert_eq!(cfg.source, Source::Idd);
    }

    #[test]
    fn the_source_flag_takes_both_names_and_refuses_anything_else() {
        assert_eq!(
            parse(&args(&["--output", "0", "--source", "dxgi"]))
                .unwrap()
                .source,
            Source::Dxgi
        );
        assert_eq!(
            parse(&args(&["--output", "0", "--source", "idd"]))
                .unwrap()
                .source,
            Source::Idd
        );
        // Not silently defaulted: a typo that fell back to dxgi would produce a
        // control-arm measurement labelled as the IDD arm.
        // Every documented value round-trips, and an undocumented one is
        // refused. This test exists because the `loopback` arm was added to the
        // enum and to `as_str` but NOT to the parser -- a silent no-op in a
        // scripted edit -- and nothing caught it until a live host rejected the
        // flag and the supervised agent went into a respawn loop.
        for (text, kind) in [
            ("off", AudioKind::Off),
            ("tone", AudioKind::Tone),
            ("loopback", AudioKind::Loopback),
        ] {
            let cfg = parse(&args(&["--source", "idd", "--audio-source", text])).unwrap();
            assert_eq!(cfg.audio_source, kind, "--audio-source {text} must parse");
            assert_eq!(kind.as_str(), text, "as_str must round-trip {text}");
        }
        let bad = parse(&args(&["--source", "idd", "--audio-source", "loopbak"])).unwrap_err();
        assert!(bad.contains("expected off, tone or loopback"), "{bad}");

        let err = parse(&args(&["--output", "0", "--source", "iddcx"])).unwrap_err();
        assert!(err.contains("expected dxgi or idd"), "{err}");
        assert_eq!(Source::Dxgi.as_str(), "dxgi");
        assert_eq!(Source::Idd.as_str(), "idd");
    }

    #[test]
    fn the_idd_source_needs_no_output_index_but_dxgi_still_does() {
        // The pool is found by name, so there is no index to give — and no index to
        // report in the header as if it meant something.
        let cfg = parse(&args(&["--source", "idd"])).unwrap();
        assert_eq!(cfg.source, Source::Idd);
        let err = parse(&args(&["--source", "dxgi"])).unwrap_err();
        assert!(err.contains("--output is required"), "{err}");
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
        // `--no-rects` disables the wire path the diff rides on, but it is not the
        // diff's own flag: the two arms must stay separable in the config, or an
        // A/B of one silently varies the other.
        assert!(cfg.diff);
    }

    #[test]
    fn no_diff_turns_the_pixel_diff_off_and_leaves_the_metadata_rects_alone() {
        // The trailing `--out` proves the cursor advanced by one, as for
        // `--no-rects` above.
        let cfg = parse(&args(&[
            "--output",
            "0",
            "--no-diff",
            "--out",
            "/tmp/z.jsonl",
        ]))
        .unwrap();
        assert!(!cfg.diff);
        // Metadata-driven rects still run: that is what makes `--no-diff` the
        // diff's control arm rather than a second `--no-rects`.
        assert!(cfg.rects);
        assert_eq!(cfg.out.as_deref(), Some("/tmp/z.jsonl"));
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
    fn the_aux_port_defaults_on_and_can_be_turned_off_with_zero() {
        // Zero is the documented "off". It is also what the header reads to
        // decide whether to advertise the channel at all, so it has to be a
        // value the parser accepts rather than a separate flag.
        assert_eq!(Config::default().aux_port, DEFAULT_AUX_PORT);
        let cfg = parse(&args(&["--source", "idd", "--aux-port", "0"])).unwrap();
        assert_eq!(cfg.aux_port, 0);
    }

    #[test]
    fn an_aux_port_colliding_with_another_channel_is_refused() {
        for other in ["--video-port", "--input-port"] {
            let cfg = parse(&args(&[
                "--source",
                "idd",
                other,
                "19000",
                "--aux-port",
                "19000",
            ]));
            assert!(
                cfg.is_err(),
                "--aux-port sharing {other} should be refused, got {cfg:?}"
            );
        }
        // …and zero, the "off" value, collides with nothing — which holds only
        // because a zero video or input port is refused before this check.
        assert!(parse(&args(&["--source", "idd", "--aux-port", "0"])).is_ok());
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
