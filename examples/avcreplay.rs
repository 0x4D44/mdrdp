//! Replay captured AVC444 container payloads (from `--capture-failures`) through the
//! production decode + combine pipeline, offline, and report the temporal stability
//! of a probe region.
//!
//!     cargo run --release --example avcreplay -- <dir> <l,t,r,b> [--ppm <outdir>]
//!
//! `<dir>` holds `avc444-NNN-<codec>.bin` files as written by the capture hook.
//! `<l,t,r,b>` is the probe rect in surface pixels (exclusive right/bottom).
//! `--ppm` additionally dumps the probe region of every frame as a PPM image.
//!
//! Per frame it prints: the LC mode, region rect counts, the probe region's mean
//! RGB, how many probe pixels moved by more than a visibility threshold since the
//! previous frame, the buffer-plane YUV at four fixed pixels covering the four
//! 2x2 phases (even/even is main-frame territory, odd columns come from the aux Y
//! plane, odd-row/even-col from the aux U/V planes) — so instability attributes to
//! a source — and, on luma-carrying frames, `avgd`: the distribution of per-block
//! chroma-average deltas between the incoming main frame and the buffer's stored
//! even/even values over the update's rects (counts above 4/8/16/32, then max).
//! That distribution separates codec noise on re-encoded static content from a
//! genuine content change, and sized `STALE_AVG_DELTA` in the combiner.
//!
//! After the run it prints a transient-spike report: probe pixels whose blue
//! channel exceeds BOTH temporal neighbours by more than 40 — the objective form
//! of MDR-BUG-FLUX-00010's one-frame blue tint (reconstruction against
//! one-catch-up-stale odd chroma overshoots U upward).

use ironrdp::core::{Decode as _, ReadCursor};
use ironrdp::pdu::geometry::ExclusiveRectangle;
use ironrdp_egfx::pdu::{Avc444BitmapStream, Encoding};
use ironrdp_graphics::avc444::{Yuv420Frame, Yuv444Buffer};

const W: u16 = 1920;
const H: u16 = 1080;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: avcreplay <dir> <l,t,r,b> [--ppm <outdir>]");
        std::process::exit(2);
    }
    let dir = &args[0];
    let probe = parse_rect(&args[1]);
    let ppm_dir = args
        .iter()
        .position(|a| a == "--ppm")
        .and_then(|i| args.get(i + 1))
        .cloned();

    let mut files: Vec<_> = std::fs::read_dir(dir)
        .expect("read capture dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "bin"))
        .collect();
    files.sort();

    let mut decoder = mdrdp::h264::hardware_decoder().expect("hardware decoder");
    let mut buffer = Yuv444Buffer::new(W, H);
    let mut main = Yuv420Frame::default();
    let mut aux = Yuv420Frame::default();
    let mut prev_rgba: Option<Vec<u8>> = None;
    let mut frames: Vec<Vec<u8>> = Vec::new();

    // Fixed probe pixels covering the four 2x2 phases, inside the probe rect.
    let px = usize::from(probe.left) / 2 * 2 + 20;
    let py = usize::from(probe.top) / 2 * 2 + 20;
    let phases = [(px, py), (px + 1, py), (px, py + 1), (px + 1, py + 1)];

    println!("replaying {} files; probe {probe:?}", files.len());
    for (idx, path) in files.iter().enumerate() {
        let data = std::fs::read(path).expect("read payload");
        let mut cursor = ReadCursor::new(&data);
        let stream = match Avc444BitmapStream::decode(&mut cursor) {
            Ok(s) => s,
            Err(e) => {
                println!("[{idx:03}] parse failed: {e}");
                continue;
            }
        };
        let rects1 = to_rects(&stream.stream1.rectangles);
        let rects2 = stream.stream2.as_ref().map(|s| to_rects(&s.rectangles));

        let lc = stream.encoding;
        let mode = match lc {
            e if e == Encoding::LUMA_AND_CHROMA => "L+C",
            e if e == Encoding::LUMA => "L  ",
            _ => "  C",
        };

        let mut avgd = String::new();
        match lc {
            e if e == Encoding::LUMA_AND_CHROMA => {
                decoder
                    .decode_yuv420(stream.stream1.data, &mut main)
                    .expect("luma decode");
                decoder
                    .decode_yuv420(stream.stream2.as_ref().unwrap().data, &mut aux)
                    .expect("chroma decode");
                avgd = avg_delta_stats(&buffer, &main, &rects1);
                buffer.apply_luma(&main, &rects1);
                buffer.apply_chroma_v2(&aux, rects2.as_ref().unwrap());
            }
            e if e == Encoding::LUMA => {
                decoder
                    .decode_yuv420(stream.stream1.data, &mut main)
                    .expect("luma decode");
                avgd = avg_delta_stats(&buffer, &main, &rects1);
                buffer.apply_luma(&main, &rects1);
            }
            _ => {
                decoder
                    .decode_yuv420(stream.stream1.data, &mut aux)
                    .expect("chroma decode");
                buffer.apply_chroma_v2(&aux, &rects1);
            }
        }

        // Did this update touch the probe rect at all?
        let touched = rects1
            .iter()
            .chain(rects2.iter().flatten())
            .any(|r| overlaps(r, &probe));

        let mut rgba = Vec::new();
        buffer.to_rgba_into(&probe, &mut rgba);
        let (mr, mg, mb) = mean_rgb(&rgba);
        let (moved, maxd) = match &prev_rgba {
            Some(prev) => pixel_moves(prev, &rgba, 30),
            None => (0, 0),
        };

        let (y_pl, u_pl, v_pl) = buffer.planes();
        let w = usize::from(W);
        let mut phase_s = String::new();
        for (x, y) in phases {
            let i = y * w + x;
            phase_s.push_str(&format!(
                " ({},{}) Y{:3} U{:3} V{:3}",
                x % 2,
                y % 2,
                y_pl[i],
                u_pl[i],
                v_pl[i]
            ));
        }

        println!(
            "[{idx:03}] {mode} r1={:2} r2={:2} touched={} mean=({mr:5.1},{mg:5.1},{mb:5.1}) moved>{}: {moved:6} maxd={maxd:3} |{phase_s}{avgd}",
            rects1.len(),
            rects2.as_ref().map_or(0, |r| r.len()),
            if touched { "Y" } else { "n" },
            30,
        );

        if let Some(out) = &ppm_dir {
            std::fs::create_dir_all(out).unwrap();
            write_ppm(
                &format!("{out}/frame-{idx:03}.ppm"),
                &rgba,
                usize::from(probe.right - probe.left),
                usize::from(probe.bottom - probe.top),
            );
        }
        prev_rgba = Some(rgba.clone());
        frames.push(rgba);
    }

    // Transient-spike report. Two per-pixel signals, each requiring the anomaly to
    // last exactly one frame (both temporal neighbours disagree):
    // - `spike`: B exceeds both neighbours by more than 40. Catches the overshoot
    //   but also counts the fix's deliberate one-frame flat-average softening, so
    //   it measures "how much moved", not "how wrong".
    // - `blueflip`: the pixel is blue-dominant (B > R + 20) while BOTH neighbours
    //   are yellow/red-dominant (B < R). A hue that was never on screen — the
    //   defining wrongness of MDR-BUG-FLUX-00010, and a hue no flat average of
    //   the block's real colours can produce on yellow-on-black content.
    println!(
        "-- one-frame transients (spike: B > both neighbours + 40; blueflip: B>R+20 vs B<R) --"
    );
    let (mut total, mut total_flips) = (0usize, 0usize);
    for t in 1..frames.len().saturating_sub(1) {
        let (mut count, mut worst, mut flips) = (0usize, 0u8, 0usize);
        for ((p, c), n) in frames[t - 1]
            .chunks_exact(4)
            .zip(frames[t].chunks_exact(4))
            .zip(frames[t + 1].chunks_exact(4))
        {
            let over = c[2].saturating_sub(p[2].max(n[2]));
            if over > 40 {
                count += 1;
                worst = worst.max(over);
            }
            if c[2] > c[0].saturating_add(20) && p[2] < p[0] && n[2] < n[0] {
                flips += 1;
            }
        }
        if count > 0 || flips > 0 {
            println!("[{t:03}] spike pixels {count:6}  worst +{worst}  blueflips {flips:6}");
        }
        total += count;
        total_flips += flips;
    }
    println!("totals across run: spikes {total}, blueflips {total_flips}");
}

/// Distribution of per-block chroma-average deltas (max of |dU|, |dV|) between the
/// incoming main frame and the buffer's stored even/even values, over the given
/// rects: " avgd n=N >4:a >8:b >16:c >32:d max=m".
fn avg_delta_stats(
    buffer: &Yuv444Buffer,
    main: &Yuv420Frame,
    rects: &[ExclusiveRectangle],
) -> String {
    let (_, u_pl, v_pl) = buffer.planes();
    let w = buffer.width();
    let uv_row = main.uv_row();
    let (mut n, mut over, mut max) = (0usize, [0usize; 4], 0u8);
    for r in rects {
        let left = usize::from(r.left).min(w);
        let top = usize::from(r.top).min(buffer.height());
        let right = usize::from(r.right).min(w).min(main.width);
        let bottom = usize::from(r.bottom).min(buffer.height()).min(main.height);
        let mut dy = top.div_ceil(2) * 2;
        while dy < bottom {
            let mut dx = left.div_ceil(2) * 2;
            while dx < right {
                let s = (dy / 2) * uv_row + dx / 2;
                let i = dy * w + dx;
                let d = u_pl[i].abs_diff(main.u[s]).max(v_pl[i].abs_diff(main.v[s]));
                n += 1;
                for (bin, thresh) in over.iter_mut().zip([4u8, 8, 16, 32]) {
                    if d > thresh {
                        *bin += 1;
                    }
                }
                max = max.max(d);
                dx += 2;
            }
            dy += 2;
        }
    }
    format!(
        " avgd n={n} >4:{} >8:{} >16:{} >32:{} max={max}",
        over[0], over[1], over[2], over[3]
    )
}

fn parse_rect(s: &str) -> ExclusiveRectangle {
    let v: Vec<u16> = s
        .split(',')
        .map(|p| p.parse().expect("rect number"))
        .collect();
    assert_eq!(v.len(), 4, "rect must be l,t,r,b");
    ExclusiveRectangle {
        left: v[0],
        top: v[1],
        right: v[2],
        bottom: v[3],
    }
}

/// Wire rects are exclusive despite the Inclusive typing (see the vendored client).
fn to_rects(rects: &[ironrdp::pdu::geometry::InclusiveRectangle]) -> Vec<ExclusiveRectangle> {
    rects
        .iter()
        .map(|r| ExclusiveRectangle {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        })
        .collect()
}

fn overlaps(a: &ExclusiveRectangle, b: &ExclusiveRectangle) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

fn mean_rgb(rgba: &[u8]) -> (f64, f64, f64) {
    let n = (rgba.len() / 4) as f64;
    let (mut r, mut g, mut b) = (0f64, 0f64, 0f64);
    for px in rgba.chunks_exact(4) {
        r += f64::from(px[0]);
        g += f64::from(px[1]);
        b += f64::from(px[2]);
    }
    (r / n, g / n, b / n)
}

/// (count of pixels whose |dR|+|dG|+|dB| exceeds `thresh`, max such distance).
fn pixel_moves(prev: &[u8], cur: &[u8], thresh: u16) -> (usize, u16) {
    let mut moved = 0usize;
    let mut maxd = 0u16;
    for (p, c) in prev.chunks_exact(4).zip(cur.chunks_exact(4)) {
        let d = u16::from(p[0].abs_diff(c[0]))
            + u16::from(p[1].abs_diff(c[1]))
            + u16::from(p[2].abs_diff(c[2]));
        if d > thresh {
            moved += 1;
        }
        maxd = maxd.max(d);
    }
    (moved, maxd)
}

fn write_ppm(path: &str, rgba: &[u8], w: usize, h: usize) {
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    for px in rgba.chunks_exact(4) {
        out.extend_from_slice(&px[..3]);
    }
    std::fs::write(path, out).unwrap();
}
