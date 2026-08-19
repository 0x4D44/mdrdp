//! Where the host's audio comes from, and how it is proved to have arrived.
//!
//! Portable by construction: the trait, the generators and the tone detector all
//! build and test on macOS with no platform dependency. WASAPI loopback lives in
//! `win::audio` behind this trait, so the whole pipeline above it is testable on
//! a machine with no Windows and no sound card — which matters more than usual
//! here, because **no fleet host can run the WASAPI half at all** (HLD tranche 6
//! §2: neither quench nor temper has an audio render endpoint).
//!
//! # Why the test tone is two different frequencies
//!
//! The obvious probe is a 440 Hz sine in both channels, and it is close to
//! useless: a resampling or rate error turns a pure sine into a slightly
//! different pure sine, and a left/right swap turns it into itself. A check that
//! cannot fail on the faults most likely to occur is decoration.
//!
//! So the generator puts **440 Hz in the left channel and 660 Hz in the right**.
//! Recovering both, in the right ears, falsifies channel swap, silent
//! mono-downmix, one-channel dropout and gross rate error in a single
//! measurement — and it is the difference between a frame counter and an oracle.

use crate::aux_proto::AudioFrame;

/// Milliseconds of audio per wire frame.
///
/// Ten milliseconds is one WASAPI buffer period and 1920 bytes at 48 kHz stereo:
/// small enough that a dropped frame is a click rather than a gap, large enough
/// that the per-frame overhead (11 bytes of framing plus a 14-byte header) is
/// under 2% of the payload.
pub const FRAME_MS: u32 = 10;

/// The left channel's test frequency.
pub const TONE_LEFT_HZ: f64 = 440.0;
/// The right channel's test frequency. Deliberately different — see module docs.
pub const TONE_RIGHT_HZ: f64 = 660.0;

/// What a source produced this tick.
#[derive(Debug, PartialEq)]
pub enum Captured {
    /// Audio to send.
    Frame(AudioFrame),
    /// The source is running and produced only silence.
    ///
    /// Distinct from [`Captured::Unavailable`] on purpose: silence means the
    /// capture position advances while nothing goes on the wire, which is what
    /// lets the client tell "the desktop was quiet" from "frames were lost".
    /// Collapsing the two would make a working-but-quiet host and a broken one
    /// look identical, which is the failure the whole `capture_pos` field exists
    /// to prevent.
    Silence { frames: u64 },
    /// There is nothing to capture from. A **supported state**, not an error:
    /// a headless host with no render endpoint reports this forever and stays
    /// healthy.
    Unavailable,
}

/// A source of host audio.
///
/// Deliberately **not** `Send`. A WASAPI capture client holds COM interface
/// pointers, and the right discipline for those is to build and use them on one
/// thread rather than to move them between threads and reason about apartments.
/// The channel's audio thread therefore constructs its own source from a factory
/// instead of being handed one — which the borrow checker insisted on before the
/// design did, and was right to.
pub trait AudioSource {
    /// Produce the next block, blocking for at most one frame period.
    fn next_block(&mut self) -> Captured;
    /// A short name for logs and the health ladder.
    fn describe(&self) -> &'static str;
}

/// How many whole audio frames fit in one wire frame at this rate.
pub fn frames_per_block(sample_rate: u32) -> usize {
    (sample_rate as usize * FRAME_MS as usize) / 1000
}

/// A synthetic two-tone generator.
///
/// Its job is to make the transport provable on a host that cannot capture. It
/// is not a stand-in for real audio and does not pretend to be: it exercises
/// framing, the outbox, the wire, the client's decode and resample path, and the
/// playback ring — everything except the WASAPI read itself.
pub struct ToneSource {
    sample_rate: u32,
    channels: u8,
    capture_pos: u64,
    /// Sample index, kept separately from `capture_pos` so phase stays continuous
    /// across blocks. Restarting the phase each block would put a discontinuity
    /// at every frame boundary — an audible buzz that would also defeat the
    /// frequency check meant to detect exactly that kind of fault.
    n: u64,
}

impl ToneSource {
    pub fn new(sample_rate: u32, channels: u8) -> Self {
        Self {
            sample_rate,
            channels,
            capture_pos: 0,
            n: 0,
        }
    }

    /// The amplitude the generator emits, as a fraction of full scale.
    ///
    /// Well below full scale so that any gain applied downstream has headroom to
    /// show up as a changed amplitude rather than as clipping, which would look
    /// like a different fault.
    pub const AMPLITUDE: f64 = 0.25;
}

impl AudioSource for ToneSource {
    fn next_block(&mut self) -> Captured {
        let frames = frames_per_block(self.sample_rate);
        let mut pcm = Vec::with_capacity(frames * self.channels as usize * 2);
        for _ in 0..frames {
            let t = self.n as f64 / self.sample_rate as f64;
            for ch in 0..self.channels {
                let hz = if ch == 0 { TONE_LEFT_HZ } else { TONE_RIGHT_HZ };
                let v = (Self::AMPLITUDE * (std::f64::consts::TAU * hz * t).sin()) as f32;
                let sample = (v * i16::MAX as f32) as i16;
                pcm.extend_from_slice(&sample.to_le_bytes());
            }
            self.n += 1;
        }
        let frame = AudioFrame {
            sample_rate: self.sample_rate,
            channels: self.channels,
            capture_pos: self.capture_pos,
            pcm,
        };
        self.capture_pos += frames as u64;
        Captured::Frame(frame)
    }

    fn describe(&self) -> &'static str {
        "tone"
    }
}

/// A source that runs correctly and produces nothing but silence.
///
/// Exists so "silence costs nothing on the wire" can be tested by a fixture that
/// is **proved to have run** — its capture position advances. Without that, the
/// criterion passes on a host with no endpoint for entirely the wrong reason,
/// because a source that never started also sends zero bytes. This repo has paid
/// for that lesson twice already.
pub struct SilentSource {
    sample_rate: u32,
    capture_pos: u64,
}

impl SilentSource {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            capture_pos: 0,
        }
    }
    /// How far the capture clock has advanced — the proof it ran.
    pub fn capture_pos(&self) -> u64 {
        self.capture_pos
    }
}

impl AudioSource for SilentSource {
    fn next_block(&mut self) -> Captured {
        let frames = frames_per_block(self.sample_rate) as u64;
        self.capture_pos += frames;
        Captured::Silence { frames }
    }
    fn describe(&self) -> &'static str {
        "silent"
    }
}

/// A source for a host with no render endpoint.
pub struct UnavailableSource;

impl AudioSource for UnavailableSource {
    fn next_block(&mut self) -> Captured {
        Captured::Unavailable
    }
    fn describe(&self) -> &'static str {
        "unavailable"
    }
}

/// Power at `target_hz` in `samples`, by the Goertzel algorithm.
///
/// One bin of a DFT for the cost of a loop — the right tool when you know which
/// frequency you are asking about, which is exactly the tone check's situation.
pub fn goertzel_power(samples: &[f32], sample_rate: u32, target_hz: f64) -> f64 {
    if samples.is_empty() || sample_rate == 0 {
        return 0.0;
    }
    let k = std::f64::consts::TAU * target_hz / sample_rate as f64;
    let coeff = 2.0 * k.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &x in samples {
        let s0 = x as f64 + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    // Squared magnitude, normalised by length so the result does not depend on
    // how much audio was handed in.
    let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
    power / (samples.len() as f64 * samples.len() as f64)
}

/// Take every `channels`-th sample starting at `channel`.
pub fn deinterleave(samples: &[f32], channels: u8, channel: u8) -> Vec<f32> {
    if channels == 0 || channel >= channels {
        return Vec::new();
    }
    samples
        .iter()
        .skip(channel as usize)
        .step_by(channels as usize)
        .copied()
        .collect()
}

/// The strongest frequency in `samples`, searched at 1 Hz resolution over
/// `range`.
///
/// Returns `None` for silence, so a dead channel is distinguishable from a
/// wrong-frequency one — those need different diagnoses, and a function that
/// returned an arbitrary bin for silence would conflate them.
pub fn peak_frequency(
    samples: &[f32],
    sample_rate: u32,
    range: std::ops::RangeInclusive<u32>,
) -> Option<f64> {
    let energy: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    if energy < 1e-6 {
        return None;
    }
    let mut best = (0.0f64, 0.0f64);
    for hz in range {
        let p = goertzel_power(samples, sample_rate, hz as f64);
        if p > best.1 {
            best = (hz as f64, p);
        }
    }
    if best.1 <= 0.0 {
        None
    } else {
        Some(best.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// Wire bytes back to samples, the way the client will.
    fn to_f32(pcm: &[u8]) -> Vec<f32> {
        pcm.chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / i16::MAX as f32)
            .collect()
    }

    /// Concatenate `blocks` blocks of tone into one sample buffer.
    fn tone_samples(blocks: usize) -> Vec<f32> {
        let mut src = ToneSource::new(RATE, 2);
        let mut all = Vec::new();
        for _ in 0..blocks {
            match src.next_block() {
                Captured::Frame(f) => all.extend(to_f32(&f.pcm)),
                other => panic!("the tone source must produce audio, got {other:?}"),
            }
        }
        all
    }

    #[test]
    fn ten_milliseconds_at_48k_is_480_frames() {
        assert_eq!(frames_per_block(48_000), 480);
        assert_eq!(frames_per_block(44_100), 441);
    }

    #[test]
    fn the_tone_source_puts_a_different_frequency_in_each_ear() {
        // **This is the oracle the acceptance criteria rest on.** A single tone
        // in both channels cannot fail on a channel swap, which is one of the
        // faults most likely to occur.
        //
        // 20 blocks is 200 ms: enough resolution to separate 440 from 660
        // comfortably at 1 Hz search steps.
        let samples = tone_samples(20);
        let left = deinterleave(&samples, 2, 0);
        let right = deinterleave(&samples, 2, 1);

        let lf = peak_frequency(&left, RATE, 300..=900).expect("left carries a tone");
        let rf = peak_frequency(&right, RATE, 300..=900).expect("right carries a tone");
        assert!(
            (lf - TONE_LEFT_HZ).abs() <= 1.0,
            "left should be {TONE_LEFT_HZ} Hz, measured {lf}"
        );
        assert!(
            (rf - TONE_RIGHT_HZ).abs() <= 1.0,
            "right should be {TONE_RIGHT_HZ} Hz, measured {rf}"
        );
    }

    #[test]
    fn a_channel_swap_is_detectable_by_this_check() {
        // The check is only worth having if it FAILS on the fault it claims to
        // catch. Swap the ears and confirm the measurement notices, so the test
        // above is known to be load-bearing rather than merely green.
        let samples = tone_samples(20);
        let swapped_left = deinterleave(&samples, 2, 1);
        let lf = peak_frequency(&swapped_left, RATE, 300..=900).expect("still a tone");
        assert!(
            (lf - TONE_LEFT_HZ).abs() > 1.0,
            "a swapped channel must NOT measure as {TONE_LEFT_HZ} Hz, got {lf}"
        );
    }

    #[test]
    fn phase_is_continuous_across_block_boundaries() {
        // Restarting the phase each block would put a discontinuity at every
        // frame boundary: an audible buzz, and one that would also break the
        // frequency check meant to detect that class of fault. Measuring across
        // many concatenated blocks is what makes a per-block reset visible.
        let samples = tone_samples(40);
        let left = deinterleave(&samples, 2, 0);
        let f = peak_frequency(&left, RATE, 300..=900).expect("a tone");
        assert!(
            (f - TONE_LEFT_HZ).abs() <= 1.0,
            "phase resets would smear the peak away from {TONE_LEFT_HZ}: measured {f}"
        );
    }

    #[test]
    fn the_capture_position_advances_by_exactly_one_block_each_time() {
        let mut src = ToneSource::new(RATE, 2);
        let expected = frames_per_block(RATE) as u64;
        for block in 0..5u64 {
            match src.next_block() {
                Captured::Frame(f) => assert_eq!(
                    f.capture_pos,
                    block * expected,
                    "block {block} should start at sample {}",
                    block * expected
                ),
                other => panic!("expected audio, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_silent_source_sends_nothing_but_still_advances_the_capture_clock() {
        // The advancing clock is the fixture's proof-of-run. Without it, "silence
        // costs nothing on the wire" is also satisfied by a source that never
        // started -- which is precisely the state every fleet host is in.
        let mut src = SilentSource::new(RATE);
        let expected = frames_per_block(RATE) as u64;
        assert_eq!(src.capture_pos(), 0);
        for n in 1..=3u64 {
            assert_eq!(src.next_block(), Captured::Silence { frames: expected });
            assert_eq!(
                src.capture_pos(),
                n * expected,
                "the capture clock must run even when nothing is sent"
            );
        }
    }

    #[test]
    fn an_unavailable_source_is_a_state_not_an_error_and_never_advances() {
        let mut src = UnavailableSource;
        for _ in 0..3 {
            assert_eq!(src.next_block(), Captured::Unavailable);
        }
        assert_eq!(src.describe(), "unavailable");
    }

    #[test]
    fn goertzel_separates_a_tone_from_a_different_tone() {
        let mut src = ToneSource::new(RATE, 1); // mono: left frequency only
        let mut samples = Vec::new();
        for _ in 0..20 {
            match src.next_block() {
                Captured::Frame(f) => samples.extend(to_f32(&f.pcm)),
                other => panic!("expected audio, got {other:?}"),
            }
        }
        let at_tone = goertzel_power(&samples, RATE, TONE_LEFT_HZ);
        let elsewhere = goertzel_power(&samples, RATE, 1_000.0);
        assert!(
            at_tone > elsewhere * 100.0,
            "power at the tone ({at_tone}) should dwarf an unrelated bin ({elsewhere})"
        );
    }

    #[test]
    fn silence_has_no_peak_frequency_so_a_dead_channel_is_not_a_wrong_one() {
        // A function that returned an arbitrary bin for silence would make a
        // dead channel look like a mistuned one. Those need different diagnoses.
        let quiet = vec![0.0f32; 4800];
        assert_eq!(peak_frequency(&quiet, RATE, 300..=900), None);

        // **Near**-silence is the case that actually needs the energy guard, and
        // a mutation pass proved it: with exact zeros every bin scores zero, so
        // the "no positive peak" check alone returns None and the test passed
        // with the energy guard deleted. Real silence is never exactly zero --
        // dither, a DC offset, a decoder's last breath -- and against that a
        // frequency-blind search happily reports whichever bin won by noise.
        let dither: Vec<f32> = (0..4800)
            .map(|i| if i % 7 == 0 { 3e-6 } else { -2e-6 })
            .collect();
        assert_eq!(
            peak_frequency(&dither, RATE, 300..=900),
            None,
            "noise this quiet is silence, not a tone"
        );
    }

    #[test]
    fn deinterleave_refuses_a_channel_that_does_not_exist() {
        let samples = vec![1.0, 2.0, 3.0, 4.0];
        assert_eq!(deinterleave(&samples, 2, 0), vec![1.0, 3.0]);
        assert_eq!(deinterleave(&samples, 2, 1), vec![2.0, 4.0]);
        assert!(deinterleave(&samples, 2, 2).is_empty());
        assert!(deinterleave(&samples, 0, 0).is_empty());
    }

    #[test]
    fn a_tone_frame_is_well_formed_for_the_wire() {
        // The generator must not be able to produce something the protocol will
        // reject: it is the fixture the whole transport is proved with.
        let mut src = ToneSource::new(RATE, 2);
        match src.next_block() {
            Captured::Frame(f) => {
                let mut wire = Vec::new();
                crate::aux_proto::encode_audio(&f, &mut wire)
                    .expect("a generated frame must satisfy the wire rules");
                assert!(f.pcm.len() <= crate::aux_proto::MAX_AUDIO_BYTES);
            }
            other => panic!("expected audio, got {other:?}"),
        }
    }
}
