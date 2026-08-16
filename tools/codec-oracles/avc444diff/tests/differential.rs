//! The differential campaigns. Run with `cargo test -- --nocapture` to see the
//! per-campaign case counts.
//!
//! Every campaign runs each case twice: once against `primitives_get()` (what a
//! real FreeRDP session uses, SIMD included) and once against
//! `primitives_get_generic()` (the portable C in `prim_YUV.c`). If only one of the
//! two disagrees with us, the failure message says which — that attributes the
//! fault to a FreeRDP SIMD path rather than to the algorithm.

use avc444diff::*;
use ironrdp_graphics::avc444::Yuv444Buffer;
use ironrdp_pdu::geometry::ExclusiveRectangle;

const CASES: usize = 60;

const BOTH: [Prims; 2] = [Prims::Auto, Prims::Generic];

/// Even width and height; any of these is a legal luma-pass frame.
const LUMA_GEOMS: &[(usize, usize)] = &[
    (64, 64),
    (128, 48),
    (32, 32),
    (96, 64),
    (48, 32),
    (80, 16),
    (16, 16),
    (48, 48),
];

/// The v1 pass additionally needs a height that is a multiple of 16.
///
/// FreeRDP's `general_ChromaV1ToYUV444` walks `padHeigth = nHeight + 16 - nHeight
/// % 16` auxiliary rows and only skips a row once its *destination* row would run
/// off the frame. For a height that is not a multiple of 16 the V half of that
/// walk reads auxiliary Y rows past the end of the plane (e.g. height 40 reads row
/// 43 of a 40-row plane) — an upstream overread we must not feed, and one our
/// bounds-checked implementation deliberately declines to reproduce.
const V1_GEOMS: &[(usize, usize)] = LUMA_GEOMS;

/// `(destination width, destination height, auxiliary frame width)`. Coordinates
/// must be multiples of 4. The third column varies so that
/// `min(align32(dest), aux)` sometimes caps and sometimes does not.
const V2_GEOMS: &[(usize, usize, usize)] = &[
    (64, 64, 64),   // align32(64) = 64, no cap
    (48, 48, 48),   // align32(48) = 64 -> capped to 48
    (80, 48, 96),   // align32(80) = 96, aux is 96 -> no cap
    (128, 48, 128), // align32(128) = 128, no cap
    (96, 32, 96),   // no cap
    (32, 32, 32),   // no cap
    (48, 32, 64),   // align32(48) = 64, aux is 64 -> no cap, aux wider than dest
    (80, 64, 80),   // align32(80) = 96 -> capped to 80
];

fn full_frame(width: usize, height: usize) -> ExclusiveRectangle {
    rect(0, 0, width as u16, height as u16)
}

/// Apply one pass to both implementations and byte-compare all three planes.
fn check(
    prims: Prims,
    label: &str,
    ours: &Yuv444Buffer,
    theirs: &CDest,
    width: usize,
) {
    if let Err(mismatch) = compare_planes(ours.planes(), theirs.planes(), width) {
        panic!("{label} [{}]: {mismatch}", prims.label());
    }
}

#[test]
fn luma_pass_matches_freerdp() {
    let mut checked = 0usize;
    for prims in BOTH {
        for case in 0..CASES {
            let mut rng = Rng::new(0x1u64 << 32 | case as u64);
            let &(w, h) = &LUMA_GEOMS[case % LUMA_GEOMS.len()];
            let main = random_420(&mut rng, w, h);
            let src = CPlanes::from_420(&main);

            let mut ours = Yuv444Buffer::new(w as u16, h as u16);
            let mut theirs = CDest::new(w, h);

            let rects: Vec<ExclusiveRectangle> = (0..1 + rng.below(3))
                .map(|_| random_aligned_rect(&mut rng, w, h, 2))
                .collect();

            for r in &rects {
                ours.apply_luma(&main, std::slice::from_ref(r));
                fr_combine(prims, AVC444_LUMA, &src, w as u32, h as u32, &mut theirs, r);
            }

            check(
                prims,
                &format!("luma case {case} ({w}x{h}, rects {rects:?})"),
                &ours,
                &theirs,
                w,
            );
            checked += 1;
        }
    }
    println!("luma: {checked} case-runs agreed ({CASES} cases x {} primitive tables)", BOTH.len());
}

#[test]
fn luma_then_chroma_v1_matches_freerdp() {
    let mut checked = 0usize;
    for prims in BOTH {
        for case in 0..CASES {
            let mut rng = Rng::new(0x2u64 << 32 | case as u64);
            let &(w, h) = &V1_GEOMS[case % V1_GEOMS.len()];
            let main = random_420(&mut rng, w, h);
            let aux = random_420(&mut rng, w, h);
            let main_c = CPlanes::from_420(&main);
            let aux_c = CPlanes::from_420(&aux);

            let mut ours = Yuv444Buffer::new(w as u16, h as u16);
            let mut theirs = CDest::new(w, h);

            // Luma first, on its own (even-aligned) rects.
            let luma_rects: Vec<ExclusiveRectangle> = (0..1 + rng.below(3))
                .map(|_| random_aligned_rect(&mut rng, w, h, 2))
                .collect();
            for r in &luma_rects {
                ours.apply_luma(&main, std::slice::from_ref(r));
                fr_combine(prims, AVC444_LUMA, &main_c, w as u32, h as u32, &mut theirs, r);
            }

            // v1's aux-Y block phase is ROI-relative, so full frame only.
            let r = full_frame(w, h);
            ours.apply_chroma_v1(&aux, std::slice::from_ref(&r));
            fr_combine(prims, AVC444_CHROMA_V1, &aux_c, w as u32, h as u32, &mut theirs, &r);

            check(
                prims,
                &format!("luma+v1 case {case} ({w}x{h}, luma rects {luma_rects:?})"),
                &ours,
                &theirs,
                w,
            );
            checked += 1;
        }
    }
    println!("luma+v1: {checked} case-runs agreed ({CASES} cases x {} primitive tables)", BOTH.len());
}

#[test]
fn chroma_v1_alone_matches_freerdp() {
    let mut checked = 0usize;
    for prims in BOTH {
        for case in 0..CASES {
            let mut rng = Rng::new(0x3u64 << 32 | case as u64);
            let &(w, h) = &V1_GEOMS[case % V1_GEOMS.len()];
            let aux = random_420(&mut rng, w, h);
            let aux_c = CPlanes::from_420(&aux);

            let mut ours = Yuv444Buffer::new(w as u16, h as u16);
            let mut theirs = CDest::new(w, h);

            let r = full_frame(w, h);
            ours.apply_chroma_v1(&aux, std::slice::from_ref(&r));
            fr_combine(prims, AVC444_CHROMA_V1, &aux_c, w as u32, h as u32, &mut theirs, &r);

            check(prims, &format!("v1-only case {case} ({w}x{h})"), &ours, &theirs, w);
            checked += 1;
        }
    }
    println!("v1 only: {checked} case-runs agreed ({CASES} cases x {} primitive tables)", BOTH.len());
}

#[test]
fn luma_then_chroma_v2_matches_freerdp() {
    let mut checked = 0usize;
    let mut capped = 0usize;
    for prims in BOTH {
        for case in 0..CASES {
            let mut rng = Rng::new(0x4u64 << 32 | case as u64);
            let &(w, h, aux_w) = &V2_GEOMS[case % V2_GEOMS.len()];
            let main = random_420(&mut rng, w, h);
            let aux = random_420(&mut rng, aux_w, h);
            let main_c = CPlanes::from_420(&main);
            let aux_c = CPlanes::from_420(&aux);

            let total_width = v2_total_width(w, aux_w);
            if align32(w) > aux_w {
                capped += 1;
            }

            let mut ours = Yuv444Buffer::new(w as u16, h as u16);
            let mut theirs = CDest::new(w, h);

            let luma_rects: Vec<ExclusiveRectangle> = (0..1 + rng.below(3))
                .map(|_| random_aligned_rect(&mut rng, w, h, 4))
                .collect();
            for r in &luma_rects {
                ours.apply_luma(&main, std::slice::from_ref(r));
                fr_combine(prims, AVC444_LUMA, &main_c, w as u32, h as u32, &mut theirs, r);
            }

            let v2_rects: Vec<ExclusiveRectangle> = (0..1 + rng.below(3))
                .map(|_| random_aligned_rect(&mut rng, w, h, 4))
                .collect();
            for r in &v2_rects {
                ours.apply_chroma_v2(&aux, std::slice::from_ref(r));
                fr_combine(
                    prims,
                    AVC444_CHROMA_V2,
                    &aux_c,
                    total_width,
                    h as u32,
                    &mut theirs,
                    r,
                );
            }

            check(
                prims,
                &format!(
                    "luma+v2 case {case} (dest {w}x{h}, aux width {aux_w}, nTotalWidth \
                     {total_width}, luma rects {luma_rects:?}, v2 rects {v2_rects:?})"
                ),
                &ours,
                &theirs,
                w,
            );
            checked += 1;
        }
    }
    println!(
        "luma+v2: {checked} case-runs agreed ({CASES} cases x {} primitive tables); \
         {capped} of them had the align32 cap binding",
        BOTH.len()
    );
}

#[test]
fn chroma_v2_full_frame_matches_freerdp() {
    let mut checked = 0usize;
    for prims in BOTH {
        for case in 0..CASES {
            let mut rng = Rng::new(0x5u64 << 32 | case as u64);
            let &(w, h, aux_w) = &V2_GEOMS[case % V2_GEOMS.len()];
            let aux = random_420(&mut rng, aux_w, h);
            let aux_c = CPlanes::from_420(&aux);
            let total_width = v2_total_width(w, aux_w);

            let mut ours = Yuv444Buffer::new(w as u16, h as u16);
            let mut theirs = CDest::new(w, h);

            let r = full_frame(w, h);
            ours.apply_chroma_v2(&aux, std::slice::from_ref(&r));
            fr_combine(prims, AVC444_CHROMA_V2, &aux_c, total_width, h as u32, &mut theirs, &r);

            check(
                prims,
                &format!("v2-only case {case} (dest {w}x{h}, aux width {aux_w}, nTotalWidth {total_width})"),
                &ours,
                &theirs,
                w,
            );
            checked += 1;
        }
    }
    println!("v2 only: {checked} case-runs agreed ({CASES} cases x {} primitive tables)", BOTH.len());
}

#[test]
fn rgba_conversion_matches_freerdp() {
    let mut checked = 0usize;
    for prims in BOTH {
        for case in 0..CASES {
            let mut rng = Rng::new(0x6u64 << 32 | case as u64);
            let &(w, h) = &LUMA_GEOMS[case % LUMA_GEOMS.len()];

            // Randomized full-resolution planes: this exercises the even/even
            // reconstruction filter over its whole input range, including the
            // clamp and the 30-step CONDITIONAL_CLIP threshold.
            let n = w * h;
            let y: Vec<u8> = (0..n).map(|_| rng.byte()).collect();
            let u: Vec<u8> = (0..n).map(|_| rng.byte()).collect();
            let v: Vec<u8> = (0..n).map(|_| rng.byte()).collect();

            let ours = Yuv444Buffer::from_planes(y.clone(), u.clone(), v.clone(), w, h);
            let mut mine = Vec::new();
            ours.to_rgba_into(&full_frame(w, h), &mut mine);

            let src = CPlanes::from_444(&y, &u, &v, w);
            let theirs = fr_to_rgba(prims, &src, w, h);

            if let Err(mismatch) = compare_rgba(&mine, &theirs, w) {
                panic!("rgba case {case} ({w}x{h}) [{}]: {mismatch}", prims.label());
            }
            checked += 1;
        }
    }
    println!("rgba: {checked} case-runs agreed ({CASES} cases x {} primitive tables)", BOTH.len());
}

/// Not a differential test — it records which FreeRDP implementation answered, so
/// a result can say whether the SIMD paths were actually in play.
#[test]
fn report_which_primitives_answered() {
    for prims in BOTH {
        let flags = prims.flags();
        println!(
            "{}: flags = 0x{flags:08x} (EXTCPU={}, EXTGPU={})",
            prims.label(),
            flags & 1 != 0,
            flags & 2 != 0
        );
    }
    let shared = shared_impls();
    println!(
        "optimized table shares the generic YUV420CombineToYUV444: {}; \
         shares the generic YUV444ToRGB_8u_P3AC4R: {}",
        shared & 1 != 0,
        shared & 2 != 0
    );
}

/// A negative control. The oracle must be able to see a difference at all, so
/// perturb one auxiliary byte and require the comparison to fail.
#[test]
fn the_oracle_can_detect_a_planted_difference() {
    let (w, h) = (64, 64);
    let mut rng = Rng::new(0xDEAD);
    let aux = random_420(&mut rng, w, h);

    let mut poisoned = aux.clone();
    poisoned.y[0] = poisoned.y[0].wrapping_add(97);
    let aux_c = CPlanes::from_420(&poisoned);

    let mut ours = Yuv444Buffer::new(w as u16, h as u16);
    let mut theirs = CDest::new(w, h);
    let r = full_frame(w, h);
    ours.apply_chroma_v1(&aux, std::slice::from_ref(&r));
    fr_combine(Prims::Generic, AVC444_CHROMA_V1, &aux_c, w as u32, h as u32, &mut theirs, &r);

    let found = compare_planes(ours.planes(), theirs.planes(), w)
        .expect_err("a one-byte perturbation of the aux Y plane must show up");
    // aux Y row 0 feeds destination U row 1.
    assert_eq!(found.plane, "U");
    assert_eq!((found.x, found.y), (0, 1));

    // And with the same input, no difference.
    let clean = CPlanes::from_420(&aux);
    let mut theirs = CDest::new(w, h);
    fr_combine(Prims::Generic, AVC444_CHROMA_V1, &clean, w as u32, h as u32, &mut theirs, &r);
    compare_planes(ours.planes(), theirs.planes(), w).expect("identical input must agree");
}

/// A negative control for the v2 campaign specifically: the `nTotalWidth` it
/// passes is the load-bearing parameter, so prove that getting it wrong is
/// visible. Destination 80 wide with a 96-wide auxiliary frame splits at
/// `align32(80) = 96`, not at 80.
#[test]
fn the_oracle_would_catch_a_wrong_v2_total_width() {
    let (w, h, aux_w) = (80usize, 48usize, 96usize);
    let mut rng = Rng::new(0xBEEF);
    let aux = random_420(&mut rng, aux_w, h);
    let aux_c = CPlanes::from_420(&aux);
    let r = full_frame(w, h);

    let mut ours = Yuv444Buffer::new(w as u16, h as u16);
    ours.apply_chroma_v2(&aux, std::slice::from_ref(&r));

    // The right value: agreement.
    let mut theirs = CDest::new(w, h);
    fr_combine(Prims::Generic, AVC444_CHROMA_V2, &aux_c, 96, h as u32, &mut theirs, &r);
    compare_planes(ours.planes(), theirs.planes(), w)
        .expect("nTotalWidth = align32(80) = 96 must agree");

    // The plausible-but-wrong value: disagreement, in V (the half that is offset
    // by nTotalWidth/2 and nTotalWidth/4).
    let mut theirs = CDest::new(w, h);
    fr_combine(Prims::Generic, AVC444_CHROMA_V2, &aux_c, w as u32, h as u32, &mut theirs, &r);
    let found = compare_planes(ours.planes(), theirs.planes(), w)
        .expect_err("nTotalWidth = 80 must NOT agree");
    assert_eq!(found.plane, "V");
}

/// A negative control for the RGBA campaign: prove the even/even reconstruction
/// filter is actually exercised. A conversion that skipped it — the same matrix,
/// applied to the stored chroma as-is — must disagree with FreeRDP.
#[test]
fn the_oracle_would_catch_a_missing_reconstruction_filter() {
    let (w, h) = (64usize, 64usize);
    let mut rng = Rng::new(0xF00D);
    let n = w * h;
    let y: Vec<u8> = (0..n).map(|_| rng.byte()).collect();
    let u: Vec<u8> = (0..n).map(|_| rng.byte()).collect();
    let v: Vec<u8> = (0..n).map(|_| rng.byte()).collect();

    let src = CPlanes::from_444(&y, &u, &v, w);
    let theirs = fr_to_rgba(Prims::Generic, &src, w, h);

    // The FreeRDP fixed-point matrix with no reconstruction step.
    let mut naive = Vec::with_capacity(n * 4);
    for i in 0..n {
        let c = 256 * i32::from(y[i]);
        let d = i32::from(u[i]) - 128;
        let e = i32::from(v[i]) - 128;
        naive.push((((c + 403 * e) >> 8).clamp(0, 255)) as u8);
        naive.push((((c - 48 * d - 120 * e) >> 8).clamp(0, 255)) as u8);
        naive.push((((c + 475 * d) >> 8).clamp(0, 255)) as u8);
        naive.push(0xFF);
    }

    let found = compare_rgba(&naive, &theirs, w)
        .expect_err("dropping the reconstruction filter must change the image");
    assert_eq!(found.x % 2, 0, "only even/even pixels are reconstructed");
    assert_eq!(found.y % 2, 0, "only even/even pixels are reconstructed");
}
