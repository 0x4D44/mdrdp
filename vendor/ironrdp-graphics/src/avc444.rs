//! AVC444 / AVC444v2 YUV combination and RGB conversion (MS-RDPEGFX 3.3.8.3).
//!
//! An AVC444 update carries one or two ordinary H.264 4:2:0 sub-frames: the *main*
//! frame (full-resolution Y plus the even-row/even-column chroma, stored as each 2x2
//! block's average) and an *auxiliary* frame whose planes are a packing container for
//! the remaining chroma samples. This module owns the pure pixel math: combining the
//! decoded 4:2:0 frames into a persistent full-resolution YUV444 buffer, and
//! converting regions of that buffer to RGBA.
//!
//! The packing layouts and the fixed-point RGB matrix are transcribed from FreeRDP's
//! `prim_YUV.c` / `prim_internal.h` (master, 2026-08-16; 3.27.1 installed locally),
//! the field-proven reference against Windows servers. Two deliberate divergences:
//! FreeRDP walks each region ROI-relative, which is only phase-correct for aligned
//! rect origins; this module maps every destination position through **absolute frame
//! coordinates**, which agrees with FreeRDP on the aligned rects Windows actually
//! sends and stays correct for arbitrary ones. And FreeRDP's luma pass replicates
//! the main frame's averaged chroma over previously delivered aux samples, which
//! visibly pumps the colour of chroma-detailed content under Windows' steady-state
//! LC=1/LC=2 alternation; this module's luma pass preserves delivered chroma (see
//! [`Yuv444Buffer::apply_luma`]) — and, where a block's average jumped under a
//! luma-only update (the preserved samples are then one chroma catch-up behind),
//! paints the flat average until the catch-up lands or luma returns to the last
//! aux-confirmed average (see [`Yuv444Buffer::to_rgba_into`]). Every source read
//! is bounds-checked:
//! a destination position whose source sample does not exist in the decoded frame is
//! left unwritten (it keeps its previous or luma-replicated value) rather than
//! shearing or panicking — wire data is untrusted.

use ironrdp_pdu::geometry::ExclusiveRectangle;
use std::collections::HashMap;

/// A decoded H.264 4:2:0 frame as tightly packed planes.
///
/// Row widths are the *pixel* widths (`width` for Y, `(width+1)/2` for U/V) — never a
/// pixel-buffer pitch, which may include alignment padding that is not image data.
#[derive(Debug, Clone, Default)]
pub struct Yuv420Frame {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

impl Yuv420Frame {
    /// Row width of the U and V planes.
    pub fn uv_row(&self) -> usize {
        self.width.div_ceil(2)
    }

    /// Height of the U and V planes.
    pub fn uv_height(&self) -> usize {
        self.height.div_ceil(2)
    }

    /// Whether the plane lengths actually hold `width * height` worth of samples.
    ///
    /// The combination passes index by `width`/`height` and would panic on a frame
    /// whose vectors are shorter than its claimed dimensions. The decoders in this
    /// repo always produce consistent frames, but `decode_yuv420` is a public trait
    /// method — callers gate on this before combining.
    pub fn is_well_formed(&self) -> bool {
        self.y.len() >= self.width * self.height && {
            let uv = self.uv_row() * self.uv_height();
            self.u.len() >= uv && self.v.len() >= uv
        }
    }
}

/// Round a width up to the next multiple of 32 (the v2 packing geometry — see
/// [`Yuv444Buffer::apply_chroma_v2`]).
pub fn align32(width: usize) -> usize {
    width.div_ceil(32) * 32
}

/// The persistent full-resolution YUV444 frame for one surface.
///
/// All three planes are `width * height`. The buffer outlives individual updates:
/// LC=1 (luma-only) and LC=2 (chroma-only) frames update only part of the state, so
/// the rest must persist between PDUs.
#[derive(Debug, Clone)]
pub struct Yuv444Buffer {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    width: usize,
    height: usize,
    /// One bit per 2x2 block (row-major, `width.div_ceil(2)` per row): set once a
    /// chroma pass has delivered true odd-position samples for the whole block.
    ///
    /// The luma pass consults this to decide what its main-frame chroma may touch.
    /// Windows alternates luma-only (LC=1) and chroma-only (LC=2) updates in steady
    /// state, and the encoder's contract is that a luma-only update leaves
    /// previously delivered chroma detail intact — it re-sends chroma only when
    /// chroma changed. Replicating the luma frame's 2x2 averages over the odd
    /// positions (FreeRDP behaviour, and this module's until 2026-08-19) degrades
    /// every chroma'd region back to 4:2:0 on each luma pass, which oscillates the
    /// colour of chroma-detailed content (measured live against temper: dithered
    /// tiles pumped between two tones, 33k of 48k probe pixels jumping by up to 98
    /// RGB units at every LC transition — the deliberate FreeRDP divergence here).
    chroma_seen: Vec<u64>,
    /// One bit per 2x2 block (same indexing as `chroma_seen`): set when a luma
    /// pass delivers a block average that differs from the last aux-confirmed
    /// average by more than [`STALE_AVG_DELTA`] in a chroma'd block — i.e. the
    /// block's colour genuinely changed under a luma-only update, so its preserved
    /// odd-position samples are one chroma catch-up behind (the encoder ships the
    /// matching aux frame later; observed next-frame to ~1.4 s against temper).
    /// Painting such a block by reconstruction overshoots (`4*new_avg - 3*stale`)
    /// into a hue that was never on screen — a retreating dark-yellow bar flashed
    /// blue (MDR-BUG-FLUX-00010) — so [`Self::to_rgba_into`] paints stale blocks
    /// with the flat average until a chroma pass covers the block or luma returns
    /// within the last aux-confirmed average's threshold and clears the bit.
    chroma_stale: Vec<u64>,
    /// The last luma-derived chroma average confirmed by an auxiliary pass for
    /// each 2x2 block. A later luma pass can return to this signature without a
    /// new aux frame, so it can safely clear [`chroma_stale`].
    chroma_confirmed_avg: Vec<[u8; 2]>,
    /// One bit per 2x2 block: set after a luma pass has supplied a real chroma
    /// average. LC=2 can legally arrive first, so the neutral buffer fill cannot
    /// serve as an aux-confirmed baseline until this bit is set.
    luma_avg_seen: Vec<u64>,
    /// One bit per surface pixel: set after a luma pass supplied that pixel's Y
    /// value. Chroma-only updates may touch only 2x2 blocks whose in-surface pixels
    /// all have a current luma baseline.
    luma_valid: Vec<u64>,
    /// One bit per 2x2 block whose in-surface luma pixels are all valid.
    valid_luma_blocks: Vec<u64>,
    /// Number of blocks not represented by `valid_luma_blocks`. This makes the
    /// steady-state full-surface chroma path O(rectangles), not O(surface pixels).
    invalid_luma_blocks: usize,
}

/// Chroma-average delta (per channel, from the last aux-confirmed average) above
/// which a chroma'd block's preserved odd samples are treated as stale.
///
/// Sized from the 32 captured temper payloads behind MDR-BUG-FLUX-00007/00010
/// (`examples/avcreplay.rs`, `avgd` stats): steady-state re-encodes of unchanged
/// content jitter the delivered averages by at most 6, while a genuine content
/// change moves them by 41+ — 10 sits in the empty gap.
const STALE_AVG_DELTA: u8 = 10;

impl Yuv444Buffer {
    /// A black frame (Y=0, U=V=128) of the given dimensions.
    pub fn new(width: u16, height: u16) -> Self {
        let (w, h) = (usize::from(width), usize::from(height));
        let blocks = w.div_ceil(2) * h.div_ceil(2);
        Self {
            y: vec![0; w * h],
            u: vec![128; w * h],
            v: vec![128; w * h],
            width: w,
            height: h,
            chroma_seen: vec![0; blocks.div_ceil(64)],
            chroma_stale: vec![0; blocks.div_ceil(64)],
            chroma_confirmed_avg: vec![[128, 128]; blocks],
            luma_avg_seen: vec![0; blocks.div_ceil(64)],
            luma_valid: vec![0; (w * h).div_ceil(64)],
            valid_luma_blocks: vec![0; blocks.div_ceil(64)],
            invalid_luma_blocks: blocks,
        }
    }

    /// Bitset word index and mask for the 2x2 block containing `(dx, dy)`.
    fn block_bit(&self, dx: usize, dy: usize) -> (usize, u64) {
        let idx = (dy / 2) * self.width.div_ceil(2) + dx / 2;
        (idx / 64, 1 << (idx % 64))
    }

    fn pixel_bit(&self, dx: usize, dy: usize) -> (usize, u64) {
        let idx = dy * self.width + dx;
        (idx / 64, 1 << (idx % 64))
    }

    fn luma_valid_at(&self, dx: usize, dy: usize) -> bool {
        let (word, mask) = self.pixel_bit(dx, dy);
        self.luma_valid[word] & mask != 0
    }

    fn block_has_valid_luma(&self, bx: usize, by: usize) -> bool {
        let idx = by * self.width.div_ceil(2) + bx;
        self.valid_luma_blocks[idx / 64] & (1 << (idx % 64)) != 0
    }

    fn promote_luma_block_if_complete(&mut self, bx: usize, by: usize) {
        if self.block_has_valid_luma(bx, by) {
            return;
        }
        let left = bx * 2;
        let top = by * 2;
        let complete = (top..(top + 2).min(self.height)).all(|y| {
            (left..(left + 2).min(self.width)).all(|x| self.luma_valid_at(x, y))
        });
        if complete {
            let idx = by * self.width.div_ceil(2) + bx;
            self.valid_luma_blocks[idx / 64] |= 1 << (idx % 64);
            self.invalid_luma_blocks = self.invalid_luma_blocks.saturating_sub(1);
        }
    }

    fn set_valid_luma_block_range(&mut self, by: usize, start_bx: usize, end_bx: usize) {
        if start_bx >= end_bx {
            return;
        }
        let blocks_per_row = self.width.div_ceil(2);
        let mut start = by * blocks_per_row + start_bx;
        let end = by * blocks_per_row + end_bx;
        while start < end {
            let word = start / 64;
            let past = end.min((word + 1) * 64);
            let bits = past - start;
            let shift = start % 64;
            let mask = if bits == 64 {
                u64::MAX
            } else {
                ((1u64 << bits) - 1) << shift
            };
            let newly_valid = (!self.valid_luma_blocks[word] & mask).count_ones() as usize;
            self.valid_luma_blocks[word] |= mask;
            self.invalid_luma_blocks = self.invalid_luma_blocks.saturating_sub(newly_valid);
            start = past;
        }
    }

    fn mark_luma_rect_valid(&mut self, left: usize, top: usize, right: usize, bottom: usize) {
        if left >= right || top >= bottom {
            return;
        }
        let (first_full_col, first_full_row) = (left.div_ceil(2), top.div_ceil(2));
        let (past_full_col, past_full_row) = (right / 2, bottom / 2);
        for by in first_full_row..past_full_row {
            self.set_valid_luma_block_range(by, first_full_col, past_full_col);
        }

        let (first_col, first_row) = (left / 2, top / 2);
        let (past_col, past_row) = (right.div_ceil(2), bottom.div_ceil(2));
        if !top.is_multiple_of(2) {
            for bx in first_col..past_col {
                self.promote_luma_block_if_complete(bx, first_row);
            }
        }
        if !bottom.is_multiple_of(2) {
            for bx in first_col..past_col {
                self.promote_luma_block_if_complete(bx, past_row - 1);
            }
        }
        if !left.is_multiple_of(2) {
            for by in first_row..past_row {
                self.promote_luma_block_if_complete(first_col, by);
            }
        }
        if !right.is_multiple_of(2) {
            for by in first_row..past_row {
                self.promote_luma_block_if_complete(past_col - 1, by);
            }
        }
    }

    /// Whether the 2x2 block containing `(dx, dy)` has ever received true chroma.
    fn chroma_seen_at(&self, dx: usize, dy: usize) -> bool {
        let (word, mask) = self.block_bit(dx, dy);
        self.chroma_seen[word] & mask != 0
    }

    /// Whether the block's preserved odd chroma is a catch-up behind its average.
    fn chroma_stale_at(&self, dx: usize, dy: usize) -> bool {
        let (word, mask) = self.block_bit(dx, dy);
        self.chroma_stale[word] & mask != 0
    }

    /// Record a block whose in-surface auxiliary samples were delivered in this pass.
    fn promote_chroma_block(&mut self, idx: usize, bx: usize, by: usize) {
        let word_mask = 1 << (idx % 64);
        self.chroma_seen[idx / 64] |= word_mask;
        self.chroma_stale[idx / 64] &= !word_mask;
        if self.luma_avg_seen[idx / 64] & word_mask != 0 {
            let i = (by * 2) * self.width + bx * 2;
            self.chroma_confirmed_avg[idx] = [self.u[i], self.v[i]];
        }
    }

    fn record_partial_chroma(
        &mut self,
        bx: usize,
        by: usize,
        bounds: (usize, usize, usize, usize),
        partial: &mut HashMap<usize, u8>,
    ) {
        let (left, top, right, bottom) = bounds;
        let idx = by * self.width.div_ceil(2) + bx;
        let x = bx * 2;
        let y = by * 2;
        let mut samples = partial.get(&idx).copied().unwrap_or_default();
        let mut required = 0;
        for (bit, dx, dy) in [(1, x + 1, y), (2, x, y + 1), (4, x + 1, y + 1)] {
            if dx < self.width && dy < self.height {
                required |= bit;
                if dx >= left && dx < right && dy >= top && dy < bottom {
                    samples |= bit;
                }
            }
        }
        if required != 0 && samples & required == required {
            partial.remove(&idx);
            self.promote_chroma_block(idx, bx, by);
        } else if required == 0 {
            partial.remove(&idx);
        } else {
            partial.insert(idx, samples);
        }
    }

    /// Add one clipped rectangle's in-surface auxiliary samples per 2x2 block to
    /// this chroma pass's coverage, promoting blocks once the union is complete.
    ///
    /// Keeping partial coverage local to one pass matters: adjacent rectangles in
    /// one PDU may split a block and still deliver all its samples, while a later
    /// partial update must not clear a stale mark using samples from an old frame.
    fn mark_chroma_seen(
        &mut self,
        left: usize,
        top: usize,
        right: usize,
        bottom: usize,
        partial: &mut HashMap<usize, u8>,
    ) {
        if left >= right || top >= bottom {
            return;
        }
        let blocks_per_row = self.width.div_ceil(2);
        let (first_full_col, first_full_row) = (left.div_ceil(2), top.div_ceil(2));
        let (past_full_col, past_full_row) = (right / 2, bottom / 2);
        for by in first_full_row..past_full_row {
            for bx in first_full_col..past_full_col {
                self.promote_chroma_block(by * blocks_per_row + bx, bx, by);
            }
        }

        let (first_partial_col, first_partial_row) = (left / 2, top / 2);
        let (past_partial_col, past_partial_row) = (right.div_ceil(2), bottom.div_ceil(2));
        let bounds = (left, top, right, bottom);
        if !top.is_multiple_of(2) {
            for bx in first_partial_col..past_partial_col {
                self.record_partial_chroma(bx, first_partial_row, bounds, partial);
            }
        }
        if !bottom.is_multiple_of(2) {
            for bx in first_partial_col..past_partial_col {
                self.record_partial_chroma(bx, past_partial_row - 1, bounds, partial);
            }
        }
        if !left.is_multiple_of(2) {
            for by in first_partial_row..past_partial_row {
                self.record_partial_chroma(first_partial_col, by, bounds, partial);
            }
        }
        if !right.is_multiple_of(2) {
            for by in first_partial_row..past_partial_row {
                self.record_partial_chroma(past_partial_col - 1, by, bounds, partial);
            }
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// The Y, U and V planes (each `width * height`), for differential testing.
    pub fn planes(&self) -> (&[u8], &[u8], &[u8]) {
        (&self.y, &self.u, &self.v)
    }

    /// Build a buffer from existing full-resolution planes (differential testing).
    ///
    /// # Panics
    /// Panics if any plane is not `width * height` bytes.
    pub fn from_planes(y: Vec<u8>, u: Vec<u8>, v: Vec<u8>, width: usize, height: usize) -> Self {
        assert_eq!(y.len(), width * height);
        assert_eq!(u.len(), width * height);
        assert_eq!(v.len(), width * height);
        let blocks = width.div_ceil(2) * height.div_ceil(2);
        Self {
            y,
            u,
            v,
            width,
            height,
            chroma_seen: vec![0; blocks.div_ceil(64)],
            chroma_stale: vec![0; blocks.div_ceil(64)],
            chroma_confirmed_avg: vec![[128, 128]; blocks],
            luma_avg_seen: vec![0; blocks.div_ceil(64)],
            luma_valid: vec![u64::MAX; (width * height).div_ceil(64)],
            valid_luma_blocks: vec![u64::MAX; blocks.div_ceil(64)],
            invalid_luma_blocks: 0,
        }
    }

    /// Invalidate every 2x2 block touched by a non-AVC destination mutation.
    ///
    /// Chroma reconstruction crosses pixel boundaries inside a block, so a partial
    /// mutation retires the whole block's baseline while preserving disjoint blocks.
    pub fn invalidate(&mut self, rects: &[ExclusiveRectangle]) {
        let blocks_per_row = self.width.div_ceil(2);
        for rect in rects {
            let (left, top, right, bottom) = self.clip(rect);
            if left >= right || top >= bottom {
                continue;
            }
            for by in top / 2..bottom.div_ceil(2) {
                for bx in left / 2..right.div_ceil(2) {
                    let block = by * blocks_per_row + bx;
                    let block_mask = 1 << (block % 64);
                    self.chroma_seen[block / 64] &= !block_mask;
                    self.chroma_stale[block / 64] &= !block_mask;
                    self.luma_avg_seen[block / 64] &= !block_mask;
                    self.chroma_confirmed_avg[block] = [128, 128];
                    if self.valid_luma_blocks[block / 64] & block_mask != 0 {
                        self.valid_luma_blocks[block / 64] &= !block_mask;
                        self.invalid_luma_blocks = self.invalid_luma_blocks.saturating_add(1);
                    }
                    for y in by * 2..(by * 2 + 2).min(self.height) {
                        for x in bx * 2..(bx * 2 + 2).min(self.width) {
                            let (word, mask) = self.pixel_bit(x, y);
                            self.luma_valid[word] &= !mask;
                        }
                    }
                }
            }
        }
    }

    /// Intersect update rectangles with 2x2 blocks that have a current luma baseline.
    /// Adjacent block-row runs are coalesced so a full-surface update remains one rect.
    pub fn valid_chroma_rects(
        &self,
        rects: &[ExclusiveRectangle],
    ) -> Option<Vec<ExclusiveRectangle>> {
        const MAX_FRAGMENTED_RECTS: usize = 1024;
        if self.invalid_luma_blocks == 0 {
            return Some(
                rects
                .iter()
                .filter_map(|rect| {
                    let (left, top, right, bottom) = self.clip(rect);
                    (left < right && top < bottom).then(|| ExclusiveRectangle {
                        left: u16::try_from(left).expect("surface coordinate is u16"),
                        top: u16::try_from(top).expect("surface coordinate is u16"),
                        right: u16::try_from(right).expect("surface coordinate is u16"),
                        bottom: u16::try_from(bottom).expect("surface coordinate is u16"),
                    })
                })
                .collect(),
            );
        }
        let mut out: Vec<ExclusiveRectangle> = Vec::new();
        for rect in rects {
            let (left, top, right, bottom) = self.clip(rect);
            if left >= right || top >= bottom {
                continue;
            }
            let mut previous_runs: HashMap<(u16, u16), usize> = HashMap::new();
            let mut current_runs: HashMap<(u16, u16), usize> = HashMap::new();
            for by in top / 2..bottom.div_ceil(2) {
                let row_top = top.max(by * 2);
                let row_bottom = bottom.min(by * 2 + 2);
                let mut bx = left / 2;
                let past_bx = right.div_ceil(2);
                current_runs.clear();
                while bx < past_bx {
                    while bx < past_bx && !self.block_has_valid_luma(bx, by) {
                        bx += 1;
                    }
                    let run_start = bx;
                    while bx < past_bx && self.block_has_valid_luma(bx, by) {
                        bx += 1;
                    }
                    if run_start == bx {
                        continue;
                    }
                    let run_left = left.max(run_start * 2);
                    let run_right = right.min(bx * 2);
                    let left_u16 = u16::try_from(run_left).expect("surface coordinate is u16");
                    let right_u16 = u16::try_from(run_right).expect("surface coordinate is u16");
                    let row_top_u16 = u16::try_from(row_top).expect("surface coordinate is u16");
                    let row_bottom_u16 =
                        u16::try_from(row_bottom).expect("surface coordinate is u16");
                    let key = (left_u16, right_u16);
                    if let Some(&index) = previous_runs.get(&key) {
                        out[index].bottom = row_bottom_u16;
                        current_runs.insert(key, index);
                    } else {
                        out.push(ExclusiveRectangle {
                            left: left_u16,
                            top: row_top_u16,
                            right: right_u16,
                            bottom: row_bottom_u16,
                        });
                        if out.len() > MAX_FRAGMENTED_RECTS {
                            return None;
                        }
                        current_runs.insert(key, out.len() - 1);
                    }
                }
                core::mem::swap(&mut previous_runs, &mut current_runs);
            }
        }
        Some(out)
    }

    /// Clip a rect to this buffer, returning half-open pixel ranges.
    fn clip(&self, rect: &ExclusiveRectangle) -> (usize, usize, usize, usize) {
        let left = usize::from(rect.left).min(self.width);
        let top = usize::from(rect.top).min(self.height);
        let right = usize::from(rect.right).min(self.width);
        let bottom = usize::from(rect.bottom).min(self.height);
        (left, top, right, bottom)
    }

    /// The luma pass: copy Y, and land the main frame's 2x2-subsampled chroma (the
    /// encoder's block averages) where it belongs.
    ///
    /// Where a block has already received true odd-position chroma from an aux
    /// frame, the average is written **only to the even/even position** (its
    /// MS-RDPEGFX home, B2/B3) and the delivered samples persist — Windows sends
    /// luma-only (LC=1) updates precisely when chroma did not change, so
    /// overwriting the odd positions with averages (FreeRDP behaviour) makes every
    /// LC=1/LC=2 alternation visibly pump the colour of chroma-detailed content
    /// (see `chroma_seen`). On blocks no chroma pass has covered yet, the average
    /// is replicated into all four positions as before, so a surface painted ahead
    /// of its first chroma pass degrades to 4:2:0 rather than to neutral grey. The
    /// RGB conversion reconstructs the even/even sample from the average either
    /// way (see [`Self::to_rgba_into`]).
    ///
    /// A chroma'd block whose incoming average differs from the last aux-confirmed
    /// average by more than [`STALE_AVG_DELTA`] is additionally marked stale (see
    /// `chroma_stale`): its content changed under a luma-only update, so the
    /// preserved odd samples describe the previous content until the encoder's
    /// chroma catch-up lands. If the average returns within that same threshold,
    /// the stale mark is cleared without requiring another aux frame.
    pub fn apply_luma(&mut self, main: &Yuv420Frame, rects: &[ExclusiveRectangle]) {
        let uv_row = main.uv_row();
        for rect in rects {
            let (left, top, right, bottom) = self.clip(rect);
            for dy in top..bottom.min(main.height) {
                let src_right = right.min(main.width);
                if left < src_right {
                    let dst = &mut self.y[dy * self.width + left..dy * self.width + src_right];
                    dst.copy_from_slice(
                        &main.y[dy * main.width + left..dy * main.width + src_right],
                    );
                }
                let sy = dy / 2;
                for dx in left..src_right {
                    let (valid_word, valid_mask) = self.pixel_bit(dx, dy);
                    self.luma_valid[valid_word] |= valid_mask;
                    let odd_position = dx % 2 != 0 || dy % 2 != 0;
                    let seen = self.chroma_seen_at(dx, dy);
                    if odd_position && seen {
                        continue;
                    }
                    let s = sy * uv_row + dx / 2;
                    let i = dy * self.width + dx;
                    if !odd_position {
                        let (word, mask) = self.block_bit(dx, dy);
                        let block = (dy / 2) * self.width.div_ceil(2) + dx / 2;
                        if !seen || self.luma_avg_seen[word] & mask == 0 {
                            // Before aux arrives, keep the candidate baseline at
                            // the latest luma average. If aux arrived first, this
                            // first real average establishes the baseline without
                            // falsely declaring the delivered detail stale.
                            self.chroma_confirmed_avg[block] = [main.u[s], main.v[s]];
                            self.luma_avg_seen[word] |= mask;
                            self.chroma_stale[word] &= !mask;
                        } else {
                            // Even/even of a chroma'd block: an average that differs
                            // from the aux-confirmed state means the content changed
                            // and the preserved odd samples are now a chroma catch-up
                            // behind — mark the block so paint falls back to the flat
                            // average (see `chroma_stale`) instead of reconstructing a
                            // wrong hue.
                            let confirmed = self.chroma_confirmed_avg[block];
                            let confirmed_delta = confirmed[0]
                                .abs_diff(main.u[s])
                                .max(confirmed[1].abs_diff(main.v[s]));
                            if confirmed_delta <= STALE_AVG_DELTA {
                                self.chroma_stale[word] &= !mask;
                            } else {
                                self.chroma_stale[word] |= mask;
                            }
                        }
                    }
                    self.u[i] = main.u[s];
                    self.v[i] = main.v[s];
                }
            }
        }
        for rect in rects {
            let (left, top, right, bottom) = self.clip(rect);
            self.mark_luma_rect_valid(left, top, right.min(main.width), bottom.min(main.height));
        }
    }

    /// The AVC444 (v1) chroma pass.
    ///
    /// Aux packing (absolute frame coordinates, verified against FreeRDP
    /// `general_ChromaV1ToYUV444` and the encoder-side split):
    /// - dst U row `2k+1` <- aux Y row `(k/8)*16 + k%8`, dst V row `2k+1` <- aux Y
    ///   row `(k/8)*16 + 8 + k%8` (full width) — the aux Y plane interleaves U and V
    ///   rows in 16-row blocks;
    /// - dst U/V even row `2y`, odd column `2x+1` <- aux U/V planes at `[y][x]`.
    pub fn apply_chroma_v1(&mut self, aux: &Yuv420Frame, rects: &[ExclusiveRectangle]) {
        let uv_row = aux.uv_row();
        let uv_height = aux.uv_height();
        let mut partial = HashMap::new();
        for rect in rects {
            let (left, top, right, bottom) = self.clip(rect);
            // Mark only rows whose aux sources exist: under SPS cropping the last
            // 16-row block's V rows are absent (see the bounds check below), and a
            // block marked chroma'd without its samples written would pin stale
            // values against future luma passes. v_row is strictly increasing in k,
            // so the first missing source bounds everything after it.
            let mut mark_bottom = bottom;
            let mut dy = if top % 2 == 0 { top + 1 } else { top };
            while dy < mark_bottom {
                let k = (dy - 1) / 2;
                let v_row = (k / 8) * 16 + 8 + k % 8;
                if v_row >= aux.height {
                    mark_bottom = dy;
                }
                dy += 2;
            }
            self.mark_chroma_seen(
                left,
                top,
                right.min(aux.width),
                mark_bottom,
                &mut partial,
            );
            for dy in top..bottom {
                if dy % 2 == 1 {
                    let k = (dy - 1) / 2;
                    let u_row = (k / 8) * 16 + k % 8;
                    // The V rows of the last 16-row block live in the coded frame's
                    // macroblock padding; an SPS-cropped decode does not contain
                    // them, so the bottom few odd rows then keep their replicated
                    // averages (bounds check below). Graceful and uncounted — the
                    // reference client over-reads its plane in the same case.
                    let v_row = u_row + 8;
                    let src_right = right.min(aux.width);
                    for dx in left..src_right {
                        if u_row < aux.height {
                            self.u[dy * self.width + dx] = aux.y[u_row * aux.width + dx];
                        }
                        if v_row < aux.height {
                            self.v[dy * self.width + dx] = aux.y[v_row * aux.width + dx];
                        }
                    }
                } else {
                    let sy = dy / 2;
                    if sy >= uv_height {
                        continue;
                    }
                    let mut dx = if left % 2 == 1 { left } else { left + 1 };
                    while dx < right {
                        let sx = (dx - 1) / 2;
                        if sx < uv_row {
                            self.u[dy * self.width + dx] = aux.u[sy * uv_row + sx];
                            self.v[dy * self.width + dx] = aux.v[sy * uv_row + sx];
                        }
                        dx += 2;
                    }
                }
            }
        }
    }

    /// The AVC444v2 chroma pass.
    ///
    /// Aux packing (absolute frame coordinates, verified against FreeRDP
    /// `general_ChromaV2ToYUV444`), where `W = min(align32(surface width), aux
    /// width)` — the 32-aligned split geometry the encoder packed against, capped at
    /// what the decoded plane holds (FreeRDP `yuv.c` `alignedWidth`):
    /// - dst U `[y][2x+1]` <- aux Y `[y][x]`; dst V `[y][2x+1]` <- aux Y `[y][W/2+x]`
    ///   (all rows — the aux Y plane holds U and V halves side by side);
    /// - dst U `[2y+1][4x]` <- aux U `[y][x]`; dst V `[2y+1][4x]` <- aux U `[y][W/4+x]`;
    /// - dst U `[2y+1][4x+2]` <- aux V `[y][x]`; dst V `[2y+1][4x+2]` <- aux V `[y][W/4+x]`.
    pub fn apply_chroma_v2(&mut self, aux: &Yuv420Frame, rects: &[ExclusiveRectangle]) {
        let w = align32(self.width).min(aux.width);
        let uv_row = aux.uv_row();
        let uv_height = aux.uv_height();
        let mut partial = HashMap::new();
        for rect in rects {
            let (left, top, right, bottom) = self.clip(rect);
            // Rows at or past the aux height get no odd-column samples (bounds
            // check below), so only blocks above it are truly chroma'd.
            self.mark_chroma_seen(
                left,
                top,
                right.min(aux.width),
                bottom.min(aux.height),
                &mut partial,
            );
            for dy in top..bottom {
                // Odd columns, every row: from the aux Y plane's two halves.
                if dy < aux.height {
                    let mut dx = if left % 2 == 1 { left } else { left + 1 };
                    while dx < right {
                        let sx = dx / 2;
                        if sx < aux.width {
                            self.u[dy * self.width + dx] = aux.y[dy * aux.width + sx];
                        }
                        if w / 2 + sx < aux.width {
                            self.v[dy * self.width + dx] = aux.y[dy * aux.width + w / 2 + sx];
                        }
                        dx += 2;
                    }
                }
                // Even columns of odd rows: from the aux U and V planes' two halves.
                if dy % 2 == 1 {
                    let sy = (dy - 1) / 2;
                    if sy >= uv_height {
                        continue;
                    }
                    let mut dx = if left % 2 == 0 { left } else { left + 1 };
                    while dx < right {
                        let (plane, sx) = if dx % 4 == 0 {
                            (&aux.u, dx / 4)
                        } else {
                            (&aux.v, (dx - 2) / 4)
                        };
                        if sx < uv_row {
                            self.u[dy * self.width + dx] = plane[sy * uv_row + sx];
                        }
                        if w / 4 + sx < uv_row {
                            self.v[dy * self.width + dx] = plane[sy * uv_row + w / 4 + sx];
                        }
                        dx += 2;
                    }
                }
            }
        }
    }

    /// Convert one region of the 444 buffer to RGBA8888, into a caller-owned buffer.
    ///
    /// `out` is cleared and filled row-major with `rect.width() * rect.height()`
    /// RGBA pixels (the rect is clipped to the buffer first).
    ///
    /// The even/even chroma sample of each 2x2 block holds the encoder's block
    /// *average*; the true sample is reconstructed as `4*avg - p01 - p10 - p11`,
    /// kept only when it moves the value by at least 30 (FreeRDP's
    /// `CONDITIONAL_CLIP` denoise heuristic — small deviations keep the smoother
    /// average). Positions without a complete 2x2 block (last row/column of an
    /// odd-sized frame) use the stored sample as-is. Neighbors come from the full
    /// buffer, so rect edges do not change the result.
    ///
    /// A block marked stale (content changed under a luma-only update — see
    /// `chroma_stale`) is painted entirely from its even/even average: its odd
    /// samples still describe the previous content, so showing them — or
    /// reconstructing against them, which overshoots into a hue that was never
    /// on screen — renders wrong colours for the whole catch-up gap
    /// (MDR-BUG-FLUX-00010's one-frame blue flash, and whole-window colour
    /// casts on large redraws). The flat average is the correct hue at 4:2:0
    /// fidelity; full detail returns when the chroma pass clears the mark or the
    /// luma average returns to the last aux-confirmed value.
    pub fn to_rgba_into(&self, rect: &ExclusiveRectangle, out: &mut Vec<u8>) {
        let (left, top, right, bottom) = self.clip(rect);
        out.clear();
        out.reserve((right - left) * (bottom - top) * 4);
        for y in top..bottom {
            for x in left..right {
                let i = y * self.width + x;
                let (mut u, mut v) = (self.u[i], self.v[i]);
                if self.chroma_stale_at(x, y) {
                    let b = (y & !1) * self.width + (x & !1);
                    u = self.u[b];
                    v = self.v[b];
                } else if self.chroma_seen_at(x, y)
                    && x % 2 == 0
                    && y % 2 == 0
                    && x + 1 < self.width
                    && y + 1 < self.height
                {
                    u = reconstruct_chroma(
                        u,
                        self.u[i + 1],
                        self.u[i + self.width],
                        self.u[i + self.width + 1],
                    );
                    v = reconstruct_chroma(
                        v,
                        self.v[i + 1],
                        self.v[i + self.width],
                        self.v[i + self.width + 1],
                    );
                }
                out.extend_from_slice(&yuv_to_rgba(self.y[i], u, v));
            }
        }
    }
}

/// Invert the encoder's 2x2 chroma averaging for the top-left sample.
fn reconstruct_chroma(avg: u8, p01: u8, p10: u8, p11: u8) -> u8 {
    let recon = 4 * i32::from(avg) - i32::from(p01) - i32::from(p10) - i32::from(p11);
    let clipped = recon.clamp(0, 255) as u8;
    if clipped.abs_diff(avg) < 30 {
        avg
    } else {
        clipped
    }
}

/// Full-range BT.709 fixed-point YUV -> RGBA (FreeRDP `prim_internal.h`, verbatim
/// coefficients).
#[inline]
fn yuv_to_rgba(y: u8, u: u8, v: u8) -> [u8; 4] {
    let c = 256 * i32::from(y);
    let d = i32::from(u) - 128;
    let e = i32::from(v) - 128;
    let r = (c + 403 * e) >> 8;
    let g = (c - 48 * d - 120 * e) >> 8;
    let b = (c + 475 * d) >> 8;
    [
        r.clamp(0, 255) as u8,
        g.clamp(0, 255) as u8,
        b.clamp(0, 255) as u8,
        0xFF,
    ]
}

/// Convert a plain 4:2:0 frame straight to RGBA8888 (the AVC420 path).
///
/// Chroma is 2x2-replicated; no reconstruction filter (a 420 frame's chroma is the
/// final signal, there is no auxiliary detail to reconstruct against).
pub fn yuv420_to_rgba(frame: &Yuv420Frame) -> Vec<u8> {
    let uv_row = frame.uv_row();
    let mut out = Vec::with_capacity(frame.width * frame.height * 4);
    for y in 0..frame.height {
        for x in 0..frame.width {
            let s = (y / 2) * uv_row + x / 2;
            out.extend_from_slice(&yuv_to_rgba(
                frame.y[y * frame.width + x],
                frame.u[s],
                frame.v[s],
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: u16, top: u16, right: u16, bottom: u16) -> ExclusiveRectangle {
        ExclusiveRectangle {
            left,
            top,
            right,
            bottom,
        }
    }

    /// A 4:2:0 frame whose every sample encodes its own plane and position, so a
    /// wrong source index shows up as a wrong value, not a coincidental match.
    fn tagged_420(width: usize, height: usize, tag: u8) -> Yuv420Frame {
        let uv_row = width.div_ceil(2);
        let uv_h = height.div_ceil(2);
        Yuv420Frame {
            y: (0..width * height)
                .map(|i| (i as u8).wrapping_add(tag))
                .collect(),
            u: (0..uv_row * uv_h)
                .map(|i| (i as u8).wrapping_mul(3).wrapping_add(tag))
                .collect(),
            v: (0..uv_row * uv_h)
                .map(|i| (i as u8).wrapping_mul(7).wrapping_add(tag))
                .collect(),
            width,
            height,
        }
    }

    fn uniform_420(width: usize, height: usize, u: u8, v: u8) -> Yuv420Frame {
        let uv_len = width.div_ceil(2) * height.div_ceil(2);
        Yuv420Frame {
            y: vec![128; width * height],
            u: vec![u; uv_len],
            v: vec![v; uv_len],
            width,
            height,
        }
    }

    #[test]
    fn the_luma_pass_copies_y_and_replicates_the_averaged_chroma() {
        let main = tagged_420(4, 4, 0);
        let mut buf = Yuv444Buffer::new(4, 4);
        buf.apply_luma(&main, &[rect(0, 0, 4, 4)]);

        // Y is a straight copy.
        assert_eq!(&buf.y, &main.y);
        // Chroma position (3,2): source is U[2/2=1][3/2=1] = index 1*2+1 = 3 -> 3*3=9.
        assert_eq!(buf.u[2 * 4 + 3], main.u[3]);
        assert_eq!(buf.u[2 * 4 + 3], 9);
        // All four positions of the 2x2 block at (2..4, 2..4) hold the same sample.
        for (x, y) in [(2, 2), (3, 2), (2, 3), (3, 3)] {
            assert_eq!(buf.u[y * 4 + x], main.u[3]);
            assert_eq!(buf.v[y * 4 + x], main.v[3]);
        }
    }

    /// The regression for the LC=1/LC=2 colour pumping (measured live against
    /// temper 2026-08-19): once an aux frame has delivered true odd-position
    /// chroma, a later luma-only pass must update Y and the even/even average but
    /// leave the delivered samples alone.
    #[test]
    fn a_luma_pass_preserves_previously_delivered_chroma_detail() {
        let aux = tagged_420(8, 8, 0);
        let mut buf = Yuv444Buffer::new(8, 8);
        buf.apply_chroma_v2(&aux, &[rect(0, 0, 8, 8)]);
        // Snapshot the delivered odd-position samples.
        let u_before = buf.u.clone();
        let v_before = buf.v.clone();

        let main = tagged_420(8, 8, 7);
        buf.apply_luma(&main, &[rect(0, 0, 8, 8)]);

        // Y is fully refreshed.
        assert_eq!(&buf.y, &main.y);
        for y in 0..8 {
            for x in 0..8 {
                let i = y * 8 + x;
                if x % 2 == 0 && y % 2 == 0 {
                    // Even/even takes the NEW average (B2/B3's home).
                    let s = (y / 2) * 4 + x / 2;
                    assert_eq!(buf.u[i], main.u[s], "even/even U at ({x},{y})");
                    assert_eq!(buf.v[i], main.v[s], "even/even V at ({x},{y})");
                } else {
                    // Every other position keeps the aux frame's sample.
                    assert_eq!(buf.u[i], u_before[i], "delivered U clobbered at ({x},{y})");
                    assert_eq!(buf.v[i], v_before[i], "delivered V clobbered at ({x},{y})");
                }
            }
        }
    }

    #[test]
    fn v1_split_rectangles_collectively_preserve_a_complete_chroma_block() {
        let aux = tagged_420(8, 16, 200);
        for (regions, axis) in [
            ([rect(0, 0, 1, 2), rect(1, 0, 2, 2)], "vertical"),
            ([rect(0, 0, 2, 1), rect(0, 1, 2, 2)], "horizontal"),
        ] {
            let mut buf = Yuv444Buffer::new(8, 16);
            buf.apply_chroma_v1(&aux, &regions);
            let before = [buf.u[1], buf.u[8], buf.u[9], buf.v[1], buf.v[8], buf.v[9]];

            buf.apply_luma(&uniform_420(8, 16, 100, 100), &[rect(0, 0, 2, 2)]);

            assert_eq!(
                [buf.u[1], buf.u[8], buf.u[9], buf.v[1], buf.v[8], buf.v[9]],
                before,
                "{axis} regions must retain their combined 4:4:4 detail"
            );
        }
    }

    #[test]
    fn v2_split_rectangles_collectively_preserve_a_complete_chroma_block() {
        let aux = tagged_420(8, 8, 200);
        for (regions, axis) in [
            ([rect(0, 0, 1, 2), rect(1, 0, 2, 2)], "vertical"),
            ([rect(0, 0, 2, 1), rect(0, 1, 2, 2)], "horizontal"),
        ] {
            let mut buf = Yuv444Buffer::new(8, 8);
            buf.apply_chroma_v2(&aux, &regions);
            let before = [buf.u[1], buf.u[8], buf.u[9], buf.v[1], buf.v[8], buf.v[9]];

            buf.apply_luma(&uniform_420(8, 8, 100, 100), &[rect(0, 0, 2, 2)]);

            assert_eq!(
                [buf.u[1], buf.u[8], buf.u[9], buf.v[1], buf.v[8], buf.v[9]],
                before,
                "{axis} regions must retain their combined 4:4:4 detail"
            );
        }
    }

    /// An LC=2 packet may arrive before this surface's first LC=1 packet. The
    /// neutral buffer fill is not a real chroma average and must not make that
    /// first luma update look like a content change.
    #[test]
    fn an_lc2_first_sequence_keeps_detail_after_the_first_luma_average() {
        let aux = tagged_420(8, 8, 200);
        let mut buf = Yuv444Buffer::new(8, 8);
        buf.apply_chroma_v2(&aux, &[rect(0, 0, 8, 8)]);

        let main = uniform_420(8, 8, 100, 100);
        buf.apply_luma(&main, &[rect(0, 0, 8, 8)]);

        let i = 3 * 8 + 3;
        assert!(
            !buf.chroma_stale_at(3, 3),
            "the first real luma average must establish the baseline"
        );
        let mut out = Vec::new();
        buf.to_rgba_into(&rect(0, 0, 8, 8), &mut out);
        let expect = yuv_to_rgba(main.y[i], aux.y[3 * 8 + 1], aux.y[3 * 8 + 5]);
        assert_eq!(
            out[i * 4..][..4],
            expect,
            "LC=2 detail must survive the first LC=1 packet"
        );
    }

    #[test]
    fn an_lc2_first_baseline_still_detects_a_later_luma_change() {
        let aux = tagged_420(8, 8, 200);
        let mut buf = Yuv444Buffer::new(8, 8);
        buf.apply_chroma_v2(&aux, &[rect(0, 0, 8, 8)]);
        buf.apply_luma(&uniform_420(8, 8, 100, 100), &[rect(0, 0, 8, 8)]);

        let changed = uniform_420(8, 8, 120, 120);
        buf.apply_luma(&changed, &[rect(0, 0, 8, 8)]);

        assert!(
            buf.chroma_stale_at(3, 3),
            "later luma must still be compared with the established baseline"
        );
        let i = 3 * 8 + 3;
        let mut out = Vec::new();
        buf.to_rgba_into(&rect(0, 0, 8, 8), &mut out);
        assert_eq!(
            out[i * 4..][..4],
            yuv_to_rgba(changed.y[i], 120, 120),
            "changed content must paint flat until aux catches up"
        );
    }

    #[test]
    fn an_avc444v1_lc2_first_sequence_also_keeps_detail() {
        let aux = tagged_420(8, 32, 200);
        let mut buf = Yuv444Buffer::new(8, 32);
        buf.apply_chroma_v1(&aux, &[rect(0, 0, 8, 32)]);

        let main = uniform_420(8, 32, 100, 100);
        buf.apply_luma(&main, &[rect(0, 0, 8, 32)]);

        let i = 3 * 8 + 3;
        assert!(
            !buf.chroma_stale_at(3, 3),
            "the shared AVC444v1 marker must not confirm neutral initialization"
        );
        let mut out = Vec::new();
        buf.to_rgba_into(&rect(0, 0, 8, 32), &mut out);
        let expect = yuv_to_rgba(main.y[i], aux.y[1 * 8 + 3], aux.y[9 * 8 + 3]);
        assert_eq!(out[i * 4..][..4], expect);
    }

    #[test]
    fn a_chroma_only_block_in_lc0_accepts_its_first_later_luma_average() {
        let aux = tagged_420(8, 8, 200);
        let main = uniform_420(8, 8, 100, 100);
        let mut buf = Yuv444Buffer::new(8, 8);

        // Model one LC0 update whose luma and chroma rects are disjoint.
        buf.apply_luma(&main, &[rect(0, 0, 4, 8)]);
        buf.apply_chroma_v2(&aux, &[rect(4, 0, 8, 8)]);
        buf.apply_luma(&main, &[rect(4, 0, 8, 8)]);

        assert!(
            !buf.chroma_stale_at(5, 3),
            "the first luma average for this block must establish its baseline"
        );
        let i = 3 * 8 + 5;
        let mut out = Vec::new();
        buf.to_rgba_into(&rect(4, 0, 8, 8), &mut out);
        let expect = yuv_to_rgba(main.y[i], aux.y[3 * 8 + 2], aux.y[3 * 8 + 6]);
        let out_i = (3 * 4 + 1) * 4;
        assert_eq!(out[out_i..][..4], expect);
    }

    /// Blocks the chroma pass never covered keep first-paint behaviour: the luma
    /// pass replicates its averages there, so a surface painted ahead of its first
    /// chroma pass degrades to 4:2:0, not to neutral grey.
    #[test]
    fn a_luma_pass_still_replicates_where_no_chroma_was_delivered() {
        let aux = tagged_420(8, 8, 0);
        let mut buf = Yuv444Buffer::new(8, 8);
        // Chroma covers only the left half (blocks 0..2 of each row).
        buf.apply_chroma_v2(&aux, &[rect(0, 0, 4, 8)]);
        let u_before = buf.u.clone();

        let main = tagged_420(8, 8, 7);
        buf.apply_luma(&main, &[rect(0, 0, 8, 8)]);

        // Left half: delivered samples preserved.
        assert_eq!(buf.u[1 * 8 + 3], u_before[1 * 8 + 3]);
        // Right half: never chroma'd, so the average is replicated as before.
        let s = (1 / 2) * 4 + 5 / 2;
        assert_eq!(buf.u[1 * 8 + 5], main.u[s]);
        assert_eq!(buf.v[1 * 8 + 5], main.v[s]);
    }

    /// The regression for MDR-BUG-FLUX-00010: content that changes under a
    /// luma-only frame must paint with the new flat average — not by
    /// reconstruction against odd samples that still describe the previous
    /// content (that overshoot flashed a retreating dark-yellow bar blue, and
    /// cast wrong colours across whole windows on large redraws).
    #[test]
    fn a_changed_average_under_a_luma_only_frame_paints_flat_until_chroma_catches_up() {
        let aux = tagged_420(8, 8, 0);
        let mut buf = Yuv444Buffer::new(8, 8);
        let main1 = tagged_420(8, 8, 7);
        buf.apply_luma(&main1, &[rect(0, 0, 8, 8)]);
        buf.apply_chroma_v2(&aux, &[rect(0, 0, 8, 8)]);

        // The content changes: averages move by 100 everywhere (tag 7 -> 107,
        // no u8 wrap in an 8x8 tagged frame), far above STALE_AVG_DELTA.
        let main2 = tagged_420(8, 8, 107);
        buf.apply_luma(&main2, &[rect(0, 0, 8, 8)]);

        let mut out = Vec::new();
        buf.to_rgba_into(&rect(0, 0, 8, 8), &mut out);
        for y in 0..8 {
            for x in 0..8 {
                // Every pixel of a stale block paints the NEW average — the odd
                // samples (still main1-era aux data) and the reconstruction are
                // both bypassed.
                let s = (y / 2) * 4 + x / 2;
                let expect = yuv_to_rgba(buf.y[y * 8 + x], main2.u[s], main2.v[s]);
                assert_eq!(out[(y * 8 + x) * 4..][..4], expect, "stale paint at ({x},{y})");
            }
        }

        // The chroma catch-up delivers matching detail: full 4:4:4 returns.
        // (Tag 200 keeps the aux samples ~100 away from main2's averages, so a
        // still-stale flat paint could not fake this assertion.)
        let aux2 = tagged_420(8, 8, 200);
        buf.apply_chroma_v2(&aux2, &[rect(0, 0, 8, 8)]);
        buf.to_rgba_into(&rect(0, 0, 8, 8), &mut out);
        // Odd/odd positions show the delivered aux samples again.
        let i = 3 * 8 + 3;
        assert_eq!(buf.u[i], aux2.y[3 * 8 + 1], "odd/odd U is aux2 data");
        let expect = yuv_to_rgba(buf.y[i], buf.u[i], buf.v[i]);
        assert_eq!(out[i * 4..][..4], expect, "detail must return after catch-up");
    }

    /// A transient luma-only change must not permanently hide aux detail when
    /// the luma average returns to the last aux-confirmed state without aux.
    #[test]
    fn a_luma_return_to_last_aux_average_clears_stale_without_aux() {
        let aux = tagged_420(8, 8, 200);
        let main_a = tagged_420(8, 8, 7);
        let mut buf = Yuv444Buffer::new(8, 8);
        buf.apply_luma(&main_a, &[rect(0, 0, 8, 8)]);
        buf.apply_chroma_v2(&aux, &[rect(0, 0, 8, 8)]);

        let main_b = tagged_420(8, 8, 107);
        buf.apply_luma(&main_b, &[rect(0, 0, 8, 8)]);
        assert!(
            buf.chroma_stale_at(3, 3),
            "changed average must mark detail stale"
        );

        // The encoder omits aux because A is again its last-sent chroma state.
        buf.apply_luma(&main_a, &[rect(0, 0, 8, 8)]);

        let i = 3 * 8 + 3;
        assert!(
            !buf.chroma_stale_at(3, 3),
            "return to A must clear stale detail"
        );
        let mut out = Vec::new();
        buf.to_rgba_into(&rect(0, 0, 8, 8), &mut out);
        let expect = yuv_to_rgba(main_a.y[i], aux.y[3 * 8 + 1], aux.y[3 * 8 + 5]);
        assert_eq!(
            out[i * 4..][..4],
            expect,
            "aux detail must return without another aux"
        );
    }

    #[test]
    fn gradual_luma_drift_marks_chroma_stale_until_return_within_threshold() {
        let aux = tagged_420(8, 8, 200);
        let main_a = uniform_420(8, 8, 100, 100);
        let mut buf = Yuv444Buffer::new(8, 8);
        buf.apply_luma(&main_a, &[rect(0, 0, 8, 8)]);
        buf.apply_chroma_v2(&aux, &[rect(0, 0, 8, 8)]);

        // Each step is within the noise threshold, but the current average has
        // drifted far enough from the last aux-confirmed state to make detail stale.
        for average in [105, 110, 115] {
            let main = uniform_420(8, 8, average, average);
            buf.apply_luma(&main, &[rect(0, 0, 8, 8)]);
        }

        assert!(
            buf.chroma_stale_at(3, 3),
            "gradual drift beyond the confirmed average must mark detail stale"
        );
        let mut out = Vec::new();
        buf.to_rgba_into(&rect(0, 0, 8, 8), &mut out);
        let i = 3 * 8 + 3;
        assert_eq!(
            out[i * 4..][..4],
            yuv_to_rgba(128, 115, 115),
            "stale detail must paint the current flat average"
        );

        // Returning within the same threshold as the confirmed average restores
        // the previously delivered detail without requiring another aux frame.
        let main_return = uniform_420(8, 8, 109, 109);
        buf.apply_luma(&main_return, &[rect(0, 0, 8, 8)]);
        assert!(
            !buf.chroma_stale_at(3, 3),
            "return within the confirmed threshold must clear stale detail"
        );
        let expect = yuv_to_rgba(128, aux.y[3 * 8 + 1], aux.y[3 * 8 + 5]);
        buf.to_rgba_into(&rect(0, 0, 8, 8), &mut out);
        assert_eq!(
            out[i * 4..][..4],
            expect,
            "aux detail must return without another aux frame"
        );
    }

    /// Steady-state luma refreshes of unchanged content (averages move only by
    /// codec noise, measured <= 6 against temper) must keep painting full
    /// detail — the stale fallback fires only on a genuine content change.
    #[test]
    fn an_unchanged_average_under_a_luma_only_frame_keeps_painting_detail() {
        // Tag 200 puts the aux samples ~200 away from the averages, so a stale
        // flat paint would visibly differ from the detail paint asserted below.
        let aux = tagged_420(8, 8, 200);
        let mut buf = Yuv444Buffer::new(8, 8);
        let main = tagged_420(8, 8, 7);
        buf.apply_luma(&main, &[rect(0, 0, 8, 8)]);
        buf.apply_chroma_v2(&aux, &[rect(0, 0, 8, 8)]);

        // Re-encode of the same content: averages jitter within the noise band
        // (measured <= 6 against temper; STALE_AVG_DELTA is 10).
        let mut noisy = main.clone();
        for s in noisy.u.iter_mut().chain(noisy.v.iter_mut()) {
            *s = s.wrapping_add(3);
        }
        buf.apply_luma(&noisy, &[rect(0, 0, 8, 8)]);

        let mut after = Vec::new();
        buf.to_rgba_into(&rect(0, 0, 8, 8), &mut after);
        // Odd/odd pixel: still painted from its delivered aux sample, never
        // collapsed to the flat average.
        let i = 3 * 8 + 3;
        assert_eq!(buf.u[i], aux.y[3 * 8 + 1], "odd/odd U still aux data");
        let expect = yuv_to_rgba(buf.y[i], buf.u[i], buf.v[i]);
        assert_eq!(after[i * 4..][..4], expect, "detail must survive a noisy refresh");
    }

    #[test]
    fn the_luma_pass_stays_inside_its_rects() {
        let main = tagged_420(4, 4, 1);
        let mut buf = Yuv444Buffer::new(4, 4);
        buf.apply_luma(&main, &[rect(2, 2, 4, 4)]);
        // Outside the rect: untouched initial state.
        assert_eq!(buf.y[0], 0);
        assert_eq!(buf.u[0], 128);
        // Inside: written.
        assert_eq!(buf.y[2 * 4 + 2], main.y[2 * 4 + 2]);
    }

    /// v1 aux-Y block interleave, hand-computed on a 8x32 frame (two 16-row blocks).
    ///
    /// dst U row 2k+1 <- aux Y row (k/8)*16 + k%8; dst V row 2k+1 <- +8.
    ///   k=0  -> dst row 1:  U from aux Y row 0,  V from aux Y row 8
    ///   k=7  -> dst row 15: U from aux Y row 7,  V from aux Y row 15
    ///   k=8  -> dst row 17: U from aux Y row 16, V from aux Y row 24  (second block)
    ///   k=15 -> dst row 31: U from aux Y row 23, V from aux Y row 31
    #[test]
    fn v1_odd_rows_come_from_the_aux_y_plane_in_16_row_blocks() {
        let aux = tagged_420(8, 32, 0);
        let mut buf = Yuv444Buffer::new(8, 32);
        buf.apply_chroma_v1(&aux, &[rect(0, 0, 8, 32)]);

        for (dst_row, aux_u_row, aux_v_row) in [(1, 0, 8), (15, 7, 15), (17, 16, 24), (31, 23, 31)]
        {
            for x in 0..8 {
                assert_eq!(
                    buf.u[dst_row * 8 + x],
                    aux.y[aux_u_row * 8 + x],
                    "U row {dst_row} col {x} must come from aux Y row {aux_u_row}"
                );
                assert_eq!(
                    buf.v[dst_row * 8 + x],
                    aux.y[aux_v_row * 8 + x],
                    "V row {dst_row} col {x} must come from aux Y row {aux_v_row}"
                );
            }
        }
    }

    #[test]
    fn v1_even_rows_odd_columns_come_from_the_aux_uv_planes() {
        let aux = tagged_420(8, 32, 0);
        let mut buf = Yuv444Buffer::new(8, 32);
        buf.apply_chroma_v1(&aux, &[rect(0, 0, 8, 32)]);

        // dst U[2y][2x+1] <- aux U[y][x]: dst (3, 4) -> aux U[2][1] = index 2*4+1=9 -> 27.
        assert_eq!(buf.u[4 * 8 + 3], aux.u[2 * 4 + 1]);
        assert_eq!(buf.u[4 * 8 + 3], 27);
        assert_eq!(buf.v[4 * 8 + 3], aux.v[2 * 4 + 1]);
        // Even/even positions are the luma pass's territory: untouched here.
        assert_eq!(buf.u[4 * 8 + 2], 128);
    }

    /// v2 layout on an 8-wide frame: align32(8)=32 caps to aux width 8, so W=8 —
    /// aux Y row = [U half: cols 0..4 | V half: cols 4..8], aux U/V rows split at 2.
    #[test]
    fn v2_odd_columns_come_from_the_split_aux_y_plane() {
        let aux = tagged_420(8, 8, 0);
        let mut buf = Yuv444Buffer::new(8, 8);
        buf.apply_chroma_v2(&aux, &[rect(0, 0, 8, 8)]);

        // dst U[y][2x+1] <- aux Y[y][x]; dst V[y][2x+1] <- aux Y[y][4 + x].
        // Row 3, dst col 5 (x=2): U <- aux Y[3][2] = 26; V <- aux Y[3][6] = 30.
        assert_eq!(buf.u[3 * 8 + 5], aux.y[3 * 8 + 2]);
        assert_eq!(buf.u[3 * 8 + 5], 26);
        assert_eq!(buf.v[3 * 8 + 5], aux.y[3 * 8 + 6]);
        assert_eq!(buf.v[3 * 8 + 5], 30);
    }

    #[test]
    fn v2_odd_rows_even_columns_come_from_the_split_aux_uv_planes() {
        let aux = tagged_420(8, 8, 0);
        let mut buf = Yuv444Buffer::new(8, 8);
        buf.apply_chroma_v2(&aux, &[rect(0, 0, 8, 8)]);

        // W=8 so the U/V planes (4 wide) split at W/4=2.
        // dst row 5 (sy=2), col 0: U <- aux U[2][0]=idx 8 -> 24; V <- aux U[2][2]=idx 10 -> 30.
        assert_eq!(buf.u[5 * 8 + 0], aux.u[2 * 4 + 0]);
        assert_eq!(buf.v[5 * 8 + 0], aux.u[2 * 4 + 2]);
        // dst row 5, col 2: U <- aux V[2][0]; V <- aux V[2][2].
        assert_eq!(buf.u[5 * 8 + 2], aux.v[2 * 4 + 0]);
        assert_eq!(buf.v[5 * 8 + 2], aux.v[2 * 4 + 2]);
    }

    #[test]
    fn v1_odd_sized_edge_chroma_survives_a_luma_update() {
        let aux = tagged_420(3, 9, 200);
        let mut buf = Yuv444Buffer::new(3, 3);
        buf.apply_chroma_v1(&aux, &[rect(0, 0, 3, 3)]);
        let before = [
            (buf.u[3 + 2], buf.v[3 + 2]),
            (buf.u[2 * 3 + 1], buf.v[2 * 3 + 1]),
        ];

        buf.apply_luma(&uniform_420(3, 3, 100, 100), &[rect(0, 0, 3, 3)]);

        assert_eq!(
            [
                (buf.u[3 + 2], buf.v[3 + 2]),
                (buf.u[2 * 3 + 1], buf.v[2 * 3 + 1])
            ],
            before,
            "v1 right and bottom edge detail must survive LC=1"
        );
        assert!(buf.chroma_seen_at(2, 1));
        assert!(buf.chroma_seen_at(1, 2));
    }

    #[test]
    fn v2_odd_sized_edge_chroma_survives_a_luma_update() {
        let aux = tagged_420(3, 3, 200);
        let mut buf = Yuv444Buffer::new(3, 3);
        buf.apply_chroma_v2(&aux, &[rect(0, 0, 3, 3)]);
        let before = [
            (buf.u[3 + 2], buf.v[3 + 2]),
            (buf.u[2 * 3 + 1], buf.v[2 * 3 + 1]),
        ];

        buf.apply_luma(&uniform_420(3, 3, 100, 100), &[rect(0, 0, 3, 3)]);

        assert_eq!(
            [
                (buf.u[3 + 2], buf.v[3 + 2]),
                (buf.u[2 * 3 + 1], buf.v[2 * 3 + 1])
            ],
            before,
            "v2 right and bottom edge detail must survive LC=1"
        );
        assert!(buf.chroma_seen_at(2, 1));
        assert!(buf.chroma_seen_at(1, 2));
    }

    #[test]
    fn a_surface_without_odd_positions_does_not_promote_chroma() {
        let mut v1 = Yuv444Buffer::new(1, 1);
        v1.apply_chroma_v1(&tagged_420(1, 9, 200), &[rect(0, 0, 1, 1)]);
        assert!(!v1.chroma_seen_at(0, 0));

        let mut v2 = Yuv444Buffer::new(1, 1);
        v2.apply_chroma_v2(&tagged_420(1, 1, 200), &[rect(0, 0, 1, 1)]);
        assert!(!v2.chroma_seen_at(0, 0));
    }

    /// The split offset uses align32(surface width), not the surface width: a
    /// 20-wide surface with a 32-wide (padded) aux frame splits at 16, not 10.
    #[test]
    fn v2_split_offset_uses_the_32_aligned_width() {
        let aux = tagged_420(32, 8, 0);
        let mut buf = Yuv444Buffer::new(20, 8);
        buf.apply_chroma_v2(&aux, &[rect(0, 0, 20, 8)]);

        // W = min(align32(20)=32, 32) = 32. dst V[1][2x+1] <- aux Y[1][16 + x].
        assert_eq!(buf.v[1 * 20 + 1], aux.y[1 * 32 + 16]);
        // A width/2=10 split would have read aux.y[1*32 + 10] instead; prove they differ.
        assert_ne!(aux.y[1 * 32 + 16], aux.y[1 * 32 + 10]);
    }

    #[test]
    fn chroma_passes_survive_rects_beyond_the_frame() {
        let aux = tagged_420(8, 8, 0);
        let mut buf = Yuv444Buffer::new(8, 8);
        // Rect exceeding the buffer: clipped, no panic.
        buf.apply_chroma_v1(&aux, &[rect(0, 0, 100, 100)]);
        buf.apply_chroma_v2(&aux, &[rect(4, 4, 200, 200)]);
        // Aux smaller than the buffer: reads bounds-checked, no panic.
        let small = tagged_420(4, 4, 0);
        let mut big = Yuv444Buffer::new(16, 16);
        big.apply_chroma_v1(&small, &[rect(0, 0, 16, 16)]);
        big.apply_chroma_v2(&small, &[rect(0, 0, 16, 16)]);
    }

    #[test]
    fn the_rgb_matrix_matches_the_reference_coefficients() {
        // Y=255, neutral chroma -> white; Y=0 -> black.
        assert_eq!(yuv_to_rgba(255, 128, 128), [255, 255, 255, 0xFF]);
        assert_eq!(yuv_to_rgba(0, 128, 128), [0, 0, 0, 0xFF]);
        // Hand-computed: Y=128, U=64, V=192.
        //   R = (256*128 + 403*64) >> 8  = (32768 + 25792) >> 8 = 228
        //   G = (256*128 - 48*(-64) - 120*64) >> 8 = (32768 + 3072 - 7680) >> 8 = 110
        //   B = (256*128 + 475*(-64)) >> 8 = (32768 - 30400) >> 8 = 9
        assert_eq!(yuv_to_rgba(128, 64, 192), [228, 110, 9, 0xFF]);
    }

    #[test]
    fn even_even_chroma_is_reconstructed_from_the_block_average_when_it_moves_far() {
        // avg=100 with neighbors 50, 60, 70 -> recon = 400-180 = 220; |220-100| >= 30.
        assert_eq!(reconstruct_chroma(100, 50, 60, 70), 220);
        // avg=100 with neighbors 95, 100, 105 -> recon = 100; unchanged (diff 0 < 30).
        assert_eq!(reconstruct_chroma(100, 95, 100, 105), 100);
        // avg=100, neighbors 90, 95, 96 -> recon = 400-281 = 119; diff 19 < 30 -> keep avg.
        assert_eq!(reconstruct_chroma(100, 90, 95, 96), 100);
        // Clamp: avg=200, neighbors 10 each -> 800-30=770 -> clipped 255, diff >= 30.
        assert_eq!(reconstruct_chroma(200, 10, 10, 10), 255);
    }

    #[test]
    fn to_rgba_applies_reconstruction_only_on_complete_blocks() {
        let mut buf = Yuv444Buffer::new(3, 3);
        // Y flat 128 everywhere; U: block average 100 at (0,0), neighbors 50/60/70.
        buf.y.fill(128);
        buf.u.fill(128);
        buf.v.fill(128);
        buf.u[0] = 100;
        buf.u[1] = 50;
        buf.u[3] = 60;
        buf.u[4] = 70;

        let mut out = Vec::new();
        buf.to_rgba_into(&rect(0, 0, 3, 3), &mut out);
        assert_eq!(out.len(), 9 * 4);

        // (0,0): reconstructed U = 220 -> B = (256*128 + 475*92) >> 8 = 298 -> 255.
        assert_eq!(out[2], 255);
        // (2,2): last row/column -> no complete block -> raw U=128 -> B = 128.
        let last = 8 * 4;
        assert_eq!(out[last + 2], 128);
    }

    #[test]
    fn yuv420_to_rgba_replicates_chroma_without_reconstruction() {
        let frame = Yuv420Frame {
            y: vec![128; 4],
            u: vec![64],
            v: vec![192],
            width: 2,
            height: 2,
        };
        let out = yuv420_to_rgba(&frame);
        assert_eq!(out.len(), 16);
        for px in out.chunks_exact(4) {
            assert_eq!(px, [228, 110, 9, 0xFF]);
        }
    }
}
