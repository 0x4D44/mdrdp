//! WASAPI loopback capture — the host's real audio source.
//!
//! # This code has never executed. Not once.
//!
//! Neither fleet host has an audio render endpoint (quench 6 endpoints / 0
//! active, temper 5 / 0 active; both return `0x80070490` from
//! `GetDefaultAudioEndpoint` — see `wrk_docs/evidence/`). Loopback capture
//! attaches to a render endpoint, so on those machines there is nothing to
//! attach to, and this module type-checks and has never run a single
//! instruction. Everything below is written from the documented contract, not
//! from observed behaviour.
//!
//! It is written to be **inert until asked**: reachable only through
//! `--audio-source loopback`, which nothing sets by default, and returning
//! [`Captured::Unavailable`] forever if the endpoint is missing rather than
//! failing a session. That is the safe shape for code that cannot be tested —
//! the worst it can do on a host without audio is nothing at all.
//!
//! Treat any behaviour here as unverified until a host with a working endpoint
//! exists. The first person to run it should expect to find something.

use std::cell::Cell;

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
fn ensure_com() {
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
    /// Advances across silence as well as audio, because the client needs to
    /// tell "the desktop was quiet" from "frames were lost".
    capture_pos: u64,
}

struct Active {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    format: MixFormat,
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
            ))
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
                capture_pos: 0,
            },
            Err(why) => {
                // Once, at construction. A message per 10 ms block would turn a
                // supported state into a fault report.
                eprintln!("audio: loopback capture unavailable ({why})");
                Self {
                    inner: None,
                    capture_pos: 0,
                }
            }
        }
    }

    fn open() -> Result<Active, String> {
        // SAFETY: standard COM activation of the endpoint enumerator.
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                .map_err(|e| format!("no device enumerator: {e}"))?;
        // SAFETY: a valid enumerator.
        let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
            .map_err(|e| format!("no default render endpoint: {e}"))?;
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
        let Some(active) = self.inner.as_ref() else {
            return Captured::Unavailable;
        };
        let stride = active.format.device_channels as usize;
        let out_ch = active.format.send_channels as usize;
        let max_frames = crate::aux_proto::MAX_AUDIO_BYTES / (out_ch * 2);

        let mut pcm: Vec<u8> = Vec::new();
        let mut start_pos: Option<u64> = None;
        let mut silent_frames = 0u64;

        // **Drain, do not take one packet per tick.** WASAPI packet sizes vary
        // and the documented contract is to consume every available packet each
        // pass. Taking exactly one per 10 ms tick means a single scheduler delay
        // leaves a packet queued forever after: we consume one while the endpoint
        // produces another, so the backlog never clears and latency ratchets up to
        // the buffer size and then starts losing samples. The pacing loop's
        // deliberate "do not catch up" rule would have made that permanent.
        loop {
            if pcm.len() / (out_ch * 2) >= max_frames {
                break;
            }
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
                // Nothing more ready. `AUDCLNT_S_BUFFER_EMPTY` is a *success*
                // HRESULT, so it arrives here as zero frames rather than as an
                // error — an earlier draft matched it as an error branch, which
                // was dead code pretending to handle a case it could never see.
                // SAFETY: paired with the GetBuffer above.
                unsafe {
                    let _ = active.capture.ReleaseBuffer(0);
                }
                break;
            }

            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
            if silent || data.is_null() {
                // SAFETY: paired with the GetBuffer above.
                unsafe {
                    let _ = active.capture.ReleaseBuffer(frames);
                }
                silent_frames += u64::from(frames);
                self.capture_pos = device_pos + u64::from(frames);
                // Stop at a silence boundary so every emitted frame is
                // contiguous in capture position. One `capture_pos` cannot
                // describe a block with a hole in the middle.
                break;
            }

            if start_pos.is_none() {
                start_pos = Some(device_pos);
            }
            // SAFETY: WASAPI guarantees `frames * device_channels` samples of the
            // mix format's width at `data`; the format was validated at open.
            unsafe {
                append_samples(
                    &mut pcm,
                    data,
                    frames as usize,
                    stride,
                    out_ch,
                    active.format.kind,
                );
            }
            // SAFETY: paired with the GetBuffer above, releasing exactly what it gave.
            unsafe {
                let _ = active.capture.ReleaseBuffer(frames);
            }
            self.capture_pos = device_pos + u64::from(frames);
        }

        if !pcm.is_empty() {
            Captured::Frame(AudioFrame {
                sample_rate: active.format.sample_rate,
                channels: active.format.send_channels,
                // **WASAPI's own device position**, not a counter we maintain.
                // A counter that only advances by frames we successfully read
                // cannot express a buffer loss, so a discontinuity would look
                // like continuous audio with a jump in time — which is the one
                // thing this field exists to make visible.
                capture_pos: start_pos.unwrap_or(self.capture_pos),
                pcm,
            })
        } else {
            Captured::Silence {
                frames: silent_frames,
            }
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
