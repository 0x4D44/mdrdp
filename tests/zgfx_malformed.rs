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
