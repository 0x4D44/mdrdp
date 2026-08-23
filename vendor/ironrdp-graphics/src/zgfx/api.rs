//! High-level ZGFX compression API for EGFX PDU preparation.

use super::ZgfxError;
use super::compressor::Compressor;
use super::wrapper::{ZGFX_SEGMENTED_MAXSIZE, wrap_compressed, wrap_uncompressed};

/// Controls whether ZGFX compression is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionMode {
    /// Send uncompressed (no CPU overhead).
    Never,
    /// Compress and use the smaller result (bandwidth vs CPU trade-off).
    Auto,
    /// Always compress (best bandwidth).
    Always,
}

/// Compress and wrap EGFX PDU bytes into ZGFX segment format for DVC transmission.
///
/// In `Auto` mode, compression is only used when it actually reduces size.
/// The `compressor` maintains history state across calls for back-reference
/// efficiency.
pub fn compress_and_wrap_egfx(
    data: &[u8],
    compressor: &mut Compressor,
    mode: CompressionMode,
) -> Result<Vec<u8>, ZgfxError> {
    match mode {
        CompressionMode::Never => Ok(wrap_uncompressed(data)),
        CompressionMode::Auto => {
            let compressed = compressor.compress(data)?;

            // Compressed wrapping is single-segment only. Both the encoded bytes and
            // their decoded source must fit that segment; uncompressed wrapping handles
            // multipart framing natively.
            if data.len() <= ZGFX_SEGMENTED_MAXSIZE && compressed.len() <= ZGFX_SEGMENTED_MAXSIZE {
                let wrapped_compressed = wrap_compressed(&compressed);
                let wrapped_uncompressed = wrap_uncompressed(data);

                if wrapped_compressed.len() < wrapped_uncompressed.len() {
                    Ok(wrapped_compressed)
                } else {
                    Ok(wrapped_uncompressed)
                }
            } else {
                Ok(wrap_uncompressed(data))
            }
        }
        CompressionMode::Always => {
            let compressed = compressor.compress(data)?;

            if data.len() <= ZGFX_SEGMENTED_MAXSIZE && compressed.len() <= ZGFX_SEGMENTED_MAXSIZE {
                Ok(wrap_compressed(&compressed))
            } else {
                // Source or compressed output is too large for one segment; send
                // uncompressed so the wrapper can split it safely.
                Ok(wrap_uncompressed(data))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_never_produces_uncompressed() {
        let mut compressor = Compressor::new();
        let data = b"Test data";

        let wrapped = compress_and_wrap_egfx(data, &mut compressor, CompressionMode::Never).unwrap();

        assert_eq!(wrapped[0], 0xE0);
        assert_eq!(wrapped[1], 0x04); // RDP8, not compressed
    }

    #[test]
    fn mode_always_produces_compressed() {
        let mut compressor = Compressor::new();
        let data = b"Test data";

        let wrapped = compress_and_wrap_egfx(data, &mut compressor, CompressionMode::Always).unwrap();

        assert_eq!(wrapped[0], 0xE0);
        assert_eq!(wrapped[1], 0x24); // RDP8 + COMPRESSED
    }

    #[test]
    fn mode_auto_compresses_repetitive_data() {
        let mut compressor = Compressor::new();
        let data = b"AAAAAAAAAAAABBBBBBBBBBBBCCCCCCCCCCCC";

        let wrapped = compress_and_wrap_egfx(data, &mut compressor, CompressionMode::Auto).unwrap();

        assert_eq!(wrapped[0], 0xE0);
        assert_eq!(wrapped[1], 0x24);
    }

    #[test]
    fn round_trip_all_modes() {
        use super::super::Decompressor;

        let data = b"Test data with some repetition: AAAA BBBB CCCC";
        let mut decompressor = Decompressor::new();

        for mode in [CompressionMode::Never, CompressionMode::Auto, CompressionMode::Always] {
            let mut compressor = Compressor::new();
            let wrapped = compress_and_wrap_egfx(data, &mut compressor, mode).unwrap();

            let mut output = Vec::new();
            decompressor.decompress(&wrapped, &mut output).unwrap();

            assert_eq!(&output, data, "Round-trip failed for mode {mode:?}");
        }
    }

    #[test]
    fn large_repetitive_data_uses_multipart_even_when_compressed_is_small() {
        use super::super::Decompressor;

        let data = vec![0x41; ZGFX_SEGMENTED_MAXSIZE + 1];
        let mut compressor = Compressor::new();
        let wrapped = compress_and_wrap_egfx(&data, &mut compressor, CompressionMode::Always).unwrap();

        assert_eq!(wrapped[0], 0xe1, "an oversized source requires multipart framing");
        let mut output = Vec::new();
        Decompressor::new().decompress(&wrapped, &mut output).unwrap();
        assert_eq!(output, data);
    }
}
