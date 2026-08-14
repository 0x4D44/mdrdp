//! Surface storage and compositing for the EGFX graphics pipeline.
//!
//! `ironrdp-egfx` tracks surfaces as **metadata only** — it holds no pixels, and
//! `SolidFill`, `SurfaceToSurface`, `SurfaceToCache` and `CacheToSurface` are all
//! no-op handler callbacks. So the pixel store, the blits and the offscreen cache are
//! ours to implement. This module is that store.
//!
//! Everything here is pure logic over byte buffers: no network, no decoder, no window.
//! That is deliberate — it is the part most likely to harbour off-by-one and clipping
//! bugs, and it can be tested exhaustively without a server.
//!
//! **Pixel format is RGBA8**, four bytes per pixel, top-down, tightly packed. The
//! ClearCodec decoder emits BGRA, so the caller converts on the way in; keeping one
//! format in the store means the presenter never has to care which codec produced a
//! region.

use std::collections::HashMap;

/// Bytes per pixel, everywhere in this module.
pub const BPP: usize = 4;

/// A rectangle in surface coordinates, `right`/`bottom` exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: u16,
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
}

impl Rect {
    pub fn new(left: u16, top: u16, right: u16, bottom: u16) -> Self {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn width(&self) -> u16 {
        self.right.saturating_sub(self.left)
    }

    pub fn height(&self) -> u16 {
        self.bottom.saturating_sub(self.top)
    }

    pub fn is_empty(&self) -> bool {
        self.width() == 0 || self.height() == 0
    }

    /// Clip to a surface of `width` x `height`, returning `None` if nothing remains.
    ///
    /// A server may legitimately send a rectangle that overhangs the surface — tile
    /// grids do not divide evenly — so clipping is the normal path, not an error path.
    pub fn clip_to(&self, width: u16, height: u16) -> Option<Rect> {
        let r = Rect {
            left: self.left.min(width),
            top: self.top.min(height),
            right: self.right.min(width),
            bottom: self.bottom.min(height),
        };
        (!r.is_empty()).then_some(r)
    }
}

/// One surface: a flat RGBA buffer with its dimensions.
#[derive(Debug, Clone)]
pub struct Surface {
    pub width: u16,
    pub height: u16,
    pixels: Vec<u8>,
}

impl Surface {
    pub fn new(width: u16, height: u16) -> Self {
        Surface {
            width,
            height,
            pixels: vec![0u8; width as usize * height as usize * BPP],
        }
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    fn row_start(&self, y: u16) -> usize {
        y as usize * self.width as usize * BPP
    }

    /// Copy RGBA rows into `dest`.
    ///
    /// `src` rows are `src_stride_px` pixels wide — **the width the producer used**, not
    /// the width that survives clipping. Those differ whenever the destination overhangs
    /// the surface, which is the normal case for tile grids that do not divide evenly.
    /// Reading clipped rows at the clipped stride shears the image diagonally and
    /// reports no error at all, so the stride is an explicit argument rather than
    /// something inferred.
    pub fn blit_rgba(
        &mut self,
        dest: Rect,
        src: &[u8],
        src_stride_px: u16,
    ) -> Result<(), SurfaceError> {
        let Some(clipped) = dest.clip_to(self.width, self.height) else {
            return Ok(());
        };
        let stride_bytes = src_stride_px as usize * BPP;
        let row_bytes = clipped.width() as usize * BPP;

        // Where the surviving region starts inside the source.
        let skip_rows = (clipped.top - dest.top) as usize;
        let skip_cols_bytes = (clipped.left - dest.left) as usize * BPP;

        let needed = skip_rows * stride_bytes
            + skip_cols_bytes
            + row_bytes
            + (clipped.height().saturating_sub(1) as usize) * stride_bytes;
        if src.len() < needed {
            return Err(SurfaceError::ShortSource {
                needed,
                got: src.len(),
            });
        }

        for row in 0..clipped.height() {
            let src_off = (skip_rows + row as usize) * stride_bytes + skip_cols_bytes;
            let dst_off = self.row_start(clipped.top + row) + clipped.left as usize * BPP;
            self.pixels[dst_off..dst_off + row_bytes]
                .copy_from_slice(&src[src_off..src_off + row_bytes]);
        }
        Ok(())
    }

    /// Fill a rectangle with one RGBA colour.
    pub fn fill(&mut self, dest: Rect, rgba: [u8; 4]) {
        let Some(dest) = dest.clip_to(self.width, self.height) else {
            return;
        };
        for row in dest.top..dest.bottom {
            let start = self.row_start(row) + dest.left as usize * BPP;
            let end = start + dest.width() as usize * BPP;
            for px in self.pixels[start..end].chunks_exact_mut(BPP) {
                px.copy_from_slice(&rgba);
            }
        }
    }

    /// Extract a rectangle as a tightly packed RGBA buffer.
    pub fn extract(&self, src: Rect) -> Option<Vec<u8>> {
        let src = src.clip_to(self.width, self.height)?;
        let row_bytes = src.width() as usize * BPP;
        let mut out = Vec::with_capacity(row_bytes * src.height() as usize);
        for row in src.top..src.bottom {
            let start = self.row_start(row) + src.left as usize * BPP;
            out.extend_from_slice(&self.pixels[start..start + row_bytes]);
        }
        Some(out)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceError {
    NoSuchSurface(u16),
    NoSuchCacheSlot(u16),
    ShortSource { needed: usize, got: usize },
}

impl std::fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SurfaceError::NoSuchSurface(id) => write!(f, "no surface with id {id}"),
            SurfaceError::NoSuchCacheSlot(slot) => write!(f, "no cache slot {slot}"),
            SurfaceError::ShortSource { needed, got } => {
                write!(
                    f,
                    "source buffer too small: needed {needed} bytes, got {got}"
                )
            }
        }
    }
}

impl std::error::Error for SurfaceError {}

/// A cached bitmap: pixels plus the size they were captured at.
#[derive(Debug, Clone)]
struct CacheEntry {
    width: u16,
    height: u16,
    pixels: Vec<u8>,
}

/// All surfaces, the offscreen cache, and which surface is mapped to output.
#[derive(Debug, Default)]
pub struct SurfaceStore {
    surfaces: HashMap<u16, Surface>,
    cache: HashMap<u16, CacheEntry>,
    output: Option<u16>,
    /// Bumped on every mutation, so a presenter can tell "changed" from "unchanged"
    /// without comparing buffers.
    generation: u64,
}

impl SurfaceStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn touch(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn create(&mut self, id: u16, width: u16, height: u16) {
        self.surfaces.insert(id, Surface::new(width, height));
        self.touch();
    }

    pub fn delete(&mut self, id: u16) {
        self.surfaces.remove(&id);
        if self.output == Some(id) {
            self.output = None;
        }
        self.touch();
    }

    pub fn get(&self, id: u16) -> Option<&Surface> {
        self.surfaces.get(&id)
    }

    /// Drop every cached bitmap. Used when the handler resets its own mirror, so the
    /// two cannot disagree about which slots exist.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.touch();
    }

    pub fn map_to_output(&mut self, id: u16) {
        self.output = Some(id);
        self.touch();
    }

    /// The surface currently mapped to output — what the window should draw.
    pub fn output_surface(&self) -> Option<&Surface> {
        self.output.and_then(|id| self.surfaces.get(&id))
    }

    pub fn blit_rgba(
        &mut self,
        id: u16,
        dest: Rect,
        src: &[u8],
        src_stride_px: u16,
    ) -> Result<(), SurfaceError> {
        let surface = self
            .surfaces
            .get_mut(&id)
            .ok_or(SurfaceError::NoSuchSurface(id))?;
        surface.blit_rgba(dest, src, src_stride_px)?;
        self.touch();
        Ok(())
    }

    pub fn solid_fill(
        &mut self,
        id: u16,
        rects: &[Rect],
        rgba: [u8; 4],
    ) -> Result<(), SurfaceError> {
        let surface = self
            .surfaces
            .get_mut(&id)
            .ok_or(SurfaceError::NoSuchSurface(id))?;
        for rect in rects {
            surface.fill(*rect, rgba);
        }
        self.touch();
        Ok(())
    }

    /// Copy a rectangle from one surface to one or more destinations.
    ///
    /// Source and destination may be the same surface, so the source region is
    /// extracted before any write — otherwise overlapping copies corrupt themselves.
    pub fn surface_to_surface(
        &mut self,
        src_id: u16,
        src_rect: Rect,
        dest_id: u16,
        dest_points: &[(u16, u16)],
    ) -> Result<(), SurfaceError> {
        let src_surface = self
            .surfaces
            .get(&src_id)
            .ok_or(SurfaceError::NoSuchSurface(src_id))?;
        let Some(pixels) = src_surface.extract(src_rect) else {
            return Ok(());
        };
        let (w, h) = (src_rect.width(), src_rect.height());

        let dest = self
            .surfaces
            .get_mut(&dest_id)
            .ok_or(SurfaceError::NoSuchSurface(dest_id))?;
        for (x, y) in dest_points {
            // Saturating, not wrapping: a destination that runs off the right edge is
            // clipped by blit_rgba. `x + w` on u16 panics in debug and wraps in release,
            // and a panic here poisons the store mutex and takes the session with it.
            let rect = Rect::new(*x, *y, x.saturating_add(w), y.saturating_add(h));
            dest.blit_rgba(rect, &pixels, w)?;
        }
        self.touch();
        Ok(())
    }

    pub fn surface_to_cache(
        &mut self,
        src_id: u16,
        src_rect: Rect,
        slot: u16,
    ) -> Result<(), SurfaceError> {
        let surface = self
            .surfaces
            .get(&src_id)
            .ok_or(SurfaceError::NoSuchSurface(src_id))?;
        // Record the CLIPPED dimensions: `extract` clips, so storing the requested
        // width would make every later cache_to_surface ask for more bytes than exist
        // and fail with ShortSource — silently dropping the cached region.
        let Some(clipped) = src_rect.clip_to(surface.width, surface.height) else {
            return Ok(());
        };
        let Some(pixels) = surface.extract(clipped) else {
            return Ok(());
        };
        self.cache.insert(
            slot,
            CacheEntry {
                width: clipped.width(),
                height: clipped.height(),
                pixels,
            },
        );
        self.touch();
        Ok(())
    }

    pub fn cache_to_surface(
        &mut self,
        slot: u16,
        dest_id: u16,
        dest_points: &[(u16, u16)],
    ) -> Result<(), SurfaceError> {
        let entry = self
            .cache
            .get(&slot)
            .ok_or(SurfaceError::NoSuchCacheSlot(slot))?
            .clone();
        let dest = self
            .surfaces
            .get_mut(&dest_id)
            .ok_or(SurfaceError::NoSuchSurface(dest_id))?;
        for (x, y) in dest_points {
            let rect = Rect::new(
                *x,
                *y,
                x.saturating_add(entry.width),
                y.saturating_add(entry.height),
            );
            dest.blit_rgba(rect, &entry.pixels, entry.width)?;
        }
        self.touch();
        Ok(())
    }

    /// Convert BGRA (what the ClearCodec decoder emits) to RGBA in place.
    pub fn bgra_to_rgba_in_place(buf: &mut [u8]) {
        for px in buf.chunks_exact_mut(BPP) {
            px.swap(0, 2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u16, h: u16, rgba: [u8; 4]) -> Vec<u8> {
        rgba.iter()
            .copied()
            .cycle()
            .take(w as usize * h as usize * BPP)
            .collect()
    }

    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];

    #[test]
    fn a_new_surface_is_transparent_black() {
        let s = Surface::new(4, 3);
        assert_eq!(s.pixels().len(), 4 * 3 * BPP);
        assert!(s.pixels().iter().all(|&b| b == 0));
    }

    #[test]
    fn blit_lands_at_the_right_offset_and_stride() {
        // The classic bug: writing rect-width rows at surface-width stride, or vice
        // versa, which skews the image. Blit a 2x2 red block at (1,1) of a 4x4 surface
        // and check exactly which pixels changed.
        let mut s = Surface::new(4, 4);
        s.blit_rgba(Rect::new(1, 1, 3, 3), &solid(2, 2, RED), 2)
            .unwrap();

        for y in 0..4u16 {
            for x in 0..4u16 {
                let off = (y as usize * 4 + x as usize) * BPP;
                let px = &s.pixels()[off..off + BPP];
                let inside = (1..3).contains(&x) && (1..3).contains(&y);
                if inside {
                    assert_eq!(px, RED, "({x},{y}) should be red");
                } else {
                    assert_eq!(px, [0, 0, 0, 0], "({x},{y}) should be untouched");
                }
            }
        }
    }

    #[test]
    fn a_rectangle_overhanging_the_surface_is_clipped_not_rejected() {
        // Tile grids do not divide evenly, so overhang is the normal path.
        let mut s = Surface::new(4, 4);
        s.blit_rgba(Rect::new(2, 2, 6, 6), &solid(4, 4, RED), 4)
            .expect("overhang must clip, not error");
        let off = (3 * 4 + 3) * BPP;
        assert_eq!(&s.pixels()[off..off + BPP], RED);
    }

    /// Per-pixel-distinct source, so a wrong stride is visible.
    fn ramp(w: u16, h: u16) -> Vec<u8> {
        let mut v = Vec::with_capacity(w as usize * h as usize * BPP);
        for y in 0..h {
            for x in 0..w {
                v.extend_from_slice(&[x as u8, y as u8, 0, 255]);
            }
        }
        v
    }

    #[test]
    fn an_overhanging_blit_reads_the_source_at_its_own_stride() {
        // The bug this guards: clipping the destination and then reading the source at
        // the CLIPPED width. Row 1 then starts in the middle of row 0 and the image
        // shears diagonally, with no error reported.
        //
        // A uniform-coloured source cannot detect this — which is exactly why the
        // original version of this test missed it. Use distinct per-pixel values.
        let mut s = Surface::new(4, 2);
        // 4x2 tile placed at x=2: only its left 2 columns survive.
        s.blit_rgba(Rect::new(2, 0, 6, 2), &ramp(4, 2), 4).unwrap();

        // Surviving pixels must be source columns 0..2 of the matching row.
        for (row, y) in [(0u16, 0u8), (1, 1)] {
            for col in 0..2u16 {
                let off = (row as usize * 4 + 2 + col as usize) * BPP;
                assert_eq!(
                    &s.pixels()[off..off + BPP],
                    &[col as u8, y, 0, 255],
                    "row {row} col {col} came from the wrong source offset (stride bug)"
                );
            }
        }
    }

    #[test]
    fn a_blit_clipped_on_the_left_and_top_skips_into_the_source() {
        // Negative-origin equivalent: dest starts before the surface, so the visible
        // region begins partway into the source rather than at its first byte.
        let mut s = Surface::new(2, 2);
        s.blit_rgba(Rect::new(0, 0, 4, 4), &ramp(4, 4), 4).unwrap();
        // Top-left of the surface is source (0,0); (1,1) is source (1,1).
        assert_eq!(&s.pixels()[0..BPP], &[0, 0, 0, 255]);
        let off = (2 + 1) * BPP; // row 1, col 1 of a 2-wide surface
        assert_eq!(&s.pixels()[off..off + BPP], &[1, 1, 0, 255]);
    }

    #[test]
    fn a_short_source_buffer_is_an_error_not_a_panic() {
        let mut s = Surface::new(4, 4);
        let err = s
            .blit_rgba(Rect::new(0, 0, 4, 4), &[0u8; 8], 4)
            .unwrap_err();
        assert!(matches!(err, SurfaceError::ShortSource { .. }));
    }

    #[test]
    fn fill_covers_exactly_the_rectangle() {
        let mut s = Surface::new(3, 3);
        s.fill(Rect::new(0, 0, 3, 1), BLUE);
        assert_eq!(&s.pixels()[0..BPP], BLUE);
        assert_eq!(&s.pixels()[2 * BPP..3 * BPP], BLUE);
        assert_eq!(&s.pixels()[3 * BPP..4 * BPP], [0, 0, 0, 0]);
    }

    #[test]
    fn extract_round_trips_through_blit() {
        let mut s = Surface::new(4, 4);
        s.blit_rgba(Rect::new(1, 1, 3, 3), &solid(2, 2, RED), 2)
            .unwrap();
        let taken = s.extract(Rect::new(1, 1, 3, 3)).unwrap();
        assert_eq!(taken, solid(2, 2, RED));
    }

    #[test]
    fn surface_to_surface_on_itself_does_not_corrupt_overlapping_regions() {
        // Source is extracted before any write; without that, an overlapping copy
        // reads pixels it has already overwritten.
        let mut store = SurfaceStore::new();
        store.create(1, 4, 1);
        store
            .blit_rgba(1, Rect::new(0, 0, 2, 1), &solid(2, 1, RED), 2)
            .unwrap();
        store
            .surface_to_surface(1, Rect::new(0, 0, 2, 1), 1, &[(1, 0)])
            .unwrap();

        let s = store.get(1).unwrap();
        assert_eq!(&s.pixels()[BPP..2 * BPP], RED, "copied pixel");
        assert_eq!(&s.pixels()[2 * BPP..3 * BPP], RED, "copied pixel");
    }

    #[test]
    fn cache_round_trip_preserves_pixels() {
        let mut store = SurfaceStore::new();
        store.create(1, 4, 4);
        store.create(2, 4, 4);
        store
            .blit_rgba(1, Rect::new(0, 0, 2, 2), &solid(2, 2, BLUE), 2)
            .unwrap();
        store.surface_to_cache(1, Rect::new(0, 0, 2, 2), 7).unwrap();
        store.cache_to_surface(7, 2, &[(2, 2)]).unwrap();

        let dest = store.get(2).unwrap();
        let off = (2 * 4 + 2) * BPP;
        assert_eq!(&dest.pixels()[off..off + BPP], BLUE);
    }

    #[test]
    fn operations_on_a_missing_surface_are_errors_not_panics() {
        let mut store = SurfaceStore::new();
        assert_eq!(
            store.blit_rgba(9, Rect::new(0, 0, 1, 1), &solid(1, 1, RED), 1),
            Err(SurfaceError::NoSuchSurface(9))
        );
        assert_eq!(
            store.cache_to_surface(3, 9, &[(0, 0)]),
            Err(SurfaceError::NoSuchCacheSlot(3))
        );
    }

    #[test]
    fn generation_changes_on_mutation_so_the_presenter_can_skip_redraws() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 2);
        let g = store.generation();
        store.solid_fill(1, &[Rect::new(0, 0, 1, 1)], RED).unwrap();
        assert_ne!(store.generation(), g);
    }

    #[test]
    fn output_surface_follows_the_mapping() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 2);
        assert!(store.output_surface().is_none(), "nothing mapped yet");
        store.map_to_output(1);
        assert!(store.output_surface().is_some());
        store.delete(1);
        assert!(
            store.output_surface().is_none(),
            "deleting clears the mapping"
        );
    }

    #[test]
    fn bgra_to_rgba_swaps_only_the_colour_channels() {
        let mut buf = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        SurfaceStore::bgra_to_rgba_in_place(&mut buf);
        assert_eq!(buf, vec![3, 2, 1, 4, 7, 6, 5, 8]);
    }
}
