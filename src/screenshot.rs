//! Write what the client is presenting to a file.
//!
//! Exists because "it connects" and "it shows the right picture" are different claims,
//! and only one of them can be checked by a counter. Every other signal this client
//! produces — frames decoded, bytes in, cache hit rate — can look perfectly healthy while
//! the surface holds garbage, a sheared tile grid, or swapped colour channels. A frame on
//! disk is the only evidence that settles it.
//!
//! BMP, deliberately, because it needs no dependency: 24-bit uncompressed BGR is a
//! fourteen-byte file header, a forty-byte info header and padded rows. PNG would mean
//! pulling in a compressor to serialise a debug artefact. `sips` (macOS) and any image
//! viewer will convert it if something prettier is wanted.
//!
//! **This writes session pixels to disk.** It happens only when the operator names a
//! path, exactly like `--capture-failures`.

use std::io::{self, Write};
use std::path::Path;

/// Bytes per pixel in the source RGBA buffer.
const SRC_BPP: usize = 4;
/// Bytes per pixel in the BMP we emit.
const DST_BPP: usize = 3;
const FILE_HEADER: usize = 14;
const INFO_HEADER: usize = 40;

/// Write an RGBA buffer as a 24-bit BMP.
///
/// `rgba` is tightly packed, `width` pixels per row, top row first — the layout
/// [`crate::surface::Surface`] uses.
pub fn write_bmp(path: &Path, width: u16, height: u16, rgba: &[u8]) -> io::Result<()> {
    let bytes = encode_bmp(width, height, rgba)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(path)?;
    file.write_all(&bytes)?;
    file.sync_all()
}

/// Encode an RGBA buffer as a 24-bit BMP, in memory.
///
/// Split from the write so the format can be tested without touching a filesystem.
pub fn encode_bmp(width: u16, height: u16, rgba: &[u8]) -> io::Result<Vec<u8>> {
    if width == 0 || height == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to write a zero-sized image",
        ));
    }
    let (w, h) = (usize::from(width), usize::from(height));
    let needed = w * h * SRC_BPP;
    if rgba.len() < needed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("source holds {} bytes, need {needed}", rgba.len()),
        ));
    }

    // BMP rows are padded to a four-byte boundary. Forgetting this is the classic BMP
    // bug: the image renders with a growing diagonal skew, which reads as a decoder fault
    // rather than as a writer one.
    let row_bytes = w * DST_BPP;
    let padding = (4 - (row_bytes % 4)) % 4;
    let stride = row_bytes + padding;
    let pixel_bytes = stride * h;

    let mut out = Vec::with_capacity(FILE_HEADER + INFO_HEADER + pixel_bytes);
    let file_size = (FILE_HEADER + INFO_HEADER + pixel_bytes) as u32;

    out.extend_from_slice(b"BM");
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved
    out.extend_from_slice(&((FILE_HEADER + INFO_HEADER) as u32).to_le_bytes());

    out.extend_from_slice(&(INFO_HEADER as u32).to_le_bytes());
    out.extend_from_slice(&(i32::from(width)).to_le_bytes());
    // Positive height means bottom-up, which is the BMP default and what every viewer
    // expects; the rows are emitted in reverse below to match.
    out.extend_from_slice(&(i32::from(height)).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&24u16.to_le_bytes()); // bits per pixel
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB, no compression
    out.extend_from_slice(&(pixel_bytes as u32).to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes()); // ~72 dpi
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // palette colours used
    out.extend_from_slice(&0u32.to_le_bytes()); // palette colours required

    for row in (0..h).rev() {
        let start = row * w * SRC_BPP;
        for px in 0..w {
            let p = start + px * SRC_BPP;
            // RGBA in, BGR out. Getting this backwards renders a recognisable desktop
            // with red and blue swapped, which is subtle enough to miss in a thumbnail.
            out.push(rgba[p + 2]);
            out.push(rgba[p + 1]);
            out.push(rgba[p]);
        }
        out.extend(std::iter::repeat_n(0u8, padding));
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x2 image with four distinct colours, so a row flip, a channel swap or a
    /// transposition each produce a different wrong answer.
    fn quad() -> Vec<u8> {
        vec![
            255, 0, 0, 255, // top-left  red
            0, 255, 0, 255, // top-right green
            0, 0, 255, 255, // bottom-left blue
            255, 255, 0, 255, // bottom-right yellow
        ]
    }

    #[test]
    fn the_headers_describe_the_image_we_actually_wrote() {
        let bmp = encode_bmp(2, 2, &quad()).unwrap();
        assert_eq!(&bmp[0..2], b"BM");
        assert_eq!(
            u32::from_le_bytes(bmp[2..6].try_into().unwrap()) as usize,
            bmp.len(),
            "the declared file size must match the real one"
        );
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 54);
        assert_eq!(i32::from_le_bytes(bmp[18..22].try_into().unwrap()), 2);
        assert_eq!(i32::from_le_bytes(bmp[22..26].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(bmp[28..30].try_into().unwrap()), 24);
    }

    #[test]
    fn rows_are_written_bottom_up_and_channels_are_bgr() {
        let bmp = encode_bmp(2, 2, &quad()).unwrap();
        let pixels = &bmp[54..];
        // Bottom-up: the FIRST row on disk is the source's LAST row — blue, then yellow.
        assert_eq!(&pixels[0..3], &[255, 0, 0], "blue as BGR");
        assert_eq!(&pixels[3..6], &[0, 255, 255], "yellow as BGR");
        // 2 px * 3 bytes = 6, padded to 8.
        assert_eq!(&pixels[8..11], &[0, 0, 255], "red as BGR");
        assert_eq!(&pixels[11..14], &[0, 255, 0], "green as BGR");
    }

    #[test]
    fn rows_are_padded_to_a_four_byte_boundary() {
        // 3 px * 3 bytes = 9, which must pad to 12. Unpadded rows skew the whole image.
        let (w, h) = (3u16, 1u16);
        let rgba = vec![0u8; usize::from(w) * usize::from(h) * SRC_BPP];
        let bmp = encode_bmp(w, h, &rgba).unwrap();
        assert_eq!(bmp.len(), 54 + 12);
    }

    #[test]
    fn a_short_source_is_refused_rather_than_reading_past_it() {
        let err = encode_bmp(4, 4, &[0u8; 8]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn a_zero_sized_image_is_refused() {
        assert!(encode_bmp(0, 4, &[]).is_err());
        assert!(encode_bmp(4, 0, &[]).is_err());
    }
}
