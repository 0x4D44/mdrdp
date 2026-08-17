//! `probe glass` — keypress-to-photon latency, measured end to end.
//!
//! Every other number the harness produces is a *component*: the TCP handshake floor, a
//! decode time, a present time. This one is the number a user would recognise — the delay
//! between pressing a key and the character appearing on the glass — measured by posting
//! a synthetic keystroke and watching one small region of the screen until it changes.
//!
//! Three things make the result trustworthy, and all three are easy to get wrong:
//!
//! * **One clock.** The keystroke's timestamp and the frame's timestamp both come from
//!   the CoreMedia host clock. The frame time is ScreenCaptureKit's presentation
//!   timestamp — when the window server says the frame was shown — not when this process
//!   got around to reading it.
//! * **A quiet gate.** A trial only starts once the watched region has been still for
//!   `--quiet-ms`. Without it a blinking caret is timed instead of the keystroke, and the
//!   result is a confident number about nothing. Trials that never go quiet are recorded
//!   `poisoned` and excluded, not quietly dropped.
//! * **An honest uncertainty band.** Sampling at a frame interval `T` can only ever read
//!   *late*, by `T/2` on average and `T` at worst. The summary states the band next to
//!   the percentiles rather than leaving the reader to assume the number is exact.
//!
//! The operator focuses the target window and positions the region over the cell that
//! will change; the probe does not hunt for a window. `probe glass --locate` prints the
//! pointer position twice a second to make finding the coordinates a ten-second job.

pub mod capture;
pub mod detect;
pub mod inject;
pub mod wakelock;

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::probe::stats::{self, Summary};
use capture::{Capture, Sample};
use detect::{DetectConfig, SplitMix64, Step, Trial};
use inject::Injector;

/// How long the loop waits on the frame channel before deciding the window server has
/// nothing to show and synthesising a no-change tick. Short enough that the quiet gate
/// still advances on a completely still screen.
const POLL: Duration = Duration::from_millis(20);

/// Belt-and-braces bound on one trial's wall clock. The state machine's own deadlines
/// always fire first; this only catches a capture that has stopped talking entirely.
const TRIAL_WALL_LIMIT: Duration = Duration::from_secs(30);

/// The quiet gate gives up after this long and poisons the trial.
const QUIET_DEADLINE_MS: u64 = 5_000;

/// The measurement's own uncertainty, restated wherever a percentile is quoted.
const VRR_NOTE: &str = "fixed-refresh assumed; VRR/ProMotion state is not machine-readable \
                        — the operator must confirm it is disabled";

pub type Error = Box<dyn std::error::Error>;

/// A watched rectangle in display points, global coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Region {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Region {
    /// Parse `X,Y,WxH`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let bad = || format!("region must be X,Y,WxH (got {text:?})");
        let (x, rest) = text.split_once(',').ok_or_else(bad)?;
        let (y, size) = rest.split_once(',').ok_or_else(bad)?;
        let (w, h) = size.split_once(['x', 'X']).ok_or_else(bad)?;

        let num = |s: &str| s.trim().parse::<f64>().map_err(|_| bad());
        let region = Region {
            x: num(x)?,
            y: num(y)?,
            w: num(w)?,
            h: num(h)?,
        };
        if region.w <= 0.0 || region.h <= 0.0 {
            return Err(format!("region {region} has no area"));
        }
        Ok(region)
    }

    /// Whether this region lies wholly within `frame`.
    pub fn is_inside(&self, frame: objc2_core_foundation::CGRect) -> bool {
        self.x >= frame.origin.x
            && self.y >= frame.origin.y
            && self.x + self.w <= frame.origin.x + frame.size.width
            && self.y + self.h <= frame.origin.y + frame.size.height
    }

    /// The same rectangle expressed relative to `frame`'s origin, which is the space
    /// ScreenCaptureKit's `sourceRect` lives in.
    pub fn relative_to(
        &self,
        frame: objc2_core_foundation::CGRect,
    ) -> objc2_core_foundation::CGRect {
        objc2_core_foundation::CGRect {
            origin: objc2_core_foundation::CGPoint {
                x: self.x - frame.origin.x,
                y: self.y - frame.origin.y,
            },
            size: objc2_core_foundation::CGSize {
                width: self.w,
                height: self.h,
            },
        }
    }
}

impl fmt::Display for Region {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{},{},{}x{}", self.x, self.y, self.w, self.h)
    }
}

/// Everything the run is configured with. Defaults match the documented CLI defaults.
#[derive(Debug, Clone)]
pub struct Config {
    pub region: Region,
    pub samples: usize,
    /// kVK_ANSI_X by default — a glyph that fills a good share of a terminal cell.
    pub key: u16,
    /// kVK_Delete by default, so the cell toggles back and never drifts along the line.
    pub erase_key: u16,
    /// Cap the capture rate instead of following the display refresh.
    pub fps: Option<f64>,
    pub settle_ms: u64,
    pub quiet_ms: u64,
    pub gap_ms: u64,
    pub jitter_ms: u64,
    pub threshold: u8,
    pub min_pixels: usize,
    pub timeout_ms: u64,
    /// Post keystrokes straight to this process id instead of to the focused window.
    /// See [`inject::Injector`] — on a busy desktop, focus-routed injection is how a run
    /// silently types into the wrong application.
    pub target_pid: Option<i32>,
    pub out: Option<String>,
    pub notes: Vec<(String, String)>,
}

impl Config {
    pub fn new(region: Region) -> Self {
        Self {
            region,
            samples: 500,
            key: 7,
            erase_key: 51,
            fps: None,
            settle_ms: 250,
            quiet_ms: 300,
            gap_ms: 150,
            jitter_ms: 100,
            threshold: 24,
            min_pixels: 6,
            timeout_ms: 2_000,
            target_pid: None,
            out: None,
            notes: Vec::new(),
        }
    }

    fn detect(&self) -> DetectConfig {
        DetectConfig {
            threshold: self.threshold,
            min_pixels: self.min_pixels,
            quiet_us: self.quiet_ms * 1_000,
            quiet_deadline_us: QUIET_DEADLINE_MS * 1_000,
            settle_us: self.settle_ms * 1_000,
            timeout_us: self.timeout_ms * 1_000,
        }
    }
}

/// One trial's record, as written to and read back from the JSONL file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrialRecord {
    #[serde(rename = "type")]
    pub kind: String,
    pub i: usize,
    /// `press` typed the key, `erase` deleted it. Alternating keeps the watched cell in
    /// one place: a run of presses would walk the caret off the region.
    pub direction: String,
    pub key_code: u16,
    pub jitter_ms: u64,
    pub quiet_wait_ms: u64,
    pub spontaneous_changes: u64,
    pub poisoned: bool,
    pub timed_out: bool,
    /// Host-clock time the keystroke was posted at, microseconds.
    pub inject_pts_us: u64,
    pub first_us: Option<u64>,
    pub converged_us: Option<u64>,
    pub frames_in_trial: u64,
}

impl TrialRecord {
    /// A trial whose numbers may be summarised.
    pub fn is_valid(&self) -> bool {
        !self.poisoned && !self.timed_out && self.first_us.is_some()
    }
}

/// The trailer: what the whole run measured, and how uncertain it is.
#[derive(Debug, Clone)]
pub struct GlassSummary {
    pub valid_trials: usize,
    pub poisoned: usize,
    pub timed_out: usize,
    pub first: Option<Summary>,
    pub converged: Option<Summary>,
    pub configured_interval_us: u64,
    pub observed_median_gap_us: Option<u64>,
}

impl GlassSummary {
    pub fn from_trials(
        trials: &[TrialRecord],
        configured_interval_us: u64,
        observed_median_gap_us: Option<u64>,
    ) -> Self {
        let first: Vec<u64> = trials.iter().filter_map(valid_first).collect();
        let converged: Vec<u64> = trials.iter().filter_map(valid_converged).collect();
        Self {
            valid_trials: trials.iter().filter(|t| t.is_valid()).count(),
            poisoned: trials.iter().filter(|t| t.poisoned).count(),
            timed_out: trials.iter().filter(|t| t.timed_out).count(),
            first: stats::summarise(&first),
            converged: stats::summarise(&converged),
            configured_interval_us,
            observed_median_gap_us,
        }
    }

    /// The sampling interval the uncertainty band is built from, and its name.
    ///
    /// Asking ScreenCaptureKit for a rate is not the same as getting it: the smoke runs
    /// configured 120 fps and were delivered at 60. The band must follow the *wider* of
    /// the configured interval and the observed median inter-frame gap — a band built
    /// from the configured number alone would claim precision the capture did not have.
    /// The observed gap is never allowed to *narrow* the band: short bursts can deliver
    /// faster than the stream sustains.
    fn band_basis(&self) -> (u64, &'static str) {
        match self.observed_median_gap_us {
            Some(gap) if gap > self.configured_interval_us => (gap, "observed median gap"),
            _ => (self.configured_interval_us, "configured interval"),
        }
    }

    pub fn to_json(&self) -> Value {
        let (basis_us, basis_name) = self.band_basis();
        json!({
            "type": "summary",
            "valid_trials": self.valid_trials,
            "poisoned": self.poisoned,
            "timed_out": self.timed_out,
            "first": summary_json(self.first.as_ref()),
            "converged": summary_json(self.converged.as_ref()),
            "capture": {
                "configured_interval_us": self.configured_interval_us,
                "observed_median_gap_us": self.observed_median_gap_us,
            },
            "band_us": {
                "mean": basis_us / 2,
                "worst": basis_us,
                "basis": basis_name,
            },
            "p99_note": "p99 is deliberately absent. At n=500 the 99th percentile rests on \
                         five order statistics; quoting one would be a tail claim the sample \
                         cannot support.",
            "vrr_note": VRR_NOTE,
        })
    }

    /// The same thing in prose, in milliseconds, for a human reading the terminal.
    pub fn print(&self) {
        println!(
            "\nglass: {} valid trials, {} poisoned, {} timed out",
            self.valid_trials, self.poisoned, self.timed_out
        );
        print_row("first    ", self.first.as_ref());
        print_row("converged", self.converged.as_ref());

        let observed = match self.observed_median_gap_us {
            Some(g) => format!("{} ms", ms(g)),
            None => "not observed".to_owned(),
        };
        println!(
            "  capture   configured interval {} ms, observed median gap {observed}",
            ms(self.configured_interval_us),
        );
        let (basis_us, basis_name) = self.band_basis();
        println!(
            "  band      results read high by +{} ms mean, +{} ms worst-case, from the \
             {basis_name} (one-sided: a frame interval can only round a latency up)",
            ms(basis_us / 2),
            ms(basis_us),
        );
        println!("  note      {VRR_NOTE}");
        println!("  note      p99 not quoted: n is too small for a defensible tail claim");
    }
}

fn valid_first(t: &TrialRecord) -> Option<u64> {
    t.is_valid().then_some(t.first_us).flatten()
}

fn valid_converged(t: &TrialRecord) -> Option<u64> {
    t.is_valid().then_some(t.converged_us).flatten()
}

/// Serialise a summary with `p99` removed.
///
/// The HLD forbids a p99 claim at this sample size, and a number present in the JSON is a
/// number someone will quote. Removing it is the only way to make that impossible.
fn summary_json(summary: Option<&Summary>) -> Value {
    let Some(summary) = summary else {
        return Value::Null;
    };
    let mut value = serde_json::to_value(summary).unwrap_or(Value::Null);
    if let Some(map) = value.as_object_mut() {
        map.remove("p99");
    }
    value
}

fn ms(micros: u64) -> String {
    format!("{:.1}", micros as f64 / 1000.0)
}

fn print_row(label: &str, summary: Option<&Summary>) {
    match summary {
        Some(s) => println!(
            "  {label} n={:<4} p50 {} ms  p95 {} ms  min {} ms  max {} ms  mean {:.1} ms  \
             p50 95% CI [{}, {}] ms",
            s.n,
            ms(s.p50),
            ms(s.p95),
            ms(s.min),
            ms(s.max),
            s.mean / 1000.0,
            ms(s.p50_ci_low),
            ms(s.p50_ci_high),
        ),
        None => println!("  {label} no valid trials"),
    }
}

/// Print the pointer position twice a second for ten seconds.
///
/// Finding a region by arithmetic on window geometry is fiddly and wrong often enough to
/// waste a session. Hovering the pointer over the cell that will change and reading the
/// number off is not.
pub fn locate() -> Result<(), Error> {
    println!("Hover the pointer over the cell you want to watch. Global points, 10 s:");
    for _ in 0..20 {
        match inject::mouse_location() {
            Some((x, y)) => println!("  {x:.0},{y:.0}"),
            None => println!("  (could not read the pointer position)"),
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    println!("A 20x28 region around a point is usually one terminal cell: --region <x>,<y>,20x28");
    Ok(())
}

/// Run the whole measurement.
pub fn run(cfg: &Config) -> Result<(), Error> {
    let permissions = inject::preflight();
    if !permissions.granted() {
        return Err(permissions.instructions().into());
    }

    let target = capture::find_display(cfg.region)?;
    let fps = cfg.fps.unwrap_or(target.refresh_hz);
    if fps <= 0.0 {
        return Err(format!("nonsensical capture rate {fps} fps").into());
    }

    let injector = Injector::new(cfg.target_pid)?;

    // Held for the whole run: an idle display sleeps mid-measurement otherwise — the
    // injected keystrokes reset the idle timer, but the pauses between runs do not, and
    // a locked screen has no photons to time.
    let wake = wakelock::DisplayWake::acquire();
    if wake.held() {
        println!("  display sleep prevented for the duration of the run (as caffeinate -d)");
    }

    let capture = Capture::start(&target, cfg.region, fps)?;

    // Timebase sanity check, before anything is measured. If frame timestamps and the
    // injection clock were not the same clock, every latency in this run would be the
    // offset between two clocks with a real measurement buried in it.
    //
    // Whatever frame it consumes is kept, not thrown away: on a still region it may be
    // the only complete frame the stream ever sends, and the first trial needs it as its
    // reference.
    let (skew_us, mut last_frame) = check_timebase(&capture, &injector)?;

    let header = header_json(cfg, &target, fps, capture.interval_us, skew_us);
    print_preamble(cfg, &target, fps, capture.interval_us, skew_us);

    let mut sink = cfg
        .out
        .as_ref()
        .map(|path| OpenOptions::new().create(true).append(true).open(path))
        .transpose()?;
    write_line(&mut sink, &header)?;

    let rng_seed = injector.now_us().unwrap_or(1);
    let mut rng = SplitMix64::new(rng_seed);
    let mut trials = Vec::with_capacity(cfg.samples);
    let mut gaps: Vec<u64> = Vec::new();

    for i in 0..cfg.samples {
        let jitter_ms = rng.below(cfg.jitter_ms);
        pace(
            &capture,
            Duration::from_millis(cfg.gap_ms + jitter_ms),
            &mut last_frame,
        );

        let press = i.is_multiple_of(2);
        let key_code = if press { cfg.key } else { cfg.erase_key };
        let (outcome, trial_gaps) = run_trial(cfg, &capture, &injector, key_code, &mut last_frame)?;
        gaps.extend_from_slice(&trial_gaps);

        let record = TrialRecord {
            kind: "trial".to_owned(),
            i,
            direction: if press { "press" } else { "erase" }.to_owned(),
            key_code,
            jitter_ms,
            quiet_wait_ms: outcome.outcome.quiet_wait_us / 1_000,
            spontaneous_changes: outcome.outcome.spontaneous_changes,
            poisoned: outcome.outcome.poisoned,
            timed_out: outcome.outcome.timed_out,
            inject_pts_us: outcome.inject_us,
            first_us: outcome.outcome.first_us,
            converged_us: outcome.outcome.converged_us,
            frames_in_trial: outcome.outcome.frames,
        };
        write_line(&mut sink, &serde_json::to_value(&record)?)?;
        trials.push(record);

        if (i + 1).is_multiple_of(50) || i + 1 == cfg.samples {
            eprintln!("  {} / {} trials", i + 1, cfg.samples);
        }
    }

    capture.stop()?;

    let summary = GlassSummary::from_trials(&trials, capture_interval(fps), median(&mut gaps));
    write_line(&mut sink, &summary.to_json())?;
    summary.print();
    if let Some(path) = &cfg.out {
        println!("  wrote     {path}");
    }
    Ok(())
}

fn capture_interval(fps: f64) -> u64 {
    (1_000_000.0 / fps).round() as u64
}

/// What one trial produced, plus the injection timestamp for the record.
struct TrialResult {
    outcome: detect::Outcome,
    inject_us: u64,
}

fn run_trial(
    cfg: &Config,
    capture: &Capture,
    injector: &Injector,
    key_code: u16,
    last_frame: &mut Option<capture::Frame>,
) -> Result<(TrialResult, Vec<u64>), Error> {
    let mut trial = Trial::new(cfg.detect());
    if let Some(frame) = last_frame.as_ref() {
        trial.prime(&frame.pixels);
    }
    let mut inject_us = 0u64;
    let started = Instant::now();

    loop {
        if started.elapsed() > TRIAL_WALL_LIMIT {
            trial.starved();
            break;
        }
        let step = match capture.recv_timeout(POLL) {
            Ok(Sample::Frame(frame)) => {
                if frame.width == capture.width && frame.height == capture.height {
                    let step = trial.feed(frame.pts_us, &frame.pixels);
                    *last_frame = Some(frame);
                    step
                } else {
                    // The display reconfigured under us. Nothing sound can be said about
                    // a frame that is not the region we asked for.
                    Step::Wait
                }
            }
            Ok(Sample::Idle { pts_us }) => trial.feed_idle(pts_us),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Nothing delivered for a whole poll interval: the window server has
                // nothing new to show, which is itself a statement of no change.
                match injector.now_us() {
                    Some(now) => trial.feed_idle(now),
                    None => Step::Wait,
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("the capture stream stopped delivering frames".into());
            }
        };

        match step {
            Step::Wait => {}
            Step::InjectNow => {
                inject_us = injector.tap(key_code)?;
                trial.injected(inject_us);
            }
            Step::Done => break,
        }
    }

    let gaps = trial.gaps().to_vec();
    Ok((
        TrialResult {
            outcome: trial.outcome().clone(),
            inject_us,
        },
        gaps,
    ))
}

/// Consume frames for `pause`, so the next trial's quiet gate looks at live frames
/// instead of a backlog accumulated while the loop was asleep.
///
/// Frames are consumed, not ignored: the most recent one becomes the next trial's
/// reference, and it is often the only complete frame a still region ever produces.
fn pace(capture: &Capture, pause: Duration, last_frame: &mut Option<capture::Frame>) {
    let until = Instant::now() + pause;
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        if let Ok(Sample::Frame(frame)) = capture.recv_timeout(left.min(POLL)) {
            *last_frame = Some(frame);
        }
    }
    while let Some(frame) = capture.drain() {
        *last_frame = Some(frame);
    }
}

/// Verify that frame timestamps and the injection clock are the same clock.
///
/// Returns the observed skew in microseconds. A large skew means the two are *not* the
/// same timebase, at which point every latency this tool reports would be that offset
/// plus noise — a plausible-looking number with no relationship to anything.
fn check_timebase(
    capture: &Capture,
    injector: &Injector,
) -> Result<(i64, Option<capture::Frame>), Error> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let (pts_us, frame) = match capture.recv_timeout(Duration::from_millis(200)) {
            Ok(Sample::Frame(f)) => (f.pts_us, Some(f)),
            Ok(Sample::Idle { pts_us }) => (pts_us, None),
            Err(_) => continue,
        };
        let now = injector
            .now_us()
            .ok_or("the CoreMedia host clock reported an invalid time")?;
        let skew = now as i64 - pts_us as i64;
        if skew.abs() >= 1_000_000 {
            return Err(format!(
                "timebase check failed: a frame presented at {pts_us} us is {:.3} s from the \
                 host clock now ({now} us). Frame timestamps and the injection timestamp are \
                 not the same clock, so no latency measured here would mean anything.",
                skew as f64 / 1e6
            )
            .into());
        }
        return Ok((skew, frame));
    }
    Err("no frame arrived within 5 s, so the timebase could not be checked".into())
}

fn median(values: &mut [u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[values.len() / 2])
}

fn write_line(sink: &mut Option<File>, value: &Value) -> Result<(), Error> {
    if let Some(file) = sink {
        writeln!(file, "{}", serde_json::to_string(value)?)?;
    }
    Ok(())
}

fn header_json(
    cfg: &Config,
    target: &capture::DisplayTarget,
    fps: f64,
    interval_us: u64,
    skew_us: i64,
) -> Value {
    let notes: serde_json::Map<String, Value> = cfg
        .notes
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();

    json!({
        "type": "header",
        "cmd": "glass",
        "probe_version": env!("CARGO_PKG_VERSION"),
        "build_profile": build_profile(),
        "started_unix": now_unix_ms(),
        "region": cfg.region.to_string(),
        "samples": cfg.samples,
        "key_code": cfg.key,
        "erase_key_code": cfg.erase_key,
        "threshold": cfg.threshold,
        "min_pixels": cfg.min_pixels,
        "settle_ms": cfg.settle_ms,
        "quiet_ms": cfg.quiet_ms,
        "quiet_deadline_ms": QUIET_DEADLINE_MS,
        "timeout_ms": cfg.timeout_ms,
        "gap_ms": cfg.gap_ms,
        "jitter_ms": cfg.jitter_ms,
        "display": {
            "id": target.id,
            "pixel_width": target.pixel_width,
            "pixel_height": target.pixel_height,
            "refresh_hz": target.refresh_hz,
            "refresh_from_display_mode": target.refresh_from_mode,
            "backing_scale": target.scale,
        },
        "capture": {
            "fps": fps,
            "fps_capped_by_flag": cfg.fps.is_some(),
            "configured_interval_us": interval_us,
        },
        "timebase_skew_us": skew_us,
        "target_pid": cfg.target_pid,
        "vrr_note": VRR_NOTE,
        "notes": notes,
    })
}

fn print_preamble(
    cfg: &Config,
    target: &capture::DisplayTarget,
    fps: f64,
    interval_us: u64,
    skew_us: i64,
) {
    println!(
        "probe glass {} ({} build)",
        env!("CARGO_PKG_VERSION"),
        build_profile()
    );
    if cfg!(debug_assertions) {
        println!(
            "  ***  DEBUG BUILD — these numbers are not a measurement. Decode and colour \
             conversion run 10-30x slower unoptimised. Rebuild with --release.  ***"
        );
    }
    println!(
        "  display   {} at {}x{} px, {:.2} Hz{}, backing scale {}",
        target.id,
        target.pixel_width,
        target.pixel_height,
        target.refresh_hz,
        if target.refresh_from_mode {
            ""
        } else {
            " (ASSUMED — the display mode reports no refresh rate)"
        },
        target.scale,
    );
    println!(
        "  capture   {} at {:.2} fps ({} ms interval){}",
        cfg.region,
        fps,
        ms(interval_us),
        if cfg.fps.is_some() {
            ", capped by --fps"
        } else {
            ""
        },
    );
    println!(
        "  detect    threshold {}, min-pixels {}, quiet {} ms, settle {} ms, timeout {} ms",
        cfg.threshold, cfg.min_pixels, cfg.quiet_ms, cfg.settle_ms, cfg.timeout_ms,
    );
    println!(
        "  pacing    {} ms + uniform[0,{}) ms, keys {} / {}",
        cfg.gap_ms, cfg.jitter_ms, cfg.key, cfg.erase_key,
    );
    println!("  timebase  frame PTS is {skew_us} us behind the host clock (same clock: good)");
    for (k, v) in &cfg.notes {
        println!("  note      {k}={v}");
    }
    println!("  BEFORE YOU TRUST THIS: disable ProMotion / variable refresh on the display");
    println!("  ({VRR_NOTE}).");
    match cfg.target_pid {
        Some(pid) => println!(
            "  Keystrokes go straight to pid {pid}; keep its window visible and unoccluded.\n"
        ),
        None => {
            println!("  Focus the target window now; the probe types into whatever has focus.\n");
        }
    }
}

fn build_profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Re-derive a glass run's summary from its JSONL file.
///
/// Returns `Ok(None)` when the file is not a glass file, so the caller can fall through
/// to the RTT reader without guessing from the filename.
pub fn resummarise(path: &str) -> Result<Option<Value>, Error> {
    let reader = BufReader::new(File::open(path)?);
    let mut trials = Vec::new();
    let mut header: Option<Value> = None;
    let mut trailer: Option<Value> = None;
    let mut malformed = 0usize;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            malformed += 1;
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("header") if value.get("cmd").and_then(Value::as_str) == Some("glass") => {
                header = Some(value);
            }
            Some("trial") => match serde_json::from_value::<TrialRecord>(value) {
                Ok(trial) => trials.push(trial),
                Err(_) => malformed += 1,
            },
            Some("summary") => trailer = Some(value),
            _ => {}
        }
    }

    if header.is_none() && trials.is_empty() {
        return Ok(None);
    }

    let interval_us = header
        .as_ref()
        .and_then(|h| h.pointer("/capture/configured_interval_us"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    // The trial records do not carry a per-frame gap, so the observed interval is read
    // back from the trailer the run wrote. A file cut short before its trailer simply
    // reports no observed gap rather than inventing one.
    let observed = trailer
        .as_ref()
        .and_then(|t| t.pointer("/capture/observed_median_gap_us"))
        .and_then(Value::as_u64);

    let summary = GlassSummary::from_trials(&trials, interval_us, observed);
    summary.print();

    Ok(Some(json!({
        "source": path,
        "cmd": "glass",
        "malformed_lines": malformed,
        "trials_read": trials.len(),
        "header": header,
        "summary": summary.to_json(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trial(i: usize, first: Option<u64>, converged: Option<u64>) -> TrialRecord {
        TrialRecord {
            kind: "trial".to_owned(),
            i,
            direction: if i.is_multiple_of(2) {
                "press"
            } else {
                "erase"
            }
            .to_owned(),
            key_code: 7,
            jitter_ms: 0,
            quiet_wait_ms: 300,
            spontaneous_changes: 0,
            poisoned: false,
            timed_out: false,
            inject_pts_us: 1_000_000 + i as u64,
            first_us: first,
            converged_us: converged,
            frames_in_trial: 30,
        }
    }

    #[test]
    fn regions_parse_and_round_trip() {
        let r = Region::parse("100,200,20x28").expect("valid");
        assert_eq!((r.x, r.y, r.w, r.h), (100.0, 200.0, 20.0, 28.0));
        assert_eq!(r.to_string(), "100,200,20x28");
    }

    #[test]
    fn a_malformed_region_is_rejected_rather_than_guessed_at() {
        for bad in [
            "100,200",
            "100,200,20",
            "100,200,20y28",
            "a,b,cxd",
            "1,2,0x10",
        ] {
            assert!(Region::parse(bad).is_err(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn poisoned_and_timed_out_trials_are_excluded_from_the_summary() {
        // The whole point of recording them: a poisoned trial timed a caret blink and a
        // timed-out one timed nothing at all. Either in the sample corrupts the median.
        let mut trials = vec![trial(0, Some(40_000), Some(50_000))];
        let mut poisoned = trial(1, Some(1_000), Some(1_000));
        poisoned.poisoned = true;
        let mut timed_out = trial(2, None, None);
        timed_out.timed_out = true;
        trials.push(poisoned);
        trials.push(timed_out);
        trials.push(trial(3, Some(60_000), Some(70_000)));

        let s = GlassSummary::from_trials(&trials, 16_667, Some(16_700));
        assert_eq!(s.valid_trials, 2);
        assert_eq!(s.poisoned, 1);
        assert_eq!(s.timed_out, 1);
        let first = s.first.expect("two valid trials");
        assert_eq!(first.n, 2);
        assert_eq!(first.min, 40_000);
        assert_eq!(first.max, 60_000);
        let converged = s.converged.expect("two valid trials");
        assert_eq!((converged.min, converged.max), (50_000, 70_000));
    }

    #[test]
    fn the_summary_json_carries_no_p99() {
        // The HLD forbids a p99 claim at this n. A field present in the JSON is a field
        // someone will quote, so it must not be there at all.
        let trials: Vec<TrialRecord> = (0..500)
            .map(|i| trial(i, Some(40_000 + i as u64), Some(50_000 + i as u64)))
            .collect();
        let json = GlassSummary::from_trials(&trials, 16_667, Some(16_700)).to_json();
        assert!(json["first"]["p50"].is_number(), "p50 is still reported");
        assert!(json["first"]["p95"].is_number(), "p95 is still reported");
        assert!(json["first"]["p99"].is_null(), "p99 must be absent");
        assert!(json["converged"]["p99"].is_null(), "p99 must be absent");
        assert!(json["p99_note"].is_string(), "and its absence explained");
    }

    #[test]
    fn the_band_is_half_the_frame_interval_on_average_and_one_at_worst() {
        let json = GlassSummary::from_trials(&[trial(0, Some(1), Some(1))], 16_667, None).to_json();
        assert_eq!(json["band_us"]["mean"], 8_333);
        assert_eq!(json["band_us"]["worst"], 16_667);
        assert_eq!(json["band_us"]["basis"], "configured interval");
    }

    #[test]
    fn the_band_widens_to_the_observed_gap_when_delivery_is_slower_than_configured() {
        // Asking ScreenCaptureKit for 120 fps does not mean getting it. When the
        // observed inter-frame gap is wider than the configured interval, the real
        // sampling resolution is the observed one, and a band built from the configured
        // number would claim more precision than the capture had.
        let trials = [trial(0, Some(1), Some(1))];
        let json = GlassSummary::from_trials(&trials, 8_333, Some(16_700)).to_json();
        assert_eq!(json["band_us"]["mean"], 8_350);
        assert_eq!(json["band_us"]["worst"], 16_700);
        assert_eq!(json["band_us"]["basis"], "observed median gap");

        // An observed gap *tighter* than configured must not narrow the band: delivery
        // during short bursts can look faster than the stream sustains.
        let json = GlassSummary::from_trials(&trials, 16_667, Some(8_000)).to_json();
        assert_eq!(json["band_us"]["mean"], 8_333);
        assert_eq!(json["band_us"]["worst"], 16_667);
        assert_eq!(json["band_us"]["basis"], "configured interval");
    }

    #[test]
    fn trial_records_round_trip_through_json() {
        let record = trial(7, Some(41_234), Some(58_000));
        let line = serde_json::to_string(&record).expect("serialise");
        assert!(line.contains("\"type\":\"trial\""));
        let back: TrialRecord = serde_json::from_str(&line).expect("deserialise");
        assert_eq!(back.i, 7);
        assert_eq!(back.first_us, Some(41_234));
        assert_eq!(back.converged_us, Some(58_000));
        assert_eq!(back.direction, "erase");
    }
}
