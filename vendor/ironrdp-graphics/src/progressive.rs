//! Progressive RFX decode and encode algorithms ([MS-RDPEGFX] 2.2.4.2).
//!
//! Provides first-pass decode (RLGR1 + progressive dequantization + sign capture)
//! and upgrade-pass decode (SRL/raw routing by DAS sign state, coefficient
//! accumulation) for the RemoteFX Progressive codec.
//!
//! These are pure algorithmic functions operating on coefficient buffers.
//! Tile state management and EGFX integration belong in a higher layer.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::collections::btree_map::Entry;

use ironrdp_pdu::codecs::rfx::EntropyAlgorithm;
use ironrdp_pdu::codecs::rfx::progressive::ComponentCodecQuant;

use crate::dwt_extrapolate::BandInfo;
use crate::rlgr::RlgrError;
use crate::srl;

/// Number of DWT coefficients per component in a 64x64 tile.
pub const COEFFICIENTS_PER_COMPONENT: usize = 4096;

/// Number of subbands in a 3-level DWT decomposition.
pub const NUM_BANDS: usize = 10;

/// DAS (Delta-Analysis State) values for tri-state sign tracking.
///
/// After the first pass, each coefficient position is classified:
/// - `SIGN_ZERO`: coefficient was zero (eligible for SRL upgrade)
/// - `SIGN_POSITIVE`: coefficient was positive (eligible for raw upgrade)
/// - `SIGN_NEGATIVE`: coefficient was negative (eligible for raw upgrade)
pub const SIGN_ZERO: i8 = 0;
pub const SIGN_POSITIVE: i8 = 1;
pub const SIGN_NEGATIVE: i8 = -1;

// ---------------------------------------------------------------------------
// First-pass decode (TILE_SIMPLE / TILE_FIRST)
// ---------------------------------------------------------------------------

/// Decode a first-pass component from RLGR1-encoded data.
///
/// Performs: RLGR1 decode -> base dequantization -> progressive dequantization
/// -> LL3 delta decode -> sign capture.
///
/// # Arguments
/// - `data`: RLGR1-encoded coefficient stream
/// - `base_quant`: base quantization values (from region quant table, `ComponentCodecQuant` format)
/// - `prog_quant`: progressive quantization BitPos values for this quality level
/// - `use_reduce_extrapolate`: whether to use asymmetric band sizes
/// - `coefficients`: output buffer for decoded coefficients (4096 i16)
/// - `sign`: output buffer for DAS sign state (4096 i8)
///
/// # Panics
///
/// Panics if `coefficients` or `sign` has fewer than 4096 elements.
///
/// # Errors
/// Returns `RlgrError` if RLGR decoding fails.
pub fn decode_first_pass(
    data: &[u8],
    base_quant: &ComponentCodecQuant,
    prog_quant: &ComponentCodecQuant,
    use_reduce_extrapolate: bool,
    coefficients: &mut [i16],
    sign: &mut [i8],
) -> Result<(), RlgrError> {
    assert!(coefficients.len() >= COEFFICIENTS_PER_COMPONENT);
    assert!(sign.len() >= COEFFICIENTS_PER_COMPONENT);

    // Step 1: RLGR1 decode into coefficient buffer
    crate::rlgr::decode(EntropyAlgorithm::Rlgr1, data, coefficients)?;

    // Step 2: LL3 differential decoding (reverse delta encoding on last subband)
    crate::subband_reconstruction::decode(&mut coefficients[ll3_offset(use_reduce_extrapolate)..]);

    // Steps 3 and 4, as ONE shift per band.
    //
    // mdrdp patch: this was two passes — `<< (quant - 1)` then `<< bitPos` — with
    // DIFFERENT overflow behaviour in each. The base pass wrapped (a plain `i16 <<`)
    // while the progressive pass saturated (`clamp_i16`), and it also shifted the
    // magnitude and reapplied the sign rather than shifting the value.
    //
    // FreeRDP applies a single shift of `quant + prog - 1` per band and wraps:
    // `(INT16)(((UINT32)val << sh) & 0xFFFF)` (general_lShiftC_16s, prim_shift.c). One
    // wrapping shift and two shifts with a clamp between them only agree while nothing
    // overflows, so the two implementations diverged on exactly those tiles whose
    // coefficients are large — which is why a single tile could render wrong while every
    // other tile on screen was right.
    dequantize_bands(coefficients, base_quant, prog_quant, use_reduce_extrapolate);

    // Step 5: Capture sign state for DAS
    capture_sign(coefficients, sign);

    Ok(())
}

/// Decode an upgrade-pass component from SRL and raw data streams.
///
/// For each coefficient position:
/// - DAS = 0 (zero): decode from SRL stream, update DAS if non-zero
/// - DAS != 0 (non-zero): decode raw magnitude bits, accumulate
///
/// # Arguments
/// - `srl_data`: SRL-encoded stream for zero-DAS positions
/// - `raw_data`: raw bit stream for non-zero-DAS positions
/// - `prev_prog_quant`: BitPos values from previous quality level
/// - `curr_prog_quant`: BitPos values for this quality level
/// - `use_reduce_extrapolate`: whether to use asymmetric band sizes
/// - `coefficients`: coefficient buffer to accumulate into (modified in-place)
/// - `sign`: DAS sign buffer (modified in-place when zeros become non-zero)
///
/// # Panics
///
/// Panics if `coefficients` or `sign` has fewer than 4096 elements.
pub fn decode_upgrade_pass(
    srl_data: &[u8],
    raw_data: &[u8],
    base_quant: &ComponentCodecQuant,
    prev_prog_quant: &ComponentCodecQuant,
    curr_prog_quant: &ComponentCodecQuant,
    use_reduce_extrapolate: bool,
    coefficients: &mut [i16],
    sign: &mut [i8],
) {
    assert!(coefficients.len() >= COEFFICIENTS_PER_COMPONENT);
    assert!(sign.len() >= COEFFICIENTS_PER_COMPONENT);

    let bands = get_band_layout(use_reduce_extrapolate);

    // mdrdp patch: ONE raw stream and ONE SRL reader for the whole component.
    //
    // Both streams run continuously across all ten bands — FreeRDP attaches them once in
    // progressive_rfx_upgrade_component and lets every band consume from where the last
    // one stopped. Re-creating them per band, as this did, makes every band after HL1
    // re-read HL1's bits, so nine tenths of a refinement pass decodes garbage.
    let mut raw = RawBitReader::new(raw_data);
    let mut srl = SrlReader::new(srl_data);

    for (band_idx, band) in bands.iter().enumerate() {
        // mdrdp patch: bit positions are base quant + progressive quant.
        //
        // The shift applied to a refinement is (bitPos - 1), the SAME total the first
        // pass applies via dequantize_component_ccq + progressive_dequantize. Shifting by
        // the progressive term alone left every refinement 2^(base-1) times too small —
        // typically 32x — so upgrade passes contributed essentially nothing and the image
        // stayed at first-pass quality.
        let base = u32::from(base_quant.for_band(band_idx));
        let prev_bit_pos = base + u32::from(prev_prog_quant.for_band(band_idx));
        let curr_bit_pos = base + u32::from(curr_prog_quant.for_band(band_idx));

        let num_bits = prev_bit_pos.saturating_sub(curr_bit_pos);
        if num_bits == 0 {
            continue;
        }
        let shift = curr_bit_pos.saturating_sub(1);

        // LL3 is the last band, and is NOT sign-coded: FreeRDP clears `nonLL` for it and
        // reads every coefficient from the raw stream, never consulting the sign array or
        // touching the SRL stream. Routing it through SRL desynchronises both streams for
        // everything that follows.
        let non_ll = band_idx != bands.len() - 1;

        for i in 0..band.count() {
            let idx = band.offset + i;

            let input: i32 = if !non_ll {
                i32::from(raw.read_bits(num_bits) as i32 as i16)
            } else if sign[idx] == SIGN_POSITIVE {
                i32::from(raw.read_bits(num_bits) as i32 as i16)
            } else if sign[idx] == SIGN_NEGATIVE {
                -i32::from(raw.read_bits(num_bits) as i32 as i16)
            } else {
                let value = srl.read(num_bits);
                if value > 0 {
                    sign[idx] = SIGN_POSITIVE;
                } else if value < 0 {
                    sign[idx] = SIGN_NEGATIVE;
                }
                i32::from(value)
            };

            let val = input << shift;
            coefficients[idx] = clamp_i16(i32::from(coefficients[idx]) + val);
        }
    }
}

/// SRL reader for progressive upgrade passes.
///
/// mdrdp patch: replaces a Golomb-Rice quotient/remainder decoder that did not match the
/// wire format. Ported from FreeRDP's `progressive_rfx_srl_read`
/// (libfreerdp/codec/progressive.c). The differences that mattered:
///
/// * `kp` starts at **8**, not 0, so the first symbol uses k = 1;
/// * there is a `mode` latch — after an escape-signalled short run the next symbol is
///   unary, rather than another zero-run bit;
/// * magnitudes are a **bounded unary** count (start at 1, stop at `(1 << numBits) - 1`),
///   not a quotient plus remainder bits;
/// * the state and the bitstream persist across every band of a component.
pub struct SrlReader<'a> {
    bits: RawBitReader<'a>,
    kp: u32,
    mode: u32,
    nz: i32,
}

impl<'a> SrlReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            bits: RawBitReader::new(data),
            kp: 8,
            mode: 0,
            nz: 0,
        }
    }

    pub fn read(&mut self, num_bits: u32) -> i16 {
        if self.nz > 0 {
            self.nz -= 1;
            return 0;
        }

        let k = self.kp / 8;

        if self.mode == 0 {
            if !self.bits.read_bit() {
                // '0': a full run of (1 << k) zeros.
                self.nz = 1i32 << k;
                self.kp = (self.kp + 4).min(80);
                self.nz -= 1;
                return 0;
            }
            // '1': a short run, its length in the next k bits, then unary next.
            self.nz = 0;
            self.mode = 1;
            if k > 0 {
                self.nz = self.bits.read_bits(k) as i32;
            }
            if self.nz != 0 {
                self.nz -= 1;
                return 0;
            }
        }

        self.mode = 0;
        let negative = self.bits.read_bit();
        self.kp = self.kp.saturating_sub(6);

        if num_bits == 1 {
            return if negative { -1 } else { 1 };
        }

        let mut mag: u32 = 1;
        let max = (1u32 << num_bits) - 1;
        while mag < max {
            if self.bits.read_bit() {
                break;
            }
            mag += 1;
        }
        let mag = i16::try_from(mag).unwrap_or(i16::MAX);
        if negative { -mag } else { mag }
    }
}

// ---------------------------------------------------------------------------
// Progressive (de)quantization
// ---------------------------------------------------------------------------

/// Apply progressive dequantization: left-shift each band by its BitPos value.
///
/// For non-LL3 bands, this shifts the absolute value (preserving sign).
/// For LL3, this is a simple left shift (floor toward negative infinity).
#[allow(dead_code)] // superseded by dequantize_bands; kept for the encoder path
fn progressive_dequantize(coefficients: &mut [i16], prog_quant: &ComponentCodecQuant, use_reduce_extrapolate: bool) {
    let bands = get_band_layout(use_reduce_extrapolate);

    for (band_idx, band) in bands.iter().enumerate() {
        let bit_pos = prog_quant.for_band(band_idx);
        if bit_pos == 0 {
            continue;
        }

        let is_ll3 = band_idx == 9;
        let start = band.offset;
        let end = start + band.count();

        if is_ll3 {
            // LL3: simple left shift (floor toward negative infinity)
            for coeff in &mut coefficients[start..end] {
                *coeff = clamp_i16(i32::from(*coeff) << i32::from(bit_pos));
            }
        } else {
            // Other bands: shift absolute value, preserve sign
            for coeff in &mut coefficients[start..end] {
                let val = i32::from(*coeff);
                if val >= 0 {
                    *coeff = clamp_i16(val << i32::from(bit_pos));
                } else {
                    *coeff = clamp_i16(-((-val) << i32::from(bit_pos)));
                }
            }
        }
    }
}

/// Apply progressive quantization: right-shift each band by its BitPos value.
///
/// Inverse of `progressive_dequantize`.
pub fn progressive_quantize(coefficients: &mut [i16], prog_quant: &ComponentCodecQuant, use_reduce_extrapolate: bool) {
    let bands = get_band_layout(use_reduce_extrapolate);

    for (band_idx, band) in bands.iter().enumerate() {
        let bit_pos = prog_quant.for_band(band_idx);
        if bit_pos == 0 {
            continue;
        }

        let is_ll3 = band_idx == 9;
        let start = band.offset;
        let end = start + band.count();

        if is_ll3 {
            // LL3: floor division (right shift)
            for coeff in &mut coefficients[start..end] {
                *coeff >>= bit_pos;
            }
        } else {
            // Other bands: truncation toward zero
            for coeff in &mut coefficients[start..end] {
                let val = i32::from(*coeff);
                if val >= 0 {
                    *coeff = clamp_i16(val >> i32::from(bit_pos));
                } else {
                    *coeff = clamp_i16(-((-val) >> i32::from(bit_pos)));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Server-side encode pipeline
// ---------------------------------------------------------------------------

/// Encode a first-pass component from spatial-domain coefficients.
///
/// Pipeline: forward DWT -> base quantization -> progressive quantization
/// -> LL3 delta encode -> RLGR1 encode.
///
/// Returns the number of bytes written to `output`.
///
/// # Arguments
/// - `coefficients`: spatial-domain coefficients (4096 i16, modified in-place)
/// - `output`: output buffer for RLGR1-encoded data
/// - `base_quant`: base quantization values
/// - `prog_quant`: progressive quantization BitPos values for this quality level
/// - `use_reduce_extrapolate`: DWT mode flag
///
/// # Panics
///
/// Panics if `coefficients` has fewer than 4096 elements.
///
/// # Errors
/// Returns `RlgrError` if RLGR encoding fails.
pub fn encode_first_pass(
    coefficients: &mut [i16],
    output: &mut [u8],
    base_quant: &ComponentCodecQuant,
    prog_quant: &ComponentCodecQuant,
    use_reduce_extrapolate: bool,
) -> Result<usize, RlgrError> {
    assert!(coefficients.len() >= COEFFICIENTS_PER_COMPONENT);

    let mut temp = [0i16; COEFFICIENTS_PER_COMPONENT];

    // Step 1: Forward DWT
    if use_reduce_extrapolate {
        crate::dwt_extrapolate::encode(coefficients, &mut temp);
    } else {
        crate::dwt::encode(coefficients, &mut temp);
    }

    // Step 2: Base quantization (right-shift by quant - 1)
    quantize_component_ccq(coefficients, base_quant, use_reduce_extrapolate);

    // Step 3: Progressive quantization (right-shift by BitPos)
    progressive_quantize(coefficients, prog_quant, use_reduce_extrapolate);

    // Step 4: LL3 delta encoding
    crate::subband_reconstruction::encode(&mut coefficients[ll3_offset(use_reduce_extrapolate)..]);

    // Step 5: RLGR1 entropy encode
    crate::rlgr::encode(EntropyAlgorithm::Rlgr1, coefficients, output)
}

/// Base quantization using `ComponentCodecQuant` (progressive format).
///
/// Each band is right-shifted by `(quant_value - 1)`. Inverse of `dequantize_component_ccq`.
fn quantize_component_ccq(coefficients: &mut [i16], quant: &ComponentCodecQuant, use_reduce_extrapolate: bool) {
    let bands = get_band_layout(use_reduce_extrapolate);

    for (band_idx, band) in bands.iter().enumerate() {
        let q = quant.for_band(band_idx);
        let factor = q.saturating_sub(1);
        if factor > 0 {
            let start = band.offset;
            let end = start + band.count();
            for coeff in &mut coefficients[start..end] {
                // Truncation toward zero (same as classic quantization::encode)
                let val = i32::from(*coeff);
                if val >= 0 {
                    *coeff = clamp_i16(val >> i32::from(factor));
                } else {
                    *coeff = clamp_i16(-((-val) >> i32::from(factor)));
                }
            }
        }
    }
}

/// Compute the upgrade-pass data for a single component.
///
/// Given the previous and current progressive quantization, produces
/// SRL-encoded data (for zero-DAS positions) and raw bit data (for
/// non-zero DAS positions) representing the refinement.
///
/// # Arguments
/// - `coefficients`: current full-resolution DWT coefficients for this component
/// - `prev_coefficients`: coefficients as reconstructed from the previous pass
/// - `prev_prog_quant`: BitPos values from the previous pass
/// - `curr_prog_quant`: BitPos values for this upgrade pass
/// - `sign`: DAS sign array from the previous pass
/// - `use_reduce_extrapolate`: DWT mode flag
///
/// # Returns
/// A tuple of `(srl_data, raw_data)` byte vectors.
///
/// # Wire-format invariants (MS-RDPRFX 3.1.8.1.7.2)
///
/// The non-zero-DAS raw-magnitude path uses `saturating_sub` to compute
/// `raw_mag = curr_q - prev_q`. Upgrade passes are *monotonic refinements*:
/// the encoder only adds magnitude bits, never subtracts. The decoder's
/// counterpart accumulates raw_mag onto the previously-decoded coefficient
/// with the DAS-determined sign (`+=` for SIGN_POSITIVE / LL3, `-=` for
/// SIGN_NEGATIVE), so a hypothetical signed delta would have no place in
/// the wire format. Switching this to a signed-delta encoding would break
/// wire compatibility with mstsc/FreeRDP — do not "fix" the saturating_sub.
///
/// The zero-DAS SRL path uses `clamp_i16(curr_shifted - prev_shifted)`. SRL
/// stream values are i16 by wire-format definition, so wider precision is
/// not available without a spec extension. The clamp is the wire-format
/// boundary, not a precision compromise.
pub fn encode_upgrade_pass(
    coefficients: &[i16],
    prev_coefficients: &[i16],
    prev_prog_quant: &ComponentCodecQuant,
    curr_prog_quant: &ComponentCodecQuant,
    sign: &[i8],
    use_reduce_extrapolate: bool,
) -> (Vec<u8>, Vec<u8>) {
    let bands = get_band_layout(use_reduce_extrapolate);
    let mut all_srl_values = Vec::new();
    let mut raw_writer = RawBitWriter::new();

    for (band_idx, band) in bands.iter().enumerate() {
        let prev_bit_pos = prev_prog_quant.for_band(band_idx);
        let curr_bit_pos = curr_prog_quant.for_band(band_idx);

        let num_bits = prev_bit_pos.saturating_sub(curr_bit_pos);
        if num_bits == 0 {
            continue;
        }

        let mut band_srl_values = Vec::new();

        for i in 0..band.count() {
            let coeff_idx = band.offset + i;

            if sign[coeff_idx] == SIGN_ZERO {
                // Zero-DAS: compute the refined value and encode via SRL
                let curr_shifted = i32::from(coefficients[coeff_idx]) >> i32::from(curr_bit_pos);
                let prev_shifted = i32::from(prev_coefficients[coeff_idx]) >> i32::from(curr_bit_pos);
                let delta = clamp_i16(curr_shifted - prev_shifted);
                band_srl_values.push(delta);
            } else {
                // Non-zero DAS: compute raw magnitude bits
                let curr_abs = i32::from(coefficients[coeff_idx]).unsigned_abs();
                let prev_abs = i32::from(prev_coefficients[coeff_idx]).unsigned_abs();

                let curr_q = curr_abs >> u32::from(curr_bit_pos);
                let prev_q = prev_abs >> u32::from(curr_bit_pos);
                let raw_mag = curr_q.saturating_sub(prev_q);

                raw_writer.write_bits(raw_mag, u32::from(num_bits));
            }
        }

        // Encode SRL values for this band
        let srl_encoded = srl::encode_srl(&band_srl_values, num_bits);
        all_srl_values.extend_from_slice(&srl_encoded);
    }

    let raw_data = raw_writer.finish();
    (all_srl_values, raw_data)
}

/// Encode RGBA pixels to spatial-domain i16 coefficients (RGB to YCbCr).
///
/// Performs ITU-R BT.601 RGB-to-YCbCr conversion on a 64x64 pixel tile.
/// Output is 3 buffers of 4096 i16 coefficients (Y, Cb, Cr) in tile order.
///
/// # Panics
///
/// Panics if `pixels` has fewer than 64 * 64 * 4 = 16384 bytes.
#[expect(clippy::similar_names)]
pub fn rgba_to_ycbcr(pixels: &[u8], y_out: &mut [i16], cb_out: &mut [i16], cr_out: &mut [i16]) {
    assert!(pixels.len() >= 64 * 64 * 4);
    assert!(y_out.len() >= COEFFICIENTS_PER_COMPONENT);
    assert!(cb_out.len() >= COEFFICIENTS_PER_COMPONENT);
    assert!(cr_out.len() >= COEFFICIENTS_PER_COMPONENT);

    for i in 0..64 * 64 {
        let off = i * 4;
        let r = i32::from(pixels[off]);
        let g = i32::from(pixels[off + 1]);
        let b = i32::from(pixels[off + 2]);

        // ITU-R BT.601: Y = 0.299R + 0.587G + 0.114B
        //               Cb = -0.169R - 0.331G + 0.500B
        //               Cr = 0.500R - 0.419G - 0.081B
        // Fixed-point with 16-bit precision
        let y = ((19595 * r + 38470 * g + 7471 * b + 32768) >> 16) - 128;
        let cb = (-11059 * r - 21709 * g + 32768 * b + 32768) >> 16;
        let cr = (32768 * r - 27439 * g - 5329 * b + 32768) >> 16;

        y_out[i] = clamp_i16(y);
        cb_out[i] = clamp_i16(cb);
        cr_out[i] = clamp_i16(cr);
    }
}

/// Base dequantization using `ComponentCodecQuant` (progressive-format quantization).
///
/// Each band is shifted left by `(quant_value - 1)`. Uses `for_band()` to map
/// band indices to quant values, which handles the progressive nibble ordering.
#[allow(dead_code)] // superseded by dequantize_bands; kept for the encoder path
fn dequantize_component_ccq(coefficients: &mut [i16], quant: &ComponentCodecQuant, use_reduce_extrapolate: bool) {
    let bands = get_band_layout(use_reduce_extrapolate);

    for (band_idx, band) in bands.iter().enumerate() {
        let q = quant.for_band(band_idx);
        let factor = i16::from(q).saturating_sub(1);
        if factor > 0 {
            let start = band.offset;
            let end = start + band.count();
            for coeff in &mut coefficients[start..end] {
                *coeff <<= factor;
            }
        }
    }
}

/// Apply the combined base + progressive dequantisation shift, one band at a time.
///
/// Mirrors FreeRDP: `shift = quant + prog - 1` per band, applied with a wrapping 16-bit
/// left shift. A shift of 16 or more is refused there; we skip the band, which leaves the
/// coefficients untouched rather than zeroing a band on a malformed quant table.
fn dequantize_bands(
    coefficients: &mut [i16],
    base_quant: &ComponentCodecQuant,
    prog_quant: &ComponentCodecQuant,
    use_reduce_extrapolate: bool,
) {
    let bands = get_band_layout(use_reduce_extrapolate);

    for (band_idx, band) in bands.iter().enumerate() {
        let shift = u32::from(base_quant.for_band(band_idx))
            .saturating_add(u32::from(prog_quant.for_band(band_idx)))
            .saturating_sub(1);
        if shift == 0 || shift >= 16 {
            continue;
        }
        let start = band.offset;
        let end = start + band.count();
        for coeff in &mut coefficients[start..end] {
            // Wrapping, keeping the low 16 bits — FreeRDP's `& 0xFFFF`.
            *coeff = ((*coeff as u16) << shift) as i16;
        }
    }
}

// ---------------------------------------------------------------------------
// Sign capture
// ---------------------------------------------------------------------------

/// Capture the tri-state sign of each coefficient into the DAS array.
fn capture_sign(coefficients: &[i16], sign: &mut [i8]) {
    for (s, &c) in sign.iter_mut().zip(coefficients.iter()) {
        *s = match c.cmp(&0) {
            core::cmp::Ordering::Greater => SIGN_POSITIVE,
            core::cmp::Ordering::Less => SIGN_NEGATIVE,
            core::cmp::Ordering::Equal => SIGN_ZERO,
        };
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Get the band layout for the current DWT mode.
fn get_band_layout(use_reduce_extrapolate: bool) -> [BandInfo; NUM_BANDS] {
    if use_reduce_extrapolate {
        crate::dwt_extrapolate::band_layout()
    } else {
        standard_band_layout()
    }
}

/// Standard (non-extrapolate) band layout for a 64x64 tile.
/// Band sizes: 1024 each for level 1, 256 each for level 2, 64 each for level 3.
fn standard_band_layout() -> [BandInfo; NUM_BANDS] {
    let mut off = 0;
    let mut b = |w: usize, h: usize| {
        let info = BandInfo {
            width: w,
            height: h,
            offset: off,
        };
        off += w * h;
        info
    };

    [
        b(32, 32), // HL1: 1024
        b(32, 32), // LH1: 1024
        b(32, 32), // HH1: 1024
        b(16, 16), // HL2: 256
        b(16, 16), // LH2: 256
        b(16, 16), // HH2: 256
        b(8, 8),   // HL3: 64
        b(8, 8),   // LH3: 64
        b(8, 8),   // HH3: 64
        b(8, 8),   // LL3: 64
    ]
}

/// Starting offset of the LL3 subband for delta decoding.
fn ll3_offset(use_reduce_extrapolate: bool) -> usize {
    if use_reduce_extrapolate {
        4015 // reduce-extrapolate: 9x9 = 81 coefficients at offset 4015
    } else {
        4032 // standard: 8x8 = 64 coefficients at offset 4032
    }
}

/// Count zero-DAS positions within a band.
#[allow(dead_code)] // superseded by the streaming SrlReader
fn band_zero_count(sign: &[i8], band: &BandInfo) -> usize {
    let start = band.offset;
    let end = start + band.count();
    sign[start..end].iter().filter(|&&s| s == SIGN_ZERO).count()
}

/// Clamp i32 to u8 range (0-255).
#[expect(
    clippy::as_conversions,
    clippy::cast_sign_loss,
    reason = "value is clamped to 0..255 before cast"
)]
fn clamp_u8(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

/// Clamp i32 to i16 range.
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "value is clamped to i16 range before cast"
)]
fn clamp_i16(value: i32) -> i16 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

// ---------------------------------------------------------------------------
// Raw bit I/O for upgrade pass
// ---------------------------------------------------------------------------

/// Writes raw magnitude bits MSB-first to a byte stream.
///
/// Symmetric counterpart of [`RawBitReader`]. Callers are expected to pass
/// `count <= 32` to [`write_bits`](Self::write_bits); the upgrade-pass call
/// site bounds `count` by `prev_bit_pos - curr_bit_pos` which is at most a
/// few bits in practice. `count > 32` reads beyond `u32` width in the shift
/// expression, which is wrap-on-release / panic-on-debug — caller responsibility.
struct RawBitWriter {
    bytes: Vec<u8>,
    current: u8,
    bit_count: u8,
}

impl RawBitWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            current: 0,
            bit_count: 0,
        }
    }

    fn write_bit(&mut self, bit: bool) {
        self.current = (self.current << 1) | u8::from(bit);
        self.bit_count += 1;
        if self.bit_count >= 8 {
            self.bytes.push(self.current);
            self.current = 0;
            self.bit_count = 0;
        }
    }

    /// Write the low `count` bits of `value`, MSB-first. Caller must ensure
    /// `count <= 32` (see type-level docs).
    fn write_bits(&mut self, value: u32, count: u32) {
        debug_assert!(count <= 32, "RawBitWriter::write_bits count must be <= 32");
        for i in (0..count).rev() {
            self.write_bit((value >> i) & 1 != 0);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bit_count > 0 {
            self.current <<= 8 - self.bit_count;
            self.bytes.push(self.current);
        }
        self.bytes
    }
}

/// Reads raw magnitude bits MSB-first from a byte stream.
///
/// Past-end-of-stream reads return zero bits rather than an error: a
/// truncated `raw_data` produces zero coefficient magnitudes (no-op upgrade)
/// for the missing positions, matching the FreeRDP reference implementation's
/// tolerance for short truncation in this exact upgrade path.
///
/// Callers are expected to pass `count <= 32` to [`read_bits`](Self::read_bits);
/// the upgrade-pass call site bounds `count` by `prev_bit_pos - curr_bit_pos`
/// which is at most a few bits in practice.
struct RawBitReader<'a> {
    data: &'a [u8],
    byte_idx: usize,
    bit_idx: u8,
}

impl<'a> RawBitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_idx: 0,
            bit_idx: 0,
        }
    }

    /// Read `count` bits MSB-first into a `u32`. Bits past the end of the
    /// underlying stream read as zero.
    fn read_bits(&mut self, count: u32) -> u32 {
        let mut value = 0u32;
        for _ in 0..count {
            value = (value << 1) | u32::from(self.read_bit());
        }
        value
    }

    /// Read one bit. Returns `false` past end-of-stream by design (see
    /// type-level docs).
    fn read_bit(&mut self) -> bool {
        if self.byte_idx >= self.data.len() {
            return false;
        }
        let bit = (self.data[self.byte_idx] >> (7 - self.bit_idx)) & 1 != 0;
        self.bit_idx += 1;
        if self.bit_idx >= 8 {
            self.bit_idx = 0;
            self.byte_idx += 1;
        }
        bit
    }
}

// ---------------------------------------------------------------------------
// Tile state machine
// ---------------------------------------------------------------------------

/// Per-tile progressive state: coefficients, signs, and quality tracking.
///
/// Each tile in a progressive surface maintains this state across decode
/// passes. The first pass (TILE_SIMPLE or TILE_FIRST) initializes the
/// coefficients and signs; subsequent upgrade passes (TILE_UPGRADE)
/// accumulate refinement data.
///
/// Memory per tile: ~37 KB (24 KB coefficients + 12 KB signs + metadata).
pub struct TileState {
    /// Accumulated DWT coefficients per component (Y, Cb, Cr).
    pub coefficients: [[i16; COEFFICIENTS_PER_COMPONENT]; 3],
    /// Tri-state sign tracking per component (DAS array).
    pub sign: [[i8; COEFFICIENTS_PER_COMPONENT]; 3],
    /// Progressive quantization BitPos from the last applied pass.
    pub prog_quant: [ComponentCodecQuant; 3],
    /// Base quantization indices (Y, Cb, Cr) into the region's quant table.
    pub quant_idx: [u8; 3],
    /// Progressive pass counter (0 = no data, 1 = first pass complete, 2+ = upgrade).
    pub pass: u16,
    /// Whether the tile was encoded as a difference tile.
    pub is_difference: bool,
    /// Last progressive quality byte (0xFF = full quality).
    pub quality: u8,
    /// Whether reduce-extrapolate DWT is used for this tile's context.
    pub use_reduce_extrapolate: bool,
}

impl TileState {
    /// Create a new tile with zeroed state.
    pub fn new() -> Self {
        Self {
            coefficients: [[0; COEFFICIENTS_PER_COMPONENT]; 3],
            sign: [[0; COEFFICIENTS_PER_COMPONENT]; 3],
            prog_quant: [ComponentCodecQuant::LOSSLESS; 3],
            quant_idx: [0; 3],
            pass: 0,
            is_difference: false,
            quality: 0,
            use_reduce_extrapolate: false,
        }
    }

    /// Decode a first-pass tile (TILE_SIMPLE or TILE_FIRST).
    ///
    /// Resets this tile's state and decodes three components from RLGR1 data.
    /// After this call, `coefficients` hold DWT-domain values ready for
    /// inverse DWT + color conversion.
    ///
    /// # Arguments
    /// - `component_data`: RLGR1-encoded data for [Y, Cb, Cr]
    /// - `base_quants`: base quantization values for [Y, Cb, Cr]
    /// - `prog_quants`: progressive quantization for [Y, Cb, Cr]
    /// - `quality`: progressive quality byte
    /// - `use_reduce_extrapolate`: DWT mode flag
    ///
    /// # Errors
    /// Returns `RlgrError` if any component's RLGR decode fails.
    pub fn decode_first(
        &mut self,
        component_data: [&[u8]; 3],
        base_quants: [&ComponentCodecQuant; 3],
        prog_quants: [ComponentCodecQuant; 3],
        quant_idx: [u8; 3],
        quality: u8,
        use_reduce_extrapolate: bool,
        is_difference: bool,
    ) -> Result<(), RlgrError> {
        // mdrdp patch: honour RFX_TILE_DIFFERENCE.
        //
        // A tile flagged as a difference carries a DELTA against the coefficients already
        // held for it, not a replacement. FreeRDP adds the two — `add_16s_inplace(buffer,
        // current, ...)` inside progressive_rfx_dwt_2d_decode when coeffDiff is set — and
        // we overwrote instead, so such a tile discarded everything the earlier passes had
        // built and rendered visibly offset from its neighbours.
        //
        // Only a few tiles per frame carry the flag, which is why this surfaced as exactly
        // ONE wrong tile on an otherwise perfect screen: in the captured frame, tile
        // (11,0) had flags=0x01 while tile (10,0) had 0x00.
        let previous = is_difference.then_some(self.coefficients);

        self.pass = 1;
        self.quality = quality;
        self.quant_idx = quant_idx;
        self.use_reduce_extrapolate = use_reduce_extrapolate;
        self.is_difference = is_difference;
        self.prog_quant = prog_quants;

        for c in 0..3 {
            decode_first_pass(
                component_data[c],
                base_quants[c],
                &prog_quants[c],
                use_reduce_extrapolate,
                &mut self.coefficients[c],
                &mut self.sign[c],
            )?;
        }

        if let Some(previous) = previous {
            for c in 0..3 {
                for (coeff, old) in self.coefficients[c].iter_mut().zip(previous[c].iter()) {
                    *coeff = coeff.wrapping_add(*old);
                }
            }
        }

        Ok(())
    }

    /// Decode an upgrade-pass tile (TILE_UPGRADE).
    ///
    /// Accumulates refinement data into existing coefficients.
    ///
    /// # Arguments
    /// - `srl_data`: SRL-encoded streams for [Y, Cb, Cr]
    /// - `raw_data`: raw bit streams for [Y, Cb, Cr]
    /// - `prog_quants`: progressive quantization for this upgrade level
    /// - `quality`: progressive quality byte for this pass
    pub fn decode_upgrade(
        &mut self,
        srl_data: [&[u8]; 3],
        raw_data: [&[u8]; 3],
        base_quants: [ComponentCodecQuant; 3],
        prog_quants: [ComponentCodecQuant; 3],
        quality: u8,
    ) {
        let prev_prog_quant = self.prog_quant;

        for c in 0..3 {
            decode_upgrade_pass(
                srl_data[c],
                raw_data[c],
                &base_quants[c],
                &prev_prog_quant[c],
                &prog_quants[c],
                self.use_reduce_extrapolate,
                &mut self.coefficients[c],
                &mut self.sign[c],
            );
        }

        self.prog_quant = prog_quants;
        self.quality = quality;
        self.pass = self.pass.saturating_add(1);
    }

    /// Reconstruct the tile to spatial domain and write RGBA pixels.
    ///
    /// Applies inverse DWT to each component, then YCbCr-to-RGB color
    /// conversion. The pixel buffer receives 64x64 RGBA pixels (16384 bytes).
    ///
    /// # Panics
    ///
    /// Panics if `pixels` has fewer than 64 * 64 * 4 = 16384 bytes.
    #[expect(clippy::similar_names, reason = "y/cb/cr are standard YCbCr component names")]
    pub fn reconstruct_to_rgba(&self, pixels: &mut [u8]) {
        assert!(pixels.len() >= 64 * 64 * 4, "pixel buffer too small");

        // Copy coefficients to scratch buffers for in-place DWT
        let mut y_buf = self.coefficients[0];
        let mut cb_buf = self.coefficients[1];
        let mut cr_buf = self.coefficients[2];
        let mut temp = [0i16; COEFFICIENTS_PER_COMPONENT];

        // Inverse DWT
        if self.use_reduce_extrapolate {
            crate::dwt_extrapolate::decode(&mut y_buf, &mut temp);
            crate::dwt_extrapolate::decode(&mut cb_buf, &mut temp);
            crate::dwt_extrapolate::decode(&mut cr_buf, &mut temp);
        } else {
            let mut dwt_temp = [0i16; COEFFICIENTS_PER_COMPONENT];
            crate::dwt::decode(&mut y_buf, &mut dwt_temp);
            crate::dwt::decode(&mut cb_buf, &mut dwt_temp);
            crate::dwt::decode(&mut cr_buf, &mut dwt_temp);
        }

        // YCbCr to RGBA conversion.
        //
        // mdrdp patch: the DWT output is NOT in 8-bit range.
        //
        // RFX carries luma scaled by 32 with a DC offset of 4096 (= 128 * 32), so the
        // conversion has to add 4096 and shift the result right by 5 — not add 128 and
        // use the value directly. Without the scale every pixel overflows and clips, and
        // the tile reconstructs as a flat, saturated block: the average colour survives
        // (the DC term still lands in range) while all detail is clipped away. That is
        // exactly what a photographic wallpaper looked like — a flat blue field where the
        // real image is a smooth gradient.
        //
        // Matches FreeRDP's general_yCbCrToRGB_16s8u_P3AC4R (libfreerdp/primitives/
        // prim_colors.c), which decodes this same stream correctly:
        //     Y = (y + 4096) << 16;  R = ((CrR + Y) >> 16) >> 5;
        // The chroma constants are the same 2^16 fixed-point values it uses.
        for i in 0..64 * 64 {
            let (r, g, b) = rfx_ycbcr_to_rgb(y_buf[i], cb_buf[i], cr_buf[i]);
            let off = i * 4;
            pixels[off] = r;
            pixels[off + 1] = g;
            pixels[off + 2] = b;
            pixels[off + 3] = 0xFF;
        }
    }
}

impl Default for TileState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Surface tile grid
// ---------------------------------------------------------------------------

/// Grid of progressive tiles for a single surface.
///
/// Manages tile state for a surface identified by its codec context ID.
/// Tiles are lazily allocated on first access to avoid upfront memory
/// cost for surfaces that only partially receive progressive updates.
pub struct SurfaceTiles {
    /// Width of the surface in tiles (ceildiv of pixel width by 64).
    pub tiles_wide: u16,
    /// Height of the surface in tiles.
    pub tiles_high: u16,
    /// Whether the associated context uses reduce-extrapolate DWT.
    pub use_reduce_extrapolate: bool,
    /// Tile storage, indexed by `y_idx * tiles_wide + x_idx`.
    /// `None` entries haven't received any progressive data yet.
    pub tiles: Vec<Option<Box<TileState>>>,
}

impl SurfaceTiles {
    /// Create a new tile grid for the given surface dimensions.
    ///
    /// Returns [`ProgressiveDecodeError::SurfaceTooLarge`] if either axis
    /// exceeds [`MAX_SURFACE_DIM`]. The check rejects only inputs that exceed
    /// the MS-RDPEGFX 2.2.2.14 normative ceiling (32766 px), so every
    /// spec-conformant surface is accepted.
    pub fn new(
        width_pixels: u16,
        height_pixels: u16,
        use_reduce_extrapolate: bool,
    ) -> Result<Self, ProgressiveDecodeError> {
        if width_pixels > MAX_SURFACE_DIM || height_pixels > MAX_SURFACE_DIM {
            return Err(ProgressiveDecodeError::SurfaceTooLarge {
                width: width_pixels,
                height: height_pixels,
            });
        }

        let tiles_wide = width_pixels.div_ceil(64);
        let tiles_high = height_pixels.div_ceil(64);
        let count = usize::from(tiles_wide) * usize::from(tiles_high);

        Ok(Self {
            tiles_wide,
            tiles_high,
            use_reduce_extrapolate,
            tiles: core::iter::repeat_with(|| None).take(count).collect(),
        })
    }

    /// Get or create the tile at the given grid position.
    ///
    /// Returns `None` if the coordinates are out of bounds.
    pub fn get_or_create(&mut self, x_idx: u16, y_idx: u16) -> Option<&mut TileState> {
        let idx = self.tile_index(x_idx, y_idx)?;
        let tile = self.tiles[idx].get_or_insert_with(|| {
            let mut t = Box::new(TileState::new());
            t.use_reduce_extrapolate = self.use_reduce_extrapolate;
            t
        });
        Some(tile)
    }

    /// Get the tile at the given grid position, if it exists.
    pub fn get(&self, x_idx: u16, y_idx: u16) -> Option<&TileState> {
        let idx = self.tile_index(x_idx, y_idx)?;
        self.tiles[idx].as_deref()
    }

    /// Reset all tiles (e.g., on context reset or surface resize).
    pub fn reset(&mut self) {
        for tile in &mut self.tiles {
            *tile = None;
        }
    }

    fn tile_index(&self, x_idx: u16, y_idx: u16) -> Option<usize> {
        if x_idx >= self.tiles_wide || y_idx >= self.tiles_high {
            return None;
        }
        Some(usize::from(y_idx) * usize::from(self.tiles_wide) + usize::from(x_idx))
    }
}

// ---------------------------------------------------------------------------
// Progressive decoder (EGFX integration)
// ---------------------------------------------------------------------------

/// Decoded tile pixel data for compositing onto a surface.
pub struct DecodedTile {
    /// Tile grid X coordinate (tile column).
    pub x_idx: u16,
    /// Tile grid Y coordinate (tile row).
    pub y_idx: u16,
    /// RGBA pixel data (64x64 = 16384 bytes).
    pub pixels: Vec<u8>,
}

/// Convert one RFX YCbCr triple to 8-bit RGB.
///
/// mdrdp patch: extracted so the arithmetic can be pinned by a test, and corrected.
///
/// RFX luma is carried scaled by 32 with a DC offset of 4096 (= 128 * 32), so the
/// conversion adds 4096 and shifts the result right by 5. The previous code added 128 and
/// used the value directly, which overflows for anything but near-zero coefficients: the
/// DC term still landed in range while every detail coefficient clipped, so a photograph
/// reconstructed as a flat, saturated block of its own average colour.
///
/// Mirrors FreeRDP's `general_yCbCrToRGB_16s8u_P3AC4R`
/// (libfreerdp/primitives/prim_colors.c), which decodes this stream correctly:
///
/// ```text
/// Y = (y + 4096) << 16
/// R = ((Cr * 1.402525 * 2^16 + Y) >> 16) >> 5
/// G = ((Y - Cb * 0.343730 * 2^16 - Cr * 0.714401 * 2^16) >> 16) >> 5
/// B = ((Cb * 1.769905 * 2^16 + Y) >> 16) >> 5
/// ```
pub fn rfx_ycbcr_to_rgb(y: i16, cb: i16, cr: i16) -> (u8, u8, u8) {
    /// 1.402525 * 65536
    const CR_R: i32 = 91916;
    /// 0.714401 * 65536
    const CR_G: i32 = 46820;
    /// 0.343730 * 65536
    const CB_G: i32 = 22526;
    /// 1.769905 * 65536
    const CB_B: i32 = 115998;

    let y = (i32::from(y) + 4096) << 16;
    let cb = i32::from(cb);
    let cr = i32::from(cr);

    let r = ((cr * CR_R + y) >> 16) >> 5;
    let g = ((y - cb * CB_G - cr * CR_G) >> 16) >> 5;
    let b = ((cb * CB_B + y) >> 16) >> 5;

    (clamp_u8(r), clamp_u8(g), clamp_u8(b))
}

/// Per-axis cap on surface dimensions, in pixels.
///
/// Per MS-RDPEGFX 2.2.2.14 RDPGFX_RESET_GRAPHICS_PDU, the normative maximum
/// allowed width and height are 32766 pixels. We round up to 32768 so that
/// the cap accepts every spec-conformant surface while bounding the
/// per-surface tile-grid allocation: at the cap the backing
/// `Vec<Option<Box<TileState>>>` is 512 * 512 = 262144 slots * 8 bytes per
/// slot = 2 MiB of pointer storage per surface before any tile is populated.
pub const MAX_SURFACE_DIM: u16 = 32768;

/// Tile flag: the tile carries a delta against the coefficients already held, not a
/// replacement (MS-RDPRFX RFX_TILE_DIFFERENCE).
pub const RFX_TILE_DIFFERENCE: u8 = 0x01;

/// Error type for progressive decoding operations.
#[derive(Debug)]
pub enum ProgressiveDecodeError {
    /// PDU parsing failed.
    Pdu(ironrdp_core::DecodeError),
    /// RLGR decode failed within a tile.
    Rlgr(RlgrError),
    /// The progressive stream is missing a required block.
    MissingBlock(&'static str),
    /// Tile coordinates are out of bounds for the surface.
    TileOutOfBounds { x_idx: u16, y_idx: u16 },
    /// Region references a quant index beyond the table.
    InvalidQuantIndex { index: usize, table_len: usize },
    /// Surface dimensions exceed [`MAX_SURFACE_DIM`] per axis.
    SurfaceTooLarge { width: u16, height: u16 },
}

impl core::fmt::Display for ProgressiveDecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Pdu(e) => write!(f, "progressive PDU decode: {e}"),
            Self::Rlgr(e) => write!(f, "progressive RLGR decode: {e}"),
            Self::MissingBlock(name) => write!(f, "progressive stream missing {name} block"),
            Self::TileOutOfBounds { x_idx, y_idx } => {
                write!(f, "tile ({x_idx}, {y_idx}) out of surface bounds")
            }
            Self::InvalidQuantIndex { index, table_len } => {
                write!(f, "quant index {index} exceeds table length {table_len}")
            }
            Self::SurfaceTooLarge { width, height } => {
                write!(
                    f,
                    "surface dimensions {width}x{height} exceed per-axis cap of {MAX_SURFACE_DIM} \
                     (MS-RDPEGFX 2.2.2.14 normative ceiling: 32766)"
                )
            }
        }
    }
}

impl From<ironrdp_core::DecodeError> for ProgressiveDecodeError {
    fn from(e: ironrdp_core::DecodeError) -> Self {
        Self::Pdu(e)
    }
}

impl From<RlgrError> for ProgressiveDecodeError {
    fn from(e: RlgrError) -> Self {
        Self::Rlgr(e)
    }
}

/// Per-context progressive state, identified by codec_context_id.
struct ProgressiveContext {
    surface: SurfaceTiles,
}

/// High-level progressive bitmap decoder for EGFX WireToSurface2 processing.
///
/// Maintains per-context tile state across frames, keyed by `codec_context_id`.
/// Feed it progressive bitmap data from `WireToSurface2Pdu.bitmap_data` and
/// get back decoded RGBA tiles for compositing.
///
/// # Usage
///
/// ```ignore
/// let mut decoder = ProgressiveDecoder::new();
///
/// // On receiving WireToSurface2Pdu:
/// let tiles = decoder.decode_bitmap(
///     pdu.codec_context_id,
///     surface_width, surface_height,
///     &pdu.bitmap_data,
/// )?;
///
/// for tile in &tiles {
///     blit_tile(surface, tile.x_idx, tile.y_idx, &tile.pixels);
/// }
/// ```
pub struct ProgressiveDecoder {
    contexts: BTreeMap<u32, ProgressiveContext>,
}

impl ProgressiveDecoder {
    /// Create a new progressive decoder with no context state.
    pub fn new() -> Self {
        Self {
            contexts: BTreeMap::new(),
        }
    }

    /// Decode a progressive bitmap stream from WireToSurface2Pdu.
    ///
    /// Parses the progressive block stream, updates per-tile state, and
    /// returns RGBA pixel data for each tile that was updated.
    ///
    /// # Arguments
    /// - `codec_context_id`: context ID from the WireToSurface2Pdu
    /// - `surface_width`: surface width in pixels (for tile grid sizing)
    /// - `surface_height`: surface height in pixels
    /// - `bitmap_data`: raw progressive block stream from the PDU
    pub fn decode_bitmap(
        &mut self,
        codec_context_id: u32,
        surface_width: u16,
        surface_height: u16,
        bitmap_data: &[u8],
    ) -> Result<Vec<DecodedTile>, ProgressiveDecodeError> {
        use ironrdp_pdu::codecs::rfx::progressive::{ProgressiveBlock, decode_progressive_stream};

        let blocks = decode_progressive_stream(bitmap_data)?;

        // Extract the band-layout flag from the CONTEXT block when present.
        // Per MS-RDPEGFX 2.2.4.2 the SYNC + CONTEXT blocks establish a codec
        // context once (keyed by `codec_context_id`) and are not required to be
        // repeated on subsequent frames that reference the same context.
        // Real-world servers (xrdp, GNOME Remote Desktop) omit the CONTEXT
        // block on every frame after the first one that established the
        // context. The strict requirement rejected each of those frames with
        // `MissingBlock("CONTEXT")`, freezing the image on the coarse first
        // pass.
        //
        // Fall back to the value stored when the context was first created.
        // Only error when neither source is available, i.e. the very first
        // frame for a context arrived without a CONTEXT block.
        let use_reduce_extrapolate = match blocks.iter().find_map(|block| match block {
            ProgressiveBlock::Context(ctx) => Some(ctx.uses_reduce_extrapolate()),
            _ => None,
        }) {
            Some(v) => v,
            None => self
                .contexts
                .get(&codec_context_id)
                .map(|c| c.surface.use_reduce_extrapolate)
                .ok_or(ProgressiveDecodeError::MissingBlock("CONTEXT"))?,
        };

        // Get or create the context for this codec_context_id
        let context = match self.contexts.entry(codec_context_id) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let surface = SurfaceTiles::new(surface_width, surface_height, use_reduce_extrapolate)?;
                e.insert(ProgressiveContext { surface })
            }
        };

        // If surface dimensions changed, reallocate
        let expected_wide = surface_width.div_ceil(64);
        let expected_high = surface_height.div_ceil(64);
        if context.surface.tiles_wide != expected_wide || context.surface.tiles_high != expected_high {
            context.surface = SurfaceTiles::new(surface_width, surface_height, use_reduce_extrapolate)?;
        }
        context.surface.use_reduce_extrapolate = use_reduce_extrapolate;

        let mut decoded_tiles = Vec::new();

        // Process REGION blocks (the main content)
        for block in &blocks {
            let region = match block {
                ProgressiveBlock::Region(r) => r,
                _ => continue,
            };

            let quant_vals = &region.quant_vals;
            let prog_quant_vals = &region.quant_prog_vals;

            // mdrdp patch: the DWT variant is a property of the REGION.
            //
            // MS-RDPRFX carries the reduce-extrapolate bit in the RFX_PROGRESSIVE_REGION
            // flags as well as in the CONTEXT block, and FreeRDP reads the region's
            // (`region->flags & RFX_DWT_REDUCE_EXTRAPOLATE`, progressive.c:959 and :1366).
            // Applying the context's value to every region decodes a dissenting region
            // with the wrong band layout AND the wrong inverse transform.
            let region_reduce_extrapolate = region.uses_reduce_extrapolate();

            for tile_block in &region.tiles {
                let tiles = decode_tile_block(
                    &mut context.surface,
                    tile_block,
                    quant_vals,
                    prog_quant_vals,
                    region_reduce_extrapolate,
                )?;
                decoded_tiles.extend(tiles);
            }
        }

        Ok(decoded_tiles)
    }

    /// Delete a codec context, freeing its tile state.
    ///
    /// Called when the server sends RDPGFX_DELETE_ENCODING_CONTEXT.
    pub fn delete_context(&mut self, codec_context_id: u32) {
        self.contexts.remove(&codec_context_id);
    }

    /// Reset all contexts (e.g., on EGFX channel reset).
    pub fn reset(&mut self) {
        self.contexts.clear();
    }

    /// How many codec contexts currently hold tile state.
    ///
    /// mdrdp patch: exposed so the client's tests can prove that a surface delete or a
    /// graphics reset actually discards the state — the contexts themselves are private.
    pub fn context_count(&self) -> usize {
        self.contexts.len()
    }
}

#[expect(
    clippy::similar_names,
    reason = "q_y/q_cb/q_cr are standard component quant index names"
)]
fn decode_tile_block(
    surface: &mut SurfaceTiles,
    tile_block: &ironrdp_pdu::codecs::rfx::progressive::ProgressiveTile<'_>,
    quant_vals: &[ComponentCodecQuant],
    prog_quant_vals: &[ironrdp_pdu::codecs::rfx::progressive::ProgressiveCodecQuant],
    use_reduce_extrapolate: bool,
) -> Result<Vec<DecodedTile>, ProgressiveDecodeError> {
    use ironrdp_pdu::codecs::rfx::progressive::ProgressiveTile;

    match tile_block {
        ProgressiveTile::Simple(tile) => {
            let x_idx = tile.x_idx;
            let y_idx = tile.y_idx;

            let tile_state = surface
                .get_or_create(x_idx, y_idx)
                .ok_or(ProgressiveDecodeError::TileOutOfBounds { x_idx, y_idx })?;

            let q_y = usize::from(tile.quant_idx_y);
            let q_cb = usize::from(tile.quant_idx_cb);
            let q_cr = usize::from(tile.quant_idx_cr);

            if q_y >= quant_vals.len() || q_cb >= quant_vals.len() || q_cr >= quant_vals.len() {
                return Err(ProgressiveDecodeError::InvalidQuantIndex {
                    index: q_y.max(q_cb).max(q_cr),
                    table_len: quant_vals.len(),
                });
            }

            // TILE_SIMPLE uses lossless progressive quant (no progressive refinement)
            let prog = ComponentCodecQuant::LOSSLESS;

            tile_state.decode_first(
                [tile.y_data, tile.cb_data, tile.cr_data],
                [&quant_vals[q_y], &quant_vals[q_cb], &quant_vals[q_cr]],
                [prog, prog, prog],
                [tile.quant_idx_y, tile.quant_idx_cb, tile.quant_idx_cr],
                0xFF, // full quality
                use_reduce_extrapolate,
                tile.flags & RFX_TILE_DIFFERENCE != 0,
            )?;

            let mut pixels = vec![0u8; 64 * 64 * 4];
            tile_state.reconstruct_to_rgba(&mut pixels);

            Ok(vec![DecodedTile { x_idx, y_idx, pixels }])
        }

        ProgressiveTile::First(tile) => {
            let x_idx = tile.x_idx;
            let y_idx = tile.y_idx;

            let tile_state = surface
                .get_or_create(x_idx, y_idx)
                .ok_or(ProgressiveDecodeError::TileOutOfBounds { x_idx, y_idx })?;

            let q_y = usize::from(tile.quant_idx_y);
            let q_cb = usize::from(tile.quant_idx_cb);
            let q_cr = usize::from(tile.quant_idx_cr);

            if q_y >= quant_vals.len() || q_cb >= quant_vals.len() || q_cr >= quant_vals.len() {
                return Err(ProgressiveDecodeError::InvalidQuantIndex {
                    index: q_y.max(q_cb).max(q_cr),
                    table_len: quant_vals.len(),
                });
            }

            // mdrdp patch: `quality` is a quality LEVEL, not an index.
            //
            // MS-RDPRFX defines it as 0 (minimum) to 0xFF, where 0xFF means full quality
            // with no extra progressive quantization — ironrdp-pdu's own doc comment on
            // the field says exactly that. Using it to index `quant_prog_vals` makes a
            // full-quality tile look like index 255 into a table the server had no reason
            // to send, so every such frame was rejected with
            // "quant index 255 exceeds table length 0" and its region went unpainted.
            //
            // The TILE_SIMPLE arm above already gets this right: no progressive
            // quantization means LOSSLESS. This does the same, and still rejects a
            // genuinely out-of-range index.
            let lossless = ironrdp_pdu::codecs::rfx::progressive::ProgressiveCodecQuant {
                quality: tile.quality,
                y_quant: ComponentCodecQuant::LOSSLESS,
                cb_quant: ComponentCodecQuant::LOSSLESS,
                cr_quant: ComponentCodecQuant::LOSSLESS,
            };
            let pq_idx = usize::from(tile.quality);
            let pq = if tile.quality == 0xFF || prog_quant_vals.is_empty() {
                &lossless
            } else if pq_idx < prog_quant_vals.len() {
                &prog_quant_vals[pq_idx]
            } else {
                return Err(ProgressiveDecodeError::InvalidQuantIndex {
                    index: pq_idx,
                    table_len: prog_quant_vals.len(),
                });
            };

            tile_state.decode_first(
                [tile.y_data, tile.cb_data, tile.cr_data],
                [&quant_vals[q_y], &quant_vals[q_cb], &quant_vals[q_cr]],
                [pq.y_quant, pq.cb_quant, pq.cr_quant],
                [tile.quant_idx_y, tile.quant_idx_cb, tile.quant_idx_cr],
                tile.quality,
                use_reduce_extrapolate,
                tile.flags & RFX_TILE_DIFFERENCE != 0,
            )?;

            let mut pixels = vec![0u8; 64 * 64 * 4];
            tile_state.reconstruct_to_rgba(&mut pixels);

            Ok(vec![DecodedTile { x_idx, y_idx, pixels }])
        }

        ProgressiveTile::Upgrade(tile) => {
            let x_idx = tile.x_idx;
            let y_idx = tile.y_idx;

            let tile_state = surface
                .get_or_create(x_idx, y_idx)
                .ok_or(ProgressiveDecodeError::TileOutOfBounds { x_idx, y_idx })?;

            // If this tile hasn't had a first pass, skip the upgrade
            if tile_state.pass == 0 {
                return Ok(Vec::new());
            }

            // The base quantiser for this tile. The upgrade shift is
            // (base + prog - 1) per band, so the base values are required here — the
            // previous code never looked them up and silently shifted by the progressive
            // term alone.
            let q_y = usize::from(tile.quant_idx_y);
            let q_cb = usize::from(tile.quant_idx_cb);
            let q_cr = usize::from(tile.quant_idx_cr);
            if q_y >= quant_vals.len() || q_cb >= quant_vals.len() || q_cr >= quant_vals.len() {
                return Err(ProgressiveDecodeError::InvalidQuantIndex {
                    index: q_y.max(q_cb).max(q_cr),
                    table_len: quant_vals.len(),
                });
            }

            // mdrdp patch: `quality` is a quality LEVEL, not an index.
            //
            // MS-RDPRFX defines it as 0 (minimum) to 0xFF, where 0xFF means full quality
            // with no extra progressive quantization — ironrdp-pdu's own doc comment on
            // the field says exactly that. Using it to index `quant_prog_vals` makes a
            // full-quality tile look like index 255 into a table the server had no reason
            // to send, so every such frame was rejected with
            // "quant index 255 exceeds table length 0" and its region went unpainted.
            //
            // The TILE_SIMPLE arm above already gets this right: no progressive
            // quantization means LOSSLESS. This does the same, and still rejects a
            // genuinely out-of-range index.
            let lossless = ironrdp_pdu::codecs::rfx::progressive::ProgressiveCodecQuant {
                quality: tile.quality,
                y_quant: ComponentCodecQuant::LOSSLESS,
                cb_quant: ComponentCodecQuant::LOSSLESS,
                cr_quant: ComponentCodecQuant::LOSSLESS,
            };
            let pq_idx = usize::from(tile.quality);
            let pq = if tile.quality == 0xFF || prog_quant_vals.is_empty() {
                &lossless
            } else if pq_idx < prog_quant_vals.len() {
                &prog_quant_vals[pq_idx]
            } else {
                return Err(ProgressiveDecodeError::InvalidQuantIndex {
                    index: pq_idx,
                    table_len: prog_quant_vals.len(),
                });
            };

            tile_state.decode_upgrade(
                [tile.y_srl_data, tile.cb_srl_data, tile.cr_srl_data],
                [tile.y_raw_data, tile.cb_raw_data, tile.cr_raw_data],
                // The BASE quant, which the upgrade shift needs and which was previously
                // never passed in at all.
                [quant_vals[q_y], quant_vals[q_cb], quant_vals[q_cr]],
                [pq.y_quant, pq.cb_quant, pq.cr_quant],
                tile.quality,
            );

            let mut pixels = vec![0u8; 64 * 64 * 4];
            tile_state.reconstruct_to_rgba(&mut pixels);

            Ok(vec![DecodedTile { x_idx, y_idx, pixels }])
        }
    }
}

impl Default for ProgressiveDecoder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[expect(clippy::as_conversions, clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
mod tests {

    /// A leading `0` means a run of `1 << k` zeros, and with kp = 8 that k is 1.
    ///
    /// So the first TWO reads return zero from a single bit, and only the third consults
    /// the stream again. With kp = 0 the run would be `1 << 0` = one zero, and the second
    /// read would consume a bit that FreeRDP does not — the divergence starts on the very
    /// first symbol of every component.
    #[test]
    fn srl_zero_run_length_follows_kp_starting_at_eight() {
        let data = [0b0100_0000u8];
        let mut srl = SrlReader::new(&data);
        assert_eq!(srl.read(3), 0, "first of the run");
        assert_eq!(srl.read(3), 0, "second of the run, consuming no further bit");
        // The run of two is now exhausted; the next symbol reads the stream again. The
        // next bit is 1 (escape), so this is no longer part of the zero run.
        assert_ne!(srl.read(3), 0, "the run is over, so a value must be decoded");
    }

    /// `num_bits == 1` is the degenerate case: sign only, magnitude always 1.
    #[test]
    fn srl_with_one_bit_returns_plus_or_minus_one() {
        // escape, run length 0, then sign = 1 (negative).
        let data = [0b1010_0000u8];
        let mut srl = SrlReader::new(&data);
        assert_eq!(srl.read(1), -1);
    }

    /// The reader is stateful across calls, which is what lets one stream serve all ten
    /// bands of a component. Two independent readers over the same bytes must agree only
    /// on their FIRST symbol — after that the shared one has advanced.
    #[test]
    fn srl_state_persists_across_reads() {
        // Two complete symbols in one byte: +1 (`1001`), then -1 (`111`). A reader
        // restarted for the second call would decode +1 again.
        let data = [0b1001_1110u8];
        let mut shared = SrlReader::new(&data);
        let first = shared.read(3);
        let second = shared.read(3);

        let mut fresh = SrlReader::new(&data);
        assert_eq!(fresh.read(3), first);
        assert_eq!((first, second), (1, -1));
    }

    use super::*;

    #[test]
    fn surface_tiles_rejects_over_cap_dimensions() {
        // At-cap accepted on both axes
        assert!(SurfaceTiles::new(MAX_SURFACE_DIM, MAX_SURFACE_DIM, false).is_ok());

        // One axis over the cap is rejected with SurfaceTooLarge carrying both inputs
        let over_w = MAX_SURFACE_DIM.checked_add(1).unwrap();
        match SurfaceTiles::new(over_w, 1024, false) {
            Err(ProgressiveDecodeError::SurfaceTooLarge { width, height }) => {
                assert_eq!(width, over_w);
                assert_eq!(height, 1024);
            }
            Err(other) => panic!("expected SurfaceTooLarge, got Err({other})"),
            Ok(_) => panic!("expected SurfaceTooLarge, got Ok"),
        }

        match SurfaceTiles::new(1024, over_w, false) {
            Err(ProgressiveDecodeError::SurfaceTooLarge { width, height }) => {
                assert_eq!(width, 1024);
                assert_eq!(height, over_w);
            }
            Err(other) => panic!("expected SurfaceTooLarge, got Err({other})"),
            Ok(_) => panic!("expected SurfaceTooLarge, got Ok"),
        }
    }

    #[test]
    fn standard_band_layout_totals_4096() {
        let bands = standard_band_layout();
        let total: usize = bands.iter().map(|b| b.count()).sum();
        assert_eq!(total, 4096);
    }

    #[test]
    fn standard_band_offsets() {
        let bands = standard_band_layout();
        assert_eq!(bands[0].offset, 0);
        assert_eq!(bands[1].offset, 1024);
        assert_eq!(bands[2].offset, 2048);
        assert_eq!(bands[3].offset, 3072);
        assert_eq!(bands[4].offset, 3328);
        assert_eq!(bands[5].offset, 3584);
        assert_eq!(bands[6].offset, 3840);
        assert_eq!(bands[7].offset, 3904);
        assert_eq!(bands[8].offset, 3968);
        assert_eq!(bands[9].offset, 4032);
    }

    #[test]
    fn sign_capture_tri_state() {
        let coefficients = [10i16, -5, 0, 100, -1, 0];
        let mut sign = [0i8; 6];
        capture_sign(&coefficients, &mut sign);
        assert_eq!(sign, [1, -1, 0, 1, -1, 0]);
    }

    #[test]
    fn progressive_dequantize_ll3_shift() {
        // LL3 is band index 9, at offset 4032 for standard layout
        let mut coefficients = vec![0i16; 4096];
        coefficients[4032] = 5;
        coefficients[4033] = -3;

        let prog_quant = ComponentCodecQuant {
            ll3: 2,
            hl3: 0,
            lh3: 0,
            hh3: 0,
            hl2: 0,
            lh2: 0,
            hh2: 0,
            hl1: 0,
            lh1: 0,
            hh1: 0,
        };

        progressive_dequantize(&mut coefficients, &prog_quant, false);

        // LL3 uses floor shift: 5 << 2 = 20, -3 << 2 = -12
        assert_eq!(coefficients[4032], 20);
        assert_eq!(coefficients[4033], -12);
    }

    #[test]
    fn progressive_dequantize_non_ll3_preserves_sign() {
        // HL1 is band index 0, at offset 0 for standard layout
        let mut coefficients = vec![0i16; 4096];
        coefficients[0] = 5;
        coefficients[1] = -5;

        let prog_quant = ComponentCodecQuant {
            ll3: 0,
            hl3: 0,
            lh3: 0,
            hh3: 0,
            hl2: 0,
            lh2: 0,
            hh2: 0,
            hl1: 2,
            lh1: 0,
            hh1: 0,
        };

        progressive_dequantize(&mut coefficients, &prog_quant, false);

        // Non-LL3: shift absolute value, preserve sign
        assert_eq!(coefficients[0], 20); // 5 << 2
        assert_eq!(coefficients[1], -20); // -(5 << 2)
    }

    #[test]
    fn progressive_quantize_round_trip() {
        let mut coefficients = vec![0i16; 4096];
        for (i, c) in coefficients.iter_mut().enumerate() {
            *c = (i as i16).wrapping_mul(7);
        }
        let original = coefficients.clone();

        let prog_quant = ComponentCodecQuant {
            ll3: 2,
            hl3: 3,
            lh3: 3,
            hh3: 4,
            hl2: 3,
            lh2: 3,
            hh2: 4,
            hl1: 2,
            lh1: 2,
            hh1: 3,
        };

        progressive_quantize(&mut coefficients, &prog_quant, false);
        progressive_dequantize(&mut coefficients, &prog_quant, false);

        // After quantize->dequantize, values lose precision from truncation
        // but should be in the right ballpark
        for (i, (&a, &b)) in coefficients.iter().zip(original.iter()).enumerate() {
            let err = (i32::from(a) - i32::from(b)).unsigned_abs();
            // Max error bounded by 2^(bit_pos)
            assert!(err < 32, "index {i}: error {err} too large");
        }
    }

    #[test]
    fn raw_bit_reader_basic() {
        let data = [0b10110000, 0b01010000];
        let mut reader = RawBitReader::new(&data);
        assert_eq!(reader.read_bits(4), 0b1011);
        assert_eq!(reader.read_bits(4), 0b0000);
        assert_eq!(reader.read_bits(4), 0b0101);
    }

    #[test]
    fn clamp_i16_limits() {
        assert_eq!(clamp_i16(40000), i16::MAX);
        assert_eq!(clamp_i16(-40000), i16::MIN);
        assert_eq!(clamp_i16(100), 100);
        assert_eq!(clamp_i16(-100), -100);
    }

    #[test]
    fn band_zero_count_counts_correctly() {
        let mut sign = [0i8; 4096];
        // Band 0 (HL1): offset 0, count 1024
        sign[0] = SIGN_POSITIVE;
        sign[1] = SIGN_NEGATIVE;
        sign[2] = SIGN_ZERO;
        // Rest are SIGN_ZERO by default

        let bands = standard_band_layout();
        assert_eq!(band_zero_count(&sign, &bands[0]), 1022); // 1024 - 2 non-zero
    }

    #[test]
    fn ll3_offsets_correct() {
        assert_eq!(ll3_offset(false), 4032);
        assert_eq!(ll3_offset(true), 4015);
    }

    #[test]
    fn upgrade_pass_zero_das_becomes_nonzero() {
        let mut coefficients = vec![0i16; 4096];
        let mut sign = vec![SIGN_ZERO; 4096];

        // Set up SRL data that produces a non-zero value for the first position
        // For band 0 (HL1), with num_bits=2, SRL should produce some values
        let prev_prog_quant = ComponentCodecQuant {
            ll3: 0,
            hl3: 0,
            lh3: 0,
            hh3: 0,
            hl2: 0,
            lh2: 0,
            hh2: 0,
            hl1: 4,
            lh1: 0,
            hh1: 0,
        };
        let curr_prog_quant = ComponentCodecQuant {
            ll3: 0,
            hl3: 0,
            lh3: 0,
            hh3: 0,
            hl2: 0,
            lh2: 0,
            hh2: 0,
            hl1: 2,
            lh1: 0,
            hh1: 0,
        };

        // Simple SRL data: a non-zero value (the SRL decoder will interpret
        // bits as magnitude + sign). With num_bits=2, k=0 initially,
        // it goes straight to magnitude decode.
        let srl_data = vec![0b01000000, 0x00]; // sign=0(+), magnitude bits follow
        let raw_data = vec![];

        decode_upgrade_pass(
            &srl_data,
            &raw_data,
            &ComponentCodecQuant::LOSSLESS,
            &prev_prog_quant,
            &curr_prog_quant,
            false,
            &mut coefficients,
            &mut sign,
        );

        // After decode, at least some positions should have been updated
        // (exact values depend on SRL interpretation, but the function shouldn't panic)
    }

    #[test]
    fn tile_state_default_is_zeroed() {
        let tile = TileState::new();
        assert_eq!(tile.pass, 0);
        assert_eq!(tile.quality, 0);
        assert!(!tile.use_reduce_extrapolate);
        assert!(tile.coefficients[0].iter().all(|&v| v == 0));
        assert!(tile.sign[0].iter().all(|&v| v == 0));
    }

    #[test]
    fn surface_tiles_dimensions() {
        let surface = SurfaceTiles::new(1920, 1080, true).unwrap();
        assert_eq!(surface.tiles_wide, 30);
        assert_eq!(surface.tiles_high, 17);
        assert!(surface.use_reduce_extrapolate);
    }

    #[test]
    fn surface_tiles_exact_multiple() {
        // 1280 / 64 = 20, 768 / 64 = 12 (exact, no rounding)
        let surface = SurfaceTiles::new(1280, 768, false).unwrap();
        assert_eq!(surface.tiles_wide, 20);
        assert_eq!(surface.tiles_high, 12);
    }

    #[test]
    fn surface_tiles_lazy_allocation() {
        let mut surface = SurfaceTiles::new(128, 128, false).unwrap();
        // No tiles allocated yet
        assert!(surface.get(0, 0).is_none());

        // Access creates tile
        let tile = surface.get_or_create(0, 0).unwrap();
        assert_eq!(tile.pass, 0);
        assert!(!tile.use_reduce_extrapolate);

        // Now it exists
        assert!(surface.get(0, 0).is_some());

        // Out of bounds returns None
        assert!(surface.get_or_create(2, 2).is_none());
    }

    #[test]
    fn surface_tiles_reset() {
        let mut surface = SurfaceTiles::new(128, 128, false).unwrap();
        surface.get_or_create(0, 0);
        assert!(surface.get(0, 0).is_some());

        surface.reset();
        assert!(surface.get(0, 0).is_none());
    }

    #[test]
    fn decoder_new_is_empty() {
        let decoder = ProgressiveDecoder::new();
        assert!(decoder.contexts.is_empty());
    }

    #[test]
    fn decoder_delete_nonexistent_context() {
        let mut decoder = ProgressiveDecoder::new();
        // Should not panic on non-existent context
        decoder.delete_context(42);
    }

    #[test]
    fn decoder_reset_clears_contexts() {
        let mut decoder = ProgressiveDecoder::new();

        // Decode a minimal valid stream to create a context
        use ironrdp_pdu::codecs::rfx::RfxRectangle;
        use ironrdp_pdu::codecs::rfx::progressive::{
            ProgressiveBlock, ProgressiveContextPdu, ProgressiveFrameBeginPdu, ProgressiveFrameEndPdu,
            ProgressiveRegion, ProgressiveSyncPdu, encode_progressive_stream,
        };

        let region = ProgressiveRegion {
            tile_size: 0x40,
            rects: vec![RfxRectangle {
                x: 0,
                y: 0,
                width: 64,
                height: 64,
            }],
            quant_vals: vec![],
            quant_prog_vals: vec![],
            flags: 0,
            tiles: vec![],
        };

        let blocks = vec![
            ProgressiveBlock::Sync(ProgressiveSyncPdu),
            ProgressiveBlock::Context(ProgressiveContextPdu {
                context_id: 0,
                tile_size: 0x0040,
                flags: 0,
            }),
            ProgressiveBlock::FrameBegin(ProgressiveFrameBeginPdu {
                frame_index: 0,
                region_count: 1,
            }),
            ProgressiveBlock::Region(region),
            ProgressiveBlock::FrameEnd(ProgressiveFrameEndPdu),
        ];

        let encoded = encode_progressive_stream(&blocks).unwrap();
        let result = decoder.decode_bitmap(1, 640, 480, &encoded);
        assert!(result.is_ok());
        assert_eq!(decoder.contexts.len(), 1);

        decoder.reset();
        assert!(decoder.contexts.is_empty());
    }

    #[test]
    fn decoder_error_display() {
        let e = ProgressiveDecodeError::MissingBlock("SYNC");
        assert!(e.to_string().contains("SYNC"));

        let e = ProgressiveDecodeError::TileOutOfBounds { x_idx: 5, y_idx: 10 };
        assert!(e.to_string().contains("5"));
        assert!(e.to_string().contains("10"));

        let e = ProgressiveDecodeError::InvalidQuantIndex { index: 3, table_len: 2 };
        assert!(e.to_string().contains("3"));
    }

    #[test]
    fn dequantize_component_ccq_shifts_correctly() {
        let mut coefficients = vec![0i16; 4096];
        coefficients[0] = 10; // HL1 band (index 0)
        coefficients[4032] = 5; // LL3 band (index 9, standard layout)

        let quant = ComponentCodecQuant {
            ll3: 3,
            hl3: 0,
            lh3: 0,
            hh3: 0,
            hl2: 0,
            lh2: 0,
            hh2: 0,
            hl1: 4,
            lh1: 0,
            hh1: 0,
        };

        dequantize_component_ccq(&mut coefficients, &quant, false);

        // HL1: shift left by (4 - 1) = 3 -> 10 << 3 = 80
        assert_eq!(coefficients[0], 80);
        // LL3: shift left by (3 - 1) = 2 -> 5 << 2 = 20
        assert_eq!(coefficients[4032], 20);
    }

    // --- B10: Server encode pipeline tests ---

    #[test]
    fn rgba_to_ycbcr_pure_white() {
        let pixels = vec![255u8; 64 * 64 * 4];
        let mut y = vec![0i16; 4096];
        let mut cb = vec![0i16; 4096];
        let mut cr = vec![0i16; 4096];

        rgba_to_ycbcr(&pixels, &mut y, &mut cb, &mut cr);

        // Pure white: R=G=B=255
        // Y = (19595*255 + 38470*255 + 7471*255 + 32768) >> 16 - 128
        //   = (65536*255 + 32768) >> 16 - 128 = 255 - 128 = 127
        // Cb and Cr should be ~0 (achromatic)
        assert!((y[0] - 127).abs() <= 1, "Y for white: got {}", y[0]);
        assert!(cb[0].abs() <= 1, "Cb for white: got {}", cb[0]);
        assert!(cr[0].abs() <= 1, "Cr for white: got {}", cr[0]);
    }

    #[test]
    fn rgba_to_ycbcr_pure_black() {
        let pixels = vec![0u8; 64 * 64 * 4];
        let mut y = vec![0i16; 4096];
        let mut cb = vec![0i16; 4096];
        let mut cr = vec![0i16; 4096];

        rgba_to_ycbcr(&pixels, &mut y, &mut cb, &mut cr);

        // Pure black: Y = -128, Cb = 0, Cr = 0
        assert_eq!(y[0], -128);
        assert_eq!(cb[0], 0);
        assert_eq!(cr[0], 0);
    }

    #[test]
    fn quantize_ccq_right_shifts() {
        let mut coefficients = [0i16; 4096];
        coefficients[0] = 80; // HL1 band
        coefficients[4032] = 20; // LL3 band

        let quant = ComponentCodecQuant {
            ll3: 3,
            hl3: 0,
            lh3: 0,
            hh3: 0,
            hl2: 0,
            lh2: 0,
            hh2: 0,
            hl1: 4,
            lh1: 0,
            hh1: 0,
        };

        quantize_component_ccq(&mut coefficients, &quant, false);

        // HL1: 80 >> (4 - 1) = 80 >> 3 = 10
        assert_eq!(coefficients[0], 10);
        // LL3: 20 >> (3 - 1) = 20 >> 2 = 5
        assert_eq!(coefficients[4032], 5);
    }

    #[test]
    fn quantize_ccq_negative_truncates_toward_zero() {
        let mut coefficients = [0i16; 4096];
        coefficients[0] = -80; // HL1 band, negative

        let quant = ComponentCodecQuant {
            ll3: 0,
            hl3: 0,
            lh3: 0,
            hh3: 0,
            hl2: 0,
            lh2: 0,
            hh2: 0,
            hl1: 4,
            lh1: 0,
            hh1: 0,
        };

        quantize_component_ccq(&mut coefficients, &quant, false);

        // -80 truncated toward zero: -(80 >> 3) = -10
        assert_eq!(coefficients[0], -10);
    }

    #[test]
    fn raw_bit_writer_single_byte() {
        let mut w = RawBitWriter::new();
        w.write_bits(0xA5, 8);
        assert_eq!(w.finish(), vec![0xA5]);
    }

    #[test]
    fn raw_bit_writer_partial_byte_padded() {
        let mut w = RawBitWriter::new();
        w.write_bits(0b101, 3);
        // 3 bits: 101, padded to 10100000 = 0xA0
        assert_eq!(w.finish(), vec![0xA0]);
    }

    #[test]
    fn raw_bit_writer_multi_byte() {
        let mut w = RawBitWriter::new();
        w.write_bits(0xFF, 8);
        w.write_bits(0b1010, 4);
        // First byte: 0xFF, second partial: 1010_0000 = 0xA0
        assert_eq!(w.finish(), vec![0xFF, 0xA0]);
    }

    #[test]
    fn encode_first_pass_produces_output() {
        // Flat tile: all same value, should compress well
        let mut coefficients = [100i16; 4096];
        let mut output = vec![0u8; 8192];

        let base_quant = ComponentCodecQuant::LOSSLESS;
        let prog_quant = ComponentCodecQuant::LOSSLESS;

        let result = encode_first_pass(&mut coefficients, &mut output, &base_quant, &prog_quant, false);

        assert!(result.is_ok(), "RLGR encode failed: {:?}", result.err());
        let bytes_written = result.unwrap();
        assert!(bytes_written > 0, "expected non-zero encoded output");
        assert!(bytes_written < 8192, "flat tile should compress");
    }

    #[test]
    fn encode_first_pass_reduce_extrapolate() {
        let mut coefficients = [50i16; 4096];
        let mut output = vec![0u8; 8192];

        let base_quant = ComponentCodecQuant::LOSSLESS;
        let prog_quant = ComponentCodecQuant::LOSSLESS;

        let result = encode_first_pass(
            &mut coefficients,
            &mut output,
            &base_quant,
            &prog_quant,
            true, // reduce-extrapolate mode
        );

        assert!(result.is_ok(), "RLGR encode failed: {:?}", result.err());
        assert!(result.unwrap() > 0);
    }

    #[test]
    fn encode_upgrade_pass_empty_when_no_refinement() {
        let coefficients = [0i16; 4096];
        let prev_coefficients = [0i16; 4096];
        let sign = [SIGN_ZERO; 4096];

        // Same prog_quant for prev and curr -> num_bits = 0, no refinement
        let prog_quant = ComponentCodecQuant::LOSSLESS;

        let (srl_data, raw_data) = encode_upgrade_pass(
            &coefficients,
            &prev_coefficients,
            &prog_quant,
            &prog_quant,
            &sign,
            false,
        );

        assert!(srl_data.is_empty(), "no refinement bits, SRL should be empty");
        assert!(raw_data.is_empty(), "no refinement bits, raw should be empty");
    }
}
