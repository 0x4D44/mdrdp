//! The `MSG_RECTS` payload — raw BGRA dirty rectangles, encoded, decoded and blitted.
//!
//! Increment 1 of the native transport sends small screen changes as uncompressed
//! pixels instead of pushing them through the HEVC encoder, which removes an
//! encode plus a decode from the typing-class path. This module is the portable
//! half of that: the macOS viewer links it to parse and paint, the Windows server
//! links it to emit. Nothing here touches a platform API.
//!
//! Payload layout, all little-endian, per §5 "Wire format" of `wrk_docs/2026.08.17 -
//! HLD - native low-latency transport.md`:
//!
//! ```text
//! u64 frame_seq
//! u32 frame_width, u32 frame_height
//! u16 rect_count
//! rect_count ×:
//!   u16 x, u16 y, u16 w, u16 h
//!   u8  encoding            # 0 = raw BGRA8, top-down, tightly packed
//!   u32 pixel_bytes
//!   pixel_bytes bytes
//! ```
//!
//! The framing layer ([`crate::framing`]) owns the length prefix and the message
//! type byte (`MSG_RECTS`); everything above is payload only.
//!
//! **Validation is a safety requirement, not hygiene.** Every length in this payload
//! arrives from the network and every one of them ends up choosing an offset into a
//! fixed-size framebuffer. [`decode`] therefore validates the whole message before
//! returning anything a caller could blit, and [`blit_bgra_to_rgba`] re-checks
//! against the canvas it was actually handed — the canvas can legitimately differ
//! from the update's declared frame size (a mode change in flight), so one check
//! does not cover the other. A malformed message is terminal for the connection,
//! exactly like a framing error.

/// One batch of dirty rectangles, all belonging to the same captured frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RectUpdate {
    pub frame_seq: u64,
    /// Desktop size these rects were captured against, as in the stats header.
    pub frame_width: u32,
    pub frame_height: u32,
    pub rects: Vec<Rect>,
}

/// One dirty rectangle and its pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    /// BGRA8, top-down, tightly packed: exactly `w * h * 4` bytes, `w * 4` per row.
    pub pixels: Vec<u8>,
}

/// A screen-to-screen copy. Every source is read from the canvas as it stood
/// before any move in the batch was applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoveRect {
    pub src_x: u16,
    pub src_y: u16,
    pub dst_x: u16,
    pub dst_y: u16,
    pub w: u16,
    pub h: u16,
}

/// Raw BGRA8, top-down, tightly packed. The only encoding v1 emits or accepts.
///
/// The byte exists so a compressed encoding (LZ4 = 1) can slot in later without a
/// format break; an unknown value is rejected rather than guessed at.
pub const ENCODING_RAW_BGRA: u8 = 0;

/// Fixed-size prefix: `frame_seq`, `frame_width`, `frame_height`, `rect_count`.
const UPDATE_HEADER_LEN: usize = 8 + 4 + 4 + 2;
/// Per-rect prefix ahead of the pixels: `x`, `y`, `w`, `h`, `encoding`, `pixel_bytes`.
const RECT_HEADER_LEN: usize = 2 + 2 + 2 + 2 + 1 + 4;

/// Bytes per pixel in both the wire format (BGRA) and the canvas (RGBA).
const BPP: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RectsError {
    /// The payload ended in the middle of a field or a pixel run.
    Truncated {
        field: &'static str,
        needed: usize,
        available: usize,
    },
    /// `encoding` named a scheme this build does not implement.
    UnknownEncoding { rect_index: usize, encoding: u8 },
    /// `pixel_bytes` disagreed with `w * h * 4`, so one of the two is a lie.
    PixelBytesMismatch {
        rect_index: usize,
        declared: u32,
        expected: u64,
    },
    /// A rect with no area carries no pixels and can only be a corrupt length.
    ZeroArea { rect_index: usize, w: u16, h: u16 },
    /// The rect does not fit inside the frame it claims to belong to.
    OutOfFrame {
        rect_index: usize,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        frame_width: u32,
        frame_height: u32,
    },
    /// Bytes remained after the declared number of rects had been read.
    TrailingBytes { extra: usize },
    /// The rect does not fit inside the canvas it was asked to paint into.
    OutOfCanvas {
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        canvas_width: u32,
        canvas_height: u32,
    },
    /// The rect's pixel buffer does not match its own dimensions.
    RectPixelCount { expected: usize, actual: usize },
    /// The canvas slice is shorter than the dimensions it was described with.
    CanvasTooSmall { needed: usize, actual: usize },
    /// Canvas dimensions overflow the addressable byte count.
    CanvasGeometryOverflow,
    /// A move with no area cannot advance the displayed baseline.
    MoveZeroArea { move_index: usize },
    /// A move's source or destination does not fit in the canvas.
    MoveOutOfCanvas { move_index: usize },
    /// Staging more than one canvas of source pixels is refused.
    MoveAreaLimit { pixels: u64, limit: u64 },
    /// Raw remainder pixels are cumulatively bounded to one canvas.
    RawAreaLimit { pixels: u64, limit: u64 },
}

impl std::fmt::Display for RectsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RectsError::Truncated {
                field,
                needed,
                available,
            } => write!(
                f,
                "rects: truncated at {field}: needed {needed} bytes, {available} remain"
            ),
            RectsError::UnknownEncoding {
                rect_index,
                encoding,
            } => write!(f, "rects: rect {rect_index} uses unknown encoding {encoding}"),
            RectsError::PixelBytesMismatch {
                rect_index,
                declared,
                expected,
            } => write!(
                f,
                "rects: rect {rect_index} declares {declared} pixel bytes, dimensions require {expected}"
            ),
            RectsError::ZeroArea { rect_index, w, h } => {
                write!(f, "rects: rect {rect_index} has zero area ({w}x{h})")
            }
            RectsError::OutOfFrame {
                rect_index,
                x,
                y,
                w,
                h,
                frame_width,
                frame_height,
            } => write!(
                f,
                "rects: rect {rect_index} at {x},{y} {w}x{h} falls outside the {frame_width}x{frame_height} frame"
            ),
            RectsError::TrailingBytes { extra } => {
                write!(f, "rects: {extra} trailing bytes after the last rect")
            }
            RectsError::OutOfCanvas {
                x,
                y,
                w,
                h,
                canvas_width,
                canvas_height,
            } => write!(
                f,
                "rects: rect at {x},{y} {w}x{h} falls outside the {canvas_width}x{canvas_height} canvas"
            ),
            RectsError::RectPixelCount { expected, actual } => write!(
                f,
                "rects: rect dimensions require {expected} pixel bytes, buffer holds {actual}"
            ),
            RectsError::CanvasTooSmall { needed, actual } => write!(
                f,
                "rects: canvas of {actual} bytes is short of the {needed} its dimensions require"
            ),
            RectsError::CanvasGeometryOverflow => {
                write!(f, "rects: canvas geometry overflows its byte count")
            }
            RectsError::MoveZeroArea { move_index } => {
                write!(f, "rects: move {move_index} has zero area")
            }
            RectsError::MoveOutOfCanvas { move_index } => {
                write!(f, "rects: move {move_index} falls outside the canvas")
            }
            RectsError::MoveAreaLimit { pixels, limit } => write!(
                f,
                "rects: moves stage {pixels} pixels, exceeding the {limit}-pixel canvas"
            ),
            RectsError::RawAreaLimit { pixels, limit } => write!(
                f,
                "rects: raw remainder holds {pixels} pixels, exceeding the {limit}-pixel canvas"
            ),
        }
    }
}

impl std::error::Error for RectsError {}

/// Bytes [`encode`] will append for `update`.
pub fn encoded_len(update: &RectUpdate) -> usize {
    UPDATE_HEADER_LEN
        + update
            .rects
            .iter()
            .map(|r| RECT_HEADER_LEN + r.pixels.len())
            .sum::<usize>()
}

/// Append the `MSG_RECTS` payload for `update` to `out`.
///
/// The two asserts are sender-side invariants, not network validation: both
/// describe a rect list our own capture path built wrong, and encoding it anyway
/// would put a payload on the wire that our own [`decode`] rejects. Callers coalesce
/// dirty regions well below 65535 rects; nothing in the capture path approaches it.
pub fn encode(update: &RectUpdate, out: &mut Vec<u8>) {
    assert!(
        update.rects.len() <= u16::MAX as usize,
        "rects: {} rects exceeds the u16 rect_count field",
        update.rects.len()
    );
    out.reserve(encoded_len(update));
    out.extend_from_slice(&update.frame_seq.to_le_bytes());
    out.extend_from_slice(&update.frame_width.to_le_bytes());
    out.extend_from_slice(&update.frame_height.to_le_bytes());
    out.extend_from_slice(&(update.rects.len() as u16).to_le_bytes());
    for (i, r) in update.rects.iter().enumerate() {
        let expected = r.w as usize * r.h as usize * BPP;
        assert_eq!(
            r.pixels.len(),
            expected,
            "rects: rect {i} is {}x{} but carries {} pixel bytes",
            r.w,
            r.h,
            r.pixels.len()
        );
        out.extend_from_slice(&r.x.to_le_bytes());
        out.extend_from_slice(&r.y.to_le_bytes());
        out.extend_from_slice(&r.w.to_le_bytes());
        out.extend_from_slice(&r.h.to_le_bytes());
        out.push(ENCODING_RAW_BGRA);
        out.extend_from_slice(&(r.pixels.len() as u32).to_le_bytes());
        out.extend_from_slice(&r.pixels);
    }
}

/// A bounds-checked forward reader. Every read either yields the bytes or reports
/// exactly how far short the payload fell, so no slice in this module can panic.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn take(&mut self, n: usize, field: &'static str) -> Result<&'a [u8], RectsError> {
        if self.remaining() < n {
            return Err(RectsError::Truncated {
                field,
                needed: n,
                available: self.remaining(),
            });
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn u8(&mut self, field: &'static str) -> Result<u8, RectsError> {
        Ok(self.take(1, field)?[0])
    }

    fn u16(&mut self, field: &'static str) -> Result<u16, RectsError> {
        let b = self.take(2, field)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self, field: &'static str) -> Result<u32, RectsError> {
        let b = self.take(4, field)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self, field: &'static str) -> Result<u64, RectsError> {
        let b = self.take(8, field)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
}

/// Parse and fully validate a `MSG_RECTS` payload.
///
/// Nothing usable is returned until the *whole* payload has checked out: an update
/// whose fifth rect is out of bounds yields an error, not four paintable rects and a
/// surprise. Any error is terminal for the connection.
pub fn decode(payload: &[u8]) -> Result<RectUpdate, RectsError> {
    let mut r = Reader::new(payload);
    let frame_seq = r.u64("frame_seq")?;
    let frame_width = r.u32("frame_width")?;
    let frame_height = r.u32("frame_height")?;
    let rect_count = r.u16("rect_count")? as usize;

    let mut rects = Vec::with_capacity(rect_count.min(1024));
    for rect_index in 0..rect_count {
        let x = r.u16("rect.x")?;
        let y = r.u16("rect.y")?;
        let w = r.u16("rect.w")?;
        let h = r.u16("rect.h")?;
        let encoding = r.u8("rect.encoding")?;
        let pixel_bytes = r.u32("rect.pixel_bytes")?;

        if encoding != ENCODING_RAW_BGRA {
            return Err(RectsError::UnknownEncoding {
                rect_index,
                encoding,
            });
        }
        // Before the size check: a 0-wide rect needs 0 bytes, so a zero-area rect
        // would otherwise sail through as consistent.
        if w == 0 || h == 0 {
            return Err(RectsError::ZeroArea { rect_index, w, h });
        }
        // Widen first. Both products overflow u16, and `w * h * 4` overflows u32 at
        // the top of the u16 range, so u64 is the narrowest type that cannot wrap.
        let expected = w as u64 * h as u64 * BPP as u64;
        if pixel_bytes as u64 != expected {
            return Err(RectsError::PixelBytesMismatch {
                rect_index,
                declared: pixel_bytes,
                expected,
            });
        }
        // x + w is a u17 in the worst case (65535 + 65535), so this must widen too —
        // in u16 it wraps to a small number and every bound looks satisfied.
        if x as u64 + w as u64 > frame_width as u64 || y as u64 + h as u64 > frame_height as u64 {
            return Err(RectsError::OutOfFrame {
                rect_index,
                x,
                y,
                w,
                h,
                frame_width,
                frame_height,
            });
        }

        let pixels = r.take(pixel_bytes as usize, "rect.pixels")?.to_vec();
        rects.push(Rect { x, y, w, h, pixels });
    }

    if r.remaining() != 0 {
        return Err(RectsError::TrailingBytes {
            extra: r.remaining(),
        });
    }

    Ok(RectUpdate {
        frame_seq,
        frame_width,
        frame_height,
        rects,
    })
}

/// Swizzle `rect` from BGRA to RGBA and copy it into an RGBA8 canvas.
///
/// `canvas` is tightly packed and top-down: `canvas_width * 4` bytes per row. Alpha
/// is written as `0xFF` — the capture path produces opaque pixels and the decoder
/// path the canvas is shared with does the same, so an alpha carried from the wire
/// would only be a way for a corrupt frame to make the desktop transparent.
///
/// Bounds are re-checked here against the *canvas*, deliberately duplicating
/// [`decode`]'s check against the declared frame size. The two dimensions can
/// disagree — the caller compares them and counts the mismatch — and this function
/// must never write outside the canvas whatever the caller did about that.
pub fn blit_bgra_to_rgba(
    rect: &Rect,
    canvas: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
) -> Result<(), RectsError> {
    validate_canvas(canvas, canvas_width, canvas_height)?;
    validate_rect_for_canvas(rect, canvas_width, canvas_height)?;
    blit_validated(rect, canvas, canvas_width);
    Ok(())
}

/// Apply one ordered move batch atomically, then paint its raw final pixels.
///
/// Validation and source staging finish before the first canvas write. This makes
/// overlapping copies deterministic and leaves the canvas unchanged on refusal.
pub fn apply_move_update(
    moves: &[MoveRect],
    raw: &[Rect],
    canvas: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
) -> Result<(), RectsError> {
    validate_canvas(canvas, canvas_width, canvas_height)?;
    let canvas_pixels = u64::from(canvas_width) * u64::from(canvas_height);
    let mut move_pixels = 0u64;
    for (move_index, movement) in moves.iter().enumerate() {
        if movement.w == 0 || movement.h == 0 {
            return Err(RectsError::MoveZeroArea { move_index });
        }
        let source_fits = u64::from(movement.src_x) + u64::from(movement.w)
            <= u64::from(canvas_width)
            && u64::from(movement.src_y) + u64::from(movement.h) <= u64::from(canvas_height);
        let destination_fits = u64::from(movement.dst_x) + u64::from(movement.w)
            <= u64::from(canvas_width)
            && u64::from(movement.dst_y) + u64::from(movement.h) <= u64::from(canvas_height);
        if !source_fits || !destination_fits {
            return Err(RectsError::MoveOutOfCanvas { move_index });
        }
        move_pixels = move_pixels.saturating_add(u64::from(movement.w) * u64::from(movement.h));
        if move_pixels > canvas_pixels {
            return Err(RectsError::MoveAreaLimit {
                pixels: move_pixels,
                limit: canvas_pixels,
            });
        }
    }

    let mut raw_pixels = 0u64;
    for (rect_index, rect) in raw.iter().enumerate() {
        if rect.w == 0 || rect.h == 0 {
            return Err(RectsError::ZeroArea {
                rect_index,
                w: rect.w,
                h: rect.h,
            });
        }
        validate_rect_for_canvas(rect, canvas_width, canvas_height)?;
        raw_pixels = raw_pixels.saturating_add(u64::from(rect.w) * u64::from(rect.h));
        if raw_pixels > canvas_pixels {
            return Err(RectsError::RawAreaLimit {
                pixels: raw_pixels,
                limit: canvas_pixels,
            });
        }
    }

    let canvas_stride = canvas_width as usize * BPP;
    let mut staged = Vec::with_capacity(move_pixels as usize * BPP);
    for movement in moves {
        let row_bytes = movement.w as usize * BPP;
        let source_x = movement.src_x as usize * BPP;
        for row in 0..movement.h as usize {
            let start = (movement.src_y as usize + row) * canvas_stride + source_x;
            staged.extend_from_slice(&canvas[start..start + row_bytes]);
        }
    }

    let mut staged_offset = 0;
    for movement in moves {
        let row_bytes = movement.w as usize * BPP;
        let destination_x = movement.dst_x as usize * BPP;
        for row in 0..movement.h as usize {
            let destination = (movement.dst_y as usize + row) * canvas_stride + destination_x;
            canvas[destination..destination + row_bytes]
                .copy_from_slice(&staged[staged_offset..staged_offset + row_bytes]);
            staged_offset += row_bytes;
        }
    }
    for rect in raw {
        blit_validated(rect, canvas, canvas_width);
    }
    Ok(())
}

fn validate_canvas(canvas: &[u8], canvas_width: u32, canvas_height: u32) -> Result<(), RectsError> {
    let needed = (canvas_width as usize)
        .checked_mul(canvas_height as usize)
        .and_then(|pixels| pixels.checked_mul(BPP))
        .ok_or(RectsError::CanvasGeometryOverflow)?;
    if canvas.len() < needed {
        return Err(RectsError::CanvasTooSmall {
            needed,
            actual: canvas.len(),
        });
    }
    Ok(())
}

fn validate_rect_for_canvas(
    rect: &Rect,
    canvas_width: u32,
    canvas_height: u32,
) -> Result<(), RectsError> {
    let expected = rect.w as usize * rect.h as usize * BPP;
    if rect.pixels.len() != expected {
        return Err(RectsError::RectPixelCount {
            expected,
            actual: rect.pixels.len(),
        });
    }
    // Widened, for the same reason as in `decode`.
    if rect.x as u64 + rect.w as u64 > canvas_width as u64
        || rect.y as u64 + rect.h as u64 > canvas_height as u64
    {
        return Err(RectsError::OutOfCanvas {
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: rect.h,
            canvas_width,
            canvas_height,
        });
    }

    Ok(())
}

fn blit_validated(rect: &Rect, canvas: &mut [u8], canvas_width: u32) {
    let canvas_stride = canvas_width as usize * BPP;
    let row_bytes = rect.w as usize * BPP;
    let x_off = rect.x as usize * BPP;
    for row in 0..rect.h as usize {
        let src_start = row * row_bytes;
        let src = &rect.pixels[src_start..src_start + row_bytes];
        let dst_start = (rect.y as usize + row) * canvas_stride + x_off;
        let dst = &mut canvas[dst_start..dst_start + row_bytes];
        for (s, d) in src.chunks_exact(BPP).zip(dst.chunks_exact_mut(BPP)) {
            d[0] = s[2]; // R <- B
            d[1] = s[1]; // G
            d[2] = s[0]; // B <- R
            d[3] = 0xFF;
        }
    }
}

/// Exact pixel area covered by a set of rectangles, after clipping to the frame.
///
/// Overlap is counted once. This is intentionally based on rectangle geometry,
/// not `sum(w * h)`: duplication metadata can overlap, and that sum would inflate
/// the changed-area evidence used by the HEVC live gate.
pub fn union_area(rectangles: &[(u32, u32, u32, u32)], width: u32, height: u32) -> u64 {
    let clipped: Vec<_> = rectangles
        .iter()
        .filter_map(|&(x, y, w, h)| {
            let left = x.min(width);
            let top = y.min(height);
            let right = x.saturating_add(w).min(width);
            let bottom = y.saturating_add(h).min(height);
            (left < right && top < bottom).then_some((left, top, right, bottom))
        })
        .collect();
    let mut edges: Vec<u32> = clipped
        .iter()
        .flat_map(|&(_, top, _, bottom)| [top, bottom])
        .collect();
    edges.sort_unstable();
    edges.dedup();

    edges
        .windows(2)
        .map(|band| {
            let (top, bottom) = (band[0], band[1]);
            let mut spans: Vec<(u32, u32)> = clipped
                .iter()
                .filter(|&&(_, rect_top, _, rect_bottom)| rect_top < bottom && rect_bottom > top)
                .map(|&(left, _, right, _)| (left, right))
                .collect();
            spans.sort_unstable();
            let mut covered = 0u64;
            let mut merged: Option<(u32, u32)> = None;
            for (left, right) in spans {
                match merged {
                    Some((start, end)) if left <= end => merged = Some((start, end.max(right))),
                    Some((start, end)) => {
                        covered += u64::from(end - start);
                        merged = Some((left, right));
                    }
                    None => merged = Some((left, right)),
                }
            }
            if let Some((start, end)) = merged {
                covered += u64::from(end - start);
            }
            covered * u64::from(bottom - top)
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BGRA pixels whose every byte differs from every other, so a swapped channel,
    /// a transposed row or an off-by-one offset all show up as a wrong value rather
    /// than an accidental match.
    fn distinct_pixels(w: u16, h: u16, seed: u8) -> Vec<u8> {
        (0..w as usize * h as usize * BPP)
            .map(|i| seed.wrapping_add(i as u8).wrapping_mul(7).wrapping_add(3))
            .collect()
    }

    fn rect(x: u16, y: u16, w: u16, h: u16, seed: u8) -> Rect {
        Rect {
            x,
            y,
            w,
            h,
            pixels: distinct_pixels(w, h, seed),
        }
    }

    /// A valid two-rect update. Every field differs from every other field it could
    /// be confused with: the two rects share no coordinate, no dimension and no
    /// pixel value.
    fn sample_update() -> RectUpdate {
        RectUpdate {
            frame_seq: 0x0102_0304_0506_0708,
            frame_width: 1920,
            frame_height: 1080,
            rects: vec![rect(10, 20, 3, 2, 0x40), rect(100, 200, 5, 7, 0x90)],
        }
    }

    fn wire(update: &RectUpdate) -> Vec<u8> {
        let mut v = Vec::new();
        encode(update, &mut v);
        v
    }

    #[test]
    fn a_two_rect_update_survives_the_roundtrip() {
        let update = sample_update();
        let bytes = wire(&update);
        assert_eq!(bytes.len(), encoded_len(&update), "encoded_len agrees");
        let back = decode(&bytes).expect("valid payload");
        assert_eq!(back, update);
    }

    #[test]
    fn the_header_fields_land_at_their_declared_offsets() {
        let update = sample_update();
        let bytes = wire(&update);
        assert_eq!(&bytes[0..8], &update.frame_seq.to_le_bytes());
        assert_eq!(&bytes[8..12], &1920u32.to_le_bytes());
        assert_eq!(&bytes[12..16], &1080u32.to_le_bytes());
        assert_eq!(&bytes[16..18], &2u16.to_le_bytes());
        // First rect header: 10, 20, 3x2, raw, 24 bytes.
        assert_eq!(&bytes[18..20], &10u16.to_le_bytes());
        assert_eq!(&bytes[20..22], &20u16.to_le_bytes());
        assert_eq!(&bytes[22..24], &3u16.to_le_bytes());
        assert_eq!(&bytes[24..26], &2u16.to_le_bytes());
        assert_eq!(bytes[26], ENCODING_RAW_BGRA);
        assert_eq!(&bytes[27..31], &24u32.to_le_bytes());
    }

    #[test]
    fn an_update_with_no_rects_is_legal_and_is_just_the_header() {
        let update = RectUpdate {
            frame_seq: 42,
            frame_width: 800,
            frame_height: 600,
            rects: Vec::new(),
        };
        let bytes = wire(&update);
        assert_eq!(bytes.len(), UPDATE_HEADER_LEN);
        assert_eq!(decode(&bytes).unwrap(), update);
    }

    #[test]
    fn every_truncation_point_errors_instead_of_panicking() {
        let full = wire(&sample_update());
        for cut in 0..full.len() {
            let err = decode(&full[..cut]);
            assert!(
                matches!(err, Err(RectsError::Truncated { .. })),
                "prefix of {cut} bytes should be Truncated, got {err:?}"
            );
        }
        // The sanity half of the loop: the uncut payload must still parse, or the
        // assertion above would pass for a decoder that rejects everything.
        assert!(decode(&full).is_ok());
    }

    #[test]
    fn an_unknown_encoding_byte_is_refused() {
        let mut bytes = wire(&sample_update());
        bytes[26] = 1; // LZ4's reserved value: reserved is not implemented.
        assert_eq!(
            decode(&bytes),
            Err(RectsError::UnknownEncoding {
                rect_index: 0,
                encoding: 1
            })
        );
    }

    #[test]
    fn pixel_bytes_that_disagree_with_the_dimensions_are_refused() {
        let mut bytes = wire(&sample_update());
        // Second rect is 5x7 = 140 bytes; claim 136 and the run would end mid-row.
        let second_header = UPDATE_HEADER_LEN + RECT_HEADER_LEN + 24;
        let field = second_header + 9;
        bytes[field..field + 4].copy_from_slice(&136u32.to_le_bytes());
        assert_eq!(
            decode(&bytes),
            Err(RectsError::PixelBytesMismatch {
                rect_index: 1,
                declared: 136,
                expected: 140
            })
        );
    }

    #[test]
    fn a_rect_hanging_off_the_frame_is_refused() {
        let mut bytes = wire(&sample_update());
        // First rect is 3 wide at x=10; move it to x=1918 and it overruns 1920.
        bytes[18..20].copy_from_slice(&1918u16.to_le_bytes());
        assert_eq!(
            decode(&bytes),
            Err(RectsError::OutOfFrame {
                rect_index: 0,
                x: 1918,
                y: 20,
                w: 3,
                h: 2,
                frame_width: 1920,
                frame_height: 1080
            })
        );
    }

    #[test]
    fn a_rect_whose_x_plus_w_overflows_u16_is_refused() {
        // 65535 + 2 wraps to 1 in u16 arithmetic, which is inside any frame. The
        // check has to widen or this payload paints wherever it likes.
        let update = RectUpdate {
            frame_seq: 9,
            frame_width: 65535,
            frame_height: 65535,
            rects: vec![rect(65535, 3, 2, 4, 0x11)],
        };
        let bytes = wire(&update);
        assert_eq!(
            decode(&bytes),
            Err(RectsError::OutOfFrame {
                rect_index: 0,
                x: 65535,
                y: 3,
                w: 2,
                h: 4,
                frame_width: 65535,
                frame_height: 65535
            })
        );
    }

    #[test]
    fn a_zero_area_rect_is_refused() {
        // Built by hand: `encode` asserts against this shape, so the wire is the
        // only place it can come from.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&7u64.to_le_bytes());
        bytes.extend_from_slice(&640u32.to_le_bytes());
        bytes.extend_from_slice(&480u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&5u16.to_le_bytes()); // x
        bytes.extend_from_slice(&6u16.to_le_bytes()); // y
        bytes.extend_from_slice(&0u16.to_le_bytes()); // w
        bytes.extend_from_slice(&9u16.to_le_bytes()); // h
        bytes.push(ENCODING_RAW_BGRA);
        bytes.extend_from_slice(&0u32.to_le_bytes()); // consistent with 0x9!
        assert_eq!(
            decode(&bytes),
            Err(RectsError::ZeroArea {
                rect_index: 0,
                w: 0,
                h: 9
            })
        );
    }

    #[test]
    fn bytes_after_the_last_rect_are_refused() {
        let mut bytes = wire(&sample_update());
        bytes.extend_from_slice(&[0xDE, 0xAD, 0xBE]);
        assert_eq!(decode(&bytes), Err(RectsError::TrailingBytes { extra: 3 }));
    }

    #[test]
    fn a_rect_count_larger_than_the_rects_present_is_truncated_not_silently_short() {
        let mut bytes = wire(&sample_update());
        bytes[16..18].copy_from_slice(&3u16.to_le_bytes());
        assert!(matches!(
            decode(&bytes),
            Err(RectsError::Truncated {
                field: "rect.x",
                ..
            })
        ));
    }

    /// 8x4 RGBA canvas prefilled with a value no blit can produce (alpha 0x11,
    /// which the blit always overwrites with 0xFF), so "untouched" is provable.
    fn canvas_8x4() -> Vec<u8> {
        vec![0x11u8; 8 * 4 * BPP]
    }

    #[test]
    fn a_blit_swizzles_bgra_to_rgba_at_the_right_offset() {
        let w = 3u16;
        let h = 2u16;
        let r = rect(2, 1, w, h, 0x40);
        let mut canvas = canvas_8x4();
        blit_bgra_to_rgba(&r, &mut canvas, 8, 4).expect("fits");

        let stride = 8 * BPP;
        for row in 0..h as usize {
            for col in 0..w as usize {
                let s = (row * w as usize + col) * BPP;
                let (b, g, red) = (r.pixels[s], r.pixels[s + 1], r.pixels[s + 2]);
                let d = (1 + row) * stride + (2 + col) * BPP;
                assert_eq!(canvas[d], red, "R at row {row} col {col}");
                assert_eq!(canvas[d + 1], g, "G at row {row} col {col}");
                assert_eq!(canvas[d + 2], b, "B at row {row} col {col}");
                assert_eq!(canvas[d + 3], 0xFF, "A at row {row} col {col}");
            }
        }
    }

    #[test]
    fn a_blit_leaves_every_pixel_outside_the_rect_alone() {
        let r = rect(2, 1, 3, 2, 0x40);
        let mut canvas = canvas_8x4();
        blit_bgra_to_rgba(&r, &mut canvas, 8, 4).expect("fits");
        for y in 0..4usize {
            for x in 0..8usize {
                let inside = (2..5).contains(&x) && (1..3).contains(&y);
                if inside {
                    continue;
                }
                let d = (y * 8 + x) * BPP;
                assert_eq!(
                    &canvas[d..d + BPP],
                    &[0x11, 0x11, 0x11, 0x11],
                    "pixel {x},{y} outside the rect was written"
                );
            }
        }
    }

    #[test]
    fn a_blit_past_the_canvas_edge_is_refused_and_writes_nothing() {
        let r = rect(6, 1, 3, 2, 0x40); // 6 + 3 = 9 > 8
        let mut canvas = canvas_8x4();
        let before = canvas.clone();
        assert_eq!(
            blit_bgra_to_rgba(&r, &mut canvas, 8, 4),
            Err(RectsError::OutOfCanvas {
                x: 6,
                y: 1,
                w: 3,
                h: 2,
                canvas_width: 8,
                canvas_height: 4
            })
        );
        assert_eq!(canvas, before, "canvas untouched after a refused blit");
    }

    #[test]
    fn a_blit_past_the_bottom_edge_is_refused() {
        let r = rect(0, 3, 2, 2, 0x55); // 3 + 2 = 5 > 4
        let mut canvas = canvas_8x4();
        assert!(matches!(
            blit_bgra_to_rgba(&r, &mut canvas, 8, 4),
            Err(RectsError::OutOfCanvas { .. })
        ));
    }

    #[test]
    fn a_blit_into_a_canvas_shorter_than_its_dimensions_is_refused() {
        let r = rect(0, 0, 2, 2, 0x77);
        let mut canvas = vec![0u8; 8 * 4 * BPP - 1];
        assert_eq!(
            blit_bgra_to_rgba(&r, &mut canvas, 8, 4),
            Err(RectsError::CanvasTooSmall {
                needed: 128,
                actual: 127
            })
        );
    }

    #[test]
    fn a_blit_of_a_rect_whose_buffer_mismatches_its_dimensions_is_refused() {
        let mut r = rect(0, 0, 3, 2, 0x40);
        r.pixels.pop();
        let mut canvas = canvas_8x4();
        assert_eq!(
            blit_bgra_to_rgba(&r, &mut canvas, 8, 4),
            Err(RectsError::RectPixelCount {
                expected: 24,
                actual: 23
            })
        );
    }

    #[test]
    fn a_decoded_update_paints_all_its_rects_into_a_canvas() {
        // End to end: the shape the viewer actually runs.
        let update = RectUpdate {
            frame_seq: 5,
            frame_width: 8,
            frame_height: 4,
            rects: vec![rect(0, 0, 2, 1, 0x20), rect(5, 2, 3, 2, 0xC0)],
        };
        let back = decode(&wire(&update)).unwrap();
        let mut canvas = canvas_8x4();
        for r in &back.rects {
            blit_bgra_to_rgba(r, &mut canvas, back.frame_width, back.frame_height).unwrap();
        }
        // Spot-check one pixel from each rect, at its own offset.
        let first = update.rects[0].pixels.clone();
        assert_eq!(&canvas[0..4], &[first[2], first[1], first[0], 0xFF]);
        let second = update.rects[1].pixels.clone();
        let d = (2 * 8 + 5) * BPP;
        assert_eq!(&canvas[d..d + 4], &[second[2], second[1], second[0], 0xFF]);
    }

    #[test]
    fn move_sources_are_staged_from_the_same_pre_update_canvas() {
        let mut canvas = vec![1, 0, 0, 0xFF, 2, 0, 0, 0xFF, 3, 0, 0, 0xFF, 4, 0, 0, 0xFF];
        let moves = [
            MoveRect {
                src_x: 0,
                src_y: 0,
                dst_x: 2,
                dst_y: 0,
                w: 2,
                h: 1,
            },
            MoveRect {
                src_x: 2,
                src_y: 0,
                dst_x: 0,
                dst_y: 0,
                w: 2,
                h: 1,
            },
        ];

        apply_move_update(&moves, &[], &mut canvas, 4, 1).unwrap();

        assert_eq!(
            canvas
                .chunks_exact(BPP)
                .map(|pixel| pixel[0])
                .collect::<Vec<_>>(),
            [3, 4, 1, 2]
        );
    }

    #[test]
    fn raw_pixels_are_applied_after_moves() {
        let mut canvas = vec![1, 0, 0, 0xFF, 2, 0, 0, 0xFF, 3, 0, 0, 0xFF, 4, 0, 0, 0xFF];
        let moves = [MoveRect {
            src_x: 0,
            src_y: 0,
            dst_x: 1,
            dst_y: 0,
            w: 3,
            h: 1,
        }];
        let raw = [Rect {
            x: 3,
            y: 0,
            w: 1,
            h: 1,
            pixels: vec![9, 8, 7, 6],
        }];

        apply_move_update(&moves, &raw, &mut canvas, 4, 1).unwrap();

        assert_eq!(
            canvas,
            [1, 0, 0, 0xFF, 1, 0, 0, 0xFF, 2, 0, 0, 0xFF, 7, 8, 9, 0xFF]
        );
    }

    #[test]
    fn invalid_move_batch_leaves_the_canvas_untouched() {
        let mut canvas = canvas_8x4();
        let before = canvas.clone();
        let moves = [MoveRect {
            src_x: 0,
            src_y: 0,
            dst_x: 7,
            dst_y: 0,
            w: 2,
            h: 1,
        }];

        assert!(matches!(
            apply_move_update(&moves, &[], &mut canvas, 8, 4),
            Err(RectsError::MoveOutOfCanvas { move_index: 0, .. })
        ));
        assert_eq!(canvas, before);
    }

    #[test]
    fn invalid_raw_remainder_leaves_moves_unapplied() {
        let mut canvas = canvas_8x4();
        let before = canvas.clone();
        let moves = [MoveRect {
            src_x: 0,
            src_y: 0,
            dst_x: 2,
            dst_y: 0,
            w: 2,
            h: 1,
        }];
        let raw = [Rect {
            x: 7,
            y: 0,
            w: 2,
            h: 1,
            pixels: vec![0; 8],
        }];

        assert!(matches!(
            apply_move_update(&moves, &raw, &mut canvas, 8, 4),
            Err(RectsError::OutOfCanvas { .. })
        ));
        assert_eq!(canvas, before);
    }

    #[test]
    fn cumulative_move_staging_is_bounded_by_one_canvas() {
        let mut canvas = canvas_8x4();
        let before = canvas.clone();
        let moves = [
            MoveRect {
                src_x: 0,
                src_y: 0,
                dst_x: 0,
                dst_y: 0,
                w: 8,
                h: 4,
            },
            MoveRect {
                src_x: 0,
                src_y: 0,
                dst_x: 0,
                dst_y: 0,
                w: 1,
                h: 1,
            },
        ];

        assert_eq!(
            apply_move_update(&moves, &[], &mut canvas, 8, 4),
            Err(RectsError::MoveAreaLimit {
                pixels: 33,
                limit: 32
            })
        );
        assert_eq!(canvas, before);
    }

    #[test]
    fn cumulative_raw_remainder_is_bounded_by_one_canvas() {
        let mut canvas = canvas_8x4();
        let before = canvas.clone();
        let raw = [rect(0, 0, 8, 4, 1), rect(0, 0, 1, 1, 2)];

        assert_eq!(
            apply_move_update(&[], &raw, &mut canvas, 8, 4),
            Err(RectsError::RawAreaLimit {
                pixels: 33,
                limit: 32
            })
        );
        assert_eq!(canvas, before);
    }

    #[test]
    fn union_area_counts_overlaps_once_and_clips_to_the_frame() {
        // The first two rectangles overlap by 2x2 pixels. The third extends two
        // pixels beyond the right edge and must be clipped before it is counted.
        let bounds = [(1, 1, 4, 3), (3, 2, 4, 3), (8, 0, 4, 2)];
        assert_eq!(union_area(&bounds, 10, 6), 24);
        assert_eq!(union_area(&[], 10, 6), 0);
    }

    #[test]
    fn the_error_display_names_the_rect_and_the_numbers() {
        let e = RectsError::OutOfFrame {
            rect_index: 4,
            x: 1918,
            y: 20,
            w: 3,
            h: 2,
            frame_width: 1920,
            frame_height: 1080,
        };
        let s = e.to_string();
        assert!(s.contains("rect 4"), "{s}");
        assert!(s.contains("1918"), "{s}");
        assert!(s.contains("1920x1080"), "{s}");
    }
}
