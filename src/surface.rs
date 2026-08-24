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

use crate::stats::CacheStats;
use ironrdp_graphics::clearcodec::MAX_DECODE_DIM;

/// Bytes per pixel, everywhere in this module.
pub const BPP: usize = 4;

/// Exact pixel coverage for a surface incarnation.
///
/// The bitset is allocated only after a partial write. A full-surface write uses the
/// marker without allocating anything, and a bitset is dropped when its last pixel is
/// covered. At one bit per pixel this is 1.76 MiB for a 5120x2880 surface, versus the
/// 56.25 MiB RGBA pixel buffer it describes.
#[derive(Debug, Clone)]
enum Coverage {
    Empty,
    Bits { bits: Vec<u8>, covered: usize },
    Full,
}

impl Coverage {
    fn is_full(&self) -> bool {
        matches!(self, Coverage::Full)
    }

    fn mark_full(&mut self) {
        *self = Coverage::Full;
    }

    fn mask_for_range(start: usize, end: usize) -> u8 {
        debug_assert!(start < end && end <= 8);
        (u8::MAX << start) & (u8::MAX >> (8 - end))
    }

    /// Mark pixels that were successfully written by a clipped or exact operation.
    fn mark_rect(&mut self, width: u16, height: u16, rect: Rect) {
        if self.is_full() {
            return;
        }
        if rect.is_empty() {
            return;
        }

        let total = width as usize * height as usize;
        if rect.left == 0 && rect.top == 0 && rect.right == width && rect.bottom == height {
            *self = Coverage::Full;
            return;
        }
        if total == 0 {
            return;
        }

        if matches!(self, Coverage::Empty) {
            let bytes = total.div_ceil(8);
            *self = Coverage::Bits {
                bits: vec![0; bytes],
                covered: 0,
            };
        }

        let complete = match self {
            Coverage::Bits { bits, covered } => {
                let width = width as usize;
                let left = rect.left as usize;
                let right = rect.right as usize;
                for y in rect.top as usize..rect.bottom as usize {
                    let first_bit = y * width + left;
                    let last_bit = y * width + right;
                    let first_byte = first_bit / 8;
                    let last_byte = (last_bit - 1) / 8;
                    for (offset, byte_bits) in bits[first_byte..=last_byte].iter_mut().enumerate() {
                        let byte = first_byte + offset;
                        let byte_start = byte * 8;
                        let start = first_bit.saturating_sub(byte_start).min(8);
                        let end = last_bit.saturating_sub(byte_start).min(8);
                        let mask = Self::mask_for_range(start, end);
                        let old = *byte_bits;
                        let new = old | mask;
                        *byte_bits = new;
                        *covered += (new ^ old).count_ones() as usize;
                    }
                }
                *covered == total
            }
            Coverage::Full => true,
            Coverage::Empty => false,
        };

        if complete {
            *self = Coverage::Full;
        }
    }
}

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
    painted: bool,
    coverage: Coverage,
}

impl Surface {
    pub fn new(width: u16, height: u16) -> Self {
        Surface {
            width,
            height,
            pixels: vec![0u8; width as usize * height as usize * BPP],
            painted: false,
            coverage: Coverage::Empty,
        }
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    fn is_painted(&self) -> bool {
        self.painted
    }

    fn is_complete(&self) -> bool {
        self.coverage.is_full()
    }

    /// Clone only presentation pixels; fallback surfaces never need protocol coverage.
    fn clone_for_presentation(&self) -> Self {
        Self {
            width: self.width,
            height: self.height,
            pixels: self.pixels.clone(),
            painted: self.painted,
            coverage: Coverage::Full,
        }
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
    ///
    /// Returns the number of bytes actually written — after clipping, which is what the
    /// cache-effectiveness accounting needs. The requested rectangle would overcount
    /// every blit that overhangs the surface.
    pub fn blit_rgba(
        &mut self,
        dest: Rect,
        src: &[u8],
        src_stride_px: u16,
    ) -> Result<usize, SurfaceError> {
        self.blit_rgba_covered(dest, src, src_stride_px, None)
    }

    /// Copy a complete decoded rectangle while marking only the supplied regions as
    /// coverage. The regions are absolute surface coordinates and must be contained by
    /// `dest`; clipping here keeps a malformed caller from proving unrelated pixels.
    fn blit_rgba_covered(
        &mut self,
        dest: Rect,
        src: &[u8],
        src_stride_px: u16,
        coverage: Option<&[Rect]>,
    ) -> Result<usize, SurfaceError> {
        let Some(clipped) = dest.clip_to(self.width, self.height) else {
            return Ok(0);
        };
        let explicit_bytes = coverage.map(|regions| {
            regions.iter().fold(0u64, |bytes, region| {
                let covered = Rect::new(
                    region.left.max(clipped.left),
                    region.top.max(clipped.top),
                    region.right.min(clipped.right),
                    region.bottom.min(clipped.bottom),
                );
                if covered.is_empty() {
                    bytes
                } else {
                    let pixels = u64::from(covered.width()) * u64::from(covered.height());
                    bytes.saturating_add(pixels.saturating_mul(BPP as u64))
                }
            })
        });
        if explicit_bytes == Some(0) {
            return Ok(0);
        }
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

        match coverage {
            None => {
                self.painted = true;
                self.coverage.mark_rect(self.width, self.height, clipped);
            }
            Some(regions) => {
                let mut marked = false;
                for region in regions {
                    let covered = Rect::new(
                        region.left.max(clipped.left),
                        region.top.max(clipped.top),
                        region.right.min(clipped.right),
                        region.bottom.min(clipped.bottom),
                    );
                    if covered.is_empty() {
                        continue;
                    }
                    self.coverage.mark_rect(self.width, self.height, covered);
                    marked = true;
                }
                if marked {
                    self.painted = true;
                }
            }
        }
        let copied_bytes = row_bytes * clipped.height() as usize;
        Ok(explicit_bytes.map_or(copied_bytes, |bytes| {
            usize::try_from(bytes).unwrap_or(usize::MAX)
        }))
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
        self.painted = true;
        self.coverage.mark_rect(self.width, self.height, dest);
    }

    /// Replace the pixel buffer wholesale with a caller-produced one, returning the
    /// old buffer for reuse.
    ///
    /// This is the native transport's AU path: the decoder hands over a full frame it
    /// already owns, and swapping `Vec`s costs nothing where a `blit_rgba` of the same
    /// frame would copy ~8 MB under the store lock on every decoded frame — a cost the
    /// spike's baselines deliberately never paid. The buffer must be exactly
    /// `width * height * 4` bytes; anything else is a producer bug or a mid-stream
    /// mode change, both of which the native session treats as terminal.
    pub fn adopt_pixels(&mut self, pixels: Vec<u8>) -> Result<Vec<u8>, SurfaceError> {
        let expected = self.width as usize * self.height as usize * BPP;
        if pixels.len() != expected {
            return Err(SurfaceError::SizeMismatch {
                expected,
                got: pixels.len(),
            });
        }
        self.painted = true;
        self.coverage.mark_full();
        Ok(std::mem::replace(&mut self.pixels, pixels))
    }

    /// Blit a tightly packed RGBA rectangle with strict wire bounds.
    ///
    /// Decoded native tiles have an exact advertised extent. Clipping one would
    /// hide a decoder or protocol mismatch, so both the rectangle and payload
    /// must match exactly.
    fn validate_strict(&self, dest: Rect, src: &[u8]) -> Result<(), SurfaceError> {
        if dest.right > self.width || dest.bottom > self.height || dest.is_empty() {
            return Err(SurfaceError::OutOfBounds {
                rect: dest,
                width: self.width,
                height: self.height,
            });
        }
        let row_bytes = dest.width() as usize * BPP;
        let expected = row_bytes * dest.height() as usize;
        if src.len() != expected {
            return Err(SurfaceError::SizeMismatch {
                expected,
                got: src.len(),
            });
        }
        Ok(())
    }

    pub fn blit_rgba_strict(&mut self, dest: Rect, src: &[u8]) -> Result<(), SurfaceError> {
        self.validate_strict(dest, src)?;
        let row_bytes = dest.width() as usize * BPP;
        for row in 0..dest.height() {
            let src_off = row as usize * row_bytes;
            let dst_off = self.row_start(dest.top + row) + dest.left as usize * BPP;
            self.pixels[dst_off..dst_off + row_bytes]
                .copy_from_slice(&src[src_off..src_off + row_bytes]);
        }
        self.painted = true;
        self.coverage.mark_rect(self.width, self.height, dest);
        Ok(())
    }

    /// Blit a tightly packed **BGRA** rectangle, swizzling to RGBA in place.
    ///
    /// The native transport's rect path: wire payloads arrive BGRA (the capture
    /// format) and are converted during the copy, with no intermediate allocation.
    /// Unlike [`Surface::blit_rgba`], bounds are **strict**: the native wire's rects
    /// are validated against the advertised frame size before they get here, so an
    /// overhanging rectangle is a protocol violation, not tile-grid slack.
    pub fn blit_bgra_strict(&mut self, dest: Rect, src: &[u8]) -> Result<(), SurfaceError> {
        self.validate_strict(dest, src)?;
        let row_px = dest.width() as usize;
        for row in 0..dest.height() {
            let src_off = row as usize * row_px * BPP;
            let dst_off = self.row_start(dest.top + row) + dest.left as usize * BPP;
            let src_row = &src[src_off..src_off + row_px * BPP];
            let dst_row = &mut self.pixels[dst_off..dst_off + row_px * BPP];
            for (s, d) in src_row.chunks_exact(BPP).zip(dst_row.chunks_exact_mut(BPP)) {
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
                d[3] = s[3];
            }
        }
        self.painted = true;
        self.coverage.mark_rect(self.width, self.height, dest);
        Ok(())
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

    /// Extract a full requested rectangle, zero-filling the part outside this surface.
    ///
    /// ClearCodec decodes over a buffer whose stride is the requested rectangle's width,
    /// even when the rectangle overhangs the surface. A clipped, tightly packed extract
    /// has the wrong length and is discarded by the decoder, so copy the visible rows at
    /// their offset inside the full requested extent.
    pub(crate) fn extract_with_zero_padding(&self, src: Rect) -> Option<Vec<u8>> {
        if src.width() > MAX_DECODE_DIM || src.height() > MAX_DECODE_DIM {
            return None;
        }
        let width = src.width() as usize;
        let height = src.height() as usize;
        let len = width.checked_mul(height)?.checked_mul(BPP)?;
        let mut out = Vec::new();
        out.try_reserve_exact(len).ok()?;
        out.resize(len, 0);

        let Some(clipped) = src.clip_to(self.width, self.height) else {
            return Some(out);
        };
        let src_row_bytes = clipped.width() as usize * BPP;
        let dst_stride_bytes = width * BPP;
        let dst_left_bytes = (clipped.left - src.left) as usize * BPP;
        let dst_top = (clipped.top - src.top) as usize;
        for row in 0..clipped.height() {
            let src_start = self.row_start(clipped.top + row) + clipped.left as usize * BPP;
            let dst_start = (dst_top + usize::from(row)) * dst_stride_bytes + dst_left_bytes;
            out[dst_start..dst_start + src_row_bytes]
                .copy_from_slice(&self.pixels[src_start..src_start + src_row_bytes]);
        }
        Some(out)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceError {
    NoSuchSurface(u16),
    NoSuchCacheSlot(u16),
    ShortSource {
        needed: usize,
        got: usize,
    },
    /// A buffer whose length does not match what the operation requires exactly.
    SizeMismatch {
        expected: usize,
        got: usize,
    },
    /// A strict-bounds blit whose rectangle does not fit the surface.
    OutOfBounds {
        rect: Rect,
        width: u16,
        height: u16,
    },
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
            SurfaceError::SizeMismatch { expected, got } => {
                write!(
                    f,
                    "buffer size mismatch: expected {expected} bytes, got {got}"
                )
            }
            SurfaceError::OutOfBounds {
                rect,
                width,
                height,
            } => {
                write!(
                    f,
                    "rect {},{}..{},{} outside {width}x{height} surface",
                    rect.left, rect.top, rect.right, rect.bottom
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OutputMapping {
    surface_id: u16,
    source_width: u16,
    source_height: u16,
    dest_x: u32,
    dest_y: u32,
    dest_width: u32,
    dest_height: u32,
}

#[derive(Debug, Clone)]
struct PresentationFallback {
    surface: Surface,
    mapping: PresentationMapping,
}

/// Geometry needed to place one source surface inside the logical output canvas.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PresentationMapping {
    pub(crate) canvas_width: u16,
    pub(crate) canvas_height: u16,
    pub(crate) source_width: u16,
    pub(crate) source_height: u16,
    pub(crate) dest_x: u32,
    pub(crate) dest_y: u32,
    pub(crate) dest_width: u32,
    pub(crate) dest_height: u32,
}

/// All surfaces, the offscreen cache, and which surface is mapped to output.
#[derive(Debug, Default)]
pub struct SurfaceStore {
    surfaces: HashMap<u16, Surface>,
    cache: HashMap<u16, CacheEntry>,
    output: Option<OutputMapping>,
    /// The mapped numeric ID was recreated, so its geometry belongs to the prior
    /// incarnation until a fresh MapSurface PDU validates the replacement.
    output_mapping_stale: bool,
    graphics_output_size: Option<(u16, u16)>,
    /// Last painted output retained while a newly mapped surface is still empty.
    ///
    /// This is presentation state only. The replacement surface remains zero-initialized,
    /// so no stale pixels can leak into protocol operations or codec reference state.
    presentation_fallback: Option<PresentationFallback>,
    /// Keep the window's already-copied snapshot when a frame commits without a presentable
    /// output. The snapshot lives outside the store; this bit is the only transaction state
    /// needed to retain it without cloning another full surface.
    presentation_suppressed: bool,
    /// Bumped when the visible output changes, so a presenter can tell "changed" from
    /// "unchanged" without comparing buffers. Offscreen and cache state do not belong in
    /// this token: they must not wake or account a presentation.
    ///
    /// Logical-frame boundary for EGFX presentation. Pixels may be mutated in place while
    /// a frame is active, but the presenter must keep its last committed snapshot until the
    /// matching EndFrame. An aborted frame is terminal for presentation because there is no
    /// rollback or second full-surface clone here.
    frame_state: FrameState,
    /// Whether the active frame, or a dirty frame that was aborted, touched presentation
    /// state. Offscreen/cache-only work must not manufacture a redraw at EndFrame.
    frame_visible_dirty: bool,
    generation: u64,
    /// Cache effectiveness, counted where the cache is actually used. Counting it here
    /// rather than in the EGFX handler means it measures what reached the pixels, not
    /// what the protocol asked for.
    cache_stats: CacheStats,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum FrameState {
    #[default]
    Idle,
    Active(u32),
    Aborted,
}

/// Result of asking the store to refresh a reusable presentation snapshot.
///
/// `Retained` is distinct from `Empty`: a frame transaction is in progress (or was
/// aborted), so the caller must keep presenting its existing snapshot rather than turn
/// the window black because the in-place surface is not yet publishable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PresentationCopy {
    Copied,
    Retained,
    Empty,
}

/// A complete, immutable presentation copy of the surface currently mapped to output.
///
/// The network/decode thread owns the store lock only while this snapshot is copied.
/// Scaling, colour conversion, overlays, and the platform present happen after the lock
/// is released, so a 5K scalar conversion cannot block the next decoded tile.
#[derive(Debug, Default)]
pub(crate) struct PresentationSnapshot {
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) pixel_width: u16,
    pub(crate) pixel_height: u16,
    pub(crate) mapping: PresentationMapping,
    pub(crate) pixels: Vec<u8>,
    pub(crate) generation: u64,
}

impl SurfaceStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cache hits, misses and the bytes behind them.
    pub fn cache_stats(&self) -> CacheStats {
        CacheStats {
            entries: self.cache.len() as u64,
            ..self.cache_stats
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn touch_presentation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    fn mark_frame_visible_dirty(&mut self) {
        if !matches!(self.frame_state, FrameState::Idle) {
            self.frame_visible_dirty = true;
        }
    }

    pub(crate) fn begin_frame(&mut self, frame_id: u32) {
        if matches!(self.frame_state, FrameState::Aborted) {
            // An aborted frame has already mutated the live surfaces without a rollback.
            // Keep presentation suppressed rather than letting a later partial frame
            // publish a mixture of pre- and post-abort pixels.
            return;
        }
        if matches!(self.frame_state, FrameState::Idle) {
            self.frame_visible_dirty = false;
        }
        self.frame_state = FrameState::Active(frame_id);
    }

    pub(crate) fn commit_frame(&mut self, frame_id: u32) -> bool {
        if self.frame_state != FrameState::Active(frame_id) {
            return false;
        }
        let frame_visible_dirty = self.frame_visible_dirty;
        let had_fallback = self.presentation_fallback.is_some();
        let had_suppressed_presentation = self.presentation_suppressed;
        let output_complete = self.output_surface().is_some_and(Surface::is_complete);
        self.frame_state = FrameState::Idle;
        self.frame_visible_dirty = false;
        if output_complete {
            self.presentation_fallback = None;
            self.presentation_suppressed = false;
            if had_fallback || had_suppressed_presentation || frame_visible_dirty {
                self.touch_presentation();
            }
        } else if self.output.is_none() {
            // A frame that terminally deletes the mapped surface publishes an empty
            // presentation at the frame boundary. Retaining the fallback here would
            // leave pixels from a surface the server destroyed visible indefinitely.
            self.presentation_fallback = None;
            self.presentation_suppressed = false;
            if had_fallback || had_suppressed_presentation || frame_visible_dirty {
                self.touch_presentation();
            }
        } else if had_suppressed_presentation || (!had_fallback && frame_visible_dirty) {
            self.presentation_suppressed = true;
        }
        true
    }

    pub(crate) fn abort_frame(&mut self) {
        if matches!(self.frame_state, FrameState::Active(_)) {
            self.frame_state = FrameState::Aborted;
        }
    }

    fn retain_painted_output(&mut self) {
        // A partial replacement may already be mapped while the old output is retained.
        // Never replace that known-good fallback with the partial replacement during a
        // second handoff.
        if self.output_mapping_stale
            || self.presentation_fallback.is_some()
            || self.presentation_suppressed
            || !matches!(self.frame_state, FrameState::Idle)
        {
            return;
        }
        if let Some(mapping) = self.output
            && let Some(presentation_mapping) = self.presentation_mapping_for(mapping)
            && let Some(surface) = self
                .surfaces
                .get(&mapping.surface_id)
                .filter(|surface| surface.is_painted())
        {
            self.presentation_fallback = Some(PresentationFallback {
                surface: surface.clone_for_presentation(),
                mapping: presentation_mapping,
            });
        }
    }

    fn finish_surface_mutation(&mut self, id: u16) {
        if !matches!(self.frame_state, FrameState::Idle) {
            let is_current_output = !self.output_mapping_stale
                && self.output.is_some_and(|mapping| mapping.surface_id == id);
            let is_complete = self.surfaces.get(&id).is_some_and(Surface::is_complete);
            if is_current_output && (self.presentation_fallback.is_none() || is_complete) {
                self.frame_visible_dirty = true;
            }
            return;
        }
        let is_current_output = !self.output_mapping_stale
            && self.output.is_some_and(|mapping| mapping.surface_id == id);
        if !is_current_output {
            return;
        }
        let is_complete = self.surfaces.get(&id).is_some_and(Surface::is_complete);
        if is_complete {
            self.presentation_fallback = None;
            if self.presentation_suppressed {
                self.presentation_suppressed = false;
                self.touch_presentation();
                return;
            }
        }
        if self.presentation_fallback.is_some() && !is_complete {
            // The fallback is still the visible surface, so do not wake the presenter for
            // a replacement write that cannot change what it will copy.
            return;
        }
        if self.presentation_suppressed {
            return;
        }
        self.touch_presentation();
    }

    pub fn create(&mut self, id: u16, width: u16, height: u16) {
        if self.output.is_some_and(|mapping| mapping.surface_id == id) {
            self.mark_frame_visible_dirty();
            self.retain_painted_output();
            self.output_mapping_stale = true;
        }
        self.surfaces.insert(id, Surface::new(width, height));
    }

    pub fn delete(&mut self, id: u16) {
        let deleting_output = self.output.is_some_and(|mapping| mapping.surface_id == id);
        let idle_presentation_changed = deleting_output
            && matches!(self.frame_state, FrameState::Idle)
            && (self.presentation_fallback.is_some()
                || self.presentation_suppressed
                || self.output_surface().is_some_and(Surface::is_painted));
        if deleting_output {
            self.mark_frame_visible_dirty();
            if !matches!(self.frame_state, FrameState::Idle) {
                self.retain_painted_output();
            }
        }
        self.surfaces.remove(&id);
        if deleting_output {
            self.output = None;
            self.output_mapping_stale = false;
            if matches!(self.frame_state, FrameState::Idle) {
                self.presentation_fallback = None;
                self.presentation_suppressed = false;
                if idle_presentation_changed {
                    self.touch_presentation();
                }
            }
        }
    }

    pub fn get(&self, id: u16) -> Option<&Surface> {
        self.surfaces.get(&id)
    }

    /// Drop every cached bitmap. Used when the handler resets its own mirror, so the
    /// two cannot disagree about which slots exist.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    /// Remove one server-selected cache slot.
    ///
    /// An eviction command for an already-empty slot is harmless and is not counted as
    /// displaced content. Reusing an occupied slot remains an eviction too.
    pub fn evict_cache(&mut self, slot: u16) {
        if self.cache.remove(&slot).is_some() {
            self.cache_stats.evictions = self.cache_stats.evictions.saturating_add(1);
        }
    }

    pub fn set_graphics_output_size(&mut self, width: u32, height: u32) -> bool {
        let (Ok(width), Ok(height)) = (u16::try_from(width), u16::try_from(height)) else {
            return false;
        };
        if width == 0 || height == 0 {
            return false;
        }
        let changed = self.graphics_output_size != Some((width, height));
        self.graphics_output_size = Some((width, height));
        if changed && self.output.is_some() {
            if matches!(self.frame_state, FrameState::Idle) {
                if !self.presentation_suppressed
                    && self.presentation_fallback.is_none()
                    && self.presentation_surface().is_some()
                {
                    self.touch_presentation();
                }
            } else {
                self.mark_frame_visible_dirty();
            }
        }
        true
    }

    pub fn map_to_output(&mut self, id: u16) {
        let Some(surface) = self.surfaces.get(&id) else {
            return;
        };
        let _ = self.map_to_output_geometry(
            id,
            surface.width,
            surface.height,
            0,
            0,
            u32::from(surface.width),
            u32::from(surface.height),
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn map_to_output_geometry(
        &mut self,
        id: u16,
        source_width: u16,
        source_height: u16,
        dest_x: u32,
        dest_y: u32,
        dest_width: u32,
        dest_height: u32,
    ) -> bool {
        let Some(surface) = self.surfaces.get(&id) else {
            return false;
        };
        if source_width == 0
            || source_height == 0
            || source_width > surface.width
            || source_height > surface.height
            || dest_width == 0
            || dest_height == 0
            || dest_x.checked_add(dest_width).is_none()
            || dest_y.checked_add(dest_height).is_none()
        {
            return false;
        }
        if self
            .graphics_output_size
            .is_some_and(|(canvas_width, canvas_height)| {
                dest_x >= u32::from(canvas_width) || dest_y >= u32::from(canvas_height)
            })
        {
            return false;
        }
        let mapping = OutputMapping {
            surface_id: id,
            source_width,
            source_height,
            dest_x,
            dest_y,
            dest_width,
            dest_height,
        };
        if self.presentation_mapping_for(mapping).is_none() {
            return false;
        }
        let mapping_changed = self.output_mapping_stale || self.output != Some(mapping);
        if mapping_changed {
            self.mark_frame_visible_dirty();
            self.retain_painted_output();
        }
        self.output = Some(mapping);
        self.output_mapping_stale = false;
        let output_complete = self.surfaces.get(&id).is_some_and(Surface::is_complete);
        let released_suppressed_presentation = if matches!(self.frame_state, FrameState::Idle)
            && output_complete
            && self.presentation_suppressed
        {
            self.presentation_fallback = None;
            self.presentation_suppressed = false;
            self.touch_presentation();
            true
        } else {
            if matches!(self.frame_state, FrameState::Idle) && output_complete {
                self.presentation_fallback = None;
            }
            false
        };
        // An unpainted or partial replacement leaves the fallback on screen. It becomes
        // a presentation change only when this mapping selects painted pixels without a
        // fallback still hiding them.
        if mapping_changed
            && self.surfaces.get(&id).is_some_and(Surface::is_painted)
            && self.presentation_fallback.is_none()
            && !self.presentation_suppressed
            && !released_suppressed_presentation
        {
            if matches!(self.frame_state, FrameState::Idle) {
                self.touch_presentation();
            } else {
                self.mark_frame_visible_dirty();
            }
        }
        true
    }

    /// The surface currently mapped to output in protocol state.
    pub fn output_surface(&self) -> Option<&Surface> {
        if self.output_mapping_stale {
            return None;
        }
        self.output
            .and_then(|mapping| self.surfaces.get(&mapping.surface_id))
    }

    fn presentation_mapping_for(&self, mapping: OutputMapping) -> Option<PresentationMapping> {
        let (canvas_width, canvas_height) = self.graphics_output_size.unwrap_or_else(|| {
            (
                u16::try_from(mapping.dest_x + mapping.dest_width).unwrap_or(u16::MAX),
                u16::try_from(mapping.dest_y + mapping.dest_height).unwrap_or(u16::MAX),
            )
        });
        if canvas_width == 0 || canvas_height == 0 {
            return None;
        }
        Some(PresentationMapping {
            canvas_width,
            canvas_height,
            source_width: mapping.source_width,
            source_height: mapping.source_height,
            dest_x: mapping.dest_x,
            dest_y: mapping.dest_y,
            dest_width: mapping.dest_width,
            dest_height: mapping.dest_height,
        })
    }

    fn presentation_frame(&self) -> Option<(&Surface, PresentationMapping)> {
        if !matches!(self.frame_state, FrameState::Idle) {
            return self
                .presentation_fallback
                .as_ref()
                .map(|fallback| (&fallback.surface, fallback.mapping));
        }
        if let Some(fallback) = self.presentation_fallback.as_ref() {
            if !self.output_mapping_stale
                && let Some(mapping) = self.output
                && let Some(surface) = self
                    .surfaces
                    .get(&mapping.surface_id)
                    .filter(|surface| surface.is_complete())
                && let Some(presentation_mapping) = self.presentation_mapping_for(mapping)
            {
                return Some((surface, presentation_mapping));
            }
            return Some((&fallback.surface, fallback.mapping));
        }
        if self.output_mapping_stale {
            return None;
        }
        let output_mapping = self.output?;
        let surface = self
            .surfaces
            .get(&output_mapping.surface_id)
            .filter(|surface| surface.is_painted())?;
        let presentation_mapping = self.presentation_mapping_for(output_mapping)?;
        Some((surface, presentation_mapping))
    }

    /// The surface the window should present.
    ///
    /// This is deliberately separate from [`Self::output_surface`]: protocol state may
    /// already map a replacement surface before that surface has received usable pixels.
    /// The presenter must keep the last good desktop through that handoff instead of
    /// flashing the replacement's zero-filled allocation.
    pub fn presentation_surface(&self) -> Option<&Surface> {
        self.presentation_frame().map(|(surface, _)| surface)
    }

    pub(crate) fn presentation_dimensions(&self) -> Option<(u16, u16)> {
        let (_, mapping) = self.presentation_frame()?;
        Some((mapping.canvas_width, mapping.canvas_height))
    }

    /// Copy the current presentation surface into a reusable snapshot.
    ///
    /// The dimensions, pixels, and generation are read under the caller's store lock,
    /// making them one coherent view. A `Retained` result means an active or aborted frame
    /// still owns the store; the snapshot is intentionally left untouched.
    pub(crate) fn copy_presentation_state(
        &self,
        snapshot: &mut PresentationSnapshot,
    ) -> PresentationCopy {
        if self.presentation_suppressed || !matches!(self.frame_state, FrameState::Idle) {
            return PresentationCopy::Retained;
        }
        snapshot.generation = self.generation;
        let Some((surface, mapping)) = self.presentation_frame() else {
            snapshot.width = 0;
            snapshot.height = 0;
            snapshot.pixel_width = 0;
            snapshot.pixel_height = 0;
            snapshot.mapping = PresentationMapping::default();
            snapshot.pixels.clear();
            return PresentationCopy::Empty;
        };
        snapshot.width = mapping.canvas_width;
        snapshot.height = mapping.canvas_height;
        snapshot.pixel_width = surface.width;
        snapshot.pixel_height = surface.height;
        snapshot.mapping = mapping;
        snapshot.pixels.resize(surface.pixels.len(), 0);
        snapshot.pixels.copy_from_slice(surface.pixels());
        PresentationCopy::Copied
    }

    /// Compatibility predicate for callers that only need to know whether a painted
    /// surface existed. Transaction-aware callers should use
    /// [`Self::copy_presentation_state`] so `Retained` is not mistaken for `Empty`.
    #[cfg(test)]
    pub(crate) fn copy_presentation(&self, snapshot: &mut PresentationSnapshot) -> bool {
        matches!(
            self.copy_presentation_state(snapshot),
            PresentationCopy::Copied
        )
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
        // This is the wire path — pixels a decoder produced. `cache_to_surface` blits
        // through `Surface` directly, so cached pixels are not counted twice here.
        let written = surface.blit_rgba(dest, src, src_stride_px)?;
        self.cache_stats.bytes_from_wire += written as u64;
        if written == 0 {
            return Ok(());
        }
        self.finish_surface_mutation(id);
        Ok(())
    }

    /// Blit a complete decoded rectangle while retiring coverage only for the exact
    /// regions the decoder says it supplied. Returns the explicit bytes that landed
    /// after clipping, so codec telemetry cannot count seeded or refused pixels.
    pub(crate) fn blit_rgba_with_coverage(
        &mut self,
        id: u16,
        dest: Rect,
        src: &[u8],
        src_stride_px: u16,
        coverage: &[Rect],
    ) -> Result<usize, SurfaceError> {
        let surface = self
            .surfaces
            .get_mut(&id)
            .ok_or(SurfaceError::NoSuchSurface(id))?;
        let written = surface.blit_rgba_covered(dest, src, src_stride_px, Some(coverage))?;
        if written == 0 {
            // No explicit decoder coverage landed on the surface. Do not account or
            // publish a mutation for a bitmap that cannot affect presentation.
            return Ok(0);
        }
        self.cache_stats.bytes_from_wire += written as u64;
        self.finish_surface_mutation(id);
        Ok(written)
    }

    /// Swap a full decoded frame into a surface (the native AU path). Returns the
    /// displaced buffer for reuse. See [`Surface::adopt_pixels`].
    pub fn adopt_pixels(&mut self, id: u16, pixels: Vec<u8>) -> Result<Vec<u8>, SurfaceError> {
        let surface = self
            .surfaces
            .get_mut(&id)
            .ok_or(SurfaceError::NoSuchSurface(id))?;
        let old = surface.adopt_pixels(pixels)?;
        self.cache_stats.bytes_from_wire += old.len() as u64;
        self.finish_surface_mutation(id);
        Ok(old)
    }

    /// Strict-bounds BGRA blit (the native rect path). See [`Surface::blit_bgra_strict`].
    pub fn blit_bgra_strict(
        &mut self,
        id: u16,
        dest: Rect,
        src: &[u8],
    ) -> Result<(), SurfaceError> {
        let surface = self
            .surfaces
            .get_mut(&id)
            .ok_or(SurfaceError::NoSuchSurface(id))?;
        surface.blit_bgra_strict(dest, src)?;
        self.cache_stats.bytes_from_wire += src.len() as u64;
        self.finish_surface_mutation(id);
        Ok(())
    }

    /// Strict-bounds RGBA blit (the native tiled decoder path).
    pub fn blit_rgba_strict(
        &mut self,
        id: u16,
        dest: Rect,
        src: &[u8],
    ) -> Result<(), SurfaceError> {
        let surface = self
            .surfaces
            .get_mut(&id)
            .ok_or(SurfaceError::NoSuchSurface(id))?;
        surface.blit_rgba_strict(dest, src)?;
        self.cache_stats.bytes_from_wire += src.len() as u64;
        self.finish_surface_mutation(id);
        Ok(())
    }

    /// Apply several tightly packed RGBA rectangles as one visible mutation.
    ///
    /// Native tiled frames decode outside the store lock. The caller holds the
    /// decoded tile buffers until this method has validated every rectangle and
    /// payload, so a malformed later tile cannot leave an earlier tile visible.
    /// One batch accounts bytes and advances the presentation generation once.
    pub(crate) fn blit_rgba_strict_batch(
        &mut self,
        id: u16,
        updates: &[(Rect, &[u8])],
    ) -> Result<u64, SurfaceError> {
        if updates.is_empty() {
            return Ok(self.generation);
        }

        {
            let surface = self
                .surfaces
                .get(&id)
                .ok_or(SurfaceError::NoSuchSurface(id))?;
            for (dest, src) in updates {
                surface.validate_strict(*dest, src)?;
            }
        }

        let surface = self
            .surfaces
            .get_mut(&id)
            .ok_or(SurfaceError::NoSuchSurface(id))?;
        let mut bytes = 0u64;
        for (dest, src) in updates {
            // Every update was validated above while the store was still
            // unchanged, so these writes cannot fail part-way through.
            surface.blit_rgba_strict(*dest, src)?;
            bytes = bytes.saturating_add(src.len() as u64);
        }
        self.cache_stats.bytes_from_wire = self.cache_stats.bytes_from_wire.saturating_add(bytes);
        self.finish_surface_mutation(id);
        Ok(self.generation)
    }

    /// Apply several tightly packed BGRA rectangles as one visible mutation.
    ///
    /// Native dirty rectangles arrive in BGRA order. Validate the complete batch before
    /// swizzling any pixel so a malformed later rectangle cannot expose an earlier one.
    pub(crate) fn blit_bgra_strict_batch<'a, I>(
        &mut self,
        id: u16,
        updates: I,
    ) -> Result<u64, SurfaceError>
    where
        I: Iterator<Item = (Rect, &'a [u8])> + Clone,
    {
        if updates.clone().next().is_none() {
            return Ok(self.generation);
        }

        {
            let surface = self
                .surfaces
                .get(&id)
                .ok_or(SurfaceError::NoSuchSurface(id))?;
            for (dest, src) in updates.clone() {
                surface.validate_strict(dest, src)?;
            }
        }

        let surface = self
            .surfaces
            .get_mut(&id)
            .ok_or(SurfaceError::NoSuchSurface(id))?;
        let mut bytes = 0u64;
        for (dest, src) in updates {
            // Preflight above makes every swizzle infallible while this store remains locked.
            surface.blit_bgra_strict(dest, src)?;
            bytes = bytes.saturating_add(src.len() as u64);
        }
        self.cache_stats.bytes_from_wire = self.cache_stats.bytes_from_wire.saturating_add(bytes);
        self.finish_surface_mutation(id);
        Ok(self.generation)
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
        if rects.is_empty() {
            return Ok(());
        }
        let writes_anything = rects
            .iter()
            .any(|rect| rect.clip_to(surface.width, surface.height).is_some());
        if !writes_anything {
            return Ok(());
        }
        for rect in rects {
            surface.fill(*rect, rgba);
        }
        self.finish_surface_mutation(id);
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
        let Some(clipped_src) = src_rect.clip_to(src_surface.width, src_surface.height) else {
            return Ok(());
        };
        let Some(pixels) = src_surface.extract(clipped_src) else {
            return Ok(());
        };
        let (w, h) = (clipped_src.width(), clipped_src.height());

        let dest = self
            .surfaces
            .get_mut(&dest_id)
            .ok_or(SurfaceError::NoSuchSurface(dest_id))?;
        if dest_points.is_empty() {
            return Ok(());
        }
        let mut written = 0usize;
        for (x, y) in dest_points {
            // Saturating, not wrapping: a destination that runs off the right edge is
            // clipped by blit_rgba. `x + w` on u16 panics in debug and wraps in release,
            // and a panic here poisons the store mutex and takes the session with it.
            let rect = Rect::new(*x, *y, x.saturating_add(w), y.saturating_add(h));
            written = written.saturating_add(dest.blit_rgba(rect, &pixels, w)?);
        }
        if written == 0 {
            return Ok(());
        }
        self.finish_surface_mutation(dest_id);
        Ok(())
    }

    pub fn surface_to_cache(
        &mut self,
        src_id: u16,
        src_rect: Rect,
        slot: u16,
    ) -> Result<Option<(u16, u16)>, SurfaceError> {
        let surface = self
            .surfaces
            .get(&src_id)
            .ok_or(SurfaceError::NoSuchSurface(src_id))?;
        // Record the CLIPPED dimensions: `extract` clips, so storing the requested
        // width would make every later cache_to_surface ask for more bytes than exist
        // and fail with ShortSource — silently dropping the cached region.
        let Some(clipped) = src_rect.clip_to(surface.width, surface.height) else {
            return Ok(None);
        };
        let Some(pixels) = surface.extract(clipped) else {
            return Ok(None);
        };
        self.cache_stats.bytes_stored += pixels.len() as u64;
        let replaced = self.cache.insert(
            slot,
            CacheEntry {
                width: clipped.width(),
                height: clipped.height(),
                pixels,
            },
        );
        if replaced.is_some() {
            // The server reused a slot: the old bitmap is gone. Worth seeing, because a
            // high eviction count against a low hit rate means the cache is thrashing.
            self.cache_stats.evictions += 1;
        }
        Ok(Some((clipped.width(), clipped.height())))
    }

    pub fn cache_to_surface(
        &mut self,
        slot: u16,
        dest_id: u16,
        dest_points: &[(u16, u16)],
    ) -> Result<(), SurfaceError> {
        let Some(entry) = self.cache.get(&slot).cloned() else {
            // A slot the server believes it filled and we do not have. Every one of these
            // is a region that will not be painted, so it is counted rather than only
            // returned — the caller may well treat the error as survivable.
            self.cache_stats.misses += 1;
            return Err(SurfaceError::NoSuchCacheSlot(slot));
        };
        let dest = self
            .surfaces
            .get_mut(&dest_id)
            .ok_or(SurfaceError::NoSuchSurface(dest_id))?;
        if dest_points.is_empty() {
            return Ok(());
        }
        let mut served = 0usize;
        for (x, y) in dest_points {
            let rect = Rect::new(
                *x,
                *y,
                x.saturating_add(entry.width),
                y.saturating_add(entry.height),
            );
            served += dest.blit_rgba(rect, &entry.pixels, entry.width)?;
        }
        // One hit per destination painted, not per PDU: a single command that stamps a
        // cached tile in twelve places saved twelve regions' worth of wire traffic.
        self.cache_stats.hits += dest_points.len() as u64;
        self.cache_stats.bytes_served += served as u64;
        if served == 0 {
            return Ok(());
        }
        self.finish_surface_mutation(dest_id);
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
    fn coverage_counts_overlapping_unaligned_edges_without_tail_bits() {
        let mut coverage = Coverage::Empty;
        coverage.mark_rect(13, 2, Rect::new(1, 0, 10, 1));
        coverage.mark_rect(13, 2, Rect::new(8, 0, 13, 1));
        coverage.mark_rect(13, 2, Rect::new(0, 0, 1, 2));

        match &coverage {
            Coverage::Bits { bits, covered } => {
                assert_eq!(bits.len(), 4, "26 pixels need four coverage bytes");
                assert_eq!(*covered, 14, "overlapping pixels count only once");
            }
            Coverage::Empty | Coverage::Full => panic!("coverage should still be partial"),
        }

        coverage.mark_rect(13, 2, Rect::new(1, 1, 13, 2));
        assert!(
            coverage.is_full(),
            "the final unaligned row completes coverage"
        );
    }

    #[test]
    fn adopt_swaps_the_buffer_and_returns_the_old_one() {
        let mut s = Surface::new(2, 2);
        // Distinct per-pixel values so a swapped-vs-copied mixup is distinguishable.
        let fresh: Vec<u8> = (0..2 * 2 * BPP as u8).collect();
        let old = s.adopt_pixels(fresh.clone()).unwrap();
        assert!(old.iter().all(|&b| b == 0), "old buffer is the zeroed one");
        assert_eq!(s.pixels(), fresh.as_slice());
    }

    #[test]
    fn adopt_rejects_a_wrong_size_buffer_without_touching_pixels() {
        let mut s = Surface::new(2, 2);
        s.blit_rgba(Rect::new(0, 0, 2, 2), &solid(2, 2, RED), 2)
            .unwrap();
        let err = s.adopt_pixels(vec![7u8; 5]).unwrap_err();
        assert_eq!(
            err,
            SurfaceError::SizeMismatch {
                expected: 2 * 2 * BPP,
                got: 5
            }
        );
        assert_eq!(&s.pixels()[0..4], RED, "pixels untouched after rejection");
    }

    #[test]
    fn bgra_strict_blit_swizzles_and_lands_at_the_right_offset() {
        // One BGRA pixel with four DIFFERENT channel values, blitted at (1,1) of 3x3:
        // any channel-order or offset mistake changes the result.
        let mut s = Surface::new(3, 3);
        let bgra = [10u8, 20, 30, 40]; // B=10 G=20 R=30 A=40
        s.blit_bgra_strict(Rect::new(1, 1, 2, 2), &bgra).unwrap();
        let off = (3 + 1) * BPP;
        assert_eq!(&s.pixels()[off..off + BPP], &[30, 20, 10, 40]); // RGBA
        // Every other pixel untouched.
        for (i, px) in s.pixels().chunks_exact(BPP).enumerate() {
            if i != 4 {
                assert_eq!(px, [0, 0, 0, 0], "pixel {i} should be untouched");
            }
        }
    }

    #[test]
    fn bgra_strict_blit_rejects_overhang_and_short_payloads() {
        let mut s = Surface::new(3, 3);
        // Overhang: 2x2 at (2,2) of a 3x3 surface.
        let err = s
            .blit_bgra_strict(Rect::new(2, 2, 4, 4), &solid(2, 2, RED))
            .unwrap_err();
        assert!(matches!(err, SurfaceError::OutOfBounds { .. }));
        // In-bounds rect, payload one byte short of 1x1.
        let err = s
            .blit_bgra_strict(Rect::new(0, 0, 1, 1), &[1, 2, 3])
            .unwrap_err();
        assert_eq!(
            err,
            SurfaceError::SizeMismatch {
                expected: BPP,
                got: 3
            }
        );
        assert!(s.pixels().iter().all(|&b| b == 0), "nothing written");
    }

    #[test]
    fn rgba_strict_blit_copies_channels_and_rejects_invalid_input() {
        let mut s = Surface::new(3, 3);
        let rgba = [10u8, 20, 30, 40];
        s.blit_rgba_strict(Rect::new(1, 1, 2, 2), &rgba).unwrap();
        let off = (3 + 1) * BPP;
        assert_eq!(&s.pixels()[off..off + BPP], rgba);

        let before = s.pixels().to_vec();
        assert!(matches!(
            s.blit_rgba_strict(Rect::new(2, 2, 4, 4), &solid(2, 2, RED)),
            Err(SurfaceError::OutOfBounds { .. })
        ));
        assert_eq!(
            s.blit_rgba_strict(Rect::new(0, 0, 1, 1), &[1, 2, 3]),
            Err(SurfaceError::SizeMismatch {
                expected: BPP,
                got: 3
            })
        );
        assert_eq!(s.pixels(), before, "rejected blits must not mutate pixels");
    }

    #[test]
    fn store_adopt_and_strict_blit_bump_the_generation() {
        let mut store = SurfaceStore::new();
        store.create(0, 2, 2);
        store.map_to_output(0);
        let g0 = store.generation();
        store.adopt_pixels(0, vec![9u8; 2 * 2 * BPP]).unwrap();
        let g1 = store.generation();
        assert!(g1 > g0, "adopt must mark the store changed");
        store
            .blit_bgra_strict(0, Rect::new(0, 0, 1, 1), &[1, 2, 3, 4])
            .unwrap();
        assert!(
            store.generation() > g1,
            "strict blit must mark the store changed"
        );
        let g2 = store.generation();
        let bytes_before = store.cache_stats().bytes_from_wire;
        store
            .blit_rgba_strict(0, Rect::new(1, 1, 2, 2), &[5, 6, 7, 8])
            .unwrap();
        assert!(
            store.generation() > g2,
            "strict RGBA blit must mark the store changed"
        );
        assert_eq!(
            store.cache_stats().bytes_from_wire,
            bytes_before + BPP as u64
        );
    }

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
    fn clearcodec_seed_padding_rejects_an_oversized_extent_before_reserving() {
        let surface = Surface::new(1, 1);
        assert!(
            surface
                .extract_with_zero_padding(Rect::new(0, 0, MAX_DECODE_DIM + 1, 1))
                .is_none(),
            "the seed helper must reject dimensions outside ClearCodec's limit"
        );
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
    fn surface_to_surface_uses_clipped_source_dimensions_for_all_destinations() {
        let mut store = SurfaceStore::new();
        store.create(1, 4, 4);
        store.create(2, 6, 6);
        store
            .blit_rgba(1, Rect::new(0, 0, 4, 4), &ramp(4, 4), 4)
            .unwrap();
        store.map_to_output(2);

        let before = store.generation();
        store
            .surface_to_surface(1, Rect::new(2, 2, 6, 6), 2, &[(0, 0), (4, 4)])
            .expect("a clipped source rectangle should copy to every destination");

        let dest = store.get(2).unwrap();
        for &(x, y) in &[(0u16, 0u16), (4, 4)] {
            for row in 0..2u16 {
                for col in 0..2u16 {
                    let off = ((y + row) as usize * dest.width as usize + (x + col) as usize) * BPP;
                    assert_eq!(
                        &dest.pixels()[off..off + BPP],
                        &[col as u8 + 2, row as u8 + 2, 0, 255],
                        "destination ({x},{y}) pixel ({col},{row}) differs"
                    );
                }
            }
        }
        assert!(
            store.generation() > before,
            "a successful copy must announce its mutation"
        );
    }

    #[test]
    fn cache_round_trip_preserves_pixels() {
        let mut store = SurfaceStore::new();
        store.create(1, 4, 4);
        store.create(2, 4, 4);
        store
            .blit_rgba(1, Rect::new(0, 0, 2, 2), &solid(2, 2, BLUE), 2)
            .unwrap();
        assert_eq!(
            store.surface_to_cache(1, Rect::new(0, 0, 2, 2), 7),
            Ok(Some((2, 2)))
        );
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
    fn generation_changes_on_visible_mutation_so_the_presenter_can_skip_redraws() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 2);
        store.map_to_output(1);
        let g = store.generation();
        store.solid_fill(1, &[Rect::new(0, 0, 1, 1)], RED).unwrap();
        assert_ne!(store.generation(), g);
    }

    #[test]
    fn offscreen_and_cache_only_work_does_not_advance_presentation_generation() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 2);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 2)], RED).unwrap();
        store.map_to_output(1);

        let mut before = PresentationSnapshot::default();
        assert!(store.copy_presentation(&mut before));
        let generation = store.generation();

        store.create(2, 2, 2);
        store.solid_fill(2, &[Rect::new(0, 0, 2, 2)], BLUE).unwrap();
        store
            .surface_to_surface(2, Rect::new(0, 0, 1, 1), 2, &[(1, 1)])
            .unwrap();
        assert_eq!(
            store.surface_to_cache(2, Rect::new(0, 0, 2, 2), 7),
            Ok(Some((2, 2)))
        );
        store.cache_to_surface(7, 2, &[(0, 0)]).unwrap();
        store.evict_cache(7);
        store.delete(2);

        assert_eq!(
            store.generation(),
            generation,
            "offscreen and cache-only work must not dirty presentation"
        );
        let mut after = PresentationSnapshot::default();
        assert!(store.copy_presentation(&mut after));
        assert_eq!(after.generation, before.generation);
        assert_eq!(after.pixels, before.pixels);
    }

    #[test]
    fn empty_or_out_of_clip_coverage_does_not_account_or_wake() {
        let mut empty = SurfaceStore::new();
        empty.create(1, 2, 2);
        empty.solid_fill(1, &[Rect::new(0, 0, 2, 2)], RED).unwrap();
        empty.map_to_output(1);
        let empty_generation = empty.generation();
        let empty_stats = empty.cache_stats();
        empty
            .blit_rgba_with_coverage(1, Rect::new(0, 0, 1, 1), &solid(1, 1, RED), 1, &[])
            .unwrap();
        assert_eq!(empty.generation(), empty_generation);
        assert_eq!(empty.cache_stats(), empty_stats);
        assert_eq!(empty.get(1).unwrap().pixels(), &solid(2, 2, RED));

        let mut outside = SurfaceStore::new();
        outside.create(1, 2, 2);
        outside
            .solid_fill(1, &[Rect::new(0, 0, 2, 2)], RED)
            .unwrap();
        outside.map_to_output(1);
        let outside_generation = outside.generation();
        let outside_stats = outside.cache_stats();
        outside
            .blit_rgba_with_coverage(
                1,
                Rect::new(2, 2, 3, 3),
                &solid(1, 1, RED),
                1,
                &[Rect::new(2, 2, 3, 3)],
            )
            .unwrap();
        assert_eq!(outside.generation(), outside_generation);
        assert_eq!(outside.cache_stats(), outside_stats);
        assert_eq!(outside.get(1).unwrap().pixels(), &solid(2, 2, RED));

        let mut partial = SurfaceStore::new();
        partial.create(1, 2, 2);
        partial
            .blit_rgba_with_coverage(
                1,
                Rect::new(0, 0, 2, 2),
                &solid(2, 2, RED),
                2,
                &[Rect::new(0, 0, 1, 1)],
            )
            .unwrap();
        assert_eq!(
            partial.cache_stats().bytes_from_wire,
            BPP as u64,
            "wire accounting follows explicit coverage, not the seeded bitmap"
        );
    }

    #[test]
    fn presentation_snapshot_copies_pixels_dimensions_and_generation_together() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 1);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 1)], RED).unwrap();
        store.map_to_output(1);
        let expected_generation = store.generation();

        let mut snapshot = PresentationSnapshot::default();
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        assert_eq!(snapshot.width, 2);
        assert_eq!(snapshot.height, 1);
        assert_eq!(snapshot.pixels, solid(2, 1, RED));
        assert_eq!(snapshot.generation, expected_generation);

        store
            .blit_rgba_strict(1, Rect::new(1, 0, 2, 1), &solid(1, 1, BLUE))
            .unwrap();
        assert_ne!(store.generation(), snapshot.generation);
        assert_eq!(
            snapshot.pixels,
            solid(2, 1, RED),
            "the old snapshot is immutable"
        );

        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        assert_eq!(snapshot.pixels, [RED, BLUE].concat());
        assert_eq!(snapshot.generation, store.generation());
    }

    #[test]
    fn a_logical_frame_publishes_split_writes_once_at_commit() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 1);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 1)], RED).unwrap();
        store.map_to_output(1);

        let mut snapshot = PresentationSnapshot::default();
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        let before = store.generation();

        store.begin_frame(7);
        store
            .blit_rgba_strict(1, Rect::new(0, 0, 1, 1), &solid(1, 1, BLUE))
            .unwrap();
        assert_eq!(store.generation(), before);
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Retained
        );
        assert_eq!(snapshot.pixels, solid(2, 1, RED));

        store
            .blit_rgba_strict(1, Rect::new(1, 0, 2, 1), &solid(1, 1, BLUE))
            .unwrap();
        assert_eq!(store.generation(), before);
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Retained
        );
        assert!(store.commit_frame(7));
        assert_eq!(store.generation(), before + 1);
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        assert_eq!(snapshot.pixels, solid(2, 1, BLUE));
    }

    #[test]
    fn an_offscreen_frame_does_not_bump_generation_at_commit() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 1);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 1)], RED).unwrap();
        store.map_to_output(1);
        store.create(2, 2, 1);
        let before = store.generation();

        store.begin_frame(11);
        store
            .blit_rgba_strict(2, Rect::new(0, 0, 2, 1), &solid(2, 1, BLUE))
            .unwrap();
        assert_eq!(store.generation(), before);
        assert!(store.commit_frame(11));
        assert_eq!(store.generation(), before);
    }

    #[test]
    fn an_aborted_frame_blocks_a_later_partial_commit() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 1);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 1)], RED).unwrap();
        store.map_to_output(1);

        let mut snapshot = PresentationSnapshot::default();
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        let before = store.generation();

        store.begin_frame(8);
        store
            .blit_rgba_strict(1, Rect::new(0, 0, 1, 1), &solid(1, 1, BLUE))
            .unwrap();
        store.abort_frame();
        assert_eq!(store.generation(), before);
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Retained
        );
        assert_eq!(snapshot.pixels, solid(2, 1, RED));

        store.begin_frame(9);
        store
            .blit_rgba_strict(1, Rect::new(1, 0, 2, 1), &solid(1, 1, BLUE))
            .unwrap();
        assert!(!store.commit_frame(9));
        assert_eq!(store.generation(), before);
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Retained
        );
        assert_eq!(snapshot.pixels, solid(2, 1, RED));
    }

    #[test]
    fn same_id_replacement_during_a_frame_retains_the_snapshot_until_complete() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 1);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 1)], RED).unwrap();
        store.map_to_output(1);

        let mut snapshot = PresentationSnapshot::default();
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        let before = store.generation();

        store.begin_frame(12);
        store.delete(1);
        store.create(1, 2, 1);
        store.map_to_output(1);
        store
            .blit_rgba_strict(1, Rect::new(0, 0, 1, 1), &solid(1, 1, BLUE))
            .unwrap();
        assert!(store.commit_frame(12));
        assert_eq!(store.generation(), before);
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Retained
        );
        assert_eq!(snapshot.pixels, solid(2, 1, RED));

        store.create(2, 2, 1);
        store.map_to_output(2);
        assert!(
            store.presentation_fallback.is_none(),
            "a suppressed partial output must not become a fallback on remap"
        );
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Retained
        );

        store.begin_frame(13);
        store.map_to_output(1);
        store
            .blit_rgba_strict(1, Rect::new(0, 0, 2, 1), &solid(2, 1, BLUE))
            .unwrap();
        assert!(store.commit_frame(13));
        assert_eq!(store.generation(), before + 1);
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        assert_eq!(snapshot.pixels, solid(2, 1, BLUE));
    }

    #[test]
    fn suppressed_presentation_ignores_a_partial_surface_mapped_after_pre_map_paint() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 1);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 1)], RED).unwrap();
        store.map_to_output(1);

        let mut snapshot = PresentationSnapshot::default();
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        let before = store.generation();

        // An incomplete frame leaves the last copied snapshot suppressed rather than
        // exposing the replacement's partial in-place pixels.
        store.begin_frame(12);
        store.create(2, 2, 1);
        store.map_to_output(2);
        store
            .blit_rgba_strict(2, Rect::new(0, 0, 1, 1), &solid(1, 1, BLUE))
            .unwrap();
        assert!(store.commit_frame(12));
        assert_eq!(store.generation(), before);
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Retained
        );
        assert_eq!(snapshot.pixels, solid(2, 1, RED));

        // Writes may arrive before MapSurfaceToOutput. Mapping this already-painted,
        // still-partial surface must not manufacture a damage event while suppression
        // keeps the old snapshot on screen.
        store.create(3, 2, 1);
        store
            .blit_rgba_strict(3, Rect::new(0, 0, 1, 1), &solid(1, 1, BLUE))
            .unwrap();
        let before_map = store.generation();
        store.map_to_output(3);
        assert_eq!(
            store.generation(),
            before_map,
            "partial pre-map pixels remain hidden until the surface is complete"
        );
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Retained
        );
        assert_eq!(snapshot.pixels, solid(2, 1, RED));

        store
            .blit_rgba_strict(3, Rect::new(1, 0, 2, 1), &solid(1, 1, BLUE))
            .unwrap();
        assert_eq!(store.generation(), before_map + 1);
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        assert_eq!(snapshot.pixels, solid(2, 1, BLUE));
    }

    #[test]
    fn empty_store_mutations_do_not_advance_presentation_generation() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 1);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 1)], RED).unwrap();
        store.map_to_output(1);
        assert_eq!(
            store.surface_to_cache(1, Rect::new(0, 0, 2, 1), 7),
            Ok(Some((2, 1)))
        );
        let before = store.generation();

        store.solid_fill(1, &[], BLUE).unwrap();
        store
            .surface_to_surface(1, Rect::new(0, 0, 1, 1), 1, &[])
            .unwrap();
        store.cache_to_surface(7, 1, &[]).unwrap();

        assert_eq!(
            store.solid_fill(99, &[], BLUE),
            Err(SurfaceError::NoSuchSurface(99))
        );
        assert_eq!(
            store.surface_to_surface(99, Rect::new(0, 0, 1, 1), 1, &[]),
            Err(SurfaceError::NoSuchSurface(99))
        );
        assert_eq!(
            store.cache_to_surface(99, 1, &[]),
            Err(SurfaceError::NoSuchCacheSlot(99))
        );

        assert_eq!(store.generation(), before);
    }

    #[test]
    fn a_replacement_fallback_retires_only_at_frame_commit() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 1);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 1)], RED).unwrap();
        store.map_to_output(1);
        store.create(2, 2, 1);
        store.map_to_output(2);
        assert!(store.presentation_fallback.is_some());

        let mut snapshot = PresentationSnapshot::default();
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        let before = store.generation();

        store.begin_frame(9);
        store.create(2, 2, 1);
        assert!(store.commit_frame(9));
        assert_eq!(store.generation(), before);
        assert!(store.presentation_fallback.is_some());
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        assert_eq!(snapshot.pixels, solid(2, 1, RED));

        store.map_to_output(2);
        store.begin_frame(10);
        store
            .blit_rgba_strict(2, Rect::new(0, 0, 2, 1), &solid(2, 1, BLUE))
            .unwrap();
        assert!(
            store.presentation_fallback.is_some(),
            "coverage completion must not retire a fallback mid-frame"
        );
        assert_eq!(store.generation(), before);
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Retained
        );
        assert_eq!(snapshot.pixels, solid(2, 1, RED));

        assert!(store.commit_frame(10));
        assert!(store.presentation_fallback.is_none());
        assert_eq!(store.generation(), before + 1);
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        assert_eq!(snapshot.pixels, solid(2, 1, BLUE));
    }

    #[test]
    fn replacement_fallback_retains_its_own_mapping() {
        let mut store = SurfaceStore::new();
        assert!(store.set_graphics_output_size(4, 1));
        store.create(1, 2, 1);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 1)], RED).unwrap();
        assert!(store.map_to_output_geometry(1, 2, 1, 0, 0, 2, 1));

        store.create(2, 2, 1);
        assert!(store.map_to_output_geometry(2, 2, 1, 2, 0, 2, 1));

        let mut snapshot = PresentationSnapshot::default();
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        assert_eq!(snapshot.mapping.dest_x, 0);
        assert_eq!(snapshot.pixels, solid(2, 1, RED));

        store.solid_fill(2, &[Rect::new(0, 0, 2, 1)], BLUE).unwrap();
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        assert_eq!(snapshot.mapping.dest_x, 2);
        assert_eq!(snapshot.pixels, solid(2, 1, BLUE));
    }

    #[test]
    fn replacement_fallback_retains_its_original_canvas_size() {
        let mut store = SurfaceStore::new();
        assert!(store.set_graphics_output_size(4, 1));
        store.create(1, 2, 1);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 1)], RED).unwrap();
        assert!(store.map_to_output_geometry(1, 2, 1, 0, 0, 2, 1));

        store.create(2, 2, 1);
        assert!(store.map_to_output_geometry(2, 2, 1, 2, 0, 2, 1));
        assert!(store.set_graphics_output_size(8, 1));

        let mut snapshot = PresentationSnapshot::default();
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        assert_eq!((snapshot.width, snapshot.height), (4, 1));
        assert_eq!(snapshot.mapping.canvas_width, 4);
        assert_eq!(snapshot.pixels, solid(2, 1, RED));
    }

    #[test]
    fn wholly_off_canvas_mapping_preserves_the_live_desktop() {
        let mut store = SurfaceStore::new();
        assert!(store.set_graphics_output_size(2, 1));
        store.create(1, 2, 1);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 1)], RED).unwrap();
        store.map_to_output(1);
        store.create(2, 1, 1);
        store.solid_fill(2, &[Rect::new(0, 0, 1, 1)], BLUE).unwrap();
        let before = store.generation();

        assert!(!store.map_to_output_geometry(2, 1, 1, 2, 0, 1, 1));
        assert_eq!(store.generation(), before);
        assert_eq!(store.output_surface().unwrap().pixels(), solid(2, 1, RED));
    }

    #[test]
    fn resizing_a_suppressed_partial_output_does_not_wake_the_presenter() {
        let mut store = SurfaceStore::new();
        assert!(store.set_graphics_output_size(2, 1));
        store.create(1, 2, 1);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 1)], RED).unwrap();
        store.map_to_output(1);
        let before = store.generation();

        store.begin_frame(31);
        store.delete(1);
        store.create(1, 2, 1);
        store.map_to_output(1);
        store
            .blit_rgba_strict(1, Rect::new(0, 0, 1, 1), &solid(1, 1, BLUE))
            .unwrap();
        assert!(store.commit_frame(31));
        assert_eq!(store.generation(), before);

        assert!(store.set_graphics_output_size(4, 1));
        assert_eq!(store.generation(), before);
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
    fn scaled_mapping_geometry_is_copied_atomically_with_pixels() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 2);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 2)], RED).unwrap();
        assert!(store.set_graphics_output_size(6, 3));
        assert!(store.map_to_output_geometry(1, 2, 2, 1, 0, 4, 2));

        let mut snapshot = PresentationSnapshot::default();
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        assert_eq!((snapshot.width, snapshot.height), (6, 3));
        assert_eq!((snapshot.pixel_width, snapshot.pixel_height), (2, 2));
        assert_eq!(
            snapshot.mapping,
            PresentationMapping {
                canvas_width: 6,
                canvas_height: 3,
                source_width: 2,
                source_height: 2,
                dest_x: 1,
                dest_y: 0,
                dest_width: 4,
                dest_height: 2,
            }
        );
        assert_eq!(snapshot.pixels, solid(2, 2, RED));
    }

    #[test]
    fn invalid_mapping_geometry_preserves_the_live_presentation() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 2);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 2)], RED).unwrap();
        store.map_to_output(1);
        let before = store.generation();

        assert!(!store.map_to_output_geometry(1, 2, 2, 0, 0, 0, 2));
        assert!(!store.map_to_output_geometry(1, 3, 2, 0, 0, 3, 2));
        assert_eq!(store.generation(), before);

        let mut snapshot = PresentationSnapshot::default();
        assert!(store.copy_presentation(&mut snapshot));
        assert_eq!((snapshot.width, snapshot.height), (2, 2));
        assert_eq!(snapshot.mapping.dest_width, 2);
    }

    #[test]
    fn deleting_the_only_mapped_surface_clears_the_presentation() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 1);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 1)], RED).unwrap();
        store.map_to_output(1);

        let before = store.generation();
        let mut snapshot = PresentationSnapshot::default();
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );

        store.delete(1);

        assert_eq!(store.generation(), before + 1);
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Empty
        );
        assert!(snapshot.pixels.is_empty());
    }

    #[test]
    fn terminal_frame_delete_clears_only_at_commit() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 1);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 1)], RED).unwrap();
        store.map_to_output(1);

        let before = store.generation();
        let mut snapshot = PresentationSnapshot::default();
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );

        store.begin_frame(7);
        store.delete(1);
        assert_eq!(store.generation(), before);
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Retained
        );

        assert!(store.commit_frame(7));
        assert_eq!(store.generation(), before + 1);
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Empty
        );
    }

    #[test]
    fn an_unpainted_replacement_does_not_cover_the_last_good_desktop() {
        // MDR-BUG-FLUX-00008. During kiln's display transition Windows replaces the
        // mapped EGFX surface before its H.264 stream has produced a usable picture.
        // CreateSurface allocates transparent black, so presenting protocol state
        // immediately makes switching back to the fullscreen window flash black. Keep
        // the last complete desktop until the replacement receives its first pixels.
        let mut store = SurfaceStore::new();
        store.create(1, 2, 2);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 2)], RED).unwrap();
        store.map_to_output(1);
        assert_eq!(
            store.presentation_surface().unwrap().pixels(),
            solid(2, 2, RED)
        );

        // Different-id overlap: the old and replacement surfaces are both live during
        // the handoff, and the server maps the replacement before its first paint.
        store.create(2, 2, 2);
        store.map_to_output(2);
        assert_eq!(
            store.presentation_surface().unwrap().pixels(),
            solid(2, 2, RED),
            "mapping an unpainted replacement must retain the last good desktop"
        );
        store.solid_fill(2, &[Rect::new(0, 0, 2, 2)], BLUE).unwrap();
        assert_eq!(
            store.presentation_surface().unwrap().pixels(),
            solid(2, 2, BLUE),
            "the replacement takes over on its first successful paint"
        );

        // Same-id replacement: CreateSurface itself swaps the mapped allocation, before
        // a separate MapSurface PDU can provide a handoff edge.
        store.create(2, 2, 2);
        assert_eq!(
            store.presentation_surface().unwrap().pixels(),
            solid(2, 2, BLUE),
            "same-id recreation must retain the old incarnation until remap"
        );
        store.map_to_output(2);
        store.solid_fill(2, &[Rect::new(0, 0, 2, 2)], RED).unwrap();
        assert_eq!(
            store.presentation_surface().unwrap().pixels(),
            solid(2, 2, RED),
            "the recreated incarnation takes over when it is painted"
        );
    }

    #[test]
    fn same_id_recreation_waits_for_fresh_mapping_geometry() {
        let mut store = SurfaceStore::new();
        assert!(store.set_graphics_output_size(8, 2));
        store.create(1, 2, 2);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 2)], RED).unwrap();
        assert!(store.map_to_output_geometry(1, 2, 2, 1, 0, 2, 2));

        let mut snapshot = PresentationSnapshot::default();
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        let before = store.generation();

        store.create(1, 4, 1);
        store.solid_fill(1, &[Rect::new(0, 0, 4, 1)], BLUE).unwrap();
        assert_eq!(
            store.generation(),
            before,
            "new pixels have no valid output geometry before remapping"
        );
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        assert_eq!(snapshot.pixels, solid(2, 2, RED));
        assert_eq!(
            (snapshot.mapping.dest_x, snapshot.mapping.dest_width),
            (1, 2)
        );

        assert!(!store.map_to_output_geometry(1, 5, 1, 2, 1, 4, 1));
        assert_eq!(
            store.generation(),
            before,
            "an invalid remap changes nothing"
        );
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        assert_eq!(snapshot.pixels, solid(2, 2, RED));

        assert!(store.map_to_output_geometry(1, 4, 1, 2, 1, 4, 1));
        assert_eq!(store.generation(), before + 1);
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        assert_eq!(snapshot.pixels, solid(4, 1, BLUE));
        assert_eq!(
            (snapshot.mapping.dest_x, snapshot.mapping.dest_width),
            (2, 4)
        );

        store.create(1, 1, 1);
        store.delete(1);
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Empty
        );
    }

    #[test]
    fn end_frame_cannot_publish_a_recreated_surface_before_remap() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 1);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 1)], RED).unwrap();
        store.map_to_output(1);
        let mut snapshot = PresentationSnapshot::default();
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        let before = store.generation();

        store.begin_frame(41);
        store.create(1, 4, 1);
        store.solid_fill(1, &[Rect::new(0, 0, 4, 1)], BLUE).unwrap();
        assert!(store.commit_frame(41));

        assert_eq!(store.generation(), before);
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Retained
        );
        assert_eq!(snapshot.pixels, solid(2, 1, RED));
        assert!(store.map_to_output_geometry(1, 4, 1, 0, 0, 4, 1));
        assert_eq!(store.generation(), before + 1);
        assert_eq!(
            store.copy_presentation_state(&mut snapshot),
            PresentationCopy::Copied
        );
        assert_eq!(snapshot.pixels, solid(4, 1, BLUE));
    }

    #[test]
    fn a_tiled_batch_retires_the_presentation_fallback_at_commit() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 2);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 2)], RED).unwrap();
        store.map_to_output(1);
        store.create(2, 2, 2);
        store.map_to_output(2);
        assert!(store.presentation_fallback.is_some());

        let left = solid(1, 2, BLUE);
        let right = solid(1, 2, RED);
        let before = store.generation();
        let committed = store
            .blit_rgba_strict_batch(
                2,
                &[
                    (Rect::new(0, 0, 1, 2), left.as_slice()),
                    (Rect::new(1, 0, 2, 2), right.as_slice()),
                ],
            )
            .unwrap();

        assert_eq!(committed, before + 1, "a tiled frame has one generation");
        assert!(
            store.presentation_fallback.is_none(),
            "a committed replacement must retire its old presentation"
        );
    }

    #[test]
    fn a_bgra_batch_swizzles_every_rect_and_publishes_once() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 1);
        store.map_to_output(1);
        let before_generation = store.generation();
        let before_bytes = store.cache_stats().bytes_from_wire;
        let left = [1, 2, 3, 4];
        let right = [10, 20, 30, 40];

        let generation = store
            .blit_bgra_strict_batch(
                1,
                [
                    (Rect::new(0, 0, 1, 1), left.as_slice()),
                    (Rect::new(1, 0, 2, 1), right.as_slice()),
                ]
                .into_iter(),
            )
            .unwrap();

        assert_eq!(generation, before_generation + 1);
        assert_eq!(store.get(1).unwrap().pixels(), [3, 2, 1, 4, 30, 20, 10, 40]);
        assert_eq!(store.cache_stats().bytes_from_wire, before_bytes + 8);
    }

    #[test]
    fn a_partial_replacement_keeps_the_fallback_when_painted_before_mapping() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 2);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 2)], RED).unwrap();
        store.map_to_output(1);

        store.create(2, 2, 2);
        store
            .blit_rgba_strict(2, Rect::new(0, 0, 1, 2), &solid(1, 2, BLUE))
            .unwrap();
        store.map_to_output(2);

        assert_eq!(
            store.presentation_surface().unwrap().pixels(),
            solid(2, 2, RED),
            "a partial replacement must not cover the last good desktop"
        );
        assert!(
            store.presentation_fallback.is_some(),
            "mapping a partially painted replacement must retain the fallback"
        );

        store.create(3, 2, 2);
        store.map_to_output(3);
        assert_eq!(
            store.presentation_surface().unwrap().pixels(),
            solid(2, 2, RED),
            "a second handoff must preserve the original last good desktop"
        );

        store
            .blit_rgba_strict(3, Rect::new(0, 0, 2, 2), &solid(2, 2, BLUE))
            .unwrap();
        assert!(
            store.presentation_fallback.is_none(),
            "the fallback retires once every replacement pixel is covered"
        );
        assert_eq!(
            store.presentation_surface().unwrap().pixels(),
            solid(2, 2, BLUE)
        );
    }

    #[test]
    fn a_partial_current_replacement_does_not_advance_generation_until_complete() {
        let mut store = SurfaceStore::new();
        store.create(1, 3, 2);
        store.solid_fill(1, &[Rect::new(0, 0, 3, 2)], RED).unwrap();
        store.map_to_output(1);
        store.create(2, 3, 2);
        store.map_to_output(2);

        let before_partial = store.generation();
        store
            .blit_rgba_strict(2, Rect::new(0, 0, 1, 2), &solid(1, 2, BLUE))
            .unwrap();
        assert_eq!(
            store.generation(),
            before_partial,
            "painting an incomplete replacement must not wake the presenter"
        );
        assert_eq!(
            store.presentation_surface().unwrap().pixels(),
            solid(3, 2, RED)
        );

        store
            .blit_rgba_strict(2, Rect::new(1, 0, 3, 2), &solid(2, 2, BLUE))
            .unwrap();
        assert_eq!(
            store.generation(),
            before_partial + 1,
            "completing the replacement must publish exactly one generation"
        );
        assert_eq!(
            store.presentation_surface().unwrap().pixels(),
            solid(3, 2, BLUE)
        );
    }

    #[test]
    fn a_full_adopt_retires_the_replacement_fallback() {
        let mut store = SurfaceStore::new();
        store.create(1, 2, 2);
        store.solid_fill(1, &[Rect::new(0, 0, 2, 2)], RED).unwrap();
        store.map_to_output(1);
        store.create(2, 2, 2);
        store.map_to_output(2);
        assert!(store.presentation_fallback.is_some());

        store.adopt_pixels(2, solid(2, 2, BLUE)).unwrap();

        assert!(store.presentation_fallback.is_none());
        assert_eq!(
            store.presentation_surface().unwrap().pixels(),
            solid(2, 2, BLUE)
        );
    }

    #[test]
    fn bgra_to_rgba_swaps_only_the_colour_channels() {
        let mut buf = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        SurfaceStore::bgra_to_rgba_in_place(&mut buf);
        assert_eq!(buf, vec![3, 2, 1, 4, 7, 6, 5, 8]);
    }

    /// A store with one 8x8 surface holding a 2x2 red square at the origin.
    fn store_with_cached_square() -> SurfaceStore {
        let mut store = SurfaceStore::new();
        store.create(1, 8, 8);
        store.create(2, 8, 8);
        store
            .blit_rgba(1, Rect::new(0, 0, 2, 2), &solid(2, 2, RED), 2)
            .unwrap();
        assert_eq!(
            store.surface_to_cache(1, Rect::new(0, 0, 2, 2), 7),
            Ok(Some((2, 2)))
        );
        store
    }

    #[test]
    fn a_cache_hit_counts_one_per_destination_and_the_bytes_it_painted() {
        let mut store = store_with_cached_square();
        let before = store.cache_stats();
        assert_eq!(before.hits, 0);

        // One command, three destinations: three regions that did not need the wire.
        store
            .cache_to_surface(7, 2, &[(0, 0), (2, 0), (4, 0)])
            .unwrap();

        let s = store.cache_stats();
        assert_eq!(s.hits, 3, "one hit per destination painted");
        assert_eq!(s.misses, 0);
        // 3 squares x 2x2 pixels x 4 bytes.
        assert_eq!(s.bytes_served, 3 * 2 * 2 * BPP as u64);
    }

    #[test]
    fn a_missing_slot_is_counted_as_a_miss_and_not_silently_dropped() {
        let mut store = store_with_cached_square();
        let err = store.cache_to_surface(99, 2, &[(0, 0)]).unwrap_err();
        assert!(matches!(err, SurfaceError::NoSuchCacheSlot(99)));

        let s = store.cache_stats();
        assert_eq!(s.misses, 1);
        assert_eq!(s.hits, 0, "a miss must not also count as a hit");
        assert_eq!(s.bytes_served, 0);
    }

    #[test]
    fn cached_pixels_are_not_also_counted_as_wire_bytes() {
        // The double-count guard: `cache_to_surface` blits through `Surface` directly, so
        // if it ever routes through `SurfaceStore::blit_rgba` the saving would be counted
        // as both served-from-cache and arrived-from-wire, and the effectiveness ratio
        // would silently become meaningless.
        let mut store = store_with_cached_square();
        let wire_after_setup = store.cache_stats().bytes_from_wire;
        // Setup blitted one 2x2 square from the wire.
        assert_eq!(wire_after_setup, 2 * 2 * BPP as u64);

        store.cache_to_surface(7, 2, &[(0, 0), (4, 4)]).unwrap();

        let s = store.cache_stats();
        assert_eq!(
            s.bytes_from_wire, wire_after_setup,
            "painting from the cache must not add wire bytes"
        );
        assert_eq!(s.bytes_served, 2 * 2 * 2 * BPP as u64);
    }

    #[test]
    fn a_blit_that_overhangs_the_surface_counts_only_what_landed() {
        let mut store = SurfaceStore::new();
        store.create(1, 4, 4);
        // Ask to paint 4x4 at (2,2): only the bottom-right 2x2 is inside the surface.
        store
            .blit_rgba(1, Rect::new(2, 2, 6, 6), &solid(4, 4, BLUE), 4)
            .unwrap();
        assert_eq!(
            store.cache_stats().bytes_from_wire,
            2 * 2 * BPP as u64,
            "the requested 4x4 would overcount by four times"
        );
    }

    #[test]
    fn reusing_a_slot_counts_an_eviction_but_a_fresh_slot_does_not() {
        let mut store = store_with_cached_square();
        assert_eq!(
            store.cache_stats().evictions,
            0,
            "first fill is not an evict"
        );

        assert_eq!(
            store.surface_to_cache(1, Rect::new(0, 0, 2, 2), 7),
            Ok(Some((2, 2)))
        );
        assert_eq!(store.cache_stats().evictions, 1, "slot 7 was overwritten");

        assert_eq!(
            store.surface_to_cache(1, Rect::new(0, 0, 2, 2), 8),
            Ok(Some((2, 2)))
        );
        assert_eq!(
            store.cache_stats().evictions,
            1,
            "a different slot displaces nothing"
        );
        assert_eq!(store.cache_stats().entries, 2);
    }

    #[test]
    fn clearing_the_cache_empties_the_entry_count_but_keeps_the_history() {
        let mut store = store_with_cached_square();
        store.cache_to_surface(7, 2, &[(0, 0)]).unwrap();
        store.clear_cache();

        let s = store.cache_stats();
        assert_eq!(s.entries, 0, "nothing is cached any more");
        assert_eq!(
            s.hits, 1,
            "but what the cache did for us this session still happened"
        );
    }
}
