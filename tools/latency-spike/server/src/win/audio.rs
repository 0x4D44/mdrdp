//! WASAPI loopback capture — the host's real audio source.
//!
//! This path is **inert until asked**: reachable only through
//! `--audio-source loopback`, which nothing sets by default, and returning
//! [`Captured::Unavailable`] forever if the endpoint is missing rather than
//! failing a session. Portable conversion tests cover the pure behavior; the
//! Windows COM and device-policy path needs live fleet-host evidence as well.

use std::cell::Cell;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
};
use windows::Win32::Media::Audio::{WAVEFORMATEX, WAVEFORMATEXTENSIBLE};
use windows::Win32::Media::KernelStreaming::KSDATAFORMAT_SUBTYPE_PCM;
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
};

/// `WAVE_FORMAT_EXTENSIBLE`.
const WAVE_FORMAT_EXTENSIBLE_TAG: u16 = 0xFFFE;
/// `WAVE_FORMAT_IEEE_FLOAT`.
const WAVE_FORMAT_IEEE_FLOAT_TAG: u16 = 3;
/// `WAVE_FORMAT_PCM`.
const WAVE_FORMAT_PCM_TAG: u16 = 1;

use crate::audio_source::{AudioSource, Captured};
use crate::aux_proto::AudioFrame;
use crate::win::audio_policy;

/// The capture buffer we ask WASAPI for, in 100-nanosecond units.
///
/// 200 ms. Comfortably more than the 10 ms we read at a time, so an occasional
/// late poll costs latency rather than samples.
const BUFFER_DURATION_100NS: i64 = 2_000_000;

thread_local! {
    /// Whether COM has been initialised **on this thread**.
    static COM_READY: Cell<bool> = const { Cell::new(false) };
}

/// Initialise COM on the calling thread, once per thread.
///
/// **This was a process-global `Once` and that was wrong.** COM initialisation
/// is per-thread, and the host creates a fresh `aux-audio` thread for every
/// connection — so with a global guard only the very first thread of the
/// process ever called `CoInitializeEx`, and every later one went on to call
/// `CoCreateInstance` on an uninitialised apartment. The first connection would
/// have worked and every subsequent one failed, which is close to the worst
/// possible failure shape: it looks like a flaky feature rather than a bug.
/// Caught by a cross-family review; the module's own comment had asserted the
/// per-thread property that the code did not provide.
pub(crate) fn ensure_com() {
    COM_READY.with(|ready| {
        if !ready.get() {
            // SAFETY: first COM call on this thread.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            ready.set(true);
        }
    });
}

/// How samples are encoded in the endpoint's mix format.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SampleKind {
    F32,
    I16,
}

/// What the endpoint's mix format is, reduced to what the converter needs.
#[derive(Clone, Copy)]
struct MixFormat {
    sample_rate: u32,
    /// The endpoint's **real** channel count — what WASAPI actually interleaves.
    ///
    /// Kept separate from [`Self::send_channels`] because an earlier version
    /// clamped this to 2 and then read `frames * 2` consecutive samples. On a
    /// 5.1 endpoint WASAPI supplies six samples per frame, so that read took the
    /// first two samples of each *frame group* without striding — rotating
    /// speaker channels and time positions through left and right. It would have
    /// produced confident garbage on the first real multichannel host.
    device_channels: u16,
    /// How many channels we put on the wire: 1 or 2.
    send_channels: u8,
    kind: SampleKind,
}

/// Loopback capture from the default render endpoint.
pub struct LoopbackCapture {
    /// `None` once we have concluded there is nothing to capture from. Set at
    /// construction and never retried: a machine does not grow a sound card
    /// mid-session, and retrying would spin.
    inner: Option<Active>,
    /// Chunks left over after one WASAPI packet was split to the wire ceiling.
    pending: VecDeque<AudioFrame>,
    quiet: QuietBoundary,
}

const QUIET_BOUNDARY_AFTER: Duration = Duration::from_millis(100);

#[derive(Default)]
struct QuietBoundary {
    last_audio: Option<Instant>,
    explicit_silence: bool,
    sent: bool,
}

impl QuietBoundary {
    fn note_audio(&mut self, now: Instant) {
        self.last_audio = Some(now);
        self.sent = false;
    }

    fn note_silence(&mut self) {
        self.explicit_silence = true;
    }

    fn take(&mut self, now: Instant) -> bool {
        let quiet_long_enough = self
            .last_audio
            .is_some_and(|last| now.saturating_duration_since(last) >= QUIET_BOUNDARY_AFTER);
        if !self.sent && (self.explicit_silence || quiet_long_enough) {
            self.explicit_silence = false;
            self.sent = true;
            true
        } else {
            false
        }
    }
}

struct Active {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    format: MixFormat,
    /// Keeps the per-user default lease and the inter-process capture guard
    /// alive for exactly the lifetime of this source.
    _capture_target: Option<audio_policy::CaptureTarget>,
}

/// Read a `WAVEFORMATEX` (possibly a `WAVEFORMATEXTENSIBLE`) into what the
/// converter needs.
///
/// **Reading only `wBitsPerSample` is not enough**, which is what an earlier
/// version did. The shared-mode mix format is normally
/// `WAVEFORMATEXTENSIBLE`, whose `SubFormat` GUID — not the bit width — says
/// whether samples are float or integer. A 32-bit *integer* endpoint would have
/// been reinterpreted as float, which is not quiet corruption: it is full-scale
/// noise.
///
/// # Safety
/// `ptr` must be a valid `WAVEFORMATEX` from `GetMixFormat`.
unsafe fn parse_mix_format(ptr: *const WAVEFORMATEX) -> Result<MixFormat, String> {
    // SAFETY: the caller guarantees a valid pointer.
    let wf = unsafe { &*ptr };
    let bits = wf.wBitsPerSample;
    let kind = match wf.wFormatTag {
        // 0xFFFE: the real format is in the extensible header's SubFormat.
        WAVE_FORMAT_EXTENSIBLE_TAG => {
            if wf.cbSize < 22 {
                return Err("extensible mix format with a truncated header".to_owned());
            }
            // SAFETY: cbSize >= 22 means the extensible fields are present.
            let ext = unsafe { &*(ptr as *const WAVEFORMATEXTENSIBLE) };
            let sub = ext.SubFormat;
            if sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT && bits == 32 {
                SampleKind::F32
            } else if sub == KSDATAFORMAT_SUBTYPE_PCM && bits == 16 {
                SampleKind::I16
            } else {
                return Err(format!(
                    "unsupported extensible mix format ({bits} bits) — refusing \
                     rather than guessing, because guessing wrong is full-scale noise"
                ));
            }
        }
        WAVE_FORMAT_IEEE_FLOAT_TAG if bits == 32 => SampleKind::F32,
        WAVE_FORMAT_PCM_TAG if bits == 16 => SampleKind::I16,
        other => {
            return Err(format!(
                "unsupported mix format tag 0x{other:04x} at {bits} bits"
            ));
        }
    };
    let device_channels = wf.nChannels;
    if device_channels == 0 {
        return Err("mix format reports zero channels".to_owned());
    }
    Ok(MixFormat {
        sample_rate: wf.nSamplesPerSec,
        device_channels,
        // Mono stays mono; anything wider is carried as stereo.
        send_channels: if device_channels == 1 { 1 } else { 2 },
        kind,
    })
}

impl LoopbackCapture {
    /// Open the default render endpoint for loopback capture.
    ///
    /// Never returns an error: a host with no endpoint is a supported
    /// configuration, and the caller's job is to carry on without audio.
    pub fn new() -> Self {
        ensure_com();
        match Self::open() {
            Ok(active) => Self {
                inner: Some(active),
                pending: VecDeque::new(),
                quiet: QuietBoundary::default(),
            },
            Err(why) => {
                // Once, at construction. A message per 10 ms block would turn a
                // supported state into a fault report.
                eprintln!("audio: loopback capture unavailable ({why})");
                Self {
                    inner: None,
                    pending: VecDeque::new(),
                    quiet: QuietBoundary::default(),
                }
            }
        }
    }

    fn open() -> Result<Active, String> {
        // SAFETY: standard COM activation of the endpoint enumerator.
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                .map_err(|e| format!("no device enumerator: {e}"))?;
        let capture_target = match audio_policy::begin_capture() {
            Ok(target) => Some(target),
            Err(error) => {
                // Preserve the pre-tranche behavior on hosts without CABLE and
                // on Windows builds where the private policy interface fails.
                // Automatic default switching is only ever attempted for a
                // positively identified base CABLE endpoint.
                eprintln!("audio: VB-CABLE policy unavailable ({error}); using current default");
                None
            }
        };
        let device = if let Some(target) = capture_target.as_ref() {
            let endpoint_id_wide: Vec<u16> = target
                .render_id
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: the endpoint ID is a NUL-terminated string owned for this call.
            unsafe {
                enumerator.GetDevice(windows::core::PCWSTR::from_raw(endpoint_id_wide.as_ptr()))
            }
            .map_err(|e| format!("cannot open selected VB-CABLE render endpoint: {e}"))?
        } else {
            // SAFETY: a valid enumerator.
            unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
                .map_err(|e| format!("no default render endpoint: {e}"))?
        };
        // SAFETY: a valid device, no activation parameters.
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
            .map_err(|e| format!("cannot activate the audio client: {e}"))?;
        // SAFETY: a freshly activated client.
        let format_ptr =
            unsafe { client.GetMixFormat() }.map_err(|e| format!("no mix format: {e}"))?;
        // SAFETY: GetMixFormat returned a valid pointer to a WAVEFORMATEX.
        let parsed = unsafe { parse_mix_format(format_ptr) };
        let format = match parsed {
            Ok(f) => f,
            Err(why) => {
                // **Free it on the error path too.** The buffer is ours the
                // moment GetMixFormat succeeds, whatever we then decide about it.
                // SAFETY: a CoTaskMem allocation returned by GetMixFormat.
                unsafe { CoTaskMemFree(Some(format_ptr as *const _)) };
                return Err(why);
            }
        };
        if capture_target.is_some() && format.sample_rate != audio_policy::SAMPLE_RATE {
            // SAFETY: a CoTaskMem allocation returned by GetMixFormat.
            unsafe { CoTaskMemFree(Some(format_ptr as *const _)) };
            return Err(format!(
                "selected VB-CABLE endpoint reports {} Hz, expected {} Hz",
                format.sample_rate,
                audio_policy::SAMPLE_RATE
            ));
        }
        // SAFETY: a valid client and the mix format it just handed us.
        let init = unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                BUFFER_DURATION_100NS,
                0,
                format_ptr,
                None,
            )
        }
        .map_err(|e| format!("cannot initialise loopback: {e}"));
        // Initialize copies what it needs; the buffer is ours to release either
        // way. Freeing before the `?` is what keeps the failure path leak-free.
        // SAFETY: a CoTaskMem allocation returned by GetMixFormat, released once.
        unsafe { CoTaskMemFree(Some(format_ptr as *const _)) };
        init?;
        // SAFETY: an initialised client.
        let capture: IAudioCaptureClient =
            unsafe { client.GetService() }.map_err(|e| format!("no capture service: {e}"))?;
        // SAFETY: an initialised client with a capture service.
        unsafe { client.Start() }.map_err(|e| format!("cannot start capture: {e}"))?;
        // **Say what is being captured.** Endpoint loopback takes the system mix,
        // not this session's audio, so anything else rendering to the same
        // endpoint — a service, a notification — is relayed to the client too.
        // Accepted deliberately (see the HLD's review record: per-session capture
        // is not something WASAPI offers), and stated here because a compromise
        // nobody can see is indistinguishable from a defect.
        eprintln!(
            "audio: capturing the system mix at {} Hz, {} ch — this includes \
             system sounds and any other audio on this endpoint, not only the \
             session's own",
            format.sample_rate, format.device_channels
        );
        Ok(Active {
            client,
            capture,
            format,
            _capture_target: capture_target,
        })
    }
}

impl Default for LoopbackCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for LoopbackCapture {
    fn drop(&mut self) {
        if let Some(active) = self.inner.take() {
            // SAFETY: a started client. Stop is idempotent enough that failing
            // here is not worth reporting during teardown.
            unsafe {
                let _ = active.client.Stop();
            }
        }
    }
}

impl AudioSource for LoopbackCapture {
    fn next_block(&mut self) -> Captured {
        if let Some(frame) = self.pending.pop_front() {
            return Captured::Frame(frame);
        }
        let Some(active) = self.inner.as_ref() else {
            return Captured::Unavailable;
        };
        let stride = active.format.device_channels as usize;
        let out_ch = active.format.send_channels as usize;
        let mut silent_frames = 0u64;

        // Drain every packet currently ready. Coalescing adjacent packets lets
        // one lap recover from a scheduler delay instead of remaining one
        // packet behind forever. queue_pcm still enforces the wire ceiling.
        loop {
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames: u32 = 0;
            let mut flags: u32 = 0;
            let mut device_pos: u64 = 0;
            // SAFETY: a started capture client; every out-parameter is owned here.
            let hr = unsafe {
                active.capture.GetBuffer(
                    &mut data,
                    &mut frames,
                    &mut flags,
                    Some(&mut device_pos),
                    None,
                )
            };
            if let Err(e) = hr {
                eprintln!("audio: capture read failed ({e}); stopping");
                self.inner = None;
                return Captured::Unavailable;
            }
            if frames == 0 {
                // `AUDCLNT_S_BUFFER_EMPTY` is a successful zero-frame poll.
                unsafe {
                    let _ = active.capture.ReleaseBuffer(0);
                }
                break;
            }

            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
            if silent || data.is_null() {
                unsafe {
                    let _ = active.capture.ReleaseBuffer(frames);
                }
                silent_frames = silent_frames.saturating_add(u64::from(frames));
                self.quiet.note_silence();
                // Preserve the quiet boundary rather than folding audio from a
                // later packet into the frame before it.
                break;
            }

            let now = Instant::now();
            if self.quiet.take(now) {
                self.pending.push_back(AudioFrame {
                    sample_rate: active.format.sample_rate,
                    channels: active.format.send_channels,
                    capture_pos: device_pos,
                    pcm: Vec::new(),
                });
            }
            self.quiet.note_audio(now);
            let mut pcm = Vec::with_capacity(frames as usize * out_ch * 2);
            // SAFETY: WASAPI guarantees `frames * stride` samples at `data` in
            // the validated width for the lifetime of this buffer.
            unsafe {
                append_samples(
                    &mut pcm,
                    data,
                    frames as usize,
                    stride,
                    out_ch,
                    active.format.kind,
                );
                let _ = active.capture.ReleaseBuffer(frames);
            }
            queue_pcm(
                &mut self.pending,
                active.format.sample_rate,
                active.format.send_channels,
                device_pos,
                pcm,
            );
        }

        if let Some(frame) = self.pending.pop_front() {
            Captured::Frame(frame)
        } else if self.quiet.take(Instant::now()) {
            Captured::Frame(AudioFrame {
                sample_rate: active.format.sample_rate,
                channels: active.format.send_channels,
                capture_pos: 0,
                pcm: Vec::new(),
            })
        } else if silent_frames > 0 {
            Captured::Silence {
                frames: silent_frames,
            }
        } else {
            Captured::Empty
        }
    }

    fn describe(&self) -> &'static str {
        if self.inner.is_some() {
            "loopback"
        } else {
            "loopback (unavailable)"
        }
    }
}

fn queue_pcm(
    pending: &mut VecDeque<AudioFrame>,
    sample_rate: u32,
    channels: u8,
    capture_pos: u64,
    pcm: Vec<u8>,
) {
    let stride = usize::from(channels) * 2;
    if stride == 0 || pcm.is_empty() || !pcm.len().is_multiple_of(stride) {
        return;
    }

    let mut offset = 0usize;
    while offset < pcm.len() {
        if let Some(last) = pending.back_mut() {
            let last_frames = last.pcm.len() / stride;
            let contiguous = last.sample_rate == sample_rate
                && last.channels == channels
                && last.capture_pos.saturating_add(last_frames as u64)
                    == capture_pos.saturating_add((offset / stride) as u64);
            let capacity = crate::aux_proto::MAX_AUDIO_BYTES.saturating_sub(last.pcm.len());
            let take = capacity.min(pcm.len() - offset) / stride * stride;
            if contiguous && take > 0 {
                last.pcm.extend_from_slice(&pcm[offset..offset + take]);
                offset += take;
                continue;
            }
        }

        let take = crate::aux_proto::MAX_AUDIO_BYTES.min(pcm.len() - offset) / stride * stride;
        if take == 0 {
            break;
        }
        pending.push_back(AudioFrame {
            sample_rate,
            channels,
            capture_pos: capture_pos.saturating_add((offset / stride) as u64),
            pcm: pcm[offset..offset + take].to_vec(),
        });
        offset += take;
    }
}

/// Convert one packet into interleaved 16-bit LE, striding over any channels we
/// do not carry.
///
/// # Safety
/// `data` must hold `frames * stride` samples of `kind`'s width.
unsafe fn append_samples(
    out: &mut Vec<u8>,
    data: *const u8,
    frames: usize,
    stride: usize,
    out_ch: usize,
    kind: SampleKind,
) {
    out.reserve(frames * out_ch * 2);
    for f in 0..frames {
        for c in 0..out_ch {
            let idx = f * stride + c;
            let v = match kind {
                SampleKind::F32 => {
                    // SAFETY: idx < frames * stride, within the packet.
                    let s = unsafe { *(data as *const f32).add(idx) };
                    // Clamp before scaling: a shared-mode mix buffer may legally
                    // exceed +/-1.0, and letting that wrap an i16 turns a loud
                    // passage into noise rather than clipping it.
                    (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
                }
                // SAFETY: idx < frames * stride, within the packet.
                SampleKind::I16 => unsafe { *(data as *const i16).add(idx) },
            };
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
}

/// Whether this host has a default audio render endpoint.
///
/// The question the health ladder's `audio` rung asks, and the same one
/// `tools/audio-probe` answers standalone. Three-valued: `None` means the
/// enumerator itself could not be created, which is a different problem from
/// having no endpoint and deserves a different answer.
///
/// Read-only and cheap — it enumerates, it does not open a capture stream — so
/// it is safe to call on every status poll.
pub fn endpoint_available() -> Option<bool> {
    ensure_com();
    // SAFETY: standard COM activation of the endpoint enumerator.
    let enumerator: IMMDeviceEnumerator =
        match unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) } {
            Ok(e) => e,
            Err(_) => return None,
        };
    // SAFETY: a valid enumerator.
    Some(unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }.is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_packets_split_on_frames_and_keep_device_positions() {
        let mut pending = VecDeque::new();
        queue_pcm(
            &mut pending,
            48_000,
            2,
            100,
            vec![0; crate::aux_proto::MAX_AUDIO_BYTES + 8],
        );
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].pcm.len(), crate::aux_proto::MAX_AUDIO_BYTES);
        assert_eq!(pending[0].capture_pos, 100);
        assert_eq!(
            pending[1].capture_pos,
            100 + (crate::aux_proto::MAX_AUDIO_BYTES / 4) as u64
        );
        assert_eq!(pending[1].pcm.len(), 8);
    }

    #[test]
    fn contiguous_small_packets_coalesce_while_draining_wasapi() {
        let mut pending = VecDeque::new();
        queue_pcm(&mut pending, 48_000, 2, 10, vec![1; 8]);
        queue_pcm(&mut pending, 48_000, 2, 12, vec![2; 8]);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].capture_pos, 10);
        assert_eq!(pending[0].pcm, [vec![1; 8], vec![2; 8]].concat());
    }

    #[test]
    fn quiet_boundary_is_emitted_once_until_audio_resumes() {
        let start = Instant::now();
        let mut quiet = QuietBoundary::default();
        quiet.note_audio(start);
        assert!(!quiet.take(start + Duration::from_millis(99)));
        assert!(quiet.take(start + Duration::from_millis(100)));
        assert!(!quiet.take(start + Duration::from_secs(1)));
        quiet.note_audio(start + Duration::from_secs(2));
        quiet.note_silence();
        assert!(quiet.take(start + Duration::from_secs(2)));
        assert!(!quiet.take(start + Duration::from_secs(3)));
    }
}
