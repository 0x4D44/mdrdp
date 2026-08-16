//! Differential-test harness: our AVC444 combination and RGB conversion against
//! FreeRDP 3.27.1's `YUV420CombineToYUV444` and `YUV444ToRGB_8u_P3AC4R`.
//!
//! This file holds only the plumbing — the PRNG, the plane allocators, the safe
//! wrappers over `shim.c`, and the plane comparator. The campaigns live in
//! `tests/differential.rs`.
//!
//! # Why the comparison is constrained
//!
//! FreeRDP walks each region ROI-*relative*: `general_LumaToYUV444` offsets its
//! chroma source by `roi->left / 2`, `general_ChromaV2ToYUV444` by `roi->left / 4`
//! and `roi->top / 2`, and `general_ChromaV1ToYUV444` derives its 16-row aux-Y
//! block phase from a counter that restarts at the ROI's top edge. Those roundings
//! only reproduce absolute frame coordinates when the rect origin is aligned, and
//! its `halfWidth`/`halfHeight` round *up*, so an unaligned rect also writes one
//! column or row past its own right/bottom edge.
//!
//! Our implementation deliberately maps every destination position through
//! absolute coordinates, so it is a superset: it agrees with FreeRDP wherever
//! FreeRDP is phase-correct, and stays correct where FreeRDP is not. This oracle
//! therefore tests only the overlap — even rects for luma, full-frame for v1,
//! 4-aligned rects for v2. The behaviour outside that overlap is pinned by
//! `avc444.rs`'s own unit tests, not here.

use ironrdp_graphics::avc444::Yuv420Frame;
use ironrdp_pdu::geometry::ExclusiveRectangle;

pub const AVC444_LUMA: i32 = 0;
pub const AVC444_CHROMA_V1: i32 = 1;
pub const AVC444_CHROMA_V2: i32 = 2;

/// Bytes of guard band after every plane. Filled with a sentinel and asserted
/// unchanged, so an out-of-bounds write by either side is a loud failure rather
/// than a heap corruption.
const SLACK: usize = 4096;
const SENTINEL: u8 = 0xA5;

extern "C" {
    fn avc444diff_flags(generic: i32) -> u32;

    fn avc444diff_shared_impls() -> u32;

    #[allow(clippy::too_many_arguments)]
    fn avc444diff_combine(
        generic: i32,
        ty: i32,
        y: *const u8,
        u: *const u8,
        v: *const u8,
        src_step: *const u32,
        n_width: u32,
        n_height: u32,
        dy: *mut u8,
        du: *mut u8,
        dv: *mut u8,
        dst_step: *const u32,
        l: u16,
        t: u16,
        r: u16,
        b: u16,
    ) -> i32;

    fn avc444diff_to_rgb(
        generic: i32,
        y: *const u8,
        u: *const u8,
        v: *const u8,
        src_step: *const u32,
        dst: *mut u8,
        dst_step: u32,
        width: u32,
        height: u32,
    ) -> i32;
}

/// Which FreeRDP primitive table to drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prims {
    /// `primitives_get()` — what a real FreeRDP session uses (SIMD where available).
    Auto,
    /// `primitives_get_generic()` — the portable C reference in `prim_YUV.c`.
    Generic,
}

impl Prims {
    fn flag(self) -> i32 {
        match self {
            Prims::Auto => 0,
            Prims::Generic => 1,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Prims::Auto => "primitives_get()",
            Prims::Generic => "primitives_get_generic()",
        }
    }

    pub fn flags(self) -> u32 {
        unsafe { avc444diff_flags(self.flag()) }
    }
}

/// Does the optimized table share `YUV420CombineToYUV444` (bit 0) and
/// `YUV444ToRGB_8u_P3AC4R` (bit 1) with the generic one?
pub fn shared_impls() -> u32 {
    unsafe { avc444diff_shared_impls() }
}

/// Deterministic xorshift64. Hand-rolled so the oracle carries no PRNG dependency
/// and a seed reproduces a case exactly on any machine.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // A zero state is a fixed point of xorshift; steer away from it.
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub fn byte(&mut self) -> u8 {
        (self.next_u64() >> 24) as u8
    }

    /// Uniform-enough in `0..n`; the modulo bias is irrelevant for picking rects.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % (n as u64)) as usize
    }
}

/// A random tightly-packed 4:2:0 frame.
pub fn random_420(rng: &mut Rng, width: usize, height: usize) -> Yuv420Frame {
    let uv_row = width.div_ceil(2);
    let uv_height = height.div_ceil(2);
    Yuv420Frame {
        y: (0..width * height).map(|_| rng.byte()).collect(),
        u: (0..uv_row * uv_height).map(|_| rng.byte()).collect(),
        v: (0..uv_row * uv_height).map(|_| rng.byte()).collect(),
        width,
        height,
    }
}

/// Three planes laid out for the C side: tight rows plus a guard band.
pub struct CPlanes {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    step: [u32; 3],
    lens: [usize; 3],
}

fn padded(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + SLACK);
    out.extend_from_slice(data);
    out.resize(data.len() + SLACK, SENTINEL);
    out
}

impl CPlanes {
    /// The same bytes and the same row widths our `Yuv420Frame` carries.
    pub fn from_420(frame: &Yuv420Frame) -> Self {
        let uv_row = frame.uv_row();
        Self {
            y: padded(&frame.y),
            u: padded(&frame.u),
            v: padded(&frame.v),
            step: [frame.width as u32, uv_row as u32, uv_row as u32],
            lens: [frame.y.len(), frame.u.len(), frame.v.len()],
        }
    }

    /// Three full-resolution planes, all `width` per row.
    pub fn from_444(y: &[u8], u: &[u8], v: &[u8], width: usize) -> Self {
        Self {
            y: padded(y),
            u: padded(u),
            v: padded(v),
            step: [width as u32; 3],
            lens: [y.len(), u.len(), v.len()],
        }
    }

    fn guards_intact(&self) -> bool {
        [(&self.y, self.lens[0]), (&self.u, self.lens[1]), (&self.v, self.lens[2])]
            .iter()
            .all(|(plane, len)| plane[*len..].iter().all(|&b| b == SENTINEL))
    }
}

/// FreeRDP's destination 444 buffer, initialized exactly like `Yuv444Buffer::new`.
pub struct CDest {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    step: [u32; 3],
    width: usize,
    height: usize,
}

impl CDest {
    pub fn new(width: usize, height: usize) -> Self {
        let n = width * height;
        let fill = |value: u8| {
            let mut plane = vec![value; n];
            plane.resize(n + SLACK, SENTINEL);
            plane
        };
        Self {
            y: fill(0),
            u: fill(128),
            v: fill(128),
            step: [width as u32; 3],
            width,
            height,
        }
    }

    pub fn planes(&self) -> (&[u8], &[u8], &[u8]) {
        let n = self.width * self.height;
        (&self.y[..n], &self.u[..n], &self.v[..n])
    }

    fn guards_intact(&self) -> bool {
        let n = self.width * self.height;
        [&self.y, &self.u, &self.v]
            .iter()
            .all(|plane| plane[n..].iter().all(|&b| b == SENTINEL))
    }
}

/// One `YUV420CombineToYUV444` pass. `n_width` is FreeRDP's `nTotalWidth` and is
/// only read by the v2 pass; pass exactly what our implementation computes.
#[allow(clippy::too_many_arguments)]
pub fn fr_combine(
    prims: Prims,
    ty: i32,
    src: &CPlanes,
    n_width: u32,
    n_height: u32,
    dst: &mut CDest,
    rect: &ExclusiveRectangle,
) {
    let status = unsafe {
        avc444diff_combine(
            prims.flag(),
            ty,
            src.y.as_ptr(),
            src.u.as_ptr(),
            src.v.as_ptr(),
            src.step.as_ptr(),
            n_width,
            n_height,
            dst.y.as_mut_ptr(),
            dst.u.as_mut_ptr(),
            dst.v.as_mut_ptr(),
            dst.step.as_ptr(),
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
        )
    };
    assert!(status >= 0, "FreeRDP YUV420CombineToYUV444 failed: {status}");
    assert!(src.guards_intact(), "FreeRDP wrote past a source plane");
    assert!(dst.guards_intact(), "FreeRDP wrote past a destination plane");
}

/// One `YUV444ToRGB_8u_P3AC4R` pass over the whole frame.
///
/// The destination is pre-filled with 0xFF: for `PIXEL_FORMAT_RGBA32` FreeRDP
/// leaves the alpha byte alone (see `shim.c`), and ours always writes 0xFF.
pub fn fr_to_rgba(prims: Prims, src: &CPlanes, width: usize, height: usize) -> Vec<u8> {
    let stride = width * 4;
    let mut out = vec![0xFFu8; stride * height + SLACK];
    out[stride * height..].fill(SENTINEL);
    let status = unsafe {
        avc444diff_to_rgb(
            prims.flag(),
            src.y.as_ptr(),
            src.u.as_ptr(),
            src.v.as_ptr(),
            src.step.as_ptr(),
            out.as_mut_ptr(),
            stride as u32,
            width as u32,
            height as u32,
        )
    };
    assert!(status >= 0, "FreeRDP YUV444ToRGB_8u_P3AC4R failed: {status}");
    assert!(src.guards_intact(), "FreeRDP wrote past a source plane");
    assert!(
        out[stride * height..].iter().all(|&b| b == SENTINEL),
        "FreeRDP wrote past the RGBA destination"
    );
    out.truncate(stride * height);
    out
}

/// The first disagreement between two implementations, in frame coordinates.
#[derive(Debug)]
pub struct Mismatch {
    pub plane: &'static str,
    pub x: usize,
    pub y: usize,
    pub ours: u8,
    pub theirs: u8,
    pub total: usize,
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "plane {} at (x={}, y={}): ours={} freerdp={} ({} byte(s) differ in total)",
            self.plane, self.x, self.y, self.ours, self.theirs, self.total
        )
    }
}

/// Byte-compare all three 444 planes.
pub fn compare_planes(
    ours: (&[u8], &[u8], &[u8]),
    theirs: (&[u8], &[u8], &[u8]),
    width: usize,
) -> Result<(), Mismatch> {
    let pairs = [("Y", ours.0, theirs.0), ("U", ours.1, theirs.1), ("V", ours.2, theirs.2)];
    let mut first: Option<Mismatch> = None;
    let mut total = 0usize;
    for (name, a, b) in pairs {
        assert_eq!(a.len(), b.len(), "plane {name} length differs");
        for (i, (&x, &y)) in a.iter().zip(b.iter()).enumerate() {
            if x != y {
                total += 1;
                if first.is_none() {
                    first = Some(Mismatch {
                        plane: name,
                        x: i % width,
                        y: i / width,
                        ours: x,
                        theirs: y,
                        total: 0,
                    });
                }
            }
        }
    }
    match first {
        None => Ok(()),
        Some(mut m) => {
            m.total = total;
            Err(m)
        }
    }
}

/// Byte-compare two RGBA images, reporting the first differing channel.
pub fn compare_rgba(ours: &[u8], theirs: &[u8], width: usize) -> Result<(), Mismatch> {
    assert_eq!(ours.len(), theirs.len(), "RGBA buffer length differs");
    let mut first: Option<Mismatch> = None;
    let mut total = 0usize;
    for (i, (&a, &b)) in ours.iter().zip(theirs.iter()).enumerate() {
        if a != b {
            total += 1;
            if first.is_none() {
                let px = i / 4;
                first = Some(Mismatch {
                    plane: ["R", "G", "B", "A"][i % 4],
                    x: px % width,
                    y: px / width,
                    ours: a,
                    theirs: b,
                    total: 0,
                });
            }
        }
    }
    match first {
        None => Ok(()),
        Some(mut m) => {
            m.total = total;
            Err(m)
        }
    }
}

/// Round a width up to the next multiple of 32 — FreeRDP `yuv.c`'s `alignedWidth`,
/// and the same value `apply_chroma_v2` computes internally.
pub fn align32(width: usize) -> usize {
    width.div_ceil(32) * 32
}

/// The `nTotalWidth` the v2 pass must be given: the 32-aligned surface width,
/// capped at what the decoded auxiliary plane actually holds.
/// LIMIT OF THIS ORACLE: both sides receive this same value as `nTotalWidth`, so
/// the campaigns prove the *primitive's* split offsets are `nTotalWidth/2` and
/// `nTotalWidth/4` — they cannot prove the align32 derivation itself (an align16
/// rule would pass identically). The derivation is transcribed from FreeRDP
/// `yuv.c` (`alignedWidth`, verified in source 2026-08-16); widths where align16
/// and align32 differ are additionally guarded at the client (degrade to
/// luma-only), pending a real capture at such a width.
pub fn v2_total_width(dest_width: usize, aux_width: usize) -> u32 {
    align32(dest_width).min(aux_width) as u32
}

pub fn rect(left: u16, top: u16, right: u16, bottom: u16) -> ExclusiveRectangle {
    ExclusiveRectangle {
        left,
        top,
        right,
        bottom,
    }
}

/// A random rect inside `width x height` whose every coordinate is a multiple of
/// `align`, and which is never empty.
pub fn random_aligned_rect(
    rng: &mut Rng,
    width: usize,
    height: usize,
    align: usize,
) -> ExclusiveRectangle {
    let cols = width / align;
    let rows = height / align;
    let left = rng.below(cols);
    let right = left + 1 + rng.below(cols - left);
    let top = rng.below(rows);
    let bottom = top + 1 + rng.below(rows - top);
    rect(
        (left * align) as u16,
        (top * align) as u16,
        (right * align) as u16,
        (bottom * align) as u16,
    )
}
