//! Geometry for the presenter.
//!
//! The pixel work itself is `mdrdp::window::present_into` (`src/window.rs:380`) — the
//! scale/letterbox/RGBA→0RGB pass an mdrdp session runs on every frame. Only the
//! [`Viewport`] handed to it is built here, and it is built at **1:1**, never
//! letterbox-scaled.
//!
//! That is a measurement decision, not a style one. `Viewport::letterbox` fits the
//! stream to the window, so a window one pixel off the stream size silently turns on
//! nearest-neighbour resampling and puts a variable per-frame cost inside the very
//! stage being measured. At 1:1 `present_into`'s column table is the identity, so the
//! present stage is a straight copy-and-convert: a window smaller than the stream
//! crops, a window larger centres the image on black. Neither costs anything that
//! varies with the mismatch.

use mdrdp::window::Viewport;

/// A viewport that maps one stream pixel to one window pixel, centred.
///
/// `None` when the frame cannot be presented at all — a zero dimension, or one past
/// `u16::MAX`, which `Viewport` cannot describe. Both are impossible from a real H.264
/// stream and both would otherwise be presented as garbage.
pub fn one_to_one(
    window_width: u32,
    window_height: u32,
    frame_width: u32,
    frame_height: u32,
) -> Option<Viewport> {
    let session_width = u16::try_from(frame_width).ok()?;
    let session_height = u16::try_from(frame_height).ok()?;
    if session_width == 0 || session_height == 0 {
        return None;
    }
    Some(Viewport {
        // Saturating: a window narrower than the frame pins the image at the origin
        // and lets `present_into` clip the overhang, which crops rather than scales.
        dest_x: window_width.saturating_sub(frame_width) / 2,
        dest_y: window_height.saturating_sub(frame_height) / 2,
        dest_width: frame_width,
        dest_height: frame_height,
        session_width,
        session_height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdrdp::window::present_into;

    /// A 2x2 RGBA image whose four pixels are all different, so a transposed or
    /// mis-strided copy cannot pass.
    fn src_2x2() -> Vec<u8> {
        vec![
            0x10, 0x20, 0x30, 0xFF, // (0,0)
            0x40, 0x50, 0x60, 0xFF, // (1,0)
            0x70, 0x80, 0x90, 0xFF, // (0,1)
            0xA0, 0xB0, 0xC0, 0xFF, // (1,1)
        ]
    }

    const P00: u32 = 0x0010_2030;
    const P10: u32 = 0x0040_5060;
    const P01: u32 = 0x0070_8090;
    const P11: u32 = 0x00A0_B0C0;

    #[test]
    fn a_window_at_the_stream_size_is_an_exact_copy() {
        let v = one_to_one(2, 2, 2, 2).unwrap();
        assert_eq!((v.dest_x, v.dest_y), (0, 0));
        let mut dst = vec![0xDEAD_BEEF_u32; 4];
        present_into(&mut dst, 2, 2, &v, &src_2x2());
        assert_eq!(dst, vec![P00, P10, P01, P11]);
    }

    #[test]
    fn a_larger_window_centres_the_image_on_black_without_scaling() {
        // 4x4 window, 2x2 stream: a one-pixel border, and the image itself untouched.
        let v = one_to_one(4, 4, 2, 2).unwrap();
        assert_eq!((v.dest_x, v.dest_y), (1, 1));
        assert_eq!(
            (v.dest_width, v.dest_height),
            (2, 2),
            "not stretched to fill"
        );
        let mut dst = vec![0xDEAD_BEEF_u32; 16];
        present_into(&mut dst, 4, 4, &v, &src_2x2());
        let expected = vec![
            0, 0, 0, 0, //
            0, P00, P10, 0, //
            0, P01, P11, 0, //
            0, 0, 0, 0,
        ];
        assert_eq!(dst, expected);
    }

    #[test]
    fn a_smaller_window_crops_rather_than_shrinking_the_image() {
        // A four-pixel red ramp, 0x00 / 0x11 / 0x22 / 0x33, into a two-pixel window.
        // Cropping keeps columns 0 and 1; fitting would sample columns 0 and 2. The
        // ramp is what makes those two outcomes distinguishable — a uniform fixture
        // would pass under either.
        let src: Vec<u8> = (0u8..4)
            .flat_map(|i| [i * 0x11, 0x00, 0x00, 0xFF])
            .collect();

        let cropped = one_to_one(2, 1, 4, 1).unwrap();
        assert_eq!(
            (cropped.dest_x, cropped.dest_y),
            (0, 0),
            "pinned, not centred off-screen"
        );
        assert_eq!(
            cropped.dest_width, 4,
            "the viewport describes the frame, not the window"
        );
        let mut dst = vec![0xDEAD_BEEF_u32; 2];
        present_into(&mut dst, 2, 1, &cropped, &src);
        assert_eq!(dst, vec![0x0000_0000, 0x0011_0000], "columns 0 and 1");

        // The alternative this module exists to avoid, run on the same input.
        let fitted = Viewport::letterbox(2, 1, 4, 1);
        let mut dst = vec![0xDEAD_BEEF_u32; 2];
        present_into(&mut dst, 2, 1, &fitted, &src);
        assert_eq!(
            dst,
            vec![0x0000_0000, 0x0022_0000],
            "resampled: columns 0 and 2"
        );
    }

    #[test]
    fn a_frame_too_large_for_a_viewport_is_refused_rather_than_truncated() {
        assert!(one_to_one(1920, 1080, 70_000, 1080).is_none());
        assert!(one_to_one(1920, 1080, 1920, 70_000).is_none());
    }

    #[test]
    fn a_zero_dimension_frame_is_refused() {
        assert!(one_to_one(1920, 1080, 0, 1080).is_none());
        assert!(one_to_one(1920, 1080, 1920, 0).is_none());
    }
}
