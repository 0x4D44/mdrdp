//! Pixel diff — which parts of a captured frame *actually* changed.
//!
//! Increment 3 of the native transport stops trusting the capture API's dirty-rect
//! hint and proves the delta itself. Desktop Duplication reports rects that are
//! conservative (a whole window when one caret blinked) and occasionally larger than
//! the true change; sending them raw wastes the bandwidth the rect fast path exists
//! to save. This module compares the frame against the previous one and returns the
//! regions that differ, so the wire carries pixels that really moved.
//!
//! This is the portable half: it takes two already-mapped BGRA surfaces and does
//! arithmetic. The D3D11 staging copy and the map/unmap live on the Windows side and
//! call in here; nothing below touches a platform API, so it is written and tested on
//! macOS.
//!
//! **The invariant everything else rests on: every differing pixel inside `region`
//! lies inside some returned rect.** The viewer paints only what it is sent, so an
//! under-reported rect is a permanently stale patch of desktop that no later frame
//! repairs (the next diff sees the *server's* history, not the viewer's). Over-
//! reporting merely costs bytes. Every design choice here — the whole-tile
//! granularity, the coalescing, the refusal to early-exit — leans that way on purpose.
//!
//! The grid is `TILE_W` x `TILE_H` and is aligned to the **desktop** origin, not to
//! the region being examined, so the same pixel always falls in the same tile whatever
//! rect the capture API happened to report. 64x16 is a compromise: wide enough that a
//! row comparison is one long `memcmp` rather than many short ones, short enough that
//! a one-line caret does not drag a 64-pixel-tall band onto the wire.

/// Bytes per pixel. Both surfaces are BGRA8; nothing here interprets the channels.
const BPP: usize = 4;

/// Tile width in pixels. See the module docs for why 64x16.
pub const TILE_W: u32 = 64;
/// Tile height in pixels.
pub const TILE_H: u32 = 16;

/// A rectangle in desktop pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// One mapped surface: its bytes and the row pitch they are laid out with.
///
/// Exists so the tile comparison takes two of these instead of four loose
/// arguments that could be transposed at the call site.
#[derive(Clone, Copy)]
struct Mapped<'a> {
    bytes: &'a [u8],
    pitch: usize,
}

/// Compare two BGRA surfaces over `region` on the desktop-aligned `TILE_W` x
/// `TILE_H` grid and coalesce the changed tiles into rects.
///
/// `cur_pitch` / `prev_pitch` are bytes per row and may exceed `width * 4` — a
/// D3D11 staging map routinely pads. **Only the `width * 4` pixel bytes of each row
/// are compared.** The padding is uninitialized memory that differs between two maps
/// of the same picture, so comparing it would report every frame as fully changed.
///
/// Only tiles intersecting `region` are examined, and each is clipped to
/// `region ∩ desktop` before comparison: a pixel that differs outside `region` is
/// never reported. That is the point of the parameter — the caller has already
/// established the true delta is inside it, and scanning less is the saving.
///
/// Returns `None` when the coalesced result exceeds `max_rects`, which the caller
/// treats as "diff miss — take the full codec path" rather than as an error. `None`
/// also comes back when either slice is too short for the geometry it was described
/// with, for the same reason: an unverifiable diff must never be reported as a clean
/// one. `Some(vec)` otherwise — possibly **empty**, meaning nothing inside `region`
/// differs. Empty is a real answer and is not the same as `None`.
///
/// Rects come back sorted by `(y, x)` so a caller's logs and tests are stable.
// The eight parameters are the contract the Windows capture side codes against —
// two surfaces, their pitches, the geometry, the window, the budget. Bundling them
// into a struct would only move the same six values one indirection away.
#[allow(clippy::too_many_arguments)]
pub fn diff_rects(
    cur: &[u8],
    cur_pitch: usize,
    prev: &[u8],
    prev_pitch: usize,
    width: u32,
    height: u32,
    region: Region,
    max_rects: usize,
) -> Option<Vec<Region>> {
    // Clip to the desktop. Callers pass a region already clipped, but a region that
    // hangs off the edge would otherwise index past the end of a row and into the
    // next one, silently comparing the wrong pixels.
    let x0 = region.x.min(width);
    let y0 = region.y.min(height);
    let x1 = region.x.saturating_add(region.w).min(width);
    let y1 = region.y.saturating_add(region.h).min(height);
    if x0 >= x1 || y0 >= y1 {
        // Empty window: nothing inside it can differ. This is the "no change"
        // answer, not a miss.
        return Some(Vec::new());
    }

    let cur = Mapped {
        bytes: cur,
        pitch: cur_pitch,
    };
    let prev = Mapped {
        bytes: prev,
        pitch: prev_pitch,
    };
    if !covers(cur, width, height) || !covers(prev, width, height) {
        return None;
    }

    let col0 = x0 / TILE_W;
    let col1 = (x1 - 1) / TILE_W;
    let row0 = y0 / TILE_H;
    let row1 = (y1 - 1) / TILE_H;

    let mut out: Vec<Region> = Vec::new();
    // Runs from the previous tile row that a run in this row may still extend.
    let mut open: Vec<Region> = Vec::new();
    // Hoisted only to keep one allocation across tile rows; `drain` empties it.
    let mut runs: Vec<Region> = Vec::new();

    for tile_row in row0..=row1 {
        let ty0 = (tile_row * TILE_H).max(y0);
        let ty1 = ((tile_row + 1) * TILE_H).min(y1);

        // Horizontal pass: consecutive changed tiles become one run. They are
        // adjacent by construction — the loop resets on the first unchanged tile —
        // and their clipped pixel spans meet exactly, because a shared tile edge
        // between two examined tiles always lies strictly inside [x0, x1).
        let mut run: Option<(u32, u32)> = None;
        for tile_col in col0..=col1 {
            let tx0 = (tile_col * TILE_W).max(x0);
            let tx1 = ((tile_col + 1) * TILE_W).min(x1);
            if tile_differs(cur, prev, tx0, tx1, ty0, ty1) {
                match run.as_mut() {
                    Some(r) => r.1 = tx1,
                    None => run = Some((tx0, tx1)),
                }
            } else if let Some((start, end)) = run.take() {
                runs.push(Region {
                    x: start,
                    y: ty0,
                    w: end - start,
                    h: ty1 - ty0,
                });
            }
        }
        if let Some((start, end)) = run.take() {
            runs.push(Region {
                x: start,
                y: ty0,
                w: end - start,
                h: ty1 - ty0,
            });
        }

        // Vertical pass: a run directly beneath one of identical x and w extends it.
        // Anything not extended can never be extended again — tile rows only move
        // downwards — so it is final.
        let mut next_open: Vec<Region> = Vec::with_capacity(runs.len());
        for r in runs.drain(..) {
            match open
                .iter()
                .position(|o| o.x == r.x && o.w == r.w && o.y + o.h == r.y)
            {
                Some(pos) => {
                    let mut merged = open.swap_remove(pos);
                    merged.h += r.h;
                    next_open.push(merged);
                }
                None => next_open.push(r),
            }
        }
        out.append(&mut open);
        open = next_open;
    }
    out.append(&mut open);

    // Counted after coalescing, not during: an early exit that stopped scanning
    // would have to guess at the tiles it never looked at, and the only safe guess
    // is "changed", which is the full-frame path anyway.
    if out.len() > max_rects {
        return None;
    }
    out.sort_unstable_by_key(|r| (r.y, r.x));
    Some(out)
}

/// Pack one rect's BGRA pixels tightly — `r.w * 4` bytes per row, `r.h` rows,
/// top-down — out of a mapped surface with row pitch `pitch`.
///
/// This is the wire layout [`crate::rects::Rect::pixels`] expects, and the reason
/// the diff exists at all: it is the only copy the fast path makes of the pixels.
///
/// `r` must lie inside the surface `pitch` describes; the slicing panics rather
/// than reading a neighbouring row if it does not. Callers pass rects that came
/// from [`diff_rects`] against this same map, which are clipped to the desktop by
/// construction.
pub fn pack_rect(src: &[u8], pitch: usize, r: Region) -> Vec<u8> {
    let row_bytes = r.w as usize * BPP;
    let mut out = Vec::with_capacity(row_bytes * r.h as usize);
    for row in 0..r.h as usize {
        let start = (r.y as usize + row) * pitch + r.x as usize * BPP;
        out.extend_from_slice(&src[start..start + row_bytes]);
    }
    out
}

/// Whether `m` really holds a `width` x `height` picture at its stated pitch.
///
/// The last row needs only `width * 4` bytes, not a full pitch, so the bound is
/// `(height - 1) * pitch + width * 4`. Every arithmetic step is checked: these
/// numbers come from a mapped resource description, and a wrapped product would
/// turn "far too small" into "plenty big enough".
fn covers(m: Mapped<'_>, width: u32, height: u32) -> bool {
    let row_bytes = width as usize * BPP;
    if m.pitch < row_bytes {
        return false;
    }
    let Some(last_row) = (height as usize).checked_sub(1) else {
        return true; // No rows at all; nothing to index.
    };
    match last_row
        .checked_mul(m.pitch)
        .and_then(|o| o.checked_add(row_bytes))
    {
        Some(needed) => m.bytes.len() >= needed,
        None => false,
    }
}

/// Whether any pixel in the half-open box `[x0, x1) x [y0, y1)` differs.
///
/// Row-at-a-time slice equality on purpose: `==` on `[u8]` is `memcmp`, which is
/// vectorised in the standard library. Hand-rolled per-pixel comparison was
/// measured slower everywhere and is not worth the intrinsics.
fn tile_differs(cur: Mapped<'_>, prev: Mapped<'_>, x0: u32, x1: u32, y0: u32, y1: u32) -> bool {
    let first = x0 as usize * BPP;
    let last = x1 as usize * BPP;
    for y in y0..y1 {
        let c = y as usize * cur.pitch;
        let p = y as usize * prev.pitch;
        if cur.bytes[c + first..c + last] != prev.bytes[p + first..p + last] {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A desktop whose dimensions are deliberately *not* multiples of the tile size,
    /// so the partial right and bottom tiles are exercised by every test that uses it.
    /// 200 = 3 full 64-wide columns + 8; 70 = 4 full 16-tall rows + 6.
    const W: u32 = 200;
    const H: u32 = 70;
    /// Padded well past `W * 4`, as a real staging map is.
    const PITCH: usize = W as usize * BPP + 13;

    /// A surface whose every pixel byte encodes its own (x, y): a row-offset error,
    /// a transposed coordinate or a wrong pitch all produce a wrong *value* rather
    /// than an accidental match. `junk` fills the pitch padding — pass a different
    /// value to each of a pair of surfaces to prove the padding is never read.
    fn surface(width: u32, height: u32, pitch: usize, junk: u8) -> Vec<u8> {
        assert!(pitch >= width as usize * BPP);
        let mut buf = vec![junk; pitch * height as usize];
        for y in 0..height {
            for x in 0..width {
                let o = y as usize * pitch + x as usize * BPP;
                buf[o] = x as u8;
                buf[o + 1] = y as u8;
                buf[o + 2] = x.wrapping_mul(7).wrapping_add(y.wrapping_mul(13)) as u8;
                buf[o + 3] = 0xFF;
            }
        }
        buf
    }

    /// Change pixel (x, y) so it cannot equal its counterpart in the other surface.
    fn flip(buf: &mut [u8], pitch: usize, x: u32, y: u32) {
        buf[y as usize * pitch + x as usize * BPP] ^= 0xFF;
    }

    fn whole_desktop() -> Region {
        Region {
            x: 0,
            y: 0,
            w: W,
            h: H,
        }
    }

    /// The standard pair: identical pictures, deliberately different padding.
    fn pair() -> (Vec<u8>, Vec<u8>) {
        (surface(W, H, PITCH, 0x00), surface(W, H, PITCH, 0xAA))
    }

    fn diff(cur: &[u8], prev: &[u8], region: Region, max_rects: usize) -> Option<Vec<Region>> {
        diff_rects(cur, PITCH, prev, PITCH, W, H, region, max_rects)
    }

    fn contains(r: Region, x: u32, y: u32) -> bool {
        x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h
    }

    #[test]
    fn two_identical_surfaces_report_no_changed_rects() {
        let (cur, prev) = pair();
        assert_eq!(diff(&cur, &prev, whole_desktop(), 64), Some(Vec::new()));
    }

    #[test]
    fn junk_in_the_pitch_padding_is_never_compared() {
        // `pair()` differs in every padding byte of every row and in no pixel byte.
        // A diff that compared whole pitch rows would report the entire desktop.
        let (cur, prev) = pair();
        assert_ne!(
            cur[W as usize * BPP..PITCH],
            prev[W as usize * BPP..PITCH],
            "fixture must actually differ in the padding, or this proves nothing"
        );
        assert_eq!(diff(&cur, &prev, whole_desktop(), 64), Some(Vec::new()));
    }

    #[test]
    fn a_single_changed_pixel_yields_one_tile_containing_it() {
        let (mut cur, prev) = pair();
        flip(&mut cur, PITCH, 70, 20); // tile column 1, tile row 1
        let rects = diff(&cur, &prev, whole_desktop(), 64).expect("under the budget");
        assert_eq!(
            rects,
            vec![Region {
                x: 64,
                y: 16,
                w: 64,
                h: 16
            }]
        );
        assert!(contains(rects[0], 70, 20));
    }

    #[test]
    fn a_change_spanning_a_tile_boundary_becomes_one_merged_rect() {
        let (mut cur, prev) = pair();
        flip(&mut cur, PITCH, 63, 5); // last pixel of column 0
        flip(&mut cur, PITCH, 64, 5); // first pixel of column 1
        let rects = diff(&cur, &prev, whole_desktop(), 64).expect("under the budget");
        assert_eq!(
            rects,
            vec![Region {
                x: 0,
                y: 0,
                w: 128,
                h: 16
            }],
            "adjacent changed tiles in one row must coalesce, not ship as two rects"
        );
    }

    #[test]
    fn vertically_adjacent_runs_of_the_same_extent_merge_into_one_rect() {
        let (mut cur, prev) = pair();
        flip(&mut cur, PITCH, 70, 5); // column 1, row 0
        flip(&mut cur, PITCH, 70, 20); // column 1, row 1
        let rects = diff(&cur, &prev, whole_desktop(), 64).expect("under the budget");
        assert_eq!(
            rects,
            vec![Region {
                x: 64,
                y: 0,
                w: 64,
                h: 32
            }]
        );
    }

    #[test]
    fn changed_areas_with_different_x_extents_are_not_merged_vertically() {
        let (mut cur, prev) = pair();
        flip(&mut cur, PITCH, 70, 5); // column 1, row 0
        flip(&mut cur, PITCH, 10, 20); // column 0, row 1 — adjacent, different x
        let rects = diff(&cur, &prev, whole_desktop(), 64).expect("under the budget");
        assert_eq!(
            rects,
            vec![
                Region {
                    x: 64,
                    y: 0,
                    w: 64,
                    h: 16
                },
                Region {
                    x: 0,
                    y: 16,
                    w: 64,
                    h: 16
                },
            ],
            "merging these would claim 64 pixels of unchanged desktop on both rows"
        );
    }

    #[test]
    fn a_difference_outside_the_region_is_not_reported() {
        let (mut cur, prev) = pair();
        flip(&mut cur, PITCH, 150, 40);
        let window = Region {
            x: 0,
            y: 0,
            w: 64,
            h: 16,
        };
        assert_eq!(diff(&cur, &prev, window, 64), Some(Vec::new()));
        // The other half of the claim: the same surfaces do report it when the
        // region covers it, so the empty answer above is the region's doing and not
        // a diff that reports nothing.
        assert_eq!(
            diff(&cur, &prev, whole_desktop(), 64),
            Some(vec![Region {
                x: 128,
                y: 32,
                w: 64,
                h: 16
            }])
        );
    }

    #[test]
    fn more_coalesced_rects_than_max_rects_is_a_diff_miss() {
        let (mut cur, prev) = pair();
        // Four tiles, separated in both axes so nothing coalesces: columns 0 and 2,
        // tile rows 0 and 2.
        for (x, y) in [(10, 5), (150, 5), (10, 40), (150, 40)] {
            flip(&mut cur, PITCH, x, y);
        }
        assert_eq!(diff(&cur, &prev, whole_desktop(), 3), None);
        // Sanity: the fixture really does coalesce to exactly four, so the None
        // above is the budget and not an unrelated refusal.
        let rects = diff(&cur, &prev, whole_desktop(), 4).expect("exactly at the budget");
        assert_eq!(rects.len(), 4);
    }

    #[test]
    fn a_change_in_the_partial_corner_tile_is_found_and_clipped_to_the_desktop() {
        let (mut cur, prev) = pair();
        flip(&mut cur, PITCH, W - 1, H - 1);
        let rects = diff(&cur, &prev, whole_desktop(), 64).expect("under the budget");
        assert_eq!(
            rects,
            vec![Region {
                x: 192,
                y: 64,
                w: 8, // 200 - 192, not a full 64
                h: 6  // 70 - 64, not a full 16
            }]
        );
    }

    #[test]
    fn a_region_hanging_off_the_desktop_is_clipped_before_anything_is_read() {
        let (mut cur, prev) = pair();
        flip(&mut cur, PITCH, W - 1, H - 1);
        let window = Region {
            x: 180,
            y: 60,
            w: 100, // 180 + 100 = 280, well past 200
            h: 100, // 60 + 100 = 160, well past 70
        };
        let rects = diff(&cur, &prev, window, 64).expect("under the budget");
        assert_eq!(
            rects,
            vec![Region {
                x: 192,
                y: 64,
                w: 8,
                h: 6
            }]
        );
    }

    #[test]
    fn a_degenerate_region_reports_no_change_rather_than_a_miss() {
        let (mut cur, prev) = pair();
        flip(&mut cur, PITCH, 10, 10);
        for window in [
            Region {
                x: 10,
                y: 10,
                w: 0,
                h: 10,
            },
            Region {
                x: 10,
                y: 10,
                w: 10,
                h: 0,
            },
            Region {
                x: 500,
                y: 500,
                w: 10,
                h: 10,
            },
        ] {
            assert_eq!(
                diff(&cur, &prev, window, 64),
                Some(Vec::new()),
                "{window:?} encloses no pixels, so it encloses no changed pixels"
            );
        }
    }

    #[test]
    fn a_surface_shorter_than_its_declared_geometry_is_a_diff_miss() {
        let (cur, prev) = pair();
        let short = &cur[..cur.len() - PITCH];
        // Unverifiable must read as "miss", never as "clean": reporting no change
        // for a surface we could not read is exactly the stale-canvas failure.
        assert_eq!(
            diff_rects(short, PITCH, &prev, PITCH, W, H, whole_desktop(), 64),
            None
        );
        assert_eq!(
            diff_rects(&cur, PITCH, short, PITCH, W, H, whole_desktop(), 64),
            None
        );
        // A pitch narrower than a row of pixels is the same class of lie.
        assert_eq!(
            diff_rects(
                &cur,
                W as usize * BPP - 1,
                &prev,
                PITCH,
                W,
                H,
                whole_desktop(),
                64
            ),
            None
        );
    }

    #[test]
    fn pack_rect_extracts_exactly_the_rects_rows_from_a_padded_surface() {
        let src = surface(W, H, PITCH, 0xEE);
        let r = Region {
            x: 3,
            y: 2,
            w: 5,
            h: 4,
        };
        let packed = pack_rect(&src, PITCH, r);
        assert_eq!(packed.len(), 5 * 4 * BPP, "tightly packed, no padding");
        for row in 0..r.h {
            for col in 0..r.w {
                let (x, y) = (r.x + col, r.y + row);
                let o = (row as usize * r.w as usize + col as usize) * BPP;
                // Every byte names its own source pixel, so a row-offset or a
                // pitch mistake cannot coincidentally agree.
                assert_eq!(packed[o], x as u8, "B of {x},{y}");
                assert_eq!(packed[o + 1], y as u8, "G of {x},{y}");
                assert_eq!(
                    packed[o + 2],
                    x.wrapping_mul(7).wrapping_add(y.wrapping_mul(13)) as u8,
                    "R of {x},{y}"
                );
                assert_eq!(packed[o + 3], 0xFF, "A of {x},{y}");
            }
        }
    }

    /// A fixed-seed LCG. Deterministic on purpose: a scattered fixture that differs
    /// between runs turns a coverage failure into an unreproducible one, and neither
    /// `rand` nor a clock is worth a dependency here.
    struct Lcg(u64);

    impl Lcg {
        fn next_below(&mut self, bound: u32) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as u32) % bound
        }
    }

    #[test]
    fn every_changed_pixel_inside_the_region_lands_inside_a_returned_rect() {
        // The load-bearing invariant, checked against a scattering the coalescing
        // logic was not written with in mind: 60 pixels flipped anywhere on the
        // desktop, a region that covers only part of it, edges included.
        let mut rng = Lcg(0x5EED_1234_ABCD_0001);
        let (mut cur, prev) = pair();
        let mut flipped = Vec::new();
        for _ in 0..60 {
            let (x, y) = (rng.next_below(W), rng.next_below(H));
            flip(&mut cur, PITCH, x, y);
            flipped.push((x, y));
        }
        let window = Region {
            x: 30,
            y: 5,
            w: 120,
            h: 50,
        };
        let rects = diff(&cur, &prev, window, 1024).expect("budget is generous");

        // Coverage: nothing inside the window may be missed.
        let mut inside = 0;
        for (x, y) in flipped {
            if !contains(window, x, y) {
                continue;
            }
            inside += 1;
            assert!(
                rects.iter().any(|r| contains(*r, x, y)),
                "changed pixel {x},{y} inside {window:?} is in none of {rects:?}"
            );
        }
        assert!(inside > 0, "fixture must flip something inside the window");

        // Containment: nothing outside the window may be claimed. The caller
        // packs and sends whatever comes back, so a rect that strays reads pixels
        // the region promised were not needed.
        for r in &rects {
            assert!(r.w > 0 && r.h > 0, "empty rect {r:?}");
            assert!(
                r.x >= window.x
                    && r.y >= window.y
                    && r.x + r.w <= window.x + window.w
                    && r.y + r.h <= window.y + window.h
                    && r.x + r.w <= W
                    && r.y + r.h <= H,
                "rect {r:?} strays outside {window:?} ∩ the {W}x{H} desktop"
            );
        }
    }
}
