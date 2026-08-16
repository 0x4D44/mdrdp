//! Hardware H.264 decode for EGFX AVC streams.
//!
//! The EGFX pipeline hands [`ironrdp_egfx::decode::H264Decoder`] H.264 access units.
//! Windows delivers them in **Annex B** framing (start codes, AUD + SPS + PPS +
//! slices — measured from captured quench payloads; the upstream ironrdp docs claim
//! AVCC and are wrong), so the units are split on start codes and re-packed with the
//! 4-byte length prefixes VideoToolbox consumes. The
//! macOS implementation drives a `VTDecompressionSession` with full-range planar
//! 4:2:0 output ('f420') for every decode: AVC444's luma+chroma combination must
//! happen in YUV space, the RGBA path converts the same planes in software, and a
//! single output format means a codec switch never rebuilds the session (a mid-GOP
//! rebuild kills decode until the next IDR — measured).
//!
//! Platform split per the architecture rule (portable by default, platform-specific at
//! the edges): the pure AVCC parsing helpers are portable and unit-tested; everything
//! Apple-specific lives behind `cfg(target_os = "macos")`. Other platforms currently
//! report no hardware decoder, which keeps AVC out of the EGFX capability advertisement
//! there (the vendored client filters AVC-bearing capability sets when no decoder is
//! configured).

use ironrdp_egfx::decode::H264Decoder;

/// Whether this build carries a hardware H.264 decoder.
///
/// The capability advertisement (see `GfxHandler::advertising_avc`) and the decoder
/// wiring in `connect::establish` must agree, and both key off this one answer.
pub fn hardware_decode_available() -> bool {
    cfg!(target_os = "macos")
}

/// The platform's hardware H.264 decoder, if this build has one.
///
/// `None` keeps the EGFX advertisement AVC-free, so a server never sends a stream
/// nothing here could decode.
pub fn hardware_decoder() -> Option<Box<dyn H264Decoder>> {
    #[cfg(target_os = "macos")]
    {
        Some(Box::new(videotoolbox::VideoToolboxDecoder::new()))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

// ---------------------------------------------------------------------------------------
// NAL parsing — portable, pure, tested
// ---------------------------------------------------------------------------------------
//
// Only the macOS backend consumes these today, so they are compiled for macOS and for
// tests; a blanket allow(dead_code) would also hide a genuinely dropped call site.

/// NAL unit types this module cares about (H.264 spec, `nal_unit_type`).
#[cfg(any(target_os = "macos", test))]
const NAL_SPS: u8 = 7;
#[cfg(any(target_os = "macos", test))]
const NAL_PPS: u8 = 8;

/// Split an H.264 elementary stream into NAL units, whatever its framing.
///
/// **Windows sends Annex B** (start-code-delimited: `00 00 01` / `00 00 00 01`,
/// AUD + SPS + PPS + slices) in RFX_AVC420/AVC444 bitmap streams — measured from
/// captured quench payloads 2026-08-16. The upstream ironrdp-egfx docs claim
/// AVC/AVCC framing (4-byte big-endian length prefixes); that is wrong for real
/// Windows, and it went unnoticed elsewhere because ffmpeg/openh264 consume Annex B
/// natively. VideoToolbox does not, so the caller re-packs these units as AVCC.
///
/// A stream that does not begin with a start code is walked as AVCC (the framing
/// the upstream docs promise), so a spec-faithful server still decodes. Wire data
/// is untrusted either way: truncation ends the walk, nothing panics or over-reads.
#[cfg(any(target_os = "macos", test))]
fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    if data.starts_with(&[0, 0, 1]) || data.starts_with(&[0, 0, 0, 1]) {
        annex_b_units(data)
    } else {
        avcc_units(data)
    }
}

/// Split an Annex B byte stream on its start codes.
///
/// Emulation prevention guarantees `00 00 01` never occurs inside a NAL, so start
/// codes are unambiguous split points. Trailing zero bytes of each unit are
/// trimmed: they are either the leading `00` of a following 4-byte start code or
/// `cabac_zero_words` padding, and decoders accept their removal (ffmpeg's
/// splitter does the same).
#[cfg(any(target_os = "macos", test))]
fn annex_b_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if data[i..i + 3] == [0, 0, 1] {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut units = Vec::with_capacity(starts.len());
    for (k, &start) in starts.iter().enumerate() {
        let mut end = match starts.get(k + 1) {
            Some(&next) => next - 3,
            None => data.len(),
        };
        while end > start && data[end - 1] == 0 {
            end -= 1;
        }
        if end > start {
            units.push(&data[start..end]);
        }
    }
    units
}

/// Walk an AVCC stream (4-byte big-endian length prefix per NAL).
#[cfg(any(target_os = "macos", test))]
fn avcc_units(data: &[u8]) -> Vec<&[u8]> {
    let mut units = Vec::new();
    let mut offset = 0usize;
    while offset + 4 <= data.len() {
        let len = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        offset += 4;
        let Some(end) = offset.checked_add(len) else {
            break;
        };
        if end > data.len() || len == 0 {
            break;
        }
        units.push(&data[offset..end]);
        offset = end;
    }
    units
}

/// `nal_unit_type` of a NAL unit (low five bits of the first byte).
#[cfg(any(target_os = "macos", test))]
fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map_or(0, |b| b & 0x1F)
}

/// The latest SPS and PPS in an access unit, if either is present.
#[cfg(any(target_os = "macos", test))]
fn parameter_sets<'a>(units: &[&'a [u8]]) -> (Option<&'a [u8]>, Option<&'a [u8]>) {
    let mut sps = None;
    let mut pps = None;
    for nal in units {
        match nal_type(nal) {
            NAL_SPS => sps = Some(*nal),
            NAL_PPS => pps = Some(*nal),
            _ => {}
        }
    }
    (sps, pps)
}

// ---------------------------------------------------------------------------------------
// VideoToolbox backend (macOS)
// ---------------------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod videotoolbox {
    use std::ffi::c_void;
    use std::ptr;

    use ironrdp_egfx::decode::{
        DecodedFrame, DecoderError, DecoderResult, H264Decoder, Yuv420Frame,
    };
    use tracing::{debug, warn};

    use super::{NAL_PPS, NAL_SPS, nal_type, nal_units, parameter_sets};

    // --- minimal FFI surface -----------------------------------------------------------
    //
    // Hand-written rather than a binding crate: the session uses exactly seven calls and
    // three statics, and every argument is either a pointer or a plain integer. The
    // types below mirror the C headers; all pointers are opaque.

    type OsStatus = i32;
    type CfIndex = isize;
    type CfTypeRef = *const c_void;
    type CfAllocatorRef = *const c_void;
    type CfDictionaryRef = *const c_void;
    type CmFormatDescriptionRef = *const c_void;
    type CmBlockBufferRef = *const c_void;
    type CmSampleBufferRef = *const c_void;
    type VtSessionRef = *const c_void;
    type CvImageBufferRef = *const c_void;
    type CvReturn = i32;

    /// CMTime by value. All-zero flags mean "invalid time", which is what a timestamp-less
    /// RDP frame wants.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CmTime {
        value: i64,
        timescale: i32,
        flags: u32,
        epoch: i64,
    }

    #[repr(C)]
    struct CmSampleTimingInfo {
        duration: CmTime,
        presentation_time_stamp: CmTime,
        decode_time_stamp: CmTime,
    }

    #[repr(C)]
    struct VtDecompressionOutputCallbackRecord {
        callback: extern "C" fn(
            refcon: *mut c_void,
            source_frame_refcon: *mut c_void,
            status: OsStatus,
            info_flags: u32,
            image_buffer: CvImageBufferRef,
            pts: CmTime,
            duration: CmTime,
        ),
        refcon: *mut c_void,
    }

    /// `kCVPixelFormatType_420YpCbCr8PlanarFullRange` ('f420').
    ///
    /// Full-range three-plane 4:2:0 — the one output format for every decode here.
    /// Measured (2026-08-16 probes): VideoToolbox normalises samples to the
    /// REQUESTED format's range regardless of the stream's VUI, so 'f420' hands over
    /// values matching the full-range BT.709 coefficients the RGB conversion uses,
    /// while 'y420' (video range) irreversibly squeezes them into 16-235. One format
    /// for both the RGBA and YUV paths also means a codec switch never rebuilds the
    /// session — a rebuild mid-GOP fails every P-frame until the next IDR (measured
    /// -12909), so rebuilds must never be routine.
    const K_CV_PIXEL_FORMAT_TYPE_420_PLANAR_FULL: u32 = u32::from_be_bytes(*b"f420");
    const K_CF_NUMBER_SINT32_TYPE: CfIndex = 3;
    const K_CV_PIXEL_BUFFER_LOCK_READ_ONLY: u64 = 1;
    /// `kVTInvalidSessionErr`: the session died (GPU reset, sleep); recreate and retry.
    const K_VT_INVALID_SESSION_ERR: OsStatus = -12903;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
        static kCFAllocatorNull: CfAllocatorRef;
        fn CFRelease(cf: CfTypeRef);
        fn CFNumberCreate(
            allocator: CfAllocatorRef,
            the_type: CfIndex,
            value_ptr: *const c_void,
        ) -> CfTypeRef;
        fn CFDictionaryCreate(
            allocator: CfAllocatorRef,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: CfIndex,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CfDictionaryRef;
    }

    #[link(name = "CoreMedia", kind = "framework")]
    unsafe extern "C" {
        fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
            allocator: CfAllocatorRef,
            parameter_set_count: usize,
            parameter_set_pointers: *const *const u8,
            parameter_set_sizes: *const usize,
            nal_unit_header_length: i32,
            format_description_out: *mut CmFormatDescriptionRef,
        ) -> OsStatus;
        fn CMBlockBufferCreateWithMemoryBlock(
            structure_allocator: CfAllocatorRef,
            memory_block: *mut c_void,
            block_length: usize,
            block_allocator: CfAllocatorRef,
            custom_block_source: *const c_void,
            offset_to_data: usize,
            data_length: usize,
            flags: u32,
            block_buffer_out: *mut CmBlockBufferRef,
        ) -> OsStatus;
        fn CMSampleBufferCreateReady(
            allocator: CfAllocatorRef,
            data_buffer: CmBlockBufferRef,
            format_description: CmFormatDescriptionRef,
            num_samples: CfIndex,
            num_sample_timing_entries: CfIndex,
            sample_timing_array: *const CmSampleTimingInfo,
            num_sample_size_entries: CfIndex,
            sample_size_array: *const usize,
            sample_buffer_out: *mut CmSampleBufferRef,
        ) -> OsStatus;
    }

    #[link(name = "CoreVideo", kind = "framework")]
    unsafe extern "C" {
        static kCVPixelBufferPixelFormatTypeKey: CfTypeRef;
        fn CVPixelBufferLockBaseAddress(buffer: CvImageBufferRef, flags: u64) -> CvReturn;
        fn CVPixelBufferUnlockBaseAddress(buffer: CvImageBufferRef, flags: u64) -> CvReturn;
        fn CVPixelBufferGetPixelFormatType(buffer: CvImageBufferRef) -> u32;
        fn CVPixelBufferGetPlaneCount(buffer: CvImageBufferRef) -> usize;
        fn CVPixelBufferGetBaseAddressOfPlane(buffer: CvImageBufferRef, plane: usize) -> *const u8;
        fn CVPixelBufferGetBytesPerRowOfPlane(buffer: CvImageBufferRef, plane: usize) -> usize;
        fn CVPixelBufferGetWidthOfPlane(buffer: CvImageBufferRef, plane: usize) -> usize;
        fn CVPixelBufferGetHeightOfPlane(buffer: CvImageBufferRef, plane: usize) -> usize;
    }

    #[link(name = "VideoToolbox", kind = "framework")]
    unsafe extern "C" {
        fn VTDecompressionSessionCreate(
            allocator: CfAllocatorRef,
            video_format_description: CmFormatDescriptionRef,
            video_decoder_specification: CfDictionaryRef,
            destination_image_buffer_attributes: CfDictionaryRef,
            output_callback: *const VtDecompressionOutputCallbackRecord,
            decompression_session_out: *mut VtSessionRef,
        ) -> OsStatus;
        fn VTDecompressionSessionDecodeFrame(
            session: VtSessionRef,
            sample_buffer: CmSampleBufferRef,
            decode_flags: u32,
            source_frame_refcon: *mut c_void,
            info_flags_out: *mut u32,
        ) -> OsStatus;
        fn VTDecompressionSessionInvalidate(session: VtSessionRef);
        fn VTDecompressionSessionWaitForAsynchronousFrames(session: VtSessionRef) -> OsStatus;
    }

    /// A VideoToolbox failure with its raw `OSStatus` preserved.
    ///
    /// Session-death detection (`kVTInvalidSessionErr`) compares this numerically;
    /// matching on a formatted string would silently break recovery the day the
    /// message changes.
    struct VtError {
        status: OsStatus,
        context: &'static str,
    }

    impl From<VtError> for DecoderError {
        fn from(e: VtError) -> Self {
            DecoderError::msg(format!("{} (status {})", e.context, e.status))
        }
    }

    /// Where the synchronous decode callback deposits its result.
    ///
    /// `out` points at the caller's live [`Yuv420Frame`] for the duration of the
    /// synchronous decode call; `produced` records that the callback actually
    /// filled it (a decode can "succeed" with no picture).
    struct CallbackSlot {
        status: OsStatus,
        produced: bool,
        out: *mut Yuv420Frame,
    }

    /// Copy one plane out of a locked pixel buffer into a tight-packed vec.
    ///
    /// Returns false (leaving the vec untouched beyond a resize) when the plane is
    /// missing or its stride is shorter than its width — wire-driven output is
    /// untrusted, and this runs inside an `extern "C"` callback where a panic would
    /// abort the process, so every access is guarded.
    ///
    /// # Safety
    /// `buffer` must be a locked, planar pixel buffer.
    unsafe fn copy_plane(
        buffer: CvImageBufferRef,
        plane: usize,
        expect_w: usize,
        expect_h: usize,
        out: &mut Vec<u8>,
    ) -> bool {
        // SAFETY: per contract, the buffer is locked and planar; plane accessors on
        // an out-of-range index return null/0, which the guards below reject.
        unsafe {
            let base = CVPixelBufferGetBaseAddressOfPlane(buffer, plane);
            let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, plane);
            let width = CVPixelBufferGetWidthOfPlane(buffer, plane);
            let height = CVPixelBufferGetHeightOfPlane(buffer, plane);
            if base.is_null()
                || width != expect_w
                || height != expect_h
                || stride < width
                || width == 0
            {
                return false;
            }
            out.clear();
            out.reserve(width * height);
            for row in 0..height {
                out.extend_from_slice(std::slice::from_raw_parts(base.add(row * stride), width));
            }
            true
        }
    }

    /// The output callback. Runs on the decoding thread before `DecodeFrame` returns
    /// (asynchronous decompression is never requested), so the raw pointer in
    /// `source_frame_refcon` is the caller's live stack slot.
    ///
    /// The delivered buffer is gated on its ACTUAL pixel format, not on what was
    /// requested: measured probes showed `CVPixelBufferGetBaseAddress` returns a
    /// non-null (but wrong) pointer on planar buffers and plane accessors "work" on
    /// packed ones, so only the format type itself is trustworthy.
    extern "C" fn decode_callback(
        _refcon: *mut c_void,
        source_frame_refcon: *mut c_void,
        status: OsStatus,
        _info_flags: u32,
        image_buffer: CvImageBufferRef,
        _pts: CmTime,
        _duration: CmTime,
    ) {
        // SAFETY: `source_frame_refcon` is the `&mut CallbackSlot` passed to
        // `VTDecompressionSessionDecodeFrame` by `decode_with_session`, alive for the
        // whole synchronous call.
        let slot = unsafe { &mut *source_frame_refcon.cast::<CallbackSlot>() };
        slot.status = status;
        if status != 0 || image_buffer.is_null() {
            return;
        }
        // SAFETY: VideoToolbox hands a valid, lockable pixel buffer for the duration
        // of the callback; `slot.out` is the caller's live frame per the slot contract.
        unsafe {
            if CVPixelBufferGetPixelFormatType(image_buffer)
                != K_CV_PIXEL_FORMAT_TYPE_420_PLANAR_FULL
                || CVPixelBufferGetPlaneCount(image_buffer) != 3
            {
                slot.status = -2;
                return;
            }
            if CVPixelBufferLockBaseAddress(image_buffer, K_CV_PIXEL_BUFFER_LOCK_READ_ONLY) != 0 {
                slot.status = -1;
                return;
            }
            let width = CVPixelBufferGetWidthOfPlane(image_buffer, 0);
            let height = CVPixelBufferGetHeightOfPlane(image_buffer, 0);
            let uv_w = width.div_ceil(2);
            let uv_h = height.div_ceil(2);
            let out = &mut *slot.out;
            if width > 0
                && height > 0
                && copy_plane(image_buffer, 0, width, height, &mut out.y)
                && copy_plane(image_buffer, 1, uv_w, uv_h, &mut out.u)
                && copy_plane(image_buffer, 2, uv_w, uv_h, &mut out.v)
            {
                out.width = width;
                out.height = height;
                slot.produced = true;
            } else {
                slot.status = -3;
            }
            CVPixelBufferUnlockBaseAddress(image_buffer, K_CV_PIXEL_BUFFER_LOCK_READ_ONLY);
        }
    }

    /// A live decompression session plus the parameter sets it was built from.
    struct Session {
        session: VtSessionRef,
        format: CmFormatDescriptionRef,
        sps: Vec<u8>,
        pps: Vec<u8>,
    }

    impl Drop for Session {
        fn drop(&mut self) {
            // SAFETY: both refs were created by this module and are released exactly once.
            unsafe {
                VTDecompressionSessionInvalidate(self.session);
                CFRelease(self.session);
                CFRelease(self.format);
            }
        }
    }

    /// H.264 decoder backed by VideoToolbox.
    ///
    /// The session is created lazily from the first in-band SPS/PPS and recreated when
    /// the parameter sets change (a resolution change) or VideoToolbox reports the
    /// session invalid (GPU reset, sleep/wake).
    pub struct VideoToolboxDecoder {
        session: Option<Session>,
        /// Parameter sets of the session BEFORE the current one — the ABAB
        /// oscillation guard (see `decode_yuv420`).
        previous_params: Option<(Vec<u8>, Vec<u8>)>,
        /// Reusable frame for the RGBA path (`decode` = YUV decode + conversion).
        scratch: Yuv420Frame,
    }

    // SAFETY: the raw VideoToolbox/CoreMedia references are used from one thread at a
    // time (the DVC processing thread owns the decoder; `H264Decoder: Send` moves it
    // there once). VideoToolbox sessions may be used from any single thread.
    unsafe impl Send for VideoToolboxDecoder {}

    impl VideoToolboxDecoder {
        pub fn new() -> Self {
            Self {
                session: None,
                previous_params: None,
                scratch: Yuv420Frame::default(),
            }
        }

        /// Build a decompression session for the given parameter sets.
        fn create_session(sps: &[u8], pps: &[u8]) -> DecoderResult<Session> {
            let mut format: CmFormatDescriptionRef = ptr::null();
            let pointers = [sps.as_ptr(), pps.as_ptr()];
            let sizes = [sps.len(), pps.len()];
            // SAFETY: pointer/size arrays are live for the call; out-pointer is valid.
            let status = unsafe {
                CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    ptr::null(),
                    2,
                    pointers.as_ptr(),
                    sizes.as_ptr(),
                    4, // AVCC 4-byte length prefixes, as on the EGFX wire
                    &mut format,
                )
            };
            if status != 0 || format.is_null() {
                return Err(DecoderError::msg(format!(
                    "CMVideoFormatDescription from SPS/PPS failed (status {status})"
                )));
            }

            // Ask for full-range planar 4:2:0 output — see the constant's docs for
            // why this is the one correct format (and the only one ever requested).
            // SAFETY: CF creation calls with valid arguments; ownership released below.
            let pixel_format = K_CV_PIXEL_FORMAT_TYPE_420_PLANAR_FULL;
            let session = unsafe {
                let num = CFNumberCreate(
                    ptr::null(),
                    K_CF_NUMBER_SINT32_TYPE,
                    (&raw const pixel_format).cast(),
                );
                if num.is_null() {
                    CFRelease(format);
                    return Err(DecoderError::msg("CFNumberCreate returned null"));
                }
                let keys = [kCVPixelBufferPixelFormatTypeKey.cast()];
                let values = [num.cast()];
                let attrs = CFDictionaryCreate(
                    ptr::null(),
                    keys.as_ptr(),
                    values.as_ptr(),
                    1,
                    &raw const kCFTypeDictionaryKeyCallBacks,
                    &raw const kCFTypeDictionaryValueCallBacks,
                );
                CFRelease(num);
                if attrs.is_null() {
                    CFRelease(format);
                    return Err(DecoderError::msg("CFDictionaryCreate returned null"));
                }
                let record = VtDecompressionOutputCallbackRecord {
                    callback: decode_callback,
                    refcon: ptr::null_mut(),
                };
                let mut session: VtSessionRef = ptr::null();
                let status = VTDecompressionSessionCreate(
                    ptr::null(),
                    format,
                    ptr::null(),
                    attrs,
                    &record,
                    &mut session,
                );
                CFRelease(attrs);
                if status != 0 || session.is_null() {
                    CFRelease(format);
                    return Err(DecoderError::msg(format!(
                        "VTDecompressionSessionCreate failed (status {status})"
                    )));
                }
                session
            };

            Ok(Session {
                session,
                format,
                sps: sps.to_vec(),
                pps: pps.to_vec(),
            })
        }

        /// Feed one AVCC access unit through the session, synchronously, filling `out`.
        fn decode_with_session(
            session: &Session,
            avcc: &mut [u8],
            out: &mut Yuv420Frame,
        ) -> Result<(), VtError> {
            let mut slot = CallbackSlot {
                status: 0,
                produced: false,
                out: core::ptr::from_mut(out),
            };
            // SAFETY: the block buffer borrows `avcc` (kCFAllocatorNull = no copy, no
            // free); the sample and block buffers are released before this function
            // returns, and the decode is synchronous, so the borrow outlives all use.
            unsafe {
                let mut block: CmBlockBufferRef = ptr::null();
                let status = CMBlockBufferCreateWithMemoryBlock(
                    ptr::null(),
                    avcc.as_mut_ptr().cast(),
                    avcc.len(),
                    kCFAllocatorNull,
                    ptr::null(),
                    0,
                    avcc.len(),
                    0,
                    &mut block,
                );
                if status != 0 || block.is_null() {
                    return Err(VtError {
                        status,
                        context: "CMBlockBuffer creation failed",
                    });
                }
                let timing = CmSampleTimingInfo {
                    duration: CmTime::default(),
                    presentation_time_stamp: CmTime::default(),
                    decode_time_stamp: CmTime::default(),
                };
                let sizes = [avcc.len()];
                let mut sample: CmSampleBufferRef = ptr::null();
                let status = CMSampleBufferCreateReady(
                    ptr::null(),
                    block,
                    session.format,
                    1,
                    1,
                    &timing,
                    1,
                    sizes.as_ptr(),
                    &mut sample,
                );
                if status != 0 || sample.is_null() {
                    CFRelease(block);
                    return Err(VtError {
                        status,
                        context: "CMSampleBuffer creation failed",
                    });
                }
                let mut info_flags = 0u32;
                let status = VTDecompressionSessionDecodeFrame(
                    session.session,
                    sample,
                    0, // synchronous: the callback runs before this returns
                    (&raw mut slot).cast(),
                    &mut info_flags,
                );
                // Asynchronous decompression is never requested, but Apple's
                // contract is "may decode asynchronously unless you wait" — and the
                // callback writes through raw pointers into THIS stack frame, so a
                // delayed callback would be memory corruption, not an error. The
                // wait turns that assumption into a guarantee for one call.
                VTDecompressionSessionWaitForAsynchronousFrames(session.session);
                CFRelease(sample);
                CFRelease(block);
                if status != 0 {
                    return Err(VtError {
                        status,
                        context: "VTDecompressionSessionDecodeFrame failed",
                    });
                }
            }
            if slot.status != 0 {
                return Err(VtError {
                    status: slot.status,
                    context: "VideoToolbox decode callback failed",
                });
            }
            if !slot.produced {
                return Err(VtError {
                    status: 0,
                    context: "VideoToolbox produced no picture",
                });
            }
            Ok(())
        }
    }

    impl H264Decoder for VideoToolboxDecoder {
        /// RGBA output, implemented on top of the planar path.
        ///
        /// One session, one output format: converting in software here (a few ms at
        /// desktop sizes, measured cheaper than a second BGRA-configured session)
        /// means an AVC420 frame arriving on a 444-negotiated connection can never
        /// force a session rebuild — which would kill decode until the next IDR.
        fn decode(&mut self, data: &[u8]) -> DecoderResult<DecodedFrame> {
            let mut out = core::mem::take(&mut self.scratch);
            let result = self.decode_yuv420(data, &mut out);
            let frame = result.map(|()| {
                let rgba = ironrdp_graphics::avc444::yuv420_to_rgba(&out);
                DecodedFrame::new(rgba, out.width as u32, out.height as u32)
            });
            self.scratch = out;
            frame
        }

        fn decode_yuv420(&mut self, data: &[u8], out: &mut Yuv420Frame) -> DecoderResult<()> {
            let units = nal_units(data);
            let (sps, pps) = parameter_sets(&units);

            // (Re)build the session when parameter sets first arrive or change —
            // EXCEPT when the "new" pair equals the pair before the current one.
            // That ABAB alternation is the signature of AVC444 sub-streams carrying
            // divergent parameter sets, and rebuilding on it would flush the decoder
            // twice per frame, destroying both reference chains until the next IDR
            // (a stall, not a glitch). Refusing without a rebuild keeps whichever
            // sub-stream built the current session alive (Windows uses one SPS for
            // both, so in practice this guard never fires); the caller skips and
            // counts the refused frames.
            if let (Some(sps), Some(pps)) = (sps, pps) {
                let stale = match &self.session {
                    Some(s) => s.sps != sps || s.pps != pps,
                    None => true,
                };
                if stale {
                    if let Some((prev_sps, prev_pps)) = &self.previous_params
                        && prev_sps == sps
                        && prev_pps == pps
                    {
                        return Err(DecoderError::msg(
                            "oscillating SPS/PPS (divergent AVC444 sub-streams); refusing to rebuild",
                        ));
                    }
                    debug!(
                        sps_len = sps.len(),
                        pps_len = pps.len(),
                        "creating VideoToolbox session from new parameter sets"
                    );
                    self.previous_params =
                        self.session.take().map(|s| (s.sps.clone(), s.pps.clone()));
                    self.session = Some(Self::create_session(sps, pps)?);
                }
            }
            let Some(session) = &self.session else {
                return Err(DecoderError::msg(
                    "no SPS/PPS seen yet; cannot decode this access unit",
                ));
            };

            // Re-encode the slice NALs (everything but SPS/PPS) as one AVCC sample.
            // SPS/PPS live in the format description; VideoToolbox rejects samples that
            // repeat them.
            let mut avcc = Vec::with_capacity(data.len());
            for nal in &units {
                if matches!(nal_type(nal), NAL_SPS | NAL_PPS) {
                    continue;
                }
                avcc.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                avcc.extend_from_slice(nal);
            }
            if avcc.is_empty() {
                return Err(DecoderError::msg("access unit carried no slice NALs"));
            }

            match Self::decode_with_session(session, &mut avcc, out) {
                Ok(()) => Ok(()),
                Err(e) if e.status == K_VT_INVALID_SESSION_ERR => {
                    // GPU reset or sleep/wake killed the session. Rebuild from the same
                    // parameter sets and retry once (recovery completes at the next IDR).
                    warn!("VideoToolbox session died; rebuilding and retrying");
                    let (sps, pps) = {
                        let s = self.session.take().expect("session existed above");
                        (s.sps.clone(), s.pps.clone())
                    };
                    let rebuilt = Self::create_session(&sps, &pps)?;
                    let result = Self::decode_with_session(&rebuilt, &mut avcc, out);
                    self.session = Some(rebuilt);
                    result.map_err(DecoderError::from)
                }
                Err(e) => Err(e.into()),
            }
        }

        fn supports_yuv420(&self) -> bool {
            true
        }

        fn reset(&mut self) {
            // New stream: the next access unit brings fresh SPS/PPS.
            self.session = None;
            self.previous_params = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn avcc(nals: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for nal in nals {
            out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            out.extend_from_slice(nal);
        }
        out
    }

    #[test]
    fn nal_units_split_annex_b_start_codes() {
        // The shape Windows actually sends (captured from quench 2026-08-16):
        // 4-byte start codes, AUD + SPS + PPS + slice.
        let mut stream = Vec::new();
        for nal in [
            &[0x09u8, 0x10][..],
            &[0x67, 0x4d, 0x00, 0x28],
            &[0x68, 0xee],
            &[0x65, 0x88, 0x84],
        ] {
            stream.extend_from_slice(&[0, 0, 0, 1]);
            stream.extend_from_slice(nal);
        }
        let units = nal_units(&stream);
        assert_eq!(units.len(), 4);
        assert_eq!(nal_type(units[0]), 9, "AUD");
        assert_eq!(nal_type(units[1]), NAL_SPS);
        assert_eq!(nal_type(units[2]), NAL_PPS);
        assert_eq!(units[3], &[0x65, 0x88, 0x84]);

        // 3-byte start codes split identically.
        let mut short = Vec::new();
        for nal in [&[0x67u8, 0x4d][..], &[0x65, 0x88]] {
            short.extend_from_slice(&[0, 0, 1]);
            short.extend_from_slice(nal);
        }
        assert_eq!(nal_units(&short), vec![&[0x67u8, 0x4d][..], &[0x65, 0x88]]);
    }

    #[test]
    fn annex_b_units_trim_trailing_zeros_but_keep_interior_ones() {
        // Trailing zeros are either the leading 00 of a 4-byte start code or
        // cabac_zero_words padding; interior zeros are payload.
        let stream = [
            0, 0, 0, 1, 0x65, 0x01, 0x00, 0x02, // slice with an interior zero
            0, 0, 0, 1, 0x41, 0x03, 0x00, 0x00, // final unit with zero padding
        ];
        let units = nal_units(&stream);
        assert_eq!(units.len(), 2);
        assert_eq!(units[0], &[0x65, 0x01, 0x00, 0x02]);
        assert_eq!(units[1], &[0x41, 0x03], "trailing zero padding trimmed");
    }

    #[test]
    fn a_stream_without_start_codes_is_walked_as_avcc() {
        // The framing the upstream docs promise; kept for spec-faithful servers.
        let stream = avcc(&[&[0x67, 1, 2], &[0x65, 3]]);
        let units = nal_units(&stream);
        assert_eq!(units.len(), 2);
        assert_eq!(units[0], &[0x67, 1, 2]);
    }

    #[test]
    fn nal_units_walk_length_prefixes_and_stop_at_truncation() {
        let stream = avcc(&[&[0x67, 1, 2], &[0x68, 3], &[0x65, 4, 5, 6]]);
        let units = nal_units(&stream);
        assert_eq!(units.len(), 3);
        assert_eq!(units[0], &[0x67, 1, 2]);
        assert_eq!(units[2], &[0x65, 4, 5, 6]);

        // A lying length prefix must not panic or over-read; the good prefix survives.
        let mut truncated = avcc(&[&[0x67, 1, 2]]);
        truncated.extend_from_slice(&[0x00, 0x00, 0x10, 0x00]); // promises 4096 bytes
        truncated.extend_from_slice(&[0xAA; 3]); // delivers 3
        assert_eq!(nal_units(&truncated).len(), 1);
    }

    #[test]
    fn parameter_sets_find_sps_and_pps_wherever_they_sit() {
        let sps = [0x67, 0x42, 0x00];
        let pps = [0x68, 0xCE];
        let idr = [0x65, 0x88];
        let stream = avcc(&[&idr, &sps, &pps]);
        let units = nal_units(&stream);
        let (found_sps, found_pps) = parameter_sets(&units);
        assert_eq!(found_sps, Some(&sps[..]), "SPS is nal_unit_type 7");
        assert_eq!(found_pps, Some(&pps[..]), "PPS is nal_unit_type 8");

        let none = avcc(&[&idr]);
        let units = nal_units(&none);
        assert_eq!(parameter_sets(&units), (None, None));
    }
}
