// Third-party code: see tools/NOTICE for provenance and licence.
// Differential test: mdrdp's vendored rlgr.rs decode  vs  a faithful transcription of
// FreeRDP's rfx_rlgr_decode (rfx_rlgr.c).
#![allow(clippy::all)]

use core::cmp::min;
use core::ops;

use bitvec::field::BitField as _;
use bitvec::prelude::*;

// ---------------------------------------------------------------------------
// utils::Bits (verbatim from vendor/ironrdp-graphics/src/utils.rs)
// ---------------------------------------------------------------------------
pub(crate) struct Bits<'a> {
    bits_slice: &'a BitSlice<u8, Msb0>,
    remaining_bits_of_last_byte: usize,
}

impl<'a> Bits<'a> {
    pub(crate) fn new(bits_slice: &'a BitSlice<u8, Msb0>) -> Self {
        Self {
            bits_slice,
            remaining_bits_of_last_byte: 0,
        }
    }

    pub(crate) fn split_to(&mut self, at: usize) -> &'a BitSlice<u8, Msb0> {
        let (value, new_bits) = self.bits_slice.split_at(at);
        self.bits_slice = new_bits;
        self.remaining_bits_of_last_byte = (self.remaining_bits_of_last_byte + at) % 8;
        value
    }
}

impl ops::Deref for Bits<'_> {
    type Target = BitSlice<u8, Msb0>;
    fn deref(&self) -> &Self::Target {
        self.bits_slice
    }
}

// ---------------------------------------------------------------------------
// OURS: verbatim copy of vendor/ironrdp-graphics/src/rlgr.rs
// ---------------------------------------------------------------------------
const KP_MAX: u32 = 80;
const LS_GR: u32 = 3;
const UP_GR: u32 = 4;
const DN_GR: u32 = 6;
const UQ_GR: u32 = 3;
const DQ_GR: u32 = 3;

#[derive(Copy, Clone, PartialEq, Debug)]
pub enum EntropyAlgorithm {
    Rlgr1,
    Rlgr3,
}

#[derive(Debug)]
pub enum RlgrError {
    EmptyTile,
    InvalidIntegralConversion(&'static str),
}

macro_rules! write_byte {
    ($output:ident, $value:ident) => {
        if !$output.is_empty() {
            $output[0] = $value;
            $output = &mut $output[1..];
        } else {
            break;
        }
    };
}

macro_rules! try_split_bits {
    ($bits:ident, $n:expr) => {
        if $bits.len() < $n {
            break;
        } else {
            $bits.split_to($n)
        }
    };
}

struct BitStreamW<'a> {
    bits: &'a mut BitSlice<u8, Msb0>,
    idx: usize,
}

impl<'a> BitStreamW<'a> {
    fn new(slice: &'a mut [u8]) -> Self {
        let bits = slice.view_bits_mut::<Msb0>();
        Self { bits, idx: 0 }
    }
    fn output_bit(&mut self, count: usize, val: bool) {
        self.bits[self.idx..self.idx + count].fill(val);
        self.idx += count;
    }
    fn output_bits(&mut self, num_bits: usize, val: u32) {
        self.bits[self.idx..self.idx + num_bits].store_be(val);
        self.idx += num_bits;
    }
    fn len(&self) -> usize {
        self.idx.div_ceil(8)
    }
}

pub fn encode(mode: EntropyAlgorithm, input: &[i16], tile: &mut [u8]) -> Result<usize, RlgrError> {
    if input.is_empty() {
        return Err(RlgrError::EmptyTile);
    }

    let mut k: u32 = 1;
    let kr: u32 = 1;
    let mut kp: u32 = k << LS_GR;
    let mut krp: u32 = kr << LS_GR;
    let mut bits = BitStreamW::new(tile);

    let mut input = input.iter().peekable();

    while input.peek().is_some() {
        match CompressionMode::from(k) {
            CompressionMode::RunLength => {
                let mut nz = 0;
                while let Some(&&x) = input.peek() {
                    if x == 0 {
                        nz += 1;
                        input.next();
                    } else {
                        break;
                    }
                }
                let mut runmax: u32 = 1 << k;
                while nz >= runmax {
                    bits.output_bit(1, false);
                    nz -= runmax;
                    kp = min(kp + UP_GR, KP_MAX);
                    k = kp >> LS_GR;
                    runmax = 1 << k;
                }
                bits.output_bit(1, true);
                bits.output_bits(k as usize, nz);

                if let Some(val) = input.next() {
                    let mag = u32::from(val.unsigned_abs());
                    bits.output_bit(1, *val < 0);
                    code_gr(&mut bits, &mut krp, mag - 1);
                }
                kp = kp.saturating_sub(DN_GR);
                k = kp >> LS_GR;
            }
            CompressionMode::GolombRice => {
                let input_first = *input.next().expect("some");
                match mode {
                    EntropyAlgorithm::Rlgr1 => {
                        let two_ms = get_2magsign(input_first);
                        code_gr(&mut bits, &mut krp, two_ms);
                        if two_ms == 0 {
                            kp = min(kp + if std::env::var("FIX_UQ").is_ok() { UQ_GR } else { UP_GR }, KP_MAX);
                        } else {
                            kp = kp.saturating_sub(DQ_GR);
                        }
                        k = kp >> LS_GR;
                    }
                    EntropyAlgorithm::Rlgr3 => {
                        let two_ms1 = get_2magsign(input_first);
                        let two_ms2 = input.next().map(|&n| get_2magsign(n)).unwrap_or(1);
                        let sum2ms = two_ms1 + two_ms2;
                        code_gr(&mut bits, &mut krp, sum2ms);
                        let m = 32 - sum2ms.leading_zeros() as usize;
                        if m != 0 {
                            bits.output_bits(m, two_ms1);
                        }
                        if two_ms1 != 0 && two_ms2 != 0 {
                            kp = kp.saturating_sub(2 * DQ_GR);
                            k = kp >> LS_GR;
                        } else if two_ms1 == 0 && two_ms2 == 0 {
                            kp = min(kp + 2 * UQ_GR, KP_MAX);
                            k = kp >> LS_GR;
                        }
                    }
                }
            }
        }
    }

    Ok(bits.len())
}

fn get_2magsign(val: i16) -> u32 {
    let sign = if val < 0 { 1 } else { 0 };
    (u32::from(val.unsigned_abs())) * 2 - sign
}

fn code_gr(bits: &mut BitStreamW<'_>, krp: &mut u32, val: u32) {
    let kr = (*krp >> LS_GR) as usize;
    let vk = val >> kr;
    let vk_usize = vk as usize;
    bits.output_bit(vk_usize, true);
    bits.output_bit(1, false);
    if kr != 0 {
        let remainder = val & ((1 << kr) - 1);
        bits.output_bits(kr, remainder);
    }
    if vk == 0 {
        *krp = krp.saturating_sub(2);
    } else if vk > 1 {
        *krp = min(*krp + vk, KP_MAX);
    }
}

pub fn decode(mode: EntropyAlgorithm, tile: &[u8], mut output: &mut [i16]) -> Result<(), RlgrError> {
    if tile.is_empty() {
        return Err(RlgrError::EmptyTile);
    }

    let mut k: u32 = 1;
    let mut kr: u32 = 1;
    let mut kp: u32 = k << LS_GR;
    let mut krp: u32 = kr << LS_GR;

    let mut bits = Bits::new(BitSlice::from_slice(tile));

    while !bits.is_empty() && !output.is_empty() {
        match CompressionMode::from(k) {
            CompressionMode::RunLength => {
                let number_of_zeros = truncate_leading_value(&mut bits, false);
                try_split_bits!(bits, 1);
                let run =
                    count_run(number_of_zeros, &mut k, &mut kp) + load_be_u32(try_split_bits!(bits, k as usize));

                let sign_bit = try_split_bits!(bits, 1).load_be::<u8>();

                let number_of_ones = truncate_leading_value(&mut bits, true);
                try_split_bits!(bits, 1);

                let code_remainder =
                    load_be_u32(try_split_bits!(bits, kr as usize)) + ((number_of_ones as u32) << kr);

                update_parameters_according_to_number_of_ones(number_of_ones, &mut kr, &mut krp);
                kp = kp.saturating_sub(DN_GR);
                k = kp >> LS_GR;

                let magnitude = compute_rl_magnitude(sign_bit, code_remainder)?;

                let size = min(run as usize, output.len());
                fill(&mut output[..size], 0);
                output = &mut output[size..];
                write_byte!(output, magnitude);
            }
            CompressionMode::GolombRice => {
                let number_of_ones = truncate_leading_value(&mut bits, true);
                try_split_bits!(bits, 1);

                let code_remainder =
                    load_be_u32(try_split_bits!(bits, kr as usize)) + ((number_of_ones as u32) << kr);

                update_parameters_according_to_number_of_ones(number_of_ones, &mut kr, &mut krp);

                match mode {
                    EntropyAlgorithm::Rlgr1 => {
                        let magnitude = compute_rlgr1_magnitude(code_remainder, &mut k, &mut kp)?;
                        write_byte!(output, magnitude);
                    }
                    EntropyAlgorithm::Rlgr3 => {
                        let n_index = compute_n_index(code_remainder);
                        let val1 = load_be_u32(try_split_bits!(bits, n_index));
                        let val2 = code_remainder - val1;
                        if val1 != 0 && val2 != 0 {
                            kp = kp.saturating_sub(2 * DQ_GR);
                            k = kp >> LS_GR;
                        } else if val1 == 0 && val2 == 0 {
                            kp = min(kp + 2 * UQ_GR, KP_MAX);
                            k = kp >> LS_GR;
                        }
                        let magnitude = compute_rlgr3_magnitude(val1)?;
                        write_byte!(output, magnitude);
                        let magnitude = compute_rlgr3_magnitude(val2)?;
                        write_byte!(output, magnitude);
                    }
                }
            }
        }
    }

    fill(output, 0);
    Ok(())
}

fn fill(buffer: &mut [i16], value: i16) {
    for v in buffer {
        *v = value;
    }
}

fn load_be_u32(s: &BitSlice<u8, Msb0>) -> u32 {
    if s.is_empty() {
        0
    } else {
        s.load_be::<u32>()
    }
}

fn truncate_leading_value(bits: &mut Bits<'_>, value: bool) -> usize {
    let leading_values = if value { bits.leading_ones() } else { bits.leading_zeros() };
    bits.split_to(leading_values);
    leading_values
}

fn count_run(number_of_zeros: usize, k: &mut u32, kp: &mut u32) -> u32 {
    core::iter::repeat_with(|| {
        let run = 1 << *k;
        *kp = min(*kp + UP_GR, KP_MAX);
        *k = *kp >> LS_GR;
        run
    })
    .take(number_of_zeros)
    .sum()
}

fn compute_rl_magnitude(sign_bit: u8, code_remainder: u32) -> Result<i16, RlgrError> {
    let rl_magnitude = i16::try_from(code_remainder + 1)
        .map_err(|_| RlgrError::InvalidIntegralConversion("code remainder + 1"))?;
    if sign_bit != 0 {
        Ok(-rl_magnitude)
    } else {
        Ok(rl_magnitude)
    }
}

fn compute_rlgr1_magnitude(code_remainder: u32, k: &mut u32, kp: &mut u32) -> Result<i16, RlgrError> {
    if code_remainder == 0 {
        *kp = min(*kp + UQ_GR, KP_MAX);
        *k = *kp >> LS_GR;
        Ok(0)
    } else {
        *kp = kp.saturating_sub(DQ_GR);
        *k = *kp >> LS_GR;
        if !code_remainder.is_multiple_of(2) {
            Ok(-i16::try_from((code_remainder + 1) >> 1)
                .map_err(|_| RlgrError::InvalidIntegralConversion("(code remainder + 1) >> 1"))?)
        } else {
            i16::try_from(code_remainder >> 1)
                .map_err(|_| RlgrError::InvalidIntegralConversion("code remainder >> 1"))
        }
    }
}

fn compute_rlgr3_magnitude(val: u32) -> Result<i16, RlgrError> {
    if !val.is_multiple_of(2) {
        Ok(-i16::try_from((val + 1) >> 1)
            .map_err(|_| RlgrError::InvalidIntegralConversion("(val + 1) >> 1"))?)
    } else {
        i16::try_from(val >> 1).map_err(|_| RlgrError::InvalidIntegralConversion("val >> 1"))
    }
}

fn compute_n_index(code_remainder: u32) -> usize {
    if code_remainder == 0 {
        return 0;
    }
    let code_bytes = code_remainder.to_be_bytes();
    let code_bits = BitSlice::<u8, Msb0>::from_slice(code_bytes.as_ref());
    let leading_zeros = code_bits.leading_zeros();
    32 - leading_zeros
}

fn update_parameters_according_to_number_of_ones(number_of_ones: usize, kr: &mut u32, krp: &mut u32) {
    if number_of_ones == 0 {
        *krp = (*krp).saturating_sub(2);
        *kr = *krp >> LS_GR;
    } else if number_of_ones > 1 {
        *krp = min(*krp + (number_of_ones as u32), KP_MAX);
        *kr = *krp >> LS_GR;
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
enum CompressionMode {
    RunLength,
    GolombRice,
}

impl From<u32> for CompressionMode {
    fn from(m: u32) -> Self {
        if m != 0 {
            Self::RunLength
        } else {
            Self::GolombRice
        }
    }
}

// ---------------------------------------------------------------------------
// REFERENCE: faithful transcription of FreeRDP rfx_rlgr_decode (rfx_rlgr.c)
//
// wBitStream model: MSB-first reader over the byte buffer; `accumulator` is the
// next 32 bits, zero-padded past the end of the buffer; position counts consumed
// bits; length = capacity * 8.
// ---------------------------------------------------------------------------
struct RefBs<'a> {
    buf: &'a [u8],
    position: usize, // bits consumed
    length: usize,   // total bits
}

impl<'a> RefBs<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self {
            buf,
            position: 0,
            length: buf.len() * 8,
        }
    }
    fn accumulator(&self) -> u32 {
        let mut acc: u32 = 0;
        for i in 0..32 {
            let bitpos = self.position + i;
            let byte = bitpos / 8;
            let bit = if byte < self.buf.len() {
                (self.buf[byte] >> (7 - (bitpos % 8))) & 1
            } else {
                0
            };
            acc = (acc << 1) | u32::from(bit);
        }
        acc
    }
    fn remaining(&self) -> usize {
        self.length.saturating_sub(self.position)
    }
    fn shift(&mut self, n: usize) {
        self.position += n;
    }
}

fn lzcnt_s(x: u32) -> u32 {
    if x == 0 { 32 } else { x.leading_zeros() }
}

/// returns Err(()) when the C code would return -1
fn decode_ref(mode: EntropyAlgorithm, src: &[u8], dst: &mut [i16]) -> Result<(), ()> {
    let dst_size = dst.len();
    if src.is_empty() || dst_size == 0 {
        return Err(());
    }

    let mut k: u32 = 1;
    let mut kp: u32 = k << LS_GR;
    let mut kr: u32 = 1;
    let mut krp: u32 = kr << LS_GR;

    let mut bs = RefBs::new(src);
    let mut out_i: usize = 0; // pOutput - pDstData

    while bs.remaining() > 0 && out_i < dst_size {
        if k != 0 {
            // ---- Run-Length mode ----
            let mut run: usize = 0;

            let mut cnt = lzcnt_s(bs.accumulator()) as usize;
            let mut nbits = bs.remaining();
            if cnt > nbits {
                cnt = nbits;
            }
            let mut vk = cnt as u32;
            while cnt == 32 && bs.remaining() > 0 {
                bs.shift(32);
                cnt = lzcnt_s(bs.accumulator()) as usize;
                nbits = bs.remaining();
                if cnt > nbits {
                    cnt = nbits;
                }
                vk += cnt as u32;
            }
            bs.shift((vk % 32) as usize);

            if bs.remaining() < 1 {
                break;
            }
            bs.shift(1);

            let mut vkc = vk;
            while vkc != 0 {
                vkc -= 1;
                run += 1usize << k;
                kp += UP_GR;
                if kp > KP_MAX {
                    kp = KP_MAX;
                }
                k = kp >> LS_GR;
            }

            if bs.remaining() < k as usize {
                break;
            }
            let mask = (1u32 << k) - 1;
            run += ((bs.accumulator() >> (32 - k)) & mask) as usize;
            bs.shift(k as usize);

            if bs.remaining() < 1 {
                break;
            }
            let sign = if bs.accumulator() & 0x8000_0000 != 0 { 1 } else { 0 };
            bs.shift(1);

            // count leading 1s
            let mut cnt = lzcnt_s(!bs.accumulator()) as usize;
            let mut nbits = bs.remaining();
            if cnt > nbits {
                cnt = nbits;
            }
            let mut vk = cnt as u32;
            while cnt == 32 && bs.remaining() > 0 {
                bs.shift(32);
                cnt = lzcnt_s(!bs.accumulator()) as usize;
                nbits = bs.remaining();
                if cnt > nbits {
                    cnt = nbits;
                }
                vk += cnt as u32;
            }
            bs.shift((vk % 32) as usize);

            if bs.remaining() < 1 {
                break;
            }
            bs.shift(1);

            if bs.remaining() < kr as usize {
                break;
            }
            let mut code: u16 = if kr > 0 {
                let mask = (1u32 << kr) - 1;
                ((bs.accumulator() >> (32 - kr)) & mask) as u16
            } else {
                0
            };
            bs.shift(kr as usize);

            code |= ((vk << kr) & 0xFFFF) as u16;

            if vk == 0 {
                if krp > 2 {
                    krp -= 2;
                } else {
                    krp = 0;
                }
                kr = krp >> LS_GR;
            } else if vk != 1 {
                krp += vk;
                if krp > KP_MAX {
                    krp = KP_MAX;
                }
                kr = krp >> LS_GR;
            }

            if kp > DN_GR {
                kp -= DN_GR;
            } else {
                kp = 0;
            }
            k = kp >> LS_GR;

            let mag: i32 = if sign != 0 {
                -(i32::from(code) + 1)
            } else {
                i32::from(code) + 1
            };

            let mut size = run;
            if out_i + size > dst_size {
                size = dst_size - out_i;
            }
            if size != 0 {
                for v in &mut dst[out_i..out_i + size] {
                    *v = 0;
                }
                out_i += size;
            }
            if out_i < dst_size {
                dst[out_i] = mag as i16;
                out_i += 1;
            }
        } else {
            // ---- Golomb-Rice mode ----
            let mut cnt = lzcnt_s(!bs.accumulator()) as usize;
            let mut nbits = bs.remaining();
            if cnt > nbits {
                cnt = nbits;
            }
            let mut vk = cnt as u32;
            while cnt == 32 && bs.remaining() > 0 {
                bs.shift(32);
                cnt = lzcnt_s(!bs.accumulator()) as usize;
                nbits = bs.remaining();
                if cnt > nbits {
                    cnt = nbits;
                }
                vk += cnt as u32;
            }
            bs.shift((vk % 32) as usize);

            if bs.remaining() < 1 {
                break;
            }
            bs.shift(1);

            if bs.remaining() < kr as usize {
                break;
            }
            let mut code: u16 = if kr > 0 {
                let mask = (1u32 << kr) - 1;
                ((bs.accumulator() >> (32 - kr)) & mask) as u16
            } else {
                0
            };
            bs.shift(kr as usize);

            code |= ((vk << kr) & 0xFFFF) as u16;

            if vk == 0 {
                if krp > 2 {
                    krp -= 2;
                } else {
                    krp = 0;
                }
                kr = krp >> LS_GR;
            } else if vk != 1 {
                krp += vk;
                if krp > KP_MAX {
                    krp = KP_MAX;
                }
                kr = krp >> LS_GR;
            }

            match mode {
                EntropyAlgorithm::Rlgr1 => {
                    let mag: i32;
                    if code == 0 {
                        kp += UQ_GR;
                        if kp > KP_MAX {
                            kp = KP_MAX;
                        }
                        k = kp >> LS_GR;
                        mag = 0;
                    } else {
                        if kp > DQ_GR {
                            kp -= DQ_GR;
                        } else {
                            kp = 0;
                        }
                        k = kp >> LS_GR;
                        if code & 1 != 0 {
                            mag = -((i32::from(code) + 1) >> 1);
                        } else {
                            mag = i32::from(code) >> 1;
                        }
                    }
                    if out_i < dst_size {
                        dst[out_i] = mag as i16;
                        out_i += 1;
                    }
                }
                EntropyAlgorithm::Rlgr3 => {
                    let mut n_idx: u32 = 0;
                    if code != 0 {
                        n_idx = 32 - lzcnt_s(u32::from(code));
                    }
                    if (bs.remaining() as u32) < n_idx {
                        break;
                    }
                    let val1: u32 = if n_idx > 0 {
                        let mask = (1u32 << n_idx) - 1;
                        (bs.accumulator() >> (32 - n_idx)) & mask
                    } else {
                        0
                    };
                    bs.shift(n_idx as usize);
                    let val2 = u32::from(code).wrapping_sub(val1);

                    if val1 != 0 && val2 != 0 {
                        if kp > 2 * DQ_GR {
                            kp -= 2 * DQ_GR;
                        } else {
                            kp = 0;
                        }
                        k = kp >> LS_GR;
                    } else if val1 == 0 && val2 == 0 {
                        kp += 2 * UQ_GR;
                        if kp > KP_MAX {
                            kp = KP_MAX;
                        }
                        k = kp >> LS_GR;
                    }

                    let mag: i32 = if val1 & 1 != 0 {
                        -(((val1 + 1) >> 1) as i32)
                    } else {
                        (val1 >> 1) as i32
                    };
                    if out_i < dst_size {
                        dst[out_i] = mag as i16;
                        out_i += 1;
                    }
                    let mag: i32 = if val2 & 1 != 0 {
                        -(((val2 + 1) >> 1) as i32)
                    } else {
                        (val2 >> 1) as i32
                    };
                    if out_i < dst_size {
                        dst[out_i] = mag as i16;
                        out_i += 1;
                    }
                }
            }
        }
    }

    if out_i < dst_size {
        for v in &mut dst[out_i..] {
            *v = 0;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn main() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);

    // ---- Test A: random raw byte streams, both decoders, RLGR1 ----
    let mut mismatches_raw = 0usize;
    let mut ours_err_ref_ok = 0usize;
    let mut first_raw: Option<(Vec<u8>, usize, i16, i16)> = None;
    for _ in 0..20000 {
        let len = 1 + rng.below(400) as usize;
        let mut data = vec![0u8; len];
        // mix of densities so we exercise long zero runs and long one runs
        let mode = rng.below(4);
        for b in data.iter_mut() {
            *b = match mode {
                0 => (rng.next() & 0xFF) as u8,
                1 => {
                    if rng.below(8) == 0 {
                        (rng.next() & 0xFF) as u8
                    } else {
                        0
                    }
                }
                2 => {
                    if rng.below(8) == 0 {
                        (rng.next() & 0xFF) as u8
                    } else {
                        0xFF
                    }
                }
                _ => {
                    if rng.below(2) == 0 { 0x00 } else { 0xFF }
                }
            };
        }
        let mut a = vec![0i16; 4096];
        let mut b = vec![0i16; 4096];
        let ra = decode(EntropyAlgorithm::Rlgr1, &data, &mut a);
        let rb = decode_ref(EntropyAlgorithm::Rlgr1, &data, &mut b);
        match (ra, rb) {
            (Ok(()), Ok(())) => {
                if a != b {
                    mismatches_raw += 1;
                    if first_raw.is_none() {
                        let idx = a.iter().zip(b.iter()).position(|(x, y)| x != y).unwrap();
                        first_raw = Some((data.clone(), idx, a[idx], b[idx]));
                    }
                }
            }
            (Err(_), Ok(())) => {
                ours_err_ref_ok += 1;
            }
            _ => {}
        }
    }
    println!(
        "Test A (random raw bytes, RLGR1): mismatches = {}, ours-Err-while-ref-Ok = {}",
        mismatches_raw, ours_err_ref_ok
    );
    if let Some((d, idx, x, y)) = &first_raw {
        println!("  first mismatch at coeff {}: ours={} ref={}  input(len {})={:02x?}", idx, x, y, d.len(), &d[..d.len().min(48)]);
    }

    // ---- Test B: round-trip through our encoder, decoded by both ----
    let mut mismatch_rt = 0usize;
    let mut ref_wrong = 0usize;
    let mut ours_wrong = 0usize;
    let mut first_rt: Option<(usize, i16, i16, i16)> = None;
    for _ in 0..3000 {
        // sparse-ish coefficient array like a real DWT tile
        let mut coeffs = vec![0i16; 4096];
        let density = 1 + rng.below(64) as u32; // 1..64 percent
        for c in coeffs.iter_mut() {
            if (rng.below(100) as u32) < density {
                let m = 1 + rng.below(200) as i16;
                *c = if rng.below(2) == 0 { m } else { -m };
            }
        }
        let mut enc = vec![0u8; 65536];
        let n = match encode(EntropyAlgorithm::Rlgr1, &coeffs, &mut enc) {
            Ok(n) => n,
            Err(_) => continue,
        };
        let stream = &enc[..n];
        let mut a = vec![0i16; 4096];
        let mut b = vec![0i16; 4096];
        let ra = decode(EntropyAlgorithm::Rlgr1, stream, &mut a);
        let rb = decode_ref(EntropyAlgorithm::Rlgr1, stream, &mut b);
        if ra.is_err() || rb.is_err() {
            continue;
        }
        if a != b {
            mismatch_rt += 1;
            if first_rt.is_none() {
                let idx = a.iter().zip(b.iter()).position(|(x, y)| x != y).unwrap();
                first_rt = Some((idx, coeffs[idx], a[idx], b[idx]));
            }
        }
        if b != coeffs {
            ref_wrong += 1;
            let idx = b.iter().zip(coeffs.iter()).position(|(x, y)| x != y).unwrap();
            if ref_wrong <= 5 {
                println!(
                    "  ref!=orig: first differing coeff index {} (of 4096): orig={} ref={}",
                    idx, coeffs[idx], b[idx]
                );
            }
        }
        if a != coeffs {
            ours_wrong += 1;
        }
    }
    println!(
        "Test B (our encoder -> both decoders): ours!=ref = {}, ref!=orig = {}, ours!=orig = {}",
        mismatch_rt, ref_wrong, ours_wrong
    );
    if let Some((idx, orig, x, y)) = first_rt {
        println!("  first mismatch at coeff {}: orig={} ours={} ref={}", idx, orig, x, y);
    }

    // ---- Test C: truncated streams (output buffer larger than stream content) ----
    let mut mism_trunc = 0usize;
    for _ in 0..5000 {
        let mut coeffs = vec![0i16; 4096];
        for c in coeffs.iter_mut() {
            if rng.below(10) == 0 {
                let m = 1 + rng.below(50) as i16;
                *c = if rng.below(2) == 0 { m } else { -m };
            }
        }
        let mut enc = vec![0u8; 65536];
        let n = match encode(EntropyAlgorithm::Rlgr1, &coeffs, &mut enc) {
            Ok(n) => n,
            Err(_) => continue,
        };
        if n < 4 {
            continue;
        }
        let cut = 1 + rng.below(n as u64) as usize;
        let stream = &enc[..cut];
        let mut a = vec![0i16; 4096];
        let mut b = vec![0i16; 4096];
        let ra = decode(EntropyAlgorithm::Rlgr1, stream, &mut a);
        let rb = decode_ref(EntropyAlgorithm::Rlgr1, stream, &mut b);
        if ra.is_err() || rb.is_err() {
            continue;
        }
        if a != b {
            mism_trunc += 1;
        }
    }
    println!("Test C (truncated streams): mismatches = {}", mism_trunc);

    // ---- Test D: large magnitudes / long runs, stress k and kr extremes ----
    let mut mism_d = 0usize;
    let mut first_d: Option<(usize, i16, i16)> = None;
    for _ in 0..3000 {
        let mut coeffs = vec![0i16; 4096];
        // long zero runs then a big value
        let mut i = 0usize;
        while i < 4096 {
            let run = rng.below(600) as usize;
            i += run;
            if i >= 4096 {
                break;
            }
            let m = 1 + rng.below(8000) as i16;
            coeffs[i] = if rng.below(2) == 0 { m } else { -m };
            i += 1;
        }
        let mut enc = vec![0u8; 262144];
        let n = match encode(EntropyAlgorithm::Rlgr1, &coeffs, &mut enc) {
            Ok(n) => n,
            Err(_) => continue,
        };
        let stream = &enc[..n];
        let mut a = vec![0i16; 4096];
        let mut b = vec![0i16; 4096];
        let ra = decode(EntropyAlgorithm::Rlgr1, stream, &mut a);
        let rb = decode_ref(EntropyAlgorithm::Rlgr1, stream, &mut b);
        if ra.is_err() || rb.is_err() {
            continue;
        }
        if a != b {
            mism_d += 1;
            if first_d.is_none() {
                let idx = a.iter().zip(b.iter()).position(|(x, y)| x != y).unwrap();
                first_d = Some((idx, a[idx], b[idx]));
            }
        }
    }
    println!("Test D (long runs / big magnitudes): mismatches = {}", mism_d);
    if let Some((idx, x, y)) = first_d {
        println!("  first mismatch at coeff {}: ours={} ref={}", idx, x, y);
    }
}
