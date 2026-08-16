//! RDPSND (MS-RDPEA) playback: the server's audio, played on this machine.
//!
//! **Playback only.** No microphone / audio-input redirection — out of scope, per the
//! product brief.
//!
//! ## Why this module is split the way it is
//!
//! `ironrdp_rdpsnd::client::Rdpsnd::process` runs on whatever thread pumps the RDP
//! session's virtual channels. Its callback contract
//! ([`RdpsndClientHandler`]) must return promptly: `Rdpsnd::process` builds the
//! `WaveConfirm` reply and hands it back to the caller to send **immediately after**
//! calling [`RdpsndClientHandler::wave`] returns, so a slow or blocking `wave()` stalls the
//! confirm — and MS-RDPEA throttles or stalls the stream when confirms fall behind. That
//! is the whole answer to requirement 6 (wave timestamps / `WaveConfirm`): the crate
//! already sends it, on our behalf, the instant `wave()` returns. Our only obligation is
//! to make sure it returns fast.
//!
//! cpal's output callback runs on a **third**, real-time thread with its own much harder
//! rule: no allocation, no blocking, no long-held lock, ever. A glitch there is audible;
//! a glitch on the network thread is just a dropped packet.
//!
//! So there are three pieces, each honest about which thread it runs on:
//!
//! * [`RdpsndBackend`] — implements [`RdpsndClientHandler`]. Runs on the channel thread.
//!   Converts wire PCM to the device's format and pushes it into the ring buffer. Never
//!   touches cpal.
//! * [`AudioRing`] — the boundary between them. A fixed-capacity sample queue behind a
//!   short-held [`Mutex`]. Both sides only ever do O(1) deque math while holding it.
//! * [`AudioPlayback`] — owns the cpal `Stream`. Its callback does one thing: pull from
//!   the ring into the device's buffer.
//!
//! ## `format_no` resolution
//!
//! [`Wave2Pdu::format_no`] indexes into the *Client Audio Formats* array — the list
//! actually sent to the server after negotiation. Published IronRDP 0.9 built that list
//! through a randomly ordered `HashSet::intersection` and never reported the result to the
//! handler, so advertising multiple formats made the index unknowable. The vendored
//! `ironrdp-rdpsnd` preserves the handler's candidate order and calls
//! [`RdpsndClientHandler::set_negotiated_formats`] with the exact vector it sends.
//! [`RdpsndBackend`] retains that vector separately from its four stable capabilities and
//! resolves every wave against it. An out-of-range index is dropped rather than guessed.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ironrdp::core::{Encode, EncodeResult, WriteCursor, ensure_size, impl_as_any};
use ironrdp::pdu::{PduResult, encode_err};
use ironrdp_dvc::{DvcChannelListener, DvcEncode, DvcMessage, DvcProcessor, DynamicChannelId};
use ironrdp_rdpsnd::client::RdpsndClientHandler;
use ironrdp_rdpsnd::pdu::{AudioFormat, PitchPdu, VolumePdu, WaveFormat};
use ironrdp_svc::SvcProcessor as _;
use serde::Serialize;
use tracing::{debug, error};

// ---------------------------------------------------------------------------------------
// Format capability and selection
// ---------------------------------------------------------------------------------------

/// Sample rates we advertise. Anything else the server offers is left unadvertised rather
/// than accepted-and-mangled.
const SUPPORTED_SAMPLE_RATES: [u32; 2] = [44_100, 48_000];

/// Channel counts we advertise: mono and stereo. Nothing wider (5.1, etc).
const SUPPORTED_CHANNELS: [u16; 2] = [1, 2];

const BITS_PER_SAMPLE: u16 = 16;

/// The formats we advertise to the server: every combination of
/// [`SUPPORTED_SAMPLE_RATES`] x [`SUPPORTED_CHANNELS`], all uncompressed 16-bit PCM.
///
/// Deliberately conservative. Advertising a compressed format we cannot decode would get
/// accepted by the server and then produce silence that looks like a working connection —
/// the worst failure mode for this feature. PCM is the only format this module knows how
/// to turn into samples, so PCM is the only thing offered.
pub fn candidate_formats() -> Vec<AudioFormat> {
    let mut formats = Vec::with_capacity(SUPPORTED_SAMPLE_RATES.len() * SUPPORTED_CHANNELS.len());
    for &rate in &SUPPORTED_SAMPLE_RATES {
        for &channels in &SUPPORTED_CHANNELS {
            formats.push(pcm_format(rate, channels));
        }
    }
    formats
}

/// Build one PCM [`AudioFormat`] entry for the wire.
fn pcm_format(n_samples_per_sec: u32, n_channels: u16) -> AudioFormat {
    let block_align = n_channels * (BITS_PER_SAMPLE / 8);
    AudioFormat {
        format: WaveFormat::PCM,
        n_channels,
        n_samples_per_sec,
        n_avg_bytes_per_sec: n_samples_per_sec * u32::from(block_align),
        n_block_align: block_align,
        bits_per_sample: BITS_PER_SAMPLE,
        data: None,
    }
}

/// Whether `fmt` is something this module can actually decode and play.
///
/// The single gate that keeps requirement 1 honest: nothing downstream of this function
/// may accept a format for which it returns `false`.
pub fn is_playable(fmt: &AudioFormat) -> bool {
    fmt.format == WaveFormat::PCM
        && fmt.bits_per_sample == BITS_PER_SAMPLE
        && SUPPORTED_CHANNELS.contains(&fmt.n_channels)
        && SUPPORTED_SAMPLE_RATES.contains(&fmt.n_samples_per_sec)
}

/// No format in an offered list was one we can play.
///
/// Returned rather than silently falling back to *something* — an accepted-but-unplayable
/// format is indistinguishable from a working, silent connection, which is exactly the
/// failure requirement 1 exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoUsableFormat;

impl fmt::Display for NoUsableFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no offered audio format is one this client can play")
    }
}

impl std::error::Error for NoUsableFormat {}

/// Pick the best playable format from a list, preferring the higher sample rate and then
/// stereo over mono (fewer conversions to do later).
///
/// Pure and independently testable. `RdpsndClientHandler` never hands this module the
/// server's offered list directly — negotiation is owned entirely by
/// `Rdpsnd::client_formats` upstream — so nothing here currently calls this in the live
/// path. It exists as the tested seam for the selection *policy*, and for any caller that
/// does see an offer (diagnostics, logging, a future protocol path).
pub fn choose_format(offered: &[AudioFormat]) -> Result<AudioFormat, NoUsableFormat> {
    offered
        .iter()
        .filter(|f| is_playable(f))
        .cloned()
        .max_by_key(|f| (f.n_samples_per_sec, f.n_channels))
        .ok_or(NoUsableFormat)
}

// ---------------------------------------------------------------------------------------
// Sample conversion
// ---------------------------------------------------------------------------------------

/// Decode little-endian 16-bit PCM bytes into `f32` samples in `[-1.0, 1.0]`.
///
/// A trailing odd byte — a truncated wave payload, the obvious hostile/corrupt input — is
/// silently dropped rather than read out of bounds or panicked on: [`slice::chunks_exact`]
/// simply never yields a partial chunk.
pub fn pcm16_le_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32768.0)
        .collect()
}

/// Duplicate each mono sample into a left/right pair.
///
/// `out.len() == mono.len() * 2`, always — the frame count callers rely on.
pub fn duplicate_mono_to_stereo(mono: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(mono.len() * 2);
    for &s in mono {
        out.push(s);
        out.push(s);
    }
    out
}

/// Average each interleaved stereo pair down to one mono sample.
///
/// Only reached if the output device itself reports a mono default config, which is rare
/// but not impossible (some USB headsets do). A trailing unpaired sample — malformed
/// input, since a real stereo stream is always an even number of samples — is dropped
/// rather than indexed out of bounds.
pub fn downmix_stereo_to_mono(stereo: &[f32]) -> Vec<f32> {
    stereo
        .chunks_exact(2)
        .map(|pair| (pair[0] + pair[1]) * 0.5)
        .collect()
}

/// Reconcile the wire format's channel count with the device's, in either direction.
///
/// Both sides are constrained to {1, 2} — [`is_playable`] on the wire side,
/// [`AudioPlayback`]'s device-config check on the device side — so this is a total
/// function over the four combinations that can actually occur.
fn remap_channels(samples: &[f32], from_channels: u16, to_channels: u16) -> Vec<f32> {
    match (from_channels, to_channels) {
        (1, 2) => duplicate_mono_to_stereo(samples),
        (2, 1) => downmix_stereo_to_mono(samples),
        _ => samples.to_vec(),
    }
}

/// Linear resample of interleaved audio with `channels` channels per frame, from
/// `from_rate` to `to_rate`.
///
/// Output frame count is `floor(input_frames * to_rate / from_rate)` — computed once in
/// integer arithmetic, not accumulated per-sample, so it cannot drift from that formula.
/// Each output frame linearly interpolates between the two nearest input frames; the last
/// input frame is clamped so the final output frame never reads past the end.
///
/// This is intentionally simple (no windowing, no anti-aliasing filter). Good enough for
/// speech/UI-sound remote-desktop audio where the alternative is a network thread doing a
/// full resampling-library pass on every wave packet; not good enough for anything that
/// would be judged on audio-engineer ears.
pub fn linear_resample(input: &[f32], channels: usize, from_rate: u32, to_rate: u32) -> Vec<f32> {
    if channels == 0 || from_rate == 0 || to_rate == 0 || input.is_empty() {
        return Vec::new();
    }
    if from_rate == to_rate {
        return input.to_vec();
    }

    let in_frames = input.len() / channels;
    if in_frames == 0 {
        return Vec::new();
    }

    let out_frames = usize::try_from(
        (in_frames as u64)
            .saturating_mul(u64::from(to_rate))
            .saturating_div(u64::from(from_rate)),
    )
    .unwrap_or(usize::MAX);

    let mut out = Vec::with_capacity(out_frames * channels);
    let step = f64::from(from_rate) / f64::from(to_rate);
    let last_frame = in_frames - 1;

    for i in 0..out_frames {
        let pos = (i as f64) * step;
        let idx0 = (pos.floor() as usize).min(last_frame);
        let idx1 = (idx0 + 1).min(last_frame);
        let frac = (pos - idx0 as f64) as f32;

        for c in 0..channels {
            let s0 = input[idx0 * channels + c];
            let s1 = input[idx1 * channels + c];
            out.push(s0 + (s1 - s0) * frac);
        }
    }

    out
}

// ---------------------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------------------

/// The negotiated format currently in use, in the terms the rest of the pipeline cares
/// about. Never carries audio content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AudioFormatSummary {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
}

/// What the audio path did, in counters cheap to poll and safe to log.
///
/// No sample data ever lands here — only sizes and counts, matching the clipboard/session
/// rule that payload content never gets logged.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AudioStats {
    /// `Wave2` PDUs received from the server.
    pub packets_received: u64,
    /// Wire bytes of PCM handed to the playback pipeline (queued, not necessarily still
    /// resident — an overrun can discard some of it after this counter increments).
    pub bytes_played: u64,
    /// Samples dropped because the ring buffer was full when new audio arrived. Each drop
    /// removed the OLDEST sample, never the newest — see [`AudioRing`].
    pub overruns: u64,
    /// Dry-spell *episodes*: times the ring ran out of samples mid-stream and playback
    /// fell to silence, counted once per episode however many device pulls the silence
    /// spans. Counted only from the moment real audio has flowed at least once — silence
    /// before the first sample ever arrives (the device opens and starts pulling before
    /// the RDP session even exists) is expected, not a glitch. One episode is one
    /// audible gap, so this number is directly comparable to what the user heard.
    pub underruns: u64,
    /// Audio-device failures observed (open failure, runtime device error). The session
    /// keeps running without audio when this is non-zero; see [`AudioPlayback`].
    pub device_errors: u64,
    pub current_format: Option<AudioFormatSummary>,
    /// How many formats went out in the most recent Client Audio Formats PDU, or `None`
    /// if no format exchange ever completed on either transport.
    ///
    /// This is the only counter that separates "the server never opened an audio channel"
    /// from "it opened one and nothing happened to be playing". `current_format` cannot:
    /// it is set by [`RdpsndBackend::wave`], so it stays `None` through a perfectly
    /// healthy silent session, and reading a negotiation failure out of it would attribute
    /// a fault to a stage that was never measured.
    pub negotiated_formats: Option<usize>,
}

/// A cloneable read/write handle on a shared [`AudioStats`].
///
/// Shared between [`RdpsndBackend`] (network thread), [`AudioRing`] (both threads) and
/// [`AudioPlayback`] (device-error callback), each of which only ever holds the lock for
/// an `O(1)` counter bump.
#[derive(Debug, Clone, Default)]
pub struct AudioStatsHandle(Arc<Mutex<AudioStats>>);

impl AudioStatsHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// A point-in-time copy. Never aliases later mutation.
    pub fn snapshot(&self) -> AudioStats {
        self.lock().clone()
    }

    fn lock(&self) -> MutexGuard<'_, AudioStats> {
        // A poisoned stats mutex means some other thread panicked while counting — not a
        // reason to bring down a running audio path, so recover the counters.
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn note<F: FnOnce(&mut AudioStats)>(&self, f: F) {
        f(&mut self.lock());
    }
}

// ---------------------------------------------------------------------------------------
// Ring buffer
// ---------------------------------------------------------------------------------------

/// How long the playback ring buffer is allowed to hold, in wall-clock time.
///
/// 400ms of *capacity*, not of latency: the ring only ever holds what the server has sent
/// ahead of playback, so steady-state depth — and therefore latency — is the server's
/// actual lead (Windows targets roughly 150-250ms), whatever the capacity. Capacity only
/// matters when a burst lands, and there the measurement decides: a 120s YouTube session
/// through a 200ms ring dropped ~20ms of audio across the bursts (MDR-BUG-FLUX-00001),
/// meaning the bursts marginally exceed 200ms. Doubling the headroom absorbs them without
/// adding a millisecond to the quiet-path latency. This is a queueing buffer for smoothing
/// delivery, not a jitter-buffer trying to reconstruct timing — RDPSND carries no
/// per-sample timestamp finer than the block, so there is nothing more precise to target.
pub const RING_BUFFER_MS: u64 = 400;

struct RingBuf {
    samples: VecDeque<f32>,
    capacity: usize,
    /// Latches to `true` the first time [`push`](Self::push) receives real samples.
    /// Distinguishes "audio has never arrived yet" from "audio arrived and then the ring
    /// ran dry" — only the latter is a real underrun. See [`pop_into`](Self::pop_into).
    has_flowed: bool,
    /// Whether the consumer is currently inside a dry spell. An underrun is counted once
    /// per *episode* — the transition into silence — not once per callback pull: the
    /// device pulls ~100 times a second, so per-pull counting turned every quiet stretch
    /// (a paused video, the stream simply ending) into thousands of "underruns" and made
    /// the counter useless for telling an audible mid-stream gap from ordinary silence.
    in_gap: bool,
}

impl RingBuf {
    fn new(capacity: usize) -> Self {
        // A zero-capacity ring can never hold anything to play; clamp to a minimum that
        // is still a real (if aggressively small) buffer rather than a permanent no-op.
        let capacity = capacity.max(1);
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
            has_flowed: false,
            in_gap: false,
        }
    }

    /// Push new samples, evicting the OLDEST buffered sample per sample that would
    /// otherwise overflow capacity. Returns the number evicted.
    ///
    /// Dropping oldest-first is the deliberate policy: latency matters more than
    /// completeness for a remote desktop, so when the network is outrunning the device we
    /// want the buffer to hold what's about to be played, not what already fell behind.
    fn push(&mut self, incoming: &[f32]) -> u64 {
        if !incoming.is_empty() {
            self.has_flowed = true;
            // Fresh audio ends any dry spell; the next one is a new episode.
            self.in_gap = false;
        }
        let mut dropped = 0u64;
        for &s in incoming {
            if self.samples.len() >= self.capacity {
                self.samples.pop_front();
                dropped += 1;
            }
            self.samples.push_back(s);
        }
        dropped
    }

    /// Fill `out` from the buffer. Any samples the buffer cannot supply are left at
    /// silence (`0.0`) — repeating the last buffer instead would produce an audible buzz,
    /// which is the one thing worse than a gap. Returns whether this pull *starts* an
    /// underrun episode: silence was needed, real audio has flowed at least once before,
    /// and the previous pull was not already dry. Silence pulled before the first sample
    /// ever arrives is not an underrun — nothing was expected yet (the cpal callback
    /// starts pulling the instant the device opens, which is before the RDP session even
    /// exists — see `AudioPlayback::start`). Continuation pulls of an ongoing dry spell
    /// do not count either: one episode is one audible gap, however long the device keeps
    /// pulling silence through it.
    fn pop_into(&mut self, out: &mut [f32]) -> bool {
        let mut had_gap = false;
        for slot in out.iter_mut() {
            *slot = match self.samples.pop_front() {
                Some(s) => s,
                None => {
                    had_gap = true;
                    0.0
                }
            };
        }
        let starts_episode = had_gap && self.has_flowed && !self.in_gap;
        if had_gap {
            self.in_gap = true;
        }
        starts_episode
    }
}

/// The bounded queue between the network thread (producer) and the cpal callback thread
/// (consumer). See the module docs for the full thread picture.
///
/// Cloning shares the same buffer and the same [`AudioStatsHandle`] — this is a handle,
/// not a value.
#[derive(Clone)]
pub struct AudioRing {
    buf: Arc<Mutex<RingBuf>>,
    stats: AudioStatsHandle,
    /// Shared gate latched off by a fatal cpal stream error. Producers then stop decoding
    /// and queuing packets while the callback emits silence, leaving the RDP session alive.
    enabled: Arc<AtomicBool>,
}

impl fmt::Debug for AudioRing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print sample content. Depth is visible via AudioStats already.
        f.debug_struct("AudioRing").finish_non_exhaustive()
    }
}

impl AudioRing {
    /// A ring holding exactly `capacity_samples` interleaved samples (i.e. `frames *
    /// channels`, not frames).
    pub fn with_capacity(capacity_samples: usize, stats: AudioStatsHandle) -> Self {
        Self {
            buf: Arc::new(Mutex::new(RingBuf::new(capacity_samples))),
            stats,
            enabled: Arc::new(AtomicBool::new(true)),
        }
    }

    /// A ring sized for [`RING_BUFFER_MS`] of audio at the given device format.
    pub fn for_device(sample_rate: u32, channels: u16, stats: AudioStatsHandle) -> Self {
        let capacity = (u64::from(sample_rate) * u64::from(channels) * RING_BUFFER_MS / 1000)
            .try_into()
            .unwrap_or(usize::MAX);
        Self::with_capacity(capacity, stats)
    }

    /// Producer side: push resampled, channel-mapped samples. Never blocks on the
    /// consumer — the lock's critical section is `O(incoming.len())` deque pushes, no
    /// syscalls, no allocation beyond what `VecDeque` already reserved.
    pub fn push(&self, incoming: &[f32]) {
        if incoming.is_empty() {
            return;
        }
        if !self.is_enabled() {
            return;
        }
        let mut guard = self.lock();
        // Re-check after taking the lock: a device error may have disabled the gate while
        // this producer was waiting, and no samples may survive that transition.
        if !self.is_enabled() {
            return;
        }
        let dropped = guard.push(incoming);
        if dropped > 0 {
            self.stats
                .note(|s| s.overruns = s.overruns.saturating_add(dropped));
        }
    }

    /// Permanently stop playback for this ring after a fatal device error.
    fn disable(&self) {
        self.enabled.store(false, Ordering::Release);
    }

    /// Consumer side: called from the cpal realtime callback. Fills `out` fully, using
    /// silence for anything the buffer could not supply. Counts an underrun only once real
    /// audio has flowed at least once — see [`RingBuf::pop_into`] — so the device opening
    /// and pulling from an empty ring before the RDP session has sent a single sample
    /// (which happens on every session start) does not inflate the counter.
    /// `O(out.len())`, no allocation, no syscalls, and the lock is held only across that
    /// same deque work.
    pub fn pop_into(&self, out: &mut [f32]) {
        if !self.is_enabled() {
            out.fill(0.0);
            return;
        }
        let is_underrun = self.lock().pop_into(out);
        if is_underrun {
            self.stats
                .note(|s| s.underruns = s.underruns.saturating_add(1));
        }
    }

    /// Discard everything currently queued and report how much was dropped. Used on
    /// `close()`/renegotiation so stale audio does not trail out after the server logically
    /// ended the stream.
    fn drain_len(&self) -> usize {
        let mut guard = self.lock();
        let n = guard.samples.len();
        guard.samples.clear();
        n
    }

    fn lock(&self) -> MutexGuard<'_, RingBuf> {
        self.buf
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
}

// ---------------------------------------------------------------------------------------
// RdpsndBackend — the channel-thread half
// ---------------------------------------------------------------------------------------

/// Implements [`RdpsndClientHandler`]: turns wave PDUs into samples in [`AudioRing`].
///
/// Runs on the session's channel-processing thread (see module docs). Every callback here
/// must return promptly and must never panic on malformed wire input — a hostile or buggy
/// server gets a dropped packet and a counter, never a crash or a stall.
#[derive(Debug)]
pub struct RdpsndBackend {
    /// Stable capability order advertised on every server-format announcement.
    candidates: Vec<AudioFormat>,
    /// Exact intersection sent in the latest Client Audio Formats PDU. `Wave2.format_no`
    /// indexes this vector, never the broader candidate list.
    negotiated_formats: Vec<AudioFormat>,
    ring: AudioRing,
    stats: AudioStatsHandle,
    device: AudioFormatSummary,
}

impl RdpsndBackend {
    /// `device` is the format [`AudioPlayback`] actually opened — `wave()` converts every
    /// incoming packet to it. Construct the playback side first so this has a real format
    /// to target instead of a guess.
    pub fn new(ring: AudioRing, stats: AudioStatsHandle, device: AudioFormatSummary) -> Self {
        Self {
            candidates: candidate_formats(),
            negotiated_formats: Vec::new(),
            ring,
            stats,
            device,
        }
    }

    /// Test-only escape hatch: construct with an explicit already-negotiated list. Exists
    /// so tests can pin `wave()` index resolution independently of the transport callback.
    #[cfg(test)]
    fn with_formats(
        formats: Vec<AudioFormat>,
        ring: AudioRing,
        stats: AudioStatsHandle,
        device: AudioFormatSummary,
    ) -> Self {
        Self {
            candidates: formats.clone(),
            negotiated_formats: formats,
            ring,
            stats,
            device,
        }
    }
}

impl RdpsndClientHandler for RdpsndBackend {
    fn get_formats(&self) -> &[AudioFormat] {
        &self.candidates
    }

    fn set_negotiated_formats(&mut self, formats: &[AudioFormat]) {
        self.negotiated_formats.clear();
        self.negotiated_formats.extend_from_slice(formats);
        // Recorded even when empty: "we exchanged formats and shared none" is a different
        // diagnosis from "no audio channel ever opened", and the report must not merge them.
        self.stats
            .note(|s| s.negotiated_formats = Some(formats.len()));
    }

    fn wave(&mut self, format_no: usize, _ts: u32, data: Cow<'_, [u8]>) {
        self.stats
            .note(|s| s.packets_received = s.packets_received.saturating_add(1));

        // A runtime device error is terminal for this stream. Keep acknowledging packets
        // through the normal callback path so the RDP session survives, but do not spend
        // CPU decoding audio that can no longer be played or queue it behind silence.
        if !self.ring.is_enabled() {
            return;
        }

        let Some(fmt) = self
            .negotiated_formats
            .get(format_no)
            .filter(|f| is_playable(f))
        else {
            // A Wave2 index outside the exact list sent on the wire is a protocol fault.
            // Silence is the only honest response; guessing a candidate would play at the
            // wrong rate or channel count. Logged at debug to avoid per-packet spam.
            debug!(
                format_no,
                formats_len = self.negotiated_formats.len(),
                "rdpsnd: wave for unresolvable format index; dropping packet"
            );
            return;
        };
        let fmt = fmt.clone();

        self.stats.note(|s| {
            s.bytes_played = s.bytes_played.saturating_add(data.len() as u64);
            s.current_format = Some(AudioFormatSummary {
                sample_rate: fmt.n_samples_per_sec,
                channels: fmt.n_channels,
                bits_per_sample: fmt.bits_per_sample,
            });
        });

        let samples = pcm16_le_to_f32(&data);
        let remapped = remap_channels(&samples, fmt.n_channels, self.device.channels);
        let resampled = if fmt.n_samples_per_sec == self.device.sample_rate {
            remapped
        } else {
            linear_resample(
                &remapped,
                usize::from(self.device.channels),
                fmt.n_samples_per_sec,
                self.device.sample_rate,
            )
        };

        self.ring.push(&resampled);
    }

    fn set_volume(&mut self, volume: VolumePdu) {
        // Local volume mixing is not implemented — the OS volume control already governs
        // device output, and duplicating it here would just be a second knob to keep in
        // sync. Noted, not applied.
        debug!(?volume, "rdpsnd: server set volume (not applied locally)");
    }

    fn set_pitch(&mut self, pitch: PitchPdu) {
        debug!(?pitch, "rdpsnd: server set pitch (not applied locally)");
    }

    fn close(&mut self) {
        // The server ended the stream (or is about to renegotiate a new format). Drop
        // whatever is still queued rather than let it trail out after the logical close —
        // and do it by replacing the ring with a fresh one of the same shape, since
        // `RingBuf` has no targeted clear that isn't itself a lock-and-drain.
        let drained = self.ring.drain_len();
        debug!(drained, "rdpsnd: stream closed");
    }
}

// ---------------------------------------------------------------------------------------
// Modern dynamic RDPSND transport
// ---------------------------------------------------------------------------------------

/// Carries the existing IronRDP RDPSND state machine over Windows' modern dynamic
/// `AUDIO_PLAYBACK_DVC` transport.
///
/// MS-RDPEA uses the same unframed audio PDUs on both transports. IronRDP 0.9 exposes its
/// client only as a static-channel processor, so this adapter changes framing only: every
/// received DVC payload goes through [`ironrdp_rdpsnd::client::Rdpsnd`] unchanged, and its
/// replies are re-encoded without an SVC header for DRDYNVC to frame.
#[derive(Debug)]
pub struct DynamicRdpsnd {
    inner: Option<ironrdp_rdpsnd::client::Rdpsnd>,
}

/// Creates a fresh RDPSND state machine each time Windows reopens its playback DVC.
///
/// `DrdynvcClient::attach_dynamic_channel` is intentionally single-use. Windows closes
/// `AUDIO_PLAYBACK_DVC` during desktop setup and opens it again, so audio must use the
/// repeatable listener API instead.
#[derive(Debug)]
pub struct DynamicRdpsndListener {
    ring: AudioRing,
    stats: AudioStatsHandle,
    device: AudioFormatSummary,
}

impl DynamicRdpsndListener {
    pub fn new(ring: AudioRing, stats: AudioStatsHandle, device: AudioFormatSummary) -> Self {
        Self {
            ring,
            stats,
            device,
        }
    }
}

impl DvcChannelListener for DynamicRdpsndListener {
    fn channel_name(&self) -> &str {
        DynamicRdpsnd::CHANNEL_NAME
    }

    fn create(&mut self, _channel_id: DynamicChannelId) -> Option<Box<dyn DvcProcessor>> {
        let backend = RdpsndBackend::new(self.ring.clone(), self.stats.clone(), self.device);
        Some(Box::new(DynamicRdpsnd::new(Box::new(backend))))
    }
}

impl DynamicRdpsnd {
    pub const CHANNEL_NAME: &'static str = "AUDIO_PLAYBACK_DVC";

    pub fn new(handler: Box<dyn RdpsndClientHandler>) -> Self {
        Self {
            inner: Some(ironrdp_rdpsnd::client::Rdpsnd::new(handler)),
        }
    }
}

impl_as_any!(DynamicRdpsnd);

#[derive(Debug)]
struct UnframedRdpsndPdu(Vec<u8>);

impl Encode for UnframedRdpsndPdu {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.0.len());
        dst.write_slice(&self.0);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "UnframedRdpsndPdu"
    }

    fn size(&self) -> usize {
        self.0.len()
    }
}

impl DvcEncode for UnframedRdpsndPdu {}

impl DvcProcessor for DynamicRdpsnd {
    fn channel_name(&self) -> &str {
        Self::CHANNEL_NAME
    }

    fn start(&mut self, _channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        Ok(Vec::new())
    }

    fn process(&mut self, _channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        let Some(inner) = self.inner.as_mut() else {
            return Ok(Vec::new());
        };

        inner
            .process(payload)?
            .into_iter()
            .map(|message| {
                let bytes = message
                    .encode_unframed_pdu()
                    .map_err(|error| encode_err!(error))?;
                Ok(Box::new(UnframedRdpsndPdu(bytes)) as DvcMessage)
            })
            .collect()
    }

    fn close(&mut self, _channel_id: u32) {
        // Dropping the processor closes playback for this DVC instance. The repeatable
        // listener creates a fresh processor if Windows reopens the channel.
        self.inner.take();
    }
}

// ---------------------------------------------------------------------------------------
// AudioPlayback — the cpal-owning half
// ---------------------------------------------------------------------------------------

/// Owns the cpal output stream. Its callback does exactly one thing: pull from
/// [`AudioRing`] into the device's buffer. See the module docs for the realtime rules that
/// callback obeys.
///
/// **Not unit tested.** It opens real hardware, which the test environment does not
/// guarantee exists. Everything this struct depends on — ring semantics, resampling,
/// channel mapping — is tested in isolation above; this is the minimal, deliberately thin
/// glue that wires them to cpal.
pub struct AudioPlayback {
    // Held only to keep the stream alive; dropping it stops playback. Never read.
    _stream: Option<cpal::Stream>,
    /// The same gate the stream's error callback latches off on a fatal device error.
    ring: AudioRing,
    format: AudioFormatSummary,
}

impl AudioPlayback {
    /// Open the default output device and start playback fed by `ring`.
    ///
    /// On any failure — no output device, an unsupported config, a stream-build error —
    /// this logs once, counts one `device_errors`, and returns a handle with no stream.
    /// The RDP session must continue without audio rather than fail; the caller does not
    /// need to check anything to get that behaviour.
    /// A deliberately-silent handle: no device opened, `is_active()` false, ring
    /// disabled. The Settings ▸ Audio "playback off" path — unlike a failed
    /// [`start`](Self::start), this counts no device error because nothing failed.
    pub fn disabled(ring: AudioRing) -> Self {
        ring.disable();
        Self {
            _stream: None,
            ring,
            format: AudioFormatSummary {
                sample_rate: 48_000,
                channels: 2,
                bits_per_sample: 16,
            },
        }
    }

    pub fn start(ring: AudioRing, stats: AudioStatsHandle) -> Self {
        match try_start(&ring, &stats) {
            Ok((stream, format)) => Self {
                _stream: Some(stream),
                ring,
                format,
            },
            Err(reason) => {
                error!(%reason, "rdpsnd: no usable audio output device; session continues without audio");
                stats.note(|s| s.device_errors = s.device_errors.saturating_add(1));
                ring.disable();
                Self {
                    _stream: None,
                    ring,
                    // A placeholder for callers that inspect the format. `is_active()` is
                    // false, so the live path does not register RDPSND; the disabled ring
                    // also rejects late packets from tests or alternate callers.
                    format: AudioFormatSummary {
                        sample_rate: 48_000,
                        channels: 2,
                        bits_per_sample: 16,
                    },
                }
            }
        }
    }

    /// The device format audio is being converted to. Feed this to [`RdpsndBackend::new`].
    pub fn format(&self) -> AudioFormatSummary {
        self.format
    }

    /// Whether a real device stream is running. Becomes `false` after a runtime device
    /// failure, even before cpal drops the stream handle.
    pub fn is_active(&self) -> bool {
        self._stream.is_some() && self.ring.is_enabled()
    }
}

fn try_start(
    ring: &AudioRing,
    stats: &AudioStatsHandle,
) -> Result<(cpal::Stream, AudioFormatSummary), String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no default output device".to_string())?;

    let (stream_config, sample_rate, channels) = pick_f32_output_config(&device)?;
    if !SUPPORTED_CHANNELS.contains(&channels) {
        return Err(format!(
            "device reports {channels} output channels; only mono/stereo are handled"
        ));
    }

    let ring_cb = ring.clone();
    let err_stats = stats.clone();
    let err_ring = ring.clone();
    let stream = device
        .build_output_stream(
            &stream_config,
            move |out: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                ring_cb.pop_into(out);
            },
            move |err| {
                // Runs on cpal's own error path (e.g. the device was unplugged). Count and
                // latch the shared gate: the session remains alive, but no later wave can
                // keep filling a ring whose device has gone away.
                error!(%err, "rdpsnd: audio device error");
                err_ring.disable();
                err_stats.note(|s| s.device_errors = s.device_errors.saturating_add(1));
            },
            None,
        )
        .map_err(|e| e.to_string())?;

    stream.play().map_err(|e| e.to_string())?;

    Ok((
        stream,
        AudioFormatSummary {
            sample_rate,
            channels,
            bits_per_sample: 16,
        },
    ))
}

/// Find an f32-capable output config: the default if it is already f32, otherwise the
/// widest-range f32 config the device advertises, at its max sample rate.
///
/// Restricted to f32 deliberately — it is what cpal's callback contract is simplest for,
/// and every device this has been run against offers it. A device that truly offers only
/// integer formats is treated as a device error (no playback) rather than adding an
/// integer-callback code path that nothing here can exercise or verify.
fn pick_f32_output_config(device: &cpal::Device) -> Result<(cpal::StreamConfig, u32, u16), String> {
    let default = device.default_output_config().map_err(|e| e.to_string())?;
    if default.sample_format() == cpal::SampleFormat::F32 {
        return Ok((
            default.config(),
            default.sample_rate().0,
            default.channels(),
        ));
    }

    let f32_range = device
        .supported_output_configs()
        .map_err(|e| e.to_string())?
        .find(|c| c.sample_format() == cpal::SampleFormat::F32)
        .ok_or_else(|| "device offers no f32 output format".to_string())?;
    let chosen = f32_range.with_max_sample_rate();
    Ok((chosen.config(), chosen.sample_rate().0, chosen.channels()))
}

// ---------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp::core::{Decode as _, encode_vec};
    use ironrdp_rdpsnd::pdu::{
        ClientAudioOutputPdu, ServerAudioFormatPdu, ServerAudioOutputPdu, Version,
    };

    fn compressed_format(tag: WaveFormat) -> AudioFormat {
        AudioFormat {
            format: tag,
            n_channels: 2,
            n_samples_per_sec: 48_000,
            n_avg_bytes_per_sec: 48_000 * 4,
            n_block_align: 4,
            bits_per_sample: 16,
            data: None,
        }
    }

    #[test]
    fn dynamic_rdpsnd_uses_the_modern_windows_playback_channel_and_replies_unframed() {
        let stats = AudioStatsHandle::new();
        let ring = AudioRing::with_capacity(4096, stats.clone());
        let mut listener = DynamicRdpsndListener::new(ring, stats, device_native());
        let mut channel = listener.create(9).unwrap();

        assert_eq!(channel.channel_name(), "AUDIO_PLAYBACK_DVC");
        assert!(channel.start(9).unwrap().is_empty());

        let server = ServerAudioOutputPdu::AudioFormat(ServerAudioFormatPdu {
            version: Version::V8,
            formats: vec![pcm_format(48_000, 2)],
        });
        let payload = encode_vec(&server).unwrap();
        let replies = channel.process(9, &payload).unwrap();

        assert_eq!(
            replies.len(),
            2,
            "formats and quality mode must both be returned"
        );
        let first = encode_vec(replies[0].as_ref()).unwrap();
        assert!(matches!(
            ClientAudioOutputPdu::decode(&mut ironrdp::core::ReadCursor::new(&first)).unwrap(),
            ClientAudioOutputPdu::AudioFormat(_)
        ));

        channel.close(9);
        let mut channel = listener.create(10).unwrap();
        assert!(channel.start(10).unwrap().is_empty());
        let replies = channel.process(10, &payload).unwrap();
        assert_eq!(
            replies.len(),
            2,
            "Windows closes and reopens the dynamic channel during desktop setup"
        );
    }

    #[test]
    fn a_wave_index_resolves_against_the_format_list_actually_put_on_the_wire() {
        // The whole point of the vendored RDPSND client. Three links must hold at once:
        // the client sends the intersection in *its own* candidate order, it reports that
        // exact vector back through `set_negotiated_formats`, and `wave()` indexes that
        // vector. This drives all three through one DVC instance, so a break in any link
        // shows up here rather than only against a live host.
        //
        // The server's offer is deliberately in the opposite order to `candidate_formats()`
        // and carries one format we never advertise, so the expected wire list can only be
        // produced by filtering our order — not by echoing the server's, and not by a
        // `HashSet` intersection, which randomises order per process and never calls the
        // callback at all (leaving the negotiated list empty and every wave dropped).
        let stats = AudioStatsHandle::new();
        let ring = AudioRing::with_capacity(4096, stats.clone());
        let mut listener = DynamicRdpsndListener::new(ring.clone(), stats.clone(), device_native());
        let mut channel = listener.create(1).unwrap();

        let server = ServerAudioOutputPdu::AudioFormat(ServerAudioFormatPdu {
            version: Version::V8,
            formats: vec![
                pcm_format(48_000, 2),
                compressed_format(WaveFormat::ADPCM),
                pcm_format(44_100, 1),
            ],
        });
        let replies = channel.process(1, &encode_vec(&server).unwrap()).unwrap();

        let first = encode_vec(replies[0].as_ref()).unwrap();
        let ClientAudioOutputPdu::AudioFormat(sent) =
            ClientAudioOutputPdu::decode(&mut ironrdp::core::ReadCursor::new(&first)).unwrap()
        else {
            panic!("the first reply must be the Client Audio Formats PDU");
        };
        assert_eq!(
            sent.formats,
            vec![pcm_format(44_100, 1), pcm_format(48_000, 2)],
            "the wire list must be our candidate order filtered by the server offer"
        );

        // Reach Ready so Wave2 is accepted.
        let training = ServerAudioOutputPdu::Training(ironrdp_rdpsnd::pdu::TrainingPdu {
            timestamp: 0,
            data: Vec::new(),
        });
        channel.process(1, &encode_vec(&training).unwrap()).unwrap();

        // Index 1 of the wire list is 48000/stereo, which matches the device exactly, so a
        // correct resolution is a bit-for-bit passthrough. Index 0 (44100/mono) would
        // resample and upmix instead, and an empty negotiated list would drop the packet.
        let pcm: Vec<u8> = [1000_i16, -1000, 2000, -2000]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let wave = ServerAudioOutputPdu::Wave2(ironrdp_rdpsnd::pdu::Wave2Pdu {
            timestamp: 0,
            format_no: 1,
            block_no: 0,
            audio_timestamp: 0,
            data: Cow::Borrowed(&pcm),
        });
        channel.process(1, &encode_vec(&wave).unwrap()).unwrap();

        let mut out = [0.0f32; 4];
        ring.pop_into(&mut out);
        assert_eq!(
            out.to_vec(),
            pcm16_le_to_f32(&pcm),
            "format_no must index the list we sent, unaltered at the device's own format"
        );
        assert_eq!(
            stats.snapshot().current_format,
            Some(AudioFormatSummary {
                sample_rate: 48_000,
                channels: 2,
                bits_per_sample: 16
            })
        );
    }

    #[test]
    fn a_silent_session_reports_the_negotiation_that_did_happen() {
        // The reporting bug this pins: `current_format` is only set when a wave plays, so
        // a healthy session with nothing playing on the remote desktop looked identical to
        // a server that never opened an audio channel — and the report called both
        // "negotiated no format".
        let stats = AudioStatsHandle::new();
        let ring = AudioRing::with_capacity(4096, stats.clone());
        let mut listener = DynamicRdpsndListener::new(ring, stats.clone(), device_native());
        let mut channel = listener.create(1).unwrap();

        assert_eq!(
            stats.snapshot().negotiated_formats,
            None,
            "nothing has been exchanged yet, so nothing may be claimed"
        );

        let server = ServerAudioOutputPdu::AudioFormat(ServerAudioFormatPdu {
            version: Version::V8,
            formats: vec![pcm_format(48_000, 2), pcm_format(44_100, 1)],
        });
        channel.process(1, &encode_vec(&server).unwrap()).unwrap();

        let snap = stats.snapshot();
        assert_eq!(
            snap.negotiated_formats,
            Some(2),
            "the exchange completed and must be visible even though nothing played"
        );
        assert_eq!(snap.current_format, None, "no wave played");
        assert_eq!(snap.packets_received, 0);
    }

    #[test]
    fn an_exchange_sharing_no_format_is_distinguishable_from_no_exchange_at_all() {
        let stats = AudioStatsHandle::new();
        let ring = AudioRing::with_capacity(4096, stats.clone());
        let mut listener = DynamicRdpsndListener::new(ring, stats.clone(), device_native());
        let mut channel = listener.create(1).unwrap();

        // Compressed-only offer: we advertise PCM, so the intersection is empty.
        let server = ServerAudioOutputPdu::AudioFormat(ServerAudioFormatPdu {
            version: Version::V8,
            formats: vec![compressed_format(WaveFormat::ADPCM)],
        });
        channel.process(1, &encode_vec(&server).unwrap()).unwrap();

        assert_eq!(
            stats.snapshot().negotiated_formats,
            Some(0),
            "an empty intersection is a real capability gap, not an absent exchange"
        );
    }

    // -- format selection -----------------------------------------------------------

    #[test]
    fn choose_format_picks_a_playable_pcm_format_from_a_mix() {
        let offered = vec![
            compressed_format(WaveFormat::OPUS),
            pcm_format(44_100, 2),
            compressed_format(WaveFormat::WMAUDIO2),
            pcm_format(48_000, 1),
        ];
        let chosen = choose_format(&offered).expect("a playable PCM format is present");
        assert!(
            is_playable(&chosen),
            "chosen format must be one we can play: {chosen:?}"
        );
        assert_eq!(chosen.format, WaveFormat::PCM);
        // Preference is highest sample rate, then stereo — 48000/1 beats 44100/2 on rate.
        assert_eq!(chosen.n_samples_per_sec, 48_000);
        assert_eq!(chosen.n_channels, 1);
    }

    #[test]
    fn choose_format_prefers_stereo_when_rates_tie() {
        let offered = vec![pcm_format(48_000, 1), pcm_format(48_000, 2)];
        let chosen = choose_format(&offered).unwrap();
        assert_eq!(chosen.n_channels, 2, "stereo must win a same-rate tie");
    }

    #[test]
    fn choose_format_rejects_an_offer_of_only_unplayable_formats() {
        let offered = vec![
            compressed_format(WaveFormat::OPUS),
            compressed_format(WaveFormat::MPEGLAYER3),
            // 8-bit PCM: right tag, wrong bit depth — must still be refused.
            AudioFormat {
                format: WaveFormat::PCM,
                n_channels: 2,
                n_samples_per_sec: 48_000,
                n_avg_bytes_per_sec: 48_000 * 2,
                n_block_align: 2,
                bits_per_sample: 8,
                data: None,
            },
        ];
        let result = choose_format(&offered);
        assert_eq!(
            result,
            Err(NoUsableFormat),
            "must be an explicit refusal, not a silent accept"
        );
    }

    #[test]
    fn candidate_formats_are_all_playable_and_cover_the_required_combinations() {
        let formats = candidate_formats();
        assert_eq!(formats.len(), 4);
        for fmt in &formats {
            assert!(
                is_playable(fmt),
                "every advertised format must be one we can play: {fmt:?}"
            );
        }
        for &rate in &SUPPORTED_SAMPLE_RATES {
            for &channels in &SUPPORTED_CHANNELS {
                assert!(
                    formats
                        .iter()
                        .any(|f| f.n_samples_per_sec == rate && f.n_channels == channels),
                    "missing {rate}Hz/{channels}ch"
                );
            }
        }
    }

    // -- channel conversion -----------------------------------------------------------

    #[test]
    fn mono_to_stereo_duplicates_every_sample_and_doubles_frame_count() {
        // A ramp, not a constant, so a duplication bug (e.g. writing the same sample into
        // both output slots regardless of input index) would be visible.
        let mono = [0.1_f32, 0.2, 0.3];
        let stereo = duplicate_mono_to_stereo(&mono);
        assert_eq!(stereo.len(), mono.len() * 2);
        assert_eq!(stereo, vec![0.1, 0.1, 0.2, 0.2, 0.3, 0.3]);
    }

    #[test]
    fn stereo_to_mono_averages_distinct_left_and_right() {
        // Distinct L/R per frame: a channel swap or an off-by-one would change the result.
        let stereo = [1.0_f32, 3.0, 2.0, 4.0];
        let mono = downmix_stereo_to_mono(&stereo);
        assert_eq!(mono, vec![2.0, 3.0]);
    }

    #[test]
    fn remap_channels_is_identity_when_counts_already_match() {
        let samples = [0.5_f32, -0.5, 0.25];
        assert_eq!(remap_channels(&samples, 1, 1), samples.to_vec());
        assert_eq!(remap_channels(&samples, 2, 2), samples.to_vec());
    }

    // -- pcm decode ---------------------------------------------------------------

    #[test]
    fn pcm16_le_decodes_known_values() {
        // i16::MIN, 0, i16::MAX as little-endian bytes.
        let bytes: Vec<u8> = [i16::MIN, 0, i16::MAX]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let samples = pcm16_le_to_f32(&bytes);
        assert_eq!(samples.len(), 3);
        assert!(
            (samples[0] - (-1.0)).abs() < 1e-6,
            "MIN must map to -1.0: {}",
            samples[0]
        );
        assert_eq!(samples[1], 0.0);
        assert!(
            (samples[2] - 0.999969).abs() < 1e-5,
            "MAX must map near +1.0: {}",
            samples[2]
        );
    }

    #[test]
    fn pcm16_le_drops_a_trailing_odd_byte_without_panicking() {
        // Two full frames plus one stray byte — the obvious hostile/truncated payload.
        let bytes = [0x00u8, 0x01, 0x02, 0x03, 0xFF];
        let samples = pcm16_le_to_f32(&bytes);
        assert_eq!(
            samples.len(),
            2,
            "the odd trailing byte must be dropped, not read OOB"
        );
    }

    #[test]
    fn pcm16_le_handles_an_empty_and_a_single_byte_payload() {
        assert_eq!(pcm16_le_to_f32(&[]), Vec::<f32>::new());
        assert_eq!(pcm16_le_to_f32(&[0xAB]), Vec::<f32>::new());
    }

    // -- linear resample ------------------------------------------------------------

    #[test]
    fn resample_upsamples_44100_to_48000_to_the_hand_computed_frame_count() {
        // 441 frames * 48000 / 44100 = 480.0 exactly (44100 * 480 == 21_168_000) — chosen
        // precisely so the expected count is exact integer math done by hand, not derived
        // from the function under test.
        let input: Vec<f32> = (0..441).map(|i| i as f32).collect(); // mono ramp
        let out = linear_resample(&input, 1, 44_100, 48_000);
        assert_eq!(out.len(), 480, "hand-computed: 441 * 48000 / 44100 = 480");
        assert_eq!(
            out[0], 0.0,
            "first output frame must align with the first input frame"
        );
    }

    #[test]
    fn resample_downsamples_48000_to_44100_to_the_hand_computed_frame_count() {
        // 480 frames * 44100 / 48000 = 441.0 exactly — the exact inverse of the case above.
        let input: Vec<f32> = (0..480).map(|i| i as f32).collect();
        let out = linear_resample(&input, 1, 48_000, 44_100);
        assert_eq!(out.len(), 441, "hand-computed: 480 * 44100 / 48000 = 441");
    }

    #[test]
    fn resample_interpolates_between_neighbouring_frames() {
        // Doubling the rate: every other output frame must land exactly on an input frame,
        // and the frames between must be the linear midpoint — a ramp makes a wrong
        // interpolation formula visible instead of coincidentally passing.
        let input = [0.0_f32, 10.0, 20.0, 30.0];
        let out = linear_resample(&input, 1, 10, 20);
        assert_eq!(out.len(), 8, "hand-computed: 4 * 20 / 10 = 8");
        assert_eq!(out[0], 0.0);
        assert_eq!(out[2], 10.0);
        assert_eq!(out[4], 20.0);
        assert!(
            (out[1] - 5.0).abs() < 1e-4,
            "midpoint between 0 and 10: {}",
            out[1]
        );
        assert!(
            (out[3] - 15.0).abs() < 1e-4,
            "midpoint between 10 and 20: {}",
            out[3]
        );
    }

    #[test]
    fn resample_preserves_stereo_frame_pairing() {
        // Interleaved stereo ramp with L and R distinguishable (R = L + 100), so a channel
        // swap or a frame/sample mixup during resampling would be visible. 2 input frames
        // doubled in rate -> hand-computed 4 output frames (8 samples).
        let input = [0.0_f32, 100.0, 10.0, 110.0]; // frame0: L=0 R=100; frame1: L=10 R=110
        let out = linear_resample(&input, 2, 10, 20);
        assert_eq!(
            out.len(),
            8,
            "hand-computed: 2 frames * 20/10 = 4 frames = 8 samples"
        );

        assert_eq!(
            &out[0..2],
            [0.0, 100.0],
            "frame 0 must align exactly with input frame 0"
        );
        assert_eq!(
            &out[4..6],
            [10.0, 110.0],
            "frame 2 must align exactly with input frame 1"
        );
        // Frame 1 is the midpoint: each channel interpolates independently, so R must stay
        // exactly 100 above L rather than drifting or swapping.
        assert!((out[2] - 5.0).abs() < 1e-4, "L midpoint: {}", out[2]);
        assert!(
            (out[3] - (out[2] + 100.0)).abs() < 1e-4,
            "R must track L + 100: {out:?}"
        );
    }

    #[test]
    fn resample_is_a_noop_when_rates_match() {
        let input = [1.0_f32, 2.0, 3.0, 4.0];
        assert_eq!(linear_resample(&input, 2, 48_000, 48_000), input.to_vec());
    }

    #[test]
    fn resample_handles_empty_and_zero_rate_input_without_panicking() {
        assert_eq!(linear_resample(&[], 2, 44_100, 48_000), Vec::<f32>::new());
        assert_eq!(
            linear_resample(&[1.0, 2.0], 2, 0, 48_000),
            Vec::<f32>::new()
        );
        assert_eq!(
            linear_resample(&[1.0, 2.0], 2, 44_100, 0),
            Vec::<f32>::new()
        );
        assert_eq!(
            linear_resample(&[1.0, 2.0], 0, 44_100, 48_000),
            Vec::<f32>::new()
        );
    }

    // -- ring buffer ------------------------------------------------------------

    fn ring(capacity: usize) -> (AudioRing, AudioStatsHandle) {
        let stats = AudioStatsHandle::new();
        (AudioRing::with_capacity(capacity, stats.clone()), stats)
    }

    #[test]
    fn overrun_drops_the_oldest_samples_and_counts_the_drop() {
        // A ramp: if the NEWEST samples were dropped instead of the oldest, or a wrong
        // index were kept, the surviving values would not be [3.0, 4.0, 5.0, 6.0].
        let (ring, stats) = ring(4);
        ring.push(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);

        assert_eq!(
            stats.snapshot().overruns,
            2,
            "two samples must have been evicted"
        );

        let mut out = [0.0f32; 4];
        ring.pop_into(&mut out);
        assert_eq!(
            out,
            [3.0, 4.0, 5.0, 6.0],
            "the OLDEST samples (1.0, 2.0) must be the ones dropped"
        );
        assert_eq!(
            stats.snapshot().underruns,
            0,
            "the buffer was exactly full; no gap here"
        );
    }

    #[test]
    fn underrun_yields_silence_not_stale_or_repeated_data() {
        let (ring, stats) = ring(8);
        ring.push(&[9.0, 9.0]); // only 2 samples available

        // Pre-fill `out` with a recognisable non-zero pattern so a bug that leaves stale
        // buffer content in place (instead of writing 0.0) is visible.
        let mut out = [42.0f32; 4];
        ring.pop_into(&mut out);

        assert_eq!(
            out,
            [9.0, 9.0, 0.0, 0.0],
            "missing samples must be silence, not stale/repeated data"
        );
        assert_eq!(stats.snapshot().underruns, 1);
    }

    #[test]
    fn a_ring_fed_exactly_to_capacity_neither_overruns_nor_underruns() {
        let (ring, stats) = ring(4);
        ring.push(&[1.0, 2.0, 3.0, 4.0]);
        let mut out = [0.0f32; 4];
        ring.pop_into(&mut out);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(stats.snapshot().overruns, 0);
        assert_eq!(stats.snapshot().underruns, 0);
    }

    #[test]
    fn an_underrun_episode_counts_once_however_long_the_dry_spell_lasts() {
        let (ring, stats) = ring(8);
        ring.push(&[1.0]); // audio must have flowed at least once for underruns to count
        let mut out = [1.0f32; 4];
        ring.pop_into(&mut out); // 1 real sample + 3 gap slots: the episode starts
        ring.pop_into(&mut out); // still dry: same episode, not a second underrun
        assert_eq!(out, [0.0; 4]);
        assert_eq!(
            stats.snapshot().underruns,
            1,
            "one dry spell is one audible gap, not one count per device pull"
        );

        ring.push(&[2.0]); // audio resumes …
        ring.pop_into(&mut out); // … and runs dry again: a NEW episode
        assert_eq!(stats.snapshot().underruns, 2);
    }

    #[test]
    fn underruns_are_not_counted_before_any_audio_has_ever_flowed() {
        // Mirrors AudioPlayback::start(): cpal's callback starts pulling from the ring the
        // instant the device opens, which is before the RDP session has sent a single
        // Wave2 packet — often before the connection even exists. That pre-connection
        // silence is expected, not a glitch, and must not inflate the counter (defect 2:
        // ~94 phantom underruns/sec at 48kHz/512-frame buffers, forever, even on a session
        // where audio worked perfectly).
        let (ring, stats) = ring(8);
        let mut out = [3.0f32; 4];
        for _ in 0..50 {
            ring.pop_into(&mut out);
        }
        assert_eq!(out, [0.0; 4], "still filled with silence");
        assert_eq!(
            stats.snapshot().underruns,
            0,
            "silence before any audio ever arrived must not count as an underrun"
        );
    }

    #[test]
    fn underruns_are_counted_once_audio_has_flowed_and_the_ring_runs_dry_mid_stream() {
        let (ring, stats) = ring(8);
        ring.push(&[1.0, 2.0]); // audio has now genuinely flowed

        let mut out = [0.0f32; 2];
        ring.pop_into(&mut out); // drains exactly what was pushed: no gap
        assert_eq!(stats.snapshot().underruns, 0, "an exact drain is not a gap");

        ring.pop_into(&mut out); // ring is now empty: a real, audible underrun
        assert_eq!(
            stats.snapshot().underruns,
            1,
            "running dry after audio has flowed must be counted"
        );
    }

    #[test]
    fn drain_len_clears_the_buffer_and_reports_what_it_removed() {
        let (ring, _stats) = ring(8);
        ring.push(&[1.0, 2.0, 3.0]);
        assert_eq!(ring.drain_len(), 3);
        let mut out = [7.0f32; 2];
        ring.pop_into(&mut out);
        assert_eq!(out, [0.0, 0.0], "nothing must survive a drain");
    }

    // -- RdpsndBackend ------------------------------------------------------------

    fn backend(device: AudioFormatSummary) -> (RdpsndBackend, AudioRing, AudioStatsHandle) {
        let stats = AudioStatsHandle::new();
        let ring = AudioRing::with_capacity(4096, stats.clone());
        let mut backend = RdpsndBackend::new(ring.clone(), stats.clone(), device);
        let negotiated = backend.get_formats().to_vec();
        backend.set_negotiated_formats(&negotiated);
        (backend, ring, stats)
    }

    fn device_native() -> AudioFormatSummary {
        AudioFormatSummary {
            sample_rate: 48_000,
            channels: 2,
            bits_per_sample: 16,
        }
    }

    #[test]
    fn get_formats_advertises_every_playable_pcm_path() {
        let (backend, _ring, _stats) = backend(device_native());
        let formats = backend.get_formats();
        assert_eq!(formats, candidate_formats());
        assert!(formats.iter().all(is_playable));
    }

    #[test]
    fn negotiated_format_callback_replaces_candidates_in_the_exact_wire_order() {
        let (mut backend, _ring, _stats) = backend(device_native());
        let negotiated = vec![pcm_format(48_000, 2), pcm_format(44_100, 1)];

        backend.set_negotiated_formats(&negotiated);

        assert_eq!(backend.negotiated_formats, negotiated);
        assert_eq!(backend.get_formats(), candidate_formats());
    }

    #[test]
    fn wave_resolves_format_no_against_the_negotiated_list_not_candidate_formats() {
        // The historical bug: wave() indexed `candidate_formats()` — our own fixed
        // 4-entry list — instead of whatever list was actually negotiated/sent. This
        // fixture is built so index 1 means something different in each list: in
        // `candidate_formats()`, index 1 is 44100Hz/stereo; here, deliberately, it is
        // 48000Hz/stereo. If `wave()` ever regresses to indexing `candidate_formats()`
        // again, this test must fail by resolving the wrong rate.
        let negotiated = vec![pcm_format(44_100, 1), pcm_format(48_000, 2)];
        assert_ne!(
            negotiated[1],
            candidate_formats()[1],
            "fixture must diverge from candidate_formats() at this index or the test proves nothing"
        );

        let stats = AudioStatsHandle::new();
        let ring = AudioRing::with_capacity(4096, stats.clone());
        let device = device_native(); // 48000/2 — matches negotiated[1] exactly: no remap/resample
        let mut backend =
            RdpsndBackend::with_formats(negotiated, ring.clone(), stats.clone(), device);

        // Two distinguishable stereo frames.
        let pcm: Vec<u8> = [1000_i16, -1000, 2000, -2000]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        backend.wave(1, 0, Cow::Borrowed(&pcm));

        let mut out = [0.0f32; 4];
        ring.pop_into(&mut out);
        // Format matches the device exactly (48000/2), so this must be an unaltered
        // passthrough. Resolving to candidate_formats()[1] (44100/stereo) instead would
        // trigger a resample here, which would NOT reproduce the input bit-for-bit.
        let expected = pcm16_le_to_f32(&pcm);
        assert_eq!(
            out.to_vec(),
            expected,
            "index 1 must resolve to negotiated[1] (48000/stereo), unaltered"
        );

        let snap = stats.snapshot();
        assert_eq!(
            snap.current_format,
            Some(AudioFormatSummary {
                sample_rate: 48_000,
                channels: 2,
                bits_per_sample: 16
            }),
            "must report the negotiated format actually used, not candidate_formats()'s"
        );
    }

    #[test]
    fn wave_with_an_index_past_the_negotiated_list_is_dropped_not_guessed() {
        // A negotiated list of exactly one entry makes format_no=1 precisely the shape of
        // the original bug: a small, valid-looking index that
        // used to resolve into candidate_formats()'s second entry instead of being
        // recognised as unresolvable.
        let negotiated = vec![pcm_format(44_100, 1)];
        let stats = AudioStatsHandle::new();
        let ring = AudioRing::with_capacity(4096, stats.clone());
        let mut backend =
            RdpsndBackend::with_formats(negotiated, ring.clone(), stats.clone(), device_native());

        backend.wave(1, 0, Cow::Borrowed(&[1, 2, 3, 4]));

        let mut out = [7.0f32; 4];
        ring.pop_into(&mut out);
        assert_eq!(
            out, [0.0; 4],
            "an unresolvable index must never produce played audio"
        );
        assert_eq!(stats.snapshot().bytes_played, 0);
        assert_eq!(stats.snapshot().current_format, None);
    }

    #[test]
    fn wave_for_a_format_matching_the_device_queues_samples_unchanged() {
        let (mut backend, ring, stats) = backend(device_native());
        // The helper simulates negotiation of all candidates in advertised order.
        let format_no = backend
            .get_formats()
            .iter()
            .position(|f| f.n_samples_per_sec == 48_000 && f.n_channels == 2)
            .expect("48000/stereo must be advertised");

        let pcm: Vec<u8> = [100_i16, -100, 200, -200]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        backend.wave(format_no, 0, Cow::Borrowed(&pcm));

        let mut out = [0.0f32; 4];
        ring.pop_into(&mut out);
        assert!(
            out.iter().any(|&s| s != 0.0),
            "samples must have reached the ring: {out:?}"
        );

        let snap = stats.snapshot();
        assert_eq!(snap.packets_received, 1);
        assert_eq!(snap.bytes_played, pcm.len() as u64);
        assert_eq!(
            snap.current_format,
            Some(AudioFormatSummary {
                sample_rate: 48_000,
                channels: 2,
                bits_per_sample: 16
            })
        );
    }

    #[test]
    fn a_disabled_playback_ring_rejects_late_wave_packets() {
        let (mut backend, ring, stats) = backend(device_native());
        ring.disable();

        let pcm: Vec<u8> = [100_i16, -100]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        backend.wave(0, 0, Cow::Borrowed(&pcm));

        let mut out = [7.0f32; 2];
        ring.pop_into(&mut out);
        assert_eq!(
            out, [0.0; 2],
            "late packets must not enter a failed playback ring"
        );
        assert_eq!(stats.snapshot().packets_received, 1);
        assert_eq!(stats.snapshot().bytes_played, 0);
    }

    #[test]
    fn wave_remaps_a_negotiated_mono_format_into_a_stereo_device_target() {
        // The server may negotiate mono while the selected device is stereo, so this pins
        // the live remapping path directly via `with_formats`.
        let negotiated = vec![pcm_format(48_000, 1)];
        let stats = AudioStatsHandle::new();
        let ring = AudioRing::with_capacity(4096, stats.clone());
        let mut backend =
            RdpsndBackend::with_formats(negotiated, ring.clone(), stats.clone(), device_native());

        let pcm: Vec<u8> = [1000_i16, 2000]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        backend.wave(0, 0, Cow::Borrowed(&pcm));

        let mut out = [0.0f32; 4];
        ring.pop_into(&mut out);
        // Mono->stereo duplication: sample 0 repeats at [0]/[1], sample 1 at [2]/[3].
        assert_eq!(out[0], out[1], "mono sample must be duplicated across L/R");
        assert_eq!(out[2], out[3]);
        assert_ne!(
            out[0], out[2],
            "the two source samples must remain distinct"
        );
    }

    #[test]
    fn wave_with_an_out_of_range_format_index_is_dropped_not_panicked() {
        let (mut backend, ring, stats) = backend(device_native());
        backend.wave(99, 0, Cow::Borrowed(&[1, 2, 3, 4]));

        let mut out = [7.0f32; 4];
        ring.pop_into(&mut out);
        assert_eq!(out, [0.0; 4], "nothing should have been queued");
        assert_eq!(
            stats.snapshot().packets_received,
            1,
            "still counted as received"
        );
        assert_eq!(stats.snapshot().bytes_played, 0, "but not as played");
    }

    #[test]
    fn close_drains_whatever_was_still_queued() {
        let (mut backend, ring, _stats) = backend(device_native());
        ring.push(&[1.0, 2.0, 3.0]);
        backend.close();

        let mut out = [9.0f32; 3];
        ring.pop_into(&mut out);
        assert_eq!(out, [0.0; 3], "close must not let stale audio trail out");
    }
}
