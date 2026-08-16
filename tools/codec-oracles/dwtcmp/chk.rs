fn t(value: i32) -> i16 {
    i16::try_from(value).unwrap_or(if value < 0 { i16::MIN } else { i16::MAX })
}
fn c(v: i32) -> i16 { if v < i16::MIN as i32 { i16::MIN } else if v > i16::MAX as i32 { i16::MAX } else { v as i16 } }
fn main() {
    for v in [-2147483648i32, -100000, -32769, -32768, -1, 0, 1, 32767, 32768, 100000, 2147483647] {
        assert_eq!(t(v), c(v), "{v}");
    }
    let mut s: u32 = 7;
    for _ in 0..2_000_000 { s = s.wrapping_mul(1103515245).wrapping_add(12345); let v = s as i32; assert_eq!(t(v), c(v), "{v}"); }
    println!("t() == clampi16() semantics: OK");
}
