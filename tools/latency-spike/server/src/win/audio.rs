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

use std::sync::Once;

use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
};

use crate::audio_source::{AudioSource, Captured};
use crate::aux_proto::AudioFrame;

/// The capture buffer we ask WASAPI for, in 100-nanosecond units.
///
/// 200 ms. Comfortably more than the 10 ms we read at a time, so an occasional
/// late poll costs latency rather than samples.
const BUFFER_DURATION_100NS: i64 = 2_000_000;

/// COM must be initialised once per thread, and the capture thread is the only
/// one that touches this.
fn ensure_com() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: called once, before any other COM call on this thread.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
    });
}

/// What the endpoint's mix format is, reduced to what the converter needs.
#[derive(Clone, Copy)]
struct MixFormat {
    sample_rate: u32,
    channels: u8,
    /// Bits per sample as the device reports them.
    ///
    /// The mix format in shared mode is **almost always 32-bit float**, but the
    /// documented contract permits others, so the sample width decides the
    /// conversion rather than an assumption. 32 is treated as float and 16 as
    /// integer; anything else is refused rather than reinterpreted, because
    /// guessing wrong here produces loud noise, not silence.
    bits: u16,
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
        let format = unsafe {
            MixFormat {
                sample_rate: (*format_ptr).nSamplesPerSec,
                channels: (*format_ptr).nChannels.min(2) as u8,
                bits: (*format_ptr).wBitsPerSample,
            }
        };
        if format.bits != 32 && format.bits != 16 {
            return Err(format!("unsupported sample width {} bits", format.bits));
        }
        // SAFETY: a valid client and the mix format it just handed us.
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                BUFFER_DURATION_100NS,
                0,
                format_ptr,
                None,
            )
        }
        .map_err(|e| format!("cannot initialise loopback: {e}"))?;
        // SAFETY: an initialised client.
        let capture: IAudioCaptureClient =
            unsafe { client.GetService() }.map_err(|e| format!("no capture service: {e}"))?;
        // SAFETY: an initialised client with a capture service.
        unsafe { client.Start() }.map_err(|e| format!("cannot start capture: {e}"))?;
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
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut frames: u32 = 0;
        let mut flags: u32 = 0;
        // SAFETY: a started capture client; every out-parameter is owned here.
        let hr = unsafe {
            active
                .capture
                .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
        };
        if let Err(e) = hr {
            eprintln!("audio: capture read failed ({e}); stopping");
            self.inner = None;
            return Captured::Unavailable;
        }
        // **"Nothing ready" arrives as zero frames, not as an error.**
        // `AUDCLNT_S_BUFFER_EMPTY` is a *success* HRESULT, so windows-rs maps it
        // to `Ok`. An earlier draft here matched it as an error branch, which
        // would have been dead code pretending to handle a case it could never
        // see — the worst kind to leave in a module that cannot be tested.
        if frames == 0 {
            // SAFETY: paired with the GetBuffer above.
            unsafe {
                let _ = active.capture.ReleaseBuffer(0);
            }
            return Captured::Silence { frames: 0 };
        }

        let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
        let channels = active.format.channels;
        let result = if silent || data.is_null() {
            // **Not sent, but the clock still advances.** That is what keeps a
            // quiet desktop distinguishable from a lossy link at the far end.
            Captured::Silence {
                frames: u64::from(frames),
            }
        } else {
            let count = frames as usize * channels as usize;
            let mut pcm = Vec::with_capacity(count * 2);
            // SAFETY: WASAPI guarantees `frames * channels` samples of the mix
            // format's width at `data`, and the width was checked at open.
            unsafe {
                match active.format.bits {
                    32 => {
                        let src = std::slice::from_raw_parts(data as *const f32, count);
                        for &s in src {
                            // Clamp before scaling: a float buffer may legally
                            // exceed +/-1.0, and letting that wrap an i16 turns a
                            // loud passage into noise rather than clipping.
                            let clamped = s.clamp(-1.0, 1.0);
                            let v = (clamped * i16::MAX as f32) as i16;
                            pcm.extend_from_slice(&v.to_le_bytes());
                        }
                    }
                    _ => {
                        let src = std::slice::from_raw_parts(data as *const i16, count);
                        for &v in src {
                            pcm.extend_from_slice(&v.to_le_bytes());
                        }
                    }
                }
            }
            Captured::Frame(AudioFrame {
                sample_rate: active.format.sample_rate,
                channels,
                capture_pos: self.capture_pos,
                pcm,
            })
        };

        // SAFETY: paired with the GetBuffer above, releasing exactly what it gave.
        unsafe {
            let _ = active.capture.ReleaseBuffer(frames);
        }
        self.capture_pos += u64::from(frames);
        result
    }

    fn describe(&self) -> &'static str {
        if self.inner.is_some() {
            "loopback"
        } else {
            "loopback (unavailable)"
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
