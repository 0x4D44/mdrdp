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
//! the field-proven reference against Windows servers. One deliberate divergence:
//! FreeRDP walks each region ROI-relative, which is only phase-correct for aligned
//! rect origins; this module maps every destination position through **absolute frame
//! coordinates**, which agrees with FreeRDP on the aligned rects Windows actually
//! sends and stays correct for arbitrary ones. Every source read is bounds-checked:
//! a destination position whose source sample does not exist in the decoded frame is
//! left unwritten (it keeps its previous or luma-replicated value) rather than
//! shearing or panicking — wire data is untrusted.

use ironrdp_pdu::geometry::ExclusiveRectangle;

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
}

impl Yuv444Buffer {
    /// A black frame (Y=0, U=V=128) of the given dimensions.
    pub fn new(width: u16, height: u16) -> Self {
        let (w, h) = (usize::from(width), usize::from(height));
        Self {
            y: vec![0; w * h],
            u: vec![128; w * h],
            v: vec![128; w * h],
            width: w,
            height: h,
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
        Self {
            y,
            u,
            v,
            width,
            height,
        }
    }

    /// Clip a rect to this buffer, returning half-open pixel ranges.
    fn clip(&self, rect: &ExclusiveRectangle) -> (usize, usize, usize, usize) {
        let left = usize::from(rect.left).min(self.width);
        let top = usize::from(rect.top).min(self.height);
        let right = usize::from(rect.right).min(self.width);
        let bottom = usize::from(rect.bottom).min(self.height);
        (left, top, right, bottom)
    }

    /// The luma pass: copy Y and replicate the main frame's 2x2-subsampled chroma
    /// into every position of each rect.
    ///
    /// The replicated values are the encoder's block averages; a following chroma
    /// pass overwrites the odd positions with true samples, and the RGB conversion
    /// reconstructs the even/even sample from the average (see [`Self::to_rgba_into`]).
    /// A luma-only (LC=1) update therefore degrades its rects to 4:2:0 chroma until
    /// the next chroma pass covers them — reference-client behavior.
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
                    let s = sy * uv_row + dx / 2;
                    self.u[dy * self.width + dx] = main.u[s];
                    self.v[dy * self.width + dx] = main.v[s];
                }
            }
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
        for rect in rects {
            let (left, top, right, bottom) = self.clip(rect);
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
        for rect in rects {
            let (left, top, right, bottom) = self.clip(rect);
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
    pub fn to_rgba_into(&self, rect: &ExclusiveRectangle, out: &mut Vec<u8>) {
        let (left, top, right, bottom) = self.clip(rect);
        out.clear();
        out.reserve((right - left) * (bottom - top) * 4);
        for y in top..bottom {
            for x in left..right {
                let i = y * self.width + x;
                let (mut u, mut v) = (self.u[i], self.v[i]);
                if x % 2 == 0 && y % 2 == 0 && x + 1 < self.width && y + 1 < self.height {
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
