use ironrdp_graphics::zgfx::Decompressor;

#[test]
fn multipart_segment_length_must_fit_remaining_payload() {
    let malformed = [
        0xe1, // multipart descriptor
        0x01, 0x00, // one segment
        0x01, 0x00, 0x00, 0x00, // one byte uncompressed
        0x02, 0x00, 0x00, 0x00, // two bytes declared
        0x04, // only one byte remains
    ];
    let mut decompressor = Decompressor::new();
    let mut output = Vec::new();

    assert!(decompressor.decompress(&malformed, &mut output).is_err());
    assert!(output.is_empty());
}

#[test]
fn truncated_compressed_tokens_return_errors_instead_of_panicking() {
    let malformed = [
        &[0xe0, 0x24, 0x00, 0x07][..], // literal token without its eight-bit value
        &[0xe0, 0x24, 0x09][..],       // impossible final-byte padding count
    ];

    for payload in malformed {
        let mut decompressor = Decompressor::new();
        let mut output = Vec::new();

        assert!(
            decompressor.decompress(payload, &mut output).is_err(),
            "malformed payload {payload:02x?} was accepted"
        );
    }
}

fn compressed_single_from_bits(bits: &str) -> Vec<u8> {
    let mut payload = vec![0xe0, 0x24];
    let mut byte = 0u8;
    let mut used = 0usize;
    for bit in bits.bytes() {
        assert!(matches!(bit, b'0' | b'1'));
        if bit == b'1' {
            byte |= 1 << (7 - used);
        }
        used += 1;
        if used == 8 {
            payload.push(byte);
            byte = 0;
            used = 0;
        }
    }
    let unused = if used == 0 { 0 } else { 8 - used };
    if used != 0 {
        payload.push(byte);
    }
    payload.push(u8::try_from(unused).expect("unused bit count fits in u8"));
    payload
}

#[test]
fn compressed_match_cannot_expand_one_segment_past_65535_bytes() {
    // One literal byte, then a distance-one match of 65,535 bytes. The match length is
    // token-size 14: fourteen one bits, a zero, then value 32,767 in fifteen bits.
    let bits = format!(
        "0{:08b}10001{:05b}{}0{}",
        0x41,
        1,
        "1".repeat(14),
        "1".repeat(15)
    );
    let payload = compressed_single_from_bits(&bits);
    let mut decompressor = Decompressor::new();
    let mut output = Vec::new();

    assert!(decompressor.decompress(&payload, &mut output).is_err());
    assert_eq!(
        output,
        [0x41],
        "the rejected match must not be applied partially"
    );
}
