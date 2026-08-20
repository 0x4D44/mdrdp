//! Geometry for the presenter, and the damage-only convert built on it.
//!
//! The full-surface pixel work is `mdrdp::window::present_into` (`src/window.rs:380`)
//! — the scale/letterbox/RGBA→0RGB pass an mdrdp session runs on every frame. Only the
//! [`Viewport`] handed to it is built here, and it is built at **1:1**, never
//! letterbox-scaled. [`present_region_into`] is the one piece of pixel work this
//! module owns: the same conversion restricted to a damaged rectangle, which is what
//! makes the window thread's persistent canvas cheap to maintain.
//!
//! That is a measurement decision, not a style one. `Viewport::letterbox` fits the
//! stream to the window, so a window one pixel off the stream size silently turns on
//! nearest-neighbour resampling and puts a variable per-frame cost inside the very
//! stage being measured. At 1:1 `present_into`'s column table is the identity, so the
//! present stage is a straight copy-and-convert: a window smaller than the stream
//! crops, a window larger centres the image on black. Neither costs anything that
//! varies with the mismatch.

use mdrdp::window::Viewport;

use crate::sink::DamageRect;

/// A viewport that maps one stream pixel to one window pixel, centred.
///
/// `None` when the frame cannot be presented at all — a zero dimension, or one past
/// `u16::MAX`, which `Viewport` cannot describe. Both are impossible from a real HEVC
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

/// Convert one damaged rectangle of `rgba` into `dst`, leaving every other pixel of
/// `dst` exactly as it was.
///
/// This is the partial-present path's whole reason to exist. softbuffer's macOS
/// backend hands out a fresh zeroed buffer per present and ignores damage, so the
/// window thread keeps its own persistent converted canvas and maintains it
/// incrementally: a rect update converts its own few thousand pixels here instead of
/// paying a full-surface `present_into` (~3–4 ms at 1920x1080) to show them.
///
/// **Only correct for the [`one_to_one`] viewport.** There is no column table and no
/// row scaling: source pixel `(x, y)` lands at window pixel `(dest_x + x, dest_y + y)`.
/// A scaled viewport would need the damage remapped through the same resampling
/// `present_into` does, which this function deliberately does not implement — it
/// refuses such a viewport and writes nothing rather than painting a wrong region.
///
/// `region` is in frame coordinates and is clipped against both the frame and the
/// window, so a rect that hangs off either edge paints its visible part and nothing
/// else. A region entirely outside writes nothing at all.
///
/// The packing is `present_into`'s: `0x00RRGGBB`, alpha dropped.
pub fn present_region_into(
    dst: &mut [u32],
    dst_width: u32,
    dst_height: u32,
    viewport: &Viewport,
    rgba: &[u8],
    region: DamageRect,
) {
    let session_width = u32::from(viewport.session_width);
    let session_height = u32::from(viewport.session_height);
    // Every guard writes nothing rather than blanking: this function only ever owns
    // the pixels of `region`, and a caller that hands it something it cannot honour
    // must not have the rest of its canvas destroyed as a side effect.
    if viewport.dest_width != session_width || viewport.dest_height != session_height {
        return; // Not 1:1 — see the doc comment.
    }
    if session_width == 0 || session_height == 0 {
        return;
    }
    if dst.len() < (dst_width as usize) * (dst_height as usize) {
        return;
    }
    if rgba.len() < (session_width as usize) * (session_height as usize) * 4 {
        return;
    }
    if viewport.dest_x >= dst_width || viewport.dest_y >= dst_height {
        return;
    }

    // Clip in frame coordinates, against the frame itself and against what the
    // window can still show at this viewport's offset.
    let visible_width = dst_width - viewport.dest_x;
    let visible_height = dst_height - viewport.dest_y;
    let x0 = region.x.min(session_width);
    let y0 = region.y.min(session_height);
    let x1 = region
        .x
        .saturating_add(region.w)
        .min(session_width)
        .min(visible_width);
    let y1 = region
        .y
        .saturating_add(region.h)
        .min(session_height)
        .min(visible_height);
    if x0 >= x1 || y0 >= y1 {
        return;
    }

    for y in y0..y1 {
        let src_row = (y as usize) * (session_width as usize) * 4;
        let dst_row = ((y + viewport.dest_y) as usize) * (dst_width as usize);
        for x in x0..x1 {
            let off = src_row + (x as usize) * 4;
            let r = u32::from(rgba[off]);
            let g = u32::from(rgba[off + 1]);
            let b = u32::from(rgba[off + 2]);
            dst[dst_row + (x + viewport.dest_x) as usize] = (r << 16) | (g << 8) | b;
        }
    }
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

    // ---- The damage-only convert ----

    /// A 4x3 RGBA frame where every pixel is unique, so a wrong row stride, a
    /// transposed index or a swapped channel lands a value that belongs somewhere
    /// else rather than accidentally matching.
    fn src_4x3() -> Vec<u8> {
        (0u8..12)
            .flat_map(|i| [i * 0x11, i * 0x11 + 1, i * 0x11 + 2, 0xFF])
            .collect()
    }

    const SENTINEL: u32 = 0xDEAD_BEEF;

    /// What a full `present_into` of the same input produces — the oracle. Expected
    /// values are never hand-derived: the whole point of the region convert is that it
    /// agrees with the function it is an optimisation of.
    fn full(window: (u32, u32), viewport: &Viewport, src: &[u8]) -> Vec<u32> {
        let mut dst = vec![SENTINEL; (window.0 * window.1) as usize];
        present_into(&mut dst, window.0, window.1, viewport, src);
        dst
    }

    #[test]
    fn converting_a_region_touches_exactly_that_regions_pixels() {
        let v = one_to_one(4, 3, 4, 3).unwrap();
        let src = src_4x3();
        let oracle = full((4, 3), &v, &src);

        let mut dst = vec![SENTINEL; 12];
        present_region_into(
            &mut dst,
            4,
            3,
            &v,
            &src,
            DamageRect {
                x: 1,
                y: 1,
                w: 2,
                h: 2,
            },
        );

        for y in 0..3u32 {
            for x in 0..4u32 {
                let i = (y * 4 + x) as usize;
                let inside = (1..3).contains(&x) && (1..3).contains(&y);
                if inside {
                    assert_eq!(
                        dst[i], oracle[i],
                        "({x},{y}) is in the region: it must equal the full convert"
                    );
                } else {
                    assert_eq!(
                        dst[i], SENTINEL,
                        "({x},{y}) is outside the region: it must be untouched"
                    );
                }
            }
        }
    }

    #[test]
    fn a_regions_pixels_are_packed_exactly_as_the_full_convert_packs_them() {
        // The differential test, over the whole surface: converting every rect of a
        // partition must reproduce `present_into` byte for byte, including the black
        // border a smaller-than-window frame gets. The border is why the canvas starts
        // from a full convert — the region pass only owns the frame's own pixels.
        let v = one_to_one(6, 5, 4, 3).unwrap();
        let src = src_4x3();
        let oracle = full((6, 5), &v, &src);

        let mut dst = vec![0u32; 30];
        for (x, w) in [(0, 1), (1, 3)] {
            present_region_into(&mut dst, 6, 5, &v, &src, DamageRect { x, y: 0, w, h: 3 });
        }
        assert_eq!(dst, oracle, "the two paths must agree pixel for pixel");
    }

    #[test]
    fn a_region_hanging_off_the_frame_or_the_window_paints_only_what_is_visible() {
        // 3x2 window over a 4x3 frame: the frame is cropped to the window (dest 0,0),
        // and the region overhangs both the frame's right edge and the window's.
        let v = one_to_one(3, 2, 4, 3).unwrap();
        let src = src_4x3();
        let oracle = full((3, 2), &v, &src);

        let mut dst = vec![SENTINEL; 6];
        present_region_into(
            &mut dst,
            3,
            2,
            &v,
            &src,
            DamageRect {
                x: 2,
                y: 1,
                w: 10,
                h: 10,
            },
        );

        // Visible part of the region: frame column 2 only (column 3 is off the
        // window), frame row 1 only (row 2 is off the window).
        assert_eq!(dst[3 + 2], oracle[3 + 2], "(2,1) is the one visible pixel");
        for i in [0, 1, 2, 3, 4] {
            assert_eq!(dst[i], SENTINEL, "index {i} is outside the visible region");
        }
    }

    #[test]
    fn a_region_entirely_outside_the_frame_writes_nothing() {
        let v = one_to_one(4, 3, 4, 3).unwrap();
        let src = src_4x3();
        let untouched = vec![SENTINEL; 12];

        for region in [
            DamageRect {
                x: 4,
                y: 0,
                w: 2,
                h: 2,
            },
            DamageRect {
                x: 0,
                y: 3,
                w: 2,
                h: 2,
            },
            DamageRect {
                x: 0,
                y: 0,
                w: 0,
                h: 2,
            },
        ] {
            let mut dst = untouched.clone();
            present_region_into(&mut dst, 4, 3, &v, &src, region);
            assert_eq!(dst, untouched, "{region:?} has no visible pixels");
        }
    }

    #[test]
    fn a_scaled_viewport_is_refused_rather_than_converted_without_the_resampling() {
        // The function has no column table; painting a letterboxed viewport 1:1 would
        // put the right pixels in the wrong places, which is worse than not painting.
        let scaled = Viewport::letterbox(8, 6, 4, 3);
        assert_ne!(scaled.dest_width, 4, "the fixture really is scaled");
        let untouched = vec![SENTINEL; 48];
        let mut dst = untouched.clone();
        present_region_into(
            &mut dst,
            8,
            6,
            &scaled,
            &src_4x3(),
            DamageRect {
                x: 0,
                y: 0,
                w: 4,
                h: 3,
            },
        );
        assert_eq!(dst, untouched);
    }
}
