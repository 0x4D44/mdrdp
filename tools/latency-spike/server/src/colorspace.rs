//! `D3D11_VIDEO_PROCESSOR_COLOR_SPACE` bit packing.
//!
//! windows-rs surfaces that struct as a bare `_bitfield: u32`, so the bit layout has
//! to be written by hand. Getting it wrong does not fail — it produces a picture
//! with the wrong contrast or swapped chroma, which is exactly the kind of bug that
//! wastes an afternoon on a remote host. So the packing lives here, portable, with
//! its own tests.
//!
//! Layout, from `d3d11.h`:
//!
//! ```text
//! UINT Usage         : 1;   // bit 0    0 = playback, 1 = video processing
//! UINT RGB_Range     : 1;   // bit 1    0 = full (0-255), 1 = limited (16-235)
//! UINT YCbCr_Matrix  : 1;   // bit 2    0 = BT.601, 1 = BT.709
//! UINT YCbCr_xvYCC   : 1;   // bit 3    0 = conventional, 1 = xvYCC
//! UINT Nominal_Range : 2;   // bits 4-5 D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE
//! UINT Reserved      : 26;
//! ```

pub const USAGE_PLAYBACK: u32 = 0;
pub const RGB_RANGE_FULL: u32 = 0;
pub const RGB_RANGE_LIMITED: u32 = 1;
pub const MATRIX_BT601: u32 = 0;
pub const MATRIX_BT709: u32 = 1;
pub const XVYCC_CONVENTIONAL: u32 = 0;

/// `D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_UNDEFINED`
pub const NOMINAL_UNDEFINED: u32 = 0;
/// `D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_16_235`
pub const NOMINAL_16_235: u32 = 1;
/// `D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_0_255`
pub const NOMINAL_0_255: u32 = 2;

/// Pack the five fields into the `_bitfield` word.
///
/// Each argument is masked to its field width, so an out-of-range value cannot
/// corrupt a neighbouring field — it clamps loudly wrong in one place instead of
/// quietly wrong in two.
pub fn pack(usage: u32, rgb_range: u32, ycbcr_matrix: u32, xvycc: u32, nominal_range: u32) -> u32 {
    (usage & 0x1)
        | ((rgb_range & 0x1) << 1)
        | ((ycbcr_matrix & 0x1) << 2)
        | ((xvycc & 0x1) << 3)
        | ((nominal_range & 0x3) << 4)
}

/// The desktop side of the conversion: BGRA from Desktop Duplication is full-range
/// sRGB, and we ask for BT.709 coefficients because that is what the HEVC decoder
/// assumes for HD content.
pub fn desktop_bgra_input() -> u32 {
    pack(
        USAGE_PLAYBACK,
        RGB_RANGE_FULL,
        MATRIX_BT709,
        XVYCC_CONVENTIONAL,
        NOMINAL_0_255,
    )
}

/// The encoder side: studio-range NV12, BT.709. Studio range is what the MF HEVC
/// encoder expects by default, and what the mdrdp decoder's YUV→RGB path assumes.
pub fn nv12_output() -> u32 {
    pack(
        USAGE_PLAYBACK,
        RGB_RANGE_FULL,
        MATRIX_BT709,
        XVYCC_CONVENTIONAL,
        NOMINAL_16_235,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_field_lands_on_its_own_bit() {
        // One field set at a time, so a shift error in any one of them shows up as a
        // single failing assertion rather than a scrambled total.
        assert_eq!(pack(1, 0, 0, 0, 0), 0b00_0001);
        assert_eq!(pack(0, 1, 0, 0, 0), 0b00_0010);
        assert_eq!(pack(0, 0, 1, 0, 0), 0b00_0100);
        assert_eq!(pack(0, 0, 0, 1, 0), 0b00_1000);
        assert_eq!(pack(0, 0, 0, 0, 1), 0b01_0000);
        assert_eq!(pack(0, 0, 0, 0, 2), 0b10_0000);
        assert_eq!(pack(0, 0, 0, 0, 3), 0b11_0000);
    }

    #[test]
    fn all_fields_together_do_not_collide() {
        assert_eq!(pack(1, 1, 1, 1, 3), 0b11_1111);
    }

    #[test]
    fn an_out_of_range_value_is_masked_rather_than_bleeding_upward() {
        // nominal_range is 2 bits; 7 must not spill into the reserved bits.
        assert_eq!(pack(0, 0, 0, 0, 7), 0b11_0000);
        // usage is 1 bit; 3 must not set bit 1 (RGB_Range).
        assert_eq!(pack(3, 0, 0, 0, 0), 0b00_0001);
    }

    #[test]
    fn the_two_named_color_spaces_are_the_documented_words() {
        // BT.709 matrix (bit 2) plus Nominal_Range 0-255 (2 << 4).
        assert_eq!(desktop_bgra_input(), 0b10_0100);
        // BT.709 matrix (bit 2) plus Nominal_Range 16-235 (1 << 4).
        assert_eq!(nv12_output(), 0b01_0100);
        assert_ne!(
            desktop_bgra_input(),
            nv12_output(),
            "input and output must differ, or the range conversion is a no-op"
        );
    }
}
