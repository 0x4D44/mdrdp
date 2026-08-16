//! Hardware H.264 decode for EGFX AVC streams.
//!
//! The EGFX pipeline hands [`ironrdp_egfx::decode::H264Decoder`] AVC-format access
//! units: NAL units with 4-byte big-endian length prefixes (AVCC), exactly the framing
//! Apple's VideoToolbox consumes natively — no Annex-B conversion, no transcode. The
//! macOS implementation drives a `VTDecompressionSession` with BGRA output, so the
//! GPU/media engine does both the H.264 decode and the YUV→RGB conversion that dominate
//! the software path's cost.
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
/// The capability advertisement (see `GfxHandler::advertising_avc420`) and the decoder
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
// AVCC parsing — portable, pure, tested
// ---------------------------------------------------------------------------------------
//
// Only the macOS backend consumes these today, so they are compiled for macOS and for
// tests; a blanket allow(dead_code) would also hide a genuinely dropped call site.

/// NAL unit types this module cares about (H.264 spec, `nal_unit_type`).
#[cfg(any(target_os = "macos", test))]
const NAL_SPS: u8 = 7;
#[cfg(any(target_os = "macos", test))]
const NAL_PPS: u8 = 8;

/// Split an AVCC stream (4-byte big-endian length prefix per NAL) into NAL units.
///
/// A truncated tail — a length prefix promising more bytes than remain — ends the walk;
/// everything before it is still returned. Wire data is untrusted, so this must never
/// panic or over-read.
#[cfg(any(target_os = "macos", test))]
fn nal_units(data: &[u8]) -> Vec<&[u8]> {
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

    use ironrdp_egfx::decode::{DecodedFrame, DecoderError, DecoderResult, H264Decoder};
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

    const K_CV_PIXEL_FORMAT_TYPE_32BGRA: u32 = u32::from_be_bytes(*b"BGRA");
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
        fn CVPixelBufferGetBaseAddress(buffer: CvImageBufferRef) -> *const u8;
        fn CVPixelBufferGetBytesPerRow(buffer: CvImageBufferRef) -> usize;
        fn CVPixelBufferGetWidth(buffer: CvImageBufferRef) -> usize;
        fn CVPixelBufferGetHeight(buffer: CvImageBufferRef) -> usize;
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
    }

    /// Where the synchronous decode callback deposits its result.
    #[derive(Default)]
    struct CallbackSlot {
        status: OsStatus,
        frame: Option<DecodedFrame>,
    }

    /// The output callback. Runs on the decoding thread before `DecodeFrame` returns
    /// (asynchronous decompression is never requested), so the raw pointer in
    /// `source_frame_refcon` is the caller's live stack slot.
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
        // SAFETY: VideoToolbox hands a valid, locked-lockable pixel buffer for the
        // duration of the callback.
        unsafe {
            if CVPixelBufferLockBaseAddress(image_buffer, K_CV_PIXEL_BUFFER_LOCK_READ_ONLY) != 0 {
                slot.status = -1;
                return;
            }
            let width = CVPixelBufferGetWidth(image_buffer);
            let height = CVPixelBufferGetHeight(image_buffer);
            let stride = CVPixelBufferGetBytesPerRow(image_buffer);
            let base = CVPixelBufferGetBaseAddress(image_buffer);
            if !base.is_null() && width > 0 && height > 0 && stride >= width * 4 {
                let mut rgba = vec![0u8; width * height * 4];
                for row in 0..height {
                    let src = std::slice::from_raw_parts(base.add(row * stride), width * 4);
                    let dst = &mut rgba[row * width * 4..(row + 1) * width * 4];
                    // BGRA -> RGBA
                    for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                        d[0] = s[2];
                        d[1] = s[1];
                        d[2] = s[0];
                        d[3] = s[3];
                    }
                }
                slot.frame = Some(DecodedFrame::new(rgba, width as u32, height as u32));
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
    }

    // SAFETY: the raw VideoToolbox/CoreMedia references are used from one thread at a
    // time (the DVC processing thread owns the decoder; `H264Decoder: Send` moves it
    // there once). VideoToolbox sessions may be used from any single thread.
    unsafe impl Send for VideoToolboxDecoder {}

    impl VideoToolboxDecoder {
        pub fn new() -> Self {
            Self { session: None }
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

            // Ask for BGRA output so the media engine does the YUV->RGB conversion.
            // SAFETY: CF creation calls with valid arguments; ownership released below.
            let pixel_format = K_CV_PIXEL_FORMAT_TYPE_32BGRA;
            let session = unsafe {
                let num = CFNumberCreate(
                    ptr::null(),
                    K_CF_NUMBER_SINT32_TYPE,
                    (&raw const pixel_format).cast(),
                );
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

        /// Feed one AVCC access unit through the session, synchronously.
        fn decode_with_session(session: &Session, avcc: &mut [u8]) -> DecoderResult<DecodedFrame> {
            let mut slot = CallbackSlot::default();
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
                    return Err(DecoderError::msg(format!(
                        "CMBlockBuffer creation failed (status {status})"
                    )));
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
                    return Err(DecoderError::msg(format!(
                        "CMSampleBuffer creation failed (status {status})"
                    )));
                }
                let mut info_flags = 0u32;
                let status = VTDecompressionSessionDecodeFrame(
                    session.session,
                    sample,
                    0, // synchronous: the callback runs before this returns
                    (&raw mut slot).cast(),
                    &mut info_flags,
                );
                CFRelease(sample);
                CFRelease(block);
                if status != 0 {
                    return Err(DecoderError::msg(format!(
                        "VTDecompressionSessionDecodeFrame failed (status {status})"
                    )));
                }
            }
            if slot.status != 0 {
                return Err(DecoderError::msg(format!(
                    "VideoToolbox decode callback reported status {}",
                    slot.status
                )));
            }
            slot.frame
                .ok_or_else(|| DecoderError::msg("VideoToolbox produced no picture"))
        }

        /// `true` when the VT status means "throw the session away and rebuild".
        fn is_session_death(err: &DecoderError) -> bool {
            err.to_string()
                .contains(&K_VT_INVALID_SESSION_ERR.to_string())
        }
    }

    impl H264Decoder for VideoToolboxDecoder {
        fn decode(&mut self, data: &[u8]) -> DecoderResult<DecodedFrame> {
            let units = nal_units(data);
            let (sps, pps) = parameter_sets(&units);

            // (Re)build the session when parameter sets first arrive or change.
            if let (Some(sps), Some(pps)) = (sps, pps) {
                let stale = match &self.session {
                    Some(s) => s.sps != sps || s.pps != pps,
                    None => true,
                };
                if stale {
                    debug!(
                        sps_len = sps.len(),
                        pps_len = pps.len(),
                        "creating VideoToolbox session from new parameter sets"
                    );
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

            match Self::decode_with_session(session, &mut avcc) {
                Ok(frame) => Ok(frame),
                Err(e) if Self::is_session_death(&e) => {
                    // GPU reset or sleep/wake killed the session. Rebuild from the same
                    // parameter sets and retry once.
                    warn!("VideoToolbox session died; rebuilding and retrying");
                    let (sps, pps) = {
                        let s = self.session.take().expect("session existed above");
                        (s.sps.clone(), s.pps.clone())
                    };
                    let rebuilt = Self::create_session(&sps, &pps)?;
                    let result = Self::decode_with_session(&rebuilt, &mut avcc);
                    self.session = Some(rebuilt);
                    result
                }
                Err(e) => Err(e),
            }
        }

        fn reset(&mut self) {
            // New stream: the next access unit brings fresh SPS/PPS.
            self.session = None;
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
