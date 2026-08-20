//! Hardware HEVC decode for rhydra's native video stream.

use ironrdp_egfx::decode::{DecodedFrame, DecoderResult};

/// Decoder contract owned by mdrdp rather than by the RDP/EGFX crate.
pub trait VideoDecoder: Send {
    fn decode(&mut self, data: &[u8]) -> DecoderResult<DecodedFrame>;

    fn reset(&mut self) {}
}

/// Whether this build carries a hardware HEVC decoder.
pub fn hardware_decode_available() -> bool {
    cfg!(target_os = "macos")
}

/// The platform's hardware HEVC decoder, if this build has one.
pub fn hardware_decoder() -> Option<Box<dyn VideoDecoder>> {
    #[cfg(target_os = "macos")]
    {
        Some(Box::new(videotoolbox::VideoToolboxDecoder::new()))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

pub const NAL_VPS: u8 = 32;
pub const NAL_SPS: u8 = 33;
pub const NAL_PPS: u8 = 34;

pub type ParameterSets<'a> = (Option<&'a [u8]>, Option<&'a [u8]>, Option<&'a [u8]>);

pub fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    if data.starts_with(&[0, 0, 1]) || data.starts_with(&[0, 0, 0, 1]) {
        annex_b_units(data)
    } else {
        avcc_units(data)
    }
}

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
    for (index, &start) in starts.iter().enumerate() {
        let mut end = starts
            .get(index + 1)
            .copied()
            .map_or(data.len(), |next| next - 3);
        while end > start && data[end - 1] == 0 {
            end -= 1;
        }
        if end > start {
            units.push(&data[start..end]);
        }
    }
    units
}

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
        if len == 0 || end > data.len() {
            break;
        }
        units.push(&data[offset..end]);
        offset = end;
    }
    units
}

pub fn nal_type(nal: &[u8]) -> Option<u8> {
    let header = nal.get(..2)?;
    // forbidden_zero_bit must be clear, and nuh_temporal_id_plus1 == 0 is invalid.
    if header[0] & 0x80 != 0 || header[1] & 0x07 == 0 {
        return None;
    }
    Some((header[0] >> 1) & 0x3F)
}

pub fn contains_irap(data: &[u8]) -> bool {
    nal_units(data)
        .iter()
        .any(|nal| nal_type(nal).is_some_and(|kind| (16..=23).contains(&kind)))
}

pub fn parameter_sets<'a>(units: &[&'a [u8]]) -> ParameterSets<'a> {
    let mut vps = None;
    let mut sps = None;
    let mut pps = None;
    for &nal in units {
        match nal_type(nal) {
            Some(NAL_VPS) => vps = Some(nal),
            Some(NAL_SPS) => sps = Some(nal),
            Some(NAL_PPS) => pps = Some(nal),
            _ => {}
        }
    }
    (vps, sps, pps)
}

/// Convert limited-range BT.709 NV12 samples into RGBA8888.
pub fn video_range_yuv_to_rgba(
    y: &[u8],
    uv: &[u8],
    width: usize,
    height: usize,
    y_stride: usize,
    uv_stride: usize,
) -> Option<Vec<u8>> {
    let y_len = y_stride.checked_mul(height)?;
    let uv_height = height.div_ceil(2);
    let uv_width = width.div_ceil(2);
    let uv_len = uv_stride.checked_mul(uv_height)?;
    let rgba_len = width.checked_mul(height)?.checked_mul(4)?;
    if width == 0
        || height == 0
        || y_stride < width
        || uv_stride < uv_width.checked_mul(2)?
        || y.len() < y_len
        || uv.len() < uv_len
    {
        return None;
    }

    let mut out = Vec::with_capacity(rgba_len);
    for row in 0..height {
        let y_row = &y[row * y_stride..row * y_stride + width];
        let uv_row = &uv[(row / 2) * uv_stride..(row / 2) * uv_stride + uv_width * 2];
        for (column, &y_sample) in y_row.iter().enumerate() {
            let chroma = (column / 2) * 2;
            let u = uv_row[chroma];
            let v = uv_row[chroma + 1];
            out.extend_from_slice(&video_range_bt709_pixel(y_sample, u, v));
        }
    }
    Some(out)
}

/// BT.709 limited-range (16..235 luma, 16..240 chroma) to RGB.
///
/// The integer coefficients are the ITU-R BT.709 video-range matrix scaled by 256:
/// `R = (298 C + 459 E) / 256`, `G = (298 C - 55 D - 136 E) / 256`, and
/// `B = (298 C + 541 D) / 256`, where `C = Y - 16`, `D = U - 128`, and `E = V - 128`.
fn video_range_bt709_pixel(y: u8, u: u8, v: u8) -> [u8; 4] {
    let c = (i32::from(y) - 16).max(0);
    let d = i32::from(u) - 128;
    let e = i32::from(v) - 128;
    let round = 128;
    [
        ((298 * c + 459 * e + round) >> 8).clamp(0, 255) as u8,
        ((298 * c - 55 * d - 136 * e + round) >> 8).clamp(0, 255) as u8,
        ((298 * c + 541 * d + round) >> 8).clamp(0, 255) as u8,
        255,
    ]
}

#[cfg(target_os = "macos")]
mod videotoolbox {
    use std::ffi::c_void;
    use std::ptr;

    use ironrdp_egfx::decode::DecoderError;
    use tracing::{debug, warn};

    use super::*;

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

    /// CMTime by value. Native access units carry no media timestamp.
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

    /// `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange` (`'420v'`).
    ///
    /// This is deliberately the only accepted output. It gives the callback the
    /// studio-range NV12 contract that rhydra's host encoder produces, so the
    /// conversion below can apply the BT.709 video-range matrix explicitly.
    const K_CV_PIXEL_FORMAT_TYPE_420_BIPLANAR_VIDEO: u32 = u32::from_be_bytes(*b"420v");
    const K_CF_NUMBER_SINT32_TYPE: CfIndex = 3;
    const K_CV_PIXEL_BUFFER_LOCK_READ_ONLY: u64 = 1;
    /// `kVTInvalidSessionErr`: GPU reset or sleep/wake invalidated the session.
    const K_VT_INVALID_SESSION_ERR: OsStatus = -12903;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
        static kCFAllocatorNull: CfAllocatorRef;
        static kCFBooleanTrue: CfTypeRef;
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
        fn CMVideoFormatDescriptionCreateFromHEVCParameterSets(
            allocator: CfAllocatorRef,
            parameter_set_count: usize,
            parameter_set_pointers: *const *const u8,
            parameter_set_sizes: *const usize,
            nal_unit_header_length: i32,
            extensions: CfDictionaryRef,
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
    // H.264 and HEVC each keep a private callback record while their existing
    // backends remain intentionally unchanged. The records have identical C ABI;
    // Rust's lint compares their private nominal types and would otherwise report a
    // harmless duplicate declaration of the same VideoToolbox symbol.
    #[allow(clashing_extern_declarations)]
    unsafe extern "C" {
        /// The decoder specification key is a CoreFoundation string exported by
        /// VideoToolbox. Passing it in the specification dictionary is what makes
        /// hardware decode a requirement rather than a best-effort preference.
        static kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder: CfTypeRef;
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

    struct VtError {
        status: OsStatus,
        context: &'static str,
    }

    impl From<VtError> for DecoderError {
        fn from(error: VtError) -> Self {
            DecoderError::msg(format!("{} (status {})", error.context, error.status))
        }
    }

    /// The one frame produced by the synchronous callback.
    #[derive(Default)]
    struct OutputFrame {
        rgba: Vec<u8>,
        width: usize,
        height: usize,
    }

    struct CallbackSlot {
        status: OsStatus,
        produced: bool,
        out: *mut OutputFrame,
    }

    /// Convert one locked `420v` pixel buffer directly into the caller's RGBA frame.
    ///
    /// The callback checks the delivered format instead of trusting the destination
    /// request. It rejects planar `f420`, full-range `420f`, packed output, and any
    /// malformed stride. The only accepted contract is two-plane, studio-range NV12.
    extern "C" fn decode_callback(
        _refcon: *mut c_void,
        source_frame_refcon: *mut c_void,
        status: OsStatus,
        _info_flags: u32,
        image_buffer: CvImageBufferRef,
        _pts: CmTime,
        _duration: CmTime,
    ) {
        // SAFETY: DecodeFrame is synchronously drained below; this pointer remains
        // valid until it returns, just as the callback's contract requires.
        let Some(slot) = (unsafe { source_frame_refcon.cast::<CallbackSlot>().as_mut() }) else {
            return;
        };
        slot.status = status;
        if status != 0 || image_buffer.is_null() {
            return;
        }

        // SAFETY: VideoToolbox owns a valid image buffer for the callback's duration.
        unsafe {
            if CVPixelBufferGetPixelFormatType(image_buffer)
                != K_CV_PIXEL_FORMAT_TYPE_420_BIPLANAR_VIDEO
                || CVPixelBufferGetPlaneCount(image_buffer) != 2
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
            let uv_width = width.div_ceil(2);
            let uv_height = height.div_ceil(2);
            let y_base = CVPixelBufferGetBaseAddressOfPlane(image_buffer, 0);
            let uv_base = CVPixelBufferGetBaseAddressOfPlane(image_buffer, 1);
            let y_stride = CVPixelBufferGetBytesPerRowOfPlane(image_buffer, 0);
            let uv_stride = CVPixelBufferGetBytesPerRowOfPlane(image_buffer, 1);
            let valid = width > 0
                && height > 0
                && !y_base.is_null()
                && !uv_base.is_null()
                && y_stride >= width
                && uv_stride >= uv_width.saturating_mul(2)
                && CVPixelBufferGetHeightOfPlane(image_buffer, 1) >= uv_height;
            if !valid {
                slot.status = -3;
                CVPixelBufferUnlockBaseAddress(image_buffer, K_CV_PIXEL_BUFFER_LOCK_READ_ONLY);
                return;
            }

            let out = &mut *slot.out;
            let Some(rgba_len) = width
                .checked_mul(height)
                .and_then(|pixels| pixels.checked_mul(4))
            else {
                slot.status = -4;
                CVPixelBufferUnlockBaseAddress(image_buffer, K_CV_PIXEL_BUFFER_LOCK_READ_ONLY);
                return;
            };
            if out.rgba.try_reserve(rgba_len).is_err() {
                slot.status = -4;
                CVPixelBufferUnlockBaseAddress(image_buffer, K_CV_PIXEL_BUFFER_LOCK_READ_ONLY);
                return;
            }
            out.rgba.clear();
            out.rgba.resize(rgba_len, 0);
            for row in 0..height {
                let y_row = y_base.add(row * y_stride);
                let uv_row = uv_base.add((row / 2) * uv_stride);
                for column in 0..width {
                    let chroma = (column / 2) * 2;
                    let pixel = super::video_range_bt709_pixel(
                        *y_row.add(column),
                        *uv_row.add(chroma),
                        *uv_row.add(chroma + 1),
                    );
                    let offset = (row * width + column) * 4;
                    out.rgba[offset..offset + 4].copy_from_slice(&pixel);
                }
            }
            out.width = width;
            out.height = height;
            slot.produced = true;
            CVPixelBufferUnlockBaseAddress(image_buffer, K_CV_PIXEL_BUFFER_LOCK_READ_ONLY);
        }
    }

    struct Session {
        session: VtSessionRef,
        format: CmFormatDescriptionRef,
        vps: Vec<u8>,
        sps: Vec<u8>,
        pps: Vec<u8>,
    }

    impl Drop for Session {
        fn drop(&mut self) {
            // SAFETY: both references came from successful creation calls and are
            // released exactly once here.
            unsafe {
                VTDecompressionSessionInvalidate(self.session);
                CFRelease(self.session);
                CFRelease(self.format);
            }
        }
    }

    /// HEVC decoder backed by a hardware-required VideoToolbox session.
    pub struct VideoToolboxDecoder {
        session: Option<Session>,
        vps: Option<Vec<u8>>,
        sps: Option<Vec<u8>>,
        pps: Option<Vec<u8>>,
    }

    // SAFETY: the native video thread owns and uses a decoder from one thread at a
    // time. VideoToolbox permits a session to be driven by that one thread, and the
    // raw references never escape the decoder's ownership.
    unsafe impl Send for VideoToolboxDecoder {}

    impl Default for VideoToolboxDecoder {
        fn default() -> Self {
            Self::new()
        }
    }

    impl VideoToolboxDecoder {
        pub fn new() -> Self {
            Self {
                session: None,
                vps: None,
                sps: None,
                pps: None,
            }
        }

        fn create_session(vps: &[u8], sps: &[u8], pps: &[u8]) -> DecoderResult<Session> {
            if vps.is_empty() || sps.is_empty() || pps.is_empty() {
                return Err(DecoderError::msg("HEVC session needs VPS, SPS, and PPS"));
            }

            let mut format: CmFormatDescriptionRef = ptr::null();
            let pointers = [vps.as_ptr(), sps.as_ptr(), pps.as_ptr()];
            let sizes = [vps.len(), sps.len(), pps.len()];
            // SAFETY: all parameter-set slices and pointer arrays stay alive for the
            // duration of the CoreMedia call; `format` is a valid out-pointer.
            let status = unsafe {
                CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    ptr::null(),
                    3,
                    pointers.as_ptr(),
                    sizes.as_ptr(),
                    4,
                    ptr::null(), // extensions: no codec-specific override
                    &mut format,
                )
            };
            if status != 0 || format.is_null() {
                return Err(DecoderError::msg(format!(
                    "CMVideoFormatDescription from VPS/SPS/PPS failed (status {status})"
                )));
            }

            // Create the destination request and the hardware-required decoder
            // specification. Both dictionaries own their temporary values only for
            // this call; the session retains the resulting configuration.
            let (attrs, specification) = unsafe {
                let pixel_format = K_CV_PIXEL_FORMAT_TYPE_420_BIPLANAR_VIDEO;
                let number = CFNumberCreate(
                    ptr::null(),
                    K_CF_NUMBER_SINT32_TYPE,
                    (&raw const pixel_format).cast(),
                );
                if number.is_null() {
                    CFRelease(format);
                    return Err(DecoderError::msg("CFNumberCreate returned null"));
                }
                let attr_keys = [kCVPixelBufferPixelFormatTypeKey];
                let attr_values = [number];
                let attrs = CFDictionaryCreate(
                    ptr::null(),
                    attr_keys.as_ptr(),
                    attr_values.as_ptr(),
                    1,
                    &raw const kCFTypeDictionaryKeyCallBacks,
                    &raw const kCFTypeDictionaryValueCallBacks,
                );
                CFRelease(number);
                if attrs.is_null() {
                    CFRelease(format);
                    return Err(DecoderError::msg(
                        "pixel-buffer attributes dictionary is null",
                    ));
                }

                let spec_keys =
                    [kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder];
                let spec_values = [kCFBooleanTrue];
                let specification = CFDictionaryCreate(
                    ptr::null(),
                    spec_keys.as_ptr(),
                    spec_values.as_ptr(),
                    1,
                    &raw const kCFTypeDictionaryKeyCallBacks,
                    &raw const kCFTypeDictionaryValueCallBacks,
                );
                if specification.is_null() {
                    CFRelease(attrs);
                    CFRelease(format);
                    return Err(DecoderError::msg("hardware decoder specification is null"));
                }
                (attrs, specification)
            };

            let record = VtDecompressionOutputCallbackRecord {
                callback: decode_callback,
                refcon: ptr::null_mut(),
            };
            let mut session: VtSessionRef = ptr::null();
            // SAFETY: `attrs`, `specification`, and `record` remain alive through the
            // session creation call. The session receives a retained description.
            let status = unsafe {
                let status = VTDecompressionSessionCreate(
                    ptr::null(),
                    format,
                    specification,
                    attrs,
                    &record,
                    &mut session,
                );
                CFRelease(attrs);
                CFRelease(specification);
                status
            };
            if status != 0 || session.is_null() {
                unsafe { CFRelease(format) };
                return Err(DecoderError::msg(format!(
                    "hardware HEVC VTDecompressionSessionCreate failed (status {status})"
                )));
            }

            Ok(Session {
                session,
                format,
                vps: vps.to_vec(),
                sps: sps.to_vec(),
                pps: pps.to_vec(),
            })
        }

        fn decode_with_session(
            session: &Session,
            avcc: &mut [u8],
            out: &mut OutputFrame,
        ) -> Result<(), VtError> {
            let mut slot = CallbackSlot {
                status: 0,
                produced: false,
                out: ptr::from_mut(out),
            };
            // SAFETY: CoreMedia borrows `avcc` through kCFAllocatorNull. The sample
            // and block buffers are released before this function returns, and the
            // synchronous wait keeps the callback's pointer valid.
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
                        context: "HEVC CMBlockBuffer creation failed",
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
                        context: "HEVC CMSampleBuffer creation failed",
                    });
                }
                let mut info_flags = 0u32;
                let status = VTDecompressionSessionDecodeFrame(
                    session.session,
                    sample,
                    0,
                    ptr::from_mut(&mut slot).cast(),
                    &mut info_flags,
                );
                let wait_status = VTDecompressionSessionWaitForAsynchronousFrames(session.session);
                CFRelease(sample);
                CFRelease(block);
                if status != 0 {
                    return Err(VtError {
                        status,
                        context: "HEVC VTDecompressionSessionDecodeFrame failed",
                    });
                }
                if wait_status != 0 {
                    return Err(VtError {
                        status: wait_status,
                        context: "HEVC VideoToolbox asynchronous wait failed",
                    });
                }
            }
            if slot.status != 0 {
                return Err(VtError {
                    status: slot.status,
                    context: "HEVC VideoToolbox callback rejected output",
                });
            }
            if !slot.produced {
                return Err(VtError {
                    status: 0,
                    context: "HEVC VideoToolbox produced no picture",
                });
            }
            Ok(())
        }
    }

    impl VideoDecoder for VideoToolboxDecoder {
        fn decode(&mut self, data: &[u8]) -> DecoderResult<DecodedFrame> {
            let units = nal_units(data);
            if units.is_empty() {
                return Err(DecoderError::msg("HEVC access unit has no NAL units"));
            }
            let (vps, sps, pps) = parameter_sets(&units);
            match (vps, sps, pps) {
                (None, None, None) => {}
                (Some(vps), Some(sps), Some(pps)) => {
                    // Replace the epoch atomically. Combining one new set with two
                    // cached old sets can create a syntactically valid but false
                    // hvcC description during renegotiation.
                    self.vps = Some(vps.to_vec());
                    self.sps = Some(sps.to_vec());
                    self.pps = Some(pps.to_vec());
                }
                _ => {
                    return Err(DecoderError::msg(
                        "HEVC access unit carries an incomplete VPS/SPS/PPS epoch",
                    ));
                }
            }

            let have_sets = self
                .vps
                .as_deref()
                .zip(self.sps.as_deref())
                .zip(self.pps.as_deref());
            if let Some(((vps, sps), pps)) = have_sets {
                let stale = self.session.as_ref().is_none_or(|session| {
                    session.vps != vps || session.sps != sps || session.pps != pps
                });
                if stale {
                    debug!(
                        vps_len = vps.len(),
                        sps_len = sps.len(),
                        pps_len = pps.len(),
                        "creating hardware HEVC VideoToolbox session"
                    );
                    self.session = Some(Self::create_session(vps, sps, pps)?);
                }
            }

            let session = self.session.as_ref().ok_or_else(|| {
                DecoderError::msg("HEVC access unit has no complete VPS/SPS/PPS triplet")
            })?;
            let mut avcc = Vec::with_capacity(data.len());
            for nal in units {
                let kind = nal_type(nal).ok_or_else(|| {
                    DecoderError::msg("HEVC access unit has an invalid NAL header")
                })?;
                if matches!(kind, NAL_VPS | NAL_SPS | NAL_PPS) {
                    continue;
                }
                let len = u32::try_from(nal.len())
                    .map_err(|_| DecoderError::msg("HEVC NAL exceeds AVCC length limit"))?;
                avcc.extend_from_slice(&len.to_be_bytes());
                avcc.extend_from_slice(nal);
            }
            if avcc.is_empty() {
                return Err(DecoderError::msg("HEVC access unit carried no slice NALs"));
            }

            let mut out = OutputFrame::default();
            match Self::decode_with_session(session, &mut avcc, &mut out) {
                Ok(()) => Ok(DecodedFrame::new(
                    out.rgba,
                    out.width as u32,
                    out.height as u32,
                )),
                Err(error) if error.status == K_VT_INVALID_SESSION_ERR => {
                    warn!("hardware HEVC VideoToolbox session died; rebuilding and retrying");
                    let old = self.session.take().expect("session existed above");
                    let rebuilt = Self::create_session(&old.vps, &old.sps, &old.pps)?;
                    let result = Self::decode_with_session(&rebuilt, &mut avcc, &mut out);
                    self.session = Some(rebuilt);
                    result
                        .map_err(DecoderError::from)
                        .map(|()| DecodedFrame::new(out.rgba, out.width as u32, out.height as u32))
                }
                Err(error) => Err(error.into()),
            }
        }

        fn reset(&mut self) {
            self.session = None;
            self.vps = None;
            self.sps = None;
            self.pps = None;
        }
    }
}

#[cfg(target_os = "macos")]
pub use videotoolbox::VideoToolboxDecoder;

#[cfg(test)]
mod tests {
    use super::*;

    fn annex_b(nals: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for nal in nals {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
        }
        out
    }

    #[test]
    fn hevc_two_byte_headers_parse_parameter_sets_and_irap() {
        let vps = [0x40, 0x01, 0xAA];
        let sps = [0x42, 0x01, 0xBB];
        let pps = [0x44, 0x01, 0xCC];
        let idr = [0x26, 0x01, 0xDD]; // nal_unit_type = 19, temporal_id_plus1 = 1
        let stream = annex_b(&[&vps, &sps, &pps, &idr]);
        let units = nal_units(&stream);
        assert_eq!(units.len(), 4);
        assert_eq!(nal_type(units[0]), Some(NAL_VPS));
        assert_eq!(nal_type(units[1]), Some(NAL_SPS));
        assert_eq!(nal_type(units[2]), Some(NAL_PPS));
        assert_eq!(nal_type(units[3]), Some(19));
        assert!(contains_irap(&stream));
        assert_eq!(
            parameter_sets(&units),
            (Some(&vps[..]), Some(&sps[..]), Some(&pps[..]))
        );
    }

    #[test]
    fn hevc_parser_rejects_short_and_zero_temporal_headers() {
        let stream = annex_b(&[&[0x40], &[0x40, 0x00], &[0x26, 0x00]]);
        let units = nal_units(&stream);
        assert_eq!(nal_type(units[0]), None);
        assert_eq!(nal_type(units[1]), None);
        assert_eq!(nal_type(units[2]), None);
        assert!(!contains_irap(&stream));
    }

    #[test]
    fn video_range_bt709_conversion_uses_studio_black_and_white() {
        let y = [16, 235];
        let uv = [128, 128, 128, 128];
        let rgba = video_range_yuv_to_rgba(&y, &uv, 2, 1, 2, 4).unwrap();
        assert_eq!(&rgba, &[0, 0, 0, 255, 255, 255, 255, 255]);
    }

    #[test]
    fn video_range_bt709_conversion_rejects_short_strides_and_buffers() {
        assert!(video_range_yuv_to_rgba(&[16, 235], &[128, 128, 128, 128], 2, 1, 2, 4).is_some());
        assert!(video_range_yuv_to_rgba(&[16], &[128, 128], 2, 1, 2, 2).is_none());
        assert!(video_range_yuv_to_rgba(&[16, 235], &[128, 128], 2, 1, 1, 2).is_none());
    }

    #[test]
    fn hevc_avcc_access_units_are_split_and_truncated_lengths_stop_cleanly() {
        let nals = [&[0x40, 0x01, 0xAA][..], &[0x26, 0x01, 0xDD]];
        let mut stream = Vec::new();
        for nal in nals {
            stream.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            stream.extend_from_slice(nal);
        }
        let units = nal_units(&stream);
        assert_eq!(units, nals);

        stream.extend_from_slice(&[0, 0, 0, 8, 0x40]);
        assert_eq!(nal_units(&stream), nals);
    }

    #[test]
    fn video_range_bt709_conversion_uses_bt709_not_bt601_chroma_coefficients() {
        let rgba = video_range_yuv_to_rgba(&[100], &[90, 180], 1, 1, 1, 2).unwrap();
        assert_eq!(rgba, [191, 78, 17, 255]);
    }
}
