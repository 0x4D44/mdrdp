//! The pure half of `probe glass`: change detection and the per-trial state machine.
//!
//! Nothing here touches a macOS API, on purpose. Every decision the instrument makes —
//! is the region quiet, has it changed, has it settled, has it timed out — is a function
//! of a sequence of `(presentation timestamp, BGRA bytes)` pairs, so the whole decision
//! layer can be driven from synthetic frames in a unit test. The ScreenCaptureKit and
//! CGEvent code in the sibling modules only supplies those pairs and posts the key.

use serde::{Deserialize, Serialize};

/// How many pixels differ between two equally-sized BGRA buffers.
///
/// A pixel counts as changed when the largest absolute per-channel delta over B, G and R
/// exceeds `threshold`. Alpha is ignored: ScreenCaptureKit hands back opaque frames, so
/// an alpha channel that never varies would only dilute the comparison.
///
/// Buffers of unequal length are compared over their common prefix; the caller is
/// expected to have rejected mismatched frame sizes before getting here.
pub fn changed_pixels(reference: &[u8], frame: &[u8], threshold: u8) -> usize {
    reference
        .chunks_exact(4)
        .zip(frame.chunks_exact(4))
        .filter(|(a, b)| {
            let delta = |i: usize| a[i].abs_diff(b[i]);
            delta(0).max(delta(1)).max(delta(2)) > threshold
        })
        .count()
}

/// The thresholds and windows one trial is judged against. All times in microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetectConfig {
    pub threshold: u8,
    /// A frame counts as changed only once this many pixels have changed.
    pub min_pixels: usize,
    /// Required run of no-change before the keystroke may be injected.
    pub quiet_us: u64,
    /// Give up waiting for quiet after this long and poison the trial.
    pub quiet_deadline_us: u64,
    /// No change for this long after the first change means the region has converged.
    pub settle_us: u64,
    /// Give up waiting for the first change after this long.
    pub timeout_us: u64,
}

/// What one trial produced. `first_us` and `converged_us` are the two event times the
/// whole instrument exists to record; both are `None` unless the trial ran clean.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    /// The region would not go quiet — something else on screen is moving (a blinking
    /// caret is the usual culprit), so this trial's timings mean nothing.
    pub poisoned: bool,
    /// The keystroke went out and nothing changed within the timeout.
    pub timed_out: bool,
    /// Inject → presentation timestamp of the first changed frame.
    pub first_us: Option<u64>,
    /// Inject → presentation timestamp of the last changed frame before settling.
    pub converged_us: Option<u64>,
    /// Frame-to-frame changes seen while waiting for the quiet gate.
    pub spontaneous_changes: u64,
    /// How long the quiet gate took to pass.
    pub quiet_wait_us: u64,
    /// Frames fed to this trial, including the ones it skipped.
    pub frames: u64,
}

/// What the caller should do after feeding a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Keep feeding frames.
    Wait,
    /// The quiet gate has passed. Post the keystroke *now*, then call
    /// [`Trial::injected`] with the host-clock time taken immediately before the post.
    InjectNow,
    /// The trial is over; read [`Trial::outcome`].
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Waiting for `quiet_us` with no frame-to-frame change.
    Quiet,
    /// Keystroke posted; waiting for the first frame that differs from the reference.
    Armed,
    /// Change seen; waiting for `settle_us` with no further frame-to-frame change.
    Settling,
    Done,
}

/// One measurement trial, driven one frame at a time.
///
/// The two comparisons are deliberately different, and the difference is load-bearing:
///
/// * **First change** is measured against the *pre-trial reference* — the last frame
///   before the keystroke. A glyph that fades in over several frames still counts from
///   the first frame that differs from the untouched region.
/// * **Convergence** is measured against the *previous frame*. Comparing against the
///   reference could never converge: once the glyph is drawn the region differs from the
///   reference forever. What settles is the frame-to-frame delta, not the total delta.
#[derive(Debug)]
pub struct Trial {
    cfg: DetectConfig,
    phase: Phase,
    /// Presentation timestamp of the first frame fed, the origin for the quiet deadline.
    started_us: Option<u64>,
    /// Presentation timestamp of the most recent frame that differed from its predecessor.
    last_change_us: u64,
    /// The previous frame, for the frame-to-frame comparison.
    prev: Vec<u8>,
    /// The last frame before the keystroke, for the first-change comparison.
    reference: Vec<u8>,
    inject_us: u64,
    /// Presentation timestamp of the previous frame seen after injection, for gap stats.
    prev_burst_us: Option<u64>,
    gaps: Vec<u64>,
    outcome: Outcome,
}

impl Trial {
    pub fn new(cfg: DetectConfig) -> Self {
        Self {
            cfg,
            phase: Phase::Quiet,
            started_us: None,
            last_change_us: 0,
            prev: Vec::new(),
            reference: Vec::new(),
            inject_us: 0,
            prev_burst_us: None,
            gaps: Vec::new(),
            outcome: Outcome::default(),
        }
    }

    pub fn outcome(&self) -> &Outcome {
        &self.outcome
    }

    /// Presentation-timestamp deltas between consecutive frames delivered after the
    /// keystroke — the observed frame interval during a change burst, which is the
    /// sampling period the uncertainty band is built from.
    pub fn gaps(&self) -> &[u64] {
        &self.gaps
    }

    /// Start the trial already knowing what the region looks like.
    ///
    /// This is what makes a trial on a *still* region possible at all. ScreenCaptureKit
    /// sends a full frame when something changes and idle samples when nothing does, so
    /// a trial that started with an empty reference and then saw nothing but idles would
    /// never get a reference and would poison — which is exactly what a well-behaved,
    /// perfectly quiet target looks like. The caller carries the last frame it saw
    /// across trials and hands it in here.
    ///
    /// Call before the first `feed`.
    pub fn prime(&mut self, frame: &[u8]) {
        debug_assert!(self.started_us.is_none(), "prime() after the trial started");
        self.set_prev(frame);
    }

    /// Record the host-clock time the keystroke was posted at. Call this immediately
    /// after acting on [`Step::InjectNow`].
    pub fn injected(&mut self, inject_us: u64) {
        debug_assert_eq!(
            self.phase,
            Phase::Armed,
            "injected() outside the armed phase"
        );
        self.inject_us = inject_us;
    }

    /// Feed one captured frame.
    ///
    /// `pts_us` is the frame's presentation timestamp on the CoreMedia host clock — the
    /// same clock the injection time is taken on. Callback arrival time would fold this
    /// process's scheduling delay into every measurement.
    pub fn feed(&mut self, pts_us: u64, frame: &[u8]) -> Step {
        self.advance(pts_us, Some(frame))
    }

    /// Feed a moment at which the region is known *not* to have changed.
    ///
    /// A static region produces no new frames: ScreenCaptureKit delivers idle samples
    /// with no pixel buffer, and when nothing at all is moving it may deliver nothing.
    /// Both are positive evidence of no change, and every window this machine measures —
    /// quiet, timeout, settle — is a window of *time*, which only elapses if something
    /// keeps telling it the clock has moved. Without this the quiet gate on a genuinely
    /// still screen would never open and every trial would poison.
    pub fn feed_idle(&mut self, pts_us: u64) -> Step {
        self.advance(pts_us, None)
    }

    /// The capture stopped producing anything at all. Recorded as a timeout after
    /// injection, and as poisoning before it.
    pub fn starved(&mut self) {
        match self.phase {
            Phase::Armed | Phase::Settling => self.outcome.timed_out = true,
            _ => self.outcome.poisoned = true,
        }
        self.phase = Phase::Done;
    }

    fn advance(&mut self, pts_us: u64, frame: Option<&[u8]>) -> Step {
        if self.phase == Phase::Done {
            return Step::Done;
        }
        if frame.is_some() {
            self.outcome.frames += 1;
        }
        match self.phase {
            Phase::Quiet => self.advance_quiet(pts_us, frame),
            Phase::Armed => self.advance_armed(pts_us, frame),
            Phase::Settling => self.advance_settling(pts_us, frame),
            Phase::Done => Step::Done,
        }
    }

    fn advance_quiet(&mut self, pts_us: u64, frame: Option<&[u8]>) -> Step {
        let started = match self.started_us {
            Some(started) => started,
            None => {
                // First tick of the trial. The quiet run starts now, whether or not a
                // frame came with it — otherwise a primed trial would arm immediately,
                // treating "no change since timestamp zero" as a satisfied quiet gate.
                self.started_us = Some(pts_us);
                self.last_change_us = pts_us;
                pts_us
            }
        };
        if let Some(frame) = frame {
            if self.prev.is_empty() {
                self.set_prev(frame);
                return Step::Wait;
            }
            if self.changed_vs_prev(frame) {
                self.outcome.spontaneous_changes += 1;
                self.last_change_us = pts_us;
            }
            self.set_prev(frame);
        }

        // Nothing to be a reference to yet — no real frame has arrived.
        if self.prev.is_empty() {
            if pts_us.saturating_sub(started) >= self.cfg.quiet_deadline_us {
                self.outcome.poisoned = true;
                self.phase = Phase::Done;
                return Step::Done;
            }
            return Step::Wait;
        }

        // Quiet wins a tie with the deadline: a trial that goes quiet on the very frame
        // the deadline expires is a usable trial, not a poisoned one.
        if pts_us.saturating_sub(self.last_change_us) >= self.cfg.quiet_us {
            self.reference.clear();
            self.reference.extend_from_slice(&self.prev);
            self.outcome.quiet_wait_us = pts_us.saturating_sub(started);
            self.phase = Phase::Armed;
            return Step::InjectNow;
        }
        if pts_us.saturating_sub(started) >= self.cfg.quiet_deadline_us {
            self.outcome.poisoned = true;
            self.phase = Phase::Done;
            return Step::Done;
        }
        Step::Wait
    }

    fn advance_armed(&mut self, pts_us: u64, frame: Option<&[u8]>) -> Step {
        // Frames already in flight when the key went out predate it and cannot show its
        // effect. Timing one of them would report a negative latency as a tiny one.
        if pts_us <= self.inject_us {
            return Step::Wait;
        }
        if let Some(frame) = frame {
            self.note_gap(pts_us);
            if self.changed_vs_reference(frame) {
                self.outcome.first_us = Some(pts_us - self.inject_us);
                self.last_change_us = pts_us;
                self.set_prev(frame);
                self.phase = Phase::Settling;
                return Step::Wait;
            }
            self.set_prev(frame);
        }
        if pts_us - self.inject_us >= self.cfg.timeout_us {
            self.outcome.timed_out = true;
            self.phase = Phase::Done;
            return Step::Done;
        }
        Step::Wait
    }

    fn advance_settling(&mut self, pts_us: u64, frame: Option<&[u8]>) -> Step {
        if let Some(frame) = frame {
            self.note_gap(pts_us);
            if self.changed_vs_prev(frame) {
                self.last_change_us = pts_us;
            }
            self.set_prev(frame);
        }

        if pts_us.saturating_sub(self.last_change_us) >= self.cfg.settle_us {
            self.outcome.converged_us = Some(self.last_change_us.saturating_sub(self.inject_us));
            self.phase = Phase::Done;
            return Step::Done;
        }
        Step::Wait
    }

    fn changed_vs_reference(&self, frame: &[u8]) -> bool {
        changed_pixels(&self.reference, frame, self.cfg.threshold) >= self.cfg.min_pixels
    }

    /// Compare against the previous frame. The buffer is swapped out and back rather
    /// than cloned so a per-frame comparison costs no allocation.
    fn changed_vs_prev(&mut self, frame: &[u8]) -> bool {
        let prev = std::mem::take(&mut self.prev);
        let changed = changed_pixels(&prev, frame, self.cfg.threshold) >= self.cfg.min_pixels;
        self.prev = prev;
        changed
    }

    fn set_prev(&mut self, frame: &[u8]) {
        self.prev.clear();
        self.prev.extend_from_slice(frame);
    }

    fn note_gap(&mut self, pts_us: u64) {
        if let Some(prev) = self.prev_burst_us {
            self.gaps.push(pts_us.saturating_sub(prev));
        }
        self.prev_burst_us = Some(pts_us);
    }
}

/// SplitMix64 — the inter-trial jitter source.
///
/// A fixed pacing interval can phase-lock to the display's refresh, which would sample
/// one point of the frame interval over and over and hide exactly the quantisation error
/// the uncertainty band exists to describe. Jitter breaks the lock. This is nine lines of
/// arithmetic with a published constant set; a `rand` dependency would buy nothing.
#[derive(Debug, Clone)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A draw from `0..n`. Returns 0 for `n == 0`. The modulo bias is on the order of
    /// `n / 2^64` for the values used here (milliseconds), which is nothing.
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 4;
    const H: usize = 4;
    const PIXELS: usize = W * H;

    fn cfg() -> DetectConfig {
        DetectConfig {
            threshold: 24,
            min_pixels: 6,
            quiet_us: 300_000,
            quiet_deadline_us: 5_000_000,
            settle_us: 250_000,
            timeout_us: 2_000_000,
        }
    }

    /// A uniform grey frame. Distinct values per call site so a test cannot accidentally
    /// compare a buffer with itself and call the agreement a result.
    fn flat(level: u8) -> Vec<u8> {
        vec![level; PIXELS * 4]
    }

    /// `n` pixels lifted by `delta` on all three colour channels; alpha left alone.
    fn with_changed(base: &[u8], n: usize, delta: u8) -> Vec<u8> {
        let mut out = base.to_vec();
        for px in out.chunks_exact_mut(4).take(n) {
            for ch in px.iter_mut().take(3) {
                *ch = ch.saturating_add(delta);
            }
        }
        out
    }

    #[test]
    fn identical_frames_have_no_changed_pixels() {
        assert_eq!(changed_pixels(&flat(40), &flat(40), 24), 0);
    }

    #[test]
    fn a_glyph_sized_change_is_counted_pixel_by_pixel() {
        let base = flat(40);
        let glyph = with_changed(&base, 9, 200);
        assert_eq!(changed_pixels(&base, &glyph, 24), 9);
    }

    #[test]
    fn alpha_alone_is_not_a_change() {
        // A frame whose only difference is the alpha byte must read as unchanged: SCK
        // hands back opaque frames and an alpha wobble is not a photon.
        //
        // The control matters as much as the assertion. The *same* delta on the blue
        // channel must count, or "alpha is ignored" would be indistinguishable from
        // "this comparison sees nothing at all".
        let base = flat(40);
        let mut alpha_only = base.clone();
        let mut blue_only = base.clone();
        for px in alpha_only.chunks_exact_mut(4) {
            px[3] = 255;
        }
        for px in blue_only.chunks_exact_mut(4) {
            px[0] = 255;
        }
        assert_eq!(
            changed_pixels(&base, &alpha_only, 24),
            0,
            "alpha is ignored"
        );
        assert_eq!(
            changed_pixels(&base, &blue_only, 24),
            PIXELS,
            "the same delta on a colour channel does count"
        );
    }

    #[test]
    fn the_threshold_is_exclusive_at_the_boundary() {
        // Delta exactly == threshold is *not* a change; one more is. The boundary is the
        // difference between counting sensor noise and missing a faint glyph.
        let base = flat(40);
        let at = with_changed(&base, PIXELS, 24);
        let over = with_changed(&base, PIXELS, 25);
        assert_eq!(changed_pixels(&base, &at, 24), 0, "delta == threshold");
        assert_eq!(
            changed_pixels(&base, &over, 24),
            PIXELS,
            "delta > threshold"
        );
    }

    #[test]
    fn a_change_below_min_pixels_does_not_arm_the_state_machine() {
        let base = flat(40);
        // Five changed pixels against a min_pixels of six.
        let speck = with_changed(&base, 5, 200);
        assert_eq!(changed_pixels(&base, &speck, 24), 5);

        let mut trial = Trial::new(cfg());
        assert_eq!(trial.feed(0, &base), Step::Wait);
        assert_eq!(trial.feed(16_000, &speck), Step::Wait);
        // The speck did not count, so the quiet run was never broken.
        assert_eq!(trial.outcome().spontaneous_changes, 0);
    }

    #[test]
    fn a_change_below_min_pixels_is_not_the_first_change_either() {
        // The quiet gate and the first-change test are separate comparisons against
        // separate buffers. A min_pixels that only guards the first of them would let a
        // stray pixel of anti-aliasing be timed as the keystroke landing.
        let base = flat(40);
        let (mut trial, quiet_pts) = armed(cfg(), &base);
        trial.injected(quiet_pts);

        let speck = with_changed(&base, 5, 200);
        let glyph = with_changed(&base, 20, 200);
        assert_eq!(trial.feed(quiet_pts + 16_000, &speck), Step::Wait);
        assert!(
            trial.outcome().first_us.is_none(),
            "five pixels is below min_pixels and must not count as the glyph appearing"
        );

        assert_eq!(trial.feed(quiet_pts + 32_000, &glyph), Step::Wait);
        assert_eq!(trial.outcome().first_us, Some(32_000));
    }

    /// Feed a trial until it stops waiting, and report the step and the timestamp it
    /// stopped at.
    ///
    /// The bound is not decoration. A test that loops "until the machine decides" hangs
    /// forever the moment the machine stops deciding — which is exactly what a broken
    /// deadline looks like — and a hung test reports nothing at all.
    fn drive(start_us: u64, step_us: u64, mut tick: impl FnMut(u64) -> Step) -> (Step, u64) {
        let mut pts = start_us;
        for _ in 0..2_000 {
            let step = tick(pts);
            if step != Step::Wait {
                return (step, pts);
            }
            pts += step_us;
        }
        panic!("the trial never left the waiting state within 2000 ticks");
    }

    /// Run the quiet gate to completion and return the armed trial plus the frame that
    /// became its reference.
    fn armed(cfg: DetectConfig, base: &[u8]) -> (Trial, u64) {
        let mut trial = Trial::new(cfg);
        let (step, pts) = drive(0, 16_000, |pts| trial.feed(pts, base));
        assert_eq!(
            step,
            Step::InjectNow,
            "quiet gate failed on a static region"
        );
        (trial, pts)
    }

    #[test]
    fn a_static_region_passes_the_quiet_gate_and_asks_for_the_keystroke() {
        let base = flat(40);
        let (trial, pts) = armed(cfg(), &base);
        assert_eq!(trial.outcome().spontaneous_changes, 0);
        // 300 ms of quiet at a 16 ms frame interval: the first frame that is >= 300 ms
        // past the last change is at 304 ms.
        assert_eq!(pts, 304_000);
        assert_eq!(trial.outcome().quiet_wait_us, 304_000);
    }

    #[test]
    fn a_blinking_caret_poisons_the_trial_instead_of_timing_it() {
        // The failure this exists to catch: a region that never goes quiet still passes
        // frames, and a naive gate would arm on one of them and time a caret blink.
        let mut trial = Trial::new(cfg());
        let base = flat(40);
        let blink = with_changed(&base, PIXELS, 200);

        let (step, _) = drive(0, 16_000, |pts| {
            // Alternate every 100 ms — faster than the 300 ms quiet window.
            let frame = if (pts / 100_000).is_multiple_of(2) {
                &base
            } else {
                &blink
            };
            trial.feed(pts, frame)
        });
        assert_eq!(step, Step::Done, "must end, not arm");
        assert!(trial.outcome().poisoned, "and end poisoned");
        assert!(!trial.outcome().timed_out);
        assert!(trial.outcome().first_us.is_none());
        assert!(trial.outcome().spontaneous_changes > 10);
    }

    #[test]
    fn first_and_converged_are_measured_from_the_injection_timestamp() {
        let base = flat(40);
        let (mut trial, quiet_pts) = armed(cfg(), &base);

        // The key goes out 1 ms after the frame that armed the trial.
        let inject = quiet_pts + 1_000;
        trial.injected(inject);

        // One frame already in flight (predates the key), then the glyph appears, grows
        // for two more frames, then the region is static.
        let glyph1 = with_changed(&base, 8, 200);
        let glyph2 = with_changed(&base, 20, 200);

        assert_eq!(
            trial.feed(quiet_pts + 500, &base),
            Step::Wait,
            "pre-key frame"
        );
        assert_eq!(trial.feed(inject + 40_000, &glyph1), Step::Wait);
        assert_eq!(trial.outcome().first_us, Some(40_000));

        assert_eq!(trial.feed(inject + 56_000, &glyph2), Step::Wait);

        // Now static. Convergence lands on the *last changed* frame, not on the frame
        // that proved the settle window had elapsed.
        let (step, _) = drive(inject + 72_000, 16_000, |pts| trial.feed(pts, &glyph2));
        assert_eq!(step, Step::Done);
        assert_eq!(trial.outcome().converged_us, Some(56_000));
        assert!(!trial.outcome().timed_out);
        assert!(!trial.outcome().poisoned);
    }

    #[test]
    fn a_keystroke_that_changes_nothing_times_out() {
        let base = flat(40);
        let (mut trial, quiet_pts) = armed(cfg(), &base);
        trial.injected(quiet_pts);

        let (step, pts) = drive(quiet_pts + 16_000, 16_000, |pts| trial.feed(pts, &base));
        assert_eq!(step, Step::Done);
        assert!(trial.outcome().timed_out);
        assert!(!trial.outcome().poisoned);
        assert!(trial.outcome().first_us.is_none());
        assert!(trial.outcome().converged_us.is_none());
        // 2 s timeout at a 16 ms interval.
        assert_eq!(pts - quiet_pts, 2_000_000);
    }

    #[test]
    fn starvation_before_the_key_poisons_and_after_it_times_out() {
        let base = flat(40);

        let mut early = Trial::new(cfg());
        early.feed(0, &base);
        early.starved();
        assert!(early.outcome().poisoned);
        assert!(!early.outcome().timed_out);

        let (mut late, quiet_pts) = armed(cfg(), &base);
        late.injected(quiet_pts);
        late.starved();
        assert!(late.outcome().timed_out);
        assert!(!late.outcome().poisoned);
    }

    #[test]
    fn gaps_are_recorded_only_for_frames_after_the_keystroke() {
        let base = flat(40);
        let (mut trial, quiet_pts) = armed(cfg(), &base);
        // The quiet gate fed ~20 frames; none of them may appear in the gap sample.
        assert!(trial.gaps().is_empty());

        trial.injected(quiet_pts);
        let glyph = with_changed(&base, 20, 200);
        trial.feed(quiet_pts + 16_000, &glyph);
        trial.feed(quiet_pts + 33_000, &glyph);
        trial.feed(quiet_pts + 49_000, &glyph);
        assert_eq!(trial.gaps(), &[17_000, 16_000]);
    }

    #[test]
    fn a_primed_trial_still_waits_out_the_full_quiet_window() {
        // A trial primed with the previous trial's last frame must open the quiet gate
        // exactly `quiet_us` after its *first tick*, not immediately. Getting this wrong
        // arms on frame one and reports the caret blink that is already in flight.
        let base = flat(40);
        let mut trial = Trial::new(cfg());
        trial.prime(&base);

        let (step, pts) = drive(9_000_000, 20_000, |pts| trial.feed_idle(pts));
        assert_eq!(step, Step::InjectNow);
        assert_eq!(
            pts - 9_000_000,
            300_000,
            "300 ms of quiet measured from the first tick, not from timestamp zero"
        );
        assert_eq!(trial.outcome().frames, 0, "primed without a fed frame");
    }

    #[test]
    fn a_primed_trial_notices_the_region_changed_while_it_was_not_looking() {
        // The prime is a real reference, not a formality: if the region moved during the
        // inter-trial pause, the first live frame must reset the quiet run.
        let base = flat(40);
        let moved = with_changed(&base, PIXELS, 200);
        let mut trial = Trial::new(cfg());
        trial.prime(&base);

        assert_eq!(trial.feed(0, &moved), Step::Wait);
        assert_eq!(trial.outcome().spontaneous_changes, 1);
    }

    #[test]
    fn idle_ticks_alone_carry_the_quiet_gate_and_the_settle_window() {
        // The failure this guards against: on a genuinely still screen ScreenCaptureKit
        // delivers no new frames at all, so a machine that only advances on frames would
        // wait for quiet forever and poison every trial.
        let base = flat(40);
        let mut trial = Trial::new(cfg());
        assert_eq!(trial.feed(0, &base), Step::Wait, "one real frame to anchor");

        let (step, pts) = drive(20_000, 20_000, |pts| trial.feed_idle(pts));
        assert_eq!(
            step,
            Step::InjectNow,
            "quiet gate opened on idle ticks alone"
        );
        assert_eq!(pts, 300_000);
        assert_eq!(trial.outcome().frames, 1, "idle ticks are not frames");

        // Settle likewise: one changed frame, then nothing but idle ticks.
        trial.injected(pts);
        let glyph = with_changed(&base, 20, 200);
        assert_eq!(trial.feed(pts + 30_000, &glyph), Step::Wait);
        let (step, _) = drive(pts + 50_000, 20_000, |tick| trial.feed_idle(tick));
        assert_eq!(step, Step::Done);
        assert_eq!(trial.outcome().first_us, Some(30_000));
        assert_eq!(trial.outcome().converged_us, Some(30_000));
    }

    #[test]
    fn an_idle_only_trial_that_never_sees_a_frame_poisons() {
        // No reference frame can be taken, so there is nothing to arm against.
        let mut trial = Trial::new(cfg());
        let (step, _) = drive(0, 100_000, |pts| trial.feed_idle(pts));
        assert_eq!(step, Step::Done);
        assert!(trial.outcome().poisoned);
        assert_eq!(trial.outcome().frames, 0);
    }

    #[test]
    fn splitmix64_is_deterministic_and_stays_in_range() {
        let mut a = SplitMix64::new(42);
        let mut b = SplitMix64::new(42);
        let first: Vec<u64> = (0..8).map(|_| a.next_u64()).collect();
        let second: Vec<u64> = (0..8).map(|_| b.next_u64()).collect();
        assert_eq!(first, second, "same seed, same stream");
        assert!(first.windows(2).all(|w| w[0] != w[1]), "not a stuck value");

        let mut c = SplitMix64::new(7);
        // A different seed must produce a different stream, or the seeding is a no-op.
        assert_ne!(c.next_u64(), first[0]);
        assert!((0..1000).all(|_| c.below(100) < 100));
        assert_eq!(c.below(0), 0);
    }
}
