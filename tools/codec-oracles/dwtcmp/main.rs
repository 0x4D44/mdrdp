// Third-party code: see tools/NOTICE for provenance and licence.
// Two copies of the Rust idwt: one with the current wrapping `t`, one with a saturating `t`.
fn tw(v: i32) -> i16 { v as i16 }
fn ts(v: i32) -> i16 { if v < i16::MIN as i32 { i16::MIN } else if v > i16::MAX as i32 { i16::MAX } else { v as i16 } }

fn idwt_row(low: &[i16], high: &[i16], dst: &mut [i16], t: fn(i32) -> i16) {
    let n_l = low.len();
    let n_h = high.len();
    let mut h0 = i32::from(high[0]);
    let mut x0 = t(i32::from(low[0]) - h0);
    let mut x2 = x0;
    let mut di = 0;
    for j in 0..n_h - 1 {
        let h1 = i32::from(high[j + 1]);
        let l_val = i32::from(low[j + 1]);
        x2 = t(l_val - (h0 + h1) / 2);
        let x1 = t((i32::from(x0) + i32::from(x2)) / 2 + 2 * h0);
        dst[di] = x0;
        dst[di + 1] = x1;
        di += 2;
        x0 = x2;
        h0 = h1;
    }
    if n_l <= n_h + 1 {
        if n_l <= n_h {
            dst[di] = x2;
            dst[di + 1] = t(i32::from(x2) + 2 * h0);
        } else {
            let x_new = t(i32::from(low[n_h]) - h0);
            dst[di] = x2;
            dst[di + 1] = t((i32::from(x_new) + i32::from(x2)) / 2 + 2 * h0);
            dst[di + 2] = x_new;
        }
    } else {
        let x_new = t(i32::from(low[n_h]) - h0 / 2);
        dst[di] = x2;
        dst[di + 1] = t((i32::from(x_new) + i32::from(x2)) / 2 + 2 * h0);
        dst[di + 2] = x_new;
        dst[di + 3] = t((i32::from(x_new) + i32::from(low[n_h + 1])) / 2);
    }
}

#[allow(clippy::too_many_arguments)]
fn idwt_col(src: &[i16], l_start: usize, h_start: usize, src_stride: usize, dst: &mut [i16],
            d_start: usize, dst_stride: usize, n_l: usize, n_h: usize, t: fn(i32) -> i16) {
    let l = |i: usize| i32::from(src[l_start + i * src_stride]);
    let h = |i: usize| i32::from(src[h_start + i * src_stride]);
    let mut h0 = h(0);
    let mut x0 = t(l(0) - h0);
    let mut x2 = x0;
    let mut d = d_start;
    for j in 0..n_h - 1 {
        let h1 = h(j + 1);
        x2 = t(l(j + 1) - (h0 + h1) / 2);
        let x1 = t((i32::from(x0) + i32::from(x2)) / 2 + 2 * h0);
        dst[d] = x0; d += dst_stride;
        dst[d] = x1; d += dst_stride;
        x0 = x2; h0 = h1;
    }
    if n_l <= n_h + 1 {
        if n_l <= n_h {
            dst[d] = x2; d += dst_stride;
            dst[d] = t(i32::from(x2) + 2 * h0);
        } else {
            let x_new = t(l(n_h) - h0);
            dst[d] = x2; d += dst_stride;
            dst[d] = t((i32::from(x_new) + i32::from(x2)) / 2 + 2 * h0); d += dst_stride;
            dst[d] = x_new;
        }
    } else {
        let x_new = t(l(n_h) - h0 / 2);
        dst[d] = x2; d += dst_stride;
        dst[d] = t((i32::from(x_new) + i32::from(x2)) / 2 + 2 * h0); d += dst_stride;
        dst[d] = x_new; d += dst_stride;
        dst[d] = t((i32::from(x_new) + l(n_h + 1)) / 2);
    }
}

fn decode_block(buffer: &mut [i16], temp: &mut [i16], n_l: usize, n_h: usize, t: fn(i32) -> i16) {
    let dst_w = n_l + n_h;
    let hl_off = 0;
    let lh_off = n_h * n_l;
    let hh_off = lh_off + n_l * n_h;
    let ll_off = hh_off + n_h * n_h;
    let l_off = 0;
    let h_off = n_l * dst_w;
    for row in 0..n_l {
        let ll_start = ll_off + row * n_l;
        let hl_start = hl_off + row * n_h;
        let l_start = l_off + row * dst_w;
        let (lo, hi) = (buffer[ll_start..ll_start + n_l].to_vec(), buffer[hl_start..hl_start + n_h].to_vec());
        idwt_row(&lo, &hi, &mut temp[l_start..l_start + dst_w], t);
    }
    for row in 0..n_h {
        let lh_start = lh_off + row * n_l;
        let hh_start = hh_off + row * n_h;
        let h_start = h_off + row * dst_w;
        let (lo, hi) = (buffer[lh_start..lh_start + n_l].to_vec(), buffer[hh_start..hh_start + n_h].to_vec());
        idwt_row(&lo, &hi, &mut temp[h_start..h_start + dst_w], t);
    }
    for col in 0..dst_w {
        idwt_col(temp, l_off + col, h_off + col, dst_w, buffer, col, dst_w, n_l, n_h, t);
    }
}

fn decode(buffer: &mut [i16], temp: &mut [i16], t: fn(i32) -> i16) {
    decode_block(&mut buffer[3807..], temp, 9, 8, t);
    decode_block(&mut buffer[3007..], temp, 17, 16, t);
    decode_block(buffer, temp, 33, 31, t);
}

fn run(t: fn(i32) -> i16, seed_hl3: i16) -> Vec<i16> {
    let mut b = vec![0i16; 4096];
    b[3807] = seed_hl3;
    let mut tmp = vec![0i16; 4096];
    decode(&mut b, &mut tmp, t);
    b
}

fn main() {
    let w = run(tw, 32767);
    let s = run(ts, 32767);
    println!("HL3[0]=32767 case:");
    println!("  wrapping  first 8: {:?}", &w[0..8]);
    println!("  saturating first 8: {:?}", &s[0..8]);
    println!("  differing samples: {}", w.iter().zip(s.iter()).filter(|(a,b)| a!=b).count());

    // full-range pseudo-random
    let mut b1 = vec![0i16; 4096];
    let mut seed: u32 = 0x1234_5678;
    for v in b1.iter_mut() { seed = seed.wrapping_mul(1103515245).wrapping_add(12345); *v = (seed >> 16) as i16; }
    let mut b2 = b1.clone();
    let mut tmp = vec![0i16; 4096];
    decode(&mut b1, &mut tmp, tw);
    let mut tmp2 = vec![0i16; 4096];
    decode(&mut b2, &mut tmp2, ts);
    println!("full-range random: differing samples {}", b1.iter().zip(b2.iter()).filter(|(a,b)| a!=b).count());

    // realistic-magnitude coefficients (|c| <= 2048): does it ever overflow?
    let mut c1 = vec![0i16; 4096];
    let mut seed2: u32 = 99;
    for v in c1.iter_mut() { seed2 = seed2.wrapping_mul(1103515245).wrapping_add(12345); *v = (((seed2 >> 16) as i16) >> 4) as i16; }
    let mut c2 = c1.clone();
    decode(&mut c1, &mut tmp, tw);
    decode(&mut c2, &mut tmp2, ts);
    println!("|c|<=2048 random: differing samples {}", c1.iter().zip(c2.iter()).filter(|(a,b)| a!=b).count());

    // find smallest single HL3[0] magnitude that diverges
    for m in 1..=32767i32 {
        let a = run(tw, m as i16); let bb = run(ts, m as i16);
        if a != bb { println!("smallest single-coefficient HL3[0] that diverges: {}", m); break; }
    }
}
