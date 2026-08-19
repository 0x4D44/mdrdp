# vt-caps-probe — what does THIS Mac decode in hardware?

Answers the question `VTIsHardwareDecodeSupported` cannot: not just "does VideoToolbox
have a hardware H.264/HEVC/AV1 decoder", but **which chroma formats and bit depths** it
takes, at what size, and how long a frame takes. It does so the only honest way — by
feeding real streams to a `VTDecompressionSession` created with
`RequireHardwareAcceleratedVideoDecoder = true` and counting the frames that come out.

It also lists the VideoToolbox encoders and the H.264/HEVC profile levels each accepts,
which is where a Mac-side 4:2:2 / 4:4:4 *encode* capability would show up.

Results for the M5 Max are in
`wrk_docs/2026.08.19 - SPIKE - VideoToolbox chroma and codec decode matrix on M5 Max.md`.
Re-run on another Mac before assuming they carry over — M1/M2 will differ at least on AV1.

## Run

```
./gen-streams.sh                         # once; writes streams/ and streams5k/
swiftc -O -swift-version 5 probe.swift -o probe \
  -framework VideoToolbox -framework CoreMedia -framework CoreVideo -framework CoreFoundation
./probe streams                          # 640x480 matrix, every codec x chroma x depth
./probe streams5k                        # native 5K, the cases that matter
```

`gen-streams.sh` needs Homebrew ffmpeg (libx264/libx265/libvpx/libsvtav1) and cargo; the
AV1 4:4:4 and 4:2:2 streams are produced by the tiny `av1gen` crate (rav1e) because no
installed AV1 encoder does 4:4:4. The Cargo target dir goes under `$TMPDIR`.

## Reading the output

- `hw-req` — session created with hardware *required* and all frames decoded: that is
  a hardware decode, full stop. `sw-ok` repeats the run with hardware merely *allowed*,
  so "NO / NO" means VideoToolbox has no decoder for that format at all, hardware or
  software (VP9 is like this — Safari's VP9 decoder is not exposed through VideoToolbox).
- `out=` is the `CVPixelBuffer` format the decoder hands back (`420v`, `422v`, `444v`,
  `p420`/`p422`/`p444` for 10-bit) — i.e. what a renderer would have to consume.
- `ms/f` is wall-clock per frame over 8 frames on a **cold** session, first frame
  included. Order-of-magnitude only; not a latency figure to quote.

Stream file names encode the case: `<codec>_<chroma>_<bits>.<ext>`; the probe builds the
VP9 `vpcC` / AV1 `av1C` atoms from that name, and H.264/HEVC format descriptions from the
in-band parameter sets.
