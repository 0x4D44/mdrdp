//! Stage 3 — H.264 via a Media Foundation MFT.
//!
//! This is the least-known component in the spike, so the documented contract is
//! spelled out here and cited at each step in the code. Whoever debugs the first
//! live run on the host needs the intended sequence legible without MSDN open.
//!
//! ## Startup, in order
//!
//! 1. `MFStartup(MF_VERSION, MFSTARTUP_LITE)` once per process, on the thread that
//!    will drive the transform. (Done by [`Session`].)
//! 2. `MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, …)` with input NV12 and output H.264.
//!    Hardware **async** MFTs are asked for first
//!    (`HARDWARE | ASYNCMFT | SORTANDFILTER`); the Microsoft software encoder, which
//!    is a **sync** MFT, is the fallback (`SYNCMFT | SORTANDFILTER`).
//! 3. `IMFActivate::ActivateObject::<IMFTransform>()`.
//! 4. `GetAttributes()`. If `MF_TRANSFORM_ASYNC` is 1, set
//!    `MF_TRANSFORM_ASYNC_UNLOCK = 1`. **Mandatory** — an async MFT refuses every
//!    other call until it is unlocked.
//! 5. If `MF_SA_D3D11_AWARE` is set, `ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER,
//!    manager)` with an `IMFDXGIDeviceManager` wrapping our D3D11 device. This is
//!    what lets the encoder read the NV12 surface without a CPU copy, and it must
//!    happen **before** the media types are set.
//! 6. `ICodecAPI` low-latency settings, then `SetOutputType`, then `SetInputType`.
//!    Order matters twice over: the H.264 encoder derives its input constraints from
//!    the output type, and several `ICodecAPI` properties are only honoured if they
//!    are set before the output type is locked in.
//! 7. `ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING)` then
//!    `MFT_MESSAGE_NOTIFY_START_OF_STREAM`.
//!
//! ## Steady state — the async event contract
//!
//! An async MFT must **never** be driven by a blind `ProcessInput`/`ProcessOutput`
//! loop. It publishes `IMFMediaEventGenerator` and issues:
//!
//! * `METransformNeedInput` (601) — one credit to call `ProcessInput` once. The
//!   event carries `MF_EVENT_MFT_INPUT_STREAM_ID`.
//! * `METransformHaveOutput` (602) — call `ProcessOutput` once.
//! * `METransformDrainComplete` (603) — the drain requested at shutdown is finished.
//!
//! Calling `ProcessInput` without a credit returns `MF_E_NOTACCEPTING`; polling
//! `ProcessOutput` without `HaveOutput` returns `MF_E_TRANSFORM_NEED_MORE_INPUT`
//! forever on some drivers. [`AsyncEncoder`] tracks unspent credits so a frame that
//! arrives after a credit does not wait for a second event.
//!
//! The sync fallback drives the plain `ProcessInput` → `ProcessOutput`-until-
//! `NEED_MORE_INPUT` loop, behind the same [`Encoder`] trait, so the pipeline code
//! above is identical either way.
//!
//! ## Output order
//!
//! B-frames are disabled (`CODECAPI_AVEncMPVDefaultBPictureCount = 0`), so encoded
//! output comes out in submission order. That is what makes the submit-stamp FIFO in
//! [`Mft::submitted`] correct; with B-frames it would mis-pair stamps to frames.

use super::{qpc, Result};
use crate::annexb::ParameterSets;
use std::collections::VecDeque;
use std::ffi::c_void;
use windows::core::{Interface, GUID};
use windows::Win32::Foundation::{VARIANT_FALSE, VARIANT_TRUE};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_UI4};

/// One encoded access unit, with the two stamps only the encoder can take.
pub struct EncodedAu {
    pub data: Vec<u8>,
    pub keyframe: bool,
    /// QPC at the moment `ProcessInput` was called for this frame.
    pub submit_qpc: i64,
    /// QPC at the moment `ProcessOutput` handed the bytes back.
    pub out_qpc: i64,
}

/// The encode stage, with the async and sync drivers behind one interface so the
/// pipeline never branches on which one was selected.
pub trait Encoder {
    /// `MFT_FRIENDLY_NAME_Attribute` of the selected transform.
    fn name(&self) -> &str;
    /// `async-hardware` or `sync-software`.
    fn kind(&self) -> &'static str;
    fn codec_api_applied(&self) -> &[String];
    fn codec_api_refused(&self) -> &[String];
    /// SPS/PPS from `MF_MT_MPEG_SEQUENCE_HEADER`, when the encoder published them.
    fn parameter_sets(&self) -> Option<&ParameterSets>;
    /// Ask for an IDR on the next frame. Best-effort: an encoder that refuses
    /// `CODECAPI_AVEncVideoForceKeyFrame` will still produce one at the next GOP.
    fn request_keyframe(&mut self);
    /// Feed one NV12 sample and hand every access unit it produces to `sink`.
    fn encode(
        &mut self,
        sample: &IMFSample,
        sink: &mut dyn FnMut(EncodedAu) -> Result<()>,
    ) -> Result<()>;
    /// Drain and stop streaming. Best-effort; failures here cannot be acted on.
    fn shutdown(&mut self);
}

/// Process-wide Media Foundation lifetime. Dropping it calls `MFShutdown`.
pub struct Session;

impl Session {
    pub fn start() -> Result<Self> {
        // Contract step 1. LITE skips the parts of MF we never touch (the media
        // session and the sink writer), which is a measurably faster start.
        // SAFETY: no arguments to get wrong; balanced by MFShutdown in `Drop`.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) }?;
        Ok(Self)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: balances the MFStartup above.
        let _ = unsafe { MFShutdown() };
    }
}

fn variant_u32(value: u32) -> VARIANT {
    let mut v = VARIANT::default();
    // SAFETY: writing the discriminant and the matching union arm of a
    // zero-initialised VARIANT. VT_UI4 selects `ulVal`, which is what is written.
    unsafe {
        (*v.Anonymous.Anonymous).vt = VT_UI4;
        (*v.Anonymous.Anonymous).Anonymous.ulVal = value;
    }
    v
}

fn variant_bool(value: bool) -> VARIANT {
    let mut v = VARIANT::default();
    // SAFETY: as above; VT_BOOL selects `boolVal`.
    unsafe {
        (*v.Anonymous.Anonymous).vt = VT_BOOL;
        (*v.Anonymous.Anonymous).Anonymous.boolVal =
            if value { VARIANT_TRUE } else { VARIANT_FALSE };
    }
    v
}

/// `MF_MT_FRAME_SIZE` and friends pack two 32-bit values into one UINT64.
fn pack_ratio(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

/// Wrap our D3D11 device so a hardware MFT can read the NV12 surfaces directly.
pub fn device_manager(device: &ID3D11Device) -> Result<IMFDXGIDeviceManager> {
    let mut token = 0u32;
    let mut manager: Option<IMFDXGIDeviceManager> = None;
    // SAFETY: both out-parameters are live locals.
    unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager) }?;
    let manager = manager.ok_or("MFCreateDXGIDeviceManager returned nothing")?;
    // SAFETY: `device` is live; `token` is the one just handed back, which is the
    // only value ResetDevice accepts for this manager.
    unsafe { manager.ResetDevice(device, token) }?;
    Ok(manager)
}

/// Wrap an NV12 texture as an `IMFSample` without copying it.
pub fn sample_from_texture(
    texture: &ID3D11Texture2D,
    time_hns: i64,
    duration_hns: i64,
) -> Result<IMFSample> {
    // SAFETY: `texture` is live; subresource 0 is the only one these textures have.
    let buffer = unsafe { MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, 0, false) }?;
    // A DXGI surface buffer starts with a current length of zero. An encoder that
    // trusts the length (the software one does) would see an empty frame.
    if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
        // SAFETY: `two_d` is a live view of the same buffer.
        if let Ok(length) = unsafe { two_d.GetContiguousLength() } {
            // SAFETY: as above.
            unsafe { buffer.SetCurrentLength(length) }?;
        }
    }
    // SAFETY: MFCreateSample takes no arguments; the buffer is live.
    let sample = unsafe { MFCreateSample() }?;
    unsafe {
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(time_hns)?;
        sample.SetSampleDuration(duration_hns)?;
    }
    Ok(sample)
}

/// State shared by both drivers.
struct Mft {
    transform: IMFTransform,
    codec_api: Option<ICodecAPI>,
    name: String,
    /// True when the MFT allocates the output samples itself, which every hardware
    /// encoder does and the software one does not.
    provides_samples: bool,
    /// `cbSize` from `GetOutputStreamInfo`, used to size our own output buffer when
    /// the MFT does not provide one.
    output_size: u32,
    applied: Vec<String>,
    refused: Vec<String>,
    parameter_sets: Option<ParameterSets>,
    /// Submit stamps in submission order. Correct only because B-frames are off.
    submitted: VecDeque<i64>,
    scratch: Vec<u8>,
}

impl Mft {
    /// One `ProcessOutput` call. `Ok(true)` when it produced an access unit.
    fn process_output(&mut self, sink: &mut dyn FnMut(EncodedAu) -> Result<()>) -> Result<bool> {
        let mut sample_in: Option<IMFSample> = None;
        if !self.provides_samples {
            // SAFETY: no arguments to get wrong.
            let buffer = unsafe { MFCreateMemoryBuffer(self.output_size.max(4096)) }?;
            // SAFETY: both interfaces are live.
            let sample = unsafe { MFCreateSample() }?;
            unsafe { sample.AddBuffer(&buffer) }?;
            sample_in = Some(sample);
        }

        let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: std::mem::ManuallyDrop::new(sample_in),
            dwStatus: 0,
            pEvents: std::mem::ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        // SAFETY: `buffers` and `status` are live locals for the call.
        let outcome = unsafe { self.transform.ProcessOutput(0, &mut buffers, &mut status) };
        let out_qpc = qpc::now();

        // Reclaim whatever is in the struct — our own sample on the sync path, or
        // the MFT's on the async path. Either way it is ours to release now.
        let produced = std::mem::ManuallyDrop::into_inner(std::mem::replace(
            &mut buffers[0].pSample,
            std::mem::ManuallyDrop::new(None),
        ));
        let events = std::mem::ManuallyDrop::into_inner(std::mem::replace(
            &mut buffers[0].pEvents,
            std::mem::ManuallyDrop::new(None),
        ));
        drop(events);

        match outcome {
            Ok(()) => {}
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(false),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // The encoder renegotiated. Accept its first proposal and refresh the
                // stored parameter sets, which may have changed with it.
                // SAFETY: the transform is live.
                let new_type = unsafe { self.transform.GetOutputAvailableType(0, 0) }?;
                unsafe { self.transform.SetOutputType(0, &new_type, 0) }?;
                self.parameter_sets = read_sequence_header(&new_type);
                return Ok(false);
            }
            Err(e) => return Err(e.into()),
        }

        let Some(sample) = produced else {
            return Ok(false);
        };
        let submit_qpc = self.submitted.pop_front().unwrap_or(out_qpc);
        // `MFSampleExtension_CleanPoint` is how MF marks an IDR.
        // SAFETY: `sample` is live; a missing attribute is an error, not a crash.
        let keyframe = unsafe { sample.GetUINT32(&MFSampleExtension_CleanPoint) }.unwrap_or(0) == 1;

        // SAFETY: contiguous buffer is live; Lock/Unlock are paired below.
        let buffer = unsafe { sample.ConvertToContiguousBuffer() }?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut current = 0u32;
        unsafe { buffer.Lock(&mut ptr, None, Some(&mut current)) }?;
        self.scratch.clear();
        if !ptr.is_null() && current > 0 {
            // SAFETY: MF guarantees `ptr` is valid for `current` bytes until Unlock.
            self.scratch
                .extend_from_slice(unsafe { std::slice::from_raw_parts(ptr, current as usize) });
        }
        // SAFETY: balances the Lock above; must run even if the copy was empty.
        unsafe { buffer.Unlock() }?;

        sink(EncodedAu {
            data: std::mem::take(&mut self.scratch),
            keyframe,
            submit_qpc,
            out_qpc,
        })?;
        Ok(true)
    }

    fn deliver(&mut self, stream_id: u32, sample: &IMFSample) -> Result<()> {
        let submit_qpc = qpc::now();
        // SAFETY: transform and sample are live.
        unsafe { self.transform.ProcessInput(stream_id, sample, 0) }?;
        self.submitted.push_back(submit_qpc);
        Ok(())
    }

    fn request_keyframe(&mut self) {
        let Some(api) = self.codec_api.as_ref() else {
            return;
        };
        let value = variant_u32(1);
        // SAFETY: both pointers are live locals. A refusal is expected on encoders
        // that do not implement the property and is deliberately ignored.
        let _ = unsafe { api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &value) };
    }
}

fn read_sequence_header(media_type: &IMFMediaType) -> Option<ParameterSets> {
    // SAFETY: `media_type` is live. A missing attribute returns an error rather
    // than writing anything, which is why the size is read first.
    let size = unsafe { media_type.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) }.ok()?;
    if size == 0 {
        return None;
    }
    let mut blob = vec![0u8; size as usize];
    // SAFETY: `blob` is exactly `size` bytes, which is what GetBlobSize reported.
    unsafe { media_type.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, None) }.ok()?;
    ParameterSets::from_sequence_header(&blob)
}

/// The async, event-driven driver — the path a hardware encoder takes.
pub struct AsyncEncoder {
    mft: Mft,
    events: IMFMediaEventGenerator,
    /// `METransformNeedInput` events seen but not yet spent.
    credits: u32,
    streaming: bool,
}

enum Dispatched {
    NeedInput(u32),
    Other,
}

impl AsyncEncoder {
    fn dispatch(
        &mut self,
        event: &IMFMediaEvent,
        sink: &mut dyn FnMut(EncodedAu) -> Result<()>,
    ) -> Result<Dispatched> {
        // SAFETY: `event` is live.
        let kind = MF_EVENT_TYPE(unsafe { event.GetType() }? as i32);
        // Compared rather than matched: the windows-rs event constants are
        // newtype values, not enum variants, so a `match` arm would silently
        // become a catch-all binding if one of them were ever renamed.
        if kind == METransformNeedInput {
            // The stream id travels on the event. With one input stream it is
            // always 0, but reading it is what the contract says.
            // SAFETY: IMFMediaEvent derefs to IMFAttributes; a missing attribute
            // is an error value, not a crash.
            let id = unsafe { event.GetUINT32(&MF_EVENT_MFT_INPUT_STREAM_ID) }.unwrap_or(0);
            return Ok(Dispatched::NeedInput(id));
        }
        if kind == METransformHaveOutput {
            self.mft.process_output(sink)?;
        }
        Ok(Dispatched::Other)
    }

    /// Consume every event already queued, without blocking.
    fn drain_queued(&mut self, sink: &mut dyn FnMut(EncodedAu) -> Result<()>) -> Result<()> {
        loop {
            // SAFETY: `events` is live; NO_WAIT makes this a poll.
            match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => {
                    if let Dispatched::NeedInput(_) = self.dispatch(&event, sink)? {
                        self.credits += 1;
                    }
                }
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                Err(e) => return Err(e.into()),
            }
        }
    }
}

impl Encoder for AsyncEncoder {
    fn name(&self) -> &str {
        &self.mft.name
    }

    fn kind(&self) -> &'static str {
        "async-hardware"
    }

    fn codec_api_applied(&self) -> &[String] {
        &self.mft.applied
    }

    fn codec_api_refused(&self) -> &[String] {
        &self.mft.refused
    }

    fn parameter_sets(&self) -> Option<&ParameterSets> {
        self.mft.parameter_sets.as_ref()
    }

    fn request_keyframe(&mut self) {
        self.mft.request_keyframe();
    }

    fn encode(
        &mut self,
        sample: &IMFSample,
        sink: &mut dyn FnMut(EncodedAu) -> Result<()>,
    ) -> Result<()> {
        // Spend a credit if we already hold one; otherwise block until the encoder
        // asks. Handling output events while we wait is the point of the contract —
        // an encoder that has both work to give and room to take must not deadlock.
        if self.credits > 0 {
            self.credits -= 1;
            self.mft.deliver(0, sample)?;
        } else {
            loop {
                // SAFETY: `events` is live; flags 0 means block until an event.
                let event = unsafe {
                    self.events
                        .GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0))
                }?;
                if let Dispatched::NeedInput(id) = self.dispatch(&event, sink)? {
                    self.mft.deliver(id, sample)?;
                    break;
                }
            }
        }
        // Anything the encoder finished while we were busy goes out now rather than
        // waiting for the next frame to pump it.
        self.drain_queued(sink)
    }

    fn shutdown(&mut self) {
        if !self.streaming {
            return;
        }
        self.streaming = false;
        // SAFETY: the transform is live; every failure here is unactionable at
        // shutdown, so each result is deliberately discarded.
        unsafe {
            let _ = self
                .mft
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self
                .mft
                .transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
        }
        // Pump until the encoder says the drain is done, bounded so a driver that
        // never sends METransformDrainComplete cannot wedge the exit.
        let mut sink = |_: EncodedAu| -> Result<()> { Ok(()) };
        for _ in 0..256 {
            // SAFETY: `events` is live.
            match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => {
                    // SAFETY: `event` is live.
                    let kind = unsafe { event.GetType() }.unwrap_or(0);
                    if MF_EVENT_TYPE(kind as i32) == METransformDrainComplete {
                        break;
                    }
                    let _ = self.dispatch(&event, &mut sink);
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(2)),
            }
        }
        // SAFETY: as above.
        unsafe {
            let _ = self
                .mft
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        }
    }
}

/// The sync driver — the Microsoft software encoder, and any hardware MFT that
/// turns out not to be async.
pub struct SyncEncoder {
    mft: Mft,
    streaming: bool,
}

impl Encoder for SyncEncoder {
    fn name(&self) -> &str {
        &self.mft.name
    }

    fn kind(&self) -> &'static str {
        "sync-software"
    }

    fn codec_api_applied(&self) -> &[String] {
        &self.mft.applied
    }

    fn codec_api_refused(&self) -> &[String] {
        &self.mft.refused
    }

    fn parameter_sets(&self) -> Option<&ParameterSets> {
        self.mft.parameter_sets.as_ref()
    }

    fn request_keyframe(&mut self) {
        self.mft.request_keyframe();
    }

    fn encode(
        &mut self,
        sample: &IMFSample,
        sink: &mut dyn FnMut(EncodedAu) -> Result<()>,
    ) -> Result<()> {
        self.mft.deliver(0, sample)?;
        // A sync MFT is drained by polling until it says it needs more input.
        while self.mft.process_output(sink)? {}
        Ok(())
    }

    fn shutdown(&mut self) {
        if !self.streaming {
            return;
        }
        self.streaming = false;
        let mut sink = |_: EncodedAu| -> Result<()> { Ok(()) };
        // SAFETY: the transform is live; shutdown failures are unactionable.
        unsafe {
            let _ = self
                .mft
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self
                .mft
                .transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
        }
        for _ in 0..256 {
            match self.mft.process_output(&mut sink) {
                Ok(true) => {}
                _ => break,
            }
        }
        // SAFETY: as above.
        unsafe {
            let _ = self
                .mft
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        }
    }
}

/// Enumerate encoder MFTs matching `flags`, newest-first as MF sorted them.
fn enumerate(flags: MFT_ENUM_FLAG) -> Result<Vec<IMFActivate>> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: the type-info locals outlive the call; `array` and `count` are live
    // out-parameters. MF allocates the array with CoTaskMemAlloc.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            Some(&input),
            Some(&output),
            &mut array,
            &mut count,
        )
    }?;
    if array.is_null() || count == 0 {
        if !array.is_null() {
            // SAFETY: the array came from CoTaskMemAlloc inside MFTEnumEx.
            unsafe { CoTaskMemFree(Some(array as *const c_void)) };
        }
        return Ok(Vec::new());
    }
    // SAFETY: MF wrote `count` interface pointers into `array`.
    let slice = unsafe { std::slice::from_raw_parts_mut(array, count as usize) };
    // `take` moves each pointer out without an extra AddRef and leaves None behind,
    // so freeing the array below cannot double-release anything.
    let activates: Vec<IMFActivate> = slice.iter_mut().filter_map(Option::take).collect();
    // SAFETY: as above.
    unsafe { CoTaskMemFree(Some(array as *const c_void)) };
    Ok(activates)
}

fn friendly_name(activate: &IMFActivate) -> String {
    let mut ptr = windows::core::PWSTR::null();
    let mut len = 0u32;
    // SAFETY: both out-parameters are live locals.
    if unsafe { activate.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut ptr, &mut len) }
        .is_err()
        || ptr.is_null()
    {
        return "<unnamed MFT>".to_owned();
    }
    // SAFETY: MF wrote a NUL-terminated wide string that it allocated with
    // CoTaskMemAlloc; `to_string` copies before we free it.
    let name = unsafe { ptr.to_string() }.unwrap_or_else(|_| "<unreadable MFT name>".to_owned());
    // SAFETY: the string came from CoTaskMemAlloc inside GetAllocatedString.
    unsafe { CoTaskMemFree(Some(ptr.as_ptr() as *const c_void)) };
    name
}

struct CodecSettings {
    applied: Vec<String>,
    refused: Vec<String>,
}

impl CodecSettings {
    fn set(&mut self, api: &ICodecAPI, label: &str, key: &GUID, value: &VARIANT) {
        // SAFETY: both pointers are live locals for the call.
        match unsafe { api.SetValue(key, value) } {
            Ok(()) => self.applied.push(label.to_owned()),
            Err(_) => self.refused.push(label.to_owned()),
        }
    }
}

/// Configure one candidate transform. Returns the assembled [`Mft`] on success.
#[allow(clippy::too_many_arguments)]
fn configure(
    transform: IMFTransform,
    name: String,
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
    gop: u32,
    manager: Option<&IMFDXGIDeviceManager>,
) -> Result<(Mft, bool)> {
    // Contract step 4 — attributes, and the mandatory async unlock.
    // SAFETY: `transform` is live for every call in this function.
    let attributes = unsafe { transform.GetAttributes() }.ok();
    let is_async = attributes
        .as_ref()
        .and_then(|a| unsafe { a.GetUINT32(&MF_TRANSFORM_ASYNC) }.ok())
        .unwrap_or(0)
        == 1;
    if is_async {
        let attributes = attributes
            .as_ref()
            .ok_or("async MFT with no attribute store")?;
        // SAFETY: `attributes` is live.
        unsafe { attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) }?;
    }

    // Contract step 5 — the D3D manager, before any media type is set.
    let d3d_aware = attributes
        .as_ref()
        .and_then(|a| unsafe { a.GetUINT32(&MF_SA_D3D11_AWARE) }.ok())
        .unwrap_or(0)
        != 0;
    if d3d_aware {
        if let Some(manager) = manager {
            let raw = manager.as_raw() as usize;
            // SAFETY: the manager outlives the transform (both are owned by the
            // pipeline), so passing its raw pointer as ULONG_PTR is sound for the
            // transform's lifetime — which is what MFT_MESSAGE_SET_D3D_MANAGER
            // requires.
            unsafe { transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, raw) }?;
        }
    }

    // Contract step 6a — the low-latency settings that must precede the types.
    let codec_api = transform.cast::<ICodecAPI>().ok();
    let mut settings = CodecSettings {
        applied: Vec::new(),
        refused: Vec::new(),
    };
    let bitrate_bps = bitrate_kbps.saturating_mul(1000);
    if let Some(api) = codec_api.as_ref() {
        settings.set(
            api,
            "AVLowLatencyMode",
            &CODECAPI_AVLowLatencyMode,
            &variant_bool(true),
        );
        settings.set(
            api,
            "AVEncCommonRateControlMode",
            &CODECAPI_AVEncCommonRateControlMode,
            &variant_u32(eAVEncCommonRateControlMode_CBR.0 as u32),
        );
        settings.set(
            api,
            "AVEncCommonMeanBitRate",
            &CODECAPI_AVEncCommonMeanBitRate,
            &variant_u32(bitrate_bps),
        );
    }

    // Contract step 6b — output type first. The encoder derives its input
    // constraints from it, so setting the input type first fails on most MFTs.
    // SAFETY: every media type below is a live local for the duration of its use.
    let output_type = unsafe { MFCreateMediaType() }?;
    unsafe {
        output_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        output_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        output_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_bps)?;
        output_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_ratio(width, height))?;
        output_type.SetUINT64(&MF_MT_FRAME_RATE, pack_ratio(fps, 1))?;
        output_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_ratio(1, 1))?;
        output_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        // Main profile: High buys compression the spike does not need and is where
        // decoder-compatibility surprises live.
        output_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32)?;
        transform.SetOutputType(0, &output_type, 0)?;
    }

    let input_type = unsafe { MFCreateMediaType() }?;
    // SAFETY: as above.
    unsafe {
        input_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        input_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        input_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_ratio(width, height))?;
        input_type.SetUINT64(&MF_MT_FRAME_RATE, pack_ratio(fps, 1))?;
        input_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_ratio(1, 1))?;
        input_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        transform.SetInputType(0, &input_type, 0)?;
    }

    // Contract step 6c — the GOP and B-frame settings, which several encoders only
    // accept once the types are in place.
    if let Some(api) = codec_api.as_ref() {
        settings.set(
            api,
            "AVEncMPVDefaultBPictureCount",
            &CODECAPI_AVEncMPVDefaultBPictureCount,
            &variant_u32(0),
        );
        settings.set(
            api,
            "AVEncMPVGOPSize",
            &CODECAPI_AVEncMPVGOPSize,
            &variant_u32(gop),
        );
    }

    // SAFETY: the transform is live.
    let stream_info = unsafe { transform.GetOutputStreamInfo(0) }?;
    let provides_samples = stream_info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;

    // The out-of-band parameter sets, if the encoder published them. See `annexb`.
    // SAFETY: the transform is live.
    let parameter_sets = unsafe { transform.GetOutputCurrentType(0) }
        .ok()
        .and_then(|t| read_sequence_header(&t));

    // Contract step 7 — start streaming.
    // SAFETY: the transform is live.
    unsafe {
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
    }

    Ok((
        Mft {
            transform,
            codec_api,
            name,
            provides_samples,
            output_size: stream_info.cbSize,
            applied: settings.applied,
            refused: settings.refused,
            parameter_sets,
            submitted: VecDeque::new(),
            scratch: Vec::new(),
        },
        is_async,
    ))
}

/// Pick and configure an encoder: hardware async first, software sync as fallback.
///
/// Every candidate that fails configuration is reported in the error, because "no
/// encoder" on a host with a GPU is nearly always one specific refusal worth reading.
pub fn create(
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
    gop: u32,
    manager: Option<&IMFDXGIDeviceManager>,
) -> Result<Box<dyn Encoder>> {
    let mut failures: Vec<String> = Vec::new();

    let hardware = MFT_ENUM_FLAG(
        MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_ASYNCMFT.0 | MFT_ENUM_FLAG_SORTANDFILTER.0,
    );
    let software = MFT_ENUM_FLAG(MFT_ENUM_FLAG_SYNCMFT.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);

    for (flags, manager) in [(hardware, manager), (software, None)] {
        for activate in enumerate(flags)? {
            let name = friendly_name(&activate);
            // SAFETY: `activate` is live.
            let transform = match unsafe { activate.ActivateObject::<IMFTransform>() } {
                Ok(t) => t,
                Err(e) => {
                    failures.push(format!("{name}: ActivateObject failed: {e}"));
                    continue;
                }
            };
            match configure(
                transform,
                name.clone(),
                width,
                height,
                fps,
                bitrate_kbps,
                gop,
                manager,
            ) {
                Ok((mft, true)) => {
                    let events = mft.transform.cast::<IMFMediaEventGenerator>()?;
                    return Ok(Box::new(AsyncEncoder {
                        mft,
                        events,
                        credits: 0,
                        streaming: true,
                    }));
                }
                Ok((mft, false)) => {
                    return Ok(Box::new(SyncEncoder {
                        mft,
                        streaming: true,
                    }))
                }
                Err(e) => failures.push(format!("{name}: {e}")),
            }
        }
    }

    Err(format!(
        "no usable H.264 encoder MFT for {width}x{height}. Candidates tried:\n  {}",
        if failures.is_empty() {
            "none enumerated".to_owned()
        } else {
            failures.join("\n  ")
        }
    )
    .into())
}
