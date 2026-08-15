//! NSCodec decoder for ClearCodec subcodec regions (MS-RDPNSC).
//!
//! ClearCodec can embed NSCodec rectangles in its third compositing layer. Leaving that
//! branch empty reports a successful ClearCodec decode while preserving stale pixels over
//! exactly those rectangles. The plane layout, RLE and YCoCg reconstruction here follow
//! MS-RDPNSC and are differentially checked against FreeRDP's decoder by mdrdp.

use ironrdp_core::{DecodeResult, invalid_field_err};

const HEADER_LEN: usize = 20;
const PLANE_COUNT: usize = 4;

pub(super) fn decode(data: &[u8], width: u16, height: u16) -> DecodeResult<Vec<u8>> {
    if width == 0 || height == 0 {
        return Err(invalid_field_err!("nscodec", "zero-sized bitmap"));
    }
    if data.len() < HEADER_LEN {
        return Err(invalid_field_err!("nscodec", "header is shorter than 20 bytes"));
    }

    let mut plane_lengths = [0usize; PLANE_COUNT];
    for (index, length) in plane_lengths.iter_mut().enumerate() {
        let start = index * 4;
        let wire = u32::from_le_bytes(
            data[start..start + 4]
                .try_into()
                .expect("four-byte header field"),
        );
        *length = usize::try_from(wire)
            .map_err(|_| invalid_field_err!("nscodec", "plane length does not fit usize"))?;
    }

    let color_loss = data[16];
    if !(1..=7).contains(&color_loss) {
        return Err(invalid_field_err!("nscodec", "color-loss level is outside 1..=7"));
    }
    let subsampled = data[17] != 0;
    let encoded_len = plane_lengths.iter().try_fold(0usize, |total, length| {
        total
            .checked_add(*length)
            .ok_or_else(|| invalid_field_err!("nscodec", "plane lengths overflow"))
    })?;
    if data.len().saturating_sub(HEADER_LEN) < encoded_len {
        return Err(invalid_field_err!("nscodec", "plane data is truncated"));
    }

    let w = usize::from(width);
    let h = usize::from(height);
    let pixels = w
        .checked_mul(h)
        .ok_or_else(|| invalid_field_err!("nscodec", "pixel count overflows"))?;
    let (y_len, chroma_len) = if subsampled {
        let rounded_w = w
            .checked_add(7)
            .map(|value| value & !7)
            .ok_or_else(|| invalid_field_err!("nscodec", "rounded width overflows"))?;
        let rounded_h = h
            .checked_add(1)
            .map(|value| value & !1)
            .ok_or_else(|| invalid_field_err!("nscodec", "rounded height overflows"))?;
        let y_len = rounded_w
            .checked_mul(h)
            .ok_or_else(|| invalid_field_err!("nscodec", "luma plane size overflows"))?;
        let chroma_len = (rounded_w / 2)
            .checked_mul(rounded_h / 2)
            .ok_or_else(|| invalid_field_err!("nscodec", "chroma plane size overflows"))?;
        (y_len, chroma_len)
    } else {
        (pixels, pixels)
    };
    let original_lengths = [y_len, chroma_len, chroma_len, pixels];

    let mut cursor = HEADER_LEN;
    let mut planes = Vec::with_capacity(PLANE_COUNT);
    for index in 0..PLANE_COUNT {
        let end = cursor + plane_lengths[index];
        planes.push(decode_plane(&data[cursor..end], original_lengths[index])?);
        cursor = end;
    }

    let stride_y = if subsampled { y_len / h } else { w };
    let stride_chroma = if subsampled { stride_y / 2 } else { w };
    let shift = u32::from(color_loss - 1);
    let mut output = Vec::with_capacity(
        pixels
            .checked_mul(4)
            .ok_or_else(|| invalid_field_err!("nscodec", "output size overflows"))?,
    );

    for y in 0..h {
        for x in 0..w {
            let y_value = i16::from(planes[0][y * stride_y + x]);
            let chroma_index = if subsampled {
                (y / 2) * stride_chroma + x / 2
            } else {
                y * stride_chroma + x
            };
            let co = recover_chroma(planes[1][chroma_index], shift);
            let cg = recover_chroma(planes[2][chroma_index], shift);
            let alpha = planes[3][y * w + x];

            let red = clamp_channel(y_value + co - cg);
            let green = clamp_channel(y_value + cg);
            let blue = clamp_channel(y_value - co - cg);
            output.extend_from_slice(&[blue, green, red, alpha]);
        }
    }

    Ok(output)
}

fn decode_plane(encoded: &[u8], original_len: usize) -> DecodeResult<Vec<u8>> {
    if encoded.is_empty() {
        return Ok(vec![0xFF; original_len]);
    }
    if encoded.len() >= original_len {
        return Ok(encoded[..original_len].to_vec());
    }

    let mut output = Vec::with_capacity(original_len);
    let mut cursor = 0usize;
    while original_len.saturating_sub(output.len()) > 4 {
        let value = *encoded
            .get(cursor)
            .ok_or_else(|| invalid_field_err!("nscodec", "RLE plane ends before its raw tail"))?;
        cursor += 1;

        if original_len - output.len() == 5 {
            output.push(value);
            continue;
        }

        if encoded.get(cursor) == Some(&value) {
            cursor += 1;
            let marker = *encoded
                .get(cursor)
                .ok_or_else(|| invalid_field_err!("nscodec", "RLE run has no length"))?;
            cursor += 1;
            let run = if marker < 0xFF {
                usize::from(marker) + 2
            } else {
                let bytes = encoded
                    .get(cursor..cursor + 4)
                    .ok_or_else(|| invalid_field_err!("nscodec", "long RLE run is truncated"))?;
                cursor += 4;
                usize::try_from(u32::from_le_bytes(
                    bytes.try_into().expect("four-byte long-run field"),
                ))
                .map_err(|_| invalid_field_err!("nscodec", "long RLE run does not fit usize"))?
            };
            if run > original_len - output.len() {
                return Err(invalid_field_err!("nscodec", "RLE run exceeds plane size"));
            }
            output.resize(output.len() + run, value);
        } else {
            output.push(value);
        }
    }

    let tail = encoded
        .get(cursor..cursor + 4)
        .ok_or_else(|| invalid_field_err!("nscodec", "RLE plane raw tail is truncated"))?;
    output.extend_from_slice(tail);
    if output.len() != original_len {
        return Err(invalid_field_err!("nscodec", "decoded plane has the wrong size"));
    }
    Ok(output)
}

fn recover_chroma(value: u8, shift: u32) -> i16 {
    // The wire byte is shifted first and then narrowed back to signed 8-bit, matching the
    // reference decoder's two's-complement truncation before color reconstruction.
    let shifted = (i16::from(value) << shift).to_le_bytes()[0];
    i16::from(i8::from_le_bytes([shifted]))
}

fn clamp_channel(value: i16) -> u8 {
    u8::try_from(value.clamp(0, 255)).expect("value is clamped to the u8 range")
}
